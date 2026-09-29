//! The integrator's side of revert tasks (planning plan-final §2.4a "M6",
//! p4-design §2 "Revert (M6)", step S4).
//!
//! Reverts awaiting a candidate appear in the integrator queue under
//! `reverts`. The integrator computes the candidate itself (`git revert -m 1
//! R` on the current tip, or the landed range when R landed fast-forward),
//! pushes it as a candidate ref and records it here as the revert's
//! submission, attested `mechanical`; review (for an agent's revert) and
//! integration then run as for any code subject. When it cannot revert
//! mechanically it reports so and the revert becomes ordinary implementation
//! work with full review; the capped revert cascade of plan-final §2.4a is
//! future work. This module also guards re-landing: a no-op candidate whose commits
//! a recorded revert undid is refused (`candidate_reverted_in_history`), and
//! a published `defect` revert proposes a re-land task.
use crate::{
    auth::Auth,
    coordination::{save_task_revision, task_record_value},
    error::AppError,
    integrator::require_integrator_project,
    integrator_observe::outstanding_authority,
    mutation::Mutation,
    response,
    reverts::{clip, evidence_text, revert_view, review_mode_floor},
    state::AppState,
    workflow::{
        Supersede, add_review_activity, bounded, payload, revision, supersede_submission,
        validate_candidate_ref, workflow_snapshot,
    },
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

/// Reverts listed per queue call.
const REVERT_QUEUE_LIMIT: i64 = 50;
/// Why the integrator could not revert mechanically.
const NOT_MECHANICAL_REASONS: &[&str] = &["conflict", "check_failed"];
/// Refusal message for a revert already converted to implementation work.
const NOT_MECHANICAL_MESSAGE: &str =
    "This revert became implementation work; the integrator no longer computes its candidate.";
/// Refusal message for a revert task that is not open.
const NOT_OPEN_MESSAGE: &str = "This revert task is not open.";
/// Blocked reason of a task whose submission is in the completion workflow.
const IN_WORKFLOW: &str = "An immutable submission is in the completion workflow.";

/// The integrator's revert routes.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/projects/{p}/integrator/reverts/{id}/candidate",
            post(candidate),
        )
        .route(
            "/api/v1/projects/{p}/integrator/reverts/{id}/not-mechanical",
            post(not_mechanical),
        )
}

/// Mechanical reverts of project `p` awaiting a candidate: open, unclaimed,
/// and with no candidate in review or integration.
macro_rules! pending_sql {
    () => {
        "SELECT tr.*,t.title,t.priority,r.t0,r.c,r.landing_range_json,s.repository_url,s.target_branch \
         FROM task_reverts tr JOIN tasks t ON t.id=tr.task_id JOIN integrator_results r ON r.id=tr.result_id \
         JOIN submissions s ON s.id=tr.submission_id LEFT JOIN workflow_subjects ws ON ws.task_id=tr.task_id \
         WHERE tr.project_id=? AND tr.mode='mechanical' AND t.lifecycle='open' AND t.current_attempt_id IS NULL \
         AND (ws.phase IS NULL OR ws.phase='revision_needed')"
    };
}

/// One pending revert as served in the queue.
fn pending_value(row: &SqliteRow) -> Result<Value, AppError> {
    let task: String = row.get("task_id");
    Ok(json!({
        "id": task, "task_id": task,
        "result_id": row.get::<String, _>("result_id"),
        "r": row.get::<String, _>("r"),
        "title": row.get::<String, _>("title"),
        "priority": row.get::<i64, _>("priority"),
        "repository_url": row.get::<Option<String>, _>("repository_url"),
        "target_branch": row.get::<Option<String>, _>("target_branch"),
        "target": {
            "submission_id": row.get::<String, _>("submission_id"),
            "result_id": row.get::<String, _>("result_id"),
            "original_task_id": row.get::<String, _>("original_task_id"),
            "r": row.get::<String, _>("r"), "t0": row.get::<String, _>("t0"),
            "c": row.get::<String, _>("c"),
            "landing_range": serde_json::from_str::<Value>(row.get("landing_range_json"))?,
            "reason": row.get::<String, _>("reason"),
            "evidence": serde_json::from_str::<Value>(row.get("evidence_json"))?,
        },
    }))
}

