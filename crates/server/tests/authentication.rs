use axum::{
    body::Body,
    extract::FromRequestParts,
    http::{HeaderMap, Request, StatusCode},
};
use coordinator_server::{
    auth::{Auth, digest, init_admin, secret},
    router,
    state::{AppState, Clock, Config},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::Row;
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};
use tower::ServiceExt;

const ORIGIN: &str = "https://coordinator.example.test";
const PASSWORD: &str = "a lengthy test-only administrator password";

struct TestClock(AtomicI64);
impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}
struct Fixture {
    state: AppState,
    clock: Arc<TestClock>,
    _dir: tempfile::TempDir,
}
impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            database_path: dir.path().join("test.sqlite3"),
            public_origin: ORIGIN.into(),
            ..Config::default()
        };
        let mut state = AppState::open(config).await.unwrap();
        let clock = Arc::new(TestClock(AtomicI64::new(1_788_976_800_000)));
        state.clock = clock.clone();
        init_admin(&state, "admin", PASSWORD.into()).await.unwrap();
        Self {
            state,
            clock,
            _dir: dir,
        }
    }
    async fn call(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<Value>,
    ) -> Reply {
        self.raw(
            method,
            path,
            headers,
            body.map(|value| value.to_string()).unwrap_or_default(),
        )
        .await
    }
    async fn raw(&self, method: &str, path: &str, headers: &[(&str, &str)], body: String) -> Reply {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let result = router(self.state.clone())
            .oneshot(request.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        let status = result.status();
        let headers = result.headers().clone();
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
        let body = serde_json::from_str(&text).unwrap_or(Value::Null);
        Reply {
            status,
            headers,
            body,
            text,
        }
    }
    async fn login(&self) -> Browser {
        let result = self
            .call(
                "POST",
                "/api/v1/auth/login",
                &[("origin", ORIGIN)],
                Some(json!({"username":"admin","password":PASSWORD})),
            )
            .await;
        result.ok();
        let set_cookie = result.headers["set-cookie"].to_str().unwrap();
        assert!(set_cookie.contains("Secure"));
        assert!(set_cookie.contains("HttpOnly"));
        assert!(set_cookie.contains("SameSite=Strict"));
        Browser {
            cookie: set_cookie.split(';').next().unwrap().into(),
            csrf: result.body["data"]["csrf_token"].as_str().unwrap().into(),
        }
    }
    async fn issue(&self, browser: &Browser, name: &str) -> (String, String) {
        let result = self
            .call(
                "POST",
                "/api/v1/admin/agents",
                &browser.headers(name),
                Some(json!({"name":name})),
            )
            .await;
        result.ok();
        (
            result.body["data"]["token"].as_str().unwrap().into(),
            result.body["data"]["credential_id"]
                .as_str()
                .unwrap()
                .into(),
        )
    }
    async fn register(&self, token: &str, session: &str, proof: &str, key: &str) -> Reply {
        self.call("POST", "/api/v1/sessions", &[("authorization", &format!("Bearer {token}")), ("x-coordinator-session-proof", proof), ("idempotency-key", key)], Some(json!({"session_id":session,"workstation_id":"workstation-1","harness":"test-harness","capabilities":["code"]}))).await
    }
}
struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
    text: String,
}
impl Reply {
    fn ok(&self) {
        assert_eq!(self.status, StatusCode::OK, "{}", self.text);
    }
    fn error(&self, status: StatusCode, code: &str) {
        assert_eq!(self.status, status, "{}", self.text);
        assert_eq!(self.body["error"]["code"], code, "{}", self.text);
        assert!(self.body["request_id"].is_string());
        assert!(self.body["server_time"].is_string());
    }
}
struct Browser {
    cookie: String,
    csrf: String,
}
impl Browser {
    fn headers<'a>(&'a self, key: &'a str) -> Vec<(&'a str, &'a str)> {
        vec![
            ("cookie", &self.cookie),
            ("origin", ORIGIN),
            ("x-csrf-token", &self.csrf),
            ("idempotency-key", key),
        ]
    }
}

