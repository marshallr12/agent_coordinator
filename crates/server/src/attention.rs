//! Attention budget (P3b): keep humans out of the loop unless they are needed.
//!
//! * A reversible decision with a recommendation that stays unanswered for 24
//!   hours proceeds with the recommendation ([`sweep_timed_out_decisions`]).
//! * The digest lists what proceeded on its own, what is about to, and the
//!   human-required interventions (HRI): open human-required integrator
//!   reports, stalled tasks (`repeated_attempt_failures`) and a stalled queue
//!   (`no_progress`: ready work and no task progress for the stall threshold).
//! * The digest also lists the agent tasks the weekly admission budget held
//!   (see [`crate::admission`]).
//! * Canary tasks (`admission_class: "canary"`) are not counted: they are
//!   never stalled tasks, ready work, progress, or human-required reports.
//! * Tasks may declare the paths they touch; the integrator records the files
//!   that landed on the target outside its own results, and `next` skips a
//!   task whose paths overlap one of them from the last 24 hours.
use crate::{auth::Auth, error::AppError, mutation::Mutation, response, state::AppState};
use anyhow::Context;
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, Query, State, rejection::JsonRejection},
    http::{HeaderMap, Method, StatusCode},
    response::Html,
    routing::{get, post},
};
use coordinator_core::timestamp;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection};
use std::{collections::BTreeSet, str::FromStr};
use subtle::ConstantTimeEq;

type Reply = Result<Json<Value>, AppError>;

const HOUR_MS: i64 = 3_600_000;
/// The canary tasks (see [`crate::admission`]): probes of the pipeline, left
/// out of the digest's human interventions and its progress clock so that
/// the dogfood figures describe real work.
macro_rules! canary_tasks {
    () => {
        "(SELECT id FROM tasks WHERE budget_exempt IS NOT NULL)"
    };
}
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
/// How long an acknowledgement link in an emailed digest stays valid.
pub const ACK_LINK_TTL_MS: i64 = 7 * DAY_MS;
const ACK_PURPOSE: &str = "agentc-digest-ack-v1";

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
            "/api/v1/projects/{project}/digest/read",
            post(mark_digest_read),
        )
        .route(
            "/api/v1/projects/{project}/digest/sender",
            post(set_digest_sender),
        )
        .route(
            "/api/v1/projects/{project}/digest/ack",
            get(ack_page).post(ack_digest),
        )
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
    ack_link: Option<bool>,
}

/// True for the two requests the signed acknowledgement link makes. They carry
/// no credential: the token in the link is the whole authority.
pub(crate) fn is_ack_link(method: &Method, path: &str) -> bool {
    matches!(*method, Method::GET | Method::POST)
        && path
            .strip_prefix("/api/v1/projects/")
            .and_then(|rest| rest.strip_suffix("/digest/ack"))
            .is_some_and(|project| !project.is_empty() && !project.contains('/'))
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let key = if key.len() > 64 {
        Sha256::digest(key).to_vec()
    } else {
        key.to_vec()
    };
    let (mut inner, mut outer) = ([0x36u8; 64], [0x5cu8; 64]);
    for (i, byte) in key.iter().enumerate() {
        inner[i] ^= byte;
        outer[i] ^= byte;
    }
    let inner_hash = Sha256::new()
        .chain_update(inner)
        .chain_update(message)
        .finalize();
    Sha256::new()
        .chain_update(outer)
        .chain_update(inner_hash)
        .finalize()
        .into()
}

fn ack_signature(key: &[u8], project: &str, expires_at: i64, minter: &str) -> String {
    hex::encode(hmac_sha256(
        key,
        format!("{ACK_PURPOSE}\n{project}\n{expires_at}\n{minter}").as_bytes(),
    ))
}

/// The acknowledgement token for `project`, valid until `expires_at`, minted by
/// the principal `minter`.
fn ack_token(key: &[u8], project: &str, expires_at: i64, minter: &str) -> String {
    format!(
        "{expires_at}.{minter}.{}",
        ack_signature(key, project, expires_at, minter)
    )
}

/// Checks `token` against the project, the signing key and the clock, and
/// returns the principal that minted it.
fn check_ack_token(key: &[u8], project: &str, token: &str, now: i64) -> Result<String, AppError> {
    let invalid = link_invalid;
    let mut parts = token.split('.');
    let (Some(expires), Some(minter), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(invalid());
    };
    let expires_at: i64 = expires.parse().map_err(|_| invalid())?;
    let expected = ack_signature(key, project, expires_at, minter);
    if !bool::from(expected.as_bytes().ct_eq(signature.as_bytes())) {
        return Err(invalid());
    }
    if now >= expires_at {
        return Err(AppError::new(
            StatusCode::GONE,
            "digest_link_expired",
            "This acknowledgement link has expired. Open the digest in the dashboard instead.",
        ));
    }
    Ok(minter.to_owned())
}

