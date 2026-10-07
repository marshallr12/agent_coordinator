//! Per-launch cost (autonomy plan §2.3 Cost, audit item 15). Once a launch
//! has ended, the loop reads its harness events (`$RUN/events.jsonl`),
//! appends the tokens and dollars it used to the root-owned ledger
//! `<state_dir>/costs.jsonl` against its task, session and attempt, and
//! adds the figure to the attempt's handoff summary. Claude reports its own
//! dollar cost; Codex usage is priced with the `[shadow]` price table. The
//! same events reveal a 429, which marks the vendor exhausted.
use super::record::LaunchRecord;
use super::{Driver, Launch, health};
use crate::config::Config;
use crate::estimate;
use crate::profile::Role;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;

/// Tokens and dollars one launch used.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    /// Input tokens not served from the prompt cache.
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    /// USD, when the harness reported it or the model is priced.
    pub usd: Option<f64>,
}

/// What a launch's event stream shows.
#[derive(Debug, Default, PartialEq)]
pub struct Digest {
    pub usage: Usage,
    /// A rate limit (429) ended the launch; holds the reset time (ms since
    /// the epoch) when the harness named one.
    pub exhausted: Option<Option<i64>>,
}

/// Reads every JSON event line: Claude's final `result` (dollars and
/// tokens), Codex's `turn.completed` usage, and rate-limit failures.
pub fn digest(events: &[u8]) -> Digest {
    let mut digest = Digest::default();
    let text = String::from_utf8_lossy(events);
    for event in text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
    {
        claude_result(&event, &mut digest.usage);
        codex_turn(&event, &mut digest.usage);
        if let Some(reset) = rate_limited(&event) {
            digest.exhausted = Some(reset.or(digest.exhausted.flatten()));
        }
    }
    digest
}

/// The `n` field of `usage` as a token count.
fn tokens(usage: &Value, n: &str) -> u64 {
    usage[n].as_u64().unwrap_or(0)
}

/// Claude's `result` event replaces the usage: it totals the whole session.
fn claude_result(event: &Value, usage: &mut Usage) {
    if event["type"] != "result" {
        return;
    }
    let u = &event["usage"];
    *usage = Usage {
        input_tokens: tokens(u, "input_tokens") + tokens(u, "cache_creation_input_tokens"),
        cached_input_tokens: tokens(u, "cache_read_input_tokens"),
        output_tokens: tokens(u, "output_tokens"),
        usd: event["total_cost_usd"]
            .as_f64()
            .filter(|usd| usd.is_finite() && *usd >= 0.0),
    };
}

/// Codex reports each turn's usage; cached tokens are part of its input.
fn codex_turn(event: &Value, usage: &mut Usage) {
    if event["type"] != "turn.completed" {
        return;
    }
    let u = &event["usage"];
    let cached = tokens(u, "cached_input_tokens");
    usage.input_tokens += tokens(u, "input_tokens").saturating_sub(cached);
    usage.cached_input_tokens += cached;
    usage.output_tokens += tokens(u, "output_tokens");
}

/// `Some(reset)` when `event` reports a rate limit that ended the turn:
/// Claude's rejected `rate_limit_event` or failed `result`, or a Codex
/// `error` / `turn.failed`. Tool output is never inspected.
fn rate_limited(event: &Value) -> Option<Option<i64>> {
    let kind = event["type"].as_str()?;
    if kind == "rate_limit_event" {
        let info = &event["rate_limit_info"];
        let rejected = info["status"] == "rejected";
        return rejected.then(|| info["resetsAt"].as_i64().map(|s| s * 1000));
    }
    let (status, text) = match kind {
        "result" if event["is_error"] == true => (&event["api_error_status"], &event["result"]),
        "error" => (&event["status"], &event["message"]),
        "turn.failed" => (&event["error"]["status"], &event["error"]["message"]),
        _ => return None,
    };
    let text = text.as_str().unwrap_or("");
    (status == 429 || limit_phrase(text)).then(|| reset_suffix(text))
}

/// Whether an error message names a rate or usage limit in words; a bare
/// "429" (a line number, a token count) is not enough.
fn limit_phrase(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let phrases = [
        "rate limit",
        "rate_limit",
        "usage limit",
        "too many requests",
    ];
    phrases.iter().any(|phrase| lower.contains(phrase))
}

/// The epoch seconds after `|` in Claude's "usage limit reached|<epoch>".
fn reset_suffix(text: &str) -> Option<i64> {
    let (_, digits) = text.rsplit_once('|')?;
    let digits: String = digits.chars().take_while(char::is_ascii_digit).collect();
    digits.parse::<i64>().ok().map(|s| s * 1000)
}

/// The root-owned ledger, `<state_dir>/costs.jsonl`.
pub fn ledger(config: &Config) -> PathBuf {
    config.state_dir.join("costs.jsonl")
}

/// Settles a finished launch: marks its vendor exhausted on a 429, records
/// its cost in the ledger, and returns the sentence its handoff carries.
pub fn settle(
    driver: &impl Driver,
    config: &Config,
    launch: &Launch,
    record: &LaunchRecord,
) -> String {
    let digest = digest(&driver.events(launch).unwrap_or_default());
    let now = driver.now_ms();
    if let Some(reset) = digest.exhausted {
        let minutes = i64::try_from(config.health.exhausted_minutes).unwrap_or(i64::MAX);
        let until = reset.unwrap_or(now.saturating_add(minutes.saturating_mul(60_000)));
        health::mark_exhausted(config, launch.vendor.harness, until);
    }
    let usage = priced(digest.usage, config, &launch.vendor.model);
    if let Err(error) = append(config, &entry(launch, record, &usage, now)) {
        eprintln!("agentc-supervisor run: record cost: {error:#}");
    }
    note(&usage)
}

