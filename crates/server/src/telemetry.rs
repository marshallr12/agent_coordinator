//! Supervisor telemetry: the launch cost rows and end-to-end canary results
//! that supervisor hosts report per project, and the aggregated reads the
//! console shows (autonomy health, planning/autonomy multi-project design).
//!
//! The hosts' own ledgers (`costs.jsonl`, `e2e-canary.jsonl`) stay the
//! durable source; the service only stores what a host reports. Every row
//! carries a `row_id`, the reporter's stable identity of one ledger row, and
//! is stored once per (project, host, row_id): a retried batch, a batch under
//! a new idempotency key and a re-imported ledger all leave one row. The first
//! report of a row wins. Each row records the reporting principal and host.
use crate::{
    auth::{Actor, Auth},
    error::AppError,
    mutation::Mutation,
    response,
    state::AppState,
    workflow::{bounded, payload},
};
use axum::{
    Json, Router,
    extract::{Path, Query, State, rejection::JsonRejection},
    http::HeaderMap,
    routing::get,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection, sqlite::SqliteRow};
use std::collections::HashMap;

type Reply = Result<Json<Value>, AppError>;

/// Rows one report may carry.
const MAX_BATCH: usize = 200;
/// The longest `host`, as a DNS name.
const MAX_HOST_BYTES: usize = 253;
/// The longest `row_id`.
const MAX_ROW_ID_BYTES: usize = 128;
/// The most bytes of a canary result's `detail`.
const MAX_DETAIL_BYTES: usize = 4000;
/// The largest token count or dollar figure a row may claim, far above any
/// real launch and well inside `i64`.
const MAX_TOKENS: u64 = 1_000_000_000_000;
const MAX_USD: f64 = 1_000_000.0;
/// Groups returned per cost read unless `limit` says otherwise.
const DEFAULT_GROUPS: usize = 100;
/// The largest `limit` of a cost read.
const MAX_GROUPS: usize = 1000;
/// Canary results returned per list unless `limit` says otherwise.
const DEFAULT_LIST_LIMIT: i64 = 100;
/// The largest `limit` of a canary list.
const MAX_LIST_LIMIT: i64 = 1000;

/// The telemetry routes: supervisor hosts write, readers aggregate.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/projects/{p}/telemetry/cost-rows",
            axum::routing::post(report_costs),
        )
        .route(
            "/api/v1/projects/{p}/telemetry/canary-results",
            axum::routing::post(report_canaries),
        )
        .route("/api/v1/projects/{p}/telemetry/cost", get(read_costs))
        .route(
            "/api/v1/projects/{p}/telemetry/canaries",
            get(read_canaries),
        )
}

/// One launch's cost, as a host's `costs.jsonl` row.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CostRow {
    row_id: String,
    /// `impl` for an implementer launch, `rev` for a reviewer's.
    role: String,
    harness: String,
    model: String,
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
    /// Dollars; absent when the launch was unpriced.
    usd: Option<f64>,
    #[serde(default)]
    usd_estimated: bool,
    /// The row belongs to the host's canary project.
    #[serde(default)]
    canary: bool,
    task_id: Option<String>,
    session_id: Option<String>,
    attempt_id: Option<String>,
    /// When the launch settled (ms since the epoch).
    occurred_at_ms: i64,
}

/// One end-to-end canary run, as a host's `e2e-canary.jsonl` row.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CanaryResult {
    row_id: String,
    harness: String,
    /// `ok`, `failed` or `timeout`.
    outcome: String,
    duration_seconds: f64,
    detail: String,
    task_id: Option<String>,
    /// The harnesses the supervisor's ledger shows for the canary task.
    served_by: Option<Vec<String>>,
    /// When the run ended (ms since the epoch).
    occurred_at_ms: i64,
}

/// A batch of rows from one host.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Batch<T> {
    host: String,
    rows: Vec<T>,
}

/// Checks the shared envelope of a batch before any state is read.
fn validate_batch<T>(batch: &Batch<T>) -> Result<(), AppError> {
    bounded(&batch.host, "host", MAX_HOST_BYTES, true)?;
    if batch.rows.is_empty() || batch.rows.len() > MAX_BATCH {
        return Err(AppError::bad_request(
            "rows must hold between 1 and 200 entries.",
        ));
    }
    Ok(())
}

