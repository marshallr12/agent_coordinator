//! Integrator reports (planning p4-design §3 steps 1, 2, 4 and 6, step S4):
//! durable findings the integrator raises for the daily digest and, when a
//! human must act, for the human queue that `next` serves.
//!
//! A report is an attestation: the first write per (project, kind,
//! dedupe_key) wins, and a replay returns the stored row unchanged. The
//! service derives `requires_human` from the kind. Only a human resolves a
//! report; resolving a `privilege_gate` report records an `allow` or `deny`
//! decision, and `allow` lets the integrator push that report's result.
use crate::{
    auth::{Actor, Auth},
    error::AppError,
    integrator::require_integrator_project,
    mutation::Mutation,
    response,
    state::AppState,
    workflow::{bounded, payload},
};
use axum::{
    Json, Router,
    extract::{Path, Query, State, rejection::JsonRejection},
    http::HeaderMap,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection, sqlite::SqliteRow};

type Reply = Result<Json<Value>, AppError>;

/// Report kinds the integrator may raise.
const KINDS: &[&str] = &[
    "privilege_gate",
    "flaky",
    "fix_target",
    "unreviewed_landing",
    "target_rewritten",
    "ruleset_missing",
];
/// Kinds that stop the integrator until a human acts on them.
const HUMAN_KINDS: &[&str] = &["privilege_gate", "target_rewritten", "ruleset_missing"];
/// Upper bound on a report's serialized details.
const MAX_DETAILS_BYTES: usize = 65_536;
/// Reports returned per list call unless `limit` says otherwise.
const DEFAULT_LIST_LIMIT: i64 = 200;
/// The largest `limit` a list call accepts.
const MAX_LIST_LIMIT: i64 = 1000;
/// Open human-required reports listed per `next` call.
const HUMAN_QUEUE_LIMIT: i64 = 50;

/// The report routes: the integrator writes, readers list, humans resolve.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/projects/{p}/integrator/reports",
            get(list_reports).post(record_report),
        )
        .route(
            "/api/v1/projects/{p}/integrator/reports/{id}/resolve",
            post(resolve_report),
        )
}

/// One finding the integrator reports.
#[derive(Deserialize, Serialize)]
struct ReportInput {
    kind: String,
    dedupe_key: String,
    task_id: Option<String>,
    submission_id: Option<String>,
    result_id: Option<String>,
    details: Value,
}

impl ReportInput {
    /// Checks the kind, identities and bounds before any state is read.
    fn validate(&self) -> Result<(), AppError> {
        if !KINDS.contains(&self.kind.as_str()) {
            return Err(AppError::bad_request(
                "kind is not an integrator report kind.",
            ));
        }
        bounded(&self.dedupe_key, "dedupe_key", 512, true)?;
        for (id, name) in self.references() {
            bounded(id.unwrap_or_default(), name, 100, false)?;
        }
        if self.kind == "privilege_gate" && self.result_id.is_none() {
            return Err(AppError::bad_request(
                "A privilege_gate report must name the result_id it gates.",
            ));
        }
        if !self.details.is_object() || self.details.to_string().len() > MAX_DETAILS_BYTES {
            return Err(AppError::bad_request(
                "details must be a JSON object of at most 64 KiB.",
            ));
        }
        Ok(())
    }

