//! Unit tests for context reranking against a local mock scoring server.
//! Every reranker here targets 127.0.0.1, so no test can reach TypeSafe.

use super::*;
use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderMap, Request, StatusCode, header},
    response::IntoResponse,
    routing::post,
};
use http_body_util::BodyExt;
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Notify,
};
use tower::ServiceExt;
use tracing::instrument::WithSubscriber;

/// The `Authorization` header and parsed JSON body of one received request.
type Received = (Option<String>, Value);
type Reply = (StatusCode, String, Option<Arc<Semaphore>>);

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
    replies: Arc<Mutex<VecDeque<Reply>>>,
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
            replies: Arc::default(),
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

    /// Queue a response whose independent gate can hold an older request open.
    fn reply(&self, status: StatusCode, body: String, gate: Option<Arc<Semaphore>>) {
        self.replies.lock().unwrap().push_back((status, body, gate));
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
    let (status, body, gate) = {
        mock.replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| (mock.status, mock.body.clone(), mock.response_gate.clone()))
    };
    if let Some(gate) = gate {
        gate.acquire_owned().await.unwrap().forget();
    }
    tokio::time::sleep(mock.delay).await;
    let mut reply = HeaderMap::new();
    if let Some(location) = &mock.location {
        reply.insert(header::LOCATION, location.parse().unwrap());
    }
    (status, reply, body)
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

/// A monotonic clock advanced explicitly; breaker tests never sleep for windows.
#[derive(Clone)]
struct ManualClock(Arc<Mutex<Instant>>);

impl ManualClock {
    fn advance(&self, duration: Duration) {
        let mut now = self.0.lock().unwrap();
        *now = now.checked_add(duration).unwrap();
    }
}

fn clocked_reranker(endpoint: String) -> (ContextReranker, ManualClock) {
    let mut reranker = limited_reranker(endpoint, 4);
    let clock = ManualClock(Arc::new(Mutex::new(Instant::now())));
    let now = clock.clone();
    reranker.breaker.now = Arc::new(move || *now.0.lock().unwrap());
    (reranker, clock)
}

/// Run a bounded test attempt over the standard candidates.
async fn run_attempt(reranker: &ContextReranker) -> Result<Vec<Value>, Unchanged> {
    let items = items();
    tokio::time::timeout(
        Duration::from_secs(1),
        reranker.attempt("q", &items, &candidate_positions(&items)),
    )
    .await
    .expect("test attempt must finish within one second")
}

fn successful_body() -> String {
    scores_body(&[score(0.0), score(3.0), score(1.0)])
}

