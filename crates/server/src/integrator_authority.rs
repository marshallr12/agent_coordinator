//! Push authority for the deterministic integrator (planning p4-design §2
//! "Push authority", step S2; plan-final §2.4b and §2.2 item 5a).
//!
//! Before pushing R the integrator asks the service. Authority is granted only
//! when the approvals still hold under the pinned task digest, every check in
//! the roster read from T0 has a deciding success receipt on R, the protected
//! checks are all in that roster, the landing range carries no unapproved
//! stacked work, no approving reviewer contributed to that range, and every
//! `privilege_gate` report on the result carries a human's `allow`. Issuing
//! it takes the submission's integration hold; the authority stays outstanding
//! until the integrator's next observation of the target.
use crate::{
    auth::Auth,
    error::AppError,
    integrator::{eligible_subject, require_integrator_project},
    mutation::Mutation,
    response,
    state::AppState,
    workflow::{bounded, payload, revision},
};
use axum::{
    Json, Router,
    extract::{Path, State, rejection::JsonRejection},
    http::HeaderMap,
    routing::post,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection, sqlite::SqliteRow};

type Reply = Result<Json<Value>, AppError>;

/// Deciding conclusions that pass a required check, as on GitHub.
const PASSING: &[&str] = &["success", "skipped", "neutral"];
/// Conclusions that count as a failed attempt of a required check.
pub(crate) const FAILING: &[&str] = &["failure", "timed_out"];

/// How long the integrator may start a push after authority is issued.
const AUTHORITY_MS: i64 = 600_000;

/// The push-authority route.
pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/v1/projects/{p}/integrator/push-authority",
        post(push_authority),
    )
}

/// One required check in the roster the integrator read from T0.
#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct RosterCheck {
    identity: String,
    check_name: String,
    workflow_path: String,
    workflow_blob: String,
}

