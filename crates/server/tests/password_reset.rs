use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use coordinator_server::{
    account_email::{MailFuture, Mailer, OutgoingMail},
    auth::init_admin,
    router,
    state::{AppState, Clock, Config},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};
use tower::ServiceExt;

const ORIGIN: &str = "https://coordinator.example.test";
const PASSWORD: &str = "initial administrator password";
const BASE: i64 = 1_788_976_800_000;

struct TestClock(Arc<AtomicI64>);
impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    fn use_monotonic_elapsed(&self) -> bool {
        false
    }
}

#[derive(Clone, Default)]
struct Inbox(Arc<Mutex<Vec<(String, String)>>>);

impl Mailer for Inbox {
    fn configured(&self) -> bool {
        true
    }
    fn send(&self, mail: OutgoingMail) -> MailFuture<'_> {
        self.0.lock().unwrap().push((mail.to, mail.body));
        Box::pin(async { Ok(()) })
    }
}

impl Inbox {
    /// Every token in the `n`th message's links, waiting for the background send.
    async fn tokens(&self, n: usize) -> (String, Vec<String>) {
        for _ in 0..200 {
            if let Some((to, body)) = self.0.lock().unwrap().get(n).cloned() {
                let tokens = body
                    .match_indices("token=")
                    .map(|(at, _)| body[at + 6..at + 70].to_owned())
                    .collect::<Vec<_>>();
                assert!(!tokens.is_empty(), "link in message");
                return (to, tokens);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("message {n} was not sent");
    }
    async fn token(&self, n: usize) -> String {
        self.tokens(n).await.1.remove(0)
    }
    fn count(&self) -> usize {
        self.0.lock().unwrap().len()
    }
}

struct Fixture {
    state: AppState,
    inbox: Inbox,
    time: Arc<AtomicI64>,
    _directory: tempfile::TempDir,
}

struct Reply {
    status: StatusCode,
    body: Value,
    text: String,
    cookie: Option<String>,
}

#[derive(Clone)]
struct Browser {
    cookie: String,
    csrf: String,
}

impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut state = AppState::open(Config {
            database_path: directory.path().join("account-email.sqlite3"),
            public_origin: ORIGIN.into(),
            ..Config::default()
        })
        .await
        .unwrap();
        let time = Arc::new(AtomicI64::new(BASE));
        state.clock = Arc::new(TestClock(time.clone()));
        let inbox = Inbox::default();
        state.mailer = Arc::new(inbox.clone());
        init_admin(&state, "admin", PASSWORD.into()).await.unwrap();
        Self {
            state,
            inbox,
            time,
            _directory: directory,
        }
    }

    async fn raw(&self, request: Request<Body>) -> Reply {
        let result = router(self.state.clone()).oneshot(request).await.unwrap();
        let status = result.status();
        let cookie = result.headers().get("set-cookie").map(|value| {
            value
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_owned()
        });
        let text = String::from_utf8(
            result
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        Reply {
            status,
            body: serde_json::from_str(&text).unwrap_or(Value::Null),
            text,
            cookie,
        }
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        browser: Option<(&Browser, &str)>,
        body: Option<Value>,
    ) -> Reply {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .header("origin", ORIGIN);
        if let Some((browser, key)) = browser {
            request = request
                .header("cookie", &browser.cookie)
                .header("x-csrf-token", &browser.csrf)
                .header("idempotency-key", key);
        }
        self.raw(
            request
                .body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
                .unwrap(),
        )
        .await
    }

    async fn login(&self, name: &str, password: &str) -> Browser {
        let reply = self
            .call(
                "POST",
                "/api/v1/auth/login",
                None,
                Some(json!({"username":name,"password":password})),
            )
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
        Browser {
            cookie: reply.cookie.unwrap(),
            csrf: reply.body["data"]["csrf_token"]
                .as_str()
                .unwrap()
                .to_owned(),
        }
    }

    async fn account(&self, browser: &Browser) -> Value {
        let request = Request::builder()
            .uri("/api/v1/auth/account")
            .header("cookie", &browser.cookie)
            .body(Body::empty())
            .unwrap();
        let reply = self.raw(request).await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
        reply.body["data"].clone()
    }

    async fn open_link(&self, token: &str) -> Reply {
        let request = Request::builder()
            .uri(format!("/api/v1/auth/password/reset?token={token}"))
            .body(Body::empty())
            .unwrap();
        self.raw(request).await
    }

    async fn form(&self, path: &str, body: String) -> Reply {
        let request = Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap();
        self.raw(request).await
    }

    async fn submit(&self, token: &str, password: &str, confirmation: &str) -> Reply {
        self.form(
            "/api/v1/auth/password/reset",
            format!("token={token}&password={password}&confirmation={confirmation}"),
        )
        .await
    }

    async fn verify(&self, n: usize) {
        let token = self.inbox.token(n).await;
        let reply = self
            .form("/api/v1/auth/email/verify", format!("token={token}"))
            .await;
        assert!(reply.text.contains("verified"), "{}", reply.text);
    }

    async fn forgot(&self, email: &str) -> Reply {
        self.call(
            "POST",
            "/api/v1/auth/password/forgot",
            None,
            Some(json!({"email":email})),
        )
        .await
    }

    async fn quiet(&self, expected: usize) {
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(self.inbox.count(), expected);
    }
}

async fn create_operator(f: &Fixture, admin: &Browser, name: &str, email: &str) -> Reply {
    f.call(
        "POST",
        "/api/v1/admin/operators",
        Some((admin, &format!("create-{name}"))),
        Some(json!({"name":name,"role":"operator","email":email,"password":"operator test password"})),
    )
    .await
}

const OLD: &str = "operator test password";
const NEW: &str = "a brand new password";

async fn verified_tester(f: &Fixture, admin: &Browser, name: &str, email: &str, n: usize) {
    let created = create_operator(f, admin, name, email).await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.text);
    f.verify(n).await;
}