fn queue_failures(mock: &Mock, count: usize) {
    for _ in 0..count {
        mock.reply(StatusCode::INTERNAL_SERVER_ERROR, String::new(), None);
    }
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
    assert_eq!(
        reranker
            .attempt("q", &items, &candidate_positions(&items))
            .await,
        Err(Unchanged::Failed("timeout"))
    );
    assert_eq!(
        reranker
            .attempt("q", &items, &candidate_positions(&items))
            .await,
        Err(Unchanged::Skipped("circuit_open"))
    );
    assert_eq!(mock.hits(), 3);
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

#[tokio::test]
async fn three_failed_status_body_or_score_outcomes_open_without_another_http_call() {
    let mock = Mock::new(StatusCode::OK, successful_body());
    mock.reply(StatusCode::INTERNAL_SERVER_ERROR, String::new(), None);
    mock.reply(StatusCode::OK, "x".repeat(MAX_RESPONSE_BYTES + 1), None);
    mock.reply(StatusCode::OK, "not json".into(), None);
    let (reranker, _) = clocked_reranker(mock.spawn().await);
    for reason in ["status", "oversized", "invalid_response"] {
        assert_eq!(run_attempt(&reranker).await, Err(Unchanged::Failed(reason)));
    }
    assert_eq!(
        run_attempt(&reranker).await,
        Err(Unchanged::Skipped("circuit_open"))
    );
    let original = items();
    assert_eq!(reranker.rerank("q", original.clone()).await, original);
    assert_eq!(mock.hits(), 3);
}

#[tokio::test]
async fn a_success_resets_the_consecutive_failure_count() {
    let mock = Mock::new(StatusCode::OK, successful_body());
    queue_failures(&mock, 2);
    mock.reply(StatusCode::OK, successful_body(), None);
    queue_failures(&mock, 3);
    let (reranker, _) = clocked_reranker(mock.spawn().await);
    for _ in 0..2 {
        assert_eq!(
            run_attempt(&reranker).await,
            Err(Unchanged::Failed("status"))
        );
    }
    assert!(run_attempt(&reranker).await.is_ok());
    for _ in 0..3 {
        assert_eq!(
            run_attempt(&reranker).await,
            Err(Unchanged::Failed("status"))
        );
    }
    assert_eq!(
        run_attempt(&reranker).await,
        Err(Unchanged::Skipped("circuit_open"))
    );
    assert_eq!(mock.hits(), 6);
}

#[tokio::test]
async fn configured_failure_threshold_and_open_window_control_admission() {
    let mock = Mock::new(StatusCode::OK, successful_body());
    queue_failures(&mock, 2);
    let config = ContextRerankConfig {
        api_key: ApiKey::new("test-key".into()),
        endpoint: mock.spawn().await,
        failure_threshold: 2,
        open_for: Duration::from_millis(250),
        ..ContextRerankConfig::default()
    };
    let mut reranker = ContextReranker::from_config(&config).unwrap().unwrap();
    let clock = ManualClock(Arc::new(Mutex::new(Instant::now())));
    let now = clock.clone();
    reranker.breaker.now = Arc::new(move || *now.0.lock().unwrap());
    for _ in 0..2 {
        assert_eq!(
            run_attempt(&reranker).await,
            Err(Unchanged::Failed("status"))
        );
    }
    clock.advance(Duration::from_millis(249));
    assert_eq!(
        run_attempt(&reranker).await,
        Err(Unchanged::Skipped("circuit_open"))
    );
    assert_eq!(mock.hits(), 2);
    clock.advance(Duration::from_millis(1));
    assert!(run_attempt(&reranker).await.is_ok());
    assert_eq!(mock.hits(), 3);
}

#[tokio::test]
async fn the_open_window_allows_exactly_one_shared_probe_and_success_closes() {
    let gate = Arc::new(Semaphore::new(0));
    let mock = Mock::new(StatusCode::OK, successful_body());
    queue_failures(&mock, 3);
    mock.reply(StatusCode::OK, successful_body(), Some(gate.clone()));
    let (reranker, clock) = clocked_reranker(mock.spawn().await);
    let reranker = Arc::new(reranker);
    for _ in 0..3 {
        assert_eq!(
            run_attempt(&reranker).await,
            Err(Unchanged::Failed("status"))
        );
    }
    clock.advance(Duration::from_secs(59));
    assert_eq!(
        run_attempt(&reranker).await,
        Err(Unchanged::Skipped("circuit_open"))
    );
    clock.advance(Duration::from_secs(1));
    let shared = reranker.clone();
    let probe = tokio::spawn(async move { run_attempt(&shared).await });
    mock.wait_for_hits(4).await;
    let mut rejected = Vec::new();
    for _ in 0..8 {
        let shared = reranker.clone();
        rejected.push(tokio::spawn(async move { run_attempt(&shared).await }));
    }
    for task in rejected {
        assert_eq!(task.await.unwrap(), Err(Unchanged::Skipped("circuit_open")));
    }
    assert_eq!(mock.hits(), 4);
    gate.add_permits(1);
    assert!(probe.await.unwrap().is_ok());
    // Closing resets the count: two failures still permit another request.
    queue_failures(&mock, 2);
    for _ in 0..2 {
        assert_eq!(
            run_attempt(&reranker).await,
            Err(Unchanged::Failed("status"))
        );
    }
    assert!(run_attempt(&reranker).await.is_ok());
    assert_eq!(mock.hits(), 7);
}

#[tokio::test]
async fn a_failed_half_open_probe_reopens_for_a_full_window() {
    let mock = Mock::new(StatusCode::INTERNAL_SERVER_ERROR, String::new());
    let (reranker, clock) = clocked_reranker(mock.spawn().await);
    for _ in 0..3 {
        assert_eq!(
            run_attempt(&reranker).await,
            Err(Unchanged::Failed("status"))
        );
    }
    clock.advance(Duration::from_secs(60));
    assert_eq!(
        run_attempt(&reranker).await,
        Err(Unchanged::Failed("status"))
    );
    clock.advance(Duration::from_secs(59));
    assert_eq!(
        run_attempt(&reranker).await,
        Err(Unchanged::Skipped("circuit_open"))
    );
    assert_eq!(mock.hits(), 4);
    clock.advance(Duration::from_secs(1));
    assert_eq!(
        run_attempt(&reranker).await,
        Err(Unchanged::Failed("status"))
    );
    assert_eq!(mock.hits(), 5);
}

#[tokio::test]
async fn an_old_success_cannot_close_a_newly_opened_circuit() {
    let gate = Arc::new(Semaphore::new(0));
    let mock = Mock::new(StatusCode::OK, successful_body());
    mock.reply(StatusCode::OK, successful_body(), Some(gate.clone()));
    queue_failures(&mock, 3);
    let (reranker, clock) = clocked_reranker(mock.spawn().await);
    let reranker = Arc::new(reranker);
    let shared = reranker.clone();
    let old = tokio::spawn(async move { run_attempt(&shared).await });
    mock.wait_for_hits(1).await;
    for _ in 0..3 {
        assert_eq!(
            run_attempt(&reranker).await,
            Err(Unchanged::Failed("status"))
        );
    }
    gate.add_permits(1);
    assert!(old.await.unwrap().is_ok());
    assert_eq!(
        run_attempt(&reranker).await,
        Err(Unchanged::Skipped("circuit_open"))
    );
    assert_eq!(mock.hits(), 4);
    clock.advance(Duration::from_secs(60));
    assert!(run_attempt(&reranker).await.is_ok());
    assert_eq!(mock.hits(), 5);
}

#[tokio::test]
async fn old_failures_cannot_reopen_a_circuit_after_a_successful_probe() {
    let gate = Arc::new(Semaphore::new(0));
    let mock = Mock::new(StatusCode::OK, successful_body());
    for _ in 0..3 {
        mock.reply(
            StatusCode::INTERNAL_SERVER_ERROR,
            String::new(),
            Some(gate.clone()),
        );
    }
    queue_failures(&mock, 3);
    let (reranker, clock) = clocked_reranker(mock.spawn().await);
    let reranker = Arc::new(reranker);
    let mut old = Vec::new();
    for _ in 0..3 {
        let shared = reranker.clone();
        old.push(tokio::spawn(async move { run_attempt(&shared).await }));
    }
    mock.wait_for_hits(3).await;
    for _ in 0..3 {
        assert_eq!(
            run_attempt(&reranker).await,
            Err(Unchanged::Failed("status"))
        );
    }
    clock.advance(Duration::from_secs(60));
    assert!(run_attempt(&reranker).await.is_ok());
    gate.add_permits(3);
    for task in old {
        assert_eq!(task.await.unwrap(), Err(Unchanged::Failed("status")));
    }
    assert!(run_attempt(&reranker).await.is_ok());
    assert_eq!(mock.hits(), 8);
}

#[tokio::test]
async fn cancelling_the_half_open_probe_releases_its_reservation_and_retries_later() {
    let gate = Arc::new(Semaphore::new(0));
    let mock = Mock::new(StatusCode::OK, successful_body());
    queue_failures(&mock, 3);
    mock.reply(StatusCode::OK, successful_body(), Some(gate.clone()));
    let (reranker, clock) = clocked_reranker(mock.spawn().await);
    let reranker = Arc::new(reranker);
    for _ in 0..3 {
        assert_eq!(
            run_attempt(&reranker).await,
            Err(Unchanged::Failed("status"))
        );
    }
    clock.advance(Duration::from_secs(60));
    let shared = reranker.clone();
    let probe = tokio::spawn(async move { run_attempt(&shared).await });
    mock.wait_for_hits(4).await;
    probe.abort();
    assert!(probe.await.unwrap_err().is_cancelled());
    assert_eq!(reranker.in_flight.available_permits(), 4);
    gate.add_permits(1);
    clock.advance(Duration::from_secs(59));
    assert_eq!(
        run_attempt(&reranker).await,
        Err(Unchanged::Skipped("circuit_open"))
    );
    clock.advance(Duration::from_secs(1));
    assert!(run_attempt(&reranker).await.is_ok());
    assert!(run_attempt(&reranker).await.is_ok());
    assert_eq!(mock.hits(), 6);
}

#[tokio::test]
async fn busy_and_candidate_count_skips_do_not_count_as_failures_or_wedge_a_probe() {
    let mock = Mock::new(StatusCode::OK, successful_body());
    queue_failures(&mock, 3);
    let (reranker, clock) = clocked_reranker(mock.spawn().await);
    assert_eq!(
        run_attempt(&reranker).await,
        Err(Unchanged::Failed("status"))
    );
    let mut permits = Vec::new();
    for _ in 0..4 {
        permits.push(reranker.in_flight.clone().try_acquire_owned().unwrap());
    }
    for _ in 0..3 {
        assert_eq!(
            run_attempt(&reranker).await,
            Err(Unchanged::Skipped("busy"))
        );
    }
    let one = vec![items().remove(0)];
    assert_eq!(
        reranker.attempt("q", &one, &[0]).await,
        Err(Unchanged::Skipped("too_few_candidates"))
    );
    let many: Vec<_> = (0..=MAX_CANDIDATES).map(|_| items().remove(0)).collect();
    assert_eq!(
        reranker
            .attempt("q", &many, &candidate_positions(&many))
            .await,
        Err(Unchanged::Skipped("too_many_candidates"))
    );
    drop(permits);
    for _ in 0..2 {
        assert_eq!(
            run_attempt(&reranker).await,
            Err(Unchanged::Failed("status"))
        );
    }
    clock.advance(Duration::from_secs(60));
    let permits: Vec<_> = (0..4)
        .map(|_| reranker.in_flight.clone().try_acquire_owned().unwrap())
        .collect();
    // An eligible probe that cannot acquire capacity must release its reservation.
    assert_eq!(
        run_attempt(&reranker).await,
        Err(Unchanged::Skipped("busy"))
    );
    assert_eq!(
        run_attempt(&reranker).await,
        Err(Unchanged::Skipped("busy"))
    );
    assert_eq!(mock.hits(), 3);
    drop(permits);
    assert_eq!(
        reranker.attempt("q", &one, &[0]).await,
        Err(Unchanged::Skipped("too_few_candidates"))
    );
    assert!(run_attempt(&reranker).await.is_ok());
    assert_eq!(mock.hits(), 4);
}

/// Capture only this future's tracing dispatch; no global subscriber is installed.
#[derive(Clone)]
struct LogWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Real authenticated context routers sharing one SQLite database. The private
/// clock is installed on AppState's reranker before that state enters its router.
struct EndpointFixture {
    plain: Router,
    reranked: Router,
    token: String,
    path: String,
    clock: ManualClock,
    _dir: tempfile::TempDir,
}

impl EndpointFixture {
    async fn new(endpoint: String) -> Self {
        use crate::{
            auth::{digest, secret},
            state::{AppState, Config},
        };
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            database_path: dir.path().join("endpoint.sqlite3"),
            public_origin: "http://127.0.0.1:8080".into(),
            allow_insecure_loopback: true,
            ..Config::default()
        };
        let plain_state = AppState::open(config.clone()).await.unwrap();
        let mut state = AppState::open(Config {
            context_rerank: ContextRerankConfig {
                api_key: ApiKey::new("endpoint-key-do-not-log".into()),
                endpoint,
                max_in_flight: 1,
                ..ContextRerankConfig::default()
            },
            ..config
        })
        .await
        .unwrap();
        let clock = ManualClock(Arc::new(Mutex::new(Instant::now())));
        let now = clock.clone();
        Arc::get_mut(state.context_reranker.as_mut().unwrap())
            .unwrap()
            .breaker
            .now = Arc::new(move || *now.0.lock().unwrap());
        let shared_state = state.clone();
        assert!(Arc::ptr_eq(
            state.context_reranker.as_ref().unwrap(),
            shared_state.context_reranker.as_ref().unwrap()
        ));
        let token = secret();
        let principal = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO principals(id,name,kind,role,password_hash,created_at) VALUES(?,?,'human','admin','unused',?)")
            .bind(&principal)
            .bind("endpoint-fixture")
            .bind(plain_state.now())
            .execute(&plain_state.pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO browser_sessions(id,principal_id,token_hash,expires_at) VALUES(?,?,?,?)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&principal)
        .bind(digest(&token))
        .bind(plain_state.now() + 86_400_000)
        .execute(&plain_state.pool)
        .await
        .unwrap();
        let plain = crate::router(plain_state);
        let reranked = crate::router(shared_state);
        let project = endpoint_call(
            &plain,
            &token,
            "POST",
            "/api/v1/projects",
            json!({"name":"endpoint-fixture","repository_url":"https://example.test/endpoint.git","target_branch":"main"}),
        )
        .await;
        let project = project["data"]["id"].as_str().unwrap();
        let mut first_task = None;
        for suffix in ["alpha", "beta", "gamma"] {
            let task = endpoint_call(
                &plain,
                &token,
                "POST",
                &format!("/api/v1/projects/{project}/tasks"),
                json!({"title":format!("privacyneedle {suffix}"),"description":"private endpoint task text","acceptance_criteria":["ordered"],"kind":"general"}),
            )
            .await;
            first_task.get_or_insert(task["data"]["id"].as_str().unwrap().to_owned());
        }
        endpoint_call(
            &plain,
            &token,
            "POST",
            &format!("/api/v1/projects/{project}/decisions"),
            json!({"question":"private endpoint decision","options":["Proceed","Wait"],"rationale":"Keep this decision in its original position.",
                "required_actor":"human","affected_tasks":[{"task_id":first_task.unwrap(),"task_revision":1}],
                "policy_revision":1,"environment":"test","conditions":"None.","expires_at":null}),
        )
        .await;
        Self {
            plain,
            reranked,
            token,
            path: format!(
                "/api/v1/projects/{project}/context?q=privacyneedle&limit=20&budget=65536"
            ),
            clock,
            _dir: dir,
        }
    }

    async fn packet(&self, app: &Router) -> Value {
        endpoint_call(app, &self.token, "GET", &self.path, json!({})).await["data"].clone()
    }
}

