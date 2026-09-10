use axum::{
    body::Body,
    http::{HeaderMap, Request, StatusCode},
};
use coordinator_server::{
    auth::init_admin,
    operator_access::recover_operator_password,
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
const ADMIN_PASSWORD: &str = "initial administrator password";
const OPERATOR_PASSWORD: &str = "initial operator password";
const NEW_PASSWORD: &str = "replacement operator password";
const ALTERNATE_PASSWORD: &str = "alternate replacement password";

struct TestClock(AtomicI64);
impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct Fixture {
    state: AppState,
    _directory: tempfile::TempDir,
}

impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let config = Config {
            database_path: directory.path().join("operator-access.sqlite3"),
            public_origin: ORIGIN.into(),
            ..Config::default()
        };
        let mut state = AppState::open(config).await.unwrap();
        state.clock = Arc::new(TestClock(AtomicI64::new(1_788_976_800_000)));
        init_admin(&state, "admin", ADMIN_PASSWORD.into())
            .await
            .unwrap();
        Self {
            state,
            _directory: directory,
        }
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<Value>,
    ) -> Reply {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let result = router(self.state.clone())
            .oneshot(
                request
                    .body(Body::from(
                        body.map(|value| value.to_string()).unwrap_or_default(),
                    ))
                    .unwrap(),
            )
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

    async fn login_as(&self, username: &str, password: &str) -> Reply {
        self.call(
            "POST",
            "/api/v1/auth/login",
            &[("origin", ORIGIN)],
            Some(json!({"username":username,"password":password})),
        )
        .await
    }

    async fn login(&self) -> Browser {
        Browser::from_reply(self.login_as("admin", ADMIN_PASSWORD).await)
    }

    async fn create_operator(
        &self,
        browser: &Browser,
        name: &str,
        role: &str,
        password: &str,
        key: &str,
    ) -> Reply {
        self.call(
            "POST",
            "/api/v1/admin/operators",
            &browser.headers(key),
            Some(json!({"name":name,"role":role,"password":password})),
        )
        .await
    }

    async fn issue_agent(&self, browser: &Browser, name: &str) -> Reply {
        self.call(
            "POST",
            "/api/v1/admin/agents",
            &browser.headers(&format!("issue-{name}")),
            Some(json!({"name":name})),
        )
        .await
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
    }
}

#[derive(Clone)]
struct Browser {
    cookie: String,
    csrf: String,
}

