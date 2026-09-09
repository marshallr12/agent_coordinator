use argon2::{
    Algorithm, Argon2, Params, PasswordHash, PasswordHasher, PasswordVerifier, Version,
    password_hash::SaltString,
};
use axum::{
    Json, Router,
    extract::{FromRequestParts, Path, Query, State},
    http::{HeaderMap, HeaderValue, Method, header, request::Parts},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection};
use subtle::ConstantTimeEq;

use crate::{error::AppError, mutation::Mutation, response, state::AppState};

const SESSION_LIFETIME_MS: i64 = 12 * 60 * 60 * 1000;
const SECURE_COOKIE: &str = "__Host-coordinator";
const LOCAL_COOKIE: &str = "coordinator_local";
pub const SESSION_HEADER: &str = "x-coordinator-session";
pub const PROOF_HEADER: &str = "x-coordinator-session-proof";

pub fn digest(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}
pub fn secret() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("Operating system randomness unavailable");
    hex::encode(bytes)
}

#[derive(Clone, Serialize)]
pub struct Actor {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub role: String,
    pub credential_id: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Clone)]
enum Credential {
    Bearer {
        token_hash: String,
        session: Option<(String, String)>,
        allow_closed: bool,
    },
    Browser {
        token_hash: String,
        csrf: String,
    },
}

/// Credential verifiers intentionally do not implement Debug or Serialize.
#[derive(Clone)]
pub struct Auth {
    pub actor: Actor,
    credential: Credential,
}

impl Auth {
    pub async fn verify(
        &self,
        connection: &mut SqliteConnection,
        now: i64,
    ) -> Result<Actor, AppError> {
        match &self.credential {
            Credential::Bearer {
                token_hash,
                session,
                allow_closed,
            } => {
                let row = sqlx::query("SELECT p.id,p.name,p.kind,p.role,c.id AS credential_id FROM credentials c JOIN principals p ON p.id=c.principal_id WHERE c.token_hash=? AND c.revoked_at IS NULL AND (c.expires_at IS NULL OR c.expires_at>?) AND p.disabled_at IS NULL AND p.kind='agent' AND p.role='agent'")
                    .bind(token_hash).bind(now).fetch_optional(&mut *connection).await?.ok_or_else(AppError::auth_required)?;
                let mut actor = Actor {
                    id: row.get("id"),
                    name: row.get("name"),
                    kind: row.get("kind"),
                    role: row.get("role"),
                    credential_id: Some(row.get("credential_id")),
                    session_id: None,
                };
                if let Some((id, proof_hash)) = session {
                    let session = sqlx::query("SELECT proof_hash,closed_at FROM agent_sessions WHERE id=? AND principal_id=? AND credential_id=?")
                        .bind(id).bind(&actor.id).bind(&actor.credential_id).fetch_optional(&mut *connection).await?.ok_or_else(AppError::auth_required)?;
                    let stored: String = session.get("proof_hash");
                    if !constant_eq(&stored, proof_hash)
                        || (!allow_closed && session.get::<Option<i64>, _>("closed_at").is_some())
                    {
                        return Err(AppError::auth_required());
                    }
                    actor.session_id = Some(id.clone());
                }
                Ok(actor)
            }
            Credential::Browser { token_hash, .. } => {
                let row = sqlx::query("SELECT p.id,p.name,p.kind,p.role,b.id AS session_id FROM browser_sessions b JOIN principals p ON p.id=b.principal_id WHERE b.token_hash=? AND b.revoked_at IS NULL AND b.expires_at>? AND p.disabled_at IS NULL AND p.kind='human'")
                    .bind(token_hash).bind(now).fetch_optional(&mut *connection).await?.ok_or_else(AppError::auth_required)?;
                Ok(Actor {
                    id: row.get("id"),
                    name: row.get("name"),
                    kind: row.get("kind"),
                    role: row.get("role"),
                    credential_id: None,
                    session_id: Some(row.get("session_id")),
                })
            }
        }
    }