/// Checks a reporter-chosen identity: visible ASCII, bounded.
fn visible(value: &str, name: &str, max: usize) -> Result<(), AppError> {
    if value.is_empty() || value.len() > max || !value.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(AppError::bad_request(&format!(
            "{name} must be 1–{max} visible ASCII characters."
        )));
    }
    Ok(())
}

/// Checks an optional record reference.
fn reference(value: &Option<String>, name: &str) -> Result<(), AppError> {
    bounded(value.as_deref().unwrap_or_default(), name, 100, false)
}

/// A timestamp in the range the service can store and format.
fn instant(ms: i64, name: &str) -> Result<(), AppError> {
    if !(0..=4_102_444_800_000).contains(&ms) {
        return Err(AppError::bad_request(&format!(
            "{name} must be milliseconds since the epoch before the year 2100."
        )));
    }
    Ok(())
}

impl CostRow {
    /// Checks the identity, role, counts and dollars before any state is read.
    fn validate(&self) -> Result<(), AppError> {
        visible(&self.row_id, "row_id", MAX_ROW_ID_BYTES)?;
        if !matches!(self.role.as_str(), "impl" | "rev") {
            return Err(AppError::bad_request("role must be impl or rev."));
        }
        bounded(&self.harness, "harness", 100, true)?;
        bounded(&self.model, "model", 200, true)?;
        let counts = [
            self.input_tokens,
            self.cached_input_tokens,
            self.output_tokens,
        ];
        if counts.iter().any(|n| *n > MAX_TOKENS) {
            return Err(AppError::bad_request("A token count is out of range."));
        }
        if self
            .usd
            .is_some_and(|usd| !usd.is_finite() || !(0.0..=MAX_USD).contains(&usd))
        {
            return Err(AppError::bad_request(
                "usd must be a finite dollar amount of at least 0.",
            ));
        }
        reference(&self.task_id, "task_id")?;
        reference(&self.session_id, "session_id")?;
        reference(&self.attempt_id, "attempt_id")?;
        instant(self.occurred_at_ms, "occurred_at_ms")
    }

    /// The dollars as millionths, exact to sum.
    fn usd_micros(&self) -> Option<i64> {
        self.usd.map(|usd| (usd * 1_000_000.0).round() as i64)
    }
}