async fn login_status(f: &Fixture, name: &str, password: &str) -> StatusCode {
    f.call(
        "POST",
        "/api/v1/auth/login",
        None,
        Some(json!({"username":name,"password":password})),
    )
    .await
    .status
}

#[tokio::test]
async fn a_forgotten_password_is_reset_through_a_mailed_single_use_link() {
    let f = Fixture::new().await;
    let admin = f.login("admin", PASSWORD).await;
    verified_tester(&f, &admin, "tester", "Tester@Example.test", 0).await;
    let session = f.login("tester", OLD).await;

    // The address is matched without regard to case; the reply is generic.
    let asked = f.forgot("tester@example.test").await;
    assert_eq!(asked.status, StatusCode::OK, "{}", asked.text);
    assert_eq!(asked.body["data"]["requested"], true);
    let (to, tokens) = f.inbox.tokens(1).await;
    assert_eq!(to, "tester@example.test");
    assert_eq!(tokens.len(), 1);
    let token = &tokens[0];
    let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM password_resets WHERE token_hash=?")
        .bind(token)
        .fetch_one(&f.state.pool)
        .await
        .unwrap();
    assert_eq!(stored, 0, "only a hash of the token is stored");

    // Opening the link (as a mail scanner would) changes nothing.
    let page = f.open_link(token).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.text.contains("<form"), "{}", page.text);
    assert!(page.text.contains("tester"));

    // Bad submissions re-show the form and keep the link usable.
    let mismatch = f.submit(token, NEW, "something else entirely").await;
    assert!(mismatch.text.contains("do not match"), "{}", mismatch.text);
    let short = f.submit(token, "short", "short").await;
    assert!(short.text.contains("12 to 1024"), "{}", short.text);

    let done = f.submit(token, NEW, NEW).await;
    assert!(done.text.contains("Password changed"), "{}", done.text);
    assert_eq!(
        login_status(&f, "tester", OLD).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(login_status(&f, "tester", NEW).await, StatusCode::OK);
    // Every earlier browser session was signed out.
    let request = Request::builder()
        .uri("/api/v1/auth/account")
        .header("cookie", &session.cookie)
        .body(Body::empty())
        .unwrap();
    assert_eq!(f.raw(request).await.status, StatusCode::UNAUTHORIZED);

    // The link works once.
    assert!(f.open_link(token).await.text.contains("has expired"));
    let again = f
        .submit(token, "another new password", "another new password")
        .await;
    assert!(again.text.contains("has expired"), "{}", again.text);
    assert_eq!(login_status(&f, "tester", NEW).await, StatusCode::OK);
}

