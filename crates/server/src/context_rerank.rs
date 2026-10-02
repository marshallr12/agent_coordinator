//! Optional relevance ordering for already bounded context items.
//! Reranking runs only when the configuration carries a nonempty TypeSafe API
//! key. Any skip or failure keeps the original SQLite ordering.

use reqwest::redirect::Policy;
use serde_json::{Map, Value, json};
use std::{
    fmt,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;

/// Production TypeSafe scoring endpoint used unless a test overrides it.
pub const TYPESAFE_ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
const TYPESAFE_MODEL: &str = "jev-latest";
const MIN_CANDIDATES: usize = 2;
const MAX_CANDIDATES: usize = 40;
const MAX_EXCERPT_CHARS: usize = 1_800;
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const MAX_SCORE: f64 = 3.0;

/// A nonempty TypeSafe API key whose `Debug` output never shows the value.
#[derive(Clone)]
pub struct ApiKey(String);

impl ApiKey {
    /// Wrap `value` without surrounding whitespace (a key read from a file
    /// often ends in a newline, which is not a valid header value), or
    /// return `None` when nothing is left.
    pub fn new(value: String) -> Option<Self> {
        let key = value.trim();
        (!key.is_empty()).then(|| Self(key.to_owned()))
    }
}

impl fmt::Debug for ApiKey {
    /// Print a fixed placeholder instead of the secret value.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ApiKey(<redacted>)")
    }
}

/// Context reranking settings, read once at startup.
#[derive(Clone, Debug)]
pub struct ContextRerankConfig {
    /// TypeSafe API key; reranking is off without one.
    pub api_key: Option<ApiKey>,
    /// Scoring endpoint URL; tests point it at a local mock server.
    pub endpoint: String,
    /// Maximum time to establish the connection.
    pub connect_timeout: Duration,
    /// Maximum time for the whole request, including the response body.
    pub timeout: Duration,
    /// Maximum concurrent scoring requests; excess attempts keep their order.
    /// With an API key, must be in `1..=Semaphore::MAX_PERMITS`.
    pub max_in_flight: usize,
    /// Consecutive completed failures that open the circuit; must be nonzero.
    pub failure_threshold: usize,
    /// Time to wait before allowing one recovery probe; must be nonzero.
    pub open_for: Duration,
}

impl Default for ContextRerankConfig {
    /// No key (so reranking is off), the production endpoint, 1 s connect
    /// and 3 s total timeouts, at most four concurrent requests, and a circuit
    /// that opens for 60 s after three consecutive failures.
    fn default() -> Self {
        Self {
            api_key: None,
            endpoint: TYPESAFE_ENDPOINT.into(),
            connect_timeout: Duration::from_secs(1),
            timeout: Duration::from_secs(3),
            max_in_flight: 4,
            failure_threshold: 3,
            open_for: Duration::from_secs(60),
        }
    }
}