/// Reverts awaiting an integrator candidate, most urgent first.
pub(crate) async fn pending_reverts(
    c: &mut SqliteConnection,
    p: &str,
) -> Result<Vec<Value>, AppError> {
    let sql = concat!(
        pending_sql!(),
        " ORDER BY t.priority,tr.created_at,tr.task_id LIMIT ?"
    );
    let rows = sqlx::query(sql)
        .bind(p)
        .bind(REVERT_QUEUE_LIMIT)
        .fetch_all(&mut *c)
        .await?;
    rows.iter().map(pending_value).collect()
}

/// Landing ranges of the project's results that a revert (not canceled)
/// targets, with the repository and branch they landed on, so the
/// integrator can refuse a no-op that re-lands them.
pub(crate) async fn reverted_landings(
    c: &mut SqliteConnection,
    p: &str,
) -> Result<Vec<Value>, AppError> {
    let rows = sqlx::query("SELECT tr.task_id,tr.result_id,r.landing_range_json,s.repository_url,s.target_branch FROM task_reverts tr JOIN tasks t ON t.id=tr.task_id JOIN integrator_results r ON r.id=tr.result_id JOIN submissions s ON s.id=r.submission_id WHERE tr.project_id=? AND t.lifecycle!='canceled' ORDER BY tr.created_at,tr.task_id")
        .bind(p).fetch_all(&mut *c).await?;
    rows.iter()
        .map(|row| {
            Ok(json!({"revert_task_id": row.get::<String, _>("task_id"),
                "result_id": row.get::<String, _>("result_id"),
                "repository_url": row.get::<Option<String>, _>("repository_url"),
                "target_branch": row.get::<Option<String>, _>("target_branch"),
                "landing_range": serde_json::from_str::<Value>(row.get("landing_range_json"))?}))
        })
        .collect()
}

/// A no-op result (`r` equal to `t0`) about to be pinned.
pub(crate) struct NoOp<'a> {
    /// The repository its submission pinned.
    pub repository_url: &'a str,
    /// The target branch its submission pinned.
    pub target_branch: &'a str,
    /// Its candidate commit and landing range.
    pub commits: Vec<String>,
}

/// The (reverted result, shared commit) pairs where `noop`'s commits meet
/// the landing range, on the same repository and target branch, of a
/// result a revert (not canceled) targets.
async fn reverted_hits(
    c: &mut SqliteConnection,
    p: &str,
    noop: &NoOp<'_>,
) -> Result<Vec<(String, String)>, AppError> {
    Ok(sqlx::query_as("SELECT DISTINCT tr.result_id,j.value FROM task_reverts tr JOIN tasks t ON t.id=tr.task_id JOIN integrator_results r ON r.id=tr.result_id JOIN submissions s ON s.id=r.submission_id, json_each(r.landing_range_json) j WHERE tr.project_id=? AND t.lifecycle!='canceled' AND s.repository_url=? AND s.target_branch=? AND j.value IN (SELECT value FROM json_each(?)) ORDER BY tr.result_id,j.value")
        .bind(p).bind(noop.repository_url).bind(noop.target_branch)
        .bind(serde_json::to_string(&noop.commits)?).fetch_all(&mut *c).await?)
}

/// Refuses a `reverted_in_history` revise of `submission` unless its
/// candidate commit, or the landing range of a result recorded for it,
/// shares a commit with a reverted landing on its repository and target
/// branch (the same test pinning a no-op result applies).
pub(crate) async fn ensure_reverted_in_history(
    c: &mut SqliteConnection,
    p: &str,
    submission: &str,
) -> Result<(), AppError> {
    let row = sqlx::query("SELECT s.candidate_revision,s.repository_url,s.target_branch,(SELECT json_group_array(j.value) FROM integrator_results r, json_each(r.landing_range_json) j WHERE r.submission_id=s.id) AS ranges FROM submissions s WHERE s.id=? AND s.project_id=?")
        .bind(submission).bind(p).fetch_one(&mut *c).await?;
    let mut commits: Vec<String> = serde_json::from_str(row.get("ranges"))?;
    commits.extend(row.get::<Option<String>, _>("candidate_revision"));
    let (repository_url, target_branch): (Option<String>, Option<String>) =
        (row.get("repository_url"), row.get("target_branch"));
    let noop = NoOp {
        repository_url: repository_url.as_deref().unwrap_or_default(),
        target_branch: target_branch.as_deref().unwrap_or_default(),
        commits,
    };
    if reverted_hits(c, p, &noop).await?.is_empty() {
        return Err(AppError::conflict(
            "not_reverted_in_history",
            "Neither this submission's candidate nor a result recorded for it shares a commit with a reverted landing on its target.",
        ));
    }
    Ok(())
}