/// The agent principal the owner designated to send `project`'s digest.
async fn digest_sender(
    c: &mut SqliteConnection,
    project: &str,
) -> Result<Option<String>, AppError> {
    Ok(
        sqlx::query_scalar("SELECT principal_id FROM digest_senders WHERE project_id=?")
            .bind(project)
            .fetch_optional(&mut *c)
            .await?,
    )
}

/// Whether `principal` may still vouch for an acknowledgement link: an enabled
/// human, or the enabled agent principal currently designated as the sender.
async fn may_mint_ack_link(
    c: &mut SqliteConnection,
    project: &str,
    principal: &str,
) -> Result<bool, AppError> {
    let row = sqlx::query("SELECT kind FROM principals WHERE id=? AND disabled_at IS NULL")
        .bind(principal)
        .fetch_optional(&mut *c)
        .await?;
    Ok(match row.map(|row| row.get::<String, _>("kind")) {
        Some(kind) if kind == "human" => true,
        Some(_) => digest_sender(c, project).await?.as_deref() == Some(principal),
        None => false,
    })
}

fn link_invalid() -> AppError {
    AppError::new(
        StatusCode::BAD_REQUEST,
        "digest_link_invalid",
        "This acknowledgement link is not valid.",
    )
}

async fn ack_key(c: &mut SqliteConnection) -> Result<Vec<u8>, AppError> {
    Ok(
        sqlx::query_scalar("SELECT key FROM digest_ack_key WHERE singleton=1")
            .fetch_one(&mut *c)
            .await?,
    )
}

async fn project_exists(c: &mut SqliteConnection, project: &str) -> Result<bool, AppError> {
    let exists: i64 = sqlx::query_scalar("SELECT count(*) FROM projects WHERE id=?")
        .bind(project)
        .fetch_one(&mut *c)
        .await?;
    Ok(exists == 1)
}

async fn record_read(
    c: &mut SqliteConnection,
    project: &str,
    now: i64,
    via: &str,
    minted_by: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO digest_reads(project_id,last_read_at,read_via,minted_by) VALUES(?,?,?,?) \
         ON CONFLICT(project_id) DO UPDATE SET last_read_at=excluded.last_read_at,\
         read_via=excluded.read_via,minted_by=excluded.minted_by",
    )
    .bind(project)
    .bind(now)
    .bind(via)
    .bind(minted_by)
    .execute(&mut *c)
    .await?;
    Ok(())
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DigestReadInput {}

/// `POST /api/v1/projects/{project}/digest/read`: a human opened the digest in
/// the dashboard. Agents read the digest without marking it read.
async fn mark_digest_read(
    State(state): State<AppState>,
    auth: Auth,
    Path(project): Path<String>,
    headers: HeaderMap,
    body: Result<Json<DigestReadInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    let mut m = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{project}/digest/read"),
        &input,
    )
    .await?;
    if m.actor.kind != "human" {
        return Err(AppError::forbidden(
            "Only a human reading the digest marks it read.",
        ));
    }
    if !project_exists(&mut m.tx, &project).await? {
        return Err(AppError::not_found());
    }
    if let Some(value) = m.replay {
        return Ok(response(value));
    }
    record_read(&mut m.tx, &project, m.now, "dashboard", None).await?;
    let value = json!({"project_id": project, "last_read_at": timestamp(m.now)});
    Ok(response(
        m.finish(value, Some(&project), "digest.read", &project)
            .await?,
    ))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DigestSenderInput {
    /// The agent principal to designate, or `null` to clear the designation.
    principal_id: Option<String>,
}

async fn require_agent_principal(
    c: &mut SqliteConnection,
    principal: &str,
) -> Result<(), AppError> {
    let valid: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM principals WHERE id=? AND kind='agent' AND role='agent' \
         AND disabled_at IS NULL",
    )
    .bind(principal)
    .fetch_one(&mut *c)
    .await?;
    if valid == 0 {
        return Err(AppError::bad_request(
            "The digest sender must be an enabled agent principal.",
        ));
    }
    Ok(())
}