/// Why an attempt kept the original order; logged as `outcome` and `reason`.
#[derive(Debug, PartialEq, Eq)]
enum Unchanged {
    /// No request is sent: the candidate count is outside
    /// `MIN_CANDIDATES..=MAX_CANDIDATES`, the client is busy, or its circuit is open.
    Skipped(&'static str),
    /// The request or its response failed; the value names the failure kind.
    Failed(&'static str),
}

/// A TypeSafe client built once at startup and shared by all requests.
pub(crate) struct ContextReranker {
    client: reqwest::Client,
    endpoint: String,
    key: ApiKey,
    in_flight: Arc<Semaphore>,
    breaker: CircuitBreaker,
}

impl ContextReranker {
    /// Build the reranker for `config`, or `None` when it has no API key.
    /// A keyed configuration must have valid, nonzero concurrency and breaker limits.
    pub(crate) fn from_config(config: &ContextRerankConfig) -> anyhow::Result<Option<Self>> {
        let Some(key) = config.api_key.clone() else {
            return Ok(None);
        };
        anyhow::ensure!(
            (1..=Semaphore::MAX_PERMITS).contains(&config.max_in_flight),
            "context rerank max_in_flight must be between 1 and {}",
            Semaphore::MAX_PERMITS
        );
        anyhow::ensure!(
            config.failure_threshold > 0,
            "context rerank failure_threshold must be nonzero"
        );
        anyhow::ensure!(
            !config.open_for.is_zero(),
            "context rerank open_for must be nonzero"
        );
        let client = reqwest::Client::builder()
            .connect_timeout(config.connect_timeout)
            .timeout(config.timeout)
            .redirect(Policy::none())
            .build()?;
        let endpoint = config.endpoint.clone();
        Ok(Some(Self {
            client,
            endpoint,
            key,
            in_flight: Arc::new(Semaphore::new(config.max_in_flight)),
            breaker: CircuitBreaker::new(config.failure_threshold, config.open_for),
        }))
    }

    /// Return `items` reordered by relevance to `query`, or unchanged on any
    /// skip or failure. Logs one line with the outcome, count and elapsed time.
    pub(crate) async fn rerank(&self, query: &str, items: Vec<Value>) -> Vec<Value> {
        let started = Instant::now();
        let positions = candidate_positions(&items);
        let result = self.attempt(query, &items, &positions).await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let candidates = positions.len();
        match result {
            Ok(reordered) => {
                tracing::info!(
                    outcome = "reordered",
                    candidates,
                    elapsed_ms,
                    "context rerank"
                );
                reordered
            }
            Err(Unchanged::Skipped(reason)) => {
                tracing::info!(
                    outcome = "skipped",
                    reason,
                    candidates,
                    elapsed_ms,
                    "context rerank"
                );
                items
            }
            Err(Unchanged::Failed(reason)) => {
                tracing::warn!(
                    outcome = "failed",
                    reason,
                    candidates,
                    elapsed_ms,
                    "context rerank"
                );
                items
            }
        }
    }

    /// Score the candidates at `positions` and reorder them, or explain why not.
    async fn attempt(
        &self,
        query: &str,
        items: &[Value],
        positions: &[usize],
    ) -> Result<Vec<Value>, Unchanged> {
        if positions.len() < MIN_CANDIDATES {
            return Err(Unchanged::Skipped("too_few_candidates"));
        }
        if positions.len() > MAX_CANDIDATES {
            return Err(Unchanged::Skipped("too_many_candidates"));
        }
        let attempt = self.breaker.begin()?;
        let result = async {
            let body = request_body(query, items, positions);
            let bytes = self.post(&body).await?;
            let scores = parse_scores(&bytes, positions.len())?;
            Ok(reorder(items, positions, &scores))
        }
        .await;
        attempt.finish(&result);
        result
    }

    /// POST `body` and return the body of a 2xx response within the size cap.
    async fn post(&self, body: &Value) -> Result<Vec<u8>, Unchanged> {
        let _permit = self
            .in_flight
            .clone()
            .try_acquire_owned()
            .map_err(|_| Unchanged::Skipped("busy"))?;
        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.key.0)
            .json(body)
            .send()
            .await
            .map_err(transport_failure)?;
        if !response.status().is_success() {
            return Err(Unchanged::Failed("status"));
        }
        read_capped(response).await
    }
}

/// Shared breaker state. Locks cover admission and completion only, never HTTP.
struct CircuitBreaker {
    failure_threshold: usize,
    open_for: Duration,
    now: Arc<dyn Fn() -> Instant + Send + Sync>,
    state: Mutex<BreakerState>,
}

struct BreakerState {
    // Fresh Arc identities cannot wrap or match old, still-live generations.
    generation: Arc<()>,
    mode: BreakerMode,
}

#[derive(Clone, Copy)]
enum BreakerMode {
    Closed { failures: usize },
    Open { opened_at: Instant },
    HalfOpen { opened_at: Instant },
}

impl BreakerState {
    /// Invalidate completions admitted before this state transition.
    fn transition(&mut self, mode: BreakerMode) {
        self.generation = Arc::new(());
        self.mode = mode;
    }
}

impl CircuitBreaker {
    fn new(failure_threshold: usize, open_for: Duration) -> Self {
        Self {
            failure_threshold,
            open_for,
            now: Arc::new(Instant::now),
            state: Mutex::new(BreakerState {
                generation: Arc::new(()),
                mode: BreakerMode::Closed { failures: 0 },
            }),
        }
    }

    /// Admit a closed request or reserve the only probe after the open window.
    fn begin(&self) -> Result<BreakerAttempt<'_>, Unchanged> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let probe = match state.mode {
            BreakerMode::Closed { .. } => false,
            BreakerMode::Open { opened_at }
                if (self.now)().saturating_duration_since(opened_at) >= self.open_for =>
            {
                state.transition(BreakerMode::HalfOpen { opened_at });
                true
            }
            _ => return Err(Unchanged::Skipped("circuit_open")),
        };
        Ok(BreakerAttempt {
            breaker: self,
            generation: state.generation.clone(),
            probe,
            completed: false,
        })
    }
}

/// A half-open reservation reopens conservatively if its future is cancelled.
struct BreakerAttempt<'a> {
    breaker: &'a CircuitBreaker,
    generation: Arc<()>,
    probe: bool,
    completed: bool,
}

impl BreakerAttempt<'_> {
    fn finish(mut self, result: &Result<Vec<Value>, Unchanged>) {
        self.completed = true;
        let mut state = self
            .breaker
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !Arc::ptr_eq(&state.generation, &self.generation) {
            return;
        }
        match (state.mode, result) {
            (BreakerMode::Closed { .. }, Ok(_)) => {
                state.mode = BreakerMode::Closed { failures: 0 };
            }
            (BreakerMode::Closed { failures }, Err(Unchanged::Failed(_))) => {
                let failures = failures + 1;
                if failures >= self.breaker.failure_threshold {
                    state.transition(BreakerMode::Open {
                        opened_at: (self.breaker.now)(),
                    });
                } else {
                    state.mode = BreakerMode::Closed { failures };
                }
            }
            (BreakerMode::HalfOpen { .. }, Ok(_)) => {
                state.transition(BreakerMode::Closed { failures: 0 });
            }
            (BreakerMode::HalfOpen { .. }, Err(Unchanged::Failed(_))) => {
                state.transition(BreakerMode::Open {
                    opened_at: (self.breaker.now)(),
                });
            }
            (BreakerMode::HalfOpen { opened_at }, Err(Unchanged::Skipped(_))) => {
                // A busy skip sent no request and may retry as soon as capacity frees.
                state.transition(BreakerMode::Open { opened_at });
            }
            _ => {}
        }
    }
}

