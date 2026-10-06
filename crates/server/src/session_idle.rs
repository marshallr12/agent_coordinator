//! Closing idle agent sessions during storage maintenance (coordinator task
//! 601d1601). Clients rarely close their sessions, so without this the open
//! set grows forever and the connected-sessions list stops reflecting reality.
//!
//! A session is idle when its newest recorded activity in any project (the
//! same derivation as `GET /api/v1/projects/{p}/sessions`: registration,
//! instruction acknowledgments, attempt claims, heartbeats, progress, endings
//! and checkpoints) is older than the idle period, and it holds nothing that
//! closing would strand: no active attempt (even one whose lease lapsed), no
//! live job reporter, no held reservation, no unreconciled job, and no open
//! subagent session of its own. Each close records an `agent_session_closed`
//! event with reason `idle`; the client must register a fresh session.
use anyhow::Context;
use coordinator_core::timestamp;
use serde_json::json;
use sqlx::{Row, Sqlite, Transaction};

/// The idle period used when the operator names none.
pub const DEFAULT_SESSION_IDLE_DAYS: i64 = 7;
/// The longest idle period accepted (one year); 0 disables closing.
pub const MAX_SESSION_IDLE_DAYS: i64 = 366;
/// Milliseconds per day.
const DAY_MS: i64 = 86_400_000;

/// Up to `?3` open sessions whose last activity is before `?1` and that hold
/// nothing live at `?2`, oldest activity first.
const IDLE_SQL: &str = "\
WITH open AS (
  SELECT s.id, s.principal_id, max(s.created_at,
    COALESCE((SELECT max(i.created_at) FROM instruction_acknowledgments i
      WHERE i.session_id=s.id), 0),
    COALESCE((SELECT max(max(a.created_at, a.last_heartbeat_at, a.last_progress_at,
        COALESCE(a.ended_at, 0),
        COALESCE((SELECT max(k.created_at) FROM checkpoints k
          WHERE k.project_id=a.project_id AND k.attempt_id=a.id), 0)))
      FROM attempts a WHERE a.session_id=s.id), 0)) AS last_activity_at
  FROM agent_sessions s WHERE s.closed_at IS NULL
)
SELECT o.id, o.principal_id, o.last_activity_at FROM open o
WHERE o.last_activity_at<?1
  AND NOT EXISTS(SELECT 1 FROM attempts a WHERE a.session_id=o.id AND a.state='active')
  AND NOT EXISTS(SELECT 1 FROM reporters r WHERE r.session_id=o.id AND r.expires_at>?2)
  AND NOT EXISTS(SELECT 1 FROM attempts a JOIN reservations v
      ON v.project_id=a.project_id AND v.attempt_id=a.id
    WHERE a.session_id=o.id AND v.state='held')
  AND NOT EXISTS(SELECT 1 FROM attempts a JOIN jobs j ON j.attempt_id=a.id
    WHERE a.session_id=o.id AND j.reconciled_at IS NULL
      AND j.state NOT IN ('succeeded','failed','not_started'))
  AND NOT EXISTS(SELECT 1 FROM agent_sessions c
    WHERE c.parent_session_id=o.id AND c.closed_at IS NULL)
ORDER BY o.last_activity_at, o.id
LIMIT ?3";

/// One idle session chosen for closing.
struct Idle {
    id: String,
    principal_id: String,
    last_activity_at: i64,
}

/// The activity cutoff for `idle_days` at `now`, or `None` when disabled.
pub fn idle_cutoff(now: i64, idle_days: i64) -> Option<i64> {
    (idle_days > 0).then(|| now.saturating_sub(idle_days.saturating_mul(DAY_MS)))
}

/// Closes up to `limit` idle sessions inside the caller's writer transaction
/// and returns how many it closed.
pub async fn close_idle_batch(
    tx: &mut Transaction<'static, Sqlite>,
    now: i64,
    idle_days: i64,
    limit: i64,
) -> anyhow::Result<u64> {
    let Some(cutoff) = idle_cutoff(now, idle_days) else {
        return Ok(0);
    };
    let idle = idle_sessions(tx, cutoff, now, limit).await?;
    let mut closed = 0_u64;
    for session in &idle {
        if close_one(tx, session, now, idle_days).await? {
            closed = closed.saturating_add(1);
        }
    }
    Ok(closed)
}

/// Selects up to `limit` idle sessions.
async fn idle_sessions(
    tx: &mut Transaction<'static, Sqlite>,
    cutoff: i64,
    now: i64,
    limit: i64,
) -> anyhow::Result<Vec<Idle>> {
    let rows = sqlx::query(IDLE_SQL)
        .bind(cutoff)
        .bind(now)
        .bind(limit)
        .fetch_all(&mut **tx)
        .await
        .context("idle session selection failed")?;
    Ok(rows
        .iter()
        .map(|row| Idle {
            id: row.get("id"),
            principal_id: row.get("principal_id"),
            last_activity_at: row.get("last_activity_at"),
        })
        .collect())
}

/// Closes one session and records its event, attributed to the session's
/// own principal; false when it was already closed.
async fn close_one(
    tx: &mut Transaction<'static, Sqlite>,
    session: &Idle,
    now: i64,
    idle_days: i64,
) -> anyhow::Result<bool> {
    let updated =
        sqlx::query("UPDATE agent_sessions SET closed_at=? WHERE id=? AND closed_at IS NULL")
            .bind(now)
            .bind(&session.id)
            .execute(&mut **tx)
            .await?;
    if updated.rows_affected() == 0 {
        return Ok(false);
    }
    let data = json!({
        "reason": "idle",
        "last_activity_at": timestamp(session.last_activity_at),
        "idle_days": idle_days,
    });
    sqlx::query(
        "INSERT INTO events(project_id,actor_id,kind,record_id,data_json,created_at) \
         VALUES(NULL,?,'agent_session_closed',?,?,?)",
    )
    .bind(&session.principal_id)
    .bind(&session.id)
    .bind(data.to_string())
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(true)
}
