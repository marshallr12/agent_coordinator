//! The choice of vendor before each claim, from the usage each vendor
//! account has left (`[health.selection]` for implementer launches,
//! `[health.reviewer_selection]` for reviewer launches, each with its own
//! settings and the role's own account).
//!
//! With `mode = "off"` (the default) nothing here runs: implementers use
//! `[run]` and the fallback after a failure, reviewers use `[run]`. The
//! existing admission rules (kill switch, daily caps, exhausted marks,
//! credential expiry, sign-in checks) are applied to every candidate first
//! and are not weakened; a vendor they rule out, or that is not installed,
//! is never chosen. The rule over the rest ([`decide`]):
//!
//! 1. a role fixed to `claude` or `codex` only ever considers that vendor;
//! 2. one candidate left: it is chosen;
//! 3. a vendor known to have less than `min_remaining_percent` left is
//!    passed over while another is known to have at least that much;
//! 4. all remaining candidates have a fresh reading: the one with more left
//!    wins, and readings within one percentage point tie, which `order`
//!    settles;
//! 5. any remaining candidate with an unknown reading (none, stale, its
//!    window reset, or the vendor gives no figure): unknown is neither
//!    preferred nor excluded, so `order` settles it. The 429 path
//!    ([`super::health::mark_exhausted`]) stays the backstop.
use super::health::{self, Probe, Vendor, VendorState};
use super::usage::{self, Reading};
use crate::config::Config;
use crate::profile::Harness;
use serde::{Deserialize, Serialize};

/// Whether and how a role's launches choose their vendor.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// No selection (the default): today's behaviour.
    #[default]
    Off,
    /// Always Claude; never switched.
    Claude,
    /// Always Codex; never switched.
    Codex,
    /// Whichever has more usage left.
    Usage,
}

/// The model and effort a vendor runs with when it is chosen.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Model {
    pub model: String,
    #[serde(default = "default_effort")]
    pub effort: String,
}

/// A vendor's effort when none is configured.
fn default_effort() -> String {
    "high".into()
}

/// One role's selection settings; every entry has a default.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct SelectionConfig {
    pub mode: Mode,
    /// Preference order; it settles ties and unknown readings.
    pub order: Vec<Harness>,
    /// A vendor with less than this share (percent) left is not chosen
    /// while another has at least that much.
    pub min_remaining_percent: f64,
    /// How long a vendor's answer is reused before it is asked again.
    pub cache_minutes: u64,
    /// A reading older than this counts as unknown.
    pub stale_minutes: u64,
    /// Claude's model and effort; default: `[run]` when it names Claude,
    /// else `[health.fallback]` when it does.
    pub claude: Option<Model>,
    /// Codex's model and effort, resolved the same way.
    pub codex: Option<Model>,
}

impl Default for SelectionConfig {
    /// Off; Claude before Codex; a 10% minimum; answers cached ten minutes
    /// and stale after an hour.
    fn default() -> Self {
        Self {
            mode: Mode::Off,
            order: vec![Harness::Claude, Harness::Codex],
            min_remaining_percent: 10.0,
            cache_minutes: 10,
            stale_minutes: 60,
            claude: None,
            codex: None,
        }
    }
}

impl SelectionConfig {
    /// The harnesses in preference order: `order`, then any it omits.
    fn ordered(&self) -> Vec<Harness> {
        let mut order = Vec::new();
        for harness in self.order.iter().chain(&[Harness::Claude, Harness::Codex]) {
            if !order.contains(harness) {
                order.push(*harness);
            }
        }
        order
    }

    /// The vendor `harness` runs as, or why it has none.
    pub fn vendor(&self, config: &Config, harness: Harness) -> Result<Vendor, String> {
        let own = match harness {
            Harness::Claude => &self.claude,
            Harness::Codex => &self.codex,
        };
        if let Some(model) = own {
            let (model, effort) = (model.model.clone(), model.effort.clone());
            return Ok(Vendor {
                harness,
                model,
                effort,
            });
        }
        let primary = Vendor::primary(&config.run);
        let named = std::iter::once(primary).chain(config.health.fallback.clone());
        let mut found = named.filter(|v| v.harness == harness);
        found.next().ok_or_else(|| {
            let name = health::name(harness);
            format!("no model is configured for {name}")
        })
    }
}

/// The vendor a launch runs with and why: the rule that decided and the
/// readings it saw. Recorded in the heartbeat, the launch record and the
/// cost ledger.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Choice {
    pub vendor: Vendor,
    /// The deciding rule; empty when nothing was chosen (selection is off).
    pub rule: String,
    pub readings: Vec<String>,
}