#[tokio::test]
async fn the_reply_is_the_same_and_no_mail_goes_to_unknown_or_unverified_addresses() {
    let f = Fixture::new().await;
    let admin = f.login("admin", PASSWORD).await;
    // Created but never verified.
    create_operator(&f, &admin, "pending", "pending@example.test").await;
    f.inbox.token(0).await;

    let known = f.forgot("pending@example.test").await;
    let unknown = f.forgot("nobody@example.test").await;
    assert_eq!(known.status, StatusCode::OK);
    assert_eq!(known.body["data"], unknown.body["data"]);
    f.quiet(1).await;

    let malformed = f.forgot("not-an-address").await;
    assert_eq!(malformed.status, StatusCode::BAD_REQUEST);
    let foreign = f
        .raw(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/password/forgot")
                .header("content-type", "application/json")
                .header("origin", "https://elsewhere.example.test")
                .body(Body::from(
                    json!({"email":"pending@example.test"}).to_string(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(foreign.status, StatusCode::FORBIDDEN, "{}", foreign.text);
    f.quiet(1).await;
}

#[tokio::test]
async fn a_link_expires_after_one_hour() {
    let f = Fixture::new().await;
    let admin = f.login("admin", PASSWORD).await;
    verified_tester(&f, &admin, "tester", "tester@example.test", 0).await;

    f.forgot("tester@example.test").await;
    let late = f.inbox.token(1).await;
    f.time.fetch_add(60 * 60 * 1000 - 1, Ordering::SeqCst);
    assert!(f.open_link(&late).await.text.contains("<form"));
    f.time.fetch_add(1, Ordering::SeqCst);
    assert!(f.open_link(&late).await.text.contains("has expired"));
    let refused = f.submit(&late, NEW, NEW).await;
    assert!(refused.text.contains("has expired"), "{}", refused.text);
    assert_eq!(login_status(&f, "tester", OLD).await, StatusCode::OK);
}

#[tokio::test]
async fn a_newer_link_replaces_the_older_one_and_requests_are_spaced_a_minute_apart() {
    let f = Fixture::new().await;
    let admin = f.login("admin", PASSWORD).await;
    verified_tester(&f, &admin, "tester", "tester@example.test", 0).await;

    f.forgot("tester@example.test").await;
    let first = f.inbox.token(1).await;
    // Within the minute the request is accepted but sends nothing.
    assert_eq!(f.forgot("tester@example.test").await.status, StatusCode::OK);
    f.quiet(2).await;
    assert!(f.open_link(&first).await.text.contains("<form"));

    f.time.fetch_add(61_000, Ordering::SeqCst);
    f.forgot("tester@example.test").await;
    let second = f.inbox.token(2).await;
    assert_ne!(first, second);
    assert!(f.open_link(&first).await.text.contains("has expired"));
    assert!(f.open_link(&second).await.text.contains("<form"));
    let done = f.submit(&second, NEW, NEW).await;
    assert!(done.text.contains("Password changed"), "{}", done.text);
}

#[tokio::test]
async fn a_link_dies_when_the_address_changes_or_the_account_is_disabled() {
    let f = Fixture::new().await;
    let admin = f.login("admin", PASSWORD).await;
    verified_tester(&f, &admin, "tester", "tester@example.test", 0).await;
    verified_tester(&f, &admin, "other", "other@example.test", 1).await;

    f.forgot("tester@example.test").await;
    let changed = f.inbox.token(2).await;
    let tester = f.login("tester", OLD).await;
    let account = f.account(&tester).await;
    let moved = f
        .call(
            "POST",
            "/api/v1/auth/email",
            Some((&tester, "change-address")),
            Some(json!({
                "expected_revision":account["operator"]["revision"],
                "email":"moved@example.test"
            })),
        )
        .await;
    assert_eq!(moved.status, StatusCode::OK, "{}", moved.text);
    assert!(f.open_link(&changed).await.text.contains("has expired"));
    // The new address is unverified, so it cannot be used for recovery yet.
    f.forgot("moved@example.test").await;
    f.quiet(4).await;

    f.forgot("other@example.test").await;
    let disabled = f.inbox.token(4).await;
    let operators = f
        .call(
            "GET",
            "/api/v1/admin/operators",
            Some((&admin, "list")),
            None,
        )
        .await;
    let other = operators.body["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|operator| operator["name"] == "other")
        .unwrap()
        .clone();
    let off = f
        .call(
            "POST",
            &format!(
                "/api/v1/admin/operators/{}/access",
                other["id"].as_str().unwrap()
            ),
            Some((&admin, "disable-other")),
            Some(json!({"expected_revision":other["revision"],"role":"operator","enabled":false})),
        )
        .await;
    assert_eq!(off.status, StatusCode::OK, "{}", off.text);
    assert!(f.open_link(&disabled).await.text.contains("has expired"));
    let refused = f.submit(&disabled, NEW, NEW).await;
    assert!(refused.text.contains("has expired"), "{}", refused.text);
    assert_eq!(
        login_status(&f, "other", NEW).await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn accounts_sharing_an_address_each_get_their_own_link() {
    let f = Fixture::new().await;
    let admin = f.login("admin", PASSWORD).await;
    verified_tester(&f, &admin, "alpha", "shared@example.test", 0).await;
    verified_tester(&f, &admin, "beta", "shared@example.test", 1).await;

    f.forgot("shared@example.test").await;
    let (_, tokens) = f.inbox.tokens(2).await;
    assert_eq!(tokens.len(), 2);
    f.quiet(3).await;
    let page = f.open_link(&tokens[1]).await;
    assert!(page.text.contains("beta"), "{}", page.text);
    let done = f.submit(&tokens[1], NEW, NEW).await;
    assert!(done.text.contains("Password changed"), "{}", done.text);
    assert_eq!(login_status(&f, "beta", NEW).await, StatusCode::OK);
    assert_eq!(login_status(&f, "alpha", OLD).await, StatusCode::OK);
    // The other account's link is untouched.
    assert!(f.open_link(&tokens[0]).await.text.contains("alpha"));
}

#[tokio::test]
async fn malformed_tokens_are_treated_as_expired() {
    let f = Fixture::new().await;
    for token in ["", "abc", &"z".repeat(64), &"0".repeat(64)] {
        assert!(f.open_link(token).await.text.contains("has expired"));
        let reply = f.submit(token, NEW, NEW).await;
        assert!(reply.text.contains("has expired"), "{}", reply.text);
    }
}
