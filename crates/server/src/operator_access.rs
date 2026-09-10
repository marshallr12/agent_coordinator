//! Human account, browser-session, and agent-credential lifecycle operations.
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row;

use crate::{
    auth::{
        Auth, admin, clear_browser_cookie, digest, hash_password_bounded, secret, timestamp,
        valid_id, validate_name, verify_password,
    },
    error::AppError,
    mutation::Mutation,
    response,
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/admin/operators",
            get(list_operators).post(create_operator),
        )
        .route("/api/v1/admin/operators/{id}", get(get_operator))
        .route(
            "/api/v1/admin/operators/{id}/access",
            post(update_operator_access),
        )
        .route("/api/v1/auth/account", get(get_own_account))
        .route("/api/v1/auth/password", post(change_password))
        .route("/api/v1/browser-sessions", get(list_browser_sessions))
        .route(
            "/api/v1/browser-sessions/{id}/revoke",
            post(revoke_browser_session),
        )
        .route(
            "/api/v1/admin/credentials/{id}/rotate",
            post(rotate_agent_credential),
        )
        .route(
            "/api/v1/admin/agents/{id}/credentials",
            post(issue_existing_agent_credential),
        )
}

fn validate_password(password: &str) -> Result<(), AppError> {
    if !(12..=1024).contains(&password.len()) {
        return Err(AppError::bad_request(
            "The password must contain 12 to 1024 bytes.",
        ));
    }
    Ok(())
}

fn validate_human_role(role: &str) -> Result<(), AppError> {
    if !matches!(role, "admin" | "operator") {
        return Err(AppError::bad_request(
            "role must be admin or operator for a human account.",
        ));
    }
    Ok(())
}

fn operator_value(row: &sqlx::sqlite::SqliteRow) -> Value {
    json!({
        "id":row.get::<String,_>("id"),
        "name":row.get::<String,_>("name"),
        "kind":"human",
        "role":row.get::<String,_>("role"),
        "enabled":row.get::<Option<i64>,_>("disabled_at").is_none(),
        "revision":row.get::<i64,_>("revision"),
        "created_at":timestamp(row.get("created_at")),
        "disabled_at":row.get::<Option<i64>,_>("disabled_at").map(timestamp),
    })
}

async fn operator_row<'e, E>(executor: E, id: &str) -> Result<sqlx::sqlite::SqliteRow, AppError>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    sqlx::query(
        "SELECT id,name,role,disabled_at,created_at,revision FROM principals WHERE id=? AND kind='human'",
    )
    .bind(id)
    .fetch_optional(executor)
    .await?
    .ok_or_else(AppError::not_found)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    cursor: Option<String>,
}

async fn list_operators(
    State(state): State<AppState>,
    auth: Auth,
    Query(page): Query<Page>,
) -> Result<Json<Value>, AppError> {
    admin(&auth.actor)?;
    if page.cursor.as_ref().is_some_and(|value| !valid_id(value)) {
        return Err(AppError::bad_request("Invalid cursor."));
    }
    let rows = sqlx::query("SELECT id,name,role,disabled_at,created_at,revision FROM principals WHERE kind='human' AND id>? ORDER BY id LIMIT 201")
        .bind(page.cursor.unwrap_or_default())
        .fetch_all(&state.pool)
        .await?;
    let next_cursor = (rows.len() > 200).then(|| rows[199].get::<String, _>("id"));
    let items = rows
        .iter()
        .take(200)
        .map(operator_value)
        .collect::<Vec<_>>();
    Ok(response(json!({"items":items,"next_cursor":next_cursor})))
}

async fn get_operator(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    admin(&auth.actor)?;
    if !valid_id(&id) {
        return Err(AppError::not_found());
    }
    let row = operator_row(&state.pool, &id).await?;
    Ok(response(json!({"operator":operator_value(&row)})))
}