/// Designates `principal` as the project's digest sender, or clears the
/// designation. `designated_by` is `None` for the host-local command.
async fn store_digest_sender(
    c: &mut SqliteConnection,
    project: &str,
    principal: Option<&str>,
    designated_by: Option<&str>,
    now: i64,
) -> Result<(), AppError> {
    if let Some(principal) = principal {
        sqlx::query(
            "INSERT INTO digest_senders(project_id,principal_id,designated_by,designated_at) \
             VALUES(?,?,?,?) ON CONFLICT(project_id) DO UPDATE SET \
             principal_id=excluded.principal_id,designated_by=excluded.designated_by,\
             designated_at=excluded.designated_at",
        )
        .bind(project)
        .bind(principal)
        .bind(designated_by)
        .bind(now)
        .execute(&mut *c)
        .await?;
    } else {
        sqlx::query("DELETE FROM digest_senders WHERE project_id=?")
            .bind(project)
            .execute(&mut *c)
            .await?;
    }
    Ok(())
}

/// Host-local designation of the digest sender, by agent name (`None` clears
/// it). Not mounted as an HTTP route: the owner runs it on the host, where the
/// digest timer's token lives. The event names the affected agent as its
/// subject, like the other host-local recoveries.
pub async fn designate_digest_sender(
    state: &AppState,
    project: &str,
    agent: Option<&str>,
    reason: &str,
) -> Result<Value, AppError> {
    if reason.trim() != reason
        || reason.is_empty()
        || reason.len() > 500
        || reason.chars().any(char::is_control)
    {
        return Err(AppError::bad_request(
            "The reason must contain 1 to 500 bytes without control characters or surrounding whitespace.",
        ));
    }
    let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    let clock = state.sample_clock(&mut tx).await?;
    if clock.incident_detected {
        tx.commit().await?;
        return Err(crate::state::clock_reconciliation_error());
    }
    if clock.incident_active {
        return Err(crate::state::clock_reconciliation_error());
    }
    if !project_exists(&mut tx, project).await? {
        return Err(AppError::not_found());
    }
    let subject: String = match agent {
        Some(name) => sqlx::query_scalar(
            "SELECT id FROM principals WHERE name=? AND kind='agent' AND role='agent' \
             AND disabled_at IS NULL",
        )
        .bind(name)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::bad_request("The digest sender must be an enabled agent."))?,
        None => digest_sender(&mut tx, project)
            .await?
            .ok_or_else(|| AppError::bad_request("This project has no digest sender to clear."))?,
    };
    let sender = agent.map(|_| subject.as_str());
    store_digest_sender(&mut tx, project, sender, None, clock.now).await?;
    let event_data = serde_json::to_string(&json!({
        "reason": reason,
        "host_local": true,
        "initiator_kind": "host_operator",
        "authenticated_principal_id": Value::Null,
        "subject_principal_id": subject,
        "digest_sender": sender,
        "actor_id_role": "subject_reference",
    }))?;
    sqlx::query(
        "INSERT INTO events(project_id,actor_id,kind,record_id,data_json,created_at) \
         VALUES(?,?,'digest.sender_set',?,?,?)",
    )
    .bind(project)
    .bind(&subject)
    .bind(project)
    .bind(event_data)
    .bind(clock.now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(json!({"project_id": project, "digest_sender": sender, "host_local": true}))
}

/// `POST /api/v1/projects/{project}/digest/sender`: a human administrator
/// designates the one agent principal that may mint acknowledgement links for
/// the project's digest, or clears the designation.
async fn set_digest_sender(
    State(state): State<AppState>,
    auth: Auth,
    Path(project): Path<String>,
    headers: HeaderMap,
    body: Result<Json<DigestSenderInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    let mut m = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{project}/digest/sender"),
        &input,
    )
    .await?;
    crate::auth::admin(&m.actor)?;
    if !project_exists(&mut m.tx, &project).await? {
        return Err(AppError::not_found());
    }
    if let Some(value) = m.replay {
        return Ok(response(value));
    }
    if let Some(principal) = &input.principal_id {
        require_agent_principal(&mut m.tx, principal).await?;
    }
    store_digest_sender(
        &mut m.tx,
        &project,
        input.principal_id.as_deref(),
        Some(&m.actor.id),
        m.now,
    )
    .await?;
    let value = json!({"project_id": project, "digest_sender": input.principal_id});
    Ok(response(
        m.finish(value, Some(&project), "digest.sender_set", &project)
            .await?,
    ))
}

#[derive(Deserialize)]
struct AckQuery {
    token: String,
}

fn ack_html(heading: &str, body: &str) -> Html<String> {
    Html(format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <title>agentc attention digest</title></head><body><h1>{heading}</h1>{body}</body></html>"
    ))
}