    /// The optional record references with their field names.
    fn references(&self) -> [(Option<&str>, &'static str); 3] {
        [
            (self.task_id.as_deref(), "task_id"),
            (self.submission_id.as_deref(), "submission_id"),
            (self.result_id.as_deref(), "result_id"),
        ]
    }
}

/// Refuses a reference that does not name a record of project `p`.
async fn require_references(
    c: &mut SqliteConnection,
    p: &str,
    input: &ReportInput,
) -> Result<(), AppError> {
    let checks = [
        (
            input.task_id.as_deref(),
            "SELECT count(*) FROM tasks WHERE id=? AND project_id=?",
        ),
        (
            input.submission_id.as_deref(),
            "SELECT count(*) FROM submissions WHERE id=? AND project_id=?",
        ),
        (
            input.result_id.as_deref(),
            "SELECT count(*) FROM integrator_results WHERE id=? AND project_id=?",
        ),
    ];
    for (id, sql) in checks {
        let Some(id) = id else { continue };
        let found: i64 = sqlx::query_scalar(sql)
            .bind(id)
            .bind(p)
            .fetch_one(&mut *c)
            .await?;
        if found == 0 {
            return Err(AppError::not_found());
        }
    }
    Ok(())
}

/// Serializes one stored report. `allowed` is true only for a resolved
/// report whose human decision was `allow`.
fn report_value(row: &SqliteRow) -> Result<Value, AppError> {
    let decision: Option<String> = row.get("decision");
    let resolved_at: Option<i64> = row.get("resolved_at");
    Ok(json!({
        "id": row.get::<String, _>("id"),
        "project_id": row.get::<String, _>("project_id"),
        "kind": row.get::<String, _>("kind"),
        "task_id": row.get::<Option<String>, _>("task_id"),
        "submission_id": row.get::<Option<String>, _>("submission_id"),
        "result_id": row.get::<Option<String>, _>("result_id"),
        "dedupe_key": row.get::<String, _>("dedupe_key"),
        "details": serde_json::from_str::<Value>(row.get("details_json"))?,
        "requires_human": row.get::<i64, _>("requires_human") == 1,
        "created_at": coordinator_core::timestamp(row.get("created_at")),
        "resolved_at": resolved_at.map(coordinator_core::timestamp),
        "resolved_by": row.get::<Option<String>, _>("resolved_by"),
        "resolution_note": row.get::<Option<String>, _>("resolution_note"),
        "decision": decision,
        "allowed": resolved_at.is_some() && decision.as_deref() == Some("allow"),
    }))
}

/// Whether a report needs a human: its kind always does, or the integrator
/// marked it `blocks_subject` (a subject it cannot move on its own, such as a
/// failure on the target or a refused rerun), so it is never parked unseen.
fn needs_human(input: &ReportInput) -> bool {
    HUMAN_KINDS.contains(&input.kind.as_str()) || input.details["blocks_subject"] == true
}

/// Inserts the report unless its (project, kind, dedupe_key) is taken,
/// deriving `requires_human` from its kind or its `blocks_subject` detail. Returns the stored row and
/// whether this call created it.
async fn insert_report(
    m: &mut Mutation,
    p: &str,
    input: &ReportInput,
) -> Result<(SqliteRow, bool), AppError> {
    let id = uuid::Uuid::new_v4().to_string();
    let human = needs_human(input);
    sqlx::query("INSERT INTO integrator_reports(id,project_id,kind,task_id,submission_id,result_id,dedupe_key,details_json,requires_human,created_by,created_at) VALUES(?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(project_id,kind,dedupe_key) DO NOTHING")
        .bind(&id).bind(p).bind(&input.kind).bind(&input.task_id).bind(&input.submission_id)
        .bind(&input.result_id).bind(&input.dedupe_key).bind(input.details.to_string())
        .bind(i64::from(human)).bind(&m.actor.id).bind(m.now).execute(&mut *m.tx).await?;
    let row = sqlx::query(
        "SELECT * FROM integrator_reports WHERE project_id=? AND kind=? AND dedupe_key=?",
    )
    .bind(p)
    .bind(&input.kind)
    .bind(&input.dedupe_key)
    .fetch_one(&mut *m.tx)
    .await?;
    let created = row.get::<String, _>("id") == id;
    Ok((row, created))
}

/// `POST …/integrator/reports`: records a finding once per dedupe key. A
/// replay returns the stored row (including any resolution) and changes
/// nothing, so it neither emits an event nor stores a mutation receipt.
async fn record_report(
    State(s): State<AppState>,
    auth: Auth,
    Path(p): Path<String>,
    headers: HeaderMap,
    body: Result<Json<ReportInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    input.validate()?;
    let op = format!("POST /api/v1/projects/{p}/integrator/reports");
    let mut m = Mutation::begin(&s, &auth, &headers, &op, &input).await?;
    if let Some(v) = m.replay.take() {
        return Ok(response(v));
    }
    require_integrator_project(&mut m.tx, &p).await?;
    require_references(&mut m.tx, &p, &input).await?;
    let (row, created) = insert_report(&mut m, &p, &input).await?;
    let (value, record) = (report_value(&row)?, row.get::<String, _>("id"));
    if !created {
        return Ok(response(value));
    }
    Ok(response(
        m.finish(value, Some(&p), "integrator.report_recorded", &record)
            .await?,
    ))
}

/// Refuses an unknown project.
async fn require_project(c: &mut SqliteConnection, p: &str) -> Result<(), AppError> {
    let found: i64 = sqlx::query_scalar("SELECT count(*) FROM projects WHERE id=?")
        .bind(p)
        .fetch_one(&mut *c)
        .await?;
    if found == 0 {
        return Err(AppError::not_found());
    }
    Ok(())
}

/// One page request of `GET …/integrator/reports`.
#[derive(Deserialize)]
struct ListQuery {
    open: Option<bool>,
    limit: Option<i64>,
    before: Option<String>,
}

impl ListQuery {
    /// The page size: `limit`, else the default; refuses out-of-range values.
    fn limit(&self) -> Result<i64, AppError> {
        match self.limit.unwrap_or(DEFAULT_LIST_LIMIT) {
            limit @ 1..=MAX_LIST_LIMIT => Ok(limit),
            _ => Err(AppError::bad_request("limit must be between 1 and 1000.")),
        }
    }
}

/// The insertion position of cursor report `before`; past the newest
/// report when there is no cursor.
async fn cursor(c: &mut SqliteConnection, p: &str, before: Option<&str>) -> Result<i64, AppError> {
    let Some(id) = before else {
        return Ok(i64::MAX);
    };
    sqlx::query_scalar("SELECT rowid FROM integrator_reports WHERE id=? AND project_id=?")
        .bind(id)
        .bind(p)
        .fetch_optional(&mut *c)
        .await?
        .ok_or_else(AppError::not_found)
}

/// One page of project `p`'s reports, newest first, recorded before
/// position `before`; only unresolved ones when `open`.
async fn report_page(
    c: &mut SqliteConnection,
    p: &str,
    open: bool,
    before: i64,
    limit: i64,
) -> Result<Vec<Value>, AppError> {
    let rows = sqlx::query(
        "SELECT * FROM integrator_reports WHERE project_id=? \
         AND (?=0 OR resolved_at IS NULL) AND rowid<? ORDER BY rowid DESC LIMIT ?",
    )
    .bind(p)
    .bind(open)
    .bind(before)
    .bind(limit)
    .fetch_all(&mut *c)
    .await?;
    rows.iter().map(report_value).collect()
}

/// `GET …/integrator/reports?open=true&limit=200&before=<id>`: the
/// project's reports, newest first, for any reader (the digest, the
/// dashboard, agents). `next_before` is the cursor of the following page,
/// or null when this page is the last.
async fn list_reports(
    State(s): State<AppState>,
    _auth: Auth,
    Path(p): Path<String>,
    Query(query): Query<ListQuery>,
) -> Reply {
    let limit = query.limit()?;
    let mut c = s.pool.acquire().await?;
    require_project(&mut c, &p).await?;
    let before = cursor(&mut c, &p, query.before.as_deref()).await?;
    let items = report_page(&mut c, &p, query.open.unwrap_or(false), before, limit).await?;
    let full = i64::try_from(items.len()).unwrap_or(i64::MAX) == limit;
    let next = full
        .then(|| items.last().map(|r| r["id"].clone()))
        .flatten();
    Ok(response(
        json!({"project_id": p, "items": items, "next_before": next}),
    ))
}

/// Open human-required reports as human-queue items for `next`, oldest
/// first: labelled with `required_actor` and the report kind as `gate`,
/// plus the resolve call.
pub(crate) async fn human_queue_items(
    c: &mut SqliteConnection,
    p: &str,
) -> Result<Vec<Value>, AppError> {
    open_human_items(c, p, false).await
}

/// The same items without those of canary tasks, for the digest's human
/// interventions: a canary task is a probe, not work.
pub(crate) async fn human_queue_items_without_canary(
    c: &mut SqliteConnection,
    p: &str,
) -> Result<Vec<Value>, AppError> {
    open_human_items(c, p, true).await
}

/// The open human-required reports of `p` as items, leaving out those about
/// canary tasks when `without_canary`.
async fn open_human_items(
    c: &mut SqliteConnection,
    p: &str,
    without_canary: bool,
) -> Result<Vec<Value>, AppError> {
    let rows = sqlx::query(
        "SELECT * FROM integrator_reports WHERE project_id=? AND resolved_at IS NULL \
         AND requires_human=1 AND (?=0 OR task_id IS NULL OR task_id NOT IN \
         (SELECT id FROM tasks WHERE budget_exempt IS NOT NULL)) ORDER BY rowid LIMIT ?",
    )
    .bind(p)
    .bind(without_canary)
    .bind(HUMAN_QUEUE_LIMIT)
    .fetch_all(&mut *c)
    .await?;
    let reports = rows
        .iter()
        .map(report_value)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(reports.into_iter().map(|r| human_item(p, r)).collect())
}

/// One open report as a human-queue item.
fn human_item(p: &str, report: Value) -> Value {
    let path = format!(
        "/api/v1/projects/{p}/integrator/reports/{}/resolve",
        report["id"].as_str().unwrap_or_default()
    );
    json!({
        "code": "integrator_report",
        "required_actor": "human",
        "gate": report["kind"],
        "report": report,
        "call": {"method": "POST", "path": path},
    })
}

/// A human's resolution of one report.
#[derive(Deserialize, Serialize)]
struct ResolveInput {
    note: String,
    decision: Option<String>,
}

impl ResolveInput {
    /// Checks the note and the decision value before any state is read.
    fn validate(&self) -> Result<(), AppError> {
        bounded(&self.note, "note", 2000, true)?;
        match self.decision.as_deref() {
            None | Some("allow" | "deny") => Ok(()),
            Some(_) => Err(AppError::bad_request("decision must be allow or deny.")),
        }
    }
}

/// Refuses a decision that does not fit the report's kind: a
/// `privilege_gate` report needs one, every other kind takes none.
fn require_decision_fits(kind: &str, input: &ResolveInput) -> Result<(), AppError> {
    match (kind == "privilege_gate", input.decision.is_some()) {
        (true, false) => Err(AppError::bad_request(
            "Resolving a privilege_gate report needs decision allow or deny.",
        )),
        (false, true) => Err(AppError::bad_request(
            "Only a privilege_gate report takes a decision.",
        )),
        _ => Ok(()),
    }
}

/// Loads an open report of project `p`, or refuses.
async fn open_report(c: &mut SqliteConnection, p: &str, id: &str) -> Result<SqliteRow, AppError> {
    let row = sqlx::query("SELECT * FROM integrator_reports WHERE id=? AND project_id=?")
        .bind(id)
        .bind(p)
        .fetch_optional(&mut *c)
        .await?
        .ok_or_else(AppError::not_found)?;
    if row.get::<Option<i64>, _>("resolved_at").is_some() {
        return Err(AppError::conflict(
            "report_already_resolved",
            "This report was already resolved.",
        ));
    }
    Ok(row)
}

/// Marks the report resolved by the caller and returns the stored row.
async fn mark_resolved(
    m: &mut Mutation,
    id: &str,
    input: &ResolveInput,
) -> Result<SqliteRow, AppError> {
    sqlx::query("UPDATE integrator_reports SET resolved_at=?,resolved_by=?,resolution_note=?,decision=? WHERE id=?")
        .bind(m.now).bind(&m.actor.id).bind(&input.note).bind(&input.decision).bind(id)
        .execute(&mut *m.tx).await?;
    Ok(sqlx::query("SELECT * FROM integrator_reports WHERE id=?")
        .bind(id)
        .fetch_one(&mut *m.tx)
        .await?)
}

/// Refuses non-human callers with the labelled resolution gate.
fn require_human(actor: &Actor) -> Result<(), AppError> {
    if actor.kind == "human" {
        return Ok(());
    }
    Err(AppError::human_gate(
        "integrator_report_resolution",
        "Only a human operator resolves integrator reports.",
    ))
}

/// `POST …/integrator/reports/{id}/resolve`: a human closes a report; for a
/// `privilege_gate` report the decision says whether its result may be pushed.
async fn resolve_report(
    State(s): State<AppState>,
    auth: Auth,
    Path((p, id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<ResolveInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    input.validate()?;
    let op = format!("POST /api/v1/projects/{p}/integrator/reports/{id}/resolve");
    let mut m = Mutation::begin(&s, &auth, &headers, &op, &input).await?;
    if let Some(v) = m.replay.take() {
        return Ok(response(v));
    }
    require_human(&m.actor)?;
    let row = open_report(&mut m.tx, &p, &id).await?;
    require_decision_fits(row.get("kind"), &input)?;
    let value = report_value(&mark_resolved(&mut m, &id, &input).await?)?;
    Ok(response(
        m.finish(value, Some(&p), "integrator.report_resolved", &id)
            .await?,
    ))
}