async fn endpoint_call(app: &Router, token: &str, method: &str, path: &str, body: Value) -> Value {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("idempotency-key", uuid::Uuid::new_v4().to_string())
        .header("cookie", format!("coordinator_local={token}"))
        .header("origin", "http://127.0.0.1:8080")
        .header(
            "x-csrf-token",
            crate::auth::digest(&format!("coordinator-browser-csrf-v1:{token}")),
        )
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn endpoint_recovery_reorders_the_same_packet_and_logs_private_safe_skip_reasons() {
    let gate = Arc::new(Semaphore::new(0));
    let reverse = scores_body(&[score(0.0), score(1.0), score(2.0)]);
    let mock = Mock::new(StatusCode::OK, reverse.clone());
    mock.reply(StatusCode::OK, reverse, Some(gate.clone()));
    let fixture = Arc::new(EndpointFixture::new(mock.spawn().await).await);
    let original = fixture.packet(&fixture.plain).await;
    assert_eq!(original["items"].as_array().unwrap().len(), 4);
    assert_eq!(original["items"][0]["type"], "decision");
    let mut expected = original.clone();
    expected["items"].as_array_mut().unwrap()[1..].reverse();
    assert_ne!(expected["items"], original["items"]);
    let logs = Arc::new(Mutex::new(Vec::new()));
    let writer = LogWriter(logs.clone());
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_target(false)
        .with_max_level(tracing::Level::INFO)
        .with_writer(move || writer.clone())
        .finish();
    let dispatch = tracing::Dispatch::new(subscriber);
    let shared = fixture.clone();
    let pending = tokio::spawn(
        async move { shared.packet(&shared.reranked).await }.with_subscriber(dispatch.clone()),
    );
    mock.wait_for_hits(1).await;
    assert_eq!(
        fixture
            .packet(&fixture.reranked)
            .with_subscriber(dispatch.clone())
            .await,
        original
    );
    assert_eq!(mock.hits(), 1);
    gate.add_permits(1);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), pending)
            .await
            .unwrap()
            .unwrap(),
        expected
    );
    queue_failures(&mock, 3);
    for expected_hits in 2..=4 {
        assert_eq!(
            fixture
                .packet(&fixture.reranked)
                .with_subscriber(dispatch.clone())
                .await,
            original
        );
        assert_eq!(mock.hits(), expected_hits);
    }
    assert_eq!(
        fixture
            .packet(&fixture.reranked)
            .with_subscriber(dispatch.clone())
            .await,
        original
    );
    fixture.clock.advance(Duration::from_secs(59));
    assert_eq!(
        fixture
            .packet(&fixture.reranked)
            .with_subscriber(dispatch.clone())
            .await,
        original
    );
    assert_eq!(mock.hits(), 4);
    fixture.clock.advance(Duration::from_secs(1));
    for expected_hits in 5..=6 {
        assert_eq!(
            fixture
                .packet(&fixture.reranked)
                .with_subscriber(dispatch.clone())
                .await,
            expected
        );
        assert_eq!(mock.hits(), expected_hits);
    }
    let logs = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("reason=\"busy\""), "missing busy skip log");
    assert!(
        logs.contains("reason=\"circuit_open\""),
        "missing circuit skip log"
    );
    assert!(
        logs.contains("outcome=\"reordered\""),
        "missing successful reorder log"
    );
    for private in [
        "endpoint-key-do-not-log",
        "privacyneedle",
        "private endpoint task text",
        "private endpoint decision",
        fixture.token.as_str(),
    ] {
        assert!(
            !logs.contains(private),
            "private context value appeared in logs"
        );
    }
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
                failure_threshold: 0,
                open_for: Duration::ZERO,
                ..ContextRerankConfig::default()
            };
            assert!(ContextReranker::from_config(&config).unwrap().is_none());
        }
    }
}