    pub(crate) async fn authenticate(parts: &Parts, state: &AppState) -> Result<Self, AppError> {
        let bearer = one_header(&parts.headers, header::AUTHORIZATION.as_str())?;
        let cookie = session_cookie(&parts.headers, state)?;
        if bearer.is_some() && cookie.is_some() {
            return Err(AppError::auth_required());
        }
        let session = one_header(&parts.headers, SESSION_HEADER)?;
        let proof = one_header(&parts.headers, PROOF_HEADER)?;
        let credential = if let Some(bearer) = bearer {
            let token = bearer
                .strip_prefix("Bearer ")
                .filter(|t| valid_secret(t))
                .ok_or_else(AppError::auth_required)?;
            let supplied_session = match (session, proof) {
                (Some(id), Some(proof)) if valid_id(id) && valid_secret(proof) => {
                    Some((id.to_owned(), digest(proof)))
                }
                (None, None) => None,
                (None, Some(proof))
                    if parts.method == Method::POST
                        && parts.uri.path() == "/api/v1/sessions"
                        && valid_secret(proof) =>
                {
                    None
                }
                _ => return Err(AppError::auth_required()),
            };
            // A closed session may inspect its state or replay closure, but it
            // cannot authenticate any operation that grants work authority.
            let allow_closed = session.is_some_and(|id| {
                (parts.method == Method::GET
                    && parts.uri.path() == format!("/api/v1/sessions/{id}"))
                    || (parts.method == Method::POST
                        && parts.uri.path() == format!("/api/v1/sessions/{id}/close"))
            });
            Credential::Bearer {
                token_hash: digest(token),
                session: supplied_session,
                allow_closed,
            }
        } else if let Some(cookie) = cookie {
            if session.is_some() || proof.is_some() {
                return Err(AppError::auth_required());
            }
            Credential::Browser {
                token_hash: digest(&cookie),
                csrf: csrf_token(&cookie),
            }
        } else {
            return Err(AppError::auth_required());
        };
        let mut auth = Self {
            actor: Actor {
                id: String::new(),
                name: String::new(),
                kind: String::new(),
                role: String::new(),
                credential_id: None,
                session_id: None,
            },
            credential,
        };
        auth.actor = auth
            .verify(&mut *state.pool.acquire().await?, state.now())
            .await?;
        if !matches!(parts.method, Method::GET | Method::HEAD | Method::OPTIONS)
            && let Credential::Browser { csrf, .. } = &auth.credential
        {
            require_origin(&parts.headers, state)?;
            if !one_header(&parts.headers, "x-csrf-token")?
                .is_some_and(|supplied| constant_eq(supplied, csrf))
            {
                return Err(AppError::forbidden(
                    "A valid CSRF token is required for browser changes.",
                ));
            }
        }
        Ok(auth)
    }

    fn browser_data(&self) -> Value {
        let csrf = match &self.credential {
            Credential::Browser { csrf, .. } => Some(csrf),
            _ => None,
        };
        json!({"actor": self.actor, "csrf_token": csrf})
    }
}

impl FromRequestParts<AppState> for Auth {
    type Rejection = AppError;
    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if let Some(auth) = parts.extensions.get::<Auth>() {
            return Ok(auth.clone());
        }
        Self::authenticate(parts, state).await
    }
}

fn constant_eq(a: &str, b: &str) -> bool {
    bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}
