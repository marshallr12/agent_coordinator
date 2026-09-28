//! Service side of the deterministic integrator (planning p4-design §2, step
//! S1): the integration queue (which doubles as the integrator heartbeat),
//! pinned integration results, and GitHub Actions check receipts.
//!
//! Only projects whose `integration_owner` is `integrator` are served, and
//! only an integrator-class credential may call these routes (the reverse is
//! enforced for writes in `Mutation::begin`). Push authority lives in
//! `integrator_authority`, observations and revises in `integrator_observe`,
//! reports in `integrator_reports`; on integrator projects the agent integration routes refuse with
//! `integration_owned_by_integrator`.
use crate::{
    auth::{Actor, Auth},
    credential_attributes::is_integrator,
    error::AppError,
    mutation::Mutation,
    response,
    state::AppState,
    workflow::{bounded, payload, revision},
};
use axum::{
    Json, Router,
    extract::{Path, State, rejection::JsonRejection},
    http::HeaderMap,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection, sqlite::SqliteRow};

type Reply = Result<Json<Value>, AppError>;

/// Queue entries returned per call.
const QUEUE_LIMIT: i64 = 50;
/// Poll interval suggested to the integrator.
const RETRY_AFTER_SECONDS: i64 = 30;
/// Upper bound on commits in one landing range.
const MAX_LANDING_RANGE: usize = 1000;
/// Upper bound on the serialized roster sent with a result.
const MAX_ROSTER_BYTES: usize = 65_536;
/// GitHub check-run conclusions a receipt may carry.
const CONCLUSIONS: &[&str] = &[
    "success",
    "failure",
    "cancelled",
    "timed_out",
    "neutral",
    "skipped",
    "action_required",
    "stale",
    "startup_failure",
];

/// Subjects in integration with their current, unsuperseded code submission.
/// A macro so each query stays one compile-time literal (sqlx refuses
/// runtime-built SQL).
macro_rules! subjects_sql {
    () => {
        "SELECT ws.task_id,s.id AS submission_id,s.candidate_revision,s.candidate_tree,\
         s.candidate_ref,s.base_revision,s.repository_url,s.target_branch,\
         s.canonical_repository_key,s.task_digest,t.title,t.description,t.acceptance_json,\
         t.kind AS subject_kind,t.priority,ws.updated_at FROM workflow_subjects ws \
         JOIN submissions s ON s.id=ws.current_submission_id JOIN tasks t ON t.id=ws.task_id \
         WHERE ws.project_id=? AND ws.phase='integration' AND s.kind='code' \
         AND s.superseded_at IS NULL"
    };
}

/// The integrator API routes.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/projects/{p}/integrator/queue", get(queue))
        .route(
            "/api/v1/projects/{p}/integrator/results",
            post(record_result),
        )
        .route(
            "/api/v1/projects/{p}/integrator/receipts",
            post(record_receipt),
        )
}

/// Refuses callers that do not hold an integrator credential.
pub(crate) fn require_integrator(actor: &Actor) -> Result<(), AppError> {
    if is_integrator(actor) {
        Ok(())
    } else {
        Err(AppError::forbidden(
            "Only an integrator credential may call the integrator API.",
        ))
    }
}

/// Refuses projects that have not handed integration to the integrator.
pub(crate) async fn require_integrator_project(
    c: &mut SqliteConnection,
    p: &str,
) -> Result<(), AppError> {
    let owner: Option<String> =
        sqlx::query_scalar("SELECT integration_owner FROM projects WHERE id=?")
            .bind(p)
            .fetch_optional(&mut *c)
            .await?;
    match owner.as_deref() {
        None => Err(AppError::not_found()),
        Some("integrator") => Ok(()),
        Some(_) => Err(AppError::conflict(
            "integration_owned_by_agents",
            "This project's integration_owner is agent; a human must switch it to integrator first.",
        )),
    }
}

/// True when the subject's approvals are satisfied and its judged task fields
/// still match the digest pinned at submission.
async fn eligible(c: &mut SqliteConnection, row: &SqliteRow) -> Result<bool, AppError> {
    let pinned: Option<String> = row.get("task_digest");
    let current = crate::autonomy::row_digest(row, "subject_kind")?;
    if pinned.is_some_and(|pinned| pinned != current) {
        return Ok(false);
    }
    crate::autonomy::approvals_satisfied(c, row.get("submission_id")).await
}

