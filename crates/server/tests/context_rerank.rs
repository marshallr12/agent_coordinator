//! Context endpoint reranking against a local mock scoring server. Every
//! configuration here sets its own endpoint and key, so no test reads
//! TYPESAFE_API_KEY or reaches TypeSafe. The startup-warning test sets the
//! variable to a literal or removes it only in the server processes it
//! spawns, which receive no context requests.

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    routing::post,
};
use coordinator_server::{
    auth::{digest, secret},
    context_rerank::{ApiKey, ContextRerankConfig},
    router,
    state::{AppState, Config},
};
use http_body_util::BodyExt;
use serde_json::{Map, Value, json};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::{Notify, Semaphore};
use tower::ServiceExt;
use uuid::Uuid;

/// A mock scoring server that counts requests and scores candidate `i` as
/// `min(i, 3)`, reversing short lists, under status `failure` when given.
#[derive(Clone)]
struct Mock {
    hits: Arc<AtomicUsize>,
    failure: Option<StatusCode>,
    arrived: Arc<Notify>,
    gate: Option<Arc<Semaphore>>,
}

impl Mock {
    fn new(failure: Option<StatusCode>) -> Self {
        Self {
            hits: Arc::new(AtomicUsize::new(0)),
            failure,
            arrived: Arc::new(Notify::new()),
            gate: None,
        }
    }

    async fn wait_for_hits(&self, expected: usize) {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while self.hits.load(Ordering::SeqCst) < expected {
                self.arrived.notified().await;
            }
        })
        .await
        .expect("context requests must reach the controlled scoring mock");
    }
}

/// Score each requested candidate by its index. A failing mock sends the
/// same well-formed scores under its failure status.
async fn score_by_index(State(mock): State<Mock>, body: String) -> (StatusCode, String) {
    mock.hits.fetch_add(1, Ordering::SeqCst);
    mock.arrived.notify_one();
    let request: Value = serde_json::from_str(&body).unwrap();
    let count = request["state"]["candidates"].as_array().unwrap().len();
    let answers: Map<String, Value> = (0..count)
        .map(|index| {
            let score = index.min(3) as f64;
            (
                format!("relevance_{index}"),
                json!({"type": "score", "score": score}),
            )
        })
        .collect();
    let status = mock.failure.unwrap_or(StatusCode::OK);
    if let Some(gate) = mock.gate {
        gate.acquire_owned().await.unwrap().forget();
    }
    (status, json!({"answers": answers}).to_string())
}

/// Serve `mock` on an ephemeral loopback port and return its URL.
async fn spawn_mock(mock: Mock) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/systemone", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/systemone", post(score_by_index))
        .with_state(mock);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

/// Authenticated callers seeded directly into the database.
struct Caller {
    token: String,
    session: String,
    proof: String,
    human: bool,
}

/// One database served by two routers: `plain` with reranking off and
/// `reranked` with the configuration under test.
struct Fixture {
    plain: Router,
    reranked: Router,
    admin: Caller,
    agent: Caller,
    mock: Mock,
    _dir: tempfile::TempDir,
}

impl Fixture {
    /// Build the fixture; `configure` receives a config with a literal test
    /// key aimed at a mock that fails with `failure` when given.
    async fn new(
        failure: Option<StatusCode>,
        configure: impl FnOnce(&mut ContextRerankConfig),
    ) -> Self {
        Self::with_mock(Mock::new(failure), configure).await
    }

    async fn with_mock(mock: Mock, configure: impl FnOnce(&mut ContextRerankConfig)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = spawn_mock(mock.clone()).await;
        let mut rerank = ContextRerankConfig {
            api_key: ApiKey::new("test-key".into()),
            endpoint,
            ..ContextRerankConfig::default()
        };
        configure(&mut rerank);
        let plain = open(dir.path(), ContextRerankConfig::default()).await;
        let reranked = open(dir.path(), rerank).await;
        Self {
            admin: seed(&plain, true).await,
            agent: seed(&plain, false).await,
            plain: router(plain),
            reranked: router(reranked),
            mock,
            _dir: dir,
        }
    }

