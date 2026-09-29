//! Unit tests for context reranking against a local mock scoring server.
//! Every reranker here targets 127.0.0.1, so no test can reach TypeSafe.

use super::*;
use axum::{
    Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::post,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

/// The `Authorization` header and parsed JSON body of one received request.
type Received = (Option<String>, Value);

/// A canned mock response plus a record of what the mock received.
#[derive(Clone)]
struct Mock {
    status: StatusCode,
    location: Option<String>,
    body: String,
    delay: Duration,
    hits: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<Received>>>,
}

impl Mock {
    /// A mock answering `status` with `body` immediately.
    fn new(status: StatusCode, body: String) -> Self {
        Self {
            status,
            location: None,
            body,
            delay: Duration::ZERO,
            hits: Arc::default(),
            requests: Arc::default(),
        }
    }

    /// Serve this mock on an ephemeral loopback port; returns its URL.
    async fn spawn(&self) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1/systemone", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/v1/systemone", post(respond))
            .with_state(self.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    /// How many requests reached the mock.
    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

/// Record the request, wait the configured delay, then send the canned reply.
async fn respond(State(mock): State<Mock>, headers: HeaderMap, body: String) -> impl IntoResponse {
    mock.hits.fetch_add(1, Ordering::SeqCst);
    let auth = headers
        .get(header::AUTHORIZATION)
        .map(|value| value.to_str().unwrap().to_owned());
    let parsed = serde_json::from_str(&body).unwrap_or(Value::Null);
    mock.requests.lock().unwrap().push((auth, parsed));
    tokio::time::sleep(mock.delay).await;
    let mut reply = HeaderMap::new();
    if let Some(location) = &mock.location {
        reply.insert(header::LOCATION, location.parse().unwrap());
    }
    (mock.status, reply, mock.body.clone())
}

/// A keyed reranker aimed at `endpoint` with a short test timeout.
fn reranker(endpoint: String) -> ContextReranker {
    let config = ContextRerankConfig {
        api_key: ApiKey::new("test-key".into()),
        endpoint,
        timeout: Duration::from_millis(300),
        ..ContextRerankConfig::default()
    };
    ContextReranker::from_config(&config).unwrap().unwrap()
}

/// A response scoring candidate `index` with `answers[index]`.
fn scores_body(answers: &[Value]) -> String {
    let answers: Map<String, Value> = answers
        .iter()
        .enumerate()
        .map(|(index, answer)| (format!("relevance_{index}"), answer.clone()))
        .collect();
    json!({"answers": answers}).to_string()
}

/// A well-formed score answer.
fn score(value: f64) -> Value {
    json!({"type": "score", "score": value})
}

/// Task A, decision D, knowledge B, task C: three candidates around a decision.
fn items() -> Vec<Value> {
    vec![
        json!({"type": "task", "record": {"title": "A", "description": "a", "acceptance_criteria": ["done"]}}),
        json!({"type": "decision", "record": {"title": "D"}}),
        json!({"type": "knowledge", "record": {"title": "B", "body": "b", "applicability": "linux"}}),
        json!({"type": "task", "record": {"title": "C", "description": "c", "acceptance_criteria": []}}),
    ]
}

/// The record titles of `items`, in order.
fn titles(items: &[Value]) -> Vec<&str> {
    items
        .iter()
        .map(|item| item["record"]["title"].as_str().unwrap())
        .collect()
}

/// Run one attempt over `items()` against a mock answering `status` and `body`.
async fn attempt_with(status: StatusCode, body: String) -> (Result<Vec<Value>, Unchanged>, Mock) {
    let mock = Mock::new(status, body);
    let reranker = reranker(mock.spawn().await);
    let items = items();
    let positions = candidate_positions(&items);
    let result = reranker.attempt("query", &items, &positions).await;
    (result, mock)
}

#[tokio::test]
async fn reorders_candidates_by_score_keeping_other_items_and_breaking_ties_by_index() {
    let body = scores_body(&[score(1.0), score(3.0), score(1.0)]);
    let mock = Mock::new(StatusCode::OK, body);
    let reranker = reranker(mock.spawn().await);
    let reordered = reranker.rerank("query", items()).await;
    assert_eq!(titles(&reordered), ["B", "D", "A", "C"]);
    assert_eq!(mock.hits(), 1);
}

#[tokio::test]
async fn request_carries_the_key_model_query_and_bounded_excerpts() {
    let mock = Mock::new(StatusCode::OK, scores_body(&[score(0.0), score(0.0)]));
    let reranker = reranker(mock.spawn().await);
    let long = "x".repeat(MAX_EXCERPT_CHARS + 50);
    let items = vec![
        json!({"type": "task", "record": {"title": "T", "description": long, "acceptance_criteria": ["ok"]}}),
        json!({"type": "knowledge", "record": {"title": "K", "body": "kb", "applicability": "ka"}}),
    ];
    reranker.rerank("the query", items).await;
    let (auth, body) = mock.requests.lock().unwrap()[0].clone();
    assert_eq!(auth.as_deref(), Some("Bearer test-key"));
    assert_eq!(body["model"], TYPESAFE_MODEL);
    assert_eq!(body["state"]["query"], "the query");
    let task_text = body["state"]["candidates"][0]["text"].as_str().unwrap();
    assert_eq!(task_text.chars().count(), MAX_EXCERPT_CHARS);
    assert_eq!(body["state"]["candidates"][1]["text"], "K\nkb\nka");
    assert_eq!(body["questions"]["relevance_1"]["type"], "score");
}

#[tokio::test]
async fn invalid_missing_or_out_of_range_scores_keep_the_original_order() {
    let cases = [
        scores_body(&[score(1.0), score(2.0)]),
        scores_body(&[score(1.0), score(3.5), score(2.0)]),
        scores_body(&[score(1.0), score(-0.5), score(2.0)]),
        scores_body(&[
            score(1.0),
            json!({"type": "choice", "score": 2.0}),
            score(2.0),
        ]),
        scores_body(&[
            score(1.0),
            json!({"type": "score", "score": "2"}),
            score(2.0),
        ]),
        json!({"answers": []}).to_string(),
        "not json".to_owned(),
    ];
    for body in cases {
        let (result, mock) = attempt_with(StatusCode::OK, body.clone()).await;
        assert_eq!(result, Err(Unchanged::Failed("invalid_response")), "{body}");
        assert_eq!(mock.hits(), 1);
    }
}

#[tokio::test]
async fn non_success_status_keeps_the_original_order() {
    let body = scores_body(&[score(0.0), score(3.0), score(1.0)]);
    let (result, _) = attempt_with(StatusCode::INTERNAL_SERVER_ERROR, body).await;
    assert_eq!(result, Err(Unchanged::Failed("status")));
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let target = Mock::new(
        StatusCode::OK,
        scores_body(&[score(0.0), score(3.0), score(1.0)]),
    );
    let mut redirect = Mock::new(StatusCode::TEMPORARY_REDIRECT, String::new());
    redirect.location = Some(target.spawn().await);
    let reranker = reranker(redirect.spawn().await);
    let items = items();
    let result = reranker
        .attempt("q", &items, &candidate_positions(&items))
        .await;
    assert_eq!(result, Err(Unchanged::Failed("status")));
    assert_eq!(target.hits(), 0);
}

#[tokio::test]
async fn responses_over_256_kib_keep_the_original_order() {
    let valid = scores_body(&[score(0.0), score(3.0), score(1.0)]);
    let padded_to = |length: usize| format!("{valid}{}", " ".repeat(length - valid.len()));
    let (at_cap, _) = attempt_with(StatusCode::OK, padded_to(256 * 1024)).await;
    assert_eq!(titles(&at_cap.unwrap()), ["B", "D", "C", "A"]);
    let (over_cap, _) = attempt_with(StatusCode::OK, padded_to(256 * 1024 + 1)).await;
    assert_eq!(over_cap, Err(Unchanged::Failed("oversized")));
}

#[tokio::test]
async fn slow_response_times_out_and_keeps_the_original_order() {
    let mut mock = Mock::new(
        StatusCode::OK,
        scores_body(&[score(0.0), score(3.0), score(1.0)]),
    );
    mock.delay = Duration::from_secs(2);
    let reranker = reranker(mock.spawn().await);
    let started = Instant::now();
    let reordered = reranker.rerank("q", items()).await;
    assert_eq!(titles(&reordered), ["A", "D", "B", "C"]);
    assert!(started.elapsed() < Duration::from_secs(1));
    let items = items();
    let result = reranker
        .attempt("q", &items, &candidate_positions(&items))
        .await;
    assert_eq!(result, Err(Unchanged::Failed("timeout")));
}

#[tokio::test]
async fn candidate_counts_outside_the_range_make_no_request() {
    let mock = Mock::new(StatusCode::OK, scores_body(&[score(3.0)]));
    let reranker = reranker(mock.spawn().await);
    let one = vec![items().remove(0), items().remove(1)];
    let many: Vec<_> = (0..=MAX_CANDIDATES).map(|_| items().remove(0)).collect();
    let few = reranker
        .attempt("q", &one, &candidate_positions(&one))
        .await;
    assert_eq!(few, Err(Unchanged::Skipped("too_few_candidates")));
    let lots = reranker
        .attempt("q", &many, &candidate_positions(&many))
        .await;
    assert_eq!(lots, Err(Unchanged::Skipped("too_many_candidates")));
    assert_eq!(reranker.rerank("q", one.clone()).await, one);
    assert_eq!(mock.hits(), 0);
}

#[test]
fn only_keyed_configurations_build_a_reranker() {
    let keyed = ContextRerankConfig {
        api_key: ApiKey::new("test-key".into()),
        ..ContextRerankConfig::default()
    };
    assert!(ContextReranker::from_config(&keyed).unwrap().is_some());
    let unset = ContextRerankConfig::default();
    assert!(ContextReranker::from_config(&unset).unwrap().is_none());
    let blank = ContextRerankConfig {
        api_key: ApiKey::new(" \n ".into()),
        ..ContextRerankConfig::default()
    };
    assert!(blank.api_key.is_none());
    assert!(ContextReranker::from_config(&blank).unwrap().is_none());
}

#[test]
fn api_keys_drop_surrounding_whitespace() {
    let key = ApiKey::new(" test-key\n".into()).unwrap();
    assert_eq!(key.0, "test-key");
}

#[test]
fn defaults_have_no_key_the_production_endpoint_and_a_redacted_key() {
    let config = ContextRerankConfig::default();
    assert!(config.api_key.is_none());
    assert_eq!(config.endpoint, TYPESAFE_ENDPOINT);
    assert_eq!(config.connect_timeout, Duration::from_secs(1));
    assert_eq!(config.timeout, Duration::from_secs(3));
    let key = ApiKey::new("secret-value".into()).unwrap();
    assert!(!format!("{key:?}").contains("secret-value"));
}
