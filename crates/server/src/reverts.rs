//! Reverting integrated changes (planning plan-final §2.4a "M6", p4-design
//! §2 "Revert (M6)", step S4): the revert task, its creation paths, and how
//! the task view and review requirements read it.
//!
//! A revert is a `code` task with a `task_reverts` row naming the published
//! integration result R it undoes. A human creates one with one click (no
//! review; recorded as an escaped-defect canary event); an agent creates one
//! with evidence, and its review judges the decision and that evidence; an
//! `author_withdraw` revise that lost to a landed push creates an urgent one
//! (see `integrator_observe`). No admission limit applies to reverts. The
//! integrator computes the candidate (see `integrator_reverts`); a published
//! `defect` revert proposes a re-land task.
use crate::{
    auth::Auth,
    coordination::{save_task_revision, task_record_value},
    error::AppError,
    integrator::require_integrator_project,
    mutation::Mutation,
    response,
    revert_rules::{Creator, record_creator},
    state::AppState,
    workflow::{bounded, payload},
};
use axum::{
    Json, Router,
    extract::{Path, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
    routing::post,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection, sqlite::SqliteRow};

type Reply = Result<Json<Value>, AppError>;

/// The reasons a revert may give.
const REASONS: &[&str] = &["defect", "author_withdraw", "audit_rejection", "human"];
/// Priority of a revert created through the route when none is given.
const DEFAULT_PRIORITY: i64 = 1;
/// Priority of the revert an `author_withdraw` that lost to a push creates.
const AUTOMATIC_PRIORITY: i64 = 0;
/// Upper bound on serialized revert evidence.
const MAX_EVIDENCE_BYTES: usize = 16_384;
/// Blocked reason of a mechanical revert until the integrator records its
/// candidate, so agents do not claim it as implementation work.
pub(crate) const AWAITING_CANDIDATE: &str =
    "Waiting for the integrator to record the mechanical revert candidate.";

/// The revert route.
pub fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/projects/{p}/reverts", post(create))
}

/// A request to revert one published integration result.
#[derive(Deserialize, Serialize)]
struct RevertInput {
    result_id: String,
    reason: String,
    #[serde(default)]
    evidence: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    priority: Option<i64>,
}

impl RevertInput {
    /// Checks the reason, bounds and priority before any state is read.
    fn validate(&self) -> Result<(), AppError> {
        bounded(&self.result_id, "result_id", 100, true)?;
        bounded(
            self.note.as_deref().unwrap_or_default(),
            "note",
            4096,
            false,
        )?;
        if !REASONS.contains(&self.reason.as_str()) {
            return Err(AppError::bad_request(
                "reason must be defect, author_withdraw, audit_rejection or human.",
            ));
        }
        self.validate_bounds()
    }

    /// Bounds on the evidence size and the priority.
    fn validate_bounds(&self) -> Result<(), AppError> {
        if self.evidence.to_string().len() > MAX_EVIDENCE_BYTES {
            return Err(AppError::bad_request(
                "evidence holds at most 16 KiB of JSON.",
            ));
        }
        if !(0..=3).contains(&self.priority.unwrap_or(DEFAULT_PRIORITY)) {
            return Err(AppError::bad_request(
                "priority must be 0 (urgent) through 3 (low).",
            ));
        }
        Ok(())
    }

    /// Refuses an agent revert without a reason other than `human` and
    /// non-empty evidence; humans decide without either.
    fn authorize(&self, actor_kind: &str) -> Result<(), AppError> {
        if actor_kind == "human" {
            return Ok(());
        }
        if self.reason == "human" {
            return Err(AppError::human_gate(
                "revert_without_evidence",
                "Only a human may revert with reason human; an agent names the defect, audit rejection or withdrawal and its evidence.",
            ));
        }
        if evidence_text(&self.evidence).trim().is_empty() || is_empty_container(&self.evidence) {
            return Err(AppError::new(
                StatusCode::BAD_REQUEST,
                "revert_evidence_required",
                "An agent revert needs non-empty evidence (text or JSON).",
            ));
        }
        Ok(())
    }
}

/// True for an empty JSON object or array.
fn is_empty_container(v: &Value) -> bool {
    v.as_object().is_some_and(|o| o.is_empty()) || v.as_array().is_some_and(|a| a.is_empty())
}

