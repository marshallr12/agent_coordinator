//! Restore authority invalidation and explicit human reconciliation.

use crate::{
    auth::{Auth, admin, hash_password, secret, timestamp, valid_id},
    error::AppError,
    mutation::Mutation,
    response,
    state::AppState,
};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::HeaderMap,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection};

type Reply = Result<Json<Value>, AppError>;
const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 200;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/admin/restore", get(status))
        .route("/api/v1/admin/restore/inspections", post(inspect_hold))
        .route(
            "/api/v1/admin/restore/old-installation-fenced",
            post(fence_old_installation),
        )
        .route(
            "/api/v1/admin/restore/post-snapshot-gap",
            post(reconcile_post_snapshot_gap),
        )
        .route("/api/v1/admin/restore/finish", post(finish_restore))
}

fn bounded(value: &str, field: &str, max: usize) -> Result<(), AppError> {
    if value.trim() != value
        || value.is_empty()
        || value.len() > max
        || value
            .chars()
            .any(|value| value.is_control() && !matches!(value, '\n' | '\t'))
    {
        return Err(AppError::bad_request(&format!(
            "{field} must contain 1 to {max} bytes without control characters or surrounding whitespace."
        )));
    }
    Ok(())
}

/// Rotate every restored authority verifier before the staged database is made
/// reachable. The current task pointers remain intact so recovery views retain
/// the exact ownership generation that became uncertain.
pub async fn invalidate_restored_state(
    state: &AppState,
    snapshot_id: &str,
    reason: &str,
) -> anyhow::Result<Value> {
    anyhow::ensure!(
        snapshot_id.trim() == snapshot_id
            && !snapshot_id.is_empty()
            && snapshot_id.len() <= 255
            && !snapshot_id.chars().any(char::is_control),
        "snapshot_id must contain 1 to 255 bytes without control characters or surrounding whitespace"
    );
    anyhow::ensure!(
        reason.trim() == reason
            && !reason.is_empty()
            && reason.len() <= 2000
            && !reason.chars().any(char::is_control),
        "reason must contain 1 to 2000 bytes without control characters or surrounding whitespace"
    );
    let disabled_password = hash_password(secret()).await?;
    let authority_epoch = secret();
    let cursor_epoch = secret();
    let restore_id = uuid::Uuid::new_v4().to_string();
    let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    let now = state.now();
    let service =
        sqlx::query("SELECT authority_epoch,cursor_epoch FROM service_state WHERE singleton=1")
            .fetch_one(&mut *tx)
            .await?;
    sqlx::query("INSERT INTO restore_runs(id,snapshot_id,reason,previous_authority_epoch,authority_epoch,previous_cursor_epoch,cursor_epoch,restored_at) VALUES(?,?,?,?,?,?,?,?)")
        .bind(&restore_id)
        .bind(snapshot_id)
        .bind(reason)
        .bind(service.get::<String,_>("authority_epoch"))
        .bind(&authority_epoch)
        .bind(service.get::<String,_>("cursor_epoch"))
        .bind(&cursor_epoch)
        .bind(now)
        .execute(&mut *tx)
        .await?;

    let reservations = sqlx::query("SELECT r.id,r.project_id,r.attempt_id,a.task_id,r.generation,COALESCE(json_group_array(json_object('resource_id',i.resource_id,'key',resources.key,'units',i.units)) FILTER (WHERE i.resource_id IS NOT NULL),'[]') AS items_json FROM reservations r JOIN attempts a ON a.id=r.attempt_id LEFT JOIN reservation_items i ON i.reservation_id=r.id LEFT JOIN resources ON resources.id=i.resource_id WHERE r.state='held' GROUP BY r.id,r.project_id,r.attempt_id,a.task_id,r.generation ORDER BY r.id")
        .fetch_all(&mut *tx)
        .await?;
    for row in reservations {
        let detail = json!({
            "reservation_id":row.get::<String,_>("id"),
            "attempt_id":row.get::<String,_>("attempt_id"),
            "task_id":row.get::<String,_>("task_id"),
            "generation":row.get::<i64,_>("generation"),
            "items":serde_json::from_str::<Value>(&row.get::<String,_>("items_json"))?
        });
        sqlx::query("INSERT INTO restore_requirements(restore_id,kind,target_id,project_id,state_at_restore,detail_json) VALUES(?,'resource_hold',?,?,'held',?)")
            .bind(&restore_id)
            .bind(row.get::<String,_>("id"))
            .bind(row.get::<String,_>("project_id"))
            .bind(serde_json::to_string(&detail)?)
            .execute(&mut *tx)
            .await?;
    }
    let integration_holds = sqlx::query("SELECT h.id,a.project_id,h.activity_id,a.subject_task_id,a.activity_task_id,h.canonical_repository_key,h.target_branch FROM integration_holds h JOIN workflow_activities a ON a.id=h.activity_id WHERE h.state='held' ORDER BY h.id")
        .fetch_all(&mut *tx)
        .await?;
    for row in integration_holds {
        let detail = json!({
            "hold_id":row.get::<String,_>("id"),
            "activity_id":row.get::<String,_>("activity_id"),
            "subject_task_id":row.get::<String,_>("subject_task_id"),
            "activity_task_id":row.get::<String,_>("activity_task_id"),
            "canonical_repository_key":row.get::<String,_>("canonical_repository_key"),
            "target_branch":row.get::<String,_>("target_branch")
        });
        sqlx::query("INSERT INTO restore_requirements(restore_id,kind,target_id,project_id,state_at_restore,detail_json) VALUES(?,'integration_hold',?,?,'held',?)")
            .bind(&restore_id)
            .bind(row.get::<String,_>("id"))
            .bind(row.get::<String,_>("project_id"))
            .bind(serde_json::to_string(&detail)?)
            .execute(&mut *tx)
            .await?;
    }

    sqlx::query("UPDATE credentials SET revoked_at=COALESCE(revoked_at,?)")
        .bind(now)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE browser_sessions SET revoked_at=COALESCE(revoked_at,?)")
        .bind(now)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE agent_sessions SET closed_at=COALESCE(closed_at,?)")
        .bind(now)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE principals SET password_hash=?,disabled_at=COALESCE(disabled_at,?),revision=revision+1 WHERE kind='human'")
        .bind(disabled_password)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE reporters SET expires_at=MIN(expires_at,?),renew_until=MIN(renew_until,?)")
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE attempts SET state='expired',ended_at=COALESCE(ended_at,?),outcome=COALESCE(outcome,'Authority expired by database restore.') WHERE state='active'")
        .bind(now)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE jobs SET state='unknown' WHERE state IN ('registered','running')")
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE artifacts SET reservation_expires_at=MIN(reservation_expires_at,?) WHERE kind='upload' AND state='reserved'")
        .bind(now)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO integration_authorization_history(activity_id,authorization_revision,submission_id,project_policy_revision,workflow_policy_revision,actor_id,summary,created_at,invalidated_at) SELECT activity_id,authorization_revision,submission_id,project_policy_revision,workflow_policy_revision,actor_id,summary,created_at,? FROM integration_authorizations WHERE invalidated_at IS NULL")
        .bind(now)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE integration_authorizations SET invalidated_at=COALESCE(invalidated_at,?)")
        .bind(now)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE service_state SET authority_epoch=?,cursor_epoch=?,coordination_state='restore_reconciliation',restore_id=?,restored_at=? WHERE singleton=1")
        .bind(&authority_epoch)
        .bind(&cursor_epoch)
        .bind(&restore_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    let required_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM restore_requirements WHERE restore_id=?")
            .bind(&restore_id)
            .fetch_one(&mut *tx)
            .await?;
    let unknown_jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE state='unknown'")
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(json!({
        "restore_id":restore_id,
        "snapshot_id":snapshot_id,
        "coordination_state":"restore_reconciliation",
        "restored_at":timestamp(now),
        "required_inspections":required_count,
        "unknown_jobs":unknown_jobs
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusQuery {
    cursor: Option<String>,
    limit: Option<i64>,
}

#[derive(Serialize, Deserialize)]
struct RestoreCursor {
    version: u8,
    restore_id: String,
    kind: String,
    target_id: String,
}

fn encode_cursor(value: &RestoreCursor) -> Result<String, AppError> {
    Ok(hex::encode(serde_json::to_vec(value)?))
}

fn decode_cursor(value: &str) -> Result<RestoreCursor, AppError> {
    if value.is_empty() || value.len() > 2048 || !value.is_ascii() {
        return Err(AppError::bad_request("The restore cursor is invalid."));
    }
    let bytes =
        hex::decode(value).map_err(|_| AppError::bad_request("The restore cursor is invalid."))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| AppError::bad_request("The restore cursor is invalid."))
}

async fn service_value(c: &mut SqliteConnection) -> Result<Value, AppError> {
    let row = sqlx::query("SELECT authority_epoch,cursor_epoch,coordination_state,restore_id,restored_at FROM service_state WHERE singleton=1")
        .fetch_one(&mut *c)
        .await?;
    Ok(json!({
        "authority_epoch":row.get::<String,_>("authority_epoch"),
        "cursor_epoch":row.get::<String,_>("cursor_epoch"),
        "coordination_state":row.get::<String,_>("coordination_state"),
        "restore_id":row.get::<Option<String>,_>("restore_id"),
        "restored_at":row.get::<Option<i64>,_>("restored_at").map(timestamp)
    }))
}

async fn restore_value(c: &mut SqliteConnection, id: &str) -> Result<Value, AppError> {
    let row = sqlx::query("SELECT * FROM restore_runs WHERE id=?")
        .bind(id)
        .fetch_one(&mut *c)
        .await?;
    let required: i64 =
        sqlx::query_scalar("SELECT count(*) FROM restore_requirements WHERE restore_id=?")
            .bind(id)
            .fetch_one(&mut *c)
            .await?;
    let inspected: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM restore_requirements WHERE restore_id=? AND inspected_at IS NOT NULL",
    )
    .bind(id)
    .fetch_one(&mut *c)
    .await?;
    let unknown_jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE state='unknown'")
        .fetch_one(&mut *c)
        .await?;
    let expired_uploads: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM artifacts WHERE kind='upload' AND state='reserved' AND reservation_expires_at IS NOT NULL AND reservation_expires_at<=(SELECT restored_at FROM restore_runs WHERE id=?)",
    )
    .bind(id)
    .fetch_one(&mut *c)
    .await?;
    Ok(json!({
        "id":id,
        "restore_id":id,
        "snapshot_id":row.get::<String,_>("snapshot_id"),
        "reason":row.get::<String,_>("reason"),
        "restored_at":timestamp(row.get("restored_at")),
        "required_inspections":required,
        "inspected":inspected,
        "remaining_inspections":required-inspected,
        "old_installation_fenced":row.get::<Option<i64>,_>("old_installation_fenced_at").is_some(),
        "old_installation_fenced_at":row.get::<Option<i64>,_>("old_installation_fenced_at").map(timestamp),
        "old_installation_fence_evidence":row.get::<Option<String>,_>("old_installation_fence_evidence"),
        "post_snapshot_gap_reconciled":row.get::<Option<i64>,_>("post_snapshot_gap_reconciled_at").is_some(),
        "post_snapshot_gap_reconciled_at":row.get::<Option<i64>,_>("post_snapshot_gap_reconciled_at").map(timestamp),
        "post_snapshot_gap_evidence":row.get::<Option<String>,_>("post_snapshot_gap_evidence"),
        "completed_at":row.get::<Option<i64>,_>("completed_at").map(timestamp),
        "unknown_jobs":unknown_jobs,
        "expired_uploads":expired_uploads
    }))
}

async fn status(
    State(state): State<AppState>,
    auth: Auth,
    Query(query): Query<StatusQuery>,
) -> Reply {
    admin(&auth.actor)?;
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(AppError::bad_request("limit must be between 1 and 200."));
    }
    let mut tx = state.pool.begin().await?;
    let service = service_value(&mut tx).await?;
    let Some(restore_id) = service["restore_id"].as_str() else {
        return Ok(response(json!({
            "service_state":service,
            "restore":Value::Null,
            "items":[],
            "next_cursor":Value::Null
        })));
    };
    let cursor = query.cursor.as_deref().map(decode_cursor).transpose()?;
    if cursor.as_ref().is_some_and(|cursor| {
        cursor.version != 1
            || cursor.restore_id != restore_id
            || !matches!(cursor.kind.as_str(), "resource_hold" | "integration_hold")
            || !valid_id(&cursor.target_id)
    }) {
        return Err(AppError::conflict(
            "restore_cursor_mismatch",
            "The restore cursor belongs to another reconciliation inventory.",
        ));
    }
    let (kind, target) = cursor
        .as_ref()
        .map(|cursor| (cursor.kind.as_str(), cursor.target_id.as_str()))
        .unwrap_or(("", ""));
    let rows = sqlx::query("SELECT kind,target_id,project_id,state_at_restore,detail_json,inspected_at,inspected_by,disposition,evidence FROM restore_requirements WHERE restore_id=? AND (kind>? OR (kind=? AND target_id>?)) ORDER BY kind,target_id LIMIT ?")
        .bind(restore_id)
        .bind(kind)
        .bind(kind)
        .bind(target)
        .bind(limit + 1)
        .fetch_all(&mut *tx)
        .await?;
    let more = rows.len() > limit as usize;
    let rows = &rows[..rows.len().min(limit as usize)];
    let items = rows
        .iter()
        .map(|row| -> Result<Value, AppError> {
            Ok(json!({
                "kind":row.get::<String,_>("kind"),
                "target_id":row.get::<String,_>("target_id"),
                "project_id":row.get::<String,_>("project_id"),
                "state_at_restore":row.get::<String,_>("state_at_restore"),
                "detail":serde_json::from_str::<Value>(&row.get::<String,_>("detail_json"))?,
                "inspected":row.get::<Option<i64>,_>("inspected_at").is_some(),
                "inspected_at":row.get::<Option<i64>,_>("inspected_at").map(timestamp),
                "inspected_by":row.get::<Option<String>,_>("inspected_by"),
                "disposition":row.get::<Option<String>,_>("disposition"),
                "evidence":row.get::<Option<String>,_>("evidence")
            }))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let next_cursor = if more {
        rows.last()
            .map(|row| {
                encode_cursor(&RestoreCursor {
                    version: 1,
                    restore_id: restore_id.to_owned(),
                    kind: row.get("kind"),
                    target_id: row.get("target_id"),
                })
            })
            .transpose()?
    } else {
        None
    };
    let restore = restore_value(&mut tx, restore_id).await?;
    Ok(response(json!({
        "service_state":service,
        "restore":restore,
        "items":items,
        "next_cursor":next_cursor
    })))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InspectionInput {
    restore_id: String,
    kind: String,
    target_id: String,
    disposition: String,
    evidence: String,
}

async fn current_restore(c: &mut SqliteConnection, id: &str) -> Result<(), AppError> {
    let current: Option<String> = sqlx::query_scalar("SELECT restore_id FROM service_state WHERE singleton=1 AND coordination_state='restore_reconciliation'")
        .fetch_optional(&mut *c)
        .await?
        .flatten();
    if current.as_deref() != Some(id) {
        return Err(AppError::conflict(
            "restore_changed",
            "Reload the active restore reconciliation before recording evidence.",
        ));
    }
    Ok(())
}

async fn inspect_hold(
    State(state): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    Json(input): Json<InspectionInput>,
) -> Reply {
    if !valid_id(&input.restore_id)
        || !valid_id(&input.target_id)
        || !matches!(input.kind.as_str(), "resource_hold" | "integration_hold")
        || !matches!(input.disposition.as_str(), "held" | "released" | "unknown")
    {
        return Err(AppError::bad_request(
            "Use the active restore, a listed hold identity, and held, released, or unknown disposition.",
        ));
    }
    bounded(&input.evidence, "evidence", 32_768)?;
    let mut mutation = Mutation::begin(
        &state,
        &auth,
        &headers,
        "POST /api/v1/admin/restore/inspections",
        &input,
    )
    .await?;
    admin(&mutation.actor)?;
    if let Some(value) = mutation.replay {
        return Ok(response(value));
    }
    current_restore(&mut mutation.tx, &input.restore_id).await?;
    let changed = sqlx::query("UPDATE restore_requirements SET inspected_at=?,inspected_by=?,disposition=?,evidence=? WHERE restore_id=? AND kind=? AND target_id=? AND inspected_at IS NULL")
        .bind(mutation.now)
        .bind(&mutation.actor.id)
        .bind(&input.disposition)
        .bind(&input.evidence)
        .bind(&input.restore_id)
        .bind(&input.kind)
        .bind(&input.target_id)
        .execute(&mut *mutation.tx)
        .await?
        .rows_affected();
    if changed == 0 {
        let exists: i64 = sqlx::query_scalar("SELECT count(*) FROM restore_requirements WHERE restore_id=? AND kind=? AND target_id=?")
            .bind(&input.restore_id).bind(&input.kind).bind(&input.target_id).fetch_one(&mut *mutation.tx).await?;
        return Err(if exists == 0 {
            AppError::not_found()
        } else {
            AppError::conflict(
                "restore_requirement_inspected",
                "This restored hold already has immutable inspection evidence.",
            )
        });
    }
    sqlx::query("INSERT INTO restore_reconciliation_events(restore_id,actor_id,kind,target_kind,target_id,disposition,evidence,created_at) VALUES(?,?,'hold_inspected',?,?,?,?,?)")
        .bind(&input.restore_id)
        .bind(&mutation.actor.id)
        .bind(&input.kind)
        .bind(&input.target_id)
        .bind(&input.disposition)
        .bind(&input.evidence)
        .bind(mutation.now)
        .execute(&mut *mutation.tx)
        .await?;
    let data = json!({
        "restore_id":input.restore_id,
        "kind":input.kind,
        "target_id":input.target_id,
        "inspected":true,
        "disposition":input.disposition,
        "evidence":input.evidence,
        "inspected_at":timestamp(mutation.now),
        "inspected_by":mutation.actor.id
    });
    Ok(response(
        mutation
            .finish(data, None, "restore.hold_inspected", &input.restore_id)
            .await?,
    ))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AttestationInput {
    restore_id: String,
    evidence: String,
}

async fn attest(
    state: &AppState,
    auth: &Auth,
    headers: &HeaderMap,
    input: &AttestationInput,
    operation: &str,
    event_kind: &str,
) -> Result<Value, AppError> {
    if !valid_id(&input.restore_id) {
        return Err(AppError::bad_request("Use the active restore identity."));
    }
    bounded(&input.evidence, "evidence", 32_768)?;
    let mut mutation = Mutation::begin(state, auth, headers, operation, input).await?;
    admin(&mutation.actor)?;
    if let Some(value) = mutation.replay {
        return Ok(value);
    }
    current_restore(&mut mutation.tx, &input.restore_id).await?;
    let query = match event_kind {
        "old_installation_fenced" => sqlx::query(
            "UPDATE restore_runs SET old_installation_fenced_at=?,old_installation_fence_evidence=? WHERE id=? AND old_installation_fenced_at IS NULL",
        ),
        "post_snapshot_gap_reconciled" => sqlx::query(
            "UPDATE restore_runs SET post_snapshot_gap_reconciled_at=?,post_snapshot_gap_evidence=? WHERE id=? AND post_snapshot_gap_reconciled_at IS NULL",
        ),
        _ => return Err(AppError::internal()),
    };
    if query
        .bind(mutation.now)
        .bind(&input.evidence)
        .bind(&input.restore_id)
        .execute(&mut *mutation.tx)
        .await?
        .rows_affected()
        == 0
    {
        return Err(AppError::conflict(
            "restore_attestation_recorded",
            "This restore attestation already has immutable evidence.",
        ));
    }
    sqlx::query("INSERT INTO restore_reconciliation_events(restore_id,actor_id,kind,evidence,created_at) VALUES(?,?,?,?,?)")
        .bind(&input.restore_id)
        .bind(&mutation.actor.id)
        .bind(event_kind)
        .bind(&input.evidence)
        .bind(mutation.now)
        .execute(&mut *mutation.tx)
        .await?;
    let recorded_at = timestamp(mutation.now);
    mutation
        .finish(
            json!({"restore_id":input.restore_id,"recorded":true,"evidence":input.evidence,"recorded_at":recorded_at}),
            None,
            event_kind,
            &input.restore_id,
        )
        .await
}

async fn fence_old_installation(
    State(state): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    Json(input): Json<AttestationInput>,
) -> Reply {
    Ok(response(
        attest(
            &state,
            &auth,
            &headers,
            &input,
            "POST /api/v1/admin/restore/old-installation-fenced",
            "old_installation_fenced",
        )
        .await?,
    ))
}

async fn reconcile_post_snapshot_gap(
    State(state): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    Json(input): Json<AttestationInput>,
) -> Reply {
    Ok(response(
        attest(
            &state,
            &auth,
            &headers,
            &input,
            "POST /api/v1/admin/restore/post-snapshot-gap",
            "post_snapshot_gap_reconciled",
        )
        .await?,
    ))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FinishInput {
    restore_id: String,
    reason: String,
}

async fn finish_restore(
    State(state): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    Json(input): Json<FinishInput>,
) -> Reply {
    if !valid_id(&input.restore_id) {
        return Err(AppError::bad_request("Use the active restore identity."));
    }
    bounded(&input.reason, "reason", 2_000)?;
    let mut mutation = Mutation::begin(
        &state,
        &auth,
        &headers,
        "POST /api/v1/admin/restore/finish",
        &input,
    )
    .await?;
    admin(&mutation.actor)?;
    if let Some(value) = mutation.replay {
        return Ok(response(value));
    }
    current_restore(&mut mutation.tx, &input.restore_id).await?;
    let row = sqlx::query("SELECT old_installation_fenced_at,post_snapshot_gap_reconciled_at FROM restore_runs WHERE id=?")
        .bind(&input.restore_id)
        .fetch_one(&mut *mutation.tx)
        .await?;
    let missing: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM restore_requirements WHERE restore_id=? AND inspected_at IS NULL",
    )
    .bind(&input.restore_id)
    .fetch_one(&mut *mutation.tx)
    .await?;
    if row
        .get::<Option<i64>, _>("old_installation_fenced_at")
        .is_none()
        || row
            .get::<Option<i64>, _>("post_snapshot_gap_reconciled_at")
            .is_none()
        || missing != 0
    {
        return Err(AppError::conflict(
            "restore_reconciliation_incomplete",
            "Fence the old installation, reconcile the post-snapshot gap, and inspect every preserved hold before resuming coordination.",
        )
        .with_details(json!({"remaining_inspections":missing})));
    }
    sqlx::query("UPDATE restore_runs SET completed_at=?,completion_reason=? WHERE id=? AND completed_at IS NULL")
        .bind(mutation.now)
        .bind(&input.reason)
        .bind(&input.restore_id)
        .execute(&mut *mutation.tx)
        .await?;
    sqlx::query(
        "UPDATE service_state SET coordination_state='ready' WHERE singleton=1 AND restore_id=?",
    )
    .bind(&input.restore_id)
    .execute(&mut *mutation.tx)
    .await?;
    sqlx::query("INSERT INTO restore_reconciliation_events(restore_id,actor_id,kind,evidence,created_at) VALUES(?,?,'restore_completed',?,?)")
        .bind(&input.restore_id)
        .bind(&mutation.actor.id)
        .bind(&input.reason)
        .bind(mutation.now)
        .execute(&mut *mutation.tx)
        .await?;
    let data = json!({
        "restore_id":input.restore_id,
        "coordination_state":"ready",
        "completed_at":timestamp(mutation.now),
        "reason":input.reason,
        "holds_released":false
    });
    Ok(response(
        mutation
            .finish(data, None, "restore.completed", &input.restore_id)
            .await?,
    ))
}