impl Drop for BreakerAttempt<'_> {
    fn drop(&mut self) {
        if self.completed || !self.probe {
            return;
        }
        let mut state = self
            .breaker
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if Arc::ptr_eq(&state.generation, &self.generation)
            && matches!(state.mode, BreakerMode::HalfOpen { .. })
        {
            state.transition(BreakerMode::Open {
                opened_at: (self.breaker.now)(),
            });
        }
    }
}

/// Classify a transport error by kind only; its message is never logged.
fn transport_failure(error: reqwest::Error) -> Unchanged {
    if error.is_timeout() {
        Unchanged::Failed("timeout")
    } else if error.is_connect() {
        Unchanged::Failed("connect")
    } else {
        Unchanged::Failed("transport")
    }
}

/// Read the response body, failing once it exceeds `MAX_RESPONSE_BYTES`.
async fn read_capped(mut response: reqwest::Response) -> Result<Vec<u8>, Unchanged> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(Unchanged::Failed("oversized"));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport_failure)? {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(Unchanged::Failed("oversized"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Indexes of the task and knowledge items; every other item keeps its place.
fn candidate_positions(items: &[Value]) -> Vec<usize> {
    items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            matches!(item["type"].as_str(), Some("task" | "knowledge")).then_some(index)
        })
        .collect()
}

/// The first `MAX_EXCERPT_CHARS` characters of a candidate's title, its
/// description or body, and its acceptance criteria or applicability.
fn candidate_text(item: &Value) -> String {
    let record = &item["record"];
    let title = record["title"].as_str().unwrap_or_default();
    let is_task = item["type"] == "task";
    let body_field = if is_task { "description" } else { "body" };
    let body = record[body_field].as_str().unwrap_or_default();
    let extra = if is_task {
        record["acceptance_criteria"].to_string()
    } else {
        record["applicability"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    };
    let text = format!("{title}\n{body}\n{extra}");
    text.chars().take(MAX_EXCERPT_CHARS).collect()
}

/// The TypeSafe request: the query, one excerpt per candidate, and one
/// `relevance_{index}` score question per candidate.
fn request_body(query: &str, items: &[Value], positions: &[usize]) -> Value {
    let candidates: Vec<_> = positions
        .iter()
        .map(|&index| json!({"kind": items[index]["type"], "text": candidate_text(&items[index])}))
        .collect();
    let questions: Map<String, Value> = (0..positions.len())
        .map(|index| (format!("relevance_{index}"), score_question(index)))
        .collect();
    json!({
        "state": {"query": query, "candidates": candidates},
        "model": TYPESAFE_MODEL,
        "questions": questions,
    })
}

/// A four-level `score` question about `candidates[index]`.
fn score_question(index: usize) -> Value {
    json!({
        "type": "score",
        "instructions": format!("How useful is `candidates[{index}]` for answering `query`? Judge relevance to the specific question, not just shared words."),
        "criteria": [
            "Unrelated to the question.",
            "Shares a topic but offers no useful answer.",
            "Provides relevant background or part of the answer.",
            "Directly answers the question with specific useful information."
        ]
    })
}

/// Parse one score per candidate from a TypeSafe response. Any missing,
/// non-score, non-finite or out-of-range answer rejects the whole response.
fn parse_scores(bytes: &[u8], count: usize) -> Result<Vec<f64>, Unchanged> {
    let invalid = || Unchanged::Failed("invalid_response");
    let result: Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    let answers = result["answers"].as_object().ok_or_else(invalid)?;
    (0..count)
        .map(|index| answer_score(answers.get(&format!("relevance_{index}"))).ok_or_else(invalid))
        .collect()
}

/// The score of one answer if it is a `score` answer within 0..=`MAX_SCORE`.
fn answer_score(answer: Option<&Value>) -> Option<f64> {
    let answer = answer?;
    if answer["type"] != "score" {
        return None;
    }
    let score = answer["score"].as_f64()?;
    (score.is_finite() && (0.0..=MAX_SCORE).contains(&score)).then_some(score)
}

/// Place the candidates at `positions` in descending score order, ties by
/// original order, leaving every other item at its own index.
fn reorder(items: &[Value], positions: &[usize], scores: &[f64]) -> Vec<Value> {
    let mut order: Vec<_> = (0..positions.len()).collect();
    order.sort_by(|&left, &right| {
        scores[right]
            .total_cmp(&scores[left])
            .then(left.cmp(&right))
    });
    let mut reordered = items.to_vec();
    for (&destination, source) in positions.iter().zip(order) {
        reordered[destination] = items[positions[source]].clone();
    }
    reordered
}

#[cfg(test)]
mod tests;
