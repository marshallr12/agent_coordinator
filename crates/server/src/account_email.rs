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
    pub html_body: String,
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
                .multipart(lettre::message::MultiPart::alternative_plain_html(
                    mail.body,
                    mail.html_body,
                ))
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

/// Escape both text and double-quoted attributes in an HTML message.
pub(crate) fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn verification_mail(name: &str, email: String, link: &str) -> OutgoingMail {
    OutgoingMail {
        to: email,
        subject: "Verify your email address".into(),
        body: format!(
            "Hello {name},\n\nOpen this link to verify the email address on your Agent Coordinator account:\n\n{link}\n\nThe link works once and expires in 24 hours. Requesting another verification email cancels this link.\n\nIf you did not expect this message, ignore it.\n"
        ),
        html_body: format!(
            "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
             <title>Verify your email address</title></head><body>\
             <p>Hello {},</p>\
             <p>Verify the email address on your Agent Coordinator account:</p>\
             <p><a href=\"{}\">Verify email address</a></p>\
             <p>The link works once and expires in 24 hours. Requesting another verification email cancels this link.</p>\
             <p>If you did not expect this message, ignore it.</p></body></html>",
            escape_html(name),
            escape_html(link),
        ),
    }
}

/// Send the message for `issued` in the background and record the outcome. Call
/// only after the transaction that issued it has committed.
pub fn deliver_verification(state: &AppState, name: String, issued: IssuedVerification) {
    let state = state.clone();
    tokio::spawn(async move {
        let link = verification_link(&state, &issued.token);
        let mail = verification_mail(&name, issued.email.clone(), &link);
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
        get(verify_page)
            .post(verify_email)
            .head(|| async { StatusCode::OK }),
    )
}

/// True for verification requests. They carry no credential:
/// the token in the link is the whole authority.
pub(crate) fn is_verify_link(method: &axum::http::Method, path: &str) -> bool {
    matches!(
        *method,
        axum::http::Method::GET | axum::http::Method::POST | axum::http::Method::HEAD
    ) && path == "/api/v1/auth/email/verify"
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

/// Opening the mailed link verifies its address without another action.
async fn verify_page(
    State(state): State<AppState>,
    Query(query): Query<VerifyQuery>,
) -> Result<Response, AppError> {
    consume_verification(&state, &query.token).await
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
    consume_verification(&state, &token).await
}

/// GET and legacy POST share the writer-locked, single-use transition.
async fn consume_verification(state: &AppState, token: &str) -> Result<Response, AppError> {
    let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    let clock = state.sample_clock(&mut tx).await?;
    if clock.incident_detected {
        tx.commit().await?;
        return Err(crate::state::clock_reconciliation_error());
    }
    if clock.incident_active {
        return Err(crate::state::clock_reconciliation_error());
    }
    let usable = if token_shaped(token) {
        usable_verification(&mut tx, token, clock.now).await?
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
pub(crate) mod tests {
    use super::*;

    /// Exercise the real SMTP sender, including its wire encoding, without an
    /// external relay. Both futures are bounded and dropped together on timeout.
    pub(crate) async fn smtp_bodies(mail: OutgoingMail) -> (String, String) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mailer = SmtpMailer {
            transport: lettre::AsyncSmtpTransport::builder_dangerous("127.0.0.1")
                .port(listener.local_addr().unwrap().port())
                .build(),
            from: "sender@example.test".parse().unwrap(),
        };
        let subject = mail.subject.clone();
        let expected_text = mail.body.clone();
        let expected_html = mail.html_body.clone();
        let capture = async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            stream
                .get_mut()
                .write_all(b"220 localhost test\r\n")
                .await
                .unwrap();
            let mut raw = Vec::new();
            let mut data = false;
            loop {
                let mut line = Vec::new();
                if stream.read_until(b'\n', &mut line).await.unwrap() == 0 {
                    break;
                }
                assert!(
                    raw.len() + line.len() < 128 * 1024,
                    "SMTP fixture size bound"
                );
                let reply: &[u8] = if data {
                    if line == b".\r\n" {
                        data = false;
                        b"250 message accepted\r\n"
                    } else {
                        raw.extend_from_slice(if line.starts_with(b"..") {
                            &line[1..]
                        } else {
                            &line
                        });
                        continue;
                    }
                } else if line.starts_with(b"EHLO ") {
                    b"250-localhost\r\n250 8BITMIME\r\n"
                } else if line.starts_with(b"MAIL FROM:")
                    || line.starts_with(b"RCPT TO:")
                    || line == b"RSET\r\n"
                {
                    b"250 ok\r\n"
                } else if line == b"DATA\r\n" {
                    data = true;
                    b"354 send message\r\n"
                } else if line == b"QUIT\r\n" {
                    stream.get_mut().write_all(b"221 bye\r\n").await.unwrap();
                    break;
                } else {
                    panic!("unexpected SMTP fixture command");
                };
                stream.get_mut().write_all(reply).await.unwrap();
            }
            assert!(!data, "SMTP DATA must complete");
            raw
        };
        let (sent, raw) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(mailer.send(mail), capture)
        })
        .await
        .expect("SMTP send/capture must finish within five seconds");
        sent.unwrap();
        let parsed = mail_parser::MessageParser::default().parse(&raw).unwrap();
        assert_eq!(parsed.subject(), Some(subject.as_str()));
        assert_eq!(parsed.header_raw("MIME-Version").unwrap().trim(), "1.0");
        assert!(
            parsed
                .header_raw("Content-Type")
                .unwrap()
                .trim()
                .starts_with("multipart/alternative")
        );
        assert_eq!(parsed.text_body.len(), 1);
        assert_eq!(parsed.html_body.len(), 1);
        assert!(matches!(
            &parsed.text_part(0).unwrap().body,
            mail_parser::PartType::Text(_)
        ));
        assert!(matches!(
            &parsed.html_part(0).unwrap().body,
            mail_parser::PartType::Html(_)
        ));
        let text = parsed.body_text(0).unwrap().replace("\r\n", "\n");
        let html = parsed.body_html(0).unwrap().replace("\r\n", "\n");
        assert_eq!(text, expected_text);
        assert_eq!(html, expected_html);
        (text, html)
    }

    #[tokio::test]
    async fn verification_smtp_mime_preserves_the_link_and_escapes_html() {
        let token = "0123456789abcdef".repeat(4);
        let link =
            format!("https://coordinator.example.test/api/v1/auth/email/verify?token={token}");
        let name = "Renée & <img src=x onerror=\"alert('x')\">";
        let (text, html) =
            smtp_bodies(verification_mail(name, "reader@example.test".into(), &link)).await;
        assert!(text.contains(&link));
        assert!(text.contains(name));
        assert!(html.contains(&format!("<a href=\"{link}\">Verify email address</a>")));
        assert!(
            html.contains("Renée &amp; &lt;img src=x onerror=&quot;alert(&#39;x&#39;)&quot;&gt;")
        );
        assert!(!html.contains("<img"));
        assert!(!text.contains("token=3D"));
        assert!(!html.contains("token=3D"));
    }

    #[test]
    fn mail_attribute_escaping_preserves_quoted_and_ampersand_characters() {
        let mail = verification_mail(
            "reader",
            "reader@example.test".into(),
            "https://example.test/?a=1&b=\"'<> ",
        );
        assert!(
            mail.html_body
                .contains("href=\"https://example.test/?a=1&amp;b=&quot;&#39;&lt;&gt; \"")
        );
    }

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
