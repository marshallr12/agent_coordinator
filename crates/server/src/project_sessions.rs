//! Connected agent sessions per project (coordinator task 027aeac3): a
//! read-only list an owner uses to see which agents are still working in a
//! project before quiescing it for a test, a preflight or a cutover.
//!
//! Everything is derived from existing records. A session is bound to a
//! project when it acknowledged the project's instructions or holds any
//! attempt in it. Its last activity is the newest of its own registration,
//! that acknowledgment, and its attempts' claims, heartbeats, progress,
//! endings and checkpoints in the project; reads never count.
use crate::{auth::Auth, error::AppError, response, state::AppState};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
};
use coordinator_core::timestamp;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection, sqlite::SqliteRow};
use std::collections::HashMap;

type Reply = Result<Json<Value>, AppError>;

/// The window used when the caller names none.
const DEFAULT_WINDOW_HOURS: i64 = 24;
/// The widest window accepted (one year).
const MAX_WINDOW_HOURS: i64 = 8760;
/// Sessions returned per call; more set `truncated`.
const MAX_ITEMS: usize = 500;
/// Milliseconds per hour.
const HOUR_MS: i64 = 3_600_000;

/// Open, live-credential sessions bound to project `?1`, with their last
/// activity, filtered by the window (since `?5`; `?3` = 0 lists all) unless
/// they hold an active attempt in the project (even one whose lease lapsed).
///
/// It starts from the open sessions (`agent_sessions_open`) and reads each
/// one's own acknowledgment, attempts and checkpoints in the project through
/// indexed lookups (`instruction_acknowledgments` key, `attempts_session`,
/// `checkpoints_attempt_history`), so its cost follows the open sessions and
/// their own history, never the project's whole attempt or checkpoint history.
const SESSIONS_SQL: &str = "\
WITH open AS MATERIALIZED (
  SELECT s.id, s.created_at,
    (SELECT i.created_at FROM instruction_acknowledgments i
      WHERE i.session_id=s.id AND i.project_id=?1) AS ack_at,
    (SELECT max(max(a.created_at, a.last_heartbeat_at, a.last_progress_at,
        COALESCE(a.ended_at, 0),
        COALESCE((SELECT max(k.created_at) FROM checkpoints k
          WHERE k.project_id=a.project_id AND k.attempt_id=a.id), 0)))
      FROM attempts a WHERE a.session_id=s.id AND a.project_id=?1) AS attempt_at,
    EXISTS(SELECT 1 FROM attempts a WHERE a.session_id=s.id AND a.project_id=?1
      AND a.state='active') AS held
  FROM agent_sessions s WHERE s.closed_at IS NULL
),
bound AS (
  SELECT id, held, max(created_at, COALESCE(ack_at, 0), COALESCE(attempt_at, 0)) AS at
  FROM open WHERE ack_at IS NOT NULL OR attempt_at IS NOT NULL
)
SELECT s.id, s.principal_id, p.name AS principal_name, p.kind AS principal_kind,
  s.harness, s.workstation_id, s.capabilities, s.credential_id, s.created_at,
  s.parent_session_id, sub.name AS subagent_name, b.at AS last_activity_at
FROM bound b
JOIN agent_sessions s ON s.id=b.id
JOIN credentials c ON c.id=s.credential_id
JOIN principals p ON p.id=s.principal_id
LEFT JOIN subagent_identities sub ON sub.id=s.subagent_identity_id
WHERE c.revoked_at IS NULL AND (c.expires_at IS NULL OR c.expires_at>?2)
  AND p.disabled_at IS NULL AND (?3=0 OR b.at>=?5 OR b.held)
ORDER BY last_activity_at DESC, s.id
LIMIT ?4";

/// The active attempts in project `?1` held by the sessions in the JSON array
/// `?3`, with the workflow activity each one claims, if any. Attempts whose
/// lease lapsed before `?2` stay listed (the session may still be running
/// and the task needs recovery), flagged by `lease_expired`.
const HELD_SQL: &str = "\
SELECT a.id, a.session_id, a.task_id, t.title, a.generation, a.mode,
  a.expires_at, a.last_heartbeat_at, wa.kind AS activity_kind, wa.subject_task_id,
  a.expires_at<=?2 AS lease_expired
FROM attempts a
JOIN tasks t ON t.id=a.task_id
LEFT JOIN workflow_activities wa ON wa.activity_task_id=a.task_id
WHERE a.project_id=?1 AND a.state='active'
  AND a.session_id IN (SELECT value FROM json_each(?3))
ORDER BY a.created_at, a.id";

/// The project session routes.
pub fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/projects/{p}/sessions", get(list_sessions))
}

/// The query string of `GET …/sessions`.
#[derive(Deserialize)]
struct SessionsQuery {
    active_within_hours: Option<i64>,
}