/// Evidence as prose: a string as-is, null as empty, anything else as JSON.
pub(crate) fn evidence_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// `text` cut to at most `max` bytes at a character boundary, so derived
/// titles and criteria stay within the task field bounds.
pub(crate) fn clip(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// The published result a revert targets, with its subject task.
pub(crate) struct RevertTarget {
    /// The result joined with its submission and task, and `published`.
    pub row: SqliteRow,
}

impl RevertTarget {
    /// One text column of the target row.
    pub fn text(&self, column: &str) -> String {
        self.row.get(column)
    }
}

/// Loads result `id` of project `p` with its task; a result of another
/// project is not found, one not yet published is refused.
pub(crate) async fn revert_target(
    c: &mut SqliteConnection,
    p: &str,
    id: &str,
) -> Result<RevertTarget, AppError> {
    let row = sqlx::query("SELECT r.id AS result_id,r.submission_id,r.r,r.t0,r.c,r.landing_range_json,s.task_id AS original_task_id,s.candidate_ref,t.title,t.description,t.acceptance_json,t.kind,t.priority,t.lifecycle,EXISTS(SELECT 1 FROM integrator_observations o WHERE o.result_id=r.id AND o.disposition='published') AS published FROM integrator_results r JOIN submissions s ON s.id=r.submission_id JOIN tasks t ON t.id=s.task_id WHERE r.id=? AND r.project_id=?")
        .bind(id).bind(p).fetch_optional(&mut *c).await?.ok_or_else(AppError::not_found)?;
    if !row.get::<bool, _>("published") || row.get::<String, _>("lifecycle") != "done" {
        return Err(AppError::conflict(
            "result_not_published",
            "Only a result with a published observation whose task is done can be reverted.",
        ));
    }
    Ok(RevertTarget { row })
}

/// The open (planned or open) revert of `result`, if one exists.
async fn open_revert(c: &mut SqliteConnection, result: &str) -> Result<Option<String>, AppError> {
    Ok(sqlx::query_scalar("SELECT tr.task_id FROM task_reverts tr JOIN tasks t ON t.id=tr.task_id WHERE tr.result_id=? AND t.lifecycle IN ('planned','open') ORDER BY tr.created_at LIMIT 1")
        .bind(result).fetch_optional(&mut *c).await?)
}

/// Refuses a second open revert of the same result.
async fn ensure_no_open_revert(c: &mut SqliteConnection, result: &str) -> Result<(), AppError> {
    match open_revert(c, result).await? {
        None => Ok(()),
        Some(task) => Err(AppError::conflict(
            "revert_exists",
            "An open revert of this result already exists.",
        )
        .with_details(json!({"revert_task_id": task}))),
    }
}

/// A revert about to be created.
pub(crate) struct NewRevert<'a> {
    pub project: &'a str,
    pub target: &'a RevertTarget,
    pub reason: &'a str,
    pub evidence: &'a Value,
    pub note: Option<&'a str>,
    pub review_required: bool,
    pub priority: i64,
    pub created_by: &'a str,
    /// The creator's session, recorded as a contributor with the creator.
    pub creator_session: Option<&'a str>,
}

/// Title, description and acceptance criteria of a revert task.
fn revert_fields(n: &NewRevert<'_>) -> (String, String, Value) {
    let (r, title) = (n.target.text("r"), n.target.text("title"));
    let mut description = format!(
        "Revert the integrated result {r} of task {} ({title}): `git revert -m 1 {r}` on the current target tip, or the landed commit range when it landed fast-forward. The integrator computes and attests the mechanical candidate. Reason: {}. Evidence: {}",
        n.target.text("original_task_id"),
        n.reason,
        evidence_text(n.evidence)
    );
    if let Some(note) = n.note.filter(|note| !note.is_empty()) {
        description.push_str(&format!("\n\nNote: {note}"));
    }
    let acceptance = json!([
        format!("The integrated result {r} is reverted on the target"),
        "The target's required checks pass on the reverted result",
    ]);
    let title = clip(&format!("Revert: {title}"), 300);
    (title, description, acceptance)
}