impl Choice {
    /// `vendor` without a selection behind it (selection off).
    pub fn unselected(vendor: Vendor) -> Self {
        Self {
            vendor,
            rule: String::new(),
            readings: Vec::new(),
        }
    }

    /// The choice to record, if a selection made it.
    pub fn recorded(&self) -> Option<Self> {
        (!self.rule.is_empty()).then(|| self.clone())
    }

    /// The choice as one sentence.
    pub fn sentence(&self) -> String {
        format!(
            "{} ({}) by rule {}: {}",
            health::name(self.vendor.harness),
            self.vendor.model,
            self.rule,
            self.readings.join("; ")
        )
    }
}

/// A usable vendor and what is known of its remaining usage.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub vendor: Vendor,
    pub reading: Reading,
}

/// Two readings this many percentage points apart or less tie.
const TIE_POINTS: f64 = 1.0;

/// The vendor among `candidates` (in preference order) that `settings`
/// choose, or why none can run. `ruled_out` are the reasons vendors were
/// left out; they are kept in the record.
pub fn decide(
    settings: &SelectionConfig,
    candidates: &[Candidate],
    ruled_out: &[String],
) -> Result<Choice, String> {
    if candidates.is_empty() {
        return Err(format!("no vendor is usable: {}", ruled_out.join("; ")));
    }
    let mut readings: Vec<String> = (candidates.iter())
        .map(|c| c.reading.describe(c.vendor.harness))
        .collect();
    readings.extend(ruled_out.iter().map(|why| format!("ruled out: {why}")));
    let choose = |candidate: &Candidate, rule: &str| {
        Ok(Choice {
            vendor: candidate.vendor.clone(),
            rule: rule.into(),
            readings: readings.clone(),
        })
    };
    if settings.mode != Mode::Usage {
        return choose(&candidates[0], "fixed");
    }
    let remaining = |c: &Candidate| c.reading.remaining();
    let minimum = settings.min_remaining_percent;
    let another_above = candidates
        .iter()
        .any(|c| remaining(c).is_some_and(|r| r >= minimum));
    let low = |c: &Candidate| remaining(c).is_some_and(|r| r < minimum);
    let kept: Vec<&Candidate> = (candidates.iter())
        .filter(|c| !(another_above && low(c)))
        .collect();
    if kept.len() == 1 {
        let rule = if candidates.len() == 1 {
            "only_usable"
        } else {
            "below_minimum_skipped"
        };
        return choose(kept[0], rule);
    }
    if kept.iter().any(|c| remaining(c).is_none()) {
        return choose(kept[0], "unknown_by_order");
    }
    let best = kept.iter().filter_map(|c| remaining(c)).fold(0.0, f64::max);
    let near = |c: &&&Candidate| remaining(c).is_some_and(|r| best - r <= TIE_POINTS);
    let tied: Vec<&&Candidate> = kept.iter().filter(near).collect();
    if tied.len() > 1 {
        return choose(tied[0], "tie_by_order");
    }
    choose(tied[0], "more_remaining")
}

/// Chooses the vendor of one launch of `role`: every candidate the mode
/// allows is checked as the existing admission checks it, then read, then
/// decided. `Err` names why no vendor can run.
pub fn choose(
    probe: &mut dyn Probe,
    config: &Config,
    settings: &SelectionConfig,
    state: &VendorState,
) -> Result<Choice, String> {
    let (role, now) = (probe.role(), probe.now_ms());
    let harnesses = match settings.mode {
        Mode::Off | Mode::Usage => settings.ordered(),
        Mode::Claude => vec![Harness::Claude],
        Mode::Codex => vec![Harness::Codex],
    };
    let (mut candidates, mut ruled_out) = (Vec::new(), Vec::new());
    for harness in harnesses {
        let name = health::name(harness);
        let vendor = match settings.vendor(config, harness) {
            Ok(vendor) => vendor,
            Err(why) => {
                ruled_out.push(why);
                continue;
            }
        };
        if !probe.installed(harness) {
            ruled_out.push(format!("{name} is not installed"));
            continue;
        }
        if let Some(why) = health::unusable(probe, state, &vendor, now) {
            ruled_out.push(why);
            continue;
        }
        let reading = if settings.mode == Mode::Usage {
            let mut poll = || probe.poll_usage(harness);
            usage::reading(config, role, harness, settings, now, &mut poll)
        } else {
            Reading::Unknown("not consulted: the role is fixed to one vendor".into())
        };
        candidates.push(Candidate { vendor, reading });
    }
    decide(settings, &candidates, &ruled_out)
}

#[cfg(test)]
mod tests;