fn valid_secret(value: &str) -> bool {
    (32..=512).contains(&value.len())
        && value
            .bytes()
            .all(|c| c.is_ascii_graphic() && c != b';' && c != b',')
}
fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
}
fn csrf_token(cookie: &str) -> String {
    digest(&format!("coordinator-browser-csrf-v1:{cookie}"))
}
fn one_header<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>, AppError> {
    let mut values = headers.get_all(name).iter();
    let first = values.next();
    if values.next().is_some() {
        return Err(AppError::auth_required());
    }
    first
        .map(|v| v.to_str().map_err(|_| AppError::auth_required()))
        .transpose()
}
fn session_cookie(headers: &HeaderMap, state: &AppState) -> Result<Option<String>, AppError> {
    let name = if state.config.secure_cookie() {
        SECURE_COOKIE
    } else {
        LOCAL_COOKIE
    };
    let mut found = None;
    for header in headers.get_all(header::COOKIE) {
        for pair in header
            .to_str()
            .map_err(|_| AppError::auth_required())?
            .split(';')
        {
            if let Some((key, value)) = pair.trim().split_once('=')
                && key == name
            {
                if found.is_some() || !valid_secret(value) {
                    return Err(AppError::auth_required());
                }
                found = Some(value.to_owned());
            }
        }
    }
    Ok(found)
}
fn require_origin(headers: &HeaderMap, state: &AppState) -> Result<(), AppError> {
    if one_header(headers, "origin")? != Some(state.config.public_origin.as_str()) {
        return Err(AppError::forbidden(
            "Browser changes must originate from the configured service origin.",
        ));
    }
    Ok(())
}
fn cookie_header(state: &AppState, token: &str, clear: bool) -> HeaderValue {
    let name = if state.config.secure_cookie() {
        SECURE_COOKIE
    } else {
        LOCAL_COOKIE
    };
    let secure = if state.config.secure_cookie() {
        "; Secure"
    } else {
        ""
    };
    HeaderValue::from_str(&format!(
        "{name}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{secure}",
        if clear { 0 } else { SESSION_LIFETIME_MS / 1000 }
    ))
    .expect("Generated cookie contains only safe characters")
}