impl SessionsQuery {
    /// The window in hours: the given value, else 24; refuses values
    /// outside 0..=8760 (0 lists every open bound session).
    fn window(&self) -> Result<i64, AppError> {
        match self.active_within_hours.unwrap_or(DEFAULT_WINDOW_HOURS) {
            hours @ 0..=MAX_WINDOW_HOURS => Ok(hours),
            _ => Err(AppError::bad_request(
                "active_within_hours must be between 0 and 8760.",
            )),
        }
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

/// Loads at most `MAX_ITEMS + 1` session rows so truncation is detectable.
async fn session_rows(
    c: &mut SqliteConnection,
    p: &str,
    now: i64,
    hours: i64,
) -> Result<Vec<SqliteRow>, AppError> {
    let limit = i64::try_from(MAX_ITEMS + 1).unwrap_or(i64::MAX);
    Ok(sqlx::query(SESSIONS_SQL)
        .bind(p)
        .bind(now)
        .bind(hours)
        .bind(limit)
        .bind(now.saturating_sub(hours.saturating_mul(HOUR_MS)))
        .fetch_all(&mut *c)
        .await?)
}

/// Serializes one held attempt row.
fn attempt_value(row: &SqliteRow) -> Value {
    json!({
        "attempt_id": row.get::<String, _>("id"),
        "task_id": row.get::<String, _>("task_id"),
        "task_title": row.get::<String, _>("title"),
        "generation": row.get::<i64, _>("generation"),
        "mode": row.get::<String, _>("mode"),
        "activity_kind": row.get::<Option<String>, _>("activity_kind"),
        "subject_task_id": row.get::<Option<String>, _>("subject_task_id"),
        "expires_at": timestamp(row.get("expires_at")),
        "last_heartbeat_at": timestamp(row.get("last_heartbeat_at")),
        "lease_expired": row.get::<bool, _>("lease_expired"),
    })
}

/// The held attempts of the listed sessions, grouped by session id, in one
/// query rather than one per session.
async fn held_attempts(
    c: &mut SqliteConnection,
    p: &str,
    now: i64,
    sessions: &[SqliteRow],
) -> Result<HashMap<String, Vec<Value>>, AppError> {
    let ids: Vec<String> = sessions.iter().map(|r| r.get("id")).collect();
    let rows = sqlx::query(HELD_SQL)
        .bind(p)
        .bind(now)
        .bind(serde_json::to_string(&ids)?)
        .fetch_all(&mut *c)
        .await?;
    let mut held: HashMap<String, Vec<Value>> = HashMap::new();
    for row in &rows {
        held.entry(row.get("session_id"))
            .or_default()
            .push(attempt_value(row));
    }
    Ok(held)
}

/// The subagent identity of a session row, or null for a top-level session.
fn subagent_value(row: &SqliteRow) -> Value {
    match row.get::<Option<String>, _>("subagent_name") {
        Some(name) => json!({
            "name": name,
            "parent_session_id": row.get::<Option<String>, _>("parent_session_id"),
        }),
        None => Value::Null,
    }
}

/// Serializes one session row with its held attempts; never includes the
/// session proof hash or the credential token hash.
fn session_value(row: &SqliteRow, held: Vec<Value>) -> Result<Value, AppError> {
    Ok(json!({
        "session_id": row.get::<String, _>("id"),
        "principal": {
            "id": row.get::<String, _>("principal_id"),
            "name": row.get::<String, _>("principal_name"),
            "kind": row.get::<String, _>("principal_kind"),
        },
        "harness": row.get::<String, _>("harness"),
        "workstation_id": row.get::<String, _>("workstation_id"),
        "capabilities": serde_json::from_str::<Value>(row.get("capabilities"))?,
        "subagent": subagent_value(row),
        "credential_id": row.get::<String, _>("credential_id"),
        "started_at": timestamp(row.get("created_at")),
        "last_activity_at": timestamp(row.get("last_activity_at")),
        "held_attempts": held,
    }))
}

/// The listed sessions as JSON items, attaching each one's held attempts.
async fn session_items(
    c: &mut SqliteConnection,
    p: &str,
    now: i64,
    rows: &[SqliteRow],
) -> Result<Vec<Value>, AppError> {
    let mut held = held_attempts(c, p, now, rows).await?;
    rows.iter()
        .map(|row| {
            let id: String = row.get("id");
            session_value(row, held.remove(&id).unwrap_or_default())
        })
        .collect()
}

/// `GET /api/v1/projects/{p}/sessions?active_within_hours=24`: the open
/// agent sessions bound to the project, most recently active first, for any
/// authenticated reader (including read-only credentials and browsers).
async fn list_sessions(
    State(s): State<AppState>,
    _auth: Auth,
    Path(p): Path<String>,
    Query(query): Query<SessionsQuery>,
) -> Reply {
    let hours = query.window()?;
    let now = s.now();
    let mut c = s.pool.acquire().await?;
    require_project(&mut c, &p).await?;
    let mut rows = session_rows(&mut c, &p, now, hours).await?;
    let truncated = rows.len() > MAX_ITEMS;
    rows.truncate(MAX_ITEMS);
    let items = session_items(&mut c, &p, now, &rows).await?;
    Ok(response(json!({
        "project_id": p, "active_within_hours": hours, "generated_at": timestamp(now),
        "items": items, "truncated": truncated,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Config;

    /// The `detail` lines of SQLite's plan for `SESSIONS_SQL` on a freshly
    /// migrated database.
    async fn sessions_plan() -> Vec<String> {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::open(Config {
            database_path: dir.path().join("plan.sqlite3"),
            ..Config::default()
        })
        .await
        .unwrap();
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "EXPLAIN QUERY PLAN {SESSIONS_SQL}"
        )))
        .bind("p")
        .bind(0_i64)
        .bind(24_i64)
        .bind(501_i64)
        .bind(0_i64)
        .fetch_all(&state.pool)
        .await
        .unwrap();
        rows.iter().map(|r| r.get::<String, _>("detail")).collect()
    }

    /// Every attempt and checkpoint read is an indexed search, and open
    /// sessions come from the partial index, so a call never scans the
    /// project's history.
    #[tokio::test]
    async fn sessions_query_never_scans_history() {
        let plan = sessions_plan().await;
        let scans: Vec<&String> = plan.iter().filter(|d| d.starts_with("SCAN ")).collect();
        assert_eq!(
            scans,
            ["SCAN s USING INDEX agent_sessions_open", "SCAN open"],
            "{plan:#?}"
        );
        assert!(
            plan.iter().any(|d| d.contains("attempts_session")),
            "{plan:#?}"
        );
    }
}
