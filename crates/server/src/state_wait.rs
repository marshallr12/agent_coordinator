//! Bounded long-poll reads for task and workflow-activity state changes.
use crate::{auth::Auth, coordination, error::AppError, response, state::AppState, workflow};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::SqliteConnection;
use std::time::Duration;

type Reply = Result<Json<Value>, AppError>;
const MAX_WAIT_SECONDS: u64 = 30;
const POLL_INTERVAL: Duration = Duration::from_millis(500);

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/v1/projects/{project}/state-wait",
        get(wait_for_change),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitQuery {
    target_kind: String,
    target_id: String,
    after_state_token: String,
    timeout_seconds: Option<u64>,
}

async fn wait_for_change(
    State(state): State<AppState>,
    _auth: Auth,
    Path(project): Path<String>,
    Query(query): Query<WaitQuery>,
) -> Reply {
    if !matches!(query.target_kind.as_str(), "task" | "activity") {
        return Err(AppError::bad_request(
            "target_kind must be task or activity.",
        ));
    }
    if query.target_id.is_empty() || query.target_id.len() > 128 {
        return Err(AppError::bad_request("target_id must contain 1–128 bytes."));
    }
    if query.after_state_token.is_empty() || query.after_state_token.len() > 64 {
        return Err(AppError::bad_request(
            "after_state_token must contain 1–64 bytes.",
        ));
    }
    let timeout_seconds = query.timeout_seconds.unwrap_or(15);
    if !(1..=MAX_WAIT_SECONDS).contains(&timeout_seconds) {
        return Err(AppError::bad_request(
            "timeout_seconds must be between 1 and 30.",
        ));
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_seconds);
    loop {
        let mut connection = state.pool.acquire().await?;
        let snapshot = target_snapshot(
            &mut connection,
            &project,
            &query.target_kind,
            &query.target_id,
            state.now(),
        )
        .await?;
        let token = state_token(&snapshot)?;
        let changed = token != query.after_state_token;
        if changed || tokio::time::Instant::now() >= deadline {
            return Ok(response(json!({
                "target_kind": query.target_kind,
                "target_id": query.target_id,
                "changed": changed,
                "timed_out": !changed,
                "state_token": token,
                "state": snapshot
            })));
        }
        drop(connection);
        tokio::time::sleep(
            POLL_INTERVAL.min(deadline.saturating_duration_since(tokio::time::Instant::now())),
        )
        .await;
    }
}

async fn target_snapshot(
    connection: &mut SqliteConnection,
    project: &str,
    kind: &str,
    id: &str,
    now: i64,
) -> Result<Value, AppError> {
    match kind {
        "task" => coordination::task_record_value(connection, project, id, now).await,
        "activity" => workflow::activity_wait_snapshot(connection, project, id, now).await,
        _ => Err(AppError::bad_request(
            "target_kind must be task or activity.",
        )),
    }
}

pub(crate) fn state_token(value: &Value) -> Result<String, AppError> {
    let mut stable = value.clone();
    if let Some(object) = stable.as_object_mut() {
        object.remove("state_token");
    }
    let bytes = serde_json::to_vec(&stable)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}