/// Inserts the revert task (blocked until its candidate) and its target row.
async fn insert_revert(m: &mut Mutation, n: &NewRevert<'_>) -> Result<String, AppError> {
    let id = uuid::Uuid::new_v4().to_string();
    let (title, description, acceptance) = revert_fields(n);
    sqlx::query("INSERT INTO tasks(id,project_id,title,description,acceptance_json,kind,priority,lifecycle,blocked_reason,created_at,ready_since) VALUES(?,?,?,?,?,'code',?,'open',?,?,?)")
        .bind(&id).bind(n.project).bind(&title).bind(&description).bind(acceptance.to_string())
        .bind(n.priority).bind(AWAITING_CANDIDATE).bind(m.now).bind(m.now).execute(&mut *m.tx).await?;
    save_task_revision(m, n.project, &id).await?;
    sqlx::query("INSERT INTO task_reverts(task_id,project_id,result_id,submission_id,original_task_id,r,reason,evidence_json,review_required,created_by,created_at) VALUES(?,?,?,?,?,?,?,?,?,?,?)")
        .bind(&id).bind(n.project).bind(n.target.text("result_id")).bind(n.target.text("submission_id"))
        .bind(n.target.text("original_task_id")).bind(n.target.text("r")).bind(n.reason)
        .bind(n.evidence.to_string()).bind(n.review_required).bind(n.created_by).bind(m.now)
        .execute(&mut *m.tx).await?;
    let creator = Creator {
        task: &id,
        principal: n.created_by,
        session: n.creator_session,
        original: &n.target.text("original_task_id"),
    };
    record_creator(&mut m.tx, &creator, m.now).await?;
    Ok(id)
}

/// Records a human's one-click revert as an escaped-defect canary event.
async fn record_canary(m: &mut Mutation, n: &NewRevert<'_>, task: &str) -> Result<(), AppError> {
    let data = json!({"revert_task_id": task, "result_id": n.target.text("result_id"),
        "original_task_id": n.target.text("original_task_id"), "reason": n.reason});
    sqlx::query("INSERT INTO events(project_id,actor_id,kind,record_id,data_json,created_at) VALUES(?,?,'revert.escaped_defect_canary',?,?,?)")
        .bind(n.project).bind(&m.actor.id).bind(task).bind(data.to_string()).bind(m.now)
        .execute(&mut *m.tx).await?;
    Ok(())
}

/// The withdraw request that lost to a landed push, as revert evidence.
fn withdraw_evidence(request: &SqliteRow) -> Value {
    json!({"reason_code": "author_withdraw",
        "reason": request.get::<String, _>("reason"),
        "evidence": request.get::<Option<String>, _>("evidence"),
        "requested_by": request.get::<String, _>("requested_by")})
}

/// Creates the revert of an `author_withdraw` revise that lost to a landed
/// push: urgent, reviewed, with the withdraw request as its evidence. An open
/// revert of the same result is reused.
pub(crate) async fn create_automatic(
    m: &mut Mutation,
    p: &str,
    result: &str,
    request: &SqliteRow,
) -> Result<String, AppError> {
    if let Some(existing) = open_revert(&mut m.tx, result).await? {
        return Ok(existing);
    }
    let target = revert_target(&mut m.tx, p, result).await?;
    let evidence = withdraw_evidence(request);
    let requested_by: String = request.get("requested_by");
    let n = NewRevert {
        project: p,
        target: &target,
        reason: "author_withdraw",
        evidence: &evidence,
        note: None,
        review_required: true,
        priority: AUTOMATIC_PRIORITY,
        created_by: &requested_by,
        creator_session: None,
    };
    insert_revert(m, &n).await
}

/// Validates the request against the result and stores the revert.
async fn store(m: &mut Mutation, p: &str, input: &RevertInput) -> Result<String, AppError> {
    require_integrator_project(&mut m.tx, p).await?;
    let target = revert_target(&mut m.tx, p, &input.result_id).await?;
    ensure_no_open_revert(&mut m.tx, &input.result_id).await?;
    let human = m.actor.kind == "human";
    let (created_by, session) = (m.actor.id.clone(), m.actor.session_id.clone());
    let n = NewRevert {
        project: p,
        target: &target,
        reason: &input.reason,
        evidence: &input.evidence,
        note: input.note.as_deref(),
        review_required: !human,
        priority: input.priority.unwrap_or(DEFAULT_PRIORITY),
        created_by: &created_by,
        creator_session: session.as_deref(),
    };
    let task = insert_revert(m, &n).await?;
    if human {
        record_canary(m, &n, &task).await?;
    }
    Ok(task)
}

/// `POST …/reverts`: a human's one-click revert (no review) or an agent's
/// revert with evidence (reviewed). Always admitted.
async fn create(
    State(s): State<AppState>,
    auth: Auth,
    Path(p): Path<String>,
    headers: HeaderMap,
    body: Result<Json<RevertInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    input.validate()?;
    input.authorize(&auth.actor.kind)?;
    let op = format!("POST /api/v1/projects/{p}/reverts");
    let mut m = Mutation::begin(&s, &auth, &headers, &op, &input).await?;
    if let Some(v) = m.replay.take() {
        return Ok(response(v));
    }
    let task = store(&mut m, &p, &input).await?;
    let mut value = task_record_value(&mut m.tx, &p, &task, m.now).await?;
    value["revert"] = revert_view(&mut m.tx, &task).await?;
    Ok(response(
        m.finish(value, Some(&p), "revert.created", &task).await?,
    ))
}