/// Refuses a no-op result whose candidate commit or landing range shares a
/// commit with the landing range, on the same repository and target
/// branch, of a result a revert (not canceled) targets: re-land candidates
/// must be new commits. The details name the reverted results and the
/// shared commits the integrator cites in a `reverted_in_history` revise.
pub(crate) async fn ensure_not_reverted_in_history(
    c: &mut SqliteConnection,
    p: &str,
    noop: &NoOp<'_>,
) -> Result<(), AppError> {
    let hits = reverted_hits(c, p, noop).await?;
    if hits.is_empty() {
        return Ok(());
    }
    let (mut results, commits): (Vec<String>, Vec<String>) = hits.into_iter().unzip();
    results.dedup();
    Err(AppError::conflict(
        "candidate_reverted_in_history",
        "The candidate's commits were landed and then reverted; a re-land candidate must be new commits (cherry-pick or revert of the revert). Send it back with an integrator revise, reason_code reverted_in_history.",
    )
    .with_details(json!({"reverted_results": results, "commits": commits})))
}

/// A revert task of project `p` with its task state and subject phase.
async fn load_revert(c: &mut SqliteConnection, p: &str, id: &str) -> Result<SqliteRow, AppError> {
    sqlx::query("SELECT tr.*,t.title,t.lifecycle,t.current_attempt_id,t.revision,t.generation,t.acceptance_json,ws.phase,ws.current_submission_id FROM task_reverts tr JOIN tasks t ON t.id=tr.task_id LEFT JOIN workflow_subjects ws ON ws.task_id=tr.task_id WHERE tr.task_id=? AND tr.project_id=?")
        .bind(id).bind(p).fetch_optional(&mut *c).await?.ok_or_else(AppError::not_found)
}

/// The integrator's mechanical revert candidate, computed on tip `t0`.
#[derive(Deserialize, Serialize)]
struct CandidateInput {
    t0: String,
    candidate_commit: String,
    candidate_tree: String,
    mechanical: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    candidate_ref: Option<String>,
}

impl CandidateInput {
    /// Checks identities and the mechanical attestation.
    fn validate(&self) -> Result<(), AppError> {
        revision(&self.t0, "t0")?;
        revision(&self.candidate_commit, "candidate_commit")?;
        revision(&self.candidate_tree, "candidate_tree")?;
        if let Some(r) = &self.candidate_ref {
            validate_candidate_ref(r)?;
        }
        if !self.mechanical {
            return Err(AppError::bad_request(
                "mechanical must be true; report a revert you cannot compute mechanically through not-mechanical.",
            ));
        }
        Ok(())
    }
}

/// The candidate stored for (revert, t0): its submission, commit and tree.
async fn stored_candidate(
    c: &mut SqliteConnection,
    task: &str,
    t0: &str,
) -> Result<Option<(String, String, String)>, AppError> {
    Ok(sqlx::query_as("SELECT submission_id,candidate_commit,candidate_tree FROM revert_candidates WHERE revert_task_id=? AND t0=?")
        .bind(task).bind(t0).fetch_optional(&mut *c).await?)
}

/// Why a revert takes no candidate at all, as (code, message): it became
/// implementation work, or it is not open.
fn retired_refusal(row: &SqliteRow) -> Option<(&'static str, &'static str)> {
    if row.get::<String, _>("mode") != "mechanical" {
        return Some(("revert_not_mechanical", NOT_MECHANICAL_MESSAGE));
    }
    if row.get::<String, _>("lifecycle") != "open" {
        return Some(("revert_not_open", NOT_OPEN_MESSAGE));
    }
    None
}

/// Why an open mechanical revert takes no candidate now, as (code,
/// message): an attempt owns it, or its candidate is in review or
/// integration.
fn busy_refusal(row: &SqliteRow) -> Option<(&'static str, &'static str)> {
    if row.get::<Option<String>, _>("current_attempt_id").is_some() {
        return Some((
            "revert_claimed",
            "An attempt currently owns this revert task.",
        ));
    }
    let phase = row.get::<Option<String>, _>("phase");
    if !matches!(phase.as_deref(), None | Some("revision_needed")) {
        return Some((
            "revert_candidate_exists",
            "This revert already has a candidate in review or integration.",
        ));
    }
    None
}