/// The current required-check roster: its revision and check list.
async fn current_roster(c: &mut SqliteConnection, p: &str) -> Result<Value, AppError> {
    let row = sqlx::query(
        "SELECT revision,required_checks_json FROM workflow_policies WHERE project_id=?",
    )
    .bind(p)
    .fetch_optional(&mut *c)
    .await?;
    Ok(match row {
        Some(r) => json!({"revision": r.get::<i64, _>("revision"),
            "required_checks": serde_json::from_str::<Value>(r.get("required_checks_json"))?}),
        None => json!({"revision": 0, "required_checks": []}),
    })
}

/// Results already recorded for one submission, oldest first.
async fn submission_results(
    c: &mut SqliteConnection,
    submission: &str,
) -> Result<Vec<Value>, AppError> {
    let rows = sqlx::query(
        "SELECT * FROM integrator_results WHERE submission_id=? ORDER BY created_at,id",
    )
    .bind(submission)
    .fetch_all(&mut *c)
    .await?;
    rows.iter().map(result_value).collect()
}

/// One queue entry as served to the integrator.
fn queue_item(row: &SqliteRow, results: Vec<Value>) -> Value {
    json!({
        "subject_task_id": row.get::<String, _>("task_id"),
        "submission_id": row.get::<String, _>("submission_id"),
        "title": row.get::<String, _>("title"),
        "priority": row.get::<i64, _>("priority"),
        "candidate_revision": row.get::<String, _>("candidate_revision"),
        "candidate_tree": row.get::<String, _>("candidate_tree"),
        "candidate_ref": row.get::<Option<String>, _>("candidate_ref"),
        "reviewed_base": row.get::<String, _>("base_revision"),
        "repository_url": row.get::<String, _>("repository_url"),
        "target_branch": row.get::<String, _>("target_branch"),
        "task_digest": row.get::<Option<String>, _>("task_digest"),
        "results": results,
    })
}

/// Records the integrator heartbeat on the project.
async fn heartbeat(c: &mut SqliteConnection, p: &str, now: i64) -> Result<(), AppError> {
    sqlx::query("UPDATE projects SET integrator_last_seen=? WHERE id=?")
        .bind(now)
        .bind(p)
        .execute(&mut *c)
        .await?;
    Ok(())
}

/// Eligible subjects in subject-priority order, then by time in integration.
pub(crate) async fn eligible_items(
    c: &mut SqliteConnection,
    p: &str,
) -> Result<(Vec<Value>, u64), AppError> {
    let sql = concat!(
        subjects_sql!(),
        " ORDER BY t.priority,ws.updated_at,ws.task_id LIMIT ?"
    );
    let rows = sqlx::query(sql)
        .bind(p)
        .bind(QUEUE_LIMIT)
        .fetch_all(&mut *c)
        .await?;
    let (mut items, mut skipped) = (Vec::new(), 0);
    for row in &rows {
        if !eligible(c, row).await? {
            skipped += 1;
            continue;
        }
        let results = submission_results(c, row.get("submission_id")).await?;
        items.push(queue_item(row, results));
    }
    Ok((items, skipped))
}

/// The distinct (repository_url, target_branch) pairs the project integrates
/// into: the project's configured target first, then any other target a
/// queued item's submission pinned.
async fn queue_targets(
    c: &mut SqliteConnection,
    p: &str,
    items: &[Value],
) -> Result<Vec<Value>, AppError> {
    let project = sqlx::query("SELECT repository_url,target_branch FROM projects WHERE id=?")
        .bind(p)
        .fetch_one(&mut *c)
        .await?;
    let configured = json!({"repository_url": project.get::<String, _>("repository_url"),
        "target_branch": project.get::<String, _>("target_branch")});
    let mut targets = vec![configured];
    for item in items {
        let target = json!({"repository_url": item["repository_url"],
            "target_branch": item["target_branch"]});
        if !targets.contains(&target) {
            targets.push(target);
        }
    }
    Ok(targets)
}

