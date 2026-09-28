//! Integrator observations and revises (planning p4-design §2 "Observations"
//! and "Revise", step S2; plan-final §2.4 roll-forward reconciliation).
//!
//! The integrator attests what it saw at the target after (or instead of) a
//! push: R `contained` in the tip, the tip still `equal_t0`, or the tip
//! `moved` elsewhere. The service cannot reach GitHub (p4-design §0), so it
//! trusts that attestation and records it. A revise that arrives while push
//! authority is outstanding is deferred: it applies if the push did not land
//! and becomes a follow-up task if it did ("revise loses to a landed push").
use crate::{
    auth::Auth,
    error::AppError,
    integrator::{require_integrator_project, result_value},
    mutation::Mutation,
    response,
    state::AppState,
    workflow::{Supersede, bounded, payload, revision, supersede_submission},
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

/// Revise reasons the integrator itself may give.
const INTEGRATOR_REASONS: &[&str] = &["conflict", "check_failed"];

/// The observation and integrator revise routes.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/projects/{p}/integrator/observations",
            post(observe),
        )
        .route("/api/v1/projects/{p}/integrator/revise", post(revise))
}

/// The result holding outstanding push authority for `submission`, if any.
pub(crate) async fn outstanding_authority(
    c: &mut SqliteConnection,
    submission: &str,
) -> Result<Option<String>, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT id FROM integrator_results WHERE submission_id=? AND authority_issued_at IS NOT NULL LIMIT 1",
    )
    .bind(submission)
    .fetch_optional(&mut *c)
    .await?)
}

/// An agent revise that may have to wait for an outstanding push.
pub(crate) struct ReviseDeferral<'a> {
    pub submission: &'a str,
    pub requested_by: &'a str,
    pub reason: &'a str,
    pub reason_code: &'a str,
    pub evidence: Option<&'a str>,
}

/// Records the revise for later when push authority is outstanding and returns
/// the response body; `None` when the revise may apply now.
pub(crate) async fn defer_revise(
    c: &mut SqliteConnection,
    d: &ReviseDeferral<'_>,
    now: i64,
) -> Result<Option<Value>, AppError> {
    let Some(result) = outstanding_authority(c, d.submission).await? else {
        return Ok(None);
    };
    let pending: i64 =
        sqlx::query_scalar("SELECT count(*) FROM integrator_revise_requests WHERE submission_id=?")
            .bind(d.submission)
            .fetch_one(&mut *c)
            .await?;
    if pending > 0 {
        return Err(AppError::conflict(
            "revise_already_pending",
            "A revise already waits for this submission's outstanding push.",
        ));
    }
    sqlx::query("INSERT INTO integrator_revise_requests(submission_id,result_id,requested_by,reason,reason_code,evidence,requested_at) VALUES(?,?,?,?,?,?,?)")
        .bind(d.submission).bind(&result).bind(d.requested_by).bind(d.reason)
        .bind(d.reason_code).bind(d.evidence).bind(now).execute(&mut *c).await?;
    Ok(Some(json!({
        "revise_deferred": true,
        "result_id": result,
        "reason_code": d.reason_code,
        "message": "Push authority is outstanding: the revise applies if the push does not land, and becomes a follow-up task if it does.",
    })))
}

/// What the integrator saw at the target for one result.
#[derive(Deserialize, Serialize)]
struct ObservationInput {
    result_id: String,
    tip: String,
    ancestry: String,
    evidence: String,
}

impl ObservationInput {
    /// Checks every identity and bound before any state is read.
    fn validate(&self) -> Result<(), AppError> {
        bounded(&self.result_id, "result_id", 100, true)?;
        revision(&self.tip, "tip")?;
        bounded(&self.evidence, "evidence", 16384, true)?;
        if !["contained", "equal_t0", "moved"].contains(&self.ancestry.as_str()) {
            return Err(AppError::bad_request(
                "ancestry must be contained, equal_t0 or moved.",
            ));
        }
        Ok(())
    }
}

/// A result joined with its subject's current state.
macro_rules! observed_sql {
    () => {
        "SELECT r.*,s.task_id,s.superseded_at,ws.phase,ws.current_submission_id \
         FROM integrator_results r JOIN submissions s ON s.id=r.submission_id \
         JOIN workflow_subjects ws ON ws.task_id=s.task_id WHERE r.id=? AND r.project_id=?"
    };
}

