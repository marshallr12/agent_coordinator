//! Optional relevance ordering for already bounded context items.
//! Reranking runs only when `ContextRerankMode::Typesafe` is configured with a
//! nonempty API key. Any skip or failure keeps the original SQLite ordering.

use reqwest::redirect::Policy;
use serde_json::{Map, Value, json};
use std::{
    fmt,
    time::{Duration, Instant},
};

/// Production TypeSafe scoring endpoint used unless a test overrides it.
pub const TYPESAFE_ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
const TYPESAFE_MODEL: &str = "jev-latest";
const MIN_CANDIDATES: usize = 2;
const MAX_CANDIDATES: usize = 40;
const MAX_EXCERPT_CHARS: usize = 1_800;
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const MAX_SCORE: f64 = 3.0;

/// Which reranker, if any, reorders context search results.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum ContextRerankMode {
    /// Keep the SQLite FTS order; no external request is made.
    #[default]
    Off,
    /// Ask TypeSafe to score the selected task and knowledge items.
    Typesafe,
}

/// A nonempty TypeSafe API key whose `Debug` output never shows the value.
#[derive(Clone)]
pub struct ApiKey(String);

impl ApiKey {
    /// Wrap `value`, or return `None` when it is empty or only whitespace.
    pub fn new(value: String) -> Option<Self> {
        (!value.trim().is_empty()).then_some(Self(value))
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
    /// Selected reranker; `Off` unless explicitly enabled.
    pub mode: ContextRerankMode,
    /// TypeSafe API key; `Typesafe` mode without a key behaves as `Off`.
    pub api_key: Option<ApiKey>,
    /// Scoring endpoint URL; tests point it at a local mock server.
    pub endpoint: String,
    /// Maximum time to establish the connection.
    pub connect_timeout: Duration,
    /// Maximum time for the whole request, including the response body.
    pub timeout: Duration,
}

impl Default for ContextRerankConfig {
    /// Off, no key, the production endpoint, 1 s connect and 3 s total timeouts.
    fn default() -> Self {
        Self {
            mode: ContextRerankMode::Off,
            api_key: None,
            endpoint: TYPESAFE_ENDPOINT.into(),
            connect_timeout: Duration::from_secs(1),
            timeout: Duration::from_secs(3),
        }
    }
}

/// Why an attempt kept the original order; logged as `outcome` and `reason`.
#[derive(Debug, PartialEq, Eq)]
enum Unchanged {
    /// No request is sent: the candidate count is outside
    /// `MIN_CANDIDATES..=MAX_CANDIDATES`.
    Skipped(&'static str),
    /// The request or its response failed; the value names the failure kind.
    Failed(&'static str),
}

/// A TypeSafe client built once at startup and shared by all requests.
pub(crate) struct ContextReranker {
    client: reqwest::Client,
    endpoint: String,
    key: ApiKey,
}

impl ContextReranker {
    /// Build the reranker for `config`, or `None` when reranking is off.
    /// Enabled without a key logs one warning and behaves as off.
    pub(crate) fn from_config(config: &ContextRerankConfig) -> anyhow::Result<Option<Self>> {
        if config.mode == ContextRerankMode::Off {
            return Ok(None);
        }
        let Some(key) = config.api_key.clone() else {
            tracing::warn!(
                "Context reranking is set to typesafe but TYPESAFE_API_KEY is empty or unset; reranking is off."
            );
            return Ok(None);
        };
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
        let body = request_body(query, items, positions);
        let bytes = self.post(&body).await?;
        let scores = parse_scores(&bytes, positions.len())?;
        Ok(reorder(items, positions, &scores))
    }

    /// POST `body` and return the body of a 2xx response within the size cap.
    async fn post(&self, body: &Value) -> Result<Vec<u8>, Unchanged> {
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