/// `GET …/integrator/queue`: approved, pins-current subjects awaiting
/// integration, and the targets the integrator watches even when no item is
/// queued. Each call also records the integrator heartbeat.
async fn queue(State(s): State<AppState>, auth: Auth, Path(p): Path<String>) -> Reply {
    require_integrator(&auth.actor)?;
    let mut c = s.pool.acquire().await?;
    require_integrator_project(&mut c, &p).await?;
    heartbeat(&mut c, &p, s.now()).await?;
    let (items, skipped) = eligible_items(&mut c, &p).await?;
    let roster = current_roster(&mut c, &p).await?;
    let targets = queue_targets(&mut c, &p, &items).await?;
    Ok(response(json!({
        "project_id": p,
        "roster": roster,
        "targets": targets,
        "items": items,
        "skipped_ineligible": skipped,
        "retry_after_seconds": RETRY_AFTER_SECONDS,
    })))
}

/// A pinned integration result R computed for submission S at target tip T0.
#[derive(Deserialize, Serialize)]
struct ResultInput {
    submission_id: String,
    t0: String,
    t0_tree: String,
    c: String,
    r: String,
    r_tree: String,
    landing_range: Vec<String>,
    roster: Value,
}

impl ResultInput {
    /// Checks every identity and bound before any state is read.
    fn validate(&self) -> Result<(), AppError> {
        bounded(&self.submission_id, "submission_id", 100, true)?;
        for (value, name) in [
            (&self.t0, "t0"),
            (&self.t0_tree, "t0_tree"),
            (&self.c, "c"),
            (&self.r, "r"),
            (&self.r_tree, "r_tree"),
        ] {
            revision(value, name)?;
        }
        self.validate_bounds()
    }

    /// Bounds on the landing range and the roster.
    fn validate_bounds(&self) -> Result<(), AppError> {
        if self.landing_range.len() > MAX_LANDING_RANGE {
            return Err(AppError::bad_request(
                "landing_range holds at most 1000 commits.",
            ));
        }
        self.landing_range
            .iter()
            .try_for_each(|sha| revision(sha, "landing_range entry"))?;
        if !self.roster.is_object() || self.roster.to_string().len() > MAX_ROSTER_BYTES {
            return Err(AppError::bad_request(
                "roster must be a JSON object of at most 64 KiB.",
            ));
        }
        crate::integrator_authority::parse_roster(&self.roster).map(|_| ())
    }
}

/// Serializes one stored result row. `authority_expires_at` is set while
/// push authority is outstanding (until an observation ends it), so the
/// integrator can observe a held result before pinning a newer one.
pub(crate) fn result_value(row: &SqliteRow) -> Result<Value, AppError> {
    Ok(json!({
        "id": row.get::<String, _>("id"),
        "submission_id": row.get::<String, _>("submission_id"),
        "t0": row.get::<String, _>("t0"),
        "t0_tree": row.get::<String, _>("t0_tree"),
        "c": row.get::<String, _>("c"),
        "r": row.get::<String, _>("r"),
        "r_tree": row.get::<String, _>("r_tree"),
        "landing_range": serde_json::from_str::<Value>(row.get("landing_range_json"))?,
        "roster": serde_json::from_str::<Value>(row.get("roster_json"))?,
        "created_at": coordinator_core::timestamp(row.get("created_at")),
        "authority_expires_at": row
            .get::<Option<i64>, _>("authority_expires_at")
            .map(coordinator_core::timestamp),
    }))
}

/// Loads the eligible subject that `submission` is current for, or refuses.
pub(crate) async fn eligible_subject(
    c: &mut SqliteConnection,
    p: &str,
    submission: &str,
) -> Result<SqliteRow, AppError> {
    let sql = concat!(subjects_sql!(), " AND s.id=?");
    let row = sqlx::query(sql)
        .bind(p)
        .bind(submission)
        .fetch_optional(&mut *c)
        .await?;
    match row {
        Some(row) if eligible(c, &row).await? => Ok(row),
        _ => Err(AppError::conflict(
            "subject_not_integrable",
            "This submission is not an approved, current candidate awaiting integration.",
        )),
    }
}

/// The stored result for (submission, t0), if one exists.
async fn existing_result(
    c: &mut SqliteConnection,
    input: &ResultInput,
) -> Result<Option<SqliteRow>, AppError> {
    Ok(
        sqlx::query("SELECT * FROM integrator_results WHERE submission_id=? AND t0=?")
            .bind(&input.submission_id)
            .bind(&input.t0)
            .fetch_optional(&mut *c)
            .await?,
    )
}