impl Browser {
    fn from_reply(reply: Reply) -> Self {
        reply.ok();
        let cookie = reply.headers["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        Self {
            cookie,
            csrf: reply.body["data"]["csrf_token"]
                .as_str()
                .unwrap()
                .to_owned(),
        }
    }

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
async fn account_creation_replay_and_last_admin_are_guarded() {
    let fixture = Fixture::new().await;
    let admin = fixture.login().await;
    let own = fixture
        .call(
            "GET",
            "/api/v1/auth/account",
            &[("cookie", &admin.cookie)],
            None,
        )
        .await;
    own.ok();
    assert_eq!(own.body["data"]["operator"]["revision"], 1);
    let own_id = own.body["data"]["operator"]["id"].as_str().unwrap();
    fixture
        .call(
            "POST",
            &format!("/api/v1/admin/operators/{own_id}/access"),
            &admin.headers("disable-only-admin"),
            Some(json!({"expected_revision":1,"role":"admin","enabled":false})),
        )
        .await
        .error(StatusCode::CONFLICT, "last_active_admin");

    let created = fixture
        .create_operator(
            &admin,
            "backup-admin",
            "admin",
            OPERATOR_PASSWORD,
            "create-backup",
        )
        .await;
    created.ok();
    let backup_id = created.body["data"]["operator"]["id"].as_str().unwrap();
    fixture
        .create_operator(
            &admin,
            "backup-admin",
            "admin",
            "different reentered password",
            "create-backup",
        )
        .await
        .error(StatusCode::CONFLICT, "idempotency_secret_mismatch");
    let replay = fixture
        .create_operator(
            &admin,
            "backup-admin",
            "admin",
            OPERATOR_PASSWORD,
            "create-backup",
        )
        .await;
    replay.ok();
    assert_eq!(replay.body["data"]["operator"]["id"], backup_id);

    let changed = fixture
        .call(
            "POST",
            &format!("/api/v1/admin/operators/{backup_id}/access"),
            &admin.headers("downgrade-backup"),
            Some(json!({"expected_revision":1,"role":"operator","enabled":false})),
        )
        .await;
    changed.ok();
    assert_eq!(changed.body["data"]["operator"]["revision"], 2);
    let current_replay = fixture
        .create_operator(
            &admin,
            "backup-admin",
            "admin",
            OPERATOR_PASSWORD,
            "create-backup",
        )
        .await;
    current_replay.ok();
    assert_eq!(current_replay.body["data"]["operator"]["role"], "operator");
    assert_eq!(current_replay.body["data"]["operator"]["enabled"], false);
    assert_eq!(current_replay.body["data"]["operator"]["revision"], 2);
    fixture
        .call(
            "POST",
            &format!("/api/v1/admin/operators/{backup_id}/access"),
            &admin.headers("stale-backup"),
            Some(json!({"expected_revision":1,"role":"operator","enabled":true})),
        )
        .await
        .error(StatusCode::CONFLICT, "revision_conflict");

    let same_left = fixture.create_operator(
        &admin,
        "same-race",
        "operator",
        OPERATOR_PASSWORD,
        "same-race-key",
    );
    let same_right = fixture.create_operator(
        &admin,
        "same-race",
        "operator",
        OPERATOR_PASSWORD,
        "same-race-key",
    );
    let (same_left, same_right) = tokio::join!(same_left, same_right);
    same_left.ok();
    same_right.ok();
    assert_eq!(
        same_left.body["data"]["operator"]["id"],
        same_right.body["data"]["operator"]["id"]
    );

    let different_left = fixture.create_operator(
        &admin,
        "different-race",
        "operator",
        OPERATOR_PASSWORD,
        "different-race-key",
    );
    let different_right = fixture.create_operator(
        &admin,
        "different-race",
        "operator",
        NEW_PASSWORD,
        "different-race-key",
    );
    let (different_left, different_right) = tokio::join!(different_left, different_right);
    let results = [&different_left, &different_right];
    assert_eq!(
        results
            .iter()
            .filter(|reply| reply.status == StatusCode::OK)
            .count(),
        1
    );
    let conflict = results
        .iter()
        .find(|reply| reply.status == StatusCode::CONFLICT)
        .unwrap();
    assert_eq!(
        conflict.body["error"]["code"],
        "idempotency_secret_mismatch"
    );
}

#[tokio::test]
async fn account_creation_replays_after_reauthentication_but_requires_current_admin() {
    let fixture = Fixture::new().await;
    let original = fixture.login().await;
    let own = fixture
        .call(
            "GET",
            "/api/v1/auth/account",
            &[("cookie", &original.cookie)],
            None,
        )
        .await;
    own.ok();
    let original_id = own.body["data"]["operator"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let created = fixture
        .create_operator(
            &original,
            "uncertain-create",
            "operator",
            OPERATOR_PASSWORD,
            "uncertain-create-key",
        )
        .await;
    created.ok();
    let created_id = created.body["data"]["operator"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let reauthenticated = fixture.login().await;
    let replay = fixture
        .create_operator(
            &reauthenticated,
            "uncertain-create",
            "operator",
            OPERATOR_PASSWORD,
            "uncertain-create-key",
        )
        .await;
    replay.ok();
    assert_eq!(replay.body["data"]["operator"]["id"], created_id);
    fixture
        .create_operator(
            &reauthenticated,
            "uncertain-create",
            "operator",
            NEW_PASSWORD,
            "uncertain-create-key",
        )
        .await
        .error(StatusCode::CONFLICT, "idempotency_secret_mismatch");
    fixture
        .create_operator(
            &reauthenticated,
            "changed-name",
            "operator",
            OPERATOR_PASSWORD,
            "uncertain-create-key",
        )
        .await
        .error(StatusCode::CONFLICT, "idempotency_conflict");
    let receipt_count: i64 = sqlx::query_scalar("SELECT count(*) FROM mutation_receipts WHERE operation='POST /api/v1/admin/operators' AND key='uncertain-create-key'")
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap();
    let event_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM events WHERE kind='operator_created' AND record_id=?",
    )
    .bind(&created_id)
    .fetch_one(&fixture.state.pool)
    .await
    .unwrap();
    assert_eq!(receipt_count, 1);
    assert_eq!(event_count, 1);

    let backup = fixture
        .create_operator(
            &reauthenticated,
            "authority-backup",
            "admin",
            OPERATOR_PASSWORD,
            "authority-backup-key",
        )
        .await;
    backup.ok();
    let backup_browser = Browser::from_reply(
        fixture
            .login_as("authority-backup", OPERATOR_PASSWORD)
            .await,
    );
    fixture
        .call(
            "POST",
            &format!("/api/v1/admin/operators/{original_id}/access"),
            &backup_browser.headers("disable-original-admin"),
            Some(json!({"expected_revision":1,"role":"admin","enabled":false})),
        )
        .await
        .ok();
    fixture
        .create_operator(
            &reauthenticated,
            "uncertain-create",
            "operator",
            OPERATOR_PASSWORD,
            "uncertain-create-key",
        )
        .await
        .error(StatusCode::UNAUTHORIZED, "authentication_required");
    fixture
        .call(
            "POST",
            &format!("/api/v1/admin/operators/{original_id}/access"),
            &backup_browser.headers("demote-original-admin"),
            Some(json!({"expected_revision":2,"role":"operator","enabled":true})),
        )
        .await
        .ok();
    let demoted = Browser::from_reply(fixture.login_as("admin", ADMIN_PASSWORD).await);
    fixture
        .create_operator(
            &demoted,
            "uncertain-create",
            "operator",
            OPERATOR_PASSWORD,
            "uncertain-create-key",
        )
        .await
        .error(StatusCode::FORBIDDEN, "operation_not_permitted");
}

#[tokio::test]
async fn password_change_revokes_every_session_and_redacts_secrets() {
    let fixture = Fixture::new().await;
    let first = fixture.login().await;
    let second = fixture.login().await;
    let changed = fixture
        .call(
            "POST",
            "/api/v1/auth/password",
            &first.headers("change-password"),
            Some(json!({
                "current_password":ADMIN_PASSWORD,
                "new_password":NEW_PASSWORD,
                "expected_revision":1
            })),
        )
        .await;
    changed.ok();
    assert_eq!(changed.body["data"]["operator"]["revision"], 2);
    assert_eq!(changed.body["data"]["all_sessions_revoked"], true);
    assert!(
        changed.headers["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    for browser in [&first, &second] {
        fixture
            .call(
                "GET",
                "/api/v1/auth/account",
                &[("cookie", &browser.cookie)],
                None,
            )
            .await
            .error(StatusCode::UNAUTHORIZED, "authentication_required");
    }
    fixture
        .login_as("admin", ADMIN_PASSWORD)
        .await
        .error(StatusCode::UNAUTHORIZED, "authentication_required");
    Browser::from_reply(fixture.login_as("admin", NEW_PASSWORD).await);

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
            assert!(!data.contains(ADMIN_PASSWORD));
            assert!(!data.contains(NEW_PASSWORD));
            assert!(!data.contains("argon2"));
            assert!(!data.contains("password_verifier"));
        }
    }
}

#[tokio::test]
async fn concurrent_password_changes_recheck_hash_and_session_under_writer_lock() {
    let fixture = Fixture::new().await;
    let first = fixture.login().await;
    let second = fixture.login().await;
    let first_headers = first.headers("password-race-one");
    let second_headers = second.headers("password-race-two");
    let left = fixture.call(
        "POST",
        "/api/v1/auth/password",
        &first_headers,
        Some(json!({
            "current_password":ADMIN_PASSWORD,
            "new_password":NEW_PASSWORD,
            "expected_revision":1
        })),
    );
    let right = fixture.call(
        "POST",
        "/api/v1/auth/password",
        &second_headers,
        Some(json!({
            "current_password":ADMIN_PASSWORD,
            "new_password":ALTERNATE_PASSWORD,
            "expected_revision":1
        })),
    );
    let (left, right) = tokio::join!(left, right);
    let results = [&left, &right];
    assert_eq!(
        results
            .iter()
            .filter(|reply| reply.status == StatusCode::OK)
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|reply| reply.status == StatusCode::UNAUTHORIZED)
            .count(),
        1
    );
    fixture
        .login_as("admin", ADMIN_PASSWORD)
        .await
        .error(StatusCode::UNAUTHORIZED, "authentication_required");
    let new_works = fixture.login_as("admin", NEW_PASSWORD).await.status == StatusCode::OK;
    let alternate_works =
        fixture.login_as("admin", ALTERNATE_PASSWORD).await.status == StatusCode::OK;
    assert_ne!(new_works, alternate_works);
}