/// Loads the observed result, refusing one whose subject is already done.
async fn observed_result(
    c: &mut SqliteConnection,
    p: &str,
    id: &str,
) -> Result<SqliteRow, AppError> {
    let row = sqlx::query(observed_sql!())
        .bind(id)
        .bind(p)
        .fetch_optional(&mut *c)
        .await?
        .ok_or_else(AppError::not_found)?;
    if row.get::<String, _>("phase") == "done" {
        return Err(AppError::conflict(
            "result_already_published",
            "This subject is already integrated; nothing is left to observe.",
        ));
    }
    Ok(row)
}

/// True while the result's submission is still its subject's current one.
fn still_current(row: &SqliteRow) -> bool {
    row.get::<Option<i64>, _>("superseded_at").is_none()
        && row.get::<String, _>("current_submission_id") == row.get::<String, _>("submission_id")
}

/// Classifies the attested ancestry, refusing tips that contradict it.
fn disposition(row: &SqliteRow, input: &ObservationInput) -> Result<&'static str, AppError> {
    let (t0, r) = (row.get::<String, _>("t0"), row.get::<String, _>("r"));
    let contradicts = match input.ancestry.as_str() {
        "equal_t0" => input.tip != t0,
        "moved" => input.tip == t0 || input.tip == r,
        _ => false,
    };
    if contradicts {
        return Err(AppError::bad_request(
            "tip contradicts ancestry: equal_t0 needs tip == t0; moved needs a tip other than t0 and r.",
        ));
    }
    Ok(match input.ancestry.as_str() {
        "contained" if !still_current(row) => "published_after_reopen",
        "contained" if r == t0 => "already_contained",
        "contained" => "published",
        "equal_t0" => "not_published",
        _ => "target_moved",
    })
}

/// Stores the observation row.
async fn insert_observation(
    m: &mut Mutation,
    input: &ObservationInput,
    disposition: &str,
) -> Result<(), AppError> {
    sqlx::query("INSERT INTO integrator_observations(id,result_id,tip,ancestry,disposition,evidence,observed_by,observed_at) VALUES(?,?,?,?,?,?,?,?)")
        .bind(uuid::Uuid::new_v4().to_string()).bind(&input.result_id).bind(&input.tip)
        .bind(&input.ancestry).bind(disposition).bind(&input.evidence)
        .bind(&m.actor.id).bind(m.now).execute(&mut *m.tx).await?;
    Ok(())
}

/// Ends push authority on the result and releases the submission's hold.
async fn end_authority(m: &mut Mutation, row: &SqliteRow, why: &str) -> Result<(), AppError> {
    let submission: String = row.get("submission_id");
    sqlx::query("UPDATE integrator_results SET authority_issued_at=NULL,authority_expires_at=NULL WHERE submission_id=?")
        .bind(&submission).execute(&mut *m.tx).await?;
    sqlx::query("UPDATE integration_holds SET state='released',released_by=?,released_at=?,release_reason=? WHERE activity_id IN (SELECT id FROM workflow_activities WHERE submission_id=?) AND state='held'")
        .bind(&m.actor.id).bind(m.now).bind(why).bind(&submission).execute(&mut *m.tx).await?;
    Ok(())
}

/// Marks the subject and its integration activity done and readies dependents.
async fn complete_subject(m: &mut Mutation, row: &SqliteRow) -> Result<(), AppError> {
    let (task, submission): (String, String) = (row.get("task_id"), row.get("submission_id"));
    sqlx::query("UPDATE tasks SET lifecycle='done',current_attempt_id=NULL,blocked_reason=NULL WHERE id=? OR id IN (SELECT activity_task_id FROM workflow_activities WHERE submission_id=? AND kind='integration')")
        .bind(&task).bind(&submission).execute(&mut *m.tx).await?;
    sqlx::query("UPDATE workflow_activities SET state='completed',completed_at=? WHERE submission_id=? AND kind='integration' AND state IN ('queued','active')")
        .bind(m.now).bind(&submission).execute(&mut *m.tx).await?;
    sqlx::query("UPDATE workflow_subjects SET phase='done',updated_at=? WHERE task_id=?")
        .bind(m.now)
        .bind(&task)
        .execute(&mut *m.tx)
        .await?;
    crate::workflow::ready_dependents(&mut m.tx, &task, m.now).await
}

/// The unresolved revise request waiting on this submission, if any.
async fn pending_revise(
    c: &mut SqliteConnection,
    submission: &str,
) -> Result<Option<SqliteRow>, AppError> {
    Ok(sqlx::query(
        "SELECT * FROM integrator_revise_requests WHERE submission_id=? AND resolved_at IS NULL",
    )
    .bind(submission)
    .fetch_optional(&mut *c)
    .await?)
}