#[tokio::test]
async fn public_setup_assets_and_private_route_authentication_order() {
    let fixture = Fixture::new().await;
    for path in ["/healthz", "/api/v1/info", "/api/v1/help/authentication"] {
        let result = fixture.call("GET", path, &[], None).await;
        result.ok();
        assert!(!result.text.contains(PASSWORD));
        assert!(!result.text.contains("password_hash"));
    }
    for path in ["/", "/app.js", "/style.css"] {
        let result = fixture.call("GET", path, &[], None).await;
        result.ok();
        assert_eq!(result.headers["x-content-type-options"], "nosniff");
        assert_eq!(result.headers["cache-control"], "no-store");
        assert!(
            result.headers["content-security-policy"]
                .to_str()
                .unwrap()
                .contains("frame-ancestors 'none'")
        );
        assert!(result.headers.contains_key("strict-transport-security"));
    }
    for path in [
        "/api/v1/me",
        "/api/v1/projects/missing",
        "/api/v1/admin/credentials",
        "/api/v1/future-private-feature",
        "/not-an-asset",
        "/api/v1/projects/%FF",
    ] {
        fixture
            .raw("POST", path, &[], "not valid JSON".into())
            .await
            .error(StatusCode::UNAUTHORIZED, "authentication_required");
    }
    fixture
        .call(
            "POST",
            "/api/v1/auth/login",
            &[],
            Some(json!({"username":"admin","password":PASSWORD})),
        )
        .await
        .error(StatusCode::FORBIDDEN, "operation_not_permitted");
    let browser = fixture.login().await;
    let restored = fixture
        .call("GET", "/api/v1/me", &[("cookie", &browser.cookie)], None)
        .await;
    restored.ok();
    assert_eq!(restored.body["data"]["csrf_token"], browser.csrf);
    assert_eq!(restored.body["data"]["actor"]["kind"], "human");
    assert!(restored.body["data"]["actor"]["session_id"].is_string());
    let hash: String =
        sqlx::query_scalar("SELECT password_hash FROM principals WHERE name='admin'")
            .fetch_one(&fixture.state.pool)
            .await
            .unwrap();
    assert!(hash.starts_with("$argon2id$"));
    assert!(!hash.contains(PASSWORD));
    let token = browser.cookie.split_once('=').unwrap().1;
    let stored: String = sqlx::query_scalar("SELECT token_hash FROM browser_sessions")
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap();
    assert_eq!(stored, digest(token));
    assert_ne!(stored, token);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&fixture.state.config.database_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn browser_writes_require_origin_csrf_and_live_session() {
    let fixture = Fixture::new().await;
    let browser = fixture.login().await;
    let input = Some(json!({"name":"workstation"}));
    for headers in [
        vec![
            ("cookie", browser.cookie.as_str()),
            ("idempotency-key", "one"),
        ],
        vec![
            ("cookie", browser.cookie.as_str()),
            ("origin", ORIGIN),
            ("x-csrf-token", "wrong"),
            ("idempotency-key", "one"),
        ],
        vec![
            ("cookie", browser.cookie.as_str()),
            ("origin", "https://attacker.test"),
            ("x-csrf-token", browser.csrf.as_str()),
            ("idempotency-key", "one"),
        ],
    ] {
        fixture
            .call("POST", "/api/v1/admin/agents", &headers, input.clone())
            .await
            .error(StatusCode::FORBIDDEN, "operation_not_permitted");
    }
    fixture
        .call(
            "POST",
            "/api/v1/admin/agents",
            &[
                ("cookie", &browser.cookie),
                ("origin", ORIGIN),
                ("x-csrf-token", &browser.csrf),
            ],
            input,
        )
        .await
        .error(StatusCode::BAD_REQUEST, "invalid_request");
    fixture
        .call(
            "POST",
            "/api/v1/auth/logout",
            &browser.headers("logout"),
            Some(json!({})),
        )
        .await
        .ok();
    fixture
        .call("GET", "/api/v1/me", &[("cookie", &browser.cookie)], None)
        .await
        .error(StatusCode::UNAUTHORIZED, "authentication_required");
    let second = fixture.login().await;
    fixture
        .clock
        .0
        .fetch_add(12 * 60 * 60 * 1000, Ordering::SeqCst);
    fixture
        .call("GET", "/api/v1/me", &[("cookie", &second.cookie)], None)
        .await
        .error(StatusCode::UNAUTHORIZED, "authentication_required");
}

#[tokio::test]
async fn token_issuance_redacts_receipts_and_revocation_rechecks_saved_auth() {
    let fixture = Fixture::new().await;
    let browser = fixture.login().await;
    let (token, credential) = fixture.issue(&browser, "workstation").await;
    let bearer = format!("Bearer {token}");
    let replay = fixture
        .call(
            "POST",
            "/api/v1/admin/agents",
            &browser.headers("workstation"),
            Some(json!({"name":"workstation"})),
        )
        .await;
    replay.ok();
    assert_eq!(replay.body["data"]["credential_id"], credential);
    assert_eq!(replay.body["data"]["secret_unavailable"], true);
    assert!(replay.body["data"].get("token").is_none());
    for query in [
        "SELECT result_json AS data FROM mutation_receipts",
        "SELECT data_json AS data FROM events",
    ] {
        for row in sqlx::query(query)
            .fetch_all(&fixture.state.pool)
            .await
            .unwrap()
        {
            let data: String = row.get("data");
            assert!(!data.contains(&token));
            assert!(!data.contains(PASSWORD));
            assert!(!data.contains(&browser.csrf));
        }
    }
    let listed = fixture
        .call(
            "GET",
            "/api/v1/admin/credentials",
            &[("cookie", &browser.cookie)],
            None,
        )
        .await;
    listed.ok();
    assert_eq!(listed.body["data"]["items"].as_array().unwrap().len(), 1);
    assert!(!listed.text.contains(&token));
    assert!(!listed.text.contains("token_hash"));
    fixture
        .call(
            "POST",
            "/api/v1/admin/agents",
            &[
                ("authorization", &bearer),
                ("idempotency-key", "escalation"),
            ],
            Some(json!({"name":"escalation"})),
        )
        .await
        .error(StatusCode::FORBIDDEN, "operation_not_permitted");
    fixture
        .call(
            "GET",
            "/api/v1/me",
            &[("authorization", &bearer), ("cookie", &browser.cookie)],
            None,
        )
        .await
        .error(StatusCode::UNAUTHORIZED, "authentication_required");
    let (mut parts, _) = Request::builder()
        .uri("/api/v1/me")
        .header("authorization", &bearer)
        .body(())
        .unwrap()
        .into_parts();
    let saved_auth = Auth::from_request_parts(&mut parts, &fixture.state)
        .await
        .unwrap();
    assert_eq!(saved_auth.actor.kind, "agent");
    assert!(saved_auth.actor.session_id.is_none());
    fixture
        .call(
            "POST",
            &format!("/api/v1/admin/credentials/{credential}/revoke"),
            &browser.headers("revoke"),
            Some(json!({})),
        )
        .await
        .ok();
    fixture
        .call("GET", "/api/v1/me", &[("authorization", &bearer)], None)
        .await
        .error(StatusCode::UNAUTHORIZED, "authentication_required");
    let mut tx = fixture
        .state
        .pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .unwrap();
    assert_eq!(
        saved_auth
            .verify(&mut tx, fixture.state.now())
            .await
            .err()
            .unwrap()
            .status,
        StatusCode::UNAUTHORIZED
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn agent_session_proofs_isolate_harnesses_and_issuing_credentials() {
    let fixture = Fixture::new().await;
    let browser = fixture.login().await;
    let (token, credential) = fixture.issue(&browser, "workstation").await;
    let bearer = format!("Bearer {token}");
    let proof_a = secret();
    let proof_b = secret();
    fixture
        .register(&token, "session-a", &proof_a, "session-a-key")
        .await
        .ok();
    fixture
        .register(&token, "session-b", &proof_b, "session-b-key")
        .await
        .ok();
    fixture
        .register(&token, "session-a", &proof_a, "session-a-key")
        .await
        .ok();
    fixture
        .register(&token, "session-a", &proof_b, "session-a-key")
        .await
        .error(StatusCode::CONFLICT, "idempotency_conflict");
    fixture
        .register(&token, "session-a", &proof_b, "another-key")
        .await
        .error(StatusCode::CONFLICT, "session_conflict");
    let stored: String =
        sqlx::query_scalar("SELECT proof_hash FROM agent_sessions WHERE id='session-a'")
            .fetch_one(&fixture.state.pool)
            .await
            .unwrap();
    assert_eq!(stored, digest(&proof_a));
    for row in sqlx::query("SELECT result_json FROM mutation_receipts")
        .fetch_all(&fixture.state.pool)
        .await
        .unwrap()
    {
        let receipt: String = row.get("result_json");
        assert!(!receipt.contains(&proof_a));
        assert!(!receipt.contains(&proof_b));
    }
    let owned = [
        ("authorization", bearer.as_str()),
        ("x-coordinator-session", "session-a"),
        ("x-coordinator-session-proof", proof_a.as_str()),
    ];
    fixture
        .call("GET", "/api/v1/sessions/session-a", &owned, None)
        .await
        .ok();
    fixture
        .call("GET", "/api/v1/sessions/session-b", &owned, None)
        .await
        .error(StatusCode::FORBIDDEN, "operation_not_permitted");
    fixture
        .call(
            "GET",
            "/api/v1/me",
            &[
                ("authorization", &bearer),
                ("x-coordinator-session", "session-a"),
                ("x-coordinator-session-proof", &proof_b),
            ],
            None,
        )
        .await
        .error(StatusCode::UNAUTHORIZED, "authentication_required");
    fixture
        .call(
            "GET",
            "/api/v1/me",
            &[
                ("authorization", &bearer),
                ("x-coordinator-session", "session-a"),
            ],
            None,
        )
        .await
        .error(StatusCode::UNAUTHORIZED, "authentication_required");
    // Even another token for the same principal cannot borrow the session proof.
    let other = secret();
    sqlx::query("INSERT INTO credentials(id,principal_id,token_hash,created_at) SELECT 'second-credential',principal_id,?,? FROM credentials WHERE id=?")
        .bind(digest(&other)).bind(fixture.state.now()).bind(&credential).execute(&fixture.state.pool).await.unwrap();
    fixture
        .call(
            "GET",
            "/api/v1/me",
            &[
                ("authorization", &format!("Bearer {other}")),
                ("x-coordinator-session", "session-a"),
                ("x-coordinator-session-proof", &proof_a),
            ],
            None,
        )
        .await
        .error(StatusCode::UNAUTHORIZED, "authentication_required");
    let mut closing = owned.to_vec();
    closing.push(("idempotency-key", "close-a"));
    fixture
        .call(
            "POST",
            "/api/v1/sessions/session-a/close",
            &closing,
            Some(json!({})),
        )
        .await
        .ok();
    // A lost close response can replay its receipt after closure. This narrow
    // exception does not allow the session to authenticate other operations.
    fixture
        .call(
            "POST",
            "/api/v1/sessions/session-a/close",
            &closing,
            Some(json!({})),
        )
        .await
        .ok();
    let closed = fixture
        .call("GET", "/api/v1/sessions/session-a", &owned, None)
        .await;
    closed.ok();
    assert!(closed.body["data"]["closed_at"].is_string());
    fixture
        .call("GET", "/api/v1/me", &owned, None)
        .await
        .error(StatusCode::UNAUTHORIZED, "authentication_required");
    fixture
        .register(&token, "session-a", &proof_a, "reopen")
        .await
        .error(StatusCode::CONFLICT, "session_closed");
    fixture
        .call(
            "POST",
            &format!("/api/v1/admin/credentials/{credential}/revoke"),
            &browser.headers("revoke"),
            Some(json!({})),
        )
        .await
        .ok();
    fixture
        .call(
            "GET",
            "/api/v1/sessions/session-b",
            &[
                ("authorization", &bearer),
                ("x-coordinator-session", "session-b"),
                ("x-coordinator-session-proof", &proof_b),
            ],
            None,
        )
        .await
        .error(StatusCode::UNAUTHORIZED, "authentication_required");
}

#[tokio::test]
async fn malformed_payloads_and_login_abuse_return_stable_errors() {
    let fixture = Fixture::new().await;
    fixture
        .raw(
            "POST",
            "/api/v1/auth/login",
            &[("origin", ORIGIN)],
            "{".into(),
        )
        .await
        .error(StatusCode::BAD_REQUEST, "invalid_request");
    let too_large = format!(
        "{{\"username\":\"admin\",\"password\":\"{}\"}}",
        "x".repeat(2 * 1024 * 1024)
    );
    fixture
        .raw(
            "POST",
            "/api/v1/auth/login",
            &[("origin", ORIGIN)],
            too_large,
        )
        .await
        .error(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large");
    for _ in 0..5 {
        fixture
            .call(
                "POST",
                "/api/v1/auth/login",
                &[("origin", ORIGIN)],
                Some(json!({"username":"admin","password":"incorrect password"})),
            )
            .await
            .error(StatusCode::UNAUTHORIZED, "authentication_required");
    }
    let limited = fixture
        .call(
            "POST",
            "/api/v1/auth/login",
            &[("origin", ORIGIN)],
            Some(json!({"username":"admin","password":PASSWORD})),
        )
        .await;
    limited.error(StatusCode::TOO_MANY_REQUESTS, "rate_limited");
    assert_eq!(limited.headers["retry-after"], "60");
}

#[tokio::test]
async fn disabled_principals_and_expired_agent_tokens_lose_authority() {
    let fixture = Fixture::new().await;
    let browser = fixture.login().await;
    let (token, credential) = fixture.issue(&browser, "expired-workstation").await;
    sqlx::query("UPDATE credentials SET expires_at=? WHERE id=?")
        .bind(fixture.state.now())
        .bind(credential)
        .execute(&fixture.state.pool)
        .await
        .unwrap();
    fixture
        .call(
            "GET",
            "/api/v1/me",
            &[("authorization", &format!("Bearer {token}"))],
            None,
        )
        .await
        .error(StatusCode::UNAUTHORIZED, "authentication_required");
    sqlx::query("UPDATE principals SET disabled_at=? WHERE name='admin'")
        .bind(fixture.state.now())
        .execute(&fixture.state.pool)
        .await
        .unwrap();
    fixture
        .call("GET", "/api/v1/me", &[("cookie", &browser.cookie)], None)
        .await
        .error(StatusCode::UNAUTHORIZED, "authentication_required");
    fixture
        .call(
            "POST",
            "/api/v1/auth/login",
            &[("origin", ORIGIN)],
            Some(json!({"username":"admin","password":PASSWORD})),
        )
        .await
        .error(StatusCode::UNAUTHORIZED, "authentication_required");
    assert_eq!(
        init_admin(&fixture.state, "replacement", PASSWORD.into())
            .await
            .err()
            .unwrap()
            .code,
        "already_initialized"
    );
}

#[test]
fn service_configuration_requires_private_listener_and_https() {
    assert!(Config::default().validate().is_ok());
    for origin in [
        "http://coordinator.example.test",
        "https://coordinator.example.test/path",
        "https://admin:password@coordinator.example.test",
        "https://coordinator.example.test/",
        "https://coordinator.example.test?token=secret",
    ] {
        assert!(
            Config {
                public_origin: origin.into(),
                ..Config::default()
            }
            .validate()
            .is_err()
        );
    }
    assert!(
        Config {
            listen: "0.0.0.0:8080".parse().unwrap(),
            ..Config::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        Config {
            public_origin: "http://127.0.0.1:8080".into(),
            ..Config::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        Config {
            public_origin: "http://127.0.0.1:8080".into(),
            allow_insecure_loopback: true,
            ..Config::default()
        }
        .validate()
        .is_ok()
    );
    assert!(
        Config {
            public_origin: "http://public.example.test".into(),
            allow_insecure_loopback: true,
            ..Config::default()
        }
        .validate()
        .is_err()
    );
}

#[tokio::test]
async fn host_cli_initializes_once_from_stdin_without_printing_password() {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bootstrap.sqlite3");
    let invoke = || {
        let mut child = Command::new(env!("CARGO_BIN_EXE_agent-coordinator-server"))
            .arg("--database")
            .arg(&path)
            .args([
                "init-admin",
                "--username",
                "first-admin",
                "--password-stdin",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(format!("{PASSWORD}\n").as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    };
    let first = invoke();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(!String::from_utf8_lossy(&first.stdout).contains(PASSWORD));
    assert!(!String::from_utf8_lossy(&first.stderr).contains(PASSWORD));
    let second = invoke();
    assert!(!second.status.success());
    assert!(!String::from_utf8_lossy(&second.stdout).contains(PASSWORD));
    assert!(!String::from_utf8_lossy(&second.stderr).contains(PASSWORD));
    let state = AppState::open(Config {
        database_path: path,
        ..Config::default()
    })
    .await
    .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM principals WHERE name='first-admin' AND kind='human' AND role='admin'")
        .fetch_one(&state.pool).await.unwrap();
    assert_eq!(count, 1);
}
