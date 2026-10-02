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
    Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Notify,
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
    arrived: Arc<Notify>,
    response_gate: Option<Arc<Semaphore>>,
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
            arrived: Arc::default(),
            response_gate: None,
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

    /// Wait for a specific number of requests, failing with a bounded deadline.
    async fn wait_for_hits(&self, expected: usize) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while self.hits() < expected {
                self.arrived.notified().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("expected {expected} requests, received {}", self.hits()));
    }
}

/// Record the request, wait the configured delay, then send the canned reply.
async fn respond(State(mock): State<Mock>, headers: HeaderMap, body: String) -> impl IntoResponse {
    mock.hits.fetch_add(1, Ordering::SeqCst);
    mock.arrived.notify_one();
    let auth = headers
        .get(header::AUTHORIZATION)
        .map(|value| value.to_str().unwrap().to_owned());
    let parsed = serde_json::from_str(&body).unwrap_or(Value::Null);
    mock.requests.lock().unwrap().push((auth, parsed));
    if let Some(gate) = &mock.response_gate {
        gate.clone().acquire_owned().await.unwrap().forget();
    }
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

/// A reranker with a chosen cap and enough time for controlled concurrent calls.
fn limited_reranker(endpoint: String, max_in_flight: usize) -> ContextReranker {
    let config = ContextRerankConfig {
        api_key: ApiKey::new("test-key".into()),
        endpoint,
        max_in_flight,
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
    assert_eq!(reranker.in_flight.available_permits(), 4);
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

#[tokio::test]
async fn shared_reranker_skips_at_the_default_cap_and_resumes_after_completion() {
    let gate = Arc::new(Semaphore::new(0));
    let mut mock = Mock::new(
        StatusCode::OK,
        scores_body(&[score(0.0), score(3.0), score(1.0)]),
    );
    mock.response_gate = Some(gate.clone());
    let config = ContextRerankConfig {
        api_key: ApiKey::new("test-key".into()),
        endpoint: mock.spawn().await,
        ..ContextRerankConfig::default()
    };
    // AppState clones share this same Arc, so their requests share one cap.
    let reranker = Arc::new(ContextReranker::from_config(&config).unwrap().unwrap());
    let mut pending = Vec::new();
    for _ in 0..4 {
        let shared = reranker.clone();
        pending.push(tokio::spawn(
            async move { shared.rerank("q", items()).await },
        ));
    }
    mock.wait_for_hits(4).await;
    let original = items();
    assert_eq!(reranker.rerank("q", original.clone()).await, original);
    assert_eq!(
        reranker
            .attempt("q", &original, &candidate_positions(&original))
            .await,
        Err(Unchanged::Skipped("busy"))
    );
    assert_eq!(mock.hits(), 4);
    gate.add_permits(4);
    for task in pending {
        let reordered = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(titles(&reordered), ["B", "D", "C", "A"]);
    }
    gate.add_permits(1);
    let reordered = reranker.rerank("q", items()).await;
    assert_eq!(titles(&reordered), ["B", "D", "C", "A"]);
    assert_eq!(mock.hits(), 5);
}

#[tokio::test]
async fn failed_requests_release_capacity_for_the_next_attempt() {
    for (status, body, reason) in [
        (StatusCode::INTERNAL_SERVER_ERROR, String::new(), "status"),
        (StatusCode::OK, "not json".to_owned(), "invalid_response"),
        (
            StatusCode::OK,
            "x".repeat(MAX_RESPONSE_BYTES + 1),
            "oversized",
        ),
    ] {
        let mock = Mock::new(status, body);
        let reranker = limited_reranker(mock.spawn().await, 1);
        let items = items();
        for _ in 0..2 {
            assert_eq!(
                reranker
                    .attempt("q", &items, &candidate_positions(&items))
                    .await,
                Err(Unchanged::Failed(reason))
            );
        }
        assert_eq!(mock.hits(), 2);
    }
}

#[tokio::test]
async fn cancelling_a_request_releases_capacity_for_a_shared_clone() {
    let gate = Arc::new(Semaphore::new(0));
    let mut mock = Mock::new(
        StatusCode::OK,
        scores_body(&[score(0.0), score(3.0), score(1.0)]),
    );
    mock.response_gate = Some(gate.clone());
    let reranker = Arc::new(limited_reranker(mock.spawn().await, 1));
    let shared = reranker.clone();
    let pending = tokio::spawn(async move { shared.rerank("q", items()).await });
    mock.wait_for_hits(1).await;
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    // Allow both the cancelled server handler and the next request to finish.
    gate.add_permits(2);
    let reordered = reranker.rerank("q", items()).await;
    assert_eq!(titles(&reordered), ["B", "D", "C", "A"]);
    assert_eq!(mock.hits(), 2);
}

#[tokio::test]
async fn capacity_is_held_until_the_response_body_finishes() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/systemone", listener.local_addr().unwrap());
    let headers_sent = Arc::new(Notify::new());
    let finish_body = Arc::new(Notify::new());
    let server_headers_sent = headers_sent.clone();
    let server_finish_body = finish_body.clone();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let count = socket.read(&mut buffer).await.unwrap();
            assert_ne!(count, 0, "client closed before sending request headers");
            request.extend_from_slice(&buffer[..count]);
        }
        let headers_end = request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap()
            + 4;
        let content_length: usize = std::str::from_utf8(&request[..headers_end])
            .unwrap()
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .unwrap()
            .1
            .trim()
            .parse()
            .unwrap();
        while request.len() < headers_end + content_length {
            let count = socket.read(&mut buffer).await.unwrap();
            assert_ne!(count, 0, "client closed before sending the request body");
            request.extend_from_slice(&buffer[..count]);
        }
        let body = scores_body(&[score(0.0), score(3.0), score(1.0)]);
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        socket.write_all(headers.as_bytes()).await.unwrap();
        // Send only part of the body; the client must keep its permit while reading.
        socket.write_all(&body.as_bytes()[..1]).await.unwrap();
        server_headers_sent.notify_one();
        server_finish_body.notified().await;
        socket.write_all(&body.as_bytes()[1..]).await.unwrap();
    });
    let reranker = Arc::new(limited_reranker(endpoint, 1));
    let shared = reranker.clone();
    let pending = tokio::spawn(async move { shared.rerank("q", items()).await });
    tokio::time::timeout(Duration::from_secs(1), headers_sent.notified())
        .await
        .unwrap();
    let items = items();
    // Keep probing while the body is withheld so this also fails if admission
    // is released just after headers arrive. Each busy skip must be immediate.
    let deadline = Instant::now() + Duration::from_millis(50);
    while Instant::now() < deadline {
        let attempt = tokio::time::timeout(
            Duration::from_millis(10),
            reranker.attempt("q", &items, &candidate_positions(&items)),
        )
        .await
        .expect("a saturated attempt must not wait for a network response");
        assert_eq!(attempt, Err(Unchanged::Skipped("busy")));
        tokio::task::yield_now().await;
    }
    finish_body.notify_one();
    let reordered = tokio::time::timeout(Duration::from_secs(1), pending)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(titles(&reordered), ["B", "D", "C", "A"]);
    tokio::time::timeout(Duration::from_secs(1), server)
        .await
        .unwrap()
        .unwrap();
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
fn keyed_configurations_reject_zero_or_excessive_capacity() {
    for max_in_flight in [0, Semaphore::MAX_PERMITS + 1, usize::MAX] {
        let config = ContextRerankConfig {
            api_key: ApiKey::new("test-key".into()),
            max_in_flight,
            ..ContextRerankConfig::default()
        };
        let error = ContextReranker::from_config(&config)
            .err()
            .expect("invalid keyed capacity must return an error instead of panicking");
        assert!(error.to_string().contains("max_in_flight"));
    }
}

#[test]
fn keyed_configurations_accept_capacity_bounds_and_the_default() {
    for max_in_flight in [
        1,
        ContextRerankConfig::default().max_in_flight,
        Semaphore::MAX_PERMITS,
    ] {
        let config = ContextRerankConfig {
            api_key: ApiKey::new("test-key".into()),
            max_in_flight,
            ..ContextRerankConfig::default()
        };
        let reranker = ContextReranker::from_config(&config).unwrap().unwrap();
        assert_eq!(reranker.in_flight.available_permits(), max_in_flight);
    }
}

#[test]
fn keyless_configurations_ignore_invalid_capacity() {
    for max_in_flight in [0, Semaphore::MAX_PERMITS + 1, usize::MAX] {
        for api_key in [None, ApiKey::new(" \n ".into())] {
            let config = ContextRerankConfig {
                api_key,
                max_in_flight,
                ..ContextRerankConfig::default()
            };
            assert!(ContextReranker::from_config(&config).unwrap().is_none());
        }
    }
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
    assert_eq!(config.max_in_flight, 4);
    let key = ApiKey::new("secret-value".into()).unwrap();
    assert!(!format!("{key:?}").contains("secret-value"));
}
