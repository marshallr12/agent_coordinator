use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode},
};
use coordinator_server::{
    auth::{digest, secret},
    router,
    state::{AppState, Clock, Config},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;
use tower::ServiceExt;
use uuid::Uuid;

const ORIGIN: &str = "http://127.0.0.1:8080";
const MODERN_PROTOCOL: &str = "2026-07-28";
const LEGACY_PROTOCOL: &str = "2025-11-25";

struct TestClock(AtomicI64);

impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }

    fn use_monotonic_elapsed(&self) -> bool {
        false
    }
}

#[derive(Clone)]
struct Caller {
    token: String,
    session: String,
    proof: String,
    principal: String,
    credential: String,
    human: bool,
}

struct Fixture {
    state: AppState,
    app: Router,
    clock: Arc<TestClock>,
    _dir: tempfile::TempDir,
    admin: Caller,
    a: Caller,
    b: Caller,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::open(Config {
            database_path: dir.path().join("mcp.sqlite3"),
            public_origin: ORIGIN.into(),
            allow_insecure_loopback: true,
            ..Config::default()
        })
        .await
        .unwrap();
        let clock = Arc::new(TestClock(AtomicI64::new(1_800_000_000_000)));
        state.clock = clock.clone();
        let admin = seed(&state, true, "mcp-admin").await;
        let a = seed(&state, false, "mcp-a").await;
        let b = seed(&state, false, "mcp-b").await;
        Self {
            app: router(state.clone()),
            state,
            clock,
            _dir: dir,
            admin,
            a,
            b,
        }
    }

    async fn project(&self, name: &str) -> String {
        let reply = rest(
            self.app.clone(),
            &self.admin,
            "POST",
            "/api/v1/projects",
            &Uuid::new_v4().to_string(),
            json!({
                "name": name,
                "repository_url": "https://example.test/mcp.git",
                "target_branch": "main"
            }),
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK);
        reply.body["data"]["id"].as_str().unwrap().into()
    }

    async fn task(&self, project: &str, title: &str) -> Value {
        let reply = rest(
            self.app.clone(),
            &self.a,
            "POST",
            &format!("/api/v1/projects/{project}/tasks"),
            &Uuid::new_v4().to_string(),
            json!({
                "title": title,
                "description": "MCP integration test task",
                "acceptance_criteria": ["The guarded operation succeeds"],
                "kind": "general"
            }),
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK);
        reply.body["data"].clone()
    }

    async fn acknowledge(&self, caller: &Caller, project: &str) {
        let reply = rest(
            self.app.clone(),
            caller,
            "POST",
            &format!(
                "/api/v1/sessions/{}/instruction-acknowledgments",
                caller.session
            ),
            &Uuid::new_v4().to_string(),
            json!({
                "project_id": project,
                "policy_revision": 1,
                "instruction_version": coordinator_core::INSTRUCTION_VERSION,
                "sections": [coordinator_core::REQUIRED_SECTION]
            }),
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK);
    }

    async fn claim_rest(&self, caller: &Caller, project: &str, task: &Value) -> Value {
        self.acknowledge(caller, project).await;
        let reply = rest(
            self.app.clone(),
            caller,
            "POST",
            &format!("/api/v1/projects/{project}/claims"),
            &Uuid::new_v4().to_string(),
            claim_body(task),
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK);
        reply.body["data"]["claim"]["attempt"].clone()
    }

    async fn mcp(&self, caller: &Caller, request: Value) -> Reply {
        mcp(self.app.clone(), Some(caller), request, true).await
    }

    async fn tool(&self, caller: &Caller, name: &str, arguments: Value) -> Reply {
        self.mcp(
            caller,
            modern_rpc(
                "tools/call",
                json!({
                    "name": name,
                    "arguments": arguments
                }),
            ),
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
    fn tool_payload(&self) -> &Value {
        assert_eq!(self.status, StatusCode::OK);
        assert_ne!(self.body["result"]["isError"], true);
        let structured = &self.body["result"]["structuredContent"];
        let text = self.body["result"]["content"]
            .as_array()
            .and_then(|content| {
                content.iter().find_map(|item| {
                    (item["type"] == "text")
                        .then(|| item["text"].as_str())
                        .flatten()
                })
            })
            .expect("tool result includes its JSON text projection");
        assert_eq!(serde_json::from_str::<Value>(text).unwrap(), *structured);
        structured
    }

    fn tool_error(&self, code: &str) {
        assert_eq!(self.status, StatusCode::OK);
        assert_eq!(self.body["result"]["isError"], true);
        assert_eq!(
            self.body["result"]["structuredContent"]["error"]["code"],
            code
        );
    }

    fn auth_error(&self) {
        assert_eq!(self.status, StatusCode::UNAUTHORIZED);
        assert_eq!(self.body["error"]["code"], "authentication_required");
    }
}

async fn seed(state: &AppState, human: bool, name: &str) -> Caller {
    let caller = Caller {
        token: secret(),
        session: Uuid::new_v4().to_string(),
        proof: secret(),
        principal: Uuid::new_v4().to_string(),
        credential: Uuid::new_v4().to_string(),
        human,
    };
    sqlx::query(
        "INSERT INTO principals(id,name,kind,role,password_hash,created_at) VALUES(?,?,?,?,?,?)",
    )
    .bind(&caller.principal)
    .bind(name)
    .bind(if human { "human" } else { "agent" })
    .bind(if human { "admin" } else { "agent" })
    .bind(if human {
        Some("unused-test-hash")
    } else {
        None
    })
    .bind(state.now())
    .execute(&state.pool)
    .await
    .unwrap();
    if human {
        sqlx::query(
            "INSERT INTO browser_sessions(id,principal_id,token_hash,expires_at) VALUES(?,?,?,?)",
        )
        .bind(&caller.session)
        .bind(&caller.principal)
        .bind(digest(&caller.token))
        .bind(state.now() + 86_400_000)
        .execute(&state.pool)
        .await
        .unwrap();
    } else {
        sqlx::query(
            "INSERT INTO credentials(id,principal_id,token_hash,created_at) VALUES(?,?,?,?)",
        )
        .bind(&caller.credential)
        .bind(&caller.principal)
        .bind(digest(&caller.token))
        .bind(state.now())
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO agent_sessions(id,principal_id,credential_id,workstation_id,proof_hash,created_at,capabilities,harness) VALUES(?,?,?,?,?,?,'[]','mcp-test')")
            .bind(&caller.session)
            .bind(&caller.principal)
            .bind(&caller.credential)
            .bind(name)
            .bind(digest(&caller.proof))
            .bind(state.now())
            .execute(&state.pool)
            .await
            .unwrap();
    }
    caller
}

async fn rest(
    app: Router,
    caller: &Caller,
    method: &str,
    path: &str,
    key: &str,
    body: Value,
) -> Reply {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("idempotency-key", key);
    if caller.human {
        request = request
            .header("cookie", format!("coordinator_local={}", caller.token))
            .header("origin", ORIGIN)
            .header(
                "x-csrf-token",
                digest(&format!("coordinator-browser-csrf-v1:{}", caller.token)),
            );
    } else {
        request = coordinator_headers(request, caller);
    }
    send(app, request.body(Body::from(body.to_string())).unwrap()).await
}

fn coordinator_headers(
    request: axum::http::request::Builder,
    caller: &Caller,
) -> axum::http::request::Builder {
    request
        .header("authorization", format!("Bearer {}", caller.token))
        .header("x-coordinator-session", &caller.session)
        .header("x-coordinator-session-proof", &caller.proof)
}

async fn mcp(app: Router, caller: Option<&Caller>, request: Value, modern: bool) -> Reply {
    mcp_raw(app, caller, request.to_string(), modern, None).await
}

async fn mcp_raw(
    app: Router,
    caller: Option<&Caller>,
    body: String,
    modern: bool,
    cookie: Option<&str>,
) -> Reply {
    let parsed = serde_json::from_str::<Value>(&body).ok();
    let method = parsed
        .as_ref()
        .and_then(|value| value["method"].as_str())
        .unwrap_or("tools/list");
    let mut request = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", "127.0.0.1:8080")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream");
    if modern {
        request = request
            .header("mcp-protocol-version", MODERN_PROTOCOL)
            .header("mcp-method", method);
        if let Some(name) = parsed
            .as_ref()
            .and_then(|value| value.pointer("/params/name"))
            .and_then(Value::as_str)
        {
            request = request.header("mcp-name", name);
        }
    }
    if let Some(caller) = caller {
        request = coordinator_headers(request, caller);
    }
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    send(app, request.body(Body::from(body)).unwrap()).await
}

