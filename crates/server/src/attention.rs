//! Attention budget (P3b): keep humans out of the loop unless they are needed.
//!
//! * A reversible decision with a recommendation that stays unanswered for 24
//!   hours proceeds with the recommendation ([`sweep_timed_out_decisions`]).
//! * The digest lists what proceeded on its own, what is about to, and the
//!   human-required interventions (HRI): open human-required integrator
//!   reports, stalled tasks (`repeated_attempt_failures`) and a stalled queue
//!   (`no_progress`: ready work and no task progress for the stall threshold).
//! * Tasks may declare the paths they touch; the integrator records the files
//!   that landed on the target outside its own results, and `next` skips a
//!   task whose paths overlap one of them from the last 24 hours.
use crate::{auth::Auth, error::AppError, mutation::Mutation, response, state::AppState};
use anyhow::Context;
use axum::{
    Json, Router,
    extract::{Path, Query, State, rejection::JsonRejection},
    http::HeaderMap,
    routing::{get, post},
};
use coordinator_core::timestamp;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection};
use std::{collections::BTreeSet, str::FromStr};

type Reply = Result<Json<Value>, AppError>;

const HOUR_MS: i64 = 3_600_000;
/// How long a reversible decision waits for an answer before it proceeds.
pub const DECISION_TIMEOUT_MS: i64 = 24 * HOUR_MS;
/// How far back a human change blocks overlapping tasks from `next`.
pub const OVERLAP_WINDOW_MS: i64 = 24 * HOUR_MS;
/// Consecutive attempts that ended without a submission before a task counts
/// as stalled.
pub const STALL_ATTEMPTS: i64 = 3;
/// Hours of ready work without task progress before the queue counts as
/// stalled, unless the service is configured otherwise.
pub const DEFAULT_STALL_HOURS: i64 = 6;
const DAY_MS: i64 = 24 * HOUR_MS;
const SWEEP_BATCH: i64 = 100;
const MAX_TASK_PATHS: usize = 50;
const MAX_SHIPPED_FILES: usize = 1_000;
const DIGEST_LIMIT: i64 = 100;
const DEFAULT_DIGEST_HOURS: i64 = 24;
const MAX_DIGEST_HOURS: i64 = 24 * 14;

/// A daily span of UTC hours that does not count toward the stall clock. The
/// span starts at `start` and ends before `end`; it wraps past midnight when
/// `start` is later than `end` (`22-07`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuietHours {
    start: u8,
    end: u8,
}

impl FromStr for QuietHours {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let invalid = || "quiet hours must be START-END in UTC hours 0 through 24, such as 22-07.";
        let (start, end) = text.split_once('-').ok_or_else(invalid)?;
        let start: u8 = start.trim().parse().map_err(|_| invalid())?;
        let end: u8 = end.trim().parse().map_err(|_| invalid())?;
        if start > 23 || end > 24 || start == end {
            return Err(invalid().to_owned());
        }
        Ok(Self { start, end })
    }
}