#[tokio::test]
async fn browser_sessions_are_private_and_individually_revocable() {
    let fixture = Fixture::new().await;
    let first = fixture.login().await;
    let second = fixture.login().await;
    let listed = fixture
        .call(
            "GET",
            "/api/v1/browser-sessions",
            &[("cookie", &first.cookie)],
            None,
        )
        .await;
    listed.ok();
    let sessions = listed.body["data"]["items"].as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    assert!(!listed.text.contains("token_hash"));
    let other = sessions
        .iter()
        .find(|value| value["current"] == false)
        .unwrap()["id"]
        .as_str()
        .unwrap();
    fixture
        .call(
            "POST",
            &format!("/api/v1/browser-sessions/{other}/revoke"),
            &first.headers("revoke-other"),
            Some(json!({})),
        )
        .await
        .ok();
    fixture
        .call(
            "GET",
            "/api/v1/auth/account",
            &[("cookie", &second.cookie)],
            None,
        )
        .await
        .error(StatusCode::UNAUTHORIZED, "authentication_required");

    fixture
        .create_operator(
            &first,
            "ordinary-operator",
            "operator",
            OPERATOR_PASSWORD,
            "create-operator",
        )
        .await
        .ok();
    let operator = Browser::from_reply(
        fixture
            .login_as("ordinary-operator", OPERATOR_PASSWORD)
            .await,
    );
    fixture
        .call(
            "GET",
            &format!(
                "/api/v1/browser-sessions?principal_id={}",
                listed.body["data"]["principal_id"].as_str().unwrap()
            ),
            &[("cookie", &operator.cookie)],
            None,
        )
        .await
        .error(StatusCode::FORBIDDEN, "operation_not_permitted");
}

