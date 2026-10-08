//! Per-launch cost (autonomy plan §2.3 Cost, audit item 15). Once a launch
//! has ended, the loop reads its harness events (`$RUN/events.jsonl`),
//! appends the tokens and dollars it used to the root-owned ledger
//! `<state_dir>/costs.jsonl` against its task, session and attempt, and
//! adds the figure to the attempt's handoff summary. Claude reports its own
//! dollar cost; Codex usage is priced with the `[shadow]` price table. The
//! same events reveal a 429, which marks the vendor exhausted. A Claude
//! launch killed before its `result` event is costed from the usage of its
//! `assistant` events and its row is marked `usd_estimated`. A reviewer
//! launch's cost is recorded the same way ([`settle_review`]), once per
//! session.
use super::record::LaunchRecord;
use super::review_cost::ReviewLaunch;
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
    /// The launch wrote no `result` event: the figures sum its `assistant`
    /// events and the dollars come from the price table.
    pub estimated: bool,
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
/// tokens, else the sum of its `assistant` messages), Codex's
/// `turn.completed` usage, and rate-limit failures.
pub fn digest(events: &[u8]) -> Digest {
    let mut digest = Digest::default();
    let mut assistant = Assistant::default();
    let mut finished = false;
    let text = String::from_utf8_lossy(events);
    for event in text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
    {
        finished |= event["type"] == "result";
        claude_result(&event, &mut digest.usage);
        assistant.add(&event);
        codex_turn(&event, &mut digest.usage);
        if let Some(reset) = rate_limited(&event) {
            digest.exhausted = Some(reset.or(digest.exhausted.flatten()));
        }
    }
    if !finished && digest.usage == Usage::default() {
        digest.usage = assistant.total();
    }
    digest
}

/// The usage of Claude's `assistant` events, kept per message: Claude
/// writes one event per content block of a message, each repeating the
/// message's usage, so a message counts once, by its last report.
#[derive(Default)]
struct Assistant {
    messages: Vec<(Option<String>, Usage)>,
}

impl Assistant {
    fn add(&mut self, event: &Value) {
        if event["type"] != "assistant" || !event["message"]["usage"].is_object() {
            return;
        }
        let message = &event["message"];
        let id = message["id"].as_str().map(str::to_owned);
        let u = &message["usage"];
        let usage = Usage {
            input_tokens: tokens(u, "input_tokens") + tokens(u, "cache_creation_input_tokens"),
            cached_input_tokens: tokens(u, "cache_read_input_tokens"),
            output_tokens: tokens(u, "output_tokens"),
            ..Usage::default()
        };
        match self
            .messages
            .iter_mut()
            .find(|(seen, _)| id.is_some() && *seen == id)
        {
            Some((_, last)) => *last = usage,
            None => self.messages.push((id, usage)),
        }
    }