/// Refuses with the first refusal `check` finds.
fn refuse(
    row: &SqliteRow,
    check: fn(&SqliteRow) -> Option<(&'static str, &'static str)>,
) -> Result<(), AppError> {
    check(row).map_or(Ok(()), |(code, message)| {
        Err(AppError::conflict(code, message))
    })
}

/// Everything the candidate submission pins from the project and its roster.
struct Pins {
    repository_url: String,
    target_branch: String,
    policy_revision: i64,
    review_mode: String,
    roster_revision: i64,
    canonical: String,
}

/// Reads the project and workflow policy the candidate submission pins.
async fn pins(c: &mut SqliteConnection, p: &str) -> Result<Pins, AppError> {
    let row = sqlx::query("SELECT p.repository_url,p.target_branch,p.policy_revision,p.review_mode,w.revision,w.canonical_repository_key FROM projects p LEFT JOIN workflow_policies w ON w.project_id=p.id WHERE p.id=?")
        .bind(p).fetch_one(&mut *c).await?;
    let Some(roster_revision) = row.get::<Option<i64>, _>("revision") else {
        return Err(AppError::conflict(
            "workflow_policy_required",
            "Configure the project's workflow policy before recording a revert candidate.",
        ));
    };
    Ok(Pins {
        repository_url: row.get("repository_url"),
        target_branch: row.get("target_branch"),
        policy_revision: row.get("policy_revision"),
        review_mode: row.get("review_mode"),
        roster_revision,
        canonical: row.get("canonical_repository_key"),
    })
}

/// Inserts the integrator's already-submitted attempt on the revert task and
/// returns its id.
async fn insert_attempt(
    m: &mut Mutation,
    p: &str,
    revert: &SqliteRow,
    pins: &Pins,
) -> Result<String, AppError> {
    let (id, generation) = (
        uuid::Uuid::new_v4().to_string(),
        revert.get::<i64, _>("generation") + 1,
    );
    sqlx::query("INSERT INTO attempts(id,project_id,task_id,owner_id,session_id,credential_id,generation,state,mode,expires_at,last_heartbeat_at,last_progress_at,created_at,ended_at,outcome,task_revision,policy_revision) VALUES(?,?,?,?,?,?,?,'submitted','work',?,?,?,?,?,'Mechanical revert candidate recorded by the integrator.',?,?)")
        .bind(&id).bind(p).bind(revert.get::<String, _>("task_id")).bind(&m.actor.id).bind(integrator_session(m))
        .bind(&m.actor.credential_id).bind(generation).bind(m.now).bind(m.now).bind(m.now).bind(m.now).bind(m.now)
        .bind(revert.get::<i64, _>("revision")).bind(pins.policy_revision).execute(&mut *m.tx).await?;
    sqlx::query("UPDATE tasks SET generation=?,blocked_reason=? WHERE id=?")
        .bind(generation)
        .bind(IN_WORKFLOW)
        .bind(revert.get::<String, _>("task_id"))
        .execute(&mut *m.tx)
        .await?;
    Ok(id)
}

/// The session recorded for the integrator's attempt and contribution.
fn integrator_session(m: &Mutation) -> String {
    m.actor.session_id.clone().unwrap_or_else(|| {
        format!(
            "integrator:{}",
            m.actor.credential_id.as_deref().unwrap_or(&m.actor.id)
        )
    })
}

/// Evidence for each acceptance criterion: the integrator's attestation.
fn attested_evidence(revert: &SqliteRow, t0: &str) -> Result<Value, AppError> {
    let criteria: Vec<String> = serde_json::from_str(revert.get("acceptance_json"))?;
    let r: String = revert.get("r");
    let evidence = format!(
        "The integrator attests that the candidate tree is the mechanical revert of {r} computed on {t0}; required checks run on its integration result."
    );
    Ok(json!(
        criteria
            .iter()
            .map(|c| json!({"criterion": c, "evidence": evidence}))
            .collect::<Vec<_>>()
    ))
}