impl QuietHours {
    /// Milliseconds of `[from, to)` that fall outside the quiet hours.
    fn active_ms(quiet: Option<Self>, from: i64, to: i64) -> i64 {
        if to <= from {
            return 0;
        }
        let Some(quiet) = quiet else {
            return to - from;
        };
        let (start, end) = (
            i64::from(quiet.start) * HOUR_MS,
            i64::from(quiet.end) * HOUR_MS,
        );
        let spans: &[(i64, i64)] = if start < end {
            &[(start, end)]
        } else {
            &[(0, end), (start, DAY_MS)]
        };
        let mut quiet_ms = 0;
        let mut day = from.div_euclid(DAY_MS) * DAY_MS;
        while day < to {
            for (a, b) in spans {
                quiet_ms += ((day + b).min(to) - (day + a).max(from)).max(0);
            }
            day += DAY_MS;
        }
        to - from - quiet_ms
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/projects/{project}/digest", get(digest))
        .route(
            "/api/v1/projects/{project}/tasks/{task}/paths",
            post(set_task_paths),
        )
        .route(
            "/api/v1/projects/{project}/integrator/human-ships",
            post(record_human_ship),
        )
}

fn payload<T>(value: Result<Json<T>, JsonRejection>) -> Result<T, AppError> {
    value.map(|Json(v)| v).map_err(|_| {
        AppError::bad_request("The JSON body does not match this operation's request schema.")
    })
}

/// A repository-relative path without `.`/`..` segments or a trailing slash.
fn normalize_path(raw: &str) -> Result<String, AppError> {
    let path = raw.trim_end_matches('/');
    let ok = !path.is_empty()
        && path.len() <= 1024
        && !path.starts_with('/')
        && !path.contains(['\0', '\\'])
        && path
            .split('/')
            .all(|segment| !matches!(segment, "" | "." | ".."));
    if ok {
        Ok(path.to_owned())
    } else {
        Err(AppError::bad_request(
            "Paths must be nonempty, relative, slash-separated and free of . and .. segments.",
        ))
    }
}

/// True when the two paths are equal or one is a directory containing the other.
fn paths_overlap(a: &str, b: &str) -> bool {
    a == b
        || a.strip_prefix(b).is_some_and(|rest| rest.starts_with('/'))
        || b.strip_prefix(a).is_some_and(|rest| rest.starts_with('/'))
}

/// The files recorded as shipped by a human since `since`.
pub(crate) async fn recent_human_files(
    c: &mut SqliteConnection,
    project: &str,
    since: i64,
) -> Result<Vec<String>, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT DISTINCT path FROM human_shipped_files WHERE project_id=? AND shipped_at>? \
         ORDER BY path LIMIT 5000",
    )
    .bind(project)
    .bind(since)
    .fetch_all(&mut *c)
    .await?)
}

/// The task's declared paths that overlap one of `human_files`.
pub(crate) async fn overlapping_paths(
    c: &mut SqliteConnection,
    task: &str,
    human_files: &[String],
) -> Result<Vec<String>, AppError> {
    if human_files.is_empty() {
        return Ok(Vec::new());
    }
    let declared: Vec<String> =
        sqlx::query_scalar("SELECT path FROM task_paths WHERE task_id=? ORDER BY path")
            .bind(task)
            .fetch_all(&mut *c)
            .await?;
    Ok(declared
        .into_iter()
        .filter(|path| human_files.iter().any(|file| paths_overlap(path, file)))
        .collect())
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TaskPathsInput {
    paths: Vec<String>,
}

/// `POST /api/v1/projects/{project}/tasks/{task}/paths`: replaces the paths
/// the task touches. They steer `next` only; no judged field changes.
async fn set_task_paths(
    State(state): State<AppState>,
    auth: Auth,
    Path((project, task)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<TaskPathsInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    if input.paths.len() > MAX_TASK_PATHS {
        return Err(AppError::bad_request("Declare at most 50 paths per task."));
    }
    let paths = input
        .paths
        .iter()
        .map(|path| normalize_path(path))
        .collect::<Result<BTreeSet<_>, _>>()?;
    let mut m = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{project}/tasks/{task}/paths"),
        &input,
    )
    .await?;
    let exists: i64 = sqlx::query_scalar("SELECT count(*) FROM tasks WHERE project_id=? AND id=?")
        .bind(&project)
        .bind(&task)
        .fetch_one(&mut *m.tx)
        .await?;
    if exists != 1 {
        return Err(AppError::not_found());
    }
    if let Some(value) = m.replay {
        return Ok(response(value));
    }
    sqlx::query("DELETE FROM task_paths WHERE task_id=?")
        .bind(&task)
        .execute(&mut *m.tx)
        .await?;
    for path in &paths {
        sqlx::query("INSERT INTO task_paths(project_id,task_id,path) VALUES(?,?,?)")
            .bind(&project)
            .bind(&task)
            .bind(path)
            .execute(&mut *m.tx)
            .await?;
    }
    let value = json!({"task_id": task, "paths": paths});
    Ok(response(
        m.finish(value, Some(&project), "task.paths_set", &task)
            .await?,
    ))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct HumanShipInput {
    commit: String,
    files: Vec<String>,
}

/// `POST /api/v1/projects/{project}/integrator/human-ships`: the integrator
/// records the files of a commit that reached the target outside its own
/// results, so `next` can steer agents away from them for a day.
async fn record_human_ship(
    State(state): State<AppState>,
    auth: Auth,
    Path(project): Path<String>,
    headers: HeaderMap,
    body: Result<Json<HumanShipInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    if !(7..=64).contains(&input.commit.len())
        || !input.commit.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(AppError::bad_request("commit must be a hexadecimal SHA."));
    }
    if input.files.len() > MAX_SHIPPED_FILES {
        return Err(AppError::bad_request(
            "Record at most 1,000 files per commit.",
        ));
    }
    let files = input
        .files
        .iter()
        .map(|file| normalize_path(file))
        .collect::<Result<BTreeSet<_>, _>>()?;
    let mut m = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{project}/integrator/human-ships"),
        &input,
    )
    .await?;
    crate::integrator::require_integrator(&m.actor)?;
    crate::integrator::require_integrator_project(&mut m.tx, &project).await?;
    if let Some(value) = m.replay {
        return Ok(response(value));
    }
    for file in &files {
        sqlx::query(
            "INSERT INTO human_shipped_files(project_id,commit_sha,path,recorded_by,shipped_at) \
             VALUES(?,?,?,?,?)",
        )
        .bind(&project)
        .bind(&input.commit)
        .bind(file)
        .bind(&m.actor.id)
        .bind(m.now)
        .execute(&mut *m.tx)
        .await?;
    }
    let value = json!({"commit": input.commit, "recorded_files": files.len()});
    Ok(response(
        m.finish(value, Some(&project), "human_ship.recorded", &input.commit)
            .await?,
    ))
}

