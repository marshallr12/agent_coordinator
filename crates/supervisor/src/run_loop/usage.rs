//! How much usage each vendor account has left in its current limit window,
//! for the choice of vendor ([`super::select`]). Only figures the vendor's
//! own CLI gives the role's own account are used; none costs inference:
//!
//! - **Codex**: `codex app-server` (the documented JSON-RPC interface the
//!   Codex clients use) answers `account/rateLimits/read` with the account's
//!   `primary` and `secondary` windows (`usedPercent`, `windowDurationMins`,
//!   `resetsAt`). The supervisor asks for it as the role, at most once per
//!   `cache_minutes`.
//! - **Claude**: nothing can be asked on demand. The role's long-lived token
//!   cannot read the account's usage endpoint, and the
//!   `anthropic-ratelimit-unified-*` response headers come only with an
//!   inference request. What Claude Code does emit, free, is a
//!   `rate_limit_event` in its own event stream (`utilization` per window,
//!   `unifiedWindows.five_hour` / `seven_day`, `resetsAt`); the supervisor
//!   keeps the last one a launch of the role wrote. Without one, or once it
//!   is older than `stale_minutes` or its window has reset, Claude's
//!   remaining usage is unknown.
//!
//! Readings are cached in `<state_dir>/usage.json` with the time they were
//! read, so a poll inside the cache period queries nothing, and a stale
//! reading counts as unknown.
use super::select::SelectionConfig;
use crate::config::Config;
use crate::profile::{Harness, Role};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// One limit window of an account.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    /// The window's length ("5h", "7d") or the vendor's name for it.
    pub label: String,
    /// Share of the window used, in percent (may exceed 100).
    pub used_percent: f64,
    /// When the window resets (ms since the epoch), if the vendor says.
    pub resets_at_ms: Option<i64>,
}

/// The window with the least left: the one that limits the account.
pub fn most_used(windows: &[Window]) -> Option<&Window> {
    (windows.iter()).max_by(|a, b| a.used_percent.total_cmp(&b.used_percent))
}

/// What is known of one vendor's remaining usage.
#[derive(Debug, Clone, PartialEq)]
pub enum Reading {
    Known {
        /// Share left in the limiting window, in percent (0 to 100).
        remaining_percent: f64,
        window: String,
        /// How old the reading is (ms).
        age_ms: i64,
        source: String,
    },
    /// No usable figure, and why.
    Unknown(String),
}

impl Reading {
    /// The share left, when known.
    pub fn remaining(&self) -> Option<f64> {
        match self {
            Self::Known {
                remaining_percent, ..
            } => Some(*remaining_percent),
            Self::Unknown(_) => None,
        }
    }

    /// The reading as one sentence for the heartbeat, the launch record and
    /// the cost ledger.
    pub fn describe(&self, harness: Harness) -> String {
        let name = super::health::name(harness);
        match self {
            Self::Known {
                remaining_percent,
                window,
                age_ms,
                source,
            } => format!(
                "{name} {remaining_percent:.0}% left ({window} window, read {} min ago, {source})",
                age_ms / 60_000
            ),
            Self::Unknown(why) => format!("{name} remaining usage unknown ({why})"),
        }
    }
}

/// One cached reading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Stored {
    /// When the figure was read (ms since the epoch).
    read_ms: i64,
    /// When the vendor was last asked (ms), even if it gave no figure.
    polled_ms: i64,
    used_percent: f64,
    window: String,
    resets_at_ms: Option<i64>,
    source: String,
}

/// `<state_dir>/usage.json`: the cached readings by `<role>/<harness>`.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct UsageState {
    readings: BTreeMap<String, Stored>,
}

impl UsageState {
    fn path(config: &Config) -> PathBuf {
        config.state_dir.join("usage.json")
    }