/// Inserts the candidate as the revert's code submission and returns its id.
async fn insert_submission(
    m: &mut Mutation,
    p: &str,
    (revert, attempt): (&SqliteRow, &str),
    pins: &Pins,
    input: &CandidateInput,
) -> Result<String, AppError> {
    let (id, task): (String, String) = (uuid::Uuid::new_v4().to_string(), revert.get("task_id"));
    let digest = crate::autonomy::current_task_digest(&mut m.tx, &task).await?;
    let summary = format!(
        "Mechanical revert of {} computed by the integrator on {}.",
        revert.get::<String, _>("r"),
        input.t0
    );
    sqlx::query("INSERT INTO submissions(id,project_id,task_id,attempt_id,kind,task_revision,project_policy_revision,workflow_policy_revision,summary,acceptance_evidence_json,handoff,canonical_repository_key,repository_url,target_branch,base_revision,candidate_revision,candidate_tree,candidate_ref,created_by,contributor_session_id,created_at,task_digest) VALUES(?,?,?,?,'code',?,?,?,?,?,'',?,?,?,?,?,?,?,?,?,?,?)")
        .bind(&id).bind(p).bind(&task).bind(attempt).bind(revert.get::<i64, _>("revision")).bind(pins.policy_revision)
        .bind(pins.roster_revision).bind(&summary).bind(attested_evidence(revert, &input.t0)?.to_string())
        .bind(&pins.canonical).bind(&pins.repository_url).bind(&pins.target_branch).bind(&input.t0)
        .bind(&input.candidate_commit).bind(&input.candidate_tree).bind(&input.candidate_ref)
        .bind(&m.actor.id).bind(integrator_session(m)).bind(m.now).bind(&digest).execute(&mut *m.tx).await?;
    let (actor, session) = (m.actor.id.clone(), integrator_session(m));
    crate::workflow::record_contributor(&mut m.tx, &task, &actor, &session, m.now).await?;
    Ok(id)
}

/// Makes the submission the revert's current one (an earlier one is already
/// superseded by the revise that reopened it), in review when the revert needs review and in
/// integration otherwise.
async fn enter_workflow(
    m: &mut Mutation,
    p: &str,
    revert: &SqliteRow,
    submission: &str,
    reviews: &[String],
) -> Result<(), AppError> {
    let task: String = revert.get("task_id");
    let phase = if reviews.is_empty() {
        "integration"
    } else {
        "review"
    };
    sqlx::query("INSERT INTO workflow_subjects(project_id,task_id,current_submission_id,phase,updated_at) VALUES(?,?,?,?,?) ON CONFLICT(task_id) DO UPDATE SET current_submission_id=excluded.current_submission_id,phase=excluded.phase,updated_at=excluded.updated_at")
        .bind(p).bind(&task).bind(submission).bind(phase).bind(m.now).execute(&mut *m.tx).await?;
    create_activities(m, p, revert, submission, reviews).await
}

/// Queues the review activities and the integration activity, which waits
/// for the reviews.
async fn create_activities(
    m: &mut Mutation,
    p: &str,
    revert: &SqliteRow,
    submission: &str,
    reviews: &[String],
) -> Result<(), AppError> {
    let (task, title): (String, String) = (revert.get("task_id"), revert.get("title"));
    let mut integration = String::new();
    for kind in reviews.iter().map(String::as_str).chain(["integration"]) {
        integration =
            add_review_activity(&mut m.tx, p, &task, &title, submission, kind, m.now).await?;
    }
    if !reviews.is_empty() {
        sqlx::query("UPDATE tasks SET blocked_reason='Required reviews are pending.' WHERE id=(SELECT activity_task_id FROM workflow_activities WHERE id=?)")
            .bind(integration).execute(&mut *m.tx).await?;
    }
    Ok(())
}

/// The review kinds the revert's candidate needs: none for a human's
/// revert, otherwise the project's (at least an agent review).
async fn candidate_reviews(
    c: &mut SqliteConnection,
    revert: &SqliteRow,
    pins: &Pins,
) -> Result<Vec<String>, AppError> {
    if !revert.get::<bool, _>("review_required") {
        return Ok(Vec::new());
    }
    let task: String = revert.get("task_id");
    let mode = review_mode_floor(c, &task, &pins.review_mode).await?;
    Ok(crate::autonomy::required_review_kinds(&mode))
}

