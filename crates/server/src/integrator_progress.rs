//! What the deterministic integrator is doing with a subject task right now,
//! for the task list and details.
//!
//! The integrator posts a snapshot of the required check runs that are still
//! queued or in progress on a result's R (`POST …/integrator/check-progress`);
//! completed runs are receipts and push authority is the publication window.
//! [`integration_processing`] reads only the current submission's newest
//! result for its exact candidate, so evidence for an older, superseded or
//! mismatched revision never makes the task look active. Activity also
//! requires a fresh snapshot and a fresh integrator heartbeat: an old report
//! alone is shown as unknown, never as running. The result is a read-only
//! display; it creates no owner, lease or claim and changes no policy.
use crate::{
    auth::Auth,
    error::AppError,
    integrator::require_integrator_project,
    mutation::Mutation,
    response,
    state::AppState,
    workflow::{bounded, payload, revision},
};
use axum::{
    Json, Router,
    extract::{Path, State, rejection::JsonRejection},
    http::HeaderMap,
    routing::post,
};
use coordinator_core::timestamp;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection};

type Reply = Result<Json<Value>, AppError>;

/// Milliseconds after which a snapshot or heartbeat no longer shows activity:
/// six of the integrator's 30-second polls.
const FRESH_MS: i64 = 180_000;
/// The most unfinished runs one snapshot lists.
const MAX_RUNS: usize = 100;
/// The most check runs a task's processing lists.
const LISTED_RUNS: usize = 25;
/// Conclusions that pass a required check.
const PASSING: &[&str] = &["success", "skipped", "neutral"];

/// The progress route.
pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/v1/projects/{p}/integrator/check-progress",
        post(record_progress),
    )
}

/// One required check run that has not completed.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProgressRun {
    check_name: String,
    status: String,
    #[serde(default)]
    run_id: Option<i64>,
    #[serde(default)]
    run_attempt: Option<i64>,
    /// RFC 3339 time the run started; absent while it is queued.
    #[serde(default)]
    started_at: Option<String>,
}

/// The integrator's latest poll of the unfinished required checks on R.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProgressInput {
    result_id: String,
    head_sha: String,
    runs: Vec<ProgressRun>,
}

impl ProgressInput {
    /// Checks every identity and bound before any state is read.
    fn validate(&self) -> Result<(), AppError> {
        bounded(&self.result_id, "result_id", 100, true)?;
        revision(&self.head_sha, "head_sha")?;
        if self.runs.len() > MAX_RUNS {
            return Err(AppError::bad_request(
                "runs lists at most 100 unfinished check runs.",
            ));
        }
        for run in &self.runs {
            bounded(&run.check_name, "check_name", 200, true)?;
            if !matches!(run.status.as_str(), "queued" | "in_progress") {
                return Err(AppError::bad_request(
                    "status must be queued or in_progress; completed runs are receipts.",
                ));
            }
            if run.run_id.is_some_and(|id| id <= 0) || run.run_attempt.is_some_and(|id| id <= 0) {
                return Err(AppError::bad_request(
                    "run_id and run_attempt must be positive.",
                ));
            }
            if let Some(started) = &run.started_at {
                bounded(started, "started_at", 64, true)?;
                chrono::DateTime::parse_from_rfc3339(started).map_err(|_| {
                    AppError::bad_request("started_at must be an RFC 3339 timestamp.")
                })?;
            }
        }
        Ok(())
    }
}

/// `POST …/integrator/check-progress`: replaces the result's snapshot of
/// unfinished required check runs.
async fn record_progress(
    State(s): State<AppState>,
    auth: Auth,
    Path(p): Path<String>,
    headers: HeaderMap,
    body: Result<Json<ProgressInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    input.validate()?;
    let op = format!("POST /api/v1/projects/{p}/integrator/check-progress");
    let mut m = Mutation::begin(&s, &auth, &headers, &op, &input).await?;
    if let Some(v) = m.replay.take() {
        return Ok(response(v));
    }
    require_integrator_project(&mut m.tx, &p).await?;
    let row =
        sqlx::query("SELECT r,roster_json FROM integrator_results WHERE id=? AND project_id=?")
            .bind(&input.result_id)
            .bind(&p)
            .fetch_optional(&mut *m.tx)
            .await?
            .ok_or_else(AppError::not_found)?;
    if row.get::<String, _>("r") != input.head_sha {
        return Err(AppError::conflict(
            "progress_head_mismatch",
            "head_sha is not the result revision R; progress binds to R only.",
        ));
    }
    let roster = roster_checks(&serde_json::from_str(row.get("roster_json"))?);
    if let Some(run) = input
        .runs
        .iter()
        .find(|run| !roster.iter().any(|check| check.name == run.check_name))
    {
        return Err(AppError::conflict(
            "progress_check_not_required",
            "A listed check is not in the result's roster.",
        )
        .with_details(json!({"check_name": run.check_name})));
    }
    sqlx::query("INSERT INTO integrator_check_progress(result_id,head_sha,runs_json,observed_at) VALUES(?,?,?,?) ON CONFLICT(result_id) DO UPDATE SET head_sha=excluded.head_sha,runs_json=excluded.runs_json,observed_at=excluded.observed_at")
        .bind(&input.result_id)
        .bind(&input.head_sha)
        .bind(serde_json::to_string(&input.runs)?)
        .bind(m.now)
        .execute(&mut *m.tx)
        .await?;
    let value = json!({"result_id": input.result_id, "head_sha": input.head_sha,
        "runs": input.runs.len(), "observed_at": timestamp(m.now)});
    let record = input.result_id.clone();
    Ok(response(
        m.finish(
            value,
            Some(&p),
            "integrator.check_progress_recorded",
            &record,
        )
        .await?,
    ))
}