    /// The saved state; a missing or unreadable file is an empty state.
    fn load(config: &Config) -> Self {
        let bytes = std::fs::read(Self::path(config)).unwrap_or_default();
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    /// Writes the state atomically (temporary file, then rename).
    fn save(&self, config: &Config) -> Result<()> {
        let path = Self::path(config);
        std::fs::create_dir_all(&config.state_dir).context("create the state directory")?;
        let temp = path.with_extension("tmp");
        std::fs::write(&temp, serde_json::to_vec(self)?)?;
        std::fs::rename(&temp, &path).with_context(|| format!("replace {}", path.display()))
    }
}

/// The cache key of a role's account at a vendor.
fn key(role: Role, harness: Harness) -> String {
    format!("{}/{}", role.slug(), super::health::name(harness))
}

/// The cache period and the age past which a reading is stale, in ms.
fn periods(settings: &SelectionConfig) -> (i64, i64) {
    let minutes = |m: u64| i64::try_from(m).unwrap_or(i64::MAX).saturating_mul(60_000);
    (
        minutes(settings.cache_minutes),
        minutes(settings.stale_minutes),
    )
}

/// Caches `windows` (the limiting one) as read at `now` from `source`.
fn store(
    config: &Config,
    role: Role,
    harness: Harness,
    windows: &[Window],
    now: i64,
    source: &str,
) {
    let Some(window) = most_used(windows) else {
        return;
    };
    let mut state = UsageState::load(config);
    let entry = state.readings.entry(key(role, harness)).or_insert(Stored {
        read_ms: now,
        polled_ms: 0,
        used_percent: 0.0,
        window: String::new(),
        resets_at_ms: None,
        source: String::new(),
    });
    entry.read_ms = now;
    entry.used_percent = window.used_percent;
    entry.window = window.label.clone();
    entry.resets_at_ms = window.resets_at_ms;
    entry.source = source.to_owned();
    if let Err(error) = state.save(config) {
        eprintln!("agentc-supervisor run: save usage readings: {error:#}");
    }
}

/// Keeps the last rate-limit figure a finished launch of `role` wrote in its
/// event stream (Claude only; Codex's events carry none).
pub fn record_events(config: &Config, role: Role, harness: Harness, events: &[u8], now: i64) {
    if harness != Harness::Claude {
        return;
    }
    store(
        config,
        role,
        harness,
        &claude_windows(events),
        now,
        "rate_limit_event",
    );
}

/// The remaining usage of `role`'s account at `harness` at `now`. When the
/// vendor can be asked (`poll` is `Some` and the cached answer is older than
/// the cache period) it is asked once and the answer cached; otherwise the
/// cache is used without any query.
pub fn reading(
    config: &Config,
    role: Role,
    harness: Harness,
    settings: &SelectionConfig,
    now: i64,
    poll: &mut dyn FnMut() -> Result<Option<Vec<Window>>>,
) -> Reading {
    let (cache, stale) = periods(settings);
    let name = key(role, harness);
    let polled = UsageState::load(config)
        .readings
        .get(&name)
        .map_or(0, |s| s.polled_ms);
    if polled == 0 || now.saturating_sub(polled) >= cache {
        match poll() {
            Ok(Some(windows)) => {
                store(config, role, harness, &windows, now, "codex app-server");
                mark_polled(config, role, harness, now);
            }
            Ok(None) => mark_polled(config, role, harness, now),
            Err(error) => {
                eprintln!(
                    "agentc-supervisor run: {} usage query failed: {error:#}",
                    super::health::name(harness)
                );
                mark_polled(config, role, harness, now);
            }
        }
    }
    let state = UsageState::load(config);
    let Some(stored) = state.readings.get(&name).filter(|s| s.read_ms > 0) else {
        return Reading::Unknown("no reading yet".into());
    };
    let age = now.saturating_sub(stored.read_ms);
    if age > stale {
        return Reading::Unknown(format!("stale: read {} min ago", age / 60_000));
    }
    if stored.resets_at_ms.is_some_and(|at| at <= now) {
        return Reading::Unknown(format!(
            "the {} window has reset since the reading",
            stored.window
        ));
    }
    Reading::Known {
        remaining_percent: (100.0 - stored.used_percent).clamp(0.0, 100.0),
        window: stored.window.clone(),
        age_ms: age,
        source: stored.source.clone(),
    }
}

/// Notes that the vendor was asked at `now`, so a failed or empty answer is
/// not asked for again within the cache period.
fn mark_polled(config: &Config, role: Role, harness: Harness, now: i64) {
    let mut state = UsageState::load(config);
    let entry = state.readings.entry(key(role, harness)).or_insert(Stored {
        read_ms: 0,
        polled_ms: now,
        used_percent: 100.0,
        window: String::new(),
        resets_at_ms: None,
        source: String::new(),
    });
    entry.polled_ms = now;
    if let Err(error) = state.save(config) {
        eprintln!("agentc-supervisor run: save usage readings: {error:#}");
    }
}

/// A window's length in minutes as "5h", "7d", ...
fn length(minutes: i64) -> String {
    match minutes {
        m if m % (24 * 60) == 0 && m > 0 => format!("{}d", m / (24 * 60)),
        m if m % 60 == 0 && m > 0 => format!("{}h", m / 60),
        m => format!("{m}m"),
    }
}

/// The windows in Codex's `account/rateLimits/read` result: the `codex`
/// bucket when the answer has several, else the single-bucket view.
pub fn codex_windows(result: &Value) -> Vec<Window> {
    let snapshot = result["rateLimitsByLimitId"]
        .get("codex")
        .filter(|v| v.is_object())
        .unwrap_or(&result["rateLimits"]);
    let mut windows = Vec::new();
    for name in ["primary", "secondary"] {
        let window = &snapshot[name];
        let Some(used) = window["usedPercent"].as_f64() else {
            continue;
        };
        let label = (window["windowDurationMins"].as_i64()).map_or(name.to_owned(), length);
        windows.push(Window {
            label,
            used_percent: used,
            resets_at_ms: window["resetsAt"].as_i64().map(|s| s.saturating_mul(1000)),
        });
    }
    if !snapshot["rateLimitReachedType"].is_null() || result["ordinaryUsageAllowed"] == false {
        windows.push(Window {
            label: "limit reached".into(),
            used_percent: 100.0,
            resets_at_ms: None,
        });
    }
    windows
}

/// What a line of `codex app-server` output says about request `id`: `None`
/// for any other line, else the windows of the answer or its error.
pub fn codex_answer(line: &str, id: u64) -> Option<Result<Vec<Window>>> {
    let value: Value = serde_json::from_str(line).ok()?;
    if value["id"].as_u64() != Some(id) {
        return None;
    }
    if let Some(message) = value["error"]["message"].as_str() {
        return Some(Err(anyhow::anyhow!("codex answered: {message}")));
    }
    let windows = codex_windows(&value["result"]);
    Some(if windows.is_empty() {
        Err(anyhow::anyhow!("codex answered with no rate-limit window"))
    } else {
        Ok(windows)
    })
}

/// The windows of the last `rate_limit_event` in a Claude event stream that
/// carries a figure (`utilization` is the fraction of the window used).
pub fn claude_windows(events: &[u8]) -> Vec<Window> {
    let mut last = Vec::new();
    let text = String::from_utf8_lossy(events);
    for event in (text.lines()).filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        if event["type"] != "rate_limit_event" {
            continue;
        }
        let info = &event["rate_limit_info"];
        let seconds = |v: &Value| v.as_i64().map(|s| s.saturating_mul(1000));
        let mut found = Vec::new();
        if let Some(windows) = info["unifiedWindows"].as_object() {
            for (name, window) in windows {
                if let Some(used) = window["utilization"].as_f64() {
                    found.push(Window {
                        label: claude_label(name),
                        used_percent: used * 100.0,
                        resets_at_ms: seconds(&window["resetsAt"]),
                    });
                }
            }
        }
        if found.is_empty()
            && let Some(used) = info["utilization"].as_f64()
        {
            found.push(Window {
                label: claude_label(info["rateLimitType"].as_str().unwrap_or("limit")),
                used_percent: used * 100.0,
                resets_at_ms: seconds(&info["resetsAt"]),
            });
        }
        if !found.is_empty() {
            last = found;
        }
    }
    last
}

/// Claude's window name as a short label.
fn claude_label(name: &str) -> String {
    match name {
        "five_hour" => "5h".into(),
        "seven_day" => "7d".into(),
        other => other.replace('_', " "),
    }
}

#[cfg(test)]
mod tests;
