//! Email addresses on human accounts: verification links and their delivery.
//!
//! An address is `unverified` until its owner opens the mailed link. Each link
//! carries a random token stored only as a hash and valid for one use. Issuing a
//! newer link invalidates every older one for the account. The message is sent
//! after the database transaction commits; a failed send is recorded as a short
//! code and the owner can ask for another link.
use axum::{
    Router,
    body::Bytes,
    extract::{Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection};
use std::{future::Future, pin::Pin, str::FromStr, sync::Arc, time::Duration};

use crate::{
    auth::{digest, secret, timestamp},
    error::AppError,
    state::AppState,
};

/// How long a mailed link stays usable.
pub const VERIFICATION_TTL_MS: i64 = 24 * 60 * 60 * 1000;
/// How long after a link is issued before another may be requested.
pub const RESEND_COOLDOWN_MS: i64 = 60 * 1000;
const SEND_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmtpTls {
    /// Plain connection upgraded with STARTTLS (port 587).
    StartTls,
    /// TLS from the first byte (port 465).
    Tls,
}

impl FromStr for SmtpTls {
    type Err = String;
    fn from_str(text: &str) -> Result<Self, String> {
        match text {
            "starttls" => Ok(Self::StartTls),
            "tls" => Ok(Self::Tls),
            _ => Err("expected starttls or tls".into()),
        }
    }
}

/// Outgoing mail relay settings. The password never leaves this struct.
#[derive(Clone)]
pub struct MailConfig {
    pub host: String,
    pub port: u16,
    pub tls: SmtpTls,
    pub username: Option<String>,
    pub password: Option<String>,
    pub from: String,
}

impl MailConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(!self.host.trim().is_empty(), "smtp host must not be empty.");
        anyhow::ensure!(
            self.username.is_some() == self.password.is_some(),
            "smtp username and password must be set together."
        );
        anyhow::ensure!(
            lettre::message::Mailbox::from_str(&self.from).is_ok(),
            "mail from must be an address such as agentc@example.com."
        );
        Ok(())
    }
}

pub struct OutgoingMail {
    pub to: String,
    pub subject: String,
    pub body: String,
}

pub type MailFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

/// Hands one message to a relay. The error is for logs only and is never stored.
pub trait Mailer: Send + Sync {
    fn configured(&self) -> bool;
    fn send(&self, mail: OutgoingMail) -> MailFuture<'_>;
}

/// The mailer of an installation without a relay: every send fails.
pub struct NoMailer;

impl Mailer for NoMailer {
    fn configured(&self) -> bool {
        false
    }
    fn send(&self, _mail: OutgoingMail) -> MailFuture<'_> {
        Box::pin(async { Err("no mail relay is configured".into()) })
    }
}

pub struct SmtpMailer {
    transport: lettre::AsyncSmtpTransport<lettre::Tokio1Executor>,
    from: lettre::message::Mailbox,
}

impl SmtpMailer {
    pub fn new(config: &MailConfig) -> anyhow::Result<Self> {
        config.validate()?;
        let builder = match config.tls {
            SmtpTls::StartTls => {
                lettre::AsyncSmtpTransport::<lettre::Tokio1Executor>::starttls_relay(&config.host)?
            }
            SmtpTls::Tls => {
                lettre::AsyncSmtpTransport::<lettre::Tokio1Executor>::relay(&config.host)?
            }
        };
        let mut builder = builder.port(config.port).timeout(Some(SEND_TIMEOUT));
        if let (Some(user), Some(password)) = (&config.username, &config.password) {
            builder =
                builder.credentials(lettre::transport::smtp::authentication::Credentials::new(
                    user.clone(),
                    password.clone(),
                ));
        }
        Ok(Self {
            transport: builder.build(),
            from: config.from.parse()?,
        })
    }
}

