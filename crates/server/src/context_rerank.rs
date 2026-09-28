//! Optional relevance ordering for already bounded context items.
//! Any failure returns None so the original SQLite ordering is preserved.

use reqwest::redirect::Policy;
use serde_json::{Map, Value, json};
use std::time::Duration;

const MAX_CANDIDATES: usize = 40;
const MAX_RESPONSE_BYTES: usize = 256 * 1024;

fn excerpt(value: &str) -> String {
    value.chars().take(1_800).collect()
}

fn candidate_text(item: &Value) -> String {
    let record = &item["record"];
    let title = record["title"].as_str().unwrap_or_default();
    let body = if item["type"] == "task" {
        record["description"].as_str().unwrap_or_default()
    } else {
        record["body"].as_str().unwrap_or_default()
    };
    let extra = if item["type"] == "task" {
        record["acceptance_criteria"].to_string()
    } else {
        record["applicability"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    };
    excerpt(&format!("{title}\n{body}\n{extra}"))
}

pub(crate) async fn try_rerank(query: &str, items: &[Value]) -> Option<Vec<Value>> {
    let key = std::env::var("TYPESAFE_API_KEY").ok()?;
    if key.trim().is_empty() {
        return None;
    }
    let positions: Vec<_> = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            matches!(item["type"].as_str(), Some("task" | "knowledge")).then_some(index)
        })
        .collect();
    if !(2..=MAX_CANDIDATES).contains(&positions.len()) {
        return None;
    }

    let candidates: Vec<_> = positions
        .iter()
        .map(|&index| {
            json!({
                "kind": items[index]["type"],
                "text": candidate_text(&items[index]),
            })
        })
        .collect();
    let mut questions = Map::new();
    for index in 0..candidates.len() {
        questions.insert(
            format!("relevance_{index}"),
            json!({
                "type": "score",
                "instructions": format!("How useful is `candidates[{index}]` for answering `query`? Judge relevance to the specific question, not just shared words."),
                "criteria": [
                    "Unrelated to the question.",
                    "Shares a topic but offers no useful answer.",
                    "Provides relevant background or part of the answer.",
                    "Directly answers the question with specific useful information."
                ]
            }),
        );
    }
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(1))
        .timeout(Duration::from_secs(3))
        .redirect(Policy::none())
        .build()
        .ok()?;
    let response = client
        .post("https://api.typesafe.ai/v1/systemone")
        .bearer_auth(key)
        .json(&json!({
            "state": {"query": query, "candidates": candidates},
            "model": "jev-latest",
            "questions": questions,
        }))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let bytes = response.bytes().await.ok()?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        return None;
    }
    let result: Value = serde_json::from_slice(&bytes).ok()?;
    let answers = result["answers"].as_object()?;
    let mut scores = Vec::with_capacity(positions.len());
    for index in 0..positions.len() {
        let answer = &answers[&format!("relevance_{index}")];
        if answer["type"] != "score" {
            return None;
        }
        let score = answer["score"].as_f64()?;
        if !score.is_finite() || !(0.0..=3.0).contains(&score) {
            return None;
        }
        scores.push(score);
    }

    let mut order: Vec<_> = (0..positions.len()).collect();
    order.sort_by(|&left, &right| {
        scores[right]
            .total_cmp(&scores[left])
            .then(left.cmp(&right))
    });
    let mut reranked = items.to_vec();
    for (destination, source) in positions.iter().zip(order) {
        reranked[*destination] = items[positions[source]].clone();
    }
    Some(reranked)
}