/// Answers every reversible decision whose current cycle has waited
/// [`DECISION_TIMEOUT_MS`] with its recommendation. The answer is an `allow`
/// flagged `timed_out`, attributed to the principal that asked. Decisions
/// whose scope went stale or whose own expiry passed are left alone: the
/// recommendation was made about a different scope. A decision that requires a
/// human is answered only when a human created or last reopened it, so an
/// agent can never run out the clock on a reserved decision. Returns the ids
/// answered.
pub async fn sweep_timed_out_decisions(state: &AppState) -> anyhow::Result<Vec<String>> {
    let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    let clock = state.sample_clock(&mut tx).await?;
    if clock.incident_detected || clock.incident_active {
        tx.commit().await?;
        anyhow::bail!("clock_reconciliation_required: decision timeouts wait for a safe clock");
    }
    let ready: bool = sqlx::query_scalar(
        "SELECT coordination_state='ready' FROM service_state WHERE singleton=1",
    )
    .fetch_one(&mut *tx)
    .await?;
    if !ready {
        tx.commit().await?;
        return Ok(Vec::new());
    }
    let now = clock.now;
    let due = sqlx::query(
        "SELECT d.id,d.project_id,d.current_generation,d.created_by,d.recommendation,\
         c.policy_revision,c.expires_at FROM decisions d \
         JOIN decision_cycles c ON c.decision_id=d.id AND c.generation=d.current_generation \
         WHERE d.reversible=1 AND d.recommendation IS NOT NULL AND c.created_at<=? \
         AND (d.required_actor<>'human' OR EXISTS(SELECT 1 FROM principals o \
         WHERE o.id=c.opened_by AND o.kind='human')) \
         AND NOT EXISTS(SELECT 1 FROM decision_answers a WHERE a.decision_id=d.id \
         AND a.generation=d.current_generation) \
         ORDER BY c.created_at,d.id LIMIT ?",
    )
    .bind(now - DECISION_TIMEOUT_MS)
    .bind(SWEEP_BATCH)
    .fetch_all(&mut *tx)
    .await?;
    let mut answered = Vec::new();
    for row in due {
        let id: String = row.get("id");
        let project: String = row.get("project_id");
        let generation: i64 = row.get("current_generation");
        let asker: String = row.get("created_by");
        let scope = crate::knowledge::ensure_decision_scope_current(
            &mut tx,
            &project,
            &id,
            generation,
            row.get("policy_revision"),
            now,
            row.get("expires_at"),
        )
        .await;
        if scope.is_err() {
            continue;
        }
        sqlx::query(
            "INSERT INTO decision_answers(decision_id,generation,disposition,answer,rationale,\
             actor_id,actor_session_id,conditions_confirmed,created_at,timed_out) \
             VALUES(?,?,'allow',?,?,?,NULL,1,?,1)",
        )
        .bind(&id)
        .bind(generation)
        .bind(row.get::<String, _>("recommendation"))
        .bind("Unanswered for 24 hours; proceeding with the recorded recommendation because the decision is reversible.")
        .bind(&asker)
        .bind(now)
        .execute(&mut *tx)
        .await
        .context("timed-out decision answer failed")?;
        sqlx::query(
            "INSERT INTO events(project_id,actor_id,kind,record_id,data_json,created_at) \
             VALUES(?,?,'decision.timed_out',?,'{}',?)",
        )
        .bind(&project)
        .bind(&asker)
        .bind(&id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        answered.push(id);
    }
    tx.commit().await?;
    Ok(answered)
}

#[derive(Deserialize)]
struct DigestQuery {
    hours: Option<i64>,
}

/// Open tasks whose last [`STALL_ATTEMPTS`] attempts all ended without a
/// submission, with the stalled attempt count.
async fn stalled_tasks(c: &mut SqliteConnection, project: &str) -> Result<Vec<Value>, AppError> {
    let rows = sqlx::query(
        "SELECT t.id,t.title,t.priority FROM tasks t WHERE t.project_id=? AND t.lifecycle='open' \
         AND t.archived_at IS NULL AND (SELECT count(*) FROM (SELECT state FROM attempts a \
         WHERE a.task_id=t.id ORDER BY a.generation DESC LIMIT ?) WHERE state IN \
         ('released','blocked','expired','canceled'))=? ORDER BY t.priority,t.created_at,t.id LIMIT ?",
    )
    .bind(project)
    .bind(STALL_ATTEMPTS)
    .bind(STALL_ATTEMPTS)
    .bind(DIGEST_LIMIT)
    .fetch_all(&mut *c)
    .await?;
    Ok(rows
        .iter()
        .map(|row| {
            json!({"code": "stalled_task", "rule": "repeated_attempt_failures",
                   "required_actor": "human",
                   "task_id": row.get::<String, _>("id"),
                   "title": row.get::<String, _>("title"),
                   "priority": row.get::<i64, _>("priority"),
                   "failed_attempts": STALL_ATTEMPTS})
        })
        .collect())
}

/// One item when the project holds ready work (open, unblocked, unowned, past
/// its dependencies and not waiting on review or integration) and nothing
/// moved on any task (a claim, checkpoint, submission, review or integration
/// result) for more than `stall_hours` outside the quiet hours. The clock starts
/// at the later of the last progress and the time the oldest ready task became
/// ready, so a queue that just filled is not already stalled. The digest is
/// computed on demand, so one continuing stall is one item however long it lasts.
async fn stalled_queue(
    c: &mut SqliteConnection,
    project: &str,
    now: i64,
    config: &crate::state::Config,
) -> Result<Option<Value>, AppError> {
    let ready = sqlx::query(
        "SELECT count(*) AS ready,min(t.ready_since) AS since FROM tasks t \
         WHERE t.project_id=? AND t.lifecycle='open' AND t.archived_at IS NULL \
         AND t.blocked_reason IS NULL \
         AND NOT EXISTS(SELECT 1 FROM task_dependencies d JOIN tasks p ON p.id=d.prerequisite_id \
         WHERE d.task_id=t.id AND p.lifecycle!='done') \
         AND NOT EXISTS(SELECT 1 FROM attempts a WHERE a.id=t.current_attempt_id \
         AND a.state IN ('active','submitted')) \
         AND NOT EXISTS(SELECT 1 FROM workflow_subjects ws WHERE ws.task_id=t.id \
         AND ws.phase!='revision_needed')",
    )
    .bind(project)
    .fetch_one(&mut *c)
    .await?;
    let ready_tasks: i64 = ready.get("ready");
    let Some(ready_since) = ready.get::<Option<i64>, _>("since") else {
        return Ok(None);
    };
    let progress: Option<i64> = sqlx::query_scalar(
        "SELECT max(at) FROM (\
         SELECT max(created_at) AS at FROM attempts WHERE project_id=?1 \
         UNION ALL SELECT max(created_at) FROM checkpoints WHERE project_id=?1 \
         UNION ALL SELECT max(created_at) FROM submissions WHERE project_id=?1 \
         UNION ALL SELECT max(r.created_at) FROM review_decisions r \
         JOIN submissions s ON s.id=r.submission_id WHERE s.project_id=?1 \
         UNION ALL SELECT max(i.created_at) FROM integration_authorizations i \
         JOIN submissions s ON s.id=i.submission_id WHERE s.project_id=?1 \
         UNION ALL SELECT max(i.created_at) FROM integration_results i \
         JOIN submissions s ON s.id=i.submission_id WHERE s.project_id=?1)",
    )
    .bind(project)
    .fetch_one(&mut *c)
    .await?;
    let since = progress.unwrap_or(0).max(ready_since);
    let idle_ms = QuietHours::active_ms(config.quiet_hours, since, now);
    if idle_ms <= config.stall_hours * HOUR_MS {
        return Ok(None);
    }
    Ok(Some(json!({
        "code": "stalled_queue", "rule": "no_progress", "required_actor": "human",
        "summary": format!(
            "{ready_tasks} ready task(s) and no task progress for {} hours (threshold {})",
            idle_ms / HOUR_MS, config.stall_hours),
        "ready_tasks": ready_tasks,
        "idle_hours": idle_ms / HOUR_MS,
        "threshold_hours": config.stall_hours,
        "last_progress_at": progress.map(timestamp),
        "stalled_since": timestamp(since),
    })))
}

/// `GET /api/v1/projects/{project}/digest?hours=N`: what the attention budget
/// did in the last `hours` (default 24) and what still needs a human.
async fn digest(
    State(state): State<AppState>,
    _auth: Auth,
    Path(project): Path<String>,
    Query(query): Query<DigestQuery>,
) -> Reply {
    let hours = query.hours.unwrap_or(DEFAULT_DIGEST_HOURS);
    if !(1..=MAX_DIGEST_HOURS).contains(&hours) {
        return Err(AppError::bad_request("hours must be between 1 and 336."));
    }
    let mut c = state.pool.acquire().await?;
    let exists: i64 = sqlx::query_scalar("SELECT count(*) FROM projects WHERE id=?")
        .bind(&project)
        .fetch_one(&mut *c)
        .await?;
    if exists != 1 {
        return Err(AppError::not_found());
    }
    let now = state.now();
    let since = now - hours * HOUR_MS;
    let proceeded = sqlx::query(
        "SELECT d.id,d.question,a.answer,a.created_at,\
         (SELECT group_concat(task_id) FROM decision_affected_tasks dt WHERE dt.decision_id=d.id \
         AND dt.generation=a.generation) AS tasks FROM decision_answers a \
         JOIN decisions d ON d.id=a.decision_id WHERE d.project_id=? AND a.timed_out=1 \
         AND a.created_at>? ORDER BY a.created_at,d.id LIMIT ?",
    )
    .bind(&project)
    .bind(since)
    .bind(DIGEST_LIMIT)
    .fetch_all(&mut *c)
    .await?;
    let upcoming = sqlx::query(
        "SELECT d.id,d.question,d.recommendation,c.created_at FROM decisions d \
         JOIN decision_cycles c ON c.decision_id=d.id AND c.generation=d.current_generation \
         WHERE d.project_id=? AND d.reversible=1 AND d.recommendation IS NOT NULL \
         AND (d.required_actor<>'human' OR EXISTS(SELECT 1 FROM principals o \
         WHERE o.id=c.opened_by AND o.kind='human')) \
         AND NOT EXISTS(SELECT 1 FROM decision_answers a WHERE a.decision_id=d.id \
         AND a.generation=d.current_generation) AND (c.expires_at IS NULL OR c.expires_at>?) \
         ORDER BY c.created_at,d.id LIMIT ?",
    )
    .bind(&project)
    .bind(now)
    .bind(DIGEST_LIMIT)
    .fetch_all(&mut *c)
    .await?;
    let mut items = stalled_tasks(&mut c, &project).await?;
    let stalled = items.len();
    let queue = stalled_queue(&mut c, &project, now, &state.config).await?;
    let stalled_queue = usize::from(queue.is_some());
    items.extend(queue);
    items.extend(crate::integrator_reports::human_queue_items(&mut c, &project).await?);
    Ok(response(json!({
        "project_id": project,
        "window_hours": hours,
        "since": timestamp(since),
        "until": timestamp(now),
        "proceeded_decisions": proceeded.iter().map(|row| json!({
            "decision_id": row.get::<String, _>("id"),
            "question": row.get::<String, _>("question"),
            "proceeded_with": row.get::<String, _>("answer"),
            "proceeded_at": timestamp(row.get("created_at")),
            "affected_task_ids": row.get::<Option<String>, _>("tasks")
                .map(|ids| ids.split(',').map(str::to_owned).collect::<Vec<_>>())
                .unwrap_or_default(),
        })).collect::<Vec<_>>(),
        "pending_reversible_decisions": upcoming.iter().map(|row| json!({
            "decision_id": row.get::<String, _>("id"),
            "question": row.get::<String, _>("question"),
            "recommendation": row.get::<String, _>("recommendation"),
            "proceeds_at": timestamp(row.get::<i64, _>("created_at") + DECISION_TIMEOUT_MS),
        })).collect::<Vec<_>>(),
        "hri": {"count": items.len(), "stalled_tasks": stalled,
                "stalled_queue": stalled_queue, "items": items},
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_overlaps_the_files_below_it_and_nothing_else() {
        assert!(paths_overlap("crates/server", "crates/server/src/next.rs"));
        assert!(paths_overlap("crates/server/src/next.rs", "crates/server"));
        assert!(paths_overlap("README.md", "README.md"));
        assert!(!paths_overlap("crates/server", "crates/server2/lib.rs"));
        assert!(!paths_overlap("a/b", "a/c"));
    }

    #[test]
    fn quiet_hours_are_parsed_and_removed_from_the_stall_clock() {
        assert!("22".parse::<QuietHours>().is_err());
        assert!("5-5".parse::<QuietHours>().is_err());
        assert!("24-3".parse::<QuietHours>().is_err());
        let overnight: QuietHours = "22-07".parse().unwrap();
        let day = |h: i64| 10 * DAY_MS + h * HOUR_MS;
        // 20:00 to 08:00 the next day holds 9 quiet hours (22-07) and 3 active.
        let active = QuietHours::active_ms(Some(overnight), day(20), day(32));
        assert_eq!(active, 3 * HOUR_MS);
        let daytime: QuietHours = "9-17".parse().unwrap();
        assert_eq!(
            QuietHours::active_ms(Some(daytime), day(0), day(24)),
            16 * HOUR_MS
        );
        assert_eq!(QuietHours::active_ms(None, day(0), day(5)), 5 * HOUR_MS);
        assert_eq!(QuietHours::active_ms(Some(daytime), day(5), day(1)), 0);
    }

    #[test]
    fn paths_are_normalized_or_refused() {
        assert_eq!(normalize_path("crates/server/").unwrap(), "crates/server");
        for bad in ["", "/etc", "a/../b", "a//b", "./a", "a\\b", "/"] {
            assert!(normalize_path(bad).is_err(), "{bad}");
        }
    }
}