/// `GET .../digest/ack?token=`: a page with one button. Mail scanners and link
/// previews fetch links without clicking, so only the button's POST records.
async fn ack_page(
    State(state): State<AppState>,
    Path(project): Path<String>,
    Query(query): Query<AckQuery>,
) -> Result<Html<String>, AppError> {
    let mut c = state.pool.acquire().await?;
    let key = ack_key(&mut c).await?;
    let minter = check_ack_token(&key, &project, &query.token, state.now())?;
    if !project_exists(&mut c, &project).await? {
        return Err(AppError::not_found());
    }
    if !may_mint_ack_link(&mut c, &project, &minter).await? {
        return Err(link_invalid());
    }
    // A valid token is digits, hex, a principal id and dots, so it needs no
    // escaping.
    Ok(ack_html(
        "Attention digest",
        &format!(
            "<form method=\"post\" action=\"/api/v1/projects/{project}/digest/ack\">\
             <input type=\"hidden\" name=\"token\" value=\"{}\">\
             <button type=\"submit\">I read this</button></form>",
            query.token
        ),
    ))
}

/// `POST .../digest/ack` with a form body `token=`: records that the owner read
/// the digest, naming the principal that minted the link. The token authorizes
/// this one effect and nothing else, and only while its minter may still mint.
async fn ack_digest(
    State(state): State<AppState>,
    Path(project): Path<String>,
    body: Bytes,
) -> Result<Html<String>, AppError> {
    let token = url::form_urlencoded::parse(&body)
        .find(|(name, _)| name == "token")
        .map(|(_, value)| value.into_owned())
        .ok_or_else(|| AppError::bad_request("The form needs a token field."))?;
    let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    let clock = state.sample_clock(&mut tx).await?;
    if clock.incident_detected {
        tx.commit().await?;
        return Err(crate::state::clock_reconciliation_error());
    }
    let key = ack_key(&mut tx).await?;
    let minter = check_ack_token(&key, &project, &token, clock.now)?;
    if !project_exists(&mut tx, &project).await? {
        return Err(AppError::not_found());
    }
    if !may_mint_ack_link(&mut tx, &project, &minter).await? {
        return Err(link_invalid());
    }
    record_read(&mut tx, &project, clock.now, "ack_link", Some(&minter)).await?;
    tx.commit().await?;
    Ok(ack_html(
        "Recorded",
        "<p>The attention digest is marked read.</p>",
    ))
}