fn argon2() -> Argon2<'static> {
    // OWASP minimum reviewed 2026-09-09: Argon2id, 19 MiB, t=2, p=1.
    Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(19 * 1024, 2, 1, None).expect("Valid Argon2 parameters"),
    )
}
pub async fn hash_password(password: String) -> Result<String, AppError> {
    tokio::task::spawn_blocking(move || {
        let mut salt = [0u8; 16];
        getrandom::fill(&mut salt).map_err(|_| AppError::internal())?;
        let salt = SaltString::encode_b64(&salt).map_err(|_| AppError::internal())?;
        argon2()
            .hash_password(password.as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(|_| AppError::internal())
    })
    .await
    .map_err(|_| AppError::internal())?
}

/// Host-local bootstrap only; never mounted as an HTTP route.
pub async fn init_admin(
    state: &AppState,
    username: &str,
    password: String,
) -> Result<String, AppError> {
    validate_name(username)?;
    if !(12..=1024).contains(&password.len()) {
        return Err(AppError::bad_request(
            "The password must contain 12 to 1024 bytes.",
        ));
    }
    let hash = hash_password(password).await?;
    let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    let existing: i64 = sqlx::query_scalar("SELECT count(*) FROM principals")
        .fetch_one(&mut *tx)
        .await?;
    if existing != 0 {
        return Err(AppError::conflict(
            "already_initialized",
            "Administrator initialization is only available for an empty installation.",
        ));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let now = state.now();
    sqlx::query("INSERT INTO principals(id,name,kind,role,password_hash,created_at) VALUES(?,?,'human','admin',?,?)")
        .bind(&id).bind(username).bind(hash).bind(now).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO events(actor_id,kind,record_id,data_json,created_at) VALUES(?,'admin_initialized',?,'{}',?)")
        .bind(&id).bind(&id).bind(now).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(id)
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/me", get(me))
        .route("/api/v1/admin/credentials", get(credentials))
        .route("/api/v1/admin/agents", post(create_agent))
        .route(
            "/api/v1/admin/credentials/{id}/revoke",
            post(revoke_credential),
        )
        .route("/api/v1/sessions", post(create_session))
        .route("/api/v1/sessions/{id}", get(get_session))
        .route("/api/v1/sessions/{id}/close", post(close_session))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Login {
    username: String,
    password: String,
}
async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<Login>,
) -> Result<Response, AppError> {
    require_origin(&headers, &state)?;
    if input.username.len() > 100 || input.password.len() > 1024 {
        return Err(AppError::auth_required());
    }
    if !state
        .login_limits
        .lock()
        .map_err(|_| AppError::internal())?
        .admit(&input.username)
    {
        return Err(AppError::rate_limited());
    }
    let permit = state
        .password_workers
        .clone()
        .try_acquire_owned()
        .map_err(|_| AppError::rate_limited())?;
    let row = sqlx::query("SELECT id,password_hash FROM principals WHERE name=? AND kind='human' AND disabled_at IS NULL")
        .bind(&input.username).fetch_optional(&state.pool).await?;
    let hash: String = row
        .as_ref()
        .map(|r| r.get("password_hash"))
        .unwrap_or_else(|| state.dummy_password_hash.as_ref().clone());
    let original_hash = hash.clone();
    let verified = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        PasswordHash::new(&hash).ok().is_some_and(|hash| {
            argon2()
                .verify_password(input.password.as_bytes(), &hash)
                .is_ok()
        })
    })
    .await
    .map_err(|_| AppError::internal())?;
    if !verified {
        return Err(AppError::auth_required());
    }
    let row = row.ok_or_else(AppError::auth_required)?;
    let id: String = row.get("id");
    let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    // Re-check under the write lock in case account revocation or a future
    // password change raced the deliberately off-thread password calculation.
    let row = sqlx::query("SELECT name,role FROM principals WHERE id=? AND password_hash=? AND disabled_at IS NULL AND kind='human'")
        .bind(&id).bind(original_hash).fetch_optional(&mut *tx).await?.ok_or_else(AppError::auth_required)?;
    let now = state.now();
    let token = secret();
    let session_id = uuid::Uuid::new_v4().to_string();
    // Bound retained session verifiers and active sign-ins for each account.
    sqlx::query("DELETE FROM browser_sessions WHERE principal_id=? AND (expires_at<=? OR revoked_at IS NOT NULL)")
        .bind(&id).bind(now).execute(&mut *tx).await?;
    sqlx::query("UPDATE browser_sessions SET revoked_at=? WHERE principal_id=? AND id NOT IN (SELECT id FROM browser_sessions WHERE principal_id=? AND revoked_at IS NULL ORDER BY expires_at DESC LIMIT 9)")
        .bind(now).bind(&id).bind(&id).execute(&mut *tx).await?;
    sqlx::query(
        "INSERT INTO browser_sessions(id,principal_id,token_hash,expires_at) VALUES(?,?,?,?)",
    )
    .bind(&session_id)
    .bind(&id)
    .bind(digest(&token))
    .bind(now + SESSION_LIFETIME_MS)
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO events(actor_id,kind,record_id,data_json,created_at) VALUES(?,'browser_signed_in',?,'{}',?)")
        .bind(&id).bind(&session_id).bind(now).execute(&mut *tx).await?;
    let actor = Actor {
        id,
        name: row.get("name"),
        kind: "human".into(),
        role: row.get("role"),
        credential_id: None,
        session_id: Some(session_id),
    };
    tx.commit().await?;
    let mut result =
        response(json!({"actor":actor,"csrf_token":csrf_token(&token)})).into_response();
    result
        .headers_mut()
        .insert(header::SET_COOKIE, cookie_header(&state, &token, false));
    Ok(result)
}