impl Mailer for SmtpMailer {
    fn configured(&self) -> bool {
        true
    }
    fn send(&self, mail: OutgoingMail) -> MailFuture<'_> {
        Box::pin(async move {
            use lettre::AsyncTransport;
            let message = lettre::Message::builder()
                .from(self.from.clone())
                .to(mail
                    .to
                    .parse()
                    .map_err(|_| "invalid recipient".to_owned())?)
                .subject(mail.subject)
                .body(mail.body)
                .map_err(|error| error.to_string())?;
            self.transport
                .send(message)
                .await
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
    }
}

pub fn mailer_for(config: &Option<MailConfig>) -> anyhow::Result<Arc<dyn Mailer>> {
    Ok(match config {
        Some(config) => Arc::new(SmtpMailer::new(config)?),
        None => Arc::new(NoMailer),
    })
}

/// A normalized address, or a message for the person who typed it.
pub fn validate_email(text: &str) -> Result<String, AppError> {
    let email = text.trim();
    let invalid = || AppError::bad_request("Enter an email address such as name@example.com.");
    if !(3..=254).contains(&email.len()) || !email.is_ascii() {
        return Err(invalid());
    }
    let (local, domain) = email.rsplit_once('@').ok_or_else(invalid)?;
    let domain_ok = domain.contains('.')
        && !domain.starts_with(['.', '-'])
        && !domain.ends_with(['.', '-'])
        && !domain.contains("..")
        && domain
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-');
    if local.is_empty()
        || local.len() > 64
        || !domain_ok
        || email.bytes().any(|byte| !byte.is_ascii_graphic())
        || lettre::Address::from_str(email).is_err()
    {
        return Err(invalid());
    }
    Ok(email.to_owned())
}

/// A newly issued link. `token` is the only copy of the secret.
pub struct IssuedVerification {
    pub id: String,
    pub email: String,
    token: String,
}

/// Invalidate the account's unused links and record a new one, inside the
/// caller's transaction.
pub async fn issue_verification(
    c: &mut SqliteConnection,
    principal_id: &str,
    email: &str,
    now: i64,
) -> Result<IssuedVerification, AppError> {
    sqlx::query(
        "UPDATE email_verifications SET invalidated_at=? WHERE principal_id=? AND used_at IS NULL AND invalidated_at IS NULL",
    )
    .bind(now)
    .bind(principal_id)
    .execute(&mut *c)
    .await?;
    let id = uuid::Uuid::new_v4().to_string();
    let token = secret();
    sqlx::query("INSERT INTO email_verifications(id,principal_id,email,token_hash,created_at,expires_at) VALUES(?,?,?,?,?,?)")
        .bind(&id)
        .bind(principal_id)
        .bind(email)
        .bind(digest(&token))
        .bind(now)
        .bind(now + VERIFICATION_TTL_MS)
        .execute(&mut *c)
        .await?;
    Ok(IssuedVerification {
        id,
        email: email.to_owned(),
        token,
    })
}

fn verification_link(state: &AppState, token: &str) -> String {
    format!(
        "{}/api/v1/auth/email/verify?token={token}",
        state.config.public_origin
    )
}

/// Send the message for `issued` in the background and record the outcome. Call
/// only after the transaction that issued it has committed.
pub fn deliver_verification(state: &AppState, name: String, issued: IssuedVerification) {
    let state = state.clone();
    tokio::spawn(async move {
        let link = verification_link(&state, &issued.token);
        let mail = OutgoingMail {
            to: issued.email.clone(),
            subject: "Verify your email address".into(),
            body: format!(
                "Hello {name},\n\nOpen this link to verify the email address on your Agent Coordinator account:\n\n{link}\n\nThe link works once and expires in 24 hours. Requesting another verification email cancels this link.\n\nIf you did not expect this message, ignore it.\n"
            ),
        };
        let outcome = match state.mailer.send(mail).await {
            Ok(()) => None,
            Err(error) => {
                tracing::warn!(%error, "Verification email was not sent");
                Some(if state.mailer.configured() {
                    "send_failed"
                } else {
                    "mail_not_configured"
                })
            }
        };
        if let Err(error) = record_delivery(&state, &issued.id, outcome).await {
            tracing::warn!(code = %error.code, "Verification delivery outcome was not recorded");
        }
    });
}

async fn record_delivery(
    state: &AppState,
    id: &str,
    send_error: Option<&str>,
) -> Result<(), AppError> {
    let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    let clock = state.sample_clock(&mut tx).await?;
    if clock.incident_detected {
        tx.commit().await?;
        return Err(crate::state::clock_reconciliation_error());
    }
    sqlx::query("UPDATE email_verifications SET sent_at=?,send_error=? WHERE id=?")
        .bind(send_error.is_none().then_some(clock.now))
        .bind(send_error)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// The `email_verification` object of an account: what the owner sees and may do.
pub async fn verification_state(
    c: &mut SqliteConnection,
    principal_id: &str,
    now: i64,
) -> Result<Value, AppError> {
    let account = sqlx::query("SELECT email,email_verified_at FROM principals WHERE id=?")
        .bind(principal_id)
        .fetch_one(&mut *c)
        .await?;
    let Some(email) = account.get::<Option<String>, _>("email") else {
        return Ok(json!({"status":"missing"}));
    };
    if account.get::<Option<i64>, _>("email_verified_at").is_some() {
        return Ok(json!({"status":"verified"}));
    }
    let latest = sqlx::query("SELECT created_at,expires_at,invalidated_at,used_at,sent_at,send_error FROM email_verifications WHERE principal_id=? AND email=? ORDER BY created_at DESC,rowid DESC LIMIT 1")
        .bind(principal_id)
        .bind(&email)
        .fetch_optional(&mut *c)
        .await?;
    let Some(latest) = latest else {
        return Ok(json!({"status":"unverified","resend_available_at":timestamp(now)}));
    };
    let created_at: i64 = latest.get("created_at");
    let expires_at: i64 = latest.get("expires_at");
    Ok(json!({
        "status":"unverified",
        "last_issued_at":timestamp(created_at),
        "link_expires_at":timestamp(expires_at),
        "link_expired":expires_at <= now,
        "sent_at":latest.get::<Option<i64>,_>("sent_at").map(timestamp),
        "send_error":latest.get::<Option<String>,_>("send_error"),
        "resend_available_at":timestamp(created_at + RESEND_COOLDOWN_MS),
    }))
}

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/v1/auth/email/verify",
        get(verify_page).post(verify_email),
    )
}