async fn get_own_account(
    State(state): State<AppState>,
    auth: Auth,
) -> Result<Json<Value>, AppError> {
    auth.require_browser()?;
    let row = operator_row(&state.pool, &auth.actor.id).await?;
    Ok(response(json!({"operator":operator_value(&row)})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateOperatorInput {
    name: String,
    role: String,
    password: String,
}

#[derive(Serialize)]
struct CreateOperatorReceipt<'a> {
    name: &'a str,
    role: &'a str,
    password_verifier: String,
}

fn idempotency_key(headers: &HeaderMap) -> Option<&str> {
    let mut values = headers.get_all("idempotency-key").iter();
    let value = values.next()?.to_str().ok()?;
    if values.next().is_some()
        || value.is_empty()
        || value.len() > 128
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return None;
    }
    Some(value)
}

struct CreatedOperatorRetry {
    password_hash: String,
    original_name: String,
    original_role: String,
}

async fn created_operator_retry(
    state: &AppState,
    actor_id: &str,
    headers: &HeaderMap,
) -> Result<Option<CreatedOperatorRetry>, AppError> {
    let Some(key) = idempotency_key(headers) else {
        return Ok(None);
    };
    let receipt: Option<String> = sqlx::query_scalar("SELECT mr.result_json FROM mutation_receipts mr JOIN service_state ss ON ss.singleton=1 AND ss.authority_epoch=mr.authority_epoch WHERE mr.principal_id=? AND mr.operation='POST /api/v1/admin/operators' AND mr.key=?")
        .bind(actor_id)
        .bind(key)
        .fetch_optional(&state.pool)
        .await?;
    let Some(receipt) = receipt else {
        return Ok(None);
    };
    let result: Value = serde_json::from_str(&receipt)?;
    let id = result
        .pointer("/operator/id")
        .and_then(Value::as_str)
        .ok_or_else(AppError::internal)?;
    let original_name = result
        .pointer("/operator/name")
        .and_then(Value::as_str)
        .ok_or_else(AppError::internal)?
        .to_owned();
    let original_role = result
        .pointer("/operator/role")
        .and_then(Value::as_str)
        .ok_or_else(AppError::internal)?
        .to_owned();
    let password_hash =
        sqlx::query_scalar("SELECT password_hash FROM principals WHERE id=? AND kind='human'")
            .bind(id)
            .fetch_optional(&state.pool)
            .await?;
    Ok(password_hash.map(|password_hash| CreatedOperatorRetry {
        password_hash,
        original_name,
        original_role,
    }))
}

async fn create_operator(
    State(state): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    Json(input): Json<CreateOperatorInput>,
) -> Result<Json<Value>, AppError> {
    admin(&auth.actor)?;
    validate_name(&input.name)?;
    validate_human_role(&input.role)?;
    validate_password(&input.password)?;
    let mut receipt_already_exists = false;
    let mut password_matches = true;
    let existing_retry = created_operator_retry(&state, &auth.actor.id, &headers).await?;
    let mut password_hash = if let Some(existing) = &existing_retry {
        receipt_already_exists = true;
        password_matches = verify_password(
            &state,
            input.password.clone(),
            existing.password_hash.clone(),
        )
        .await?;
        existing.password_hash.clone()
    } else {
        hash_password_bounded(&state, input.password.clone()).await?
    };
    let mut receipt = CreateOperatorReceipt {
        name: &input.name,
        role: &input.role,
        // Hashing the already slow, uniquely salted verifier binds a retry
        // without retaining another password-checking oracle.
        password_verifier: digest(&password_hash),
    };
    let mut mutation = match Mutation::begin_human_admin_account_creation(
        &state, &auth, &headers, &receipt,
    )
    .await
    {
        Ok(mutation) => mutation,
        Err(error) if error.code == "idempotency_conflict" && !receipt_already_exists => {
            // An identical concurrent request may have committed while this
            // request was hashing with its own salt. Reconstruct the verifier
            // from the committed result, verify the re-entered secret off-lock,
            // and then let Mutation recheck current authentication under a new
            // writer lock.
            let existing = created_operator_retry(&state, &auth.actor.id, &headers)
                .await?
                .ok_or(error)?;
            if !verify_password(
                &state,
                input.password.clone(),
                existing.password_hash.clone(),
            )
            .await?
            {
                return Err(AppError::conflict(
                    "idempotency_secret_mismatch",
                    "The re-entered password does not match the original account-creation request.",
                ));
            }
            password_hash = existing.password_hash;
            receipt.password_verifier = digest(&password_hash);
            Mutation::begin_human_admin_account_creation(&state, &auth, &headers, &receipt).await?
        }
        Err(error) if error.code == "idempotency_conflict" => {
            if let Some(existing) = &existing_retry
                && (existing.original_name != input.name || existing.original_role != input.role)
            {
                return Err(error);
            }
            return Err(AppError::conflict(
                "idempotency_secret_mismatch",
                "The original account-creation password can no longer be verified against the current account.",
            ));
        }
        Err(error) => return Err(error),
    };
    admin(&mutation.actor)?;
    if !password_matches {
        return Err(AppError::conflict(
            "idempotency_secret_mismatch",
            "The re-entered password does not match the original account-creation request.",
        ));
    }
    if let Some(replay) = &mutation.replay {
        let replay_id = replay
            .pointer("/operator/id")
            .and_then(Value::as_str)
            .ok_or_else(AppError::internal)?;
        let current_hash: String =
            sqlx::query_scalar("SELECT password_hash FROM principals WHERE id=? AND kind='human'")
                .bind(replay_id)
                .fetch_optional(&mut *mutation.tx)
                .await?
                .ok_or_else(AppError::not_found)?;
        if current_hash != password_hash {
            return Err(AppError::conflict(
                "idempotency_secret_mismatch",
                "The original account-creation password can no longer be verified against the current account.",
            ));
        }
        let current = operator_row(&mut *mutation.tx, replay_id).await?;
        return Ok(response(json!({"operator":operator_value(&current)})));
    }
    let exists: i64 = sqlx::query_scalar("SELECT count(*) FROM principals WHERE name=?")
        .bind(&input.name)
        .fetch_one(&mut *mutation.tx)
        .await?;
    if exists != 0 {
        return Err(AppError::conflict(
            "operator_name_conflict",
            "An account already uses that name.",
        ));
    }
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO principals(id,name,kind,role,password_hash,created_at,revision) VALUES(?,?,'human',?,?,?,1)")
        .bind(&id)
        .bind(&input.name)
        .bind(&input.role)
        .bind(password_hash)
        .bind(mutation.now)
        .execute(&mut *mutation.tx)
        .await?;
    let row = operator_row(&mut *mutation.tx, &id).await?;
    let data = json!({"operator":operator_value(&row)});
    Ok(response(
        mutation.finish(data, None, "operator_created", &id).await?,
    ))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct UpdateAccessInput {
    expected_revision: i64,
    role: String,
    enabled: bool,
}

async fn update_operator_access(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<UpdateAccessInput>,
) -> Result<Json<Value>, AppError> {
    admin(&auth.actor)?;
    validate_human_role(&input.role)?;
    if !valid_id(&id) || input.expected_revision < 1 {
        return Err(AppError::bad_request(
            "Provide a valid operator ID and positive expected_revision.",
        ));
    }
    let mut mutation = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/admin/operators/{id}/access"),
        &input,
    )
    .await?;
    admin(&mutation.actor)?;
    if let Some(replay) = &mutation.replay {
        let current = operator_row(&mut *mutation.tx, &id).await?;
        let sessions_revoked = replay
            .get("sessions_revoked")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        return Ok(response(json!({
            "operator":operator_value(&current),
            "sessions_revoked":sessions_revoked
        })));
    }
    let current = operator_row(&mut *mutation.tx, &id).await?;
    let revision: i64 = current.get("revision");
    if revision != input.expected_revision {
        return Err(AppError::conflict(
            "revision_conflict",
            "The operator account changed. Reload it before changing access.",
        ));
    }
    let current_role: String = current.get("role");
    let current_enabled = current.get::<Option<i64>, _>("disabled_at").is_none();
    if current_role == "admin" && current_enabled && (input.role != "admin" || !input.enabled) {
        let active_admins: i64 = sqlx::query_scalar("SELECT count(*) FROM principals WHERE kind='human' AND role='admin' AND disabled_at IS NULL")
            .fetch_one(&mut *mutation.tx)
            .await?;
        if active_admins <= 1 {
            return Err(AppError::conflict(
                "last_active_admin",
                "Create or enable another administrator before removing this administrator's access.",
            ));
        }
    }
    if current_role != input.role || current_enabled != input.enabled {
        let disabled_at = (!input.enabled).then_some(mutation.now);
        sqlx::query("UPDATE principals SET role=?,disabled_at=?,revision=revision+1 WHERE id=? AND revision=?")
            .bind(&input.role)
            .bind(disabled_at)
            .bind(&id)
            .bind(input.expected_revision)
            .execute(&mut *mutation.tx)
            .await?;
        sqlx::query(
            "UPDATE browser_sessions SET revoked_at=COALESCE(revoked_at,?) WHERE principal_id=?",
        )
        .bind(mutation.now)
        .bind(&id)
        .execute(&mut *mutation.tx)
        .await?;
    }
    let row = operator_row(&mut *mutation.tx, &id).await?;
    let data = json!({"operator":operator_value(&row),"sessions_revoked":current_role != input.role || current_enabled != input.enabled});
    Ok(response(
        mutation
            .finish(data, None, "operator_access_changed", &id)
            .await?,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangePasswordInput {
    current_password: String,
    new_password: String,
    expected_revision: i64,
}

#[derive(Serialize)]
struct ChangePasswordReceipt {
    expected_revision: i64,
    new_password_verifier: String,
}

async fn change_password(
    State(state): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    Json(input): Json<ChangePasswordInput>,
) -> Result<Response, AppError> {
    auth.require_browser()?;
    validate_password(&input.current_password)?;
    validate_password(&input.new_password)?;
    if input.current_password == input.new_password || input.expected_revision < 1 {
        return Err(AppError::bad_request(
            "Choose a different password and provide a positive expected_revision.",
        ));
    }
    let original_hash: String = sqlx::query_scalar(
        "SELECT password_hash FROM principals WHERE id=? AND kind='human' AND disabled_at IS NULL",
    )
    .bind(&auth.actor.id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(AppError::auth_required)?;
    if !verify_password(&state, input.current_password, original_hash.clone()).await? {
        return Err(AppError::auth_required());
    }
    let new_hash = hash_password_bounded(&state, input.new_password).await?;
    let receipt = ChangePasswordReceipt {
        expected_revision: input.expected_revision,
        new_password_verifier: digest(&new_hash),
    };
    let mut mutation = Mutation::begin(
        &state,
        &auth,
        &headers,
        "POST /api/v1/auth/password",
        &receipt,
    )
    .await?;
    mutation
        .actor
        .session_id
        .as_ref()
        .ok_or_else(AppError::auth_required)?;
    let current = operator_row(&mut *mutation.tx, &mutation.actor.id).await?;
    if current.get::<i64, _>("revision") != input.expected_revision {
        return Err(AppError::conflict(
            "revision_conflict",
            "The account changed. Sign in again before changing the password.",
        ));
    }
    let unchanged: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM principals WHERE id=? AND password_hash=? AND disabled_at IS NULL",
    )
    .bind(&mutation.actor.id)
    .bind(&original_hash)
    .fetch_one(&mut *mutation.tx)
    .await?;
    if unchanged != 1 {
        return Err(AppError::auth_required());
    }
    sqlx::query("UPDATE principals SET password_hash=?,revision=revision+1 WHERE id=?")
        .bind(new_hash)
        .bind(&mutation.actor.id)
        .execute(&mut *mutation.tx)
        .await?;
    sqlx::query(
        "UPDATE browser_sessions SET revoked_at=COALESCE(revoked_at,?) WHERE principal_id=?",
    )
    .bind(mutation.now)
    .bind(&mutation.actor.id)
    .execute(&mut *mutation.tx)
    .await?;
    let id = mutation.actor.id.clone();
    let row = operator_row(&mut *mutation.tx, &id).await?;
    let data = mutation
        .finish(
            json!({"operator":operator_value(&row),"all_sessions_revoked":true}),
            None,
            "operator_password_changed",
            &id,
        )
        .await?;
    let mut result = response(data).into_response();
    result
        .headers_mut()
        .insert(header::SET_COOKIE, clear_browser_cookie(&state));
    Ok(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserSessionPage {
    principal_id: Option<String>,
    cursor: Option<String>,
}

fn browser_session_value(row: &sqlx::sqlite::SqliteRow, current: Option<&str>) -> Value {
    let id: String = row.get("id");
    json!({
        "id":id,
        "principal_id":row.get::<String,_>("principal_id"),
        "created_at":row.get::<Option<i64>,_>("created_at").map(timestamp),
        "expires_at":timestamp(row.get("expires_at")),
        "revoked_at":row.get::<Option<i64>,_>("revoked_at").map(timestamp),
        "current":current == Some(id.as_str()),
    })
}

async fn list_browser_sessions(
    State(state): State<AppState>,
    auth: Auth,
    Query(page): Query<BrowserSessionPage>,
) -> Result<Json<Value>, AppError> {
    auth.require_browser()?;
    if page.cursor.as_ref().is_some_and(|value| !valid_id(value))
        || page
            .principal_id
            .as_ref()
            .is_some_and(|value| !valid_id(value))
    {
        return Err(AppError::bad_request("Invalid principal or cursor."));
    }
    let principal = page.principal_id.unwrap_or_else(|| auth.actor.id.clone());
    if principal != auth.actor.id {
        admin(&auth.actor)?;
    }
    let rows = sqlx::query("SELECT id,principal_id,created_at,expires_at,revoked_at FROM browser_sessions WHERE principal_id=? AND id>? ORDER BY id LIMIT 201")
        .bind(&principal)
        .bind(page.cursor.unwrap_or_default())
        .fetch_all(&state.pool)
        .await?;
    let next_cursor = (rows.len() > 200).then(|| rows[199].get::<String, _>("id"));
    let items = rows
        .iter()
        .take(200)
        .map(|row| browser_session_value(row, auth.actor.session_id.as_deref()))
        .collect::<Vec<_>>();
    Ok(response(json!({
        "principal_id":principal,
        "items":items,
        "next_cursor":next_cursor
    })))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

async fn revoke_browser_session(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<Empty>,
) -> Result<Response, AppError> {
    auth.require_browser()?;
    if !valid_id(&id) {
        return Err(AppError::not_found());
    }
    let mut mutation = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/browser-sessions/{id}/revoke"),
        &input,
    )
    .await?;
    if let Some(replay) = &mutation.replay {
        let target_principal = replay
            .get("principal_id")
            .and_then(Value::as_str)
            .ok_or_else(AppError::internal)?;
        if target_principal != mutation.actor.id {
            admin(&mutation.actor)?;
        }
        return Ok(response(replay.clone()).into_response());
    }
    let target = sqlx::query("SELECT principal_id FROM browser_sessions WHERE id=?")
        .bind(&id)
        .fetch_optional(&mut *mutation.tx)
        .await?
        .ok_or_else(AppError::not_found)?;
    let principal_id: String = target.get("principal_id");
    if principal_id != mutation.actor.id {
        admin(&mutation.actor)?;
    }
    sqlx::query("UPDATE browser_sessions SET revoked_at=COALESCE(revoked_at,?) WHERE id=?")
        .bind(mutation.now)
        .bind(&id)
        .execute(&mut *mutation.tx)
        .await?;
    let current = mutation.actor.session_id.as_deref() == Some(id.as_str());
    let data = mutation
        .finish(
            json!({"id":id,"principal_id":principal_id,"revoked":true,"current":current}),
            None,
            "browser_session_revoked",
            &id,
        )
        .await?;
    let mut result = response(data).into_response();
    if current {
        result
            .headers_mut()
            .insert(header::SET_COOKIE, clear_browser_cookie(&state));
    }
    Ok(result)
}

fn default_revoke_old() -> bool {
    true
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RotateCredentialInput {
    name: String,
    #[serde(default = "default_revoke_old")]
    revoke_old: bool,
}

async fn rotate_agent_credential(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<RotateCredentialInput>,
) -> Result<Json<Value>, AppError> {
    admin(&auth.actor)?;
    validate_name(&input.name)?;
    if !valid_id(&id) {
        return Err(AppError::not_found());
    }
    let mut mutation = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/admin/credentials/{id}/rotate"),
        &input,
    )
    .await?;
    admin(&mutation.actor)?;
    if let Some(replay) = &mutation.replay {
        let mut data = replay.clone();
        data["secret_unavailable"] = json!(true);
        data["next_action"] = json!(
            "Rotate the newly created replacement credential with a new idempotency key; the default rotation revokes that credential while issuing another token."
        );
        return Ok(response(data));
    }
    let old = sqlx::query("SELECT c.principal_id,c.revoked_at,p.name AS principal_name,p.disabled_at FROM credentials c JOIN principals p ON p.id=c.principal_id WHERE c.id=? AND p.kind='agent' AND p.role='agent'")
        .bind(&id)
        .fetch_optional(&mut *mutation.tx)
        .await?
        .ok_or_else(AppError::not_found)?;
    if old.get::<Option<i64>, _>("revoked_at").is_some()
        || old.get::<Option<i64>, _>("disabled_at").is_some()
    {
        return Err(AppError::conflict(
            "credential_not_active",
            "Rotate an active credential for an enabled agent principal.",
        ));
    }
    let principal_id: String = old.get("principal_id");
    let principal_name: String = old.get("principal_name");
    let credential_id = uuid::Uuid::new_v4().to_string();
    let token = secret();
    sqlx::query("INSERT INTO credentials(id,principal_id,token_hash,name,issued_by,created_at) VALUES(?,?,?,?,?,?)")
        .bind(&credential_id)
        .bind(&principal_id)
        .bind(digest(&token))
        .bind(&input.name)
        .bind(&mutation.actor.id)
        .bind(mutation.now)
        .execute(&mut *mutation.tx)
        .await?;
    if input.revoke_old {
        sqlx::query("UPDATE credentials SET revoked_at=? WHERE id=? AND revoked_at IS NULL")
            .bind(mutation.now)
            .bind(&id)
            .execute(&mut *mutation.tx)
            .await?;
        sqlx::query(
            "UPDATE agent_sessions SET closed_at=COALESCE(closed_at,?) WHERE credential_id=?",
        )
        .bind(mutation.now)
        .bind(&id)
        .execute(&mut *mutation.tx)
        .await?;
    }
    let data = json!({
        "principal_id":principal_id,
        "principal_name":principal_name,
        "credential":{
            "id":credential_id,
            "name":input.name,
            "created_at":timestamp(mutation.now),
            "expires_at":Value::Null,
            "revoked_at":Value::Null
        },
        "replaced_credential_id":id,
        "replaced_credential_revoked":input.revoke_old
    });
    let mut data = mutation
        .finish(data, None, "agent_credential_rotated", &credential_id)
        .await?;
    data["token"] = json!(token);
    Ok(response(data))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct IssueExistingCredentialInput {
    name: String,
}

async fn issue_existing_agent_credential(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<IssueExistingCredentialInput>,
) -> Result<Json<Value>, AppError> {
    admin(&auth.actor)?;
    validate_name(&input.name)?;
    if !valid_id(&id) {
        return Err(AppError::not_found());
    }
    let mut mutation = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/admin/agents/{id}/credentials"),
        &input,
    )
    .await?;
    admin(&mutation.actor)?;
    if let Some(replay) = &mutation.replay {
        let mut data = replay.clone();
        data["secret_unavailable"] = json!(true);
        data["next_action"] =
            json!("Issue another replacement credential with a new name and idempotency key.");
        return Ok(response(data));
    }
    let principal = sqlx::query(
        "SELECT id,name,disabled_at FROM principals WHERE id=? AND kind='agent' AND role='agent'",
    )
    .bind(&id)
    .fetch_optional(&mut *mutation.tx)
    .await?
    .ok_or_else(AppError::not_found)?;
    if principal.get::<Option<i64>, _>("disabled_at").is_some() {
        return Err(AppError::conflict(
            "agent_not_active",
            "Enable the existing agent principal before issuing a credential.",
        ));
    }
    let credential_id = uuid::Uuid::new_v4().to_string();
    let token = secret();
    sqlx::query("INSERT INTO credentials(id,principal_id,token_hash,name,issued_by,created_at) VALUES(?,?,?,?,?,?)")
        .bind(&credential_id)
        .bind(&id)
        .bind(digest(&token))
        .bind(&input.name)
        .bind(&mutation.actor.id)
        .bind(mutation.now)
        .execute(&mut *mutation.tx)
        .await?;
    let data = json!({
        "principal_id":id,
        "principal_name":principal.get::<String,_>("name"),
        "credential":{
            "id":credential_id,
            "name":input.name,
            "created_at":timestamp(mutation.now),
            "expires_at":Value::Null,
            "revoked_at":Value::Null
        }
    });
    let mut data = mutation
        .finish(
            data,
            None,
            "existing_agent_credential_issued",
            &credential_id,
        )
        .await?;
    data["token"] = json!(token);
    Ok(response(data))
}

/// Host-local lost-password recovery. This function is intentionally not mounted
/// as an HTTP route. It replaces the password, enables the named human account,
/// and revokes all browser sessions without changing work or agent credentials.
pub async fn recover_operator_password(
    state: &AppState,
    username: &str,
    new_password: String,
    reason: &str,
) -> Result<Value, AppError> {
    validate_name(username)?;
    validate_password(&new_password)?;
    if reason.trim() != reason
        || reason.is_empty()
        || reason.len() > 500
        || reason.chars().any(char::is_control)
        || reason.contains(&new_password)
    {
        return Err(AppError::bad_request(
            "Recovery reason must contain 1 to 500 bytes without control characters, surrounding whitespace, or the new password.",
        ));
    }
    let password_hash = hash_password_bounded(state, new_password).await?;
    let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    let now = state.now();
    let current = sqlx::query("SELECT id,name,role,disabled_at,created_at,revision FROM principals WHERE name=? AND kind='human'")
        .bind(username)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(AppError::not_found)?;
    let id: String = current.get("id");
    sqlx::query(
        "UPDATE principals SET password_hash=?,disabled_at=NULL,revision=revision+1 WHERE id=?",
    )
    .bind(password_hash)
    .bind(&id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE browser_sessions SET revoked_at=COALESCE(revoked_at,?) WHERE principal_id=?",
    )
    .bind(now)
    .bind(&id)
    .execute(&mut *tx)
    .await?;
    let event_data = serde_json::to_string(&json!({
        "reason":reason,
        "host_local":true,
        "initiator_kind":"host_operator",
        "authenticated_principal_id":Value::Null,
        "subject_principal_id":id,
        "actor_id_role":"subject_reference"
    }))?;
    sqlx::query("INSERT INTO events(actor_id,kind,record_id,data_json,created_at) VALUES(?,'operator_password_recovered',?,?,?)")
        .bind(&id)
        .bind(&id)
        .bind(event_data)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    let row = operator_row(&mut *tx, &id).await?;
    let result = json!({
        "operator":operator_value(&row),
        "all_sessions_revoked":true,
        "host_local":true
    });
    tx.commit().await?;
    Ok(result)
}