async fn me(auth: Auth) -> Json<Value> {
    response(auth.browser_data())
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
async fn logout(
    State(state): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    Json(input): Json<Empty>,
) -> Result<Response, AppError> {
    if !matches!(auth.credential, Credential::Browser { .. }) {
        return Err(AppError::forbidden(
            "Browser sign-out requires a browser session.",
        ));
    }
    let mut mutation =
        Mutation::begin(&state, &auth, &headers, "POST /api/v1/auth/logout", &input).await?;
    if let Some(replay) = &mutation.replay {
        return Ok(response(replay.clone()).into_response());
    }
    let id = mutation
        .actor
        .session_id
        .clone()
        .ok_or_else(AppError::auth_required)?;
    sqlx::query("UPDATE browser_sessions SET revoked_at=? WHERE id=?")
        .bind(mutation.now)
        .bind(&id)
        .execute(&mut *mutation.tx)
        .await?;
    let data = mutation
        .finish(json!({"signed_out":true}), None, "browser_signed_out", &id)
        .await?;
    let mut result = response(data).into_response();
    result
        .headers_mut()
        .insert(header::SET_COOKIE, cookie_header(&state, "", true));
    Ok(result)
}

fn admin(actor: &Actor) -> Result<(), AppError> {
    if actor.kind != "human" || actor.role != "admin" {
        return Err(AppError::forbidden(
            "A human administrator is required for credential administration.",
        ));
    }
    Ok(())
}
fn agent(actor: &Actor) -> Result<(), AppError> {
    if actor.kind != "agent" || actor.role != "agent" || actor.credential_id.is_none() {
        return Err(AppError::forbidden(
            "This operation requires an agent credential.",
        ));
    }
    Ok(())
}
fn validate_name(name: &str) -> Result<(), AppError> {
    if name.is_empty()
        || name.len() > 100
        || name.trim() != name
        || name.chars().any(char::is_control)
    {
        return Err(AppError::bad_request(
            "name must contain 1 to 100 bytes without control characters or surrounding whitespace.",
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
struct Page {
    cursor: Option<String>,
}
async fn credentials(
    State(state): State<AppState>,
    auth: Auth,
    Query(page): Query<Page>,
) -> Result<Json<Value>, AppError> {
    admin(&auth.actor)?;
    if page.cursor.as_ref().is_some_and(|cursor| !valid_id(cursor)) {
        return Err(AppError::bad_request("Invalid cursor."));
    }
    let rows = sqlx::query("SELECT c.id,p.name,c.principal_id,c.created_at,c.revoked_at,c.expires_at FROM credentials c JOIN principals p ON p.id=c.principal_id WHERE c.id>? ORDER BY c.id LIMIT 201")
        .bind(page.cursor.unwrap_or_default()).fetch_all(&state.pool).await?;
    let next = if rows.len() > 200 {
        Some(rows[199].get::<String, _>("id"))
    } else {
        None
    };
    let items: Vec<_> = rows.iter().take(200).map(|row| json!({"id":row.get::<String,_>("id"), "name":row.get::<String,_>("name"), "principal_id":row.get::<String,_>("principal_id"), "created_at":timestamp(row.get("created_at")), "revoked_at":row.get::<Option<i64>,_>("revoked_at").map(timestamp), "expires_at":row.get::<Option<i64>,_>("expires_at").map(timestamp)})).collect();
    Ok(response(json!({"items":items,"next_cursor":next})))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct NewAgent {
    name: String,
}
async fn create_agent(
    State(state): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    Json(input): Json<NewAgent>,
) -> Result<Json<Value>, AppError> {
    admin(&auth.actor)?;
    validate_name(&input.name)?;
    let mut mutation =
        Mutation::begin(&state, &auth, &headers, "POST /api/v1/admin/agents", &input).await?;
    admin(&mutation.actor)?;
    if let Some(replay) = &mutation.replay {
        let mut data = replay.clone();
        data["secret_unavailable"] = json!(true);
        data["next_action"] = json!(
            "Revoke the unused credential, then issue a replacement with a new agent name and idempotency key."
        );
        return Ok(response(data));
    }
    let principal_id = uuid::Uuid::new_v4().to_string();
    let credential_id = uuid::Uuid::new_v4().to_string();
    let token = secret();
    sqlx::query(
        "INSERT INTO principals(id,name,kind,role,created_at) VALUES(?,?,'agent','agent',?)",
    )
    .bind(&principal_id)
    .bind(&input.name)
    .bind(mutation.now)
    .execute(&mut *mutation.tx)
    .await?;
    sqlx::query("INSERT INTO credentials(id,principal_id,token_hash,created_at) VALUES(?,?,?,?)")
        .bind(&credential_id)
        .bind(&principal_id)
        .bind(digest(&token))
        .bind(mutation.now)
        .execute(&mut *mutation.tx)
        .await?;
    let mut data = mutation
        .finish(
            json!({"principal_id":principal_id,"credential_id":credential_id,"name":input.name}),
            None,
            "agent_credential_issued",
            &credential_id,
        )
        .await?;
    data["token"] = json!(token);
    Ok(response(data))
}

async fn revoke_credential(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<Empty>,
) -> Result<Json<Value>, AppError> {
    admin(&auth.actor)?;
    let mut mutation = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/admin/credentials/{id}/revoke"),
        &input,
    )
    .await?;
    admin(&mutation.actor)?;
    if let Some(replay) = &mutation.replay {
        return Ok(response(replay.clone()));
    }
    let found = sqlx::query("UPDATE credentials SET revoked_at=COALESCE(revoked_at,?) WHERE id=?")
        .bind(mutation.now)
        .bind(&id)
        .execute(&mut *mutation.tx)
        .await?
        .rows_affected();
    if found == 0 {
        return Err(AppError::not_found());
    }
    sqlx::query("UPDATE agent_sessions SET closed_at=COALESCE(closed_at,?) WHERE credential_id=?")
        .bind(mutation.now)
        .bind(&id)
        .execute(&mut *mutation.tx)
        .await?;
    Ok(response(
        mutation
            .finish(
                json!({"id":id,"revoked":true}),
                None,
                "agent_credential_revoked",
                &id,
            )
            .await?,
    ))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct NewSession {
    session_id: String,
    workstation_id: String,
    harness: String,
    capabilities: Vec<String>,
}
async fn create_session(
    State(state): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    Json(input): Json<NewSession>,
) -> Result<Json<Value>, AppError> {
    agent(&auth.actor)?;
    if !valid_id(&input.session_id)
        || !valid_id(&input.workstation_id)
        || input.harness.is_empty()
        || input.harness.len() > 200
        || input.harness.chars().any(char::is_control)
        || input.capabilities.len() > 64
        || input
            .capabilities
            .iter()
            .any(|v| v.is_empty() || v.len() > 128 || v.chars().any(char::is_control))
    {
        return Err(AppError::bad_request(
            "Invalid session identity, workstation, harness, or capabilities.",
        ));
    }
    // Never persist the resume proof, including in idempotency receipts/events.
    let proof_hash = digest(
        one_header(&headers, PROOF_HEADER)?
            .filter(|proof| valid_secret(proof))
            .ok_or_else(|| AppError::bad_request("A random session proof is required."))?,
    );
    let fingerprint = json!({"input":input,"proof_hash":proof_hash});
    let mut mutation = Mutation::begin(
        &state,
        &auth,
        &headers,
        "POST /api/v1/sessions",
        &fingerprint,
    )
    .await?;
    agent(&mutation.actor)?;
    if let Some(replay) = &mutation.replay {
        return Ok(response(replay.clone()));
    }
    let existing = sqlx::query("SELECT * FROM agent_sessions WHERE id=?")
        .bind(&input.session_id)
        .fetch_optional(&mut *mutation.tx)
        .await?;
    let data = if let Some(existing) = existing {
        if existing.get::<String, _>("principal_id") != mutation.actor.id
            || Some(existing.get::<String, _>("credential_id")) != mutation.actor.credential_id
            || !constant_eq(&existing.get::<String, _>("proof_hash"), &proof_hash)
            || existing.get::<String, _>("workstation_id") != input.workstation_id
            || existing.get::<String, _>("harness") != input.harness
            || serde_json::from_str::<Vec<String>>(&existing.get::<String, _>("capabilities"))?
                != input.capabilities
        {
            return Err(AppError::conflict(
                "session_conflict",
                "That session identity is already registered with different parameters.",
            ));
        }
        if existing.get::<Option<i64>, _>("closed_at").is_some() {
            return Err(AppError::conflict(
                "session_closed",
                "This session is closed. Create a new session identity and proof.",
            ));
        }
        session_json(&existing)?
    } else {
        sqlx::query("INSERT INTO agent_sessions(id,principal_id,credential_id,workstation_id,proof_hash,created_at,capabilities,harness) VALUES(?,?,?,?,?,?,?,?)")
            .bind(&input.session_id).bind(&mutation.actor.id).bind(&mutation.actor.credential_id).bind(&input.workstation_id).bind(&proof_hash).bind(mutation.now)
            .bind(serde_json::to_string(&input.capabilities)?).bind(&input.harness).execute(&mut *mutation.tx).await?;
        let row = sqlx::query("SELECT * FROM agent_sessions WHERE id=?")
            .bind(&input.session_id)
            .fetch_one(&mut *mutation.tx)
            .await?;
        session_json(&row)?
    };
    Ok(response(
        mutation
            .finish(data, None, "agent_session_registered", &input.session_id)
            .await?,
    ))
}

fn session_json(row: &sqlx::sqlite::SqliteRow) -> Result<Value, AppError> {
    Ok(
        json!({"id":row.get::<String,_>("id"),"session_id":row.get::<String,_>("id"),"principal_id":row.get::<String,_>("principal_id"),
        "credential_id":row.get::<String,_>("credential_id"),"workstation_id":row.get::<String,_>("workstation_id"),
        "harness":row.get::<String,_>("harness"),"capabilities":serde_json::from_str::<Value>(&row.get::<String,_>("capabilities"))?,
        "created_at":timestamp(row.get("created_at")),"closed_at":row.get::<Option<i64>,_>("closed_at").map(timestamp)}),
    )
}
fn timestamp(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
async fn get_session(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    agent(&auth.actor)?;
    if auth.actor.session_id.as_deref() != Some(&id) {
        return Err(AppError::forbidden(
            "Supply this session's identity and proof to inspect it.",
        ));
    }
    let row = sqlx::query(
        "SELECT * FROM agent_sessions WHERE id=? AND credential_id=? AND principal_id=?",
    )
    .bind(&id)
    .bind(&auth.actor.credential_id)
    .bind(&auth.actor.id)
    .fetch_one(&state.pool)
    .await?;
    Ok(response(session_json(&row)?))
}
async fn close_session(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<Empty>,
) -> Result<Json<Value>, AppError> {
    agent(&auth.actor)?;
    if auth.actor.session_id.as_deref() != Some(&id) {
        return Err(AppError::forbidden(
            "Only the authenticated owning session can close itself.",
        ));
    }
    let mut mutation = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/sessions/{id}/close"),
        &input,
    )
    .await?;
    if let Some(replay) = &mutation.replay {
        return Ok(response(replay.clone()));
    }
    sqlx::query(
        "UPDATE agent_sessions SET closed_at=COALESCE(closed_at,?) WHERE id=? AND credential_id=?",
    )
    .bind(mutation.now)
    .bind(&id)
    .bind(&mutation.actor.credential_id)
    .execute(&mut *mutation.tx)
    .await?;
    Ok(response(
        mutation
            .finish(
                json!({"id":id,"session_id":id,"closed":true}),
                None,
                "agent_session_closed",
                &id,
            )
            .await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn salted_argon2id_passwords() {
        let password = "a test password with enough entropy";
        let first = hash_password(password.into()).await.unwrap();
        let second = hash_password(password.into()).await.unwrap();
        assert_ne!(first, second);
        assert!(first.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
        assert!(
            argon2()
                .verify_password(password.as_bytes(), &PasswordHash::new(&first).unwrap())
                .is_ok()
        );
        assert!(
            argon2()
                .verify_password(b"incorrect", &PasswordHash::new(&first).unwrap())
                .is_err()
        );
    }
    #[test]
    fn login_budget_bounds_different_names_and_repeated_attempts() {
        let mut limits = crate::state::LoginLimits::default();
        for _ in 0..5 {
            assert!(limits.admit("admin"));
        }
        assert!(!limits.admit("admin"));
        for i in 0..24 {
            assert!(limits.admit(&format!("name-{i}")));
        }
        assert!(!limits.admit("new-name"));
    }
}