#[tokio::test]
async fn token_rotation_preserves_principal_and_redacts_replay() {
    let fixture = Fixture::new().await;
    let admin = fixture.login().await;
    let issued = fixture.issue_agent(&admin, "builder").await;
    issued.ok();
    let old_token = issued.body["data"]["token"].as_str().unwrap();
    let old_credential = issued.body["data"]["credential_id"].as_str().unwrap();
    let principal = issued.body["data"]["principal_id"].as_str().unwrap();
    let rotated = fixture
        .call(
            "POST",
            &format!("/api/v1/admin/credentials/{old_credential}/rotate"),
            &admin.headers("rotate-builder"),
            Some(json!({"name":"builder-laptop"})),
        )
        .await;
    rotated.ok();
    assert_eq!(rotated.body["data"]["principal_id"], principal);
    assert_eq!(rotated.body["data"]["replaced_credential_revoked"], true);
    let new_token = rotated.body["data"]["token"].as_str().unwrap();
    assert_ne!(new_token, old_token);
    fixture
        .call(
            "GET",
            "/api/v1/me",
            &[("authorization", &format!("Bearer {old_token}"))],
            None,
        )
        .await
        .error(StatusCode::UNAUTHORIZED, "authentication_required");
    fixture
        .call(
            "GET",
            "/api/v1/me",
            &[("authorization", &format!("Bearer {new_token}"))],
            None,
        )
        .await
        .ok();
    let replay = fixture
        .call(
            "POST",
            &format!("/api/v1/admin/credentials/{old_credential}/rotate"),
            &admin.headers("rotate-builder"),
            Some(json!({"name":"builder-laptop"})),
        )
        .await;
    replay.ok();
    assert_eq!(
        replay.body["data"]["credential"]["id"],
        rotated.body["data"]["credential"]["id"]
    );
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
            assert!(!data.contains(old_token));
            assert!(!data.contains(new_token));
            assert!(!data.contains("token_hash"));
        }
    }
}