impl CanaryResult {
    /// Checks the identity, outcome and bounds before any state is read.
    fn validate(&self) -> Result<(), AppError> {
        visible(&self.row_id, "row_id", MAX_ROW_ID_BYTES)?;
        bounded(&self.harness, "harness", 100, true)?;
        if !matches!(self.outcome.as_str(), "ok" | "failed" | "timeout") {
            return Err(AppError::bad_request(
                "outcome must be ok, failed or timeout.",
            ));
        }
        if !self.duration_seconds.is_finite() || !(0.0..=1e9).contains(&self.duration_seconds) {
            return Err(AppError::bad_request(
                "duration_seconds must be a finite number of at least 0.",
            ));
        }
        bounded(&self.detail, "detail", MAX_DETAIL_BYTES, false)?;
        reference(&self.task_id, "task_id")?;
        let served = self.served_by.iter().flatten();
        if self.served_by.as_ref().is_some_and(|s| s.len() > 8) {
            return Err(AppError::bad_request(
                "served_by names at most 8 harnesses.",
            ));
        }
        for harness in served {
            bounded(harness, "served_by", 100, true)?;
        }
        instant(self.occurred_at_ms, "occurred_at_ms")
    }
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

/// Refuses a caller that is not an agent: hosts report with an agent
/// credential, whose write access and class the mutation already checked
/// (a read-only shadow credential and the integrator's are refused there).
fn require_reporter(actor: &Actor) -> Result<(), AppError> {
    if actor.kind != "agent" {
        return Err(AppError::forbidden(
            "Only an agent credential reports supervisor telemetry.",
        ));
    }
    Ok(())
}

/// Inserts one cost row unless the host already reported it; true when new.
async fn insert_cost(
    m: &mut Mutation,
    p: &str,
    host: &str,
    row: &CostRow,
) -> Result<bool, AppError> {
    let done = sqlx::query("INSERT INTO cost_rows(id,project_id,host,row_id,role,harness,model,input_tokens,cached_input_tokens,output_tokens,usd_micros,usd_estimated,canary,task_id,session_id,attempt_id,occurred_at,reported_by,reported_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(project_id,host,row_id) DO NOTHING")
        .bind(uuid::Uuid::new_v4().to_string()).bind(p).bind(host).bind(&row.row_id)
        .bind(&row.role).bind(&row.harness).bind(&row.model)
        .bind(row.input_tokens as i64).bind(row.cached_input_tokens as i64)
        .bind(row.output_tokens as i64).bind(row.usd_micros())
        .bind(i64::from(row.usd_estimated)).bind(i64::from(row.canary))
        .bind(&row.task_id).bind(&row.session_id).bind(&row.attempt_id)
        .bind(row.occurred_at_ms).bind(&m.actor.id).bind(m.now)
        .execute(&mut *m.tx).await?;
    Ok(done.rows_affected() == 1)
}

/// Inserts one canary result unless the host already reported it; true when
/// new.
async fn insert_canary(
    m: &mut Mutation,
    p: &str,
    host: &str,
    row: &CanaryResult,
) -> Result<bool, AppError> {
    let served = row.served_by.as_ref().map(|s| json!(s).to_string());
    let done = sqlx::query("INSERT INTO canary_results(id,project_id,host,row_id,harness,outcome,duration_seconds,detail,task_id,served_by_json,occurred_at,reported_by,reported_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(project_id,host,row_id) DO NOTHING")
        .bind(uuid::Uuid::new_v4().to_string()).bind(p).bind(host).bind(&row.row_id)
        .bind(&row.harness).bind(&row.outcome).bind(row.duration_seconds).bind(&row.detail)
        .bind(&row.task_id).bind(served).bind(row.occurred_at_ms)
        .bind(&m.actor.id).bind(m.now)
        .execute(&mut *m.tx).await?;
    Ok(done.rows_affected() == 1)
}

/// `POST …/telemetry/cost-rows`: records a host's launch cost rows, each once
/// per (project, host, row_id). The reply counts the rows this call stored
/// (`accepted`) and the ones already stored (`duplicates`); a replay of the
/// same Idempotency-Key returns the first reply.
async fn report_costs(
    State(s): State<AppState>,
    auth: Auth,
    Path(p): Path<String>,
    headers: HeaderMap,
    body: Result<Json<Batch<CostRow>>, JsonRejection>,
) -> Reply {
    let batch = payload(body)?;
    validate_batch(&batch)?;
    batch.rows.iter().try_for_each(CostRow::validate)?;
    let op = format!("POST /api/v1/projects/{p}/telemetry/cost-rows");
    let mut m = Mutation::begin(&s, &auth, &headers, &op, &batch).await?;
    if let Some(v) = m.replay.take() {
        return Ok(response(v));
    }
    require_reporter(&m.actor)?;
    require_project(&mut m.tx, &p).await?;
    let mut accepted = 0_usize;
    for row in &batch.rows {
        accepted += usize::from(insert_cost(&mut m, &p, &batch.host, row).await?);
    }
    let duplicates = batch.rows.len() - accepted;
    let value = json!({"project_id": p, "host": batch.host, "kind": "cost",
                       "accepted": accepted, "duplicates": duplicates});
    let event = json!({"host": batch.host, "accepted": accepted, "duplicates": duplicates});
    let record = format!("{p}/{}", batch.host);
    Ok(response(
        m.finish_with_event_data(value, &event, Some(&p), "telemetry.cost_reported", &record)
            .await?,
    ))
}

/// `POST …/telemetry/canary-results`: as [`report_costs`], for canary runs.
async fn report_canaries(
    State(s): State<AppState>,
    auth: Auth,
    Path(p): Path<String>,
    headers: HeaderMap,
    body: Result<Json<Batch<CanaryResult>>, JsonRejection>,
) -> Reply {
    let batch = payload(body)?;
    validate_batch(&batch)?;
    batch.rows.iter().try_for_each(CanaryResult::validate)?;
    let op = format!("POST /api/v1/projects/{p}/telemetry/canary-results");
    let mut m = Mutation::begin(&s, &auth, &headers, &op, &batch).await?;
    if let Some(v) = m.replay.take() {
        return Ok(response(v));
    }
    require_reporter(&m.actor)?;
    require_project(&mut m.tx, &p).await?;
    let mut accepted = 0_usize;
    for row in &batch.rows {
        accepted += usize::from(insert_canary(&mut m, &p, &batch.host, row).await?);
    }
    let duplicates = batch.rows.len() - accepted;
    let value = json!({"project_id": p, "host": batch.host, "kind": "canary",
                       "accepted": accepted, "duplicates": duplicates});
    let event = json!({"host": batch.host, "accepted": accepted, "duplicates": duplicates});
    let record = format!("{p}/{}", batch.host);
    Ok(response(
        m.finish_with_event_data(
            value,
            &event,
            Some(&p),
            "telemetry.canary_reported",
            &record,
        )
        .await?,
    ))
}

/// The filters and page size of a cost read.
#[derive(Deserialize)]
struct CostQuery {
    /// `task` (default) or `day`.
    group_by: Option<String>,
    host: Option<String>,
    /// Rows settled at or after / before these instants (ms since the epoch).
    since_ms: Option<i64>,
    until_ms: Option<i64>,
    /// Keep the canary probes' cost (default false).
    include_canary: Option<bool>,
    limit: Option<usize>,
}

/// What one read groups cost rows by.
#[derive(Clone, Copy, PartialEq)]
enum Group {
    Task,
    Day,
}

impl CostQuery {
    fn group(&self) -> Result<Group, AppError> {
        match self.group_by.as_deref() {
            None | Some("task") => Ok(Group::Task),
            Some("day") => Ok(Group::Day),
            Some(_) => Err(AppError::bad_request("group_by must be task or day.")),
        }
    }