/// Serializes one `task_reverts` row.
fn revert_value(row: &SqliteRow) -> Result<Value, AppError> {
    Ok(json!({
        "result_id": row.get::<String, _>("result_id"),
        "submission_id": row.get::<String, _>("submission_id"),
        "original_task_id": row.get::<String, _>("original_task_id"),
        "r": row.get::<String, _>("r"),
        "reason": row.get::<String, _>("reason"),
        "evidence": serde_json::from_str::<Value>(row.get("evidence_json"))?,
        "review_required": row.get::<bool, _>("review_required"),
        "mode": row.get::<String, _>("mode"),
        "not_mechanical": json_column(row, "not_mechanical_json")?,
        "rejection": json_column(row, "rejection_json")?,
        "reland_task_id": row.get::<Option<String>, _>("reland_task_id"),
        "created_by": row.get::<String, _>("created_by"),
        "created_at": coordinator_core::timestamp(row.get("created_at")),
    }))
}

/// A nullable JSON text column, decoded.
fn json_column(row: &SqliteRow, column: &str) -> Result<Value, AppError> {
    let text: Option<String> = row.get(column);
    Ok(text
        .map(|v| serde_json::from_str(&v))
        .transpose()?
        .unwrap_or(Value::Null))
}

/// The mechanical candidates the integrator attested for revert `task`,
/// oldest first, so reviewers see what they judge.
async fn attested_candidates(c: &mut SqliteConnection, task: &str) -> Result<Value, AppError> {
    let rows = sqlx::query(
        "SELECT * FROM revert_candidates WHERE revert_task_id=? ORDER BY recorded_at,t0",
    )
    .bind(task)
    .fetch_all(&mut *c)
    .await?;
    Ok(json!(
        rows.iter()
            .map(|r| json!({
                "t0": r.get::<String, _>("t0"),
                "submission_id": r.get::<String, _>("submission_id"),
                "candidate_commit": r.get::<String, _>("candidate_commit"),
                "candidate_tree": r.get::<String, _>("candidate_tree"),
                "attestation": r.get::<String, _>("attestation"),
                "attested_by": r.get::<String, _>("recorded_by"),
                "attested_at": coordinator_core::timestamp(r.get("recorded_at")),
            }))
            .collect::<Vec<_>>()
    ))
}

/// The revert target of task `task` with its attested candidates, or null
/// when it is not a revert.
pub(crate) async fn revert_view(c: &mut SqliteConnection, task: &str) -> Result<Value, AppError> {
    let row = sqlx::query("SELECT * FROM task_reverts WHERE task_id=?")
        .bind(task)
        .fetch_optional(&mut *c)
        .await?;
    let Some(row) = row else {
        return Ok(Value::Null);
    };
    let mut value = revert_value(&row)?;
    value["candidates"] = attested_candidates(c, task).await?;
    Ok(value)
}

/// The newest revert of task `task` whose task is not canceled, or null.
pub(crate) async fn reverted_by(c: &mut SqliteConnection, task: &str) -> Result<Value, AppError> {
    let id: Option<String> = sqlx::query_scalar("SELECT tr.task_id FROM task_reverts tr JOIN tasks t ON t.id=tr.task_id WHERE tr.original_task_id=? AND t.lifecycle!='canceled' ORDER BY tr.created_at DESC,tr.task_id DESC LIMIT 1")
        .bind(task).fetch_optional(&mut *c).await?;
    Ok(json!(id))
}

/// The review mode `task` is held to: a revert that needs review (decided by
/// an agent, or converted to implementation work) has at least an agent
/// review even when the project's review mode is `none`.
pub(crate) async fn review_mode_floor(
    c: &mut SqliteConnection,
    task: &str,
    mode: &str,
) -> Result<String, AppError> {
    if mode != "none" {
        return Ok(mode.to_owned());
    }
    let reviewed: Option<bool> =
        sqlx::query_scalar("SELECT review_required FROM task_reverts WHERE task_id=?")
            .bind(task)
            .fetch_optional(&mut *c)
            .await?;
    Ok(if reviewed == Some(true) {
        "agent"
    } else {
        mode
    }
    .to_owned())
}