/// Inserts a new result row and returns its id.
async fn insert_result(m: &mut Mutation, p: &str, input: &ResultInput) -> Result<String, AppError> {
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO integrator_results(id,project_id,submission_id,t0,t0_tree,c,r,r_tree,landing_range_json,roster_json,created_by,created_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)")
        .bind(&id).bind(p).bind(&input.submission_id).bind(&input.t0).bind(&input.t0_tree)
        .bind(&input.c).bind(&input.r).bind(&input.r_tree)
        .bind(serde_json::to_string(&input.landing_range)?).bind(input.roster.to_string())
        .bind(&m.actor.id).bind(m.now).execute(&mut *m.tx).await?;
    Ok(id)
}

/// Replays an identical result; a different R for the same key is a conflict.
fn replay_result(row: &SqliteRow, input: &ResultInput) -> Result<Value, AppError> {
    if row.get::<String, _>("r") != input.r || row.get::<String, _>("c") != input.c {
        return Err(AppError::conflict(
            "result_conflict",
            "A different result is already pinned for this submission and target tip.",
        ));
    }
    result_value(row)
}

/// Stores the result unless an identical one is already pinned.
async fn store_result(m: &mut Mutation, p: &str, input: &ResultInput) -> Result<Value, AppError> {
    let row = eligible_subject(&mut m.tx, p, &input.submission_id).await?;
    if row.get::<String, _>("candidate_revision") != input.c {
        return Err(AppError::conflict(
            "candidate_changed",
            "c is not this submission's candidate revision; read the queue again.",
        ));
    }
    if let Some(existing) = existing_result(&mut m.tx, input).await? {
        return replay_result(&existing, input);
    }
    let id = insert_result(m, p, input).await?;
    let row = sqlx::query("SELECT * FROM integrator_results WHERE id=?")
        .bind(&id)
        .fetch_one(&mut *m.tx)
        .await?;
    result_value(&row)
}

/// `POST …/integrator/results`: pins R for (submission, T0), idempotently.
async fn record_result(
    State(s): State<AppState>,
    auth: Auth,
    Path(p): Path<String>,
    headers: HeaderMap,
    body: Result<Json<ResultInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    input.validate()?;
    let op = format!("POST /api/v1/projects/{p}/integrator/results");
    let mut m = Mutation::begin(&s, &auth, &headers, &op, &input).await?;
    if let Some(v) = m.replay.take() {
        return Ok(response(v));
    }
    require_integrator_project(&mut m.tx, &p).await?;
    let value = store_result(&mut m, &p, &input).await?;
    let record = value["id"].as_str().unwrap_or_default().to_owned();
    Ok(response(
        m.finish(value, Some(&p), "integrator.result_recorded", &record)
            .await?,
    ))
}

/// One observed GitHub Actions check run for a result's R.
#[derive(Deserialize, Serialize)]
struct ReceiptInput {
    result_id: String,
    check_name: String,
    run_id: i64,
    run_attempt: i64,
    head_sha: String,
    app_id: i64,
    workflow_path: String,
    workflow_blob: String,
    conclusion: String,
}

impl ReceiptInput {
    /// Checks every identity and bound before any state is read.
    fn validate(&self) -> Result<(), AppError> {
        bounded(&self.result_id, "result_id", 100, true)?;
        bounded(&self.check_name, "check_name", 200, true)?;
        revision(&self.head_sha, "head_sha")?;
        revision(&self.workflow_blob, "workflow_blob")?;
        if self.run_id <= 0 || self.run_attempt <= 0 || self.app_id <= 0 {
            return Err(AppError::bad_request(
                "run_id, run_attempt and app_id must be positive.",
            ));
        }
        self.validate_run_shape()
    }

    /// The workflow file location and a completed conclusion.
    fn validate_run_shape(&self) -> Result<(), AppError> {
        if !self.workflow_path.starts_with(".github/workflows/") || self.workflow_path.len() > 300 {
            return Err(AppError::bad_request(
                "workflow_path must name a file under .github/workflows/.",
            ));
        }
        if !CONCLUSIONS.contains(&self.conclusion.as_str()) {
            return Err(AppError::bad_request(
                "conclusion must be a completed check-run conclusion.",
            ));
        }
        Ok(())
    }
}