    /// A project with three matching tasks and one pending decision; returns
    /// the context path for a query scoped to the first task.
    async fn seed_context(&self) -> String {
        let (_, project) = self.post(&self.admin, "/api/v1/projects",
            json!({"name":"rerank","repository_url":"https://example.test/rerank.git","target_branch":"main"})).await;
        let project = project["data"]["id"].as_str().unwrap().to_owned();
        let mut first = None;
        for title in [
            "Rerankneedle alpha",
            "Rerankneedle beta",
            "Rerankneedle gamma",
        ] {
            let (status, task) = self.post(&self.agent, &format!("/api/v1/projects/{project}/tasks"),
                json!({"title":title,"description":"rerank route test","acceptance_criteria":["ordered"],"kind":"general"})).await;
            assert_eq!(status, StatusCode::OK, "{task}");
            first.get_or_insert(task["data"]["id"].as_str().unwrap().to_owned());
        }
        let task = first.unwrap();
        let (status, decision) = self.post(&self.agent, &format!("/api/v1/projects/{project}/decisions"),
            json!({"question":"Proceed?","options":["Proceed","Wait"],"rationale":"Decisions stay first.",
                "required_actor":"human","affected_tasks":[{"task_id":task,"task_revision":1}],
                "policy_revision":1,"environment":"test","conditions":"None.","expires_at":null})).await;
        assert_eq!(status, StatusCode::OK, "{decision}");
        format!("/api/v1/projects/{project}/context?q=rerankneedle&limit=20&budget=65536")
    }

    /// POST `body` to `path` as `caller` through the plain router.
    async fn post(&self, caller: &Caller, path: &str, body: Value) -> (StatusCode, Value) {
        call(self.plain.clone(), caller, "POST", path, body).await
    }

