//! Admission before each claim (autonomy plan §2.3 Health and Cost, audit
//! items 8 and 15): the kill switch, the per-role daily spend cap, and the
//! vendor the launch runs with. A vendor is skipped while a 429 marked it
//! exhausted (`<state_dir>/vendors.json`), once its credential has expired,
//! or when its own sign-in check (`claude auth status` / `codex login
//! status`) fails; the loop then routes to the configured fallback vendor.
//! Once a day the loop warns about credentials that expire soon.
use super::{Driver, RunConfig};
use crate::config::Config;
use crate::profile::{Harness, Role};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// One day in milliseconds.
pub const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// `[health]` settings; every entry has a default.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct HealthConfig {
    /// While this path exists the loop claims nothing (default
    /// `<state_dir>/kill-switch`).
    pub kill_switch: Option<PathBuf>,
    /// The vendor launches route to while `[run] harness` is unusable.
    pub fallback: Option<Vendor>,
    /// How long a 429 without a reset time marks a vendor exhausted.
    pub exhausted_minutes: u64,
    /// How long a `claude setup-token` token lives after it is written.
    pub token_lifetime_days: u64,
    /// Warn this many days before a credential expires.
    pub expiry_warn_days: u64,
    /// Daily spend cap (USD, last 24 hours) for implementer launches; 0 disables.
    pub implementer_daily_usd: f64,
    /// Daily spend cap (USD, last 24 hours) for reviewer launches; 0 disables.
    pub reviewer_daily_usd: f64,
}

impl Default for HealthConfig {
    /// No fallback, an hour of exhaustion, year-long tokens warned two weeks
    /// ahead, and pilot-sized daily caps.
    fn default() -> Self {
        Self {
            kill_switch: None,
            fallback: None,
            exhausted_minutes: 60,
            token_lifetime_days: 365,
            expiry_warn_days: 14,
            implementer_daily_usd: 150.0,
            reviewer_daily_usd: 50.0,
        }
    }
}

impl HealthConfig {
    /// The daily cap of `role` in USD (0 means none).
    pub fn cap(&self, role: Role) -> f64 {
        match role {
            Role::Implementer => self.implementer_daily_usd,
            Role::Reviewer => self.reviewer_daily_usd,
        }
    }
}

/// The harness, model and effort a launch runs with.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Vendor {
    pub harness: Harness,
    pub model: String,
    #[serde(default = "default_effort")]
    pub effort: String,
}

/// A fallback vendor's effort when none is configured.
fn default_effort() -> String {
    "high".into()
}

impl Vendor {
    /// The vendor `[run]` names.
    pub fn primary(run: &RunConfig) -> Self {
        Self {
            harness: run.harness,
            model: run.model.clone(),
            effort: run.effort.clone(),
        }
    }
}

/// The harness's name in state files, logs and the cost ledger.
pub fn name(harness: Harness) -> &'static str {
    match harness {
        Harness::Claude => "claude",
        Harness::Codex => "codex",
    }
}

/// Per-host vendor state that outlives one poll, `<state_dir>/vendors.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct VendorState {
    /// Until when (ms since the epoch) each harness is exhausted.
    exhausted_until: BTreeMap<String, i64>,
    /// When the daily credential-expiry check last ran.
    expiry_checked_ms: i64,
}