impl RosterCheck {
    /// Checks the entry's bounds and workflow location.
    fn validate(&self) -> Result<(), AppError> {
        bounded(&self.identity, "roster identity", 200, true)?;
        bounded(&self.check_name, "roster check_name", 200, true)?;
        revision(&self.workflow_blob, "roster workflow_blob")?;
        if !self.workflow_path.starts_with(".github/workflows/") || self.workflow_path.len() > 300 {
            return Err(AppError::bad_request(
                "roster workflow_path must name a file under .github/workflows/.",
            ));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct Roster {
    required_checks: Vec<RosterCheck>,
}

/// Parses and validates a result's roster (`required_checks` read from T0).
pub(crate) fn parse_roster(value: &Value) -> Result<Vec<RosterCheck>, AppError> {
    let roster: Roster = serde_json::from_value(value.clone()).map_err(|_| {
        AppError::bad_request(
            "roster.required_checks must list {identity, check_name, workflow_path, workflow_blob}.",
        )
    })?;
    if roster.required_checks.len() > 100 {
        return Err(AppError::bad_request(
            "roster lists at most 100 required checks.",
        ));
    }
    roster
        .required_checks
        .iter()
        .try_for_each(RosterCheck::validate)?;
    Ok(roster.required_checks)
}

#[derive(Deserialize, Serialize)]
struct AuthorityInput {
    result_id: String,
}

/// Loads one result of this project.
async fn load_result(c: &mut SqliteConnection, p: &str, id: &str) -> Result<SqliteRow, AppError> {
    sqlx::query("SELECT * FROM integrator_results WHERE id=? AND project_id=?")
        .bind(id)
        .bind(p)
        .fetch_optional(&mut *c)
        .await?
        .ok_or_else(AppError::not_found)
}

/// The eligible subject for the result, refusing a changed candidate or a
/// submission whose other result still awaits an observation.
async fn authority_subject(
    c: &mut SqliteConnection,
    p: &str,
    result: &SqliteRow,
) -> Result<SqliteRow, AppError> {
    let submission: String = result.get("submission_id");
    let subject = eligible_subject(c, p, &submission).await?;
    if subject.get::<String, _>("candidate_revision") != result.get::<String, _>("c") {
        return Err(AppError::conflict(
            "candidate_changed",
            "This result was computed for a different candidate revision.",
        ));
    }
    let other: i64 = sqlx::query_scalar("SELECT count(*) FROM integrator_results WHERE submission_id=? AND id!=? AND authority_issued_at IS NOT NULL")
        .bind(&submission).bind(result.get::<String, _>("id")).fetch_one(&mut *c).await?;
    if other > 0 {
        return Err(AppError::conflict(
            "observation_required",
            "Another result of this submission holds push authority; observe the target first.",
        ));
    }
    Ok(subject)
}

/// Identities of the protected checks (the constitution floor).
async fn protected_ids(c: &mut SqliteConnection, p: &str) -> Result<Vec<String>, AppError> {
    let raw: Option<String> =
        sqlx::query_scalar("SELECT required_checks_json FROM workflow_policies WHERE project_id=?")
            .bind(p)
            .fetch_optional(&mut *c)
            .await?;
    let checks: Vec<Value> = serde_json::from_str(raw.as_deref().unwrap_or("[]"))?;
    Ok(checks
        .iter()
        .filter_map(|check| check["identity"].as_str().map(str::to_owned))
        .collect())
}

/// Refuses a roster that drops any protected check.
fn ensure_protected(roster: &[RosterCheck], protected: &[String]) -> Result<(), AppError> {
    let missing: Vec<&String> = protected
        .iter()
        .filter(|id| !roster.iter().any(|check| &check.identity == *id))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    Err(AppError::conflict(
        "protected_check_missing",
        "The roster read from the target drops a protected check.",
    )
    .with_details(json!({"missing": missing})))
}

/// The deciding run (latest run, latest attempt) of one roster check on R.
async fn deciding_run(
    c: &mut SqliteConnection,
    result: &str,
    check: &RosterCheck,
) -> Result<Option<SqliteRow>, AppError> {
    Ok(sqlx::query("SELECT run_id,run_attempt,conclusion FROM integrator_receipts WHERE result_id=? AND check_name=? AND workflow_blob=? ORDER BY run_id DESC,run_attempt DESC LIMIT 1")
        .bind(result).bind(&check.check_name).bind(&check.workflow_blob)
        .fetch_optional(&mut *c).await?)
}

/// One roster check's failure record on R under the roster's workflow
/// blob: its deciding conclusion and how many attempts failed.
pub(crate) struct FailureRecord {
    pub identity: String,
    pub deciding: Option<String>,
    pub failures: i64,
}

/// The failure record of every roster check on R.
pub(crate) async fn failure_records(
    c: &mut SqliteConnection,
    result: &str,
    roster: &[RosterCheck],
) -> Result<Vec<FailureRecord>, AppError> {
    let mut records = Vec::new();
    for check in roster {
        let deciding = deciding_run(c, result, check).await?;
        let failures: i64 = sqlx::query_scalar("SELECT count(*) FROM integrator_receipts WHERE result_id=? AND check_name=? AND workflow_blob=? AND conclusion IN ('failure','timed_out')")
            .bind(result).bind(&check.check_name).bind(&check.workflow_blob)
            .fetch_one(&mut *c).await?;
        records.push(FailureRecord {
            identity: check.identity.clone(),
            deciding: deciding.map(|run| run.get("conclusion")),
            failures,
        });
    }
    Ok(records)
}

/// The deciding success run of every roster check, or a refusal naming the
/// pending and failed ones.
async fn deciding_runs(
    c: &mut SqliteConnection,
    result: &str,
    roster: &[RosterCheck],
) -> Result<Vec<Value>, AppError> {
    let (mut passed, mut pending, mut failed) = (Vec::new(), Vec::new(), Vec::new());
    for check in roster {
        match deciding_run(c, result, check).await? {
            None => pending.push(check.identity.clone()),
            Some(run) if PASSING.contains(&run.get::<String, _>("conclusion").as_str()) => passed.push(json!({
                "identity": check.identity, "check_name": check.check_name,
                "run_id": run.get::<i64, _>("run_id"), "run_attempt": run.get::<i64, _>("run_attempt")})),
            Some(_) => failed.push(check.identity.clone()),
        }
    }
    if pending.is_empty() && failed.is_empty() {
        return Ok(passed);
    }
    Err(AppError::conflict(
        "checks_not_passed",
        "Every roster check needs a deciding success receipt on R.",
    )
    .with_details(json!({"pending": pending, "failed": failed})))
}

/// Submissions of other tasks whose candidate commit, or whose recorded
/// landing range, shares a commit with this landing range.
async fn foreign_submissions(
    c: &mut SqliteConnection,
    p: &str,
    task: &str,
    range: &str,
) -> Result<Vec<SqliteRow>, AppError> {
    Ok(sqlx::query("SELECT DISTINCT s.id,s.task_id,s.superseded_at,t.lifecycle FROM submissions s JOIN tasks t ON t.id=s.task_id \
        WHERE s.project_id=? AND s.task_id!=? AND (s.candidate_revision IN (SELECT value FROM json_each(?)) \
        OR s.id IN (SELECT r.submission_id FROM integrator_results r, json_each(r.landing_range_json) j \
        WHERE r.project_id=? AND j.value IN (SELECT value FROM json_each(?))))")
        .bind(p).bind(task).bind(range).bind(p).bind(range)
        .fetch_all(&mut *c).await?)
}

/// True when a foreign submission is current and approved or integrated.
async fn settled(c: &mut SqliteConnection, row: &SqliteRow) -> Result<bool, AppError> {
    if row.get::<Option<i64>, _>("superseded_at").is_some() {
        return Ok(false);
    }
    if row.get::<String, _>("lifecycle") == "done" {
        return Ok(true);
    }
    crate::autonomy::approvals_satisfied(c, row.get("id")).await
}

/// Refuses a landing range that carries work neither approved nor integrated.
async fn ensure_not_stacked(
    c: &mut SqliteConnection,
    foreign: &[SqliteRow],
) -> Result<(), AppError> {
    let mut unapproved = Vec::new();
    for row in foreign {
        if !settled(c, row).await? {
            unapproved.push(row.get::<String, _>("id"));
        }
    }
    if unapproved.is_empty() {
        return Ok(());
    }
    Err(AppError::conflict(
        "stacked_on_unapproved",
        "The landing range carries commits of a submission that is neither approved nor integrated.",
    )
    .with_details(json!({"submissions": unapproved})))
}

/// Current approvals of the submission with their activity kind.
async fn approvals(c: &mut SqliteConnection, submission: &str) -> Result<Vec<SqliteRow>, AppError> {
    Ok(sqlx::query("SELECT rd.activity_id,rd.reviewer_id,rd.reviewer_session_id,wa.kind FROM review_decisions rd JOIN workflow_activities wa ON wa.id=rd.activity_id WHERE rd.submission_id=? AND rd.decision='approved' AND rd.invalidated_at IS NULL")
        .bind(submission).fetch_all(&mut *c).await?)
}

/// True when the approving reviewer contributed to any of `tasks`.
async fn approver_contributed(
    c: &mut SqliteConnection,
    p: &str,
    approval: &SqliteRow,
    tasks: &[String],
) -> Result<bool, AppError> {
    let reviewer: String = approval.get("reviewer_id");
    let session: String = approval.get("reviewer_session_id");
    for task in tasks {
        if crate::workflow::contributed(c, p, task, &reviewer, &session).await? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Approvals given by a contributor to the landing range's other tasks.
async fn contributor_approvals(
    c: &mut SqliteConnection,
    p: &str,
    submission: &str,
    tasks: &[String],
) -> Result<Vec<SqliteRow>, AppError> {
    let mut voided = Vec::new();
    for approval in approvals(c, submission).await? {
        if approver_contributed(c, p, &approval, tasks).await? {
            voided.push(approval);
        }
    }
    Ok(voided)
}

/// Voids one approval and queues a replacement review of the same kind.
async fn void_approval(
    m: &mut Mutation,
    p: &str,
    subject: &SqliteRow,
    approval: &SqliteRow,
) -> Result<(), AppError> {
    sqlx::query("UPDATE review_decisions SET invalidated_at=?,invalidated_reason='Reviewer contributed to the landing range.' WHERE activity_id=?")
        .bind(m.now).bind(approval.get::<String, _>("activity_id")).execute(&mut *m.tx).await?;
    crate::workflow::add_review_activity(
        &mut m.tx,
        p,
        subject.get("task_id"),
        subject.get("title"),
        subject.get("submission_id"),
        approval.get("kind"),
        m.now,
    )
    .await?;
    Ok(())
}

/// Sends the subject back to review after voiding contributor approvals.
async fn return_to_review(
    m: &mut Mutation,
    p: &str,
    subject: &SqliteRow,
    voided: &[SqliteRow],
) -> Result<(), AppError> {
    for approval in voided {
        void_approval(m, p, subject, approval).await?;
    }
    let (task, submission): (String, String) =
        (subject.get("task_id"), subject.get("submission_id"));
    sqlx::query("UPDATE workflow_subjects SET phase='review',updated_at=? WHERE task_id=?")
        .bind(m.now)
        .bind(&task)
        .execute(&mut *m.tx)
        .await?;
    sqlx::query("UPDATE tasks SET blocked_reason='Required reviews are pending.' WHERE id=(SELECT activity_task_id FROM workflow_activities WHERE submission_id=? AND kind='integration' AND state='queued')")
        .bind(&submission).execute(&mut *m.tx).await?;
    Ok(())
}

/// The submission's live integration activity id.
async fn integration_activity(
    c: &mut SqliteConnection,
    submission: &str,
) -> Result<String, AppError> {
    sqlx::query_scalar("SELECT id FROM workflow_activities WHERE submission_id=? AND kind='integration' AND state IN ('queued','active')")
        .bind(submission).fetch_optional(&mut *c).await?
        .ok_or_else(|| AppError::conflict("subject_not_integrable", "This submission has no live integration activity."))
}

/// The pinned (repository key, target branch) of the subject's submission.
fn hold_target(subject: &SqliteRow) -> Result<(String, String), AppError> {
    match (
        subject.get::<Option<String>, _>("canonical_repository_key"),
        subject.get::<Option<String>, _>("target_branch"),
    ) {
        (Some(key), Some(branch)) => Ok((key, branch)),
        _ => Err(AppError::conflict(
            "integration_target_unpinned",
            "The candidate does not pin a canonical repository key and target branch.",
        )),
    }
}

/// Takes (or keeps) the hold on the submission's integration activity,
/// refusing while another activity holds the same target.
async fn take_hold(m: &mut Mutation, subject: &SqliteRow) -> Result<(), AppError> {
    let activity = integration_activity(&mut m.tx, subject.get("submission_id")).await?;
    let (key, branch) = hold_target(subject)?;
    let other: i64 = sqlx::query_scalar("SELECT count(*) FROM integration_holds WHERE state='held' AND activity_id!=? AND canonical_repository_key=? AND target_branch=?")
        .bind(&activity).bind(&key).bind(&branch).fetch_one(&mut *m.tx).await?;
    if other > 0 {
        return Err(AppError::conflict(
            "integration_target_held",
            "Another integration holds this repository target.",
        ));
    }
    sqlx::query("INSERT INTO integration_holds(id,activity_id,canonical_repository_key,target_branch,state,acquired_by,acquired_at) VALUES(?,?,?,?,'held',?,?) \
        ON CONFLICT(activity_id) DO UPDATE SET state='held',acquired_by=excluded.acquired_by,acquired_at=excluded.acquired_at,released_by=NULL,released_at=NULL,release_reason=NULL")
        .bind(uuid::Uuid::new_v4().to_string()).bind(&activity).bind(&key).bind(&branch)
        .bind(&m.actor.id).bind(m.now).execute(&mut *m.tx).await?;
    Ok(())
}

/// Stores the contributor tasks on the result so later reviews see them.
async fn record_contributors(
    c: &mut SqliteConnection,
    result: &str,
    tasks: &[String],
) -> Result<(), AppError> {
    sqlx::query("UPDATE integrator_results SET contributor_tasks_json=? WHERE id=?")
        .bind(serde_json::to_string(tasks)?)
        .bind(result)
        .execute(&mut *c)
        .await?;
    Ok(())
}

/// Marks authority outstanding on the result and returns its expiry.
async fn issue(m: &mut Mutation, result: &str) -> Result<i64, AppError> {
    let expires = m.now + AUTHORITY_MS;
    sqlx::query(
        "UPDATE integrator_results SET authority_issued_at=?,authority_expires_at=? WHERE id=?",
    )
    .bind(m.now)
    .bind(expires)
    .bind(result)
    .execute(&mut *m.tx)
    .await?;
    Ok(expires)
}

/// What authority is judged from: the result, its subject, its roster checks
/// and the other tasks its landing range carries.
struct Judged {
    result: SqliteRow,
    subject: SqliteRow,
    roster: Vec<RosterCheck>,
    contributor_tasks: Vec<String>,
}

/// Refuses a result whose `privilege_gate` report a human has not resolved
/// (`privilege_gate_unresolved`) or resolved with `deny`
/// (`privilege_gate_denied`); a result without such a report, or whose
/// report carries the decision `allow`, passes.
async fn ensure_privilege_allowed(c: &mut SqliteConnection, result: &str) -> Result<(), AppError> {
    let gate: Option<(String, Option<i64>)> = sqlx::query_as("SELECT id,resolved_at FROM integrator_reports WHERE kind='privilege_gate' AND result_id=? AND (resolved_at IS NULL OR decision IS NOT 'allow') ORDER BY resolved_at IS NOT NULL,created_at LIMIT 1")
        .bind(result).fetch_optional(&mut *c).await?;
    let Some((report, resolved)) = gate else {
        return Ok(());
    };
    let (code, message) = match resolved {
        None => (
            "privilege_gate_unresolved",
            "A privilege_gate report on this result awaits a human decision.",
        ),
        Some(_) => (
            "privilege_gate_denied",
            "A human denied pushing this result in its privilege_gate report.",
        ),
    };
    Err(AppError::conflict(code, message).with_details(json!({"report_id": report})))
}

/// Loads the result and applies every refusal that has no side effect.
async fn judge(m: &mut Mutation, p: &str, id: &str) -> Result<(Judged, Vec<Value>), AppError> {
    let result = load_result(&mut m.tx, p, id).await?;
    ensure_privilege_allowed(&mut m.tx, id).await?;
    let subject = authority_subject(&mut m.tx, p, &result).await?;
    let roster = parse_roster(&serde_json::from_str(result.get("roster_json"))?)?;
    ensure_protected(&roster, &protected_ids(&mut m.tx, p).await?)?;
    let runs = deciding_runs(&mut m.tx, id, &roster).await?;
    let range: String = result.get("landing_range_json");
    let foreign = foreign_submissions(&mut m.tx, p, subject.get("task_id"), &range).await?;
    ensure_not_stacked(&mut m.tx, &foreign).await?;
    let mut contributor_tasks: Vec<String> = foreign.iter().map(|r| r.get("task_id")).collect();
    contributor_tasks.sort();
    contributor_tasks.dedup();
    let judged = Judged {
        result,
        subject,
        roster,
        contributor_tasks,
    };
    Ok((judged, runs))
}

/// The granted authority as returned to the integrator.
fn granted(j: &Judged, runs: Vec<Value>, protected: Vec<String>, expires: i64) -> Value {
    json!({
        "granted": true,
        "result_id": j.result.get::<String, _>("id"),
        "r": j.result.get::<String, _>("r"),
        "t0": j.result.get::<String, _>("t0"),
        "deciding_runs": runs,
        "roster_ids": j.roster.iter().map(|c| c.identity.clone()).collect::<Vec<_>>(),
        "protected_ids": protected,
        "expires_at": coordinator_core::timestamp(expires),
    })
}

/// Grants authority, or returns the subject to review when an approver
/// contributed to the landing range.
async fn decide(
    m: &mut Mutation,
    p: &str,
    j: &Judged,
    runs: Vec<Value>,
) -> Result<Value, AppError> {
    let id: String = j.result.get("id");
    record_contributors(&mut m.tx, &id, &j.contributor_tasks).await?;
    let submission: String = j.subject.get("submission_id");
    let voided = contributor_approvals(&mut m.tx, p, &submission, &j.contributor_tasks).await?;
    if !voided.is_empty() {
        return_to_review(m, p, &j.subject, &voided).await?;
        let reviewers: Vec<String> = voided.iter().map(|a| a.get("reviewer_id")).collect();
        return Ok(
            json!({"granted": false, "refusal": "approver_is_contributor",
            "result_id": id, "voided_reviewers": reviewers, "phase": "review"}),
        );
    }
    take_hold(m, &j.subject).await?;
    let expires = issue(m, &id).await?;
    let protected = protected_ids(&mut m.tx, p).await?;
    Ok(granted(j, runs, protected, expires))
}

/// `POST …/integrator/push-authority`: authorizes pushing one result's R.
async fn push_authority(
    State(s): State<AppState>,
    auth: Auth,
    Path(p): Path<String>,
    headers: HeaderMap,
    body: Result<Json<AuthorityInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    bounded(&input.result_id, "result_id", 100, true)?;
    let op = format!("POST /api/v1/projects/{p}/integrator/push-authority");
    let mut m = Mutation::begin(&s, &auth, &headers, &op, &input).await?;
    if let Some(v) = m.replay.take() {
        return Ok(response(v));
    }
    require_integrator_project(&mut m.tx, &p).await?;
    let (judged, runs) = judge(&mut m, &p, &input.result_id).await?;
    let value = decide(&mut m, &p, &judged, runs).await?;
    let kind = if value["granted"] == true {
        "integrator.push_authorized"
    } else {
        "integrator.returned_to_review"
    };
    Ok(response(
        m.finish(value, Some(&p), kind, &input.result_id).await?,
    ))
}
