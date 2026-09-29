//! Context endpoint reranking against a local mock scoring server. Every
//! configuration here sets its own endpoint and key, so no test reads
//! TYPESAFE_API_KEY or reaches TypeSafe.

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    routing::post,
};
use coordinator_server::{
    auth::{digest, secret},
    context_rerank::{ApiKey, ContextRerankConfig, ContextRerankMode},
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
use tower::ServiceExt;
use uuid::Uuid;

/// A mock scoring server that counts requests and scores candidate `i` as
/// `min(i, 3)`, reversing short lists, under status `failure` when given.
#[derive(Clone)]
struct Mock {
    hits: Arc<AtomicUsize>,
    failure: Option<StatusCode>,
}

/// Score each requested candidate by its index. A failing mock sends the
/// same well-formed scores under its failure status.
async fn score_by_index(State(mock): State<Mock>, body: String) -> (StatusCode, String) {
    mock.hits.fetch_add(1, Ordering::SeqCst);
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
    hits: Arc<AtomicUsize>,
    _dir: tempfile::TempDir,
}

impl Fixture {
    /// Build the fixture; `configure` receives a TypeSafe-mode config with a
    /// test key aimed at a mock that fails with `failure` when given.
    async fn new(
        failure: Option<StatusCode>,
        configure: impl FnOnce(&mut ContextRerankConfig),
    ) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let endpoint = spawn_mock(Mock {
            hits: hits.clone(),
            failure,
        })
        .await;
        let mut rerank = ContextRerankConfig {
            mode: ContextRerankMode::Typesafe,
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
            hits,
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
        let (status, value) = call(app.clone(), &self.agent, "GET", path, json!({})).await;
        assert_eq!(status, StatusCode::OK, "{value}");
        let items = value["data"]["items"].as_array().unwrap();
        items.iter().map(label).collect()
    }

    /// Requests that reached the mock scoring server.
    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
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
async fn enabled_reranking_reorders_candidates_around_decisions() {
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
async fn reranking_is_off_by_default_and_makes_no_request() {
    let f = Fixture::new(None, |config| config.mode = ContextRerankMode::default()).await;
    let path = f.seed_context().await;
    let original = f.context(&f.plain, &path).await;
    assert_eq!(f.context(&f.reranked, &path).await, original);
    assert_eq!(f.hits(), 0);
}

#[tokio::test]
async fn enabled_without_a_key_behaves_as_off() {
    let f = Fixture::new(None, |config| config.api_key = None).await;
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