#[tokio::test]
async fn host_recovery_enables_account_revokes_sessions_and_preserves_other_authority() {
    let fixture = Fixture::new().await;
    let admin = fixture.login().await;
    let operator = fixture
        .create_operator(
            &admin,
            "recover-me",
            "operator",
            OPERATOR_PASSWORD,
            "create-recovery-target",
        )
        .await;
    operator.ok();
    let operator_id = operator.body["data"]["operator"]["id"].as_str().unwrap();
    let operator_browser =
        Browser::from_reply(fixture.login_as("recover-me", OPERATOR_PASSWORD).await);
    let issued = fixture.issue_agent(&admin, "unrelated-agent").await;
    issued.ok();
    let credential_id = issued.body["data"]["credential_id"].as_str().unwrap();
    fixture
        .call(
            "POST",
            &format!("/api/v1/admin/operators/{operator_id}/access"),
            &admin.headers("disable-recovery-target"),
            Some(json!({"expected_revision":1,"role":"operator","enabled":false})),
        )
        .await
        .ok();
    let recovered = recover_operator_password(
        &fixture.state,
        "recover-me",
        NEW_PASSWORD.into(),
        "password was lost",
    )
    .await
    .unwrap();
    assert_eq!(recovered["operator"]["id"], operator_id);
    assert_eq!(recovered["operator"]["enabled"], true);
    assert_eq!(recovered["operator"]["revision"], 3);
    fixture
        .call(
            "GET",
            "/api/v1/auth/account",
            &[("cookie", &operator_browser.cookie)],
            None,
        )
        .await
        .error(StatusCode::UNAUTHORIZED, "authentication_required");
    fixture
        .login_as("recover-me", OPERATOR_PASSWORD)
        .await
        .error(StatusCode::UNAUTHORIZED, "authentication_required");
    Browser::from_reply(fixture.login_as("recover-me", NEW_PASSWORD).await);
    let revoked: Option<i64> = sqlx::query_scalar("SELECT revoked_at FROM credentials WHERE id=?")
        .bind(credential_id)
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap();
    assert!(revoked.is_none());
    let event = sqlx::query(
        "SELECT actor_id,record_id,data_json FROM events WHERE kind='operator_password_recovered'",
    )
    .fetch_one(&fixture.state.pool)
    .await
    .unwrap();
    assert_eq!(event.get::<String, _>("actor_id"), operator_id);
    assert_eq!(event.get::<String, _>("record_id"), operator_id);
    let event_data: Value = serde_json::from_str(&event.get::<String, _>("data_json")).unwrap();
    assert_eq!(event_data["reason"], "password was lost");
    assert_eq!(event_data["host_local"], true);
    assert_eq!(event_data["initiator_kind"], "host_operator");
    assert_eq!(event_data["authenticated_principal_id"], Value::Null);
    assert_eq!(event_data["subject_principal_id"], operator_id);
    assert_eq!(event_data["actor_id_role"], "subject_reference");
    assert!(!event.get::<String, _>("data_json").contains(NEW_PASSWORD));
}

#[tokio::test]
async fn concurrent_admin_removal_leaves_one_active_admin() {
    let fixture = Fixture::new().await;
    let first = fixture.login().await;
    let first_id = fixture
        .call(
            "GET",
            "/api/v1/auth/account",
            &[("cookie", &first.cookie)],
            None,
        )
        .await
        .body["data"]["operator"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let second_created = fixture
        .create_operator(
            &first,
            "second-admin",
            "admin",
            OPERATOR_PASSWORD,
            "create-second-admin",
        )
        .await;
    second_created.ok();
    let second_id = second_created.body["data"]["operator"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let second = Browser::from_reply(fixture.login_as("second-admin", OPERATOR_PASSWORD).await);
    let first_path = format!("/api/v1/admin/operators/{first_id}/access");
    let second_path = format!("/api/v1/admin/operators/{second_id}/access");
    let first_headers = first.headers("disable-first");
    let second_headers = second.headers("disable-second");
    let left = fixture.call(
        "POST",
        &first_path,
        &first_headers,
        Some(json!({"expected_revision":1,"role":"admin","enabled":false})),
    );
    let right = fixture.call(
        "POST",
        &second_path,
        &second_headers,
        Some(json!({"expected_revision":1,"role":"admin","enabled":false})),
    );
    let (left, right) = tokio::join!(left, right);
    let statuses = [left.status, right.status];
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == StatusCode::OK)
            .count(),
        1
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == StatusCode::CONFLICT)
            .count(),
        1
    );
    let active: i64 = sqlx::query_scalar("SELECT count(*) FROM principals WHERE kind='human' AND role='admin' AND disabled_at IS NULL")
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap();
    assert_eq!(active, 1);
}