#[test]
fn keyed_breaker_configurations_require_a_positive_threshold_and_window() {
    for (failure_threshold, open_for, field) in [
        (0, Duration::from_secs(60), "failure_threshold"),
        (3, Duration::ZERO, "open_for"),
    ] {
        let config = ContextRerankConfig {
            api_key: ApiKey::new("test-key".into()),
            failure_threshold,
            open_for,
            ..ContextRerankConfig::default()
        };
        let error = ContextReranker::from_config(&config)
            .err()
            .expect("zero breaker limits must return an error");
        assert!(error.to_string().contains(field));
    }
    let large = ContextRerankConfig {
        api_key: ApiKey::new("test-key".into()),
        failure_threshold: usize::MAX,
        open_for: Duration::MAX,
        ..ContextRerankConfig::default()
    };
    assert!(ContextReranker::from_config(&large).unwrap().is_some());
}

#[test]
fn keyless_configurations_ignore_zero_breaker_limits() {
    for api_key in [None, ApiKey::new(" \n ".into())] {
        let config = ContextRerankConfig {
            api_key,
            failure_threshold: 0,
            open_for: Duration::ZERO,
            ..ContextRerankConfig::default()
        };
        assert!(ContextReranker::from_config(&config).unwrap().is_none());
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
    assert_eq!(config.failure_threshold, 3);
    assert_eq!(config.open_for, Duration::from_secs(60));
    let key = ApiKey::new("secret-value".into()).unwrap();
    assert!(!format!("{key:?}").contains("secret-value"));
}