/// `usage` with dollars from the price table when the harness gave none.
fn priced(mut usage: Usage, config: &Config, model: &str) -> Usage {
    if usage.usd.is_none()
        && let Some(price) = config.shadow.price_table().get(model)
    {
        let dollars = usage.input_tokens as f64 * price.input
            + usage.cached_input_tokens as f64 * price.cached_input
            + usage.output_tokens as f64 * price.output;
        usage.usd = Some(estimate::cents(dollars / 1_000_000.0));
    }
    usage
}

/// One ledger line for the launch.
fn entry(launch: &Launch, record: &LaunchRecord, usage: &Usage, now: i64) -> Value {
    json!({
        "at_ms": now, "role": Role::Implementer.slug(), "project": launch.project,
        "task": launch.suggestion.task, "session": launch.session_id,
        "attempt": record.attempt, "harness": health::name(launch.vendor.harness),
        "model": launch.vendor.model, "input_tokens": usage.input_tokens,
        "cached_input_tokens": usage.cached_input_tokens,
        "output_tokens": usage.output_tokens, "usd": usage.usd,
    })
}

/// Appends `line` to the ledger (mode 0600, created on first use).
fn append(config: &Config, line: &Value) -> Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(&config.state_dir)?;
    let mut options = std::fs::OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(ledger(config))?;
    Ok(writeln!(file, "{line}")?)
}

/// The handoff's cost sentence.
fn note(usage: &Usage) -> String {
    let usd = usage
        .usd
        .map_or("unpriced".into(), |usd| format!("${usd:.2}"));
    format!(
        "Cost: {usd} ({} input, {} cached, {} output tokens).",
        usage.input_tokens, usage.cached_input_tokens, usage.output_tokens
    )
}

/// Dollars `role` spent since `since` (ms since the epoch), from the ledger.
pub fn spent_since(config: &Config, role: Role, since: i64) -> f64 {
    let text = std::fs::read_to_string(ledger(config)).unwrap_or_default();
    let lines = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok());
    lines
        .filter(|e| e["role"] == role.slug() && e["at_ms"].as_i64().is_some_and(|at| at >= since))
        .filter_map(|e| e["usd"].as_f64())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The events as JSON lines.
    fn lines(events: &[Value]) -> Vec<u8> {
        let text: Vec<String> = events.iter().map(Value::to_string).collect();
        text.join("\n").into_bytes()
    }

    #[test]
    fn codex_turns_add_up_and_cached_tokens_leave_the_input() {
        let turn = json!({"type": "turn.completed",
            "usage": {"input_tokens": 100, "cached_input_tokens": 60, "output_tokens": 7}});
        let digest = digest(&lines(&[turn.clone(), turn]));
        let usage = Usage {
            input_tokens: 80,
            cached_input_tokens: 120,
            output_tokens: 14,
            usd: None,
        };
        assert_eq!(
            digest,
            Digest {
                usage,
                exhausted: None
            }
        );
    }

    #[test]
    fn codex_and_claude_rate_limits_are_recognised() {
        let failed = json!({"type": "turn.failed", "error": {"message": "429 Too Many Requests"}});
        assert_eq!(digest(&lines(&[failed])).exhausted, Some(None));
        let error = json!({"type": "error", "message": "You've hit your usage limit."});
        assert_eq!(digest(&lines(&[error])).exhausted, Some(None));
        let api = json!({"type": "result", "is_error": true, "api_error_status": 429});
        assert_eq!(digest(&lines(&[api])).exhausted, Some(None));
        let limit = json!({"type": "result", "is_error": true,
            "result": "Claude AI usage limit reached|1700000000"});
        assert_eq!(
            digest(&lines(&[limit])).exhausted,
            Some(Some(1_700_000_000_000))
        );
    }

    #[test]
    fn tool_output_and_token_counts_never_look_like_a_429() {
        let tool = json!({"type": "user", "message": {"content": "HTTP 429 rate limit"}});
        let allowed = json!({"type": "rate_limit_event",
            "rate_limit_info": {"status": "allowed", "resetsAt": 1}});
        let ok = json!({"type": "result", "is_error": false, "total_cost_usd": 0.5,
            "usage": {"input_tokens": 1429}});
        let digest = digest(&lines(&[tool, allowed, ok]));
        assert_eq!((digest.exhausted, digest.usage.usd), (None, Some(0.5)));
    }

    #[test]
    fn a_negative_reported_cost_is_ignored() {
        let result = json!({"type": "result", "total_cost_usd": -3.0});
        assert_eq!(digest(&lines(&[result])).usage.usd, None);
    }

    #[test]
    fn an_unrelated_error_mentioning_429_marks_nothing() {
        let error = json!({"type": "error", "message": "parse error at line 429"});
        let failed = json!({"type": "turn.failed", "error": {"message": "exit 1 after 429 ms"}});
        assert_eq!(digest(&lines(&[error, failed])).exhausted, None);
        let status = json!({"type": "turn.failed", "error": {"status": 429, "message": "x"}});
        assert_eq!(digest(&lines(&[status])).exhausted, Some(None));
    }

    #[test]
    fn unreported_dollars_come_from_the_price_table() {
        let usage = Usage {
            input_tokens: 1_000_000,
            cached_input_tokens: 0,
            output_tokens: 0,
            usd: None,
        };
        let config = Config::default();
        assert_eq!(
            priced(usage.clone(), &config, "claude-opus-5-5").usd,
            Some(4.0)
        );
        assert_eq!(priced(usage, &config, "unknown").usd, None);
    }
}