/// One required check named by a result's roster.
struct RosterEntry {
    name: String,
    blob: String,
}

/// The roster's checks, ignoring any entry that does not name a check.
fn roster_checks(roster: &Value) -> Vec<RosterEntry> {
    roster["required_checks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|check| {
            Some(RosterEntry {
                name: check["check_name"].as_str()?.to_owned(),
                blob: check["workflow_blob"].as_str()?.to_owned(),
            })
        })
        .collect()
}

/// A required check's standing on R.
#[derive(Default)]
struct CheckView {
    state: &'static str,
    run_id: Option<i64>,
    run_attempt: Option<i64>,
    conclusion: Option<String>,
    started_at: Option<String>,
}

/// The deciding receipt of `check` on the result: the latest attempt of the
/// latest run under the roster's workflow blob.
async fn deciding(
    c: &mut SqliteConnection,
    result: &str,
    check: &RosterEntry,
) -> Result<Option<(i64, i64, String)>, AppError> {
    Ok(sqlx::query_as("SELECT run_id,run_attempt,conclusion FROM integrator_receipts WHERE result_id=? AND check_name=? AND workflow_blob=? ORDER BY run_id DESC,run_attempt DESC LIMIT 1")
        .bind(result).bind(&check.name).bind(&check.blob)
        .fetch_optional(&mut *c).await?)
}

/// Whether the unfinished run supersedes the deciding receipt: a rerun has a
/// later run or attempt; a run whose identity is not yet known only counts
/// when nothing has completed.
fn newer(run: &ProgressRun, deciding: Option<&(i64, i64, String)>) -> bool {
    match (deciding, run.run_id) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some((id, attempt, _)), Some(run_id)) => {
            (run_id, run.run_attempt.unwrap_or(0)) > (*id, *attempt)
        }
    }
}

/// Standing of every roster check from receipts and the snapshot's runs.
async fn check_views(
    c: &mut SqliteConnection,
    result: &str,
    roster: &[RosterEntry],
    runs: &[ProgressRun],
) -> Result<Vec<(String, CheckView)>, AppError> {
    let mut views = Vec::new();
    for check in roster {
        let done = deciding(c, result, check).await?;
        let unfinished = runs
            .iter()
            .filter(|run| run.check_name == check.name && newer(run, done.as_ref()))
            .max_by_key(|run| (run.run_id, run.run_attempt));
        let view = match (unfinished, done) {
            (Some(run), _) => CheckView {
                state: if run.status == "in_progress" {
                    "running"
                } else {
                    "queued"
                },
                run_id: run.run_id,
                run_attempt: run.run_attempt,
                conclusion: None,
                started_at: run.started_at.clone(),
            },
            (None, Some((run_id, run_attempt, conclusion))) => CheckView {
                state: if PASSING.contains(&conclusion.as_str()) {
                    "passed"
                } else {
                    "failed"
                },
                run_id: Some(run_id),
                run_attempt: Some(run_attempt),
                conclusion: Some(conclusion),
                started_at: None,
            },
            (None, None) => CheckView {
                state: "pending",
                ..CheckView::default()
            },
        };
        views.push((check.name.clone(), view));
    }
    Ok(views)
}