/// True for the two requests the mailed link makes. They carry no credential:
/// the token in the link is the whole authority.
pub(crate) fn is_verify_link(method: &axum::http::Method, path: &str) -> bool {
    matches!(*method, axum::http::Method::GET | axum::http::Method::POST)
        && path == "/api/v1/auth/email/verify"
}

#[derive(Deserialize)]
struct VerifyQuery {
    token: String,
}

fn page(status: StatusCode, heading: &str, body: &str) -> Response {
    (
        status,
        Html(format!(
            "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
             <title>Agent Coordinator email verification</title></head><body><h1>{heading}</h1>{body}</body></html>"
        )),
    )
        .into_response()
}

fn expired_page() -> Response {
    page(
        // Not a 4xx: the router replaces non-JSON client errors with a JSON envelope.
        StatusCode::OK,
        "This link has expired",
        "<p>This verification link has expired, was already used, or was replaced by a newer email. Use the newest verification email, or request another under My account.</p><p><a href=\"/#account\">Open My account</a></p>",
    )
}

fn token_shaped(token: &str) -> bool {
    token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// `GET /api/v1/auth/email/verify?token=`: a page with one button. Mail scanners
/// and link previews fetch links without clicking, so only the POST verifies.
async fn verify_page(
    State(state): State<AppState>,
    Query(query): Query<VerifyQuery>,
) -> Result<Response, AppError> {
    if !token_shaped(&query.token) {
        return Ok(expired_page());
    }
    let mut c = state.pool.acquire().await?;
    if usable_verification(&mut c, &query.token, state.now())
        .await?
        .is_none()
    {
        return Ok(expired_page());
    }
    // A token is 64 hexadecimal digits, so it needs no escaping.
    Ok(page(
        StatusCode::OK,
        "Verify your email address",
        &format!(
            "<form method=\"post\" action=\"/api/v1/auth/email/verify\">\
             <input type=\"hidden\" name=\"token\" value=\"{}\">\
             <button type=\"submit\">Verify email address</button></form>",
            query.token
        ),
    ))
}

struct UsableVerification {
    id: String,
    principal_id: String,
    name: String,
}

/// The link's verification when it is unused, current, unexpired, and still
/// names the account's address and an enabled account.
async fn usable_verification(
    c: &mut SqliteConnection,
    token: &str,
    now: i64,
) -> Result<Option<UsableVerification>, AppError> {
    let row = sqlx::query(
        "SELECT v.id,v.principal_id,p.name FROM email_verifications v JOIN principals p ON p.id=v.principal_id \
         WHERE v.token_hash=? AND v.used_at IS NULL AND v.invalidated_at IS NULL AND v.expires_at>? \
         AND p.kind='human' AND p.disabled_at IS NULL AND p.email=v.email AND p.email_verified_at IS NULL",
    )
    .bind(digest(token))
    .bind(now)
    .fetch_optional(c)
    .await?;
    Ok(row.map(|row| UsableVerification {
        id: row.get("id"),
        principal_id: row.get("principal_id"),
        name: row.get("name"),
    }))
}

/// `POST /api/v1/auth/email/verify` with a form body `token=`: marks the
/// address verified. The token authorizes this one effect and nothing else.
async fn verify_email(State(state): State<AppState>, body: Bytes) -> Result<Response, AppError> {
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
    let usable = if token_shaped(&token) {
        usable_verification(&mut tx, &token, clock.now).await?
    } else {
        None
    };
    let Some(usable) = usable else {
        tx.commit().await?;
        return Ok(expired_page());
    };
    sqlx::query("UPDATE email_verifications SET used_at=? WHERE id=?")
        .bind(clock.now)
        .bind(&usable.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE email_verifications SET invalidated_at=? WHERE principal_id=? AND used_at IS NULL AND invalidated_at IS NULL")
        .bind(clock.now)
        .bind(&usable.principal_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE principals SET email_verified_at=?,revision=revision+1 WHERE id=?")
        .bind(clock.now)
        .bind(&usable.principal_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO events(project_id,actor_id,kind,record_id,data_json,created_at) VALUES(NULL,?,'operator_email_verified',?,'{}',?)")
        .bind(&usable.principal_id)
        .bind(&usable.principal_id)
        .bind(clock.now)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    tracing::info!(account = %usable.name, "Account email address verified");
    Ok(page(
        StatusCode::OK,
        "Email address verified",
        "<p>Your email address is verified. Sign in and open My account to change your password or review your account.</p><p><a href=\"/#account\">Open My account</a></p>",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_checked_and_trimmed() {
        assert_eq!(
            validate_email(" a.b+c@example.com ").unwrap(),
            "a.b+c@example.com"
        );
        for bad in [
            "",
            "plain",
            "@example.com",
            "a@",
            "a@example",
            "a@@example.com",
            "a b@example.com",
            "a@exa mple.com",
            "a@example..com",
            "a@-example.com",
            "é@example.com",
        ] {
            assert!(validate_email(bad).is_err(), "{bad:?}");
        }
        assert!(validate_email(&format!("{}@example.com", "a".repeat(65))).is_err());
    }

    #[test]
    fn a_token_is_sixty_four_hex_digits() {
        assert!(token_shaped(&secret()));
        assert!(!token_shaped("abc"));
        assert!(!token_shaped(&"g".repeat(64)));
    }
}