/// Marks a revise request resolved.
async fn resolve_revise(
    m: &mut Mutation,
    submission: &str,
    resolution: &str,
    follow_up: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query("UPDATE integrator_revise_requests SET resolved_at=?,resolution=?,follow_up_task_id=? WHERE submission_id=?")
        .bind(m.now).bind(resolution).bind(follow_up).bind(submission)
        .execute(&mut *m.tx).await?;
    Ok(())
}

/// Title, priority and description of the follow-up for a revise that lost.
fn follow_up_fields(subject: &SqliteRow, request: &SqliteRow, r: &str) -> (String, i64, String) {
    let title: String = subject.get("title");
    let code: String = request.get("reason_code");
    let (title, priority) = if code == "author_withdraw" {
        (format!("Revert: {title}"), 0)
    } else {
        (format!("Follow up: {title}"), subject.get("priority"))
    };
    let description = format!(
        "A {code} revise arrived while {r} was being pushed, and the push landed. Reason: {}\n\nEvidence: {}",
        request.get::<String, _>("reason"),
        request
            .get::<Option<String>, _>("evidence")
            .unwrap_or_default()
    );
    (title.chars().take(300).collect(), priority, description)
}

/// Creates the follow-up task for a revise that lost to a landed push.
async fn create_follow_up(
    m: &mut Mutation,
    p: &str,
    row: &SqliteRow,
    request: &SqliteRow,
) -> Result<String, AppError> {
    let subject = sqlx::query("SELECT title,priority,kind FROM tasks WHERE id=?")
        .bind(row.get::<String, _>("task_id"))
        .fetch_one(&mut *m.tx)
        .await?;
    let (title, priority, description) = follow_up_fields(&subject, request, row.get("r"));
    let id = uuid::Uuid::new_v4().to_string();
    let acceptance = json!(["The revise request that lost to the landed push is addressed"]);
    sqlx::query("INSERT INTO tasks(id,project_id,title,description,acceptance_json,kind,priority,lifecycle,created_at,ready_since) VALUES(?,?,?,?,?,?,?,'open',?,?)")
        .bind(&id).bind(p).bind(&title).bind(&description).bind(acceptance.to_string())
        .bind(subject.get::<String, _>("kind")).bind(priority).bind(m.now).bind(m.now)
        .execute(&mut *m.tx).await?;
    crate::coordination::save_task_revision(m, p, &id).await?;
    Ok(id)
}

/// Applies a deferred revise now that the push did not land.
async fn apply_revise(
    m: &mut Mutation,
    p: &str,
    row: &SqliteRow,
    request: &SqliteRow,
) -> Result<(), AppError> {
    let (task, submission): (String, String) = (row.get("task_id"), row.get("submission_id"));
    let (actor, reason): (String, String) = (request.get("requested_by"), request.get("reason"));
    let target = Supersede {
        project: p,
        task: &task,
        submission: &submission,
        actor: &actor,
        reason: &reason,
    };
    supersede_submission(&mut m.tx, &target, m.now).await?;
    resolve_revise(m, &submission, "applied", None).await
}

/// Settles a deferred revise after the observation: a follow-up task when R
/// landed, the revise itself when it did not. Returns what happened.
async fn settle_revise(
    m: &mut Mutation,
    p: &str,
    row: &SqliteRow,
    landed: bool,
) -> Result<Value, AppError> {
    let submission: String = row.get("submission_id");
    let Some(request) = pending_revise(&mut m.tx, &submission).await? else {
        return Ok(Value::Null);
    };
    if !landed {
        apply_revise(m, p, row, &request).await?;
        return Ok(json!({"resolution": "applied"}));
    }
    let task = create_follow_up(m, p, row, &request).await?;
    resolve_revise(m, &submission, "follow_up", Some(&task)).await?;
    Ok(json!({"resolution": "follow_up", "follow_up_task_id": task}))
}

/// Applies the state change a disposition implies and returns the revise
/// outcome.
async fn apply_disposition(
    m: &mut Mutation,
    p: &str,
    row: &SqliteRow,
    disposition: &str,
) -> Result<Value, AppError> {
    end_authority(m, row, &format!("Integrator observation: {disposition}.")).await?;
    match disposition {
        "published" | "already_contained" => {
            complete_subject(m, row).await?;
            settle_revise(m, p, row, true).await
        }
        "published_after_reopen" => Ok(Value::Null),
        _ => settle_revise(m, p, row, false).await,
    }
}