async fn send(app: Router, request: Request<Body>) -> Reply {
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let text = String::from_utf8(
        response
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

fn modern_rpc(method: &str, mut params: Value) -> Value {
    let params = params.as_object_mut().unwrap();
    params.insert(
        "_meta".into(),
        json!({
            "io.modelcontextprotocol/protocolVersion": MODERN_PROTOCOL,
            "io.modelcontextprotocol/clientInfo": {
                "name": "agent-coordinator-wire-test",
                "version": "1"
            },
            "io.modelcontextprotocol/clientCapabilities": {}
        }),
    );
    json!({
        "jsonrpc": "2.0",
        "id": Uuid::new_v4().to_string(),
        "method": method,
        "params": params
    })
}

fn claim_body(task: &Value) -> Value {
    json!({
        "task_id": task["id"],
        "expected_task_revision": task["revision"],
        "mode": "work",
        "policy_revision": 1,
        "instruction_version": coordinator_core::INSTRUCTION_VERSION
    })
}

fn tool_args(body: Value, key: &str) -> Value {
    json!({"body": body, "idempotency_key": key})
}

fn activity<'a>(snapshot: &'a Value, kind: &str) -> &'a Value {
    snapshot["activities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|activity| activity["kind"] == kind && activity["status"] != "canceled")
        .unwrap()
}

#[tokio::test]
async fn bearer_authentication_precedes_parsing_and_browser_cookies_are_rejected() {
    let fixture = Fixture::new().await;
    let invalid = Caller {
        token: secret(),
        ..fixture.a.clone()
    };
    let browser_cookie = format!("coordinator_local={}", fixture.admin.token);
    for reply in [
        mcp_raw(fixture.app.clone(), None, "not-json".into(), true, None).await,
        mcp_raw(
            fixture.app.clone(),
            Some(&invalid),
            "not-json".into(),
            true,
            None,
        )
        .await,
        mcp_raw(
            fixture.app.clone(),
            None,
            "not-json".into(),
            true,
            Some(&browser_cookie),
        )
        .await,
        mcp_raw(
            fixture.app.clone(),
            Some(&fixture.a),
            "not-json".into(),
            true,
            Some(&browser_cookie),
        )
        .await,
    ] {
        reply.auth_error();
        assert!(!reply.text.contains(&fixture.a.token));
        assert!(!reply.text.contains(&fixture.a.proof));
        assert!(!reply.text.contains(&fixture.admin.token));
    }
    sqlx::query("UPDATE credentials SET revoked_at=? WHERE id=?")
        .bind(fixture.state.now())
        .bind(&fixture.a.credential)
        .execute(&fixture.state.pool)
        .await
        .unwrap();
    mcp_raw(
        fixture.app.clone(),
        Some(&fixture.a),
        "not-json".into(),
        true,
        None,
    )
    .await
    .auth_error();
}

#[tokio::test]
async fn modern_and_legacy_discovery_expose_only_the_fixed_safe_catalog() {
    let fixture = Fixture::new().await;
    let discover = fixture
        .mcp(&fixture.a, modern_rpc("server/discover", json!({})))
        .await;
    assert_eq!(discover.status, StatusCode::OK);
    assert!(discover.body["result"].is_object());
    assert!(discover.body["result"]["capabilities"]["tools"].is_object());

    let modern = fixture
        .mcp(&fixture.a, modern_rpc("tools/list", json!({})))
        .await;
    assert_eq!(modern.status, StatusCode::OK);
    let tools = modern.body["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 56);
    let names: Vec<_> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    for required in [
        "coordinator_session_register",
        "coordinator_claim",
        "coordinator_checkpoint",
        "coordinator_submit",
        "coordinator_activity_claim",
        "coordinator_review",
        "coordinator_context",
        "coordinator_task_history",
    ] {
        assert!(names.contains(&required));
    }
    for forbidden in [
        "coordinator_request",
        "coordinator_credentials_create",
        "coordinator_job_register",
        "coordinator_reporter_create",
        "coordinator_artifact_upload",
        "coordinator_artifact_download",
        "coordinator_integration_authorize",
    ] {
        assert!(!names.contains(&forbidden));
    }
    assert!(tools.iter().all(|tool| {
        tool["inputSchema"]["type"] == "object"
            && tool["inputSchema"]["additionalProperties"] == false
    }));

    let legacy = mcp(
        fixture.app.clone(),
        Some(&fixture.a),
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": LEGACY_PROTOCOL,
                "capabilities": {},
                "clientInfo": {"name": "legacy-wire-test", "version": "1"}
            }
        }),
        false,
    )
    .await;
    assert_eq!(legacy.status, StatusCode::OK);
    assert_eq!(legacy.body["result"]["protocolVersion"], LEGACY_PROTOCOL);
    assert!(!legacy.headers.contains_key("mcp-session-id"));

    let mut legacy_list = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", "127.0.0.1:8080")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", LEGACY_PROTOCOL);
    legacy_list = coordinator_headers(legacy_list, &fixture.a);
    let legacy_list = send(
        fixture.app.clone(),
        legacy_list
            .body(Body::from(
                json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(legacy_list.status, StatusCode::OK);
    assert_eq!(
        legacy_list.body["result"]["tools"],
        modern.body["result"]["tools"]
    );
}

#[tokio::test]
async fn protocol_header_mismatch_and_unknown_versions_are_rejected() {
    let fixture = Fixture::new().await;
    let body = modern_rpc("ping", json!({}));
    let mut mismatch = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", "127.0.0.1:8080")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", MODERN_PROTOCOL)
        .header("mcp-method", "tools/list");
    mismatch = coordinator_headers(mismatch, &fixture.a);
    let mismatch = send(
        fixture.app.clone(),
        mismatch.body(Body::from(body.to_string())).unwrap(),
    )
    .await;
    assert!(mismatch.status.is_client_error() || mismatch.body["error"]["code"].as_i64().is_some());

    let mut unknown = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", "127.0.0.1:8080")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "1999-01-01")
        .header("mcp-method", "tools/list");
    unknown = coordinator_headers(unknown, &fixture.a);
    let unknown = send(
        fixture.app.clone(),
        unknown
            .body(Body::from(
                json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert!(unknown.status.is_client_error() || unknown.body["error"]["code"].is_i64());
    for reply in [mismatch, unknown] {
        assert!(!reply.text.contains(&fixture.a.token));
        assert!(!reply.text.contains(&fixture.a.proof));
    }
}

#[tokio::test]
async fn session_registration_is_credential_bootstrap_and_binds_header_identity() {
    let fixture = Fixture::new().await;
    sqlx::query("DELETE FROM agent_sessions WHERE id=?")
        .bind(&fixture.a.session)
        .execute(&fixture.state.pool)
        .await
        .unwrap();
    let body = json!({
        "session_id": fixture.a.session,
        "workstation_id": "mcp-workstation",
        "harness": "mcp-wire-test",
        "capabilities": ["coordination"]
    });
    let mismatch = fixture
        .tool(
            &fixture.a,
            "coordinator_session_register",
            tool_args(
                json!({"session_id":"different-session","workstation_id":"mcp-workstation","harness":"mcp-wire-test","capabilities":[]}),
                "session-mismatch-key",
            ),
        )
        .await;
    mismatch.tool_error("invalid_request");
    let registered = fixture
        .tool(
            &fixture.a,
            "coordinator_session_register",
            tool_args(body.clone(), "session-register-key"),
        )
        .await;
    assert_eq!(registered.tool_payload()["data"]["id"], fixture.a.session);
    let replay = fixture
        .tool(
            &fixture.a,
            "coordinator_session_register",
            tool_args(body, "session-register-key"),
        )
        .await;
    assert_eq!(
        registered.tool_payload()["data"],
        replay.tool_payload()["data"]
    );
    for reply in [registered, replay] {
        assert!(!reply.text.contains(&fixture.a.token));
        assert!(!reply.text.contains(&fixture.a.proof));
    }
}

#[tokio::test]
async fn catalog_rejects_path_query_and_wrapper_injection_before_dispatch() {
    let fixture = Fixture::new().await;
    for arguments in [
        json!({"project":"../admin"}),
        json!({"project":"safe","query":{"cursor":"x&limit=200","url":"https://example.test"}}),
        json!({"project":"safe","method":"DELETE","url":"https://example.test"}),
    ] {
        fixture
            .tool(&fixture.a, "coordinator_tasks_list", arguments)
            .await
            .tool_error("invalid_request");
    }
    let attempts: i64 = sqlx::query_scalar("SELECT count(*) FROM attempts")
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap();
    assert_eq!(attempts, 0);
}

#[tokio::test]
async fn two_mcp_harnesses_compete_through_the_same_atomic_claim_guard() {
    let fixture = Fixture::new().await;
    let project = fixture.project("mcp-claim-race").await;
    let task = fixture.task(&project, "One owner").await;
    fixture.acknowledge(&fixture.a, &project).await;
    fixture.acknowledge(&fixture.b, &project).await;
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut calls = Vec::new();
    for (caller, key) in [
        (fixture.a.clone(), "mcp-race-claim-a"),
        (fixture.b.clone(), "mcp-race-claim-b"),
    ] {
        let app = fixture.app.clone();
        let project = project.clone();
        let body = claim_body(&task);
        let barrier = barrier.clone();
        calls.push(tokio::spawn(async move {
            barrier.wait().await;
            mcp(
                app,
                Some(&caller),
                modern_rpc(
                    "tools/call",
                    json!({
                        "name":"coordinator_claim",
                        "arguments":{
                            "project":project,
                            "body":body,
                            "idempotency_key":key
                        }
                    }),
                ),
                true,
            )
            .await
        }));
    }
    let mut success = 0;
    let mut conflict = 0;
    for call in calls {
        let reply = call.await.unwrap();
        if reply.body["result"]["isError"] == true {
            assert_eq!(
                reply.body["result"]["structuredContent"]["error"]["code"],
                "claim_conflict"
            );
            conflict += 1;
        } else {
            reply.tool_payload();
            success += 1;
        }
    }
    assert_eq!((success, conflict), (1, 1));
    let attempts: i64 = sqlx::query_scalar("SELECT count(*) FROM attempts WHERE project_id=?")
        .bind(&project)
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap();
    assert_eq!(attempts, 1);
}

#[tokio::test]
async fn generation_and_session_authority_are_rechecked_by_guarded_routes() {
    let fixture = Fixture::new().await;
    let project = fixture.project("mcp-authority").await;
    let task = fixture.task(&project, "Authority guarded").await;
    let attempt = fixture.claim_rest(&fixture.a, &project, &task).await;
    let attempt_id = attempt["id"].as_str().unwrap();
    let wrong_generation = fixture
        .tool(
            &fixture.a,
            "coordinator_checkpoint",
            json!({
                "project":project,
                "attempt":attempt_id,
                "body":{"generation":attempt["generation"].as_i64().unwrap()+1,"summary":"stale"},
                "idempotency_key":"stale-generation-key"
            }),
        )
        .await;
    wrong_generation.tool_error("lease_expired");
    let wrong_session = fixture
        .tool(
            &fixture.b,
            "coordinator_checkpoint",
            json!({
                "project":project,
                "attempt":attempt_id,
                "body":{"generation":attempt["generation"],"summary":"borrowed"},
                "idempotency_key":"wrong-session-key"
            }),
        )
        .await;
    wrong_session.tool_error("operation_not_permitted");
    let checkpoints: i64 = sqlx::query_scalar("SELECT count(*) FROM checkpoints")
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap();
    assert_eq!(checkpoints, 0);
}

#[tokio::test]
async fn mcp_replays_the_exact_rest_receipt_once_and_rejects_changed_input() {
    let fixture = Fixture::new().await;
    let project = fixture.project("mcp-receipt").await;
    let body = json!({
        "title":"Exactly once",
        "description":"Created through MCP",
        "acceptance_criteria":["One task exists"],
        "kind":"general"
    });
    let arguments = json!({
        "project":project,
        "body":body,
        "idempotency_key":"mcp-exact-receipt-key"
    });
    let first = fixture
        .tool(&fixture.a, "coordinator_task_create", arguments.clone())
        .await;
    let replay = fixture
        .tool(&fixture.a, "coordinator_task_create", arguments)
        .await;
    assert_eq!(first.tool_payload()["data"], replay.tool_payload()["data"]);
    let mut changed = body;
    changed["title"] = json!("Changed input");
    fixture
        .tool(
            &fixture.a,
            "coordinator_task_create",
            json!({
                "project":project,
                "body":changed,
                "idempotency_key":"mcp-exact-receipt-key"
            }),
        )
        .await
        .tool_error("idempotency_conflict");
    let tasks: i64 = sqlx::query_scalar("SELECT count(*) FROM tasks WHERE project_id=?")
        .bind(&project)
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap();
    assert_eq!(tasks, 1);
}

#[tokio::test]
async fn revocation_while_mcp_body_is_waiting_is_observed_before_the_inner_write() {
    let fixture = Fixture::new().await;
    let project = fixture.project("mcp-revocation-race").await;
    let task = fixture.task(&project, "Never claimed").await;
    fixture.acknowledge(&fixture.a, &project).await;
    let initial_clock: i64 =
        sqlx::query_scalar("SELECT last_safe_time_ms FROM clock_state WHERE singleton=1")
            .fetch_one(&fixture.state.pool)
            .await
            .unwrap();
    fixture.clock.0.store(initial_clock + 1, Ordering::SeqCst);

    let (reader, mut writer) = tokio::io::duplex(16 * 1024);
    let body = modern_rpc(
        "tools/call",
        json!({
            "name":"coordinator_claim",
            "arguments":{
                "project":project,
                "body":claim_body(&task),
                "idempotency_key":"revoked-queued-claim"
            }
        }),
    )
    .to_string();
    let mut request = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", "127.0.0.1:8080")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", MODERN_PROTOCOL)
        .header("mcp-method", "tools/call")
        .header("mcp-name", "coordinator_claim");
    request = coordinator_headers(request, &fixture.a);
    let app = fixture.app.clone();
    let pending = tokio::spawn(async move {
        send(
            app,
            request
                .body(Body::from_stream(ReaderStream::new(reader)))
                .unwrap(),
        )
        .await
    });

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let sampled: i64 =
                sqlx::query_scalar("SELECT last_safe_time_ms FROM clock_state WHERE singleton=1")
                    .fetch_one(&fixture.state.pool)
                    .await
                    .unwrap();
            if sampled == initial_clock + 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let mut tx = fixture
        .state
        .pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .unwrap();
    writer.write_all(body.as_bytes()).await.unwrap();
    writer.shutdown().await.unwrap();
    sqlx::query("UPDATE credentials SET revoked_at=? WHERE id=?")
        .bind(initial_clock + 1)
        .bind(&fixture.a.credential)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    pending.await.unwrap().tool_error("authentication_required");
    let attempts: i64 = sqlx::query_scalar("SELECT count(*) FROM attempts WHERE project_id=?")
        .bind(&project)
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap();
    assert_eq!(attempts, 0);
}

#[tokio::test]
async fn policy_delegation_and_independent_review_boundaries_survive_mcp_dispatch() {
    let fixture = Fixture::new().await;
    let project = fixture.project("mcp-policy-review").await;
    fixture
        .tool(
            &fixture.a,
            "coordinator_policy_update",
            json!({
                "project":project,
                "body":{
                    "expected_revision":1,
                    "review_mode":"agent",
                    "recovery_mode":"agent",
                    "lease_seconds":600,
                    "rules":"Agent attempted an undelegated edit.",
                    "agent_rule_editing":false,
                    "automatic_integration":true
                },
                "idempotency_key":"undelegated-policy-edit"
            }),
        )
        .await
        .tool_error("operation_not_permitted");

    let task = fixture.task(&project, "Independent review").await;
    let attempt = fixture.claim_rest(&fixture.a, &project, &task).await;
    let submission = rest(
        fixture.app.clone(),
        &fixture.a,
        "POST",
        &format!(
            "/api/v1/projects/{project}/attempts/{}/submissions",
            attempt["id"].as_str().unwrap()
        ),
        "mcp-review-submission",
        json!({
            "generation":attempt["generation"],
            "task_revision":task["revision"],
            "project_policy_revision":1,
            "workflow_policy_revision":0,
            "kind":"general",
            "summary":"Ready for independent review",
            "acceptance_evidence":[{"criterion":"The guarded operation succeeds","evidence":"Observed"}],
            "handoff":"Review the immutable evidence",
            "repository":null,
            "base_revision":null,
            "candidate_revision":null,
            "candidate_tree":null
        }),
    )
    .await;
    assert_eq!(submission.status, StatusCode::OK);
    let review = activity(&submission.body["data"], "agent_review");
    fixture
        .tool(
            &fixture.a,
            "coordinator_activity_claim",
            json!({
                "project":project,
                "activity":review["id"],
                "body":{
                    "expected_submission_id":review["submission_id"],
                    "expected_project_policy_revision":1,
                    "expected_workflow_policy_revision":0
                },
                "idempotency_key":"self-review-claim"
            }),
        )
        .await
        .tool_error("reviewer_not_independent");
}

#[tokio::test]
async fn protocol_ping_does_not_renew_coordinator_attempt_authority() {
    let fixture = Fixture::new().await;
    let project = fixture.project("mcp-ping").await;
    let task = fixture.task(&project, "Ping is not renewal").await;
    let attempt = fixture.claim_rest(&fixture.a, &project, &task).await;
    let before: i64 = sqlx::query_scalar("SELECT expires_at FROM attempts WHERE id=?")
        .bind(attempt["id"].as_str().unwrap())
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap();
    fixture.clock.0.fetch_add(590_000, Ordering::SeqCst);
    let ping = mcp(
        fixture.app.clone(),
        Some(&fixture.a),
        json!({"jsonrpc":"2.0","id":1,"method":"ping","params":{}}),
        false,
    )
    .await;
    assert_eq!(ping.status, StatusCode::OK);
    assert!(ping.body["result"].is_object());
    let after: i64 = sqlx::query_scalar("SELECT expires_at FROM attempts WHERE id=?")
        .bind(attempt["id"].as_str().unwrap())
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap();
    assert_eq!(after, before);
    fixture.clock.0.fetch_add(10_000, Ordering::SeqCst);
    fixture
        .tool(
            &fixture.a,
            "coordinator_attempt_renew",
            json!({
                "project":project,
                "attempt":attempt["id"],
                "body":{"generation":attempt["generation"]},
                "idempotency_key":"late-after-ping-renew"
            }),
        )
        .await
        .tool_error("lease_expired");
}

#[tokio::test]
async fn clock_reconciliation_pause_blocks_mcp_mutations_without_effect() {
    let fixture = Fixture::new().await;
    let project = fixture.project("mcp-clock-pause").await;
    fixture.clock.0.fetch_add(10_000, Ordering::SeqCst);
    let read = fixture
        .tool(
            &fixture.a,
            "coordinator_tasks_list",
            json!({"project":project}),
        )
        .await;
    read.tool_payload();
    fixture.clock.0.fetch_sub(10_000, Ordering::SeqCst);
    let blocked = fixture
        .tool(
            &fixture.a,
            "coordinator_task_create",
            json!({
                "project":project,
                "body":{
                    "title":"Blocked by clock pause",
                    "description":"Must not be created",
                    "acceptance_criteria":["No effect"],
                    "kind":"general"
                },
                "idempotency_key":"clock-paused-create"
            }),
        )
        .await;
    blocked.tool_error("clock_reconciliation_required");
    let tasks: i64 = sqlx::query_scalar("SELECT count(*) FROM tasks WHERE project_id=?")
        .bind(&project)
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap();
    assert_eq!(tasks, 0);
}