    /// The per-turn usage summed, marked estimated; zero when no assistant
    /// event reported usage.
    fn total(self) -> Usage {
        let mut total = Usage::default();
        for (_, usage) in &self.messages {
            total.input_tokens += usage.input_tokens;
            total.cached_input_tokens += usage.cached_input_tokens;
            total.output_tokens += usage.output_tokens;
        }
        total.estimated = !self.messages.is_empty();
        total
    }
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
        estimated: false,
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

/// Records a finished reviewer launch's cost in the ledger against its task
/// and session, unless the ledger already holds that session's reviewer row.
/// Returns whether the ledger now holds it.
pub fn settle_review(config: &Config, launch: &ReviewLaunch, events: &[u8], now: i64) -> bool {
    if recorded(config, Role::Reviewer, &launch.session.to_string()) {
        return true;
    }
    let usage = priced(digest(events).usage, config, &launch.vendor.model);
    let line = json!({
        "at_ms": now, "role": Role::Reviewer.slug(), "project": launch.project,
        "task": launch.task, "session": launch.session,
        "attempt": launch.attempt, "harness": health::name(launch.vendor.harness),
        "model": launch.vendor.model, "input_tokens": usage.input_tokens,
        "cached_input_tokens": usage.cached_input_tokens,
        "output_tokens": usage.output_tokens, "usd": usage.usd,
    });
    let line = estimated(line, &usage);
    let appended = append(config, &line);
    if let Err(error) = &appended {
        eprintln!("agentc-supervisor run: record review cost: {error:#}");
    }
    appended.is_ok()
}

/// Whether the ledger holds a row of `role` for `session`.
fn recorded(config: &Config, role: Role, session: &str) -> bool {
    let text = std::fs::read_to_string(ledger(config)).unwrap_or_default();
    let mut lines = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok());
    lines.any(|e| e["role"] == role.slug() && e["session"] == session)
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
    let line = json!({
        "at_ms": now, "role": Role::Implementer.slug(), "project": launch.project,
        "task": launch.suggestion.task, "session": launch.session_id,
        "attempt": record.attempt, "harness": health::name(launch.vendor.harness),
        "model": launch.vendor.model, "input_tokens": usage.input_tokens,
        "cached_input_tokens": usage.cached_input_tokens,
        "output_tokens": usage.output_tokens, "usd": usage.usd,
    });
    estimated(line, usage)
}

/// `line` with `usd_estimated: true` when the usage was summed from
/// `assistant` events; rows costed from a `result` carry no such field.
fn estimated(mut line: Value, usage: &Usage) -> Value {
    if usage.estimated {
        line["usd_estimated"] = json!(true);
    }
    line
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
    let estimate = if usage.estimated { ", estimated" } else { "" };
    format!(
        "Cost: {usd} ({} input, {} cached, {} output tokens{estimate}).",
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
            estimated: false,
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
            estimated: false,
        };
        let config = Config::default();
        assert_eq!(
            priced(usage.clone(), &config, "claude-opus-5-5").usd,
            Some(4.0)
        );
        assert_eq!(priced(usage, &config, "unknown").usd, None);
    }

    /// A Claude `assistant` event of message `id` reporting `output` tokens.
    fn assistant(id: &str, output: u64) -> Value {
        json!({"type": "assistant", "message": {"id": id, "usage": {
            "input_tokens": 10, "cache_creation_input_tokens": 5,
            "cache_read_input_tokens": 100, "output_tokens": output}}})
    }

    #[test]
    fn a_launch_without_a_result_is_summed_from_its_assistant_events() {
        let events = [
            assistant("m1", 3),
            assistant("m1", 7),
            assistant("m2", 1_000_000),
            json!({"type": "user", "message": {"content": "tool output"}}),
        ];
        let usage = digest(&lines(&events)).usage;
        assert_eq!(
            usage,
            Usage {
                input_tokens: 30,
                cached_input_tokens: 200,
                output_tokens: 1_000_007,
                usd: None,
                estimated: true,
            }
        );
        let config = Config::default();
        let priced = priced(usage, &config, "claude-opus-5-5");
        assert_eq!(priced.usd, Some(20.0));
        assert!(note(&priced).ends_with("1000007 output tokens, estimated)."));
    }

    #[test]
    fn a_result_event_wins_over_assistant_events() {
        let result = json!({"type": "result", "total_cost_usd": 2.0,
            "usage": {"input_tokens": 1, "output_tokens": 2}});
        let usage = digest(&lines(&[assistant("m1", 9), result])).usage;
        assert_eq!((usage.estimated, usage.usd), (false, Some(2.0)));
        assert_eq!((usage.input_tokens, usage.output_tokens), (1, 2));
    }

    #[test]
    fn events_with_no_assistant_usage_stay_zero_and_unestimated() {
        assert_eq!(digest(b"").usage, Usage::default());
        let user = json!({"type": "user", "message": {"content": "x"}});
        assert_eq!(digest(&lines(&[user])).usage, Usage::default());
    }
}
