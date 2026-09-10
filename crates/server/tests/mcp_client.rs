use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
};

use axum::http::{HeaderName, HeaderValue};
use coordinator_server::{
    auth::{digest, secret},
    router,
    state::{AppState, Clock, Config},
};
use rmcp::{
    ClientLifecycleMode, ClientServiceExt, ServiceExt,
    model::{CallToolRequestParams, ProtocolVersion},
    transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde_json::{Map, Value, json};
use sqlx::Row;
use tokio::task::JoinHandle;
use uuid::Uuid;

const NOW: i64 = 1_800_000_000_000;

struct TestClock(AtomicI64);

impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }

    fn use_monotonic_elapsed(&self) -> bool {
        false
    }
}

struct Fixture {
    state: AppState,
    url: String,
    token: String,
    session: String,
    proof: String,
    project: String,
    server: JoinHandle<()>,
    _dir: tempfile::TempDir,
}

impl Fixture {
    async fn new() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::open(Config {
            database_path: dir.path().join("test.sqlite3"),
            public_origin: format!("http://{address}"),
            allow_insecure_loopback: true,
            ..Config::default()
        })
        .await
        .unwrap();
        state.clock = Arc::new(TestClock(AtomicI64::new(NOW)));

        let token = secret();
        let proof = secret();
        let session = Uuid::new_v4().to_string();
        let principal = Uuid::new_v4().to_string();
        let credential = Uuid::new_v4().to_string();
        let project = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO principals(id,name,kind,role,password_hash,created_at) \
             VALUES(?,'mcp-agent','agent','agent',NULL,?)",
        )
        .bind(&principal)
        .bind(NOW)
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO credentials(id,principal_id,token_hash,created_at) VALUES(?,?,?,?)",
        )
        .bind(&credential)
        .bind(&principal)
        .bind(digest(&token))
        .bind(NOW)
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO projects(id,name,repository_url,target_branch,created_at) \
             VALUES(?,'MCP project','https://example.test/repository.git','main',?)",
        )
        .bind(&project)
        .bind(NOW)
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO policy_revisions(project_id,revision,data_json,actor_id,created_at) \
             VALUES(?,1,'{}',?,?)",
        )
        .bind(&project)
        .bind(&principal)
        .bind(NOW)
        .execute(&state.pool)
        .await
        .unwrap();

        let app = router(state.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            state,
            url: format!("http://{address}/mcp"),
            token,
            session,
            proof,
            project,
            server,
            _dir: dir,
        }
    }

    fn transport(&self) -> StreamableHttpClientTransport<reqwest::Client> {
        transport(&self.url, &self.token, &self.session, &self.proof)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn transport(
    url: &str,
    token: &str,
    session: &str,
    proof: &str,
) -> StreamableHttpClientTransport<reqwest::Client> {
    let mut headers = HashMap::new();
    headers.insert(
        HeaderName::from_static("x-coordinator-session"),
        HeaderValue::from_str(session).unwrap(),
    );
    headers.insert(
        HeaderName::from_static("x-coordinator-session-proof"),
        HeaderValue::from_str(proof).unwrap(),
    );
    let config = StreamableHttpClientTransportConfig::with_uri(url)
        .auth_header(token)
        .custom_headers(headers)
        .max_concurrent_requests(1);
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    StreamableHttpClientTransport::with_client(http, config)
}

fn args(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

#[tokio::test]
async fn official_client_discovers_registers_claims_and_reconnects_without_renewal() {
    let fixture = Fixture::new().await;
    let client = ()
        .serve_with_lifecycle(
            fixture.transport(),
            ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .unwrap();
    assert_eq!(
        client.peer_info().unwrap().protocol_version,
        ProtocolVersion::V_2026_07_28
    );
    let tools = client.list_tools(None).await.unwrap();
    assert_eq!(tools.tools.len(), 56);
    for required in [
        "coordinator_session_register",
        "coordinator_orientation",
        "coordinator_claim",
        "coordinator_attempt_get",
    ] {
        assert!(tools.tools.iter().any(|tool| tool.name == required));
    }

    let registration = client
        .call_tool(
            CallToolRequestParams::new("coordinator_session_register").with_arguments(args(
                json!({
                    "idempotency_key": Uuid::new_v4().to_string(),
                    "body": {
                        "session_id": fixture.session,
                        "workstation_id": "mcp-test-workstation",
                        "harness": "rmcp-3.2.0-test",
                        "capabilities": ["code"]
                    }
                }),
            )),
        )
        .await
        .unwrap();
    assert_eq!(registration.is_error, Some(false));
    assert_eq!(
        registration.structured_content.as_ref().unwrap()["data"]["id"],
        fixture.session
    );

    let configured_session = client
        .call_tool(
            CallToolRequestParams::new("coordinator_session_get").with_arguments(args(json!({}))),
        )
        .await
        .unwrap();
    assert_eq!(configured_session.is_error, Some(false));
    assert_eq!(
        configured_session.structured_content.as_ref().unwrap()["data"]["session_id"],
        fixture.session
    );

    let orientation = client
        .call_tool(
            CallToolRequestParams::new("coordinator_orientation")
                .with_arguments(args(json!({"project": fixture.project}))),
        )
        .await
        .unwrap();
    assert_eq!(orientation.is_error, Some(false));
    assert_eq!(
        orientation.structured_content.as_ref().unwrap()["data"]["instructions_complete"],
        true
    );

    let acknowledgment = client
        .call_tool(
            CallToolRequestParams::new("coordinator_instructions_ack").with_arguments(args(
                json!({
                    "session": fixture.session,
                    "idempotency_key": Uuid::new_v4().to_string(),
                    "body": {
                        "project_id": fixture.project,
                        "policy_revision": 1,
                        "instruction_version": coordinator_core::INSTRUCTION_VERSION,
                        "sections": [coordinator_core::REQUIRED_SECTION]
                    }
                }),
            )),
        )
        .await
        .unwrap();
    assert_eq!(acknowledgment.is_error, Some(false));

    let created = client
        .call_tool(
            CallToolRequestParams::new("coordinator_task_create").with_arguments(args(json!({
                "project": fixture.project,
                "idempotency_key": Uuid::new_v4().to_string(),
                "body": {
                    "title": "Exercise the official MCP client",
                    "description": "Round-trip through the public TCP transport.",
                    "acceptance_criteria": ["The exact task can be claimed"],
                    "kind": "code",
                    "priority": 1,
                    "depends_on": [],
                    "planned": false
                }
            }))),
        )
        .await
        .unwrap();
    assert_eq!(created.is_error, Some(false));
    let task = &created.structured_content.as_ref().unwrap()["data"];
    let task_id = task["id"].as_str().unwrap().to_owned();
    let task_revision = task["revision"].as_i64().unwrap();

    let claimed = client
        .call_tool(
            CallToolRequestParams::new("coordinator_claim").with_arguments(args(json!({
                "project": fixture.project,
                "idempotency_key": Uuid::new_v4().to_string(),
                "body": {
                    "task_id": task_id,
                    "expected_task_revision": task_revision,
                    "mode": "work",
                    "policy_revision": 1,
                    "instruction_version": coordinator_core::INSTRUCTION_VERSION
                }
            }))),
        )
        .await
        .unwrap();
    assert_eq!(claimed.is_error, Some(false));
    let claim = &claimed.structured_content.as_ref().unwrap()["data"]["claim"];
    let attempt_id = claim["attempt"]["id"].as_str().unwrap().to_owned();
    let before: (i64, i64, i64) = sqlx::query(
        "SELECT expires_at,last_heartbeat_at,(SELECT count(*) FROM events WHERE kind='attempt.renewed') \
         FROM attempts WHERE id=?",
    )
    .bind(&attempt_id)
    .map(|row: sqlx::sqlite::SqliteRow| {
        (row.get(0), row.get(1), row.get(2))
    })
    .fetch_one(&fixture.state.pool)
    .await
    .unwrap();
    client.cancel().await.unwrap();

    let legacy = ().serve(fixture.transport()).await.unwrap();
    assert_eq!(
        legacy.peer_info().unwrap().protocol_version,
        ProtocolVersion::V_2025_11_25
    );
    let inspected = legacy
        .call_tool(
            CallToolRequestParams::new("coordinator_attempt_get").with_arguments(args(json!({
                "project": fixture.project,
                "attempt": attempt_id
            }))),
        )
        .await
        .unwrap();
    assert_eq!(inspected.is_error, Some(false));
    assert_eq!(
        inspected.structured_content.as_ref().unwrap()["data"]["authority_valid"],
        true
    );
    let after: (i64, i64, i64) = sqlx::query(
        "SELECT expires_at,last_heartbeat_at,(SELECT count(*) FROM events WHERE kind='attempt.renewed') \
         FROM attempts WHERE id=?",
    )
    .bind(&attempt_id)
    .map(|row: sqlx::sqlite::SqliteRow| {
        (row.get(0), row.get(1), row.get(2))
    })
    .fetch_one(&fixture.state.pool)
    .await
    .unwrap();
    assert_eq!(
        after, before,
        "initialization and reads must not renew authority"
    );
    legacy.cancel().await.unwrap();
}

#[tokio::test]
async fn official_client_rejects_invalid_bearer_before_initialization() {
    let fixture = Fixture::new().await;
    let result = ()
        .serve_with_lifecycle(
            transport(
                &fixture.url,
                "invalid-test-credential",
                &fixture.session,
                &fixture.proof,
            ),
            ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            },
        )
        .await;
    assert!(result.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM agent_sessions")
            .fetch_one(&fixture.state.pool)
            .await
            .unwrap(),
        0
    );
}