/// Open tasks whose last [`STALL_ATTEMPTS`] attempts all ended without a
/// submission, with the stalled attempt count.
async fn stalled_tasks(c: &mut SqliteConnection, project: &str) -> Result<Vec<Value>, AppError> {
    let rows = sqlx::query(
        "SELECT t.id,t.title,t.priority FROM tasks t WHERE t.project_id=? AND t.lifecycle='open' \
         AND t.budget_exempt IS NULL AND t.archived_at IS NULL AND (SELECT count(*) FROM (SELECT state FROM attempts a \
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
         AND t.budget_exempt IS NULL AND t.blocked_reason IS NULL \
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
    let progress: Option<i64> = sqlx::query_scalar(concat!(
        "SELECT max(at) FROM (SELECT max(created_at) AS at FROM attempts WHERE project_id=?1 AND task_id NOT IN ",
        canary_tasks!(),
        " UNION ALL SELECT max(c.created_at) FROM checkpoints c JOIN attempts a ON a.id=c.attempt_id WHERE c.project_id=?1 AND a.task_id NOT IN ",
        canary_tasks!(),
        " UNION ALL SELECT max(created_at) FROM submissions WHERE project_id=?1 AND task_id NOT IN ",
        canary_tasks!(),
        " UNION ALL SELECT max(r.created_at) FROM review_decisions r JOIN submissions s ON s.id=r.submission_id WHERE s.project_id=?1 AND s.task_id NOT IN ",
        canary_tasks!(),
        " UNION ALL SELECT max(i.created_at) FROM integration_authorizations i JOIN submissions s ON s.id=i.submission_id WHERE s.project_id=?1 AND s.task_id NOT IN ",
        canary_tasks!(),
        " UNION ALL SELECT max(i.created_at) FROM integration_results i JOIN submissions s ON s.id=i.submission_id WHERE s.project_id=?1 AND s.task_id NOT IN ",
        canary_tasks!(),
        ")",
    ))
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

/// Agent-created tasks of the project that the weekly admission budget
/// holds as planned, oldest first.
async fn held_agent_tasks(c: &mut SqliteConnection, project: &str) -> Result<Vec<Value>, AppError> {
    let rows = sqlx::query(
        "SELECT id,title,priority,budget_held_at FROM tasks WHERE project_id=? \
         AND budget_held_at IS NOT NULL AND lifecycle='planned' AND archived_at IS NULL \
         AND deleted_at IS NULL ORDER BY budget_held_at,id LIMIT ?",
    )
    .bind(project)
    .bind(DIGEST_LIMIT)
    .fetch_all(&mut *c)
    .await?;
    Ok(rows
        .iter()
        .map(|row| {
            json!({"task_id": row.get::<String, _>("id"),
                   "title": row.get::<String, _>("title"),
                   "priority": row.get::<i64, _>("priority"),
                   "held_at": timestamp(row.get("budget_held_at"))})
        })
        .collect())
}

/// `GET /api/v1/projects/{project}/digest?hours=N`: what the attention budget
/// did in the last `hours` (default 24) and what still needs a human.
async fn digest(
    State(state): State<AppState>,
    auth: Auth,
    Path(project): Path<String>,
    Query(query): Query<DigestQuery>,
) -> Reply {
    let hours = query.hours.unwrap_or(DEFAULT_DIGEST_HOURS);
    if !(1..=MAX_DIGEST_HOURS).contains(&hours) {
        return Err(AppError::bad_request("hours must be between 1 and 336."));
    }
    let mut c = state.pool.acquire().await?;
    if !project_exists(&mut c, &project).await? {
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
    let held = held_agent_tasks(&mut c, &project).await?;
    let mut items = stalled_tasks(&mut c, &project).await?;
    let stalled = items.len();
    let queue = stalled_queue(&mut c, &project, now, &state.config).await?;
    let stalled_queue = usize::from(queue.is_some());
    items.extend(queue);
    items.extend(
        crate::integrator_reports::human_queue_items_without_canary(&mut c, &project).await?,
    );
    let last_read: Option<i64> =
        sqlx::query_scalar("SELECT last_read_at FROM digest_reads WHERE project_id=?")
            .bind(&project)
            .fetch_optional(&mut *c)
            .await?;
    let sender = digest_sender(&mut c, &project).await?;
    // The link is a bearer capability to mark the digest read, which silences
    // the neglect page, so only a human or the designated digest sender that
    // asks for it receives one. Any other agent gets the digest without it.
    let may_mint = auth.actor.kind == "human" || sender.as_deref() == Some(&auth.actor.id);
    let ack = if query.ack_link.unwrap_or(false) && may_mint {
        let expires_at = now + ACK_LINK_TTL_MS;
        let token = ack_token(
            &ack_key(&mut c).await?,
            &project,
            expires_at,
            &auth.actor.id,
        );
        Some(json!({
            "url": format!(
                "{}/api/v1/projects/{project}/digest/ack?token={token}",
                state.config.public_origin
            ),
            "expires_at": timestamp(expires_at),
        }))
    } else {
        None
    };
    Ok(response(json!({
        "project_id": project,
        "last_read_at": last_read.map(timestamp),
        "ack_link": ack,
        "digest_sender": sender,
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
        "held_agent_tasks": held,
        "agent_task_weekly_budget": {
            "limit": state.config.agent_task_weekly_budget,
            "admitted_this_week": crate::admission::admitted_in_week(
                &mut c, &project, crate::admission::week_start(now)).await?,
            "week_start": timestamp(crate::admission::week_start(now)),
        },
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
    fn hmac_matches_the_rfc_4231_vector_and_binds_project_and_expiry() {
        assert_eq!(
            hex::encode(hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        let key = [7u8; 32];
        let token = ack_token(&key, "p", 1_000, "m");
        assert_eq!(check_ack_token(&key, "p", &token, 999).unwrap(), "m");
        let (head, signature) = token.rsplit_once('.').unwrap();
        let swapped = format!("{}.{signature}", head.replace(".m", ".other"));
        assert!(check_ack_token(&key, "p", &swapped, 999).is_err());
        assert!(check_ack_token(&key, "p", &token, 1_000).is_err());
        assert!(check_ack_token(&key, "q", &token, 999).is_err());
        assert!(check_ack_token(&[8u8; 32], "p", &token, 999).is_err());
    }

    #[test]
    fn paths_are_normalized_or_refused() {
        assert_eq!(normalize_path("crates/server/").unwrap(), "crates/server");
        for bad in ["", "/etc", "a/../b", "a//b", "./a", "a\\b", "/"] {
            assert!(normalize_path(bad).is_err(), "{bad}");
        }
    }
}
