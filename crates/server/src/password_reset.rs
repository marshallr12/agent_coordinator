//! Password recovery by email.
//!
//! Anyone may ask for a reset link by typing an address; the answer never says
//! whether an account has it. When a verified address belongs to enabled human
//! accounts, each gets a random token, stored only as a hash and valid for one
//! hour and one use. The mailed link opens a page with a form; only its POST
//! changes the password, so mail scanners cannot consume the link. A reset
//! signs the account out everywhere and cancels its other links.
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection};

use crate::{
    account_email::{OutgoingMail, RESEND_COOLDOWN_MS, validate_email},
    auth::{digest, hash_password_bounded, require_origin, secret},
    error::AppError,
    response,
    state::AppState,
};

/// How long a mailed reset link stays usable.
pub const RESET_TTL_MS: i64 = 60 * 60 * 1000;
/// Accounts that may share one address and be offered a link in one message.
const MAX_ACCOUNTS_PER_ADDRESS: i64 = 10;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/auth/password/forgot", post(request_reset))
        .route(
            "/api/v1/auth/password/reset",
            get(reset_page).post(reset_password),
        )
}

/// True for the requests of the recovery workflow. They carry no credential:
/// the typed address (which only ever causes a message) or the token in the
/// mailed link is the whole authority.
pub(crate) fn is_recovery_request(method: &axum::http::Method, path: &str) -> bool {
    use axum::http::Method;
    (*method == Method::POST && path == "/api/v1/auth/password/forgot")
        || (matches!(*method, Method::GET | Method::POST) && path == "/api/v1/auth/password/reset")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ForgotInput {
    email: String,
}

struct IssuedReset {
    name: String,
    token: String,
}

/// `POST /api/v1/auth/password/forgot {email}`: mail a reset link to every
/// enabled account whose verified address this is. The reply is the same
/// whether or not any account matched.
async fn request_reset(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<ForgotInput>,
) -> Result<Json<Value>, AppError> {
    require_origin(&headers, &state)?;
    let email = validate_email(&input.email)?;
    if !state
        .reset_limits
        .lock()
        .map_err(|_| AppError::internal())?
        .admit(&email.to_ascii_lowercase())
    {
        return Err(AppError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "Too many reset requests. Wait one minute before trying again.",
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
    let now = clock.now;
    let accounts = sqlx::query(
        "SELECT id,name,email FROM principals WHERE kind='human' AND disabled_at IS NULL \
         AND email_verified_at IS NOT NULL AND lower(email)=lower(?) ORDER BY name LIMIT ?",
    )
    .bind(&email)
    .bind(MAX_ACCOUNTS_PER_ADDRESS)
    .fetch_all(&mut *tx)
    .await?;
    let mut issued = Vec::new();
    for account in &accounts {
        let id: String = account.get("id");
        // One message a minute per account, so the form cannot flood an inbox.
        let last: Option<i64> =
            sqlx::query_scalar("SELECT max(created_at) FROM password_resets WHERE principal_id=?")
                .bind(&id)
                .fetch_one(&mut *tx)
                .await?;
        if last.is_some_and(|last| last + RESEND_COOLDOWN_MS > now) {
            continue;
        }
        sqlx::query("UPDATE password_resets SET invalidated_at=? WHERE principal_id=? AND used_at IS NULL AND invalidated_at IS NULL")
            .bind(now)
            .bind(&id)
            .execute(&mut *tx)
            .await?;
        let token = secret();
        sqlx::query("INSERT INTO password_resets(id,principal_id,email,token_hash,created_at,expires_at) VALUES(?,?,?,?,?,?)")
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(&id)
            .bind(account.get::<String, _>("email"))
            .bind(digest(&token))
            .bind(now)
            .bind(now + RESET_TTL_MS)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO events(project_id,actor_id,kind,record_id,data_json,created_at) VALUES(NULL,?,'operator_password_reset_requested',?,'{}',?)")
            .bind(&id)
            .bind(&id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        issued.push(IssuedReset {
            name: account.get("name"),
            token,
        });
    }
    tx.commit().await?;
    if !issued.is_empty() {
        deliver(&state, email, issued);
    }
    Ok(response(json!({"requested":true})))
}

/// Send one message holding a link for each account, after the transaction
/// that issued them has committed.
fn deliver(state: &AppState, to: String, issued: Vec<IssuedReset>) {
    let state = state.clone();
    tokio::spawn(async move {
        let links = issued
            .iter()
            .map(|reset| {
                format!(
                    "Account {}:\n{}/api/v1/auth/password/reset?token={}\n",
                    reset.name, state.config.public_origin, reset.token
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mail = OutgoingMail {
            to,
            subject: "Reset your password".into(),
            body: format!(
                "Someone asked to reset the password of an Agent Coordinator account with this email address. Open the link for your account to choose a new password:\n\n{links}\nEach link works once and expires in 1 hour. Resetting a password signs that account out everywhere.\n\nIf you did not ask for this, ignore this message; your password has not changed.\n"
            ),
        };
        if let Err(error) = state.mailer.send(mail).await {
            tracing::warn!(%error, "Password reset email was not sent");
        }
    });
}

#[derive(Deserialize)]
struct ResetQuery {
    token: String,
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn page(heading: &str, body: &str) -> Response {
    // Not a 4xx: the router replaces non-JSON client errors with a JSON envelope.
    (
        StatusCode::OK,
        Html(format!(
            "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
             <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
             <title>Agent Coordinator password reset</title></head><body><h1>{heading}</h1>{body}</body></html>"
        )),
    )
        .into_response()
}

fn expired_page() -> Response {
    page(
        "This link has expired",
        "<p>This password reset link has expired, was already used, or was replaced by a newer email. Request another from the sign-in page.</p><p><a href=\"/\">Open the sign-in page</a></p>",
    )
}

fn form_page(token: &str, name: &str, problem: Option<&str>) -> Response {
    let problem = problem
        .map(|text| format!("<p role=\"alert\">{}</p>", escape(text)))
        .unwrap_or_default();
    // A token is 64 hexadecimal digits, so it needs no escaping.
    page(
        "Choose a new password",
        &format!(
            "<p>Account: <strong>{}</strong>. Resetting the password signs this account out everywhere.</p>{problem}\
             <form method=\"post\" action=\"/api/v1/auth/password/reset\">\
             <input type=\"hidden\" name=\"token\" value=\"{token}\">\
             <p><label>New password (at least 12 characters)<br>\
             <input type=\"password\" name=\"password\" autocomplete=\"new-password\" minlength=\"12\" maxlength=\"1024\" required></label></p>\
             <p><label>Confirm new password<br>\
             <input type=\"password\" name=\"confirmation\" autocomplete=\"new-password\" minlength=\"12\" maxlength=\"1024\" required></label></p>\
             <button type=\"submit\">Reset password</button></form>",
            escape(name)
        ),
    )
}

fn token_shaped(token: &str) -> bool {
    token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit())
}

struct UsableReset {
    id: String,
    principal_id: String,
    name: String,
}

/// The link's reset when it is unused, current, unexpired, and still names the
/// verified address of an enabled account.
async fn usable_reset(
    c: &mut SqliteConnection,
    token: &str,
    now: i64,
) -> Result<Option<UsableReset>, AppError> {
    if !token_shaped(token) {
        return Ok(None);
    }
    let row = sqlx::query(
        "SELECT r.id,r.principal_id,p.name FROM password_resets r JOIN principals p ON p.id=r.principal_id \
         WHERE r.token_hash=? AND r.used_at IS NULL AND r.invalidated_at IS NULL AND r.expires_at>? \
         AND p.kind='human' AND p.disabled_at IS NULL AND p.email=r.email AND p.email_verified_at IS NOT NULL",
    )
    .bind(digest(token))
    .bind(now)
    .fetch_optional(c)
    .await?;
    Ok(row.map(|row| UsableReset {
        id: row.get("id"),
        principal_id: row.get("principal_id"),
        name: row.get("name"),
    }))
}

/// `GET /api/v1/auth/password/reset?token=`: the new-password form. Mail
/// scanners and link previews fetch links without submitting, so only the POST
/// changes anything.
async fn reset_page(
    State(state): State<AppState>,
    Query(query): Query<ResetQuery>,
) -> Result<Response, AppError> {
    let mut c = state.pool.acquire().await?;
    Ok(
        match usable_reset(&mut c, &query.token, state.now()).await? {
            Some(usable) => form_page(&query.token, &usable.name, None),
            None => expired_page(),
        },
    )
}

/// `POST /api/v1/auth/password/reset` with a form body `token=&password=&confirmation=`.
/// The token authorizes this one password change and nothing else.
async fn reset_password(State(state): State<AppState>, body: Bytes) -> Result<Response, AppError> {
    let field = |wanted: &str| {
        url::form_urlencoded::parse(&body)
            .find(|(name, _)| name == wanted)
            .map(|(_, value)| value.into_owned())
    };
    let token =
        field("token").ok_or_else(|| AppError::bad_request("The form needs a token field."))?;
    let password = field("password").unwrap_or_default();
    let confirmation = field("confirmation").unwrap_or_default();
    let name = {
        let mut c = state.pool.acquire().await?;
        match usable_reset(&mut c, &token, state.now()).await? {
            Some(usable) => usable.name,
            None => return Ok(expired_page()),
        }
    };
    let problem = if password != confirmation {
        Some("The two passwords do not match.")
    } else if !(12..=1024).contains(&password.len()) {
        Some("The password must contain 12 to 1024 bytes.")
    } else {
        None
    };
    if let Some(problem) = problem {
        return Ok(form_page(&token, &name, Some(problem)));
    }
    let password_hash = match hash_password_bounded(&state, password).await {
        Ok(hash) => hash,
        Err(error) if error.code == "rate_limited" => {
            return Ok(form_page(
                &token,
                &name,
                Some("The service is busy. Submit the form again in a moment."),
            ));
        }
        Err(error) => return Err(error),
    };
    let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    let clock = state.sample_clock(&mut tx).await?;
    if clock.incident_detected {
        tx.commit().await?;
        return Err(crate::state::clock_reconciliation_error());
    }
    if clock.incident_active {
        return Err(crate::state::clock_reconciliation_error());
    }
    let Some(usable) = usable_reset(&mut tx, &token, clock.now).await? else {
        tx.commit().await?;
        return Ok(expired_page());
    };
    sqlx::query("UPDATE password_resets SET used_at=? WHERE id=?")
        .bind(clock.now)
        .bind(&usable.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE password_resets SET invalidated_at=? WHERE principal_id=? AND used_at IS NULL AND invalidated_at IS NULL")
        .bind(clock.now)
        .bind(&usable.principal_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE principals SET password_hash=?,revision=revision+1 WHERE id=?")
        .bind(password_hash)
        .bind(&usable.principal_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE browser_sessions SET revoked_at=COALESCE(revoked_at,?) WHERE principal_id=?",
    )
    .bind(clock.now)
    .bind(&usable.principal_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO events(project_id,actor_id,kind,record_id,data_json,created_at) VALUES(NULL,?,'operator_password_reset',?,'{}',?)")
        .bind(&usable.principal_id)
        .bind(&usable.principal_id)
        .bind(clock.now)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    tracing::info!(account = %usable.name, "Account password reset by email link");
    Ok(page(
        "Password changed",
        "<p>Your password was changed and every browser session for the account was signed out. Sign in with the new password.</p><p><a href=\"/\">Open the sign-in page</a></p>",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping_covers_markup_characters() {
        assert_eq!(
            escape("<a href=\"x\">&"),
            "&lt;a href=&quot;x&quot;&gt;&amp;"
        );
    }

    #[test]
    fn only_the_recovery_requests_skip_authentication() {
        use axum::http::Method;
        assert!(is_recovery_request(
            &Method::POST,
            "/api/v1/auth/password/forgot"
        ));
        assert!(!is_recovery_request(
            &Method::GET,
            "/api/v1/auth/password/forgot"
        ));
        assert!(is_recovery_request(
            &Method::GET,
            "/api/v1/auth/password/reset"
        ));
        assert!(is_recovery_request(
            &Method::POST,
            "/api/v1/auth/password/reset"
        ));
        assert!(!is_recovery_request(&Method::POST, "/api/v1/auth/password"));
    }
}
