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
use sqlx::Row;
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
    /// The token in the `n`th message's link, waiting for the background send.
    async fn token(&self, n: usize) -> (String, String) {
        for _ in 0..200 {
            if let Some((to, body)) = self.0.lock().unwrap().get(n).cloned() {
                let start = body.find("token=").expect("link in message") + "token=".len();
                return (to, body[start..start + 64].to_owned());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("message {n} was not sent");
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
            .uri(format!("/api/v1/auth/email/verify?token={token}"))
            .body(Body::empty())
            .unwrap();
        self.raw(request).await
    }

    async fn click_verify(&self, token: &str) -> Reply {
        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/auth/email/verify")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(format!("token={token}")))
            .unwrap();
        self.raw(request).await
    }

    /// Wait until the background send of the newest message is recorded.
    async fn settled(&self, browser: &Browser) -> Value {
        for _ in 0..200 {
            let data = self.account(browser).await;
            let state = &data["email_verification"];
            if !state["sent_at"].is_null() || !state["send_error"].is_null() {
                return data;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("delivery outcome was not recorded");
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

#[tokio::test]
async fn a_new_account_needs_an_email_and_gets_a_verification_link() {
    let f = Fixture::new().await;
    let admin = f.login("admin", PASSWORD).await;
    let missing = f
        .call(
            "POST",
            "/api/v1/admin/operators",
            Some((&admin, "no-email")),
            Some(json!({"name":"nobody","role":"operator","password":"operator test password"})),
        )
        .await;
    assert_ne!(missing.status, StatusCode::OK, "{}", missing.text);
    let bad = create_operator(&f, &admin, "nobody", "not-an-address").await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST, "{}", bad.text);
    assert_eq!(f.inbox.count(), 0);

    let created = create_operator(&f, &admin, "tester", "tester@example.test").await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.text);
    let operator = &created.body["data"]["operator"];
    assert_eq!(operator["email"], "tester@example.test");
    assert_eq!(operator["email_verified"], false);
    let (to, token) = f.inbox.token(0).await;
    assert_eq!(to, "tester@example.test");
    // Only a hash of the token is stored.
    let stored: i64 =
        sqlx::query_scalar("SELECT count(*) FROM email_verifications WHERE token_hash=?")
            .bind(&token)
            .fetch_one(&f.state.pool)
            .await
            .unwrap();
    assert_eq!(stored, 0);

    // A replayed creation does not send another message.
    let replay = create_operator(&f, &admin, "tester", "tester@example.test").await;
    assert_eq!(replay.status, StatusCode::OK, "{}", replay.text);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(f.inbox.count(), 1);

    let tester = f.login("tester", "operator test password").await;
    let data = f.settled(&tester).await;
    assert_eq!(data["operator"]["email_verified"], false);
    assert_eq!(data["email_verification"]["status"], "unverified");
    assert_eq!(data["mail_configured"], true);
    assert!(data["email_verification"]["send_error"].is_null());
}

#[tokio::test]
async fn resending_invalidates_the_earlier_link_and_the_later_one_verifies() {
    let f = Fixture::new().await;
    let admin = f.login("admin", PASSWORD).await;
    create_operator(&f, &admin, "tester", "tester@example.test").await;
    let tester = f.login("tester", "operator test password").await;
    let (_, first) = f.inbox.token(0).await;
    f.settled(&tester).await;

    let too_soon = f
        .call(
            "POST",
            "/api/v1/auth/email/resend",
            Some((&tester, "resend-1")),
            Some(json!({})),
        )
        .await;
    assert_eq!(too_soon.status, StatusCode::CONFLICT, "{}", too_soon.text);
    assert_eq!(
        too_soon.body["error"]["code"],
        "verification_resend_too_soon"
    );
    assert_eq!(f.inbox.count(), 1);

    f.time.fetch_add(61_000, Ordering::SeqCst);
    let resent = f
        .call(
            "POST",
            "/api/v1/auth/email/resend",
            Some((&tester, "resend-2")),
            Some(json!({})),
        )
        .await;
    assert_eq!(resent.status, StatusCode::OK, "{}", resent.text);
    let (_, second) = f.inbox.token(1).await;
    assert_ne!(first, second);

    // The first link is dead at once: no button, and posting it changes nothing.
    let page = f.open_link(&first).await;
    assert!(page.text.contains("has expired"), "{}", page.text);
    assert!(!page.text.contains("<form"));
    let posted = f.click_verify(&first).await;
    assert!(posted.text.contains("has expired"), "{}", posted.text);
    assert_eq!(
        f.account(&tester).await["operator"]["email_verified"],
        false
    );

    // Opening a link does not verify; only the button's POST does.
    let page = f.open_link(&second).await;
    assert!(page.text.contains("<form"), "{}", page.text);
    assert_eq!(
        f.account(&tester).await["operator"]["email_verified"],
        false
    );
    let done = f.click_verify(&second).await;
    assert_eq!(done.status, StatusCode::OK);
    assert!(
        done.text.contains("Email address verified"),
        "{}",
        done.text
    );
    let data = f.account(&tester).await;
    assert_eq!(data["operator"]["email_verified"], true);
    assert_eq!(data["email_verification"]["status"], "verified");
    // The link works once.
    assert!(f.click_verify(&second).await.text.contains("has expired"));
    let again = f
        .call(
            "POST",
            "/api/v1/auth/email/resend",
            Some((&tester, "resend-3")),
            Some(json!({})),
        )
        .await;
    assert_eq!(again.body["error"]["code"], "email_already_verified");
}

#[tokio::test]
async fn links_expire_and_changing_the_address_replaces_them() {
    let f = Fixture::new().await;
    let admin = f.login("admin", PASSWORD).await;
    let none = f.account(&admin).await;
    assert_eq!(none["email_verification"]["status"], "missing");
    let revision = none["operator"]["revision"].as_i64().unwrap();
    let set = f
        .call(
            "POST",
            "/api/v1/auth/email",
            Some((&admin, "set-1")),
            Some(json!({"expected_revision":revision,"email":"first@example.test"})),
        )
        .await;
    assert_eq!(set.status, StatusCode::OK, "{}", set.text);
    let (to, first) = f.inbox.token(0).await;
    assert_eq!(to, "first@example.test");
    f.settled(&admin).await;

    let stale = f
        .call(
            "POST",
            "/api/v1/auth/email",
            Some((&admin, "set-stale")),
            Some(json!({"expected_revision":revision,"email":"second@example.test"})),
        )
        .await;
    assert_eq!(stale.body["error"]["code"], "revision_conflict");
    let same = f
        .call(
            "POST",
            "/api/v1/auth/email",
            Some((&admin, "set-same")),
            Some(json!({"expected_revision":revision + 1,"email":"FIRST@example.test"})),
        )
        .await;
    assert_eq!(same.body["error"]["code"], "email_unchanged");

    let changed = f
        .call(
            "POST",
            "/api/v1/auth/email",
            Some((&admin, "set-2")),
            Some(json!({"expected_revision":revision + 1,"email":"second@example.test"})),
        )
        .await;
    assert_eq!(changed.status, StatusCode::OK, "{}", changed.text);
    let (to, second) = f.inbox.token(1).await;
    assert_eq!(to, "second@example.test");
    assert!(f.click_verify(&first).await.text.contains("has expired"));

    f.time.fetch_add(24 * 60 * 60 * 1000 + 1, Ordering::SeqCst);
    assert!(f.open_link(&second).await.text.contains("has expired"));
    assert!(f.click_verify(&second).await.text.contains("has expired"));
    // The browser session lapsed with the day; sign in again.
    let admin = f.login("admin", PASSWORD).await;
    assert_eq!(f.account(&admin).await["operator"]["email_verified"], false);
    let resent = f
        .call(
            "POST",
            "/api/v1/auth/email/resend",
            Some((&admin, "resend-late")),
            Some(json!({})),
        )
        .await;
    assert_eq!(resent.status, StatusCode::OK, "{}", resent.text);
    let (_, third) = f.inbox.token(2).await;
    assert!(
        f.click_verify(&third)
            .await
            .text
            .contains("Email address verified")
    );
}

#[tokio::test]
async fn an_installation_without_a_relay_records_that_nothing_was_sent() {
    let mut f = Fixture::new().await;
    f.state.mailer = Arc::new(coordinator_server::account_email::NoMailer);
    let admin = f.login("admin", PASSWORD).await;
    create_operator(&f, &admin, "tester", "tester@example.test").await;
    let tester = f.login("tester", "operator test password").await;
    let data = f.settled(&tester).await;
    assert_eq!(data["mail_configured"], false);
    assert_eq!(
        data["email_verification"]["send_error"],
        "mail_not_configured"
    );
    assert!(data["email_verification"]["sent_at"].is_null());
}

#[tokio::test]
async fn malformed_tokens_are_expired_pages() {
    let f = Fixture::new().await;
    for token in ["", "abc", &"z".repeat(64), &"0".repeat(64)] {
        assert!(
            f.open_link(token).await.text.contains("has expired"),
            "{token}"
        );
        assert!(
            f.click_verify(token).await.text.contains("has expired"),
            "{token}"
        );
    }
}

#[tokio::test]
async fn the_migration_marks_existing_administrators_verified() {
    let f = Fixture::new().await;
    let admin = f.login("admin", PASSWORD).await;
    create_operator(&f, &admin, "tester", "tester@example.test").await;
    // Re-run the migration's backfill against an installation that already has
    // an administrator without an address, an operator, and an agent.
    sqlx::query("UPDATE principals SET email=NULL,email_verified_at=NULL WHERE name='admin'")
        .execute(&f.state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO principals(id,name,kind,role,created_at,revision) VALUES('agent-1','an-agent','agent','agent',1,1)")
        .execute(&f.state.pool)
        .await
        .unwrap();
    let script = include_str!("../migrations/0042_account_email.sql");
    let backfill = &script[script.find("UPDATE principals").unwrap()..];
    sqlx::query(backfill).execute(&f.state.pool).await.unwrap();
    let rows = sqlx::query("SELECT name,email,email_verified_at FROM principals ORDER BY name")
        .fetch_all(&f.state.pool)
        .await
        .unwrap();
    let find = |name: &str| {
        rows.iter()
            .find(|row| row.get::<String, _>("name") == name)
            .unwrap()
    };
    assert_eq!(
        find("admin").get::<Option<String>, _>("email").as_deref(),
        Some("marshall.rosenstein@gmail.com")
    );
    assert!(
        find("admin")
            .get::<Option<i64>, _>("email_verified_at")
            .is_some()
    );
    assert_eq!(
        find("tester").get::<Option<String>, _>("email").as_deref(),
        Some("tester@example.test")
    );
    assert!(
        find("tester")
            .get::<Option<i64>, _>("email_verified_at")
            .is_none()
    );
    assert!(find("an-agent").get::<Option<String>, _>("email").is_none());
}