impl VendorState {
    /// `<state_dir>/vendors.json`.
    fn path(config: &Config) -> PathBuf {
        config.state_dir.join("vendors.json")
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

/// Marks `harness` exhausted until `until` (ms since the epoch), so the
/// loop routes to the other vendor meanwhile.
pub fn mark_exhausted(config: &Config, harness: Harness, until: i64) {
    let mut state = VendorState::load(config);
    state.exhausted_until.insert(name(harness).into(), until);
    eprintln!(
        "agentc-supervisor run: {} hit a rate limit; exhausted until {}",
        name(harness),
        shown(until)
    );
    if let Err(error) = state.save(config) {
        eprintln!("agentc-supervisor run: save vendor state: {error:#}");
    }
}

/// The kill switch's path: configured, else `<state_dir>/kill-switch`.
pub fn kill_switch(config: &Config) -> PathBuf {
    let default = || config.state_dir.join("kill-switch");
    config.health.kill_switch.clone().unwrap_or_else(default)
}

/// Why no claim may start now, or else the vendor the next launch runs
/// with. Checked every poll, so setting the kill switch stops new claims
/// within one poll.
pub fn admit(driver: &mut impl Driver, config: &Config) -> Result<Vendor, String> {
    let switch = kill_switch(config);
    if switch.symlink_metadata().is_ok() {
        return Err(format!("the kill switch {} is set", switch.display()));
    }
    let now = driver.now_ms();
    capped(config, Role::Implementer, now)?;
    let mut state = VendorState::load(config);
    warn_daily(driver, config, &mut state, now);
    let mut reasons = Vec::new();
    for vendor in vendors(config) {
        match unusable(driver, &state, &vendor, now) {
            None => return Ok(vendor),
            Some(reason) => reasons.push(reason),
        }
    }
    Err(format!("no vendor is usable: {}", reasons.join("; ")))
}

/// The primary vendor, then the fallback if one is configured.
fn vendors(config: &Config) -> Vec<Vendor> {
    let primary = Vendor::primary(&config.run);
    std::iter::once(primary)
        .chain(config.health.fallback.clone())
        .collect()
}

/// Refuses when `role` has spent its daily cap in the last 24 hours.
pub fn capped(config: &Config, role: Role, now: i64) -> Result<(), String> {
    let cap = config.health.cap(role);
    let spent = super::cost::spent_since(config, role, now - DAY_MS);
    if cap > 0.0 && spent >= cap {
        let role = role.slug();
        return Err(format!(
            "{role} spent ${spent:.2} in 24 h, reaching its ${cap:.2} cap"
        ));
    }
    Ok(())
}

/// Why `vendor` cannot run a launch now, if it cannot.
fn unusable(
    driver: &mut impl Driver,
    state: &VendorState,
    vendor: &Vendor,
    now: i64,
) -> Option<String> {
    let name = name(vendor.harness);
    if let Some(until) = state.exhausted_until.get(name).filter(|&&at| at > now) {
        return Some(format!("{name} is exhausted until {}", shown(*until)));
    }
    if let Some(expiry) = driver.credential_expiry_ms(vendor.harness)
        && expiry <= now
    {
        return Some(format!("{name}'s credential expired at {}", shown(expiry)));
    }
    let status = driver.harness_status(vendor.harness);
    status
        .err()
        .map(|error| format!("{name} is not signed in: {error:#}"))
}

/// Once a day, warns about every vendor credential that expires within
/// `[health] expiry_warn_days`.
fn warn_daily(driver: &impl Driver, config: &Config, state: &mut VendorState, now: i64) {
    if now - state.expiry_checked_ms < DAY_MS && state.expiry_checked_ms != 0 {
        return;
    }
    let ahead = i64::try_from(config.health.expiry_warn_days).unwrap_or(i64::MAX);
    for vendor in vendors(config) {
        let expiry = driver.credential_expiry_ms(vendor.harness);
        if let Some(at) = expiry.filter(|&at| at - now <= ahead.saturating_mul(DAY_MS)) {
            let name = name(vendor.harness);
            eprintln!(
                "agentc-supervisor run: warning: {name}'s credential expires at {}",
                shown(at)
            );
        }
    }
    state.expiry_checked_ms = now;
    if let Err(error) = state.save(config) {
        eprintln!("agentc-supervisor run: save vendor state: {error:#}");
    }
}

/// `ms` since the epoch as RFC 3339, for logs and refusals.
fn shown(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms).map_or_else(|| ms.to_string(), |at| at.to_rfc3339())
}