/// Records the candidate as the revert's submission and its candidate row;
/// returns the submission id.
async fn record_candidate(
    m: &mut Mutation,
    p: &str,
    revert: &SqliteRow,
    input: &CandidateInput,
) -> Result<String, AppError> {
    let pins = pins(&mut m.tx, p).await?;
    let attempt = insert_attempt(m, p, revert, &pins).await?;
    let submission = insert_submission(m, p, (revert, &attempt), &pins, input).await?;
    let reviews = candidate_reviews(&mut m.tx, revert, &pins).await?;
    enter_workflow(m, p, revert, &submission, &reviews).await?;
    sqlx::query("INSERT INTO revert_candidates(revert_task_id,t0,submission_id,candidate_commit,candidate_tree,attestation,recorded_by,recorded_at) VALUES(?,?,?,?,?,'mechanical',?,?)")
        .bind(revert.get::<String, _>("task_id")).bind(&input.t0).bind(&submission)
        .bind(&input.candidate_commit).bind(&input.candidate_tree).bind(&m.actor.id).bind(m.now)
        .execute(&mut *m.tx).await?;
    Ok(submission)
}

/// True when `stored` (submission, commit, tree) is `input` and still the
/// revert's live candidate in review or integration.
fn replays(revert: &SqliteRow, stored: &(String, String, String), input: &CandidateInput) -> bool {
    let (submission, commit, tree) = stored;
    *commit == input.candidate_commit
        && *tree == input.candidate_tree
        && live_candidate(revert).as_ref() == Some(submission)
}

/// Records the candidate unless one is stored for this tip: the same
/// commit and tree replay while that submission is still live; a stored
/// candidate that differs, or is superseded, conflicts. Returns the
/// submission.
async fn candidate_for_tip(
    m: &mut Mutation,
    p: &str,
    id: &str,
    input: &CandidateInput,
) -> Result<String, AppError> {
    let revert = load_revert(&mut m.tx, p, id).await?;
    let stored = stored_candidate(&mut m.tx, id, &input.t0).await?;
    if let Some(stored) = stored.as_ref().filter(|s| replays(&revert, s, input)) {
        return Ok(stored.0.clone());
    }
    refuse(&revert, retired_refusal)?;
    if stored.is_some() {
        return Err(AppError::conflict(
            "revert_candidate_conflict",
            "A different or superseded candidate is already recorded for this revert and target tip; compute the revert on the current tip.",
        ));
    }
    refuse(&revert, busy_refusal)?;
    record_candidate(m, p, &revert, input).await
}