    fn limit(&self) -> Result<usize, AppError> {
        match self.limit.unwrap_or(DEFAULT_GROUPS) {
            limit @ 1..=MAX_GROUPS => Ok(limit),
            _ => Err(AppError::bad_request("limit must be between 1 and 1000.")),
        }
    }
}

/// One role's figures within a group.
#[derive(Default, Clone)]
struct Spend {
    launches: i64,
    micros: i64,
    unpriced: i64,
    estimated: i64,
    input_tokens: i64,
    cached_input_tokens: i64,
    output_tokens: i64,
}

impl Spend {
    fn add(&mut self, other: &Spend) {
        self.launches += other.launches;
        self.micros += other.micros;
        self.unpriced += other.unpriced;
        self.estimated += other.estimated;
        self.input_tokens += other.input_tokens;
        self.cached_input_tokens += other.cached_input_tokens;
        self.output_tokens += other.output_tokens;
    }

    fn value(&self) -> Value {
        json!({
            "launches": self.launches,
            "usd": usd(self.micros),
            "unpriced_launches": self.unpriced,
            "estimated_launches": self.estimated,
            "input_tokens": self.input_tokens,
            "cached_input_tokens": self.cached_input_tokens,
            "output_tokens": self.output_tokens,
        })
    }
}

/// Millionths of a dollar as dollars.
fn usd(micros: i64) -> f64 {
    micros as f64 / 1_000_000.0
}

/// One group (a task or a day) with its implementer and reviewer spend.
#[derive(Default)]
struct Bucket {
    title: Option<String>,
    last_at: i64,
    imp: Spend,
    rev: Spend,
}

impl Bucket {
    fn value(&self, key_name: &str, key: &Option<String>) -> Value {
        let total = self.imp.micros + self.rev.micros;
        let mut item = json!({
            key_name: key,
            "impl": self.imp.value(),
            "rev": self.rev.value(),
            "total_usd": usd(total),
            "last_at": coordinator_core::timestamp(self.last_at),
        });
        if key_name == "task_id" {
            item["title"] = json!(self.title);
        }
        item
    }
}

/// The cost query grouped by the SQL expression `$key`.
macro_rules! grouped_sql {
    ($key:literal) => {
        concat!(
            "SELECT ", $key, " AS k, c.role, count(*) AS launches, COALESCE(sum(c.usd_micros),0) AS micros, \
             COALESCE(sum(c.usd_micros IS NULL),0) AS unpriced, sum(c.usd_estimated) AS estimated, \
             sum(c.input_tokens) AS input_tokens, sum(c.cached_input_tokens) AS cached_input_tokens, \
             sum(c.output_tokens) AS output_tokens, max(c.occurred_at) AS last_at, max(t.title) AS title \
             FROM cost_rows c LEFT JOIN tasks t ON t.id=c.task_id AND t.project_id=c.project_id \
             WHERE c.project_id=? AND (? IS NULL OR c.host=?) AND c.occurred_at>=? AND c.occurred_at<? \
             AND (?=1 OR (c.canary=0 AND (t.id IS NULL OR t.budget_exempt IS NULL))) \
             GROUP BY k, c.role"
        )
    };
}

/// Folds the grouped rows of project `p` into buckets by task or day.
async fn buckets(
    c: &mut SqliteConnection,
    p: &str,
    query: &CostQuery,
    group: Group,
) -> Result<HashMap<Option<String>, Bucket>, AppError> {
    let sql = match group {
        Group::Task => grouped_sql!("c.task_id"),
        Group::Day => grouped_sql!("strftime('%Y-%m-%d', c.occurred_at/1000, 'unixepoch')"),
    };
    let rows = sqlx::query(sql)
        .bind(p)
        .bind(&query.host)
        .bind(&query.host)
        .bind(query.since_ms.unwrap_or(i64::MIN))
        .bind(query.until_ms.unwrap_or(i64::MAX))
        .bind(i64::from(query.include_canary.unwrap_or(false)))
        .fetch_all(&mut *c)
        .await?;
    let mut buckets: HashMap<Option<String>, Bucket> = HashMap::new();
    for row in &rows {
        let bucket = buckets.entry(row.get("k")).or_default();
        let spend = Spend {
            launches: row.get("launches"),
            micros: row.get("micros"),
            unpriced: row.get("unpriced"),
            estimated: row.get("estimated"),
            input_tokens: row.get("input_tokens"),
            cached_input_tokens: row.get("cached_input_tokens"),
            output_tokens: row.get("output_tokens"),
        };
        match row.get::<String, _>("role").as_str() {
            "impl" => bucket.imp.add(&spend),
            _ => bucket.rev.add(&spend),
        }
        bucket.last_at = bucket.last_at.max(row.get("last_at"));
        bucket.title = bucket.title.take().or(row.get("title"));
    }
    Ok(buckets)
}

/// `GET …/telemetry/cost?group_by=task|day&host=&since_ms=&until_ms=&include_canary=&limit=`:
/// the project's launch cost per task or per UTC day, split into implementer
/// (`impl`) and reviewer (`rev`) spend, most recent group first, with the
/// totals of every matching row. `truncated` says `limit` cut groups off.
/// Canary probes (rows of the host's canary project and of budget-exempt
/// tasks) are left out unless `include_canary`.
async fn read_costs(
    State(s): State<AppState>,
    _auth: Auth,
    Path(p): Path<String>,
    Query(query): Query<CostQuery>,
) -> Reply {
    let (group, limit) = (query.group()?, query.limit()?);
    let mut c = s.pool.acquire().await?;
    require_project(&mut c, &p).await?;
    let buckets = buckets(&mut c, &p, &query, group).await?;
    let (mut imp, mut rev) = (Spend::default(), Spend::default());
    for bucket in buckets.values() {
        imp.add(&bucket.imp);
        rev.add(&bucket.rev);
    }
    let mut groups: Vec<_> = buckets.into_iter().collect();
    groups.sort_by(|(ka, a), (kb, b)| b.last_at.cmp(&a.last_at).then_with(|| kb.cmp(ka)));
    let truncated = groups.len() > limit;
    let key_name = if group == Group::Task {
        "task_id"
    } else {
        "day"
    };
    let items: Vec<Value> = (groups.iter().take(limit))
        .map(|(key, bucket)| bucket.value(key_name, key))
        .collect();
    let totals = json!({"impl": imp.value(), "rev": rev.value(),
                        "total_usd": usd(imp.micros + rev.micros)});
    Ok(response(json!({
        "project_id": p,
        "group_by": if group == Group::Task { "task" } else { "day" },
        "items": items, "truncated": truncated, "totals": totals,
    })))
}

/// The filters and page of a canary read.
#[derive(Deserialize)]
struct CanaryQuery {
    host: Option<String>,
    harness: Option<String>,
    outcome: Option<String>,
    since_ms: Option<i64>,
    limit: Option<i64>,
    before: Option<String>,
}

impl CanaryQuery {
    fn limit(&self) -> Result<i64, AppError> {
        match self.limit.unwrap_or(DEFAULT_LIST_LIMIT) {
            limit @ 1..=MAX_LIST_LIMIT => Ok(limit),
            _ => Err(AppError::bad_request("limit must be between 1 and 1000.")),
        }
    }
}

/// Serializes one stored canary result.
fn canary_value(row: &SqliteRow) -> Result<Value, AppError> {
    let served: Option<String> = row.get("served_by_json");
    Ok(json!({
        "id": row.get::<String, _>("id"),
        "project_id": row.get::<String, _>("project_id"),
        "host": row.get::<String, _>("host"),
        "harness": row.get::<String, _>("harness"),
        "outcome": row.get::<String, _>("outcome"),
        "duration_seconds": row.get::<f64, _>("duration_seconds"),
        "detail": row.get::<String, _>("detail"),
        "task_id": row.get::<Option<String>, _>("task_id"),
        "served_by": served.map(|s| serde_json::from_str::<Value>(&s)).transpose()?,
        "occurred_at": coordinator_core::timestamp(row.get("occurred_at")),
        "reported_by": row.get::<String, _>("reported_by"),
        "reported_at": coordinator_core::timestamp(row.get("reported_at")),
    }))
}

/// The (occurred_at, rowid) position of cursor result `before`; past the
/// newest result when there is no cursor.
async fn canary_cursor(
    c: &mut SqliteConnection,
    p: &str,
    before: Option<&str>,
) -> Result<(i64, i64), AppError> {
    let Some(id) = before else {
        return Ok((i64::MAX, i64::MAX));
    };
    let row =
        sqlx::query("SELECT occurred_at,rowid FROM canary_results WHERE id=? AND project_id=?")
            .bind(id)
            .bind(p)
            .fetch_optional(&mut *c)
            .await?
            .ok_or_else(AppError::not_found)?;
    Ok((row.get(0), row.get(1)))
}

/// `GET …/telemetry/canaries?host=&harness=&outcome=&since_ms=&limit=&before=`:
/// the project's canary history, newest run first, and `latest`, the newest
/// run of every (host, harness) the project has seen. `next_before` is the
/// cursor of the following page, or null on the last.
async fn read_canaries(
    State(s): State<AppState>,
    _auth: Auth,
    Path(p): Path<String>,
    Query(query): Query<CanaryQuery>,
) -> Reply {
    let limit = query.limit()?;
    let mut c = s.pool.acquire().await?;
    require_project(&mut c, &p).await?;
    let (at, rowid) = canary_cursor(&mut c, &p, query.before.as_deref()).await?;
    let rows = sqlx::query(
        "SELECT * FROM canary_results WHERE project_id=? AND (? IS NULL OR host=?) \
         AND (? IS NULL OR harness=?) AND (? IS NULL OR outcome=?) AND occurred_at>=? \
         AND (occurred_at,rowid)<(?,?) ORDER BY occurred_at DESC, rowid DESC LIMIT ?",
    )
    .bind(&p)
    .bind(&query.host)
    .bind(&query.host)
    .bind(&query.harness)
    .bind(&query.harness)
    .bind(&query.outcome)
    .bind(&query.outcome)
    .bind(query.since_ms.unwrap_or(i64::MIN))
    .bind(at)
    .bind(rowid)
    .bind(limit)
    .fetch_all(&mut *c)
    .await?;
    let items = rows
        .iter()
        .map(canary_value)
        .collect::<Result<Vec<_>, _>>()?;
    let full = i64::try_from(items.len()).unwrap_or(i64::MAX) == limit;
    let next = full
        .then(|| items.last().map(|r| r["id"].clone()))
        .flatten();
    let newest = sqlx::query(
        "SELECT * FROM canary_results c WHERE project_id=? AND NOT EXISTS \
         (SELECT 1 FROM canary_results d WHERE d.project_id=c.project_id AND d.host=c.host \
          AND d.harness=c.harness AND (d.occurred_at,d.rowid)>(c.occurred_at,c.rowid)) \
         ORDER BY host, harness",
    )
    .bind(&p)
    .fetch_all(&mut *c)
    .await?;
    let latest = newest
        .iter()
        .map(canary_value)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(response(json!({
        "project_id": p, "items": items, "next_before": next, "latest": latest,
    })))
}