/// Refuses a receipt whose head is not the result's R.
async fn require_receipt_head(
    c: &mut SqliteConnection,
    p: &str,
    input: &ReceiptInput,
) -> Result<(), AppError> {
    let r: Option<String> =
        sqlx::query_scalar("SELECT r FROM integrator_results WHERE id=? AND project_id=?")
            .bind(&input.result_id)
            .bind(p)
            .fetch_optional(&mut *c)
            .await?;
    match r {
        None => Err(AppError::not_found()),
        Some(r) if r == input.head_sha => Ok(()),
        Some(_) => Err(AppError::conflict(
            "receipt_head_mismatch",
            "head_sha is not the result revision R; receipts bind to R only.",
        )),
    }
}

/// The stored (conclusion, workflow blob) of one run attempt, if recorded.
async fn stored_receipt(
    c: &mut SqliteConnection,
    input: &ReceiptInput,
) -> Result<Option<(String, String)>, AppError> {
    Ok(sqlx::query_as("SELECT conclusion,workflow_blob FROM integrator_receipts WHERE result_id=? AND check_name=? AND run_id=? AND run_attempt=?")
        .bind(&input.result_id).bind(&input.check_name).bind(input.run_id).bind(input.run_attempt)
        .fetch_optional(&mut *c).await?)
}

/// Inserts the receipt; an existing run attempt must match it exactly.
async fn insert_receipt(m: &mut Mutation, input: &ReceiptInput) -> Result<(), AppError> {
    match stored_receipt(&mut m.tx, input).await? {
        Some((conclusion, blob))
            if conclusion == input.conclusion && blob == input.workflow_blob =>
        {
            return Ok(());
        }
        Some(_) => {
            return Err(AppError::conflict(
                "receipt_conflict",
                "This run attempt was already recorded with a different outcome.",
            ));
        }
        None => {}
    }
    sqlx::query("INSERT INTO integrator_receipts(result_id,check_name,run_id,run_attempt,head_sha,app_id,workflow_path,workflow_blob,conclusion,observed_at) VALUES(?,?,?,?,?,?,?,?,?,?)")
        .bind(&input.result_id).bind(&input.check_name).bind(input.run_id).bind(input.run_attempt)
        .bind(&input.head_sha).bind(input.app_id).bind(&input.workflow_path).bind(&input.workflow_blob)
        .bind(&input.conclusion).bind(m.now).execute(&mut *m.tx).await?;
    Ok(())
}

/// The deciding run for (result, check, workflow blob): the latest attempt of
/// the latest run.
async fn deciding_run(c: &mut SqliteConnection, input: &ReceiptInput) -> Result<Value, AppError> {
    let row = sqlx::query("SELECT run_id,run_attempt,conclusion FROM integrator_receipts WHERE result_id=? AND check_name=? AND workflow_blob=? ORDER BY run_id DESC,run_attempt DESC LIMIT 1")
        .bind(&input.result_id).bind(&input.check_name).bind(&input.workflow_blob)
        .fetch_one(&mut *c).await?;
    Ok(json!({
        "run_id": row.get::<i64, _>("run_id"),
        "run_attempt": row.get::<i64, _>("run_attempt"),
        "conclusion": row.get::<String, _>("conclusion"),
    }))
}

/// `POST …/integrator/receipts`: records one check run observed on R.
async fn record_receipt(
    State(s): State<AppState>,
    auth: Auth,
    Path(p): Path<String>,
    headers: HeaderMap,
    body: Result<Json<ReceiptInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    input.validate()?;
    let op = format!("POST /api/v1/projects/{p}/integrator/receipts");
    let mut m = Mutation::begin(&s, &auth, &headers, &op, &input).await?;
    if let Some(v) = m.replay.take() {
        return Ok(response(v));
    }
    require_integrator_project(&mut m.tx, &p).await?;
    require_receipt_head(&mut m.tx, &p, &input).await?;
    insert_receipt(&mut m, &input).await?;
    let deciding = deciding_run(&mut m.tx, &input).await?;
    let value = json!({"receipt": input, "deciding_run": deciding});
    let record = input.result_id.clone();
    Ok(response(
        m.finish(value, Some(&p), "integrator.receipt_recorded", &record)
            .await?,
    ))
}