/// The stage the checks' standing shows; `fresh` says whether the snapshot
/// and heartbeat still support claiming activity.
fn verification_stage(views: &[(String, CheckView)], fresh: bool) -> (&'static str, bool) {
    let count = |state: &str| views.iter().filter(|(_, view)| view.state == state).count();
    if count("running") + count("queued") > 0 {
        return match (fresh, count("running") > 0) {
            (false, _) => ("verification_unknown", false),
            (true, true) => ("verifying", true),
            (true, false) => ("verification_queued", false),
        };
    }
    if count("failed") > 0 {
        ("checks_failed", false)
    } else if !views.is_empty() && count("passed") == views.len() {
        ("checks_passed", false)
    } else {
        ("awaiting_checks", false)
    }
}

/// Counts and a bounded list of the checks' standing.
fn checks_value(views: &[(String, CheckView)]) -> (Value, Value) {
    let count = |state: &str| views.iter().filter(|(_, view)| view.state == state).count();
    let counts = json!({"total": views.len(), "passed": count("passed"), "failed": count("failed"),
        "running": count("running"), "queued": count("queued"), "pending": count("pending")});
    let listed: Vec<Value> = views
        .iter()
        .take(LISTED_RUNS)
        .map(|(name, view)| {
            json!({"check_name": name, "state": view.state, "run_id": view.run_id,
                "run_attempt": view.run_attempt, "conclusion": view.conclusion,
                "started_at": view.started_at})
        })
        .collect();
    (counts, json!(listed))
}

/// The integrator's activity on the task's current candidate, or `None` when
/// the project integrates through agents, the submission is not the subject's
/// current code candidate, or no result was pinned for its exact candidate
/// revision.
pub(crate) async fn integration_processing(
    c: &mut SqliteConnection,
    project: &str,
    task: &str,
    now: i64,
) -> Result<Option<Value>, AppError> {
    let owner: Option<(String, Option<i64>)> =
        sqlx::query_as("SELECT integration_owner,integrator_last_seen FROM projects WHERE id=?")
            .bind(project)
            .fetch_optional(&mut *c)
            .await?;
    let Some((_, last_seen)) = owner.filter(|(owner, _)| owner == "integrator") else {
        return Ok(None);
    };
    let submission: Option<(String, String)> = sqlx::query_as("SELECT s.id,s.candidate_revision FROM workflow_subjects ws JOIN submissions s ON s.id=ws.current_submission_id WHERE ws.project_id=? AND ws.task_id=? AND ws.phase='integration' AND s.kind='code' AND s.superseded_at IS NULL")
        .bind(project).bind(task).fetch_optional(&mut *c).await?;
    let Some((submission, candidate)) = submission else {
        return Ok(None);
    };
    let result = sqlx::query("SELECT id,r,roster_json,created_at,authority_expires_at FROM integrator_results WHERE project_id=? AND submission_id=? AND c=? ORDER BY authority_expires_at IS NOT NULL DESC,created_at DESC,id DESC LIMIT 1")
        .bind(project).bind(&submission).bind(&candidate).fetch_optional(&mut *c).await?;
    let Some(result) = result else {
        return Ok(None);
    };
    let (id, r): (String, String) = (result.get("id"), result.get("r"));
    let snapshot: Option<(String, String, i64)> = sqlx::query_as(
        "SELECT head_sha,runs_json,observed_at FROM integrator_check_progress WHERE result_id=?",
    )
    .bind(&id)
    .fetch_optional(&mut *c)
    .await?;
    // A snapshot bound to another revision is not evidence about this R.
    let snapshot = snapshot.filter(|(head, _, _)| *head == r);
    let observed_at = snapshot.as_ref().map(|(_, _, at)| *at);
    let runs: Vec<ProgressRun> = match &snapshot {
        Some((_, json, _)) => serde_json::from_str(json)?,
        None => Vec::new(),
    };
    let roster = roster_checks(&serde_json::from_str(result.get("roster_json"))?);
    let views = check_views(c, &id, &roster, &runs).await?;
    let recent = |at: Option<i64>| at.is_some_and(|at| now - at <= FRESH_MS);
    let fresh = recent(observed_at) && recent(last_seen);
    let publishing = result.get::<Option<i64>, _>("authority_expires_at");
    let (stage, active) = match publishing {
        Some(expires) if expires > now => ("publishing", true),
        Some(_) => ("publication_unconfirmed", false),
        None => verification_stage(&views, fresh),
    };
    let (checks, check_runs) = checks_value(&views);
    Ok(Some(json!({
        "kind": "integration", "stage": stage, "active": active, "holders": [],
        "submission_id": submission, "candidate_revision": candidate,
        "result_id": id, "result_revision": r,
        "result_created_at": timestamp(result.get("created_at")),
        "observed_at": observed_at.map(timestamp),
        "authority_expires_at": publishing.map(timestamp),
        "checks": checks, "check_runs": check_runs,
    })))
}