/// `POST …/integrator/reverts/{id}/candidate`: records the integrator's
/// mechanical revert candidate as the revert's submission.
async fn candidate(
    State(s): State<AppState>,
    auth: Auth,
    Path((p, id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<CandidateInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    input.validate()?;
    let op = format!("POST /api/v1/projects/{p}/integrator/reverts/{id}/candidate");
    let mut m = Mutation::begin(&s, &auth, &headers, &op, &input).await?;
    if let Some(v) = m.replay.take() {
        return Ok(response(v));
    }
    require_integrator_project(&mut m.tx, &p).await?;
    let submission = candidate_for_tip(&mut m, &p, &id, &input).await?;
    let mut value = workflow_snapshot(&mut m.tx, &p, &id, m.now).await?;
    value["revert_task_id"] = json!(id);
    value["candidate_submission_id"] = json!(submission);
    Ok(response(
        m.finish(value, Some(&p), "revert.candidate_recorded", &id)
            .await?,
    ))
}

/// The integrator's report that it cannot revert mechanically.
#[derive(Deserialize, Serialize)]
struct NotMechanicalInput {
    t0: String,
    reason: String,
    evidence: String,
}

impl NotMechanicalInput {
    /// Checks the tip, reason and evidence bounds.
    fn validate(&self) -> Result<(), AppError> {
        revision(&self.t0, "t0")?;
        bounded(&self.evidence, "evidence", 16384, true)?;
        if !NOT_MECHANICAL_REASONS.contains(&self.reason.as_str()) {
            return Err(AppError::bad_request(
                "reason must be conflict or check_failed.",
            ));
        }
        Ok(())
    }
}

/// The revert's current submission while it is in review or integration.
fn live_candidate(revert: &SqliteRow) -> Option<String> {
    let phase = revert.get::<Option<String>, _>("phase");
    matches!(phase.as_deref(), Some("review" | "integration"))
        .then(|| revert.get("current_submission_id"))
}

/// Supersedes the revert's candidate in review or integration, unless push
/// authority for it is outstanding.
async fn withdraw_candidate(m: &mut Mutation, p: &str, revert: &SqliteRow) -> Result<(), AppError> {
    let Some(submission) = live_candidate(revert) else {
        return Ok(());
    };
    if outstanding_authority(&mut m.tx, &submission)
        .await?
        .is_some()
    {
        return Err(AppError::conflict(
            "observation_required",
            "Push authority is outstanding for this revert's candidate; observe the target first.",
        ));
    }
    let (task, actor): (String, String) = (revert.get("task_id"), m.actor.id.clone());
    let reason = "The integrator could not revert mechanically.";
    let target = Supersede {
        project: p,
        task: &task,
        submission: &submission,
        actor: &actor,
        reason,
    };
    supersede_submission(&mut m.tx, &target, m.now).await
}

/// Description and acceptance criteria of a revert converted to
/// implementation work.
fn conversion_fields(revert: &SqliteRow, input: &NotMechanicalInput) -> (String, Value) {
    let (r, original): (String, String) = (revert.get("r"), revert.get("original_task_id"));
    let description = format!(
        "Undo the behaviour of task {original} (integrated result {r}) while keeping the changes integrated after it. The integrator could not revert it mechanically on {} ({}): {}\n\nRevert reason: {}. Revert evidence: {}",
        input.t0,
        input.reason,
        input.evidence,
        revert.get::<String, _>("reason"),
        evidence_text(&serde_json::from_str(revert.get("evidence_json")).unwrap_or_default())
    );
    let acceptance = json!([
        format!("The behaviour introduced by result {r} is undone"),
        format!("Changes integrated after result {r} are kept"),
        "The target's required checks pass on the integrated result",
    ]);
    (description, acceptance)
}

/// Rewrites the revert as ordinary implementation work that needs review.
async fn convert(
    m: &mut Mutation,
    p: &str,
    revert: &SqliteRow,
    input: &NotMechanicalInput,
) -> Result<(), AppError> {
    let task: String = revert.get("task_id");
    let (description, acceptance) = conversion_fields(revert, input);
    sqlx::query("UPDATE tasks SET description=?,acceptance_json=?,revision=revision+1,blocked_reason=NULL,ready_since=? WHERE id=?")
        .bind(&description).bind(acceptance.to_string()).bind(m.now).bind(&task).execute(&mut *m.tx).await?;
    save_task_revision(m, p, &task).await?;
    let report = json!({"t0": input.t0, "reason": input.reason, "evidence": input.evidence,
        "reported_by": m.actor.id, "reported_at": coordinator_core::timestamp(m.now)});
    sqlx::query("UPDATE task_reverts SET mode='not_mechanical',not_mechanical_json=?,review_required=1 WHERE task_id=?")
        .bind(report.to_string()).bind(&task).execute(&mut *m.tx).await?;
    Ok(())
}

/// Replays a conversion already recorded with the same tip and reason;
/// any other report for a converted revert conflicts.
fn replay_conversion(revert: &SqliteRow, input: &NotMechanicalInput) -> Result<(), AppError> {
    let stored: Option<String> = revert.get("not_mechanical_json");
    let stored: Value = serde_json::from_str(stored.as_deref().unwrap_or("{}"))?;
    if stored["t0"] == input.t0.as_str() && stored["reason"] == input.reason.as_str() {
        return Ok(());
    }
    Err(AppError::conflict(
        "revert_not_mechanical",
        "This revert was already converted to implementation work.",
    ))
}

/// Converts the revert unless it is already converted (see
/// [`replay_conversion`]).
async fn convert_once(
    m: &mut Mutation,
    p: &str,
    id: &str,
    input: &NotMechanicalInput,
) -> Result<(), AppError> {
    let revert = load_revert(&mut m.tx, p, id).await?;
    if revert.get::<String, _>("mode") == "not_mechanical" {
        return replay_conversion(&revert, input);
    }
    if revert.get::<String, _>("lifecycle") != "open" {
        return Err(AppError::conflict("revert_not_open", NOT_OPEN_MESSAGE));
    }
    withdraw_candidate(m, p, &revert).await?;
    convert(m, p, &revert, input).await
}

/// `POST …/integrator/reverts/{id}/not-mechanical`: the integrator cannot
/// revert mechanically; the revert becomes implementation work with review.
async fn not_mechanical(
    State(s): State<AppState>,
    auth: Auth,
    Path((p, id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<NotMechanicalInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    input.validate()?;
    let op = format!("POST /api/v1/projects/{p}/integrator/reverts/{id}/not-mechanical");
    let mut m = Mutation::begin(&s, &auth, &headers, &op, &input).await?;
    if let Some(v) = m.replay.take() {
        return Ok(response(v));
    }
    require_integrator_project(&mut m.tx, &p).await?;
    convert_once(&mut m, &p, &id, &input).await?;
    let mut value = task_record_value(&mut m.tx, &p, &id, m.now).await?;
    value["revert"] = revert_view(&mut m.tx, &id).await?;
    Ok(response(
        m.finish(value, Some(&p), "revert.not_mechanical", &id)
            .await?,
    ))
}

/// The published revert of `task` when its reason is `defect` and it has no
/// re-land task yet, with the original task and candidate.
async fn defect_revert(
    c: &mut SqliteConnection,
    task: &str,
) -> Result<Option<SqliteRow>, AppError> {
    Ok(sqlx::query("SELECT tr.*,o.title,o.description,o.acceptance_json,o.kind,o.priority,s.candidate_ref,s.candidate_revision FROM task_reverts tr JOIN tasks o ON o.id=tr.original_task_id JOIN submissions s ON s.id=tr.submission_id WHERE tr.task_id=? AND tr.reason='defect' AND tr.reland_task_id IS NULL")
        .bind(task).fetch_optional(&mut *c).await?)
}

/// Description of a re-land task: the original's, then its candidate
/// reference and the revert that undid it.
fn reland_description(revert: &SqliteRow) -> String {
    let candidate = revert
        .get::<Option<String>, _>("candidate_ref")
        .or_else(|| revert.get("candidate_revision"))
        .unwrap_or_default();
    format!(
        "{}\n\nRe-land of task {} after its integrated result {} (candidate {candidate}) was reverted by task {} for a defect. Re-land candidates must be new commits (cherry-pick or a revert of the revert); the reverted commits themselves are refused as candidate_reverted_in_history.",
        clip(revert.get("description"), 30_000),
        revert.get::<String, _>("original_task_id"),
        revert.get::<String, _>("r"),
        revert.get::<String, _>("task_id")
    )
}

/// Acceptance criteria of a re-land task: the original's, then the revert
/// evidence as the defect to fix.
fn reland_acceptance(revert: &SqliteRow) -> Result<Value, AppError> {
    let evidence = evidence_text(&serde_json::from_str(revert.get("evidence_json"))?);
    let fixed = format!("The defect that caused the revert is fixed: {evidence}");
    let mut acceptance: Vec<Value> = serde_json::from_str(revert.get("acceptance_json"))?;
    acceptance.push(json!(clip(&fixed, 2048)));
    Ok(json!(acceptance))
}

/// After a revert of `task` is published: proposes (as a planned task) a
/// re-land of the original when the revert's reason is `defect`.
pub(crate) async fn propose_reland(m: &mut Mutation, p: &str, task: &str) -> Result<(), AppError> {
    let Some(revert) = defect_revert(&mut m.tx, task).await? else {
        return Ok(());
    };
    let title = clip(
        &format!("Re-land: {}", revert.get::<String, _>("title")),
        300,
    );
    let (description, acceptance) = (reland_description(&revert), reland_acceptance(&revert)?);
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO tasks(id,project_id,title,description,acceptance_json,kind,priority,lifecycle,created_at,ready_since) VALUES(?,?,?,?,?,?,?,'planned',?,?)")
        .bind(&id).bind(p).bind(&title).bind(&description).bind(acceptance.to_string())
        .bind(revert.get::<String, _>("kind")).bind(revert.get::<i64, _>("priority")).bind(m.now).bind(m.now)
        .execute(&mut *m.tx).await?;
    save_task_revision(m, p, &id).await?;
    sqlx::query("UPDATE task_reverts SET reland_task_id=? WHERE task_id=?")
        .bind(&id)
        .bind(task)
        .execute(&mut *m.tx)
        .await?;
    Ok(())
}