    /// Item titles or decision questions from `app`'s context response.
    async fn context(&self, app: &Router, path: &str) -> Vec<String> {
        let value = self.packet(app, path).await;
        value["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(label)
            .collect()
    }

    /// Complete stable context data, including the item budget and policy.
    async fn packet(&self, app: &Router, path: &str) -> Value {
        let (status, value) = call(app.clone(), &self.agent, "GET", path, json!({})).await;
        assert_eq!(status, StatusCode::OK, "{value}");
        value["data"].clone()
    }

    /// Requests that reached the mock scoring server.
    fn hits(&self) -> usize {
        self.mock.hits.load(Ordering::SeqCst)
    }
}

/// A task or knowledge title, or `decision` for a decision item.
fn label(item: &Value) -> String {
    match item["type"].as_str().unwrap() {
        "decision" => "decision".into(),
        _ => item["record"]["title"].as_str().unwrap().into(),
    }
}

/// Open the shared test database with `context_rerank`.
async fn open(dir: &Path, context_rerank: ContextRerankConfig) -> AppState {
    AppState::open(Config {
        database_path: dir.join("rerank.sqlite3"),
        public_origin: "http://127.0.0.1:8080".into(),
        allow_insecure_loopback: true,
        context_rerank,
        ..Config::default()
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn a_key_reranks_candidates_around_decisions() {
    let f = Fixture::new(None, |_| {}).await;
    let path = f.seed_context().await;
    let original = f.context(&f.plain, &path).await;
    assert_eq!(original.len(), 4, "{original:?}");
    assert_eq!(original[0], "decision");
    assert_eq!(f.hits(), 0);
    let reranked = f.context(&f.reranked, &path).await;
    let mut expected = original.clone();
    expected[1..].reverse();
    assert_ne!(expected, original);
    assert_eq!(reranked, expected);
    assert_eq!(f.hits(), 1);
}

#[tokio::test]
async fn without_a_key_reranking_is_off_and_makes_no_request() {
    let f = Fixture::new(None, |config| config.api_key = None).await;
    let path = f.seed_context().await;
    let original = f.context(&f.plain, &path).await;
    assert_eq!(f.context(&f.reranked, &path).await, original);
    assert_eq!(f.hits(), 0);
}

#[tokio::test]
async fn a_blank_key_is_no_key_and_makes_no_request() {
    let f = Fixture::new(None, |config| config.api_key = ApiKey::new(" \t\n".into())).await;
    let path = f.seed_context().await;
    let original = f.context(&f.plain, &path).await;
    assert_eq!(f.context(&f.reranked, &path).await, original);
    assert_eq!(f.hits(), 0);
}

#[tokio::test]
async fn scoring_failure_keeps_the_search_order() {
    let f = Fixture::new(Some(StatusCode::BAD_GATEWAY), |_| {}).await;
    let path = f.seed_context().await;
    let original = f.context(&f.plain, &path).await;
    assert_eq!(f.context(&f.reranked, &path).await, original);
    assert_eq!(f.hits(), 1);
}

#[tokio::test]
async fn saturation_returns_the_complete_fts_packet_without_an_extra_provider_call() {
    let gate = Arc::new(Semaphore::new(0));
    let mut mock = Mock::new(None);
    mock.gate = Some(gate.clone());
    let f = Arc::new(Fixture::with_mock(mock, |_| {}).await);
    let path = f.seed_context().await;
    let original = f.packet(&f.plain, &path).await;
    assert_eq!(original["items"].as_array().unwrap().len(), 4);
    let mut pending = Vec::new();
    for _ in 0..4 {
        let shared = f.clone();
        let path = path.clone();
        pending.push(tokio::spawn(async move {
            shared.packet(&shared.reranked, &path).await
        }));
    }
    f.mock.wait_for_hits(4).await;
    assert_eq!(f.packet(&f.reranked, &path).await, original);
    assert_eq!(f.hits(), 4);
    gate.add_permits(4);
    let mut expected = original.clone();
    expected["items"].as_array_mut().unwrap()[1..].reverse();
    assert_ne!(expected["items"], original["items"]);
    for task in pending {
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap(),
            expected
        );
    }
    assert_eq!(f.hits(), 4);
}

#[tokio::test]
async fn three_provider_failures_open_the_endpoint_circuit_preserving_items_order_and_budget() {
    let f = Fixture::new(Some(StatusCode::BAD_GATEWAY), |_| {}).await;
    let path = f.seed_context().await;
    let original = f.packet(&f.plain, &path).await;
    for expected_hits in 1..=3 {
        assert_eq!(f.packet(&f.reranked, &path).await, original);
        assert_eq!(f.hits(), expected_hits);
    }
    for _ in 0..3 {
        assert_eq!(f.packet(&f.reranked, &path).await, original);
    }
    assert_eq!(f.hits(), 3);
}

/// The part of the serve-time warning that names the missing key.
const KEY_WARNING: &str = "TYPESAFE_API_KEY is not set";

/// Run the server binary on `database` with `args`, with TYPESAFE_API_KEY
/// set to `key` or removed. A `serve` run is killed once it logs its start
/// line; other commands run to completion. Returns the log lines, which the
/// binary writes to stdout, up to that point.
fn run_binary(database: &Path, args: &[&str], key: Option<&str>) -> Vec<String> {
    use std::{
        io::{BufRead, BufReader},
        process::{Command, Stdio},
        sync::mpsc,
        time::{Duration, Instant},
    };
    let mut command = Command::new(env!("CARGO_BIN_EXE_agent-coordinator-server"));
    command
        .arg("--database")
        .arg(database)
        .args(["--listen", "127.0.0.1:0"])
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    match key {
        Some(key) => command.env("TYPESAFE_API_KEY", key),
        None => command.env_remove("TYPESAFE_API_KEY"),
    };
    let mut child = command.spawn().unwrap();
    let (sender, receiver) = mpsc::channel();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    std::thread::spawn(move || {
        stdout
            .lines()
            .map_while(Result::ok)
            .try_for_each(|line| sender.send(line))
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut lines = Vec::new();
    while let Ok(line) = receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        let started = line.contains("Agent Coordinator service started");
        lines.push(line);
        if started {
            break;
        }
    }
    let _ = child.kill();
    let status = child.wait().unwrap();
    let serving = lines.iter().any(|line| line.contains("service started"));
    assert!(serving || status.success(), "{status}: {lines:?}");
    lines
}

/// Log lines from `lines` that carry the missing-key warning.
fn key_warnings(lines: &[String]) -> usize {
    lines
        .iter()
        .filter(|line| line.contains(KEY_WARNING))
        .count()
}

#[test]
fn only_serve_without_a_key_warns_once_at_startup() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("startup.sqlite3");
    let keyless = run_binary(&database, &["serve"], None);
    assert!(
        keyless.iter().any(|line| line.contains("service started")),
        "{keyless:?}"
    );
    assert_eq!(key_warnings(&keyless), 1, "{keyless:?}");
    let blank = run_binary(&database, &["serve"], Some("  "));
    assert_eq!(key_warnings(&blank), 1, "{blank:?}");
    let keyed = run_binary(&database, &["serve"], Some("test-key"));
    assert!(
        keyed.iter().any(|line| line.contains("service started")),
        "{keyed:?}"
    );
    assert_eq!(key_warnings(&keyed), 0, "{keyed:?}");
    let maintenance = run_binary(&database, &["maintenance"], None);
    assert!(!maintenance.is_empty(), "maintenance printed nothing");
    assert_eq!(key_warnings(&maintenance), 0, "{maintenance:?}");
}

/// Insert a human admin with a browser session, or an agent with a
/// credential and harness session.
async fn seed(state: &AppState, human: bool) -> Caller {
    let caller = Caller {
        token: secret(),
        session: Uuid::new_v4().to_string(),
        proof: secret(),
        human,
    };
    let principal = Uuid::new_v4().to_string();
    let credential = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO principals(id,name,kind,role,password_hash,created_at) VALUES(?,?,?,?,?,?)",
    )
    .bind(&principal)
    .bind(format!("rerank-{principal}"))
    .bind(if human { "human" } else { "agent" })
    .bind(if human { "admin" } else { "agent" })
    .bind(if human { Some("unused") } else { None })
    .bind(state.now())
    .execute(&state.pool)
    .await
    .unwrap();
    if human {
        sqlx::query(
            "INSERT INTO browser_sessions(id,principal_id,token_hash,expires_at) VALUES(?,?,?,?)",
        )
        .bind(&caller.session)
        .bind(&principal)
        .bind(digest(&caller.token))
        .bind(state.now() + 86_400_000)
        .execute(&state.pool)
        .await
        .unwrap();
    } else {
        sqlx::query(
            "INSERT INTO credentials(id,principal_id,token_hash,created_at) VALUES(?,?,?,?)",
        )
        .bind(&credential)
        .bind(&principal)
        .bind(digest(&caller.token))
        .bind(state.now())
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO agent_sessions(id,principal_id,credential_id,workstation_id,proof_hash,created_at,capabilities,harness) VALUES(?,?,?,?,?,?,'[]','test')")
            .bind(&caller.session).bind(&principal).bind(&credential).bind(format!("{principal}-workstation")).bind(digest(&caller.proof)).bind(state.now()).execute(&state.pool).await.unwrap();
    }
    caller
}

/// Send one authenticated JSON request through `app`.
async fn call(
    app: Router,
    caller: &Caller,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("idempotency-key", Uuid::new_v4().to_string());
    if caller.human {
        request = request
            .header("cookie", format!("coordinator_local={}", caller.token))
            .header("origin", "http://127.0.0.1:8080")
            .header(
                "x-csrf-token",
                digest(&format!("coordinator-browser-csrf-v1:{}", caller.token)),
            );
    } else {
        request = request
            .header("authorization", format!("Bearer {}", caller.token))
            .header("x-coordinator-session", &caller.session)
            .header("x-coordinator-session-proof", &caller.proof);
    }
    let response = app
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}