/// `POST …/integrator/observations`: records an attested tip observation.
async fn observe(
    State(s): State<AppState>,
    auth: Auth,
    Path(p): Path<String>,
    headers: HeaderMap,
    body: Result<Json<ObservationInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    input.validate()?;
    let op = format!("POST /api/v1/projects/{p}/integrator/observations");
    let mut m = Mutation::begin(&s, &auth, &headers, &op, &input).await?;
    if let Some(v) = m.replay.take() {
        return Ok(response(v));
    }
    require_integrator_project(&mut m.tx, &p).await?;
    let row = observed_result(&mut m.tx, &p, &input.result_id).await?;
    let disposition = disposition(&row, &input)?;
    insert_observation(&mut m, &input, disposition).await?;
    let revise = apply_disposition(&mut m, &p, &row, disposition).await?;
    let value = json!({"result": result_value(&row)?, "tip": input.tip,
        "disposition": disposition, "revise": revise});
    Ok(response(
        m.finish(value, Some(&p), "integrator.observed", &input.result_id)
            .await?,
    ))
}

/// The integrator's own revise: a conflict or a reproduced check failure.
#[derive(Deserialize, Serialize)]
struct IntegratorReviseInput {
    submission_id: String,
    reason_code: String,
    evidence: String,
}

impl IntegratorReviseInput {
    /// Checks the reason code and evidence bounds.
    fn validate(&self) -> Result<(), AppError> {
        bounded(&self.submission_id, "submission_id", 100, true)?;
        bounded(&self.evidence, "evidence", 16384, true)?;
        if !INTEGRATOR_REASONS.contains(&self.reason_code.as_str()) {
            return Err(AppError::bad_request(
                "The integrator revises only with reason_code conflict or check_failed.",
            ));
        }
        Ok(())
    }
}

/// The subject task of a submission awaiting integration in this project.
async fn integrating_task(
    c: &mut SqliteConnection,
    p: &str,
    submission: &str,
) -> Result<String, AppError> {
    sqlx::query_scalar("SELECT ws.task_id FROM workflow_subjects ws JOIN submissions s ON s.id=ws.current_submission_id WHERE ws.project_id=? AND s.id=? AND ws.phase='integration' AND s.superseded_at IS NULL")
        .bind(p).bind(submission).fetch_optional(&mut *c).await?
        .ok_or_else(|| AppError::conflict("subject_not_integrable", "This submission is not a current candidate awaiting integration."))
}

/// Refuses an integrator revise that must first observe its own push, or that
/// has hit the per-subject limit (the subject then parks for a human).
async fn ensure_revisable(
    c: &mut SqliteConnection,
    task: &str,
    submission: &str,
    now: i64,
) -> Result<(), AppError> {
    if outstanding_authority(c, submission).await?.is_some() {
        return Err(AppError::conflict(
            "observation_required",
            "Push authority is outstanding for this submission; observe the target first.",
        ));
    }
    if crate::autonomy::revise_limit_reached(c, task, now).await? {
        return Err(AppError::forbidden(
            "Agents revised this task three times in 24 hours; a human must look at it.",
        )
        .with_details(json!({"required_actor":"human","gate":"revise_limit_reached"})));
    }
    Ok(())
}

/// `POST …/integrator/revise`: sends a conflicting or check-failing candidate
/// back to its author.
async fn revise(
    State(s): State<AppState>,
    auth: Auth,
    Path(p): Path<String>,
    headers: HeaderMap,
    body: Result<Json<IntegratorReviseInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    input.validate()?;
    let op = format!("POST /api/v1/projects/{p}/integrator/revise");
    let mut m = Mutation::begin(&s, &auth, &headers, &op, &input).await?;
    if let Some(v) = m.replay.take() {
        return Ok(response(v));
    }
    require_integrator_project(&mut m.tx, &p).await?;
    let task = integrating_task(&mut m.tx, &p, &input.submission_id).await?;
    ensure_revisable(&mut m.tx, &task, &input.submission_id, m.now).await?;
    let reason = format!("Integrator revise: {}", input.reason_code);
    let target = Supersede {
        project: &p,
        task: &task,
        submission: &input.submission_id,
        actor: &m.actor.id,
        reason: &reason,
    };
    supersede_submission(&mut m.tx, &target, m.now).await?;
    let value = json!({"subject_task_id": task, "submission_id": input.submission_id,
        "revise": {"reason_code": input.reason_code, "evidence": input.evidence}});
    Ok(response(
        m.finish(value, Some(&p), "submission.reopened", &input.submission_id)
            .await?,
    ))
}
