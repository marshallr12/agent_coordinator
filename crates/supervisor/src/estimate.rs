//! Per-launch cost estimates for shadow mode (autonomy plan P3a).
//!
//! Numbers are deliberately rough: a token profile per role (how much input a
//! launch reads, how much of it is a cache hit, how much it writes) times a
//! per-model price. The costed pilot (P3b) replaces the default profiles with
//! measured ones; until then every figure is an API-equivalent estimate, even
//! on subscription billing.
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// What one launch of a role is expected to run and consume.
#[derive(Debug, Clone, PartialEq)]
pub struct LaunchEstimate {
    /// Harness that would run (`claude` or `codex`).
    pub harness: String,
    pub model: String,
    pub effort: String,
    /// Total input tokens over the whole launch (all turns).
    pub input_tokens: u64,
    /// Fraction of `input_tokens` served from the prompt cache (0.0–1.0).
    pub cached_share: f64,
    pub output_tokens: u64,
}

impl LaunchEstimate {
    /// Default implementer profile: a short single-task session.
    pub fn implementer() -> Self {
        Self::profile(4_000_000, 80_000)
    }

    /// Default reviewer profile: read the diff and evidence, write a verdict.
    pub fn reviewer() -> Self {
        Self::profile(1_200_000, 20_000)
    }

    /// A Claude launch with the given token volume and a 90% cache hit rate.
    fn profile(input_tokens: u64, output_tokens: u64) -> Self {
        Self {
            harness: "claude".into(),
            model: "claude-opus-5-5".into(),
            effort: "high".into(),
            input_tokens,
            cached_share: 0.9,
            output_tokens,
        }
    }
}

/// Configured changes to a role's profile; unset keys keep that role's default.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct LaunchOverride {
    pub harness: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub input_tokens: Option<u64>,
    pub cached_share: Option<f64>,
    pub output_tokens: Option<u64>,
}

impl LaunchOverride {
    /// `base` with every configured key replaced.
    pub fn apply(&self, base: LaunchEstimate) -> LaunchEstimate {
        LaunchEstimate {
            harness: self.harness.clone().unwrap_or(base.harness),
            model: self.model.clone().unwrap_or(base.model),
            effort: self.effort.clone().unwrap_or(base.effort),
            input_tokens: self.input_tokens.unwrap_or(base.input_tokens),
            cached_share: self.cached_share.unwrap_or(base.cached_share),
            output_tokens: self.output_tokens.unwrap_or(base.output_tokens),
        }
    }
}

/// USD per million tokens.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Price {
    pub input: f64,
    pub cached_input: f64,
    pub output: f64,
}

/// Anthropic first-party list prices (claude-api reference cached
/// 2026-06-24). Codex models have no default: their cost stays `null`
/// until the host owner configures a price.
pub fn default_prices() -> BTreeMap<String, Price> {
    let price = |input, cached_input, output| Price {
        input,
        cached_input,
        output,
    };
    BTreeMap::from([
        ("claude-opus-5-5".into(), price(4.0, 0.20, 20.0)),
        ("claude-sonnet-5".into(), price(2.0, 0.20, 10.0)),
        ("claude-haiku-4-5".into(), price(1.0, 0.10, 5.0)),
    ])
}

/// The estimate record for one launch: token split plus USD when priced.
pub fn estimate(launch: &LaunchEstimate, prices: &BTreeMap<String, Price>) -> Value {
    let share = launch.cached_share.clamp(0.0, 1.0);
    let cached = (launch.input_tokens as f64 * share).round() as u64;
    let fresh = launch.input_tokens - cached;
    let usd = prices.get(&launch.model).map(|p| {
        let dollars = (fresh as f64 * p.input
            + cached as f64 * p.cached_input
            + launch.output_tokens as f64 * p.output)
            / 1_000_000.0;
        cents(dollars)
    });
    json!({
        "input_tokens": fresh,
        "cached_input_tokens": cached,
        "output_tokens": launch.output_tokens,
        "usd": usd,
    })
}

/// Rounds dollars to whole cents.
pub fn cents(dollars: f64) -> f64 {
    (dollars * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_implementer_costs_a_few_dollars() {
        let value = estimate(&LaunchEstimate::implementer(), &default_prices());
        assert_eq!(value["cached_input_tokens"], 3_600_000);
        assert_eq!(value["usd"], 3.92);
    }

    #[test]
    fn unpriced_model_has_no_dollar_figure() {
        let launch = LaunchEstimate {
            model: "gpt-unknown".into(),
            ..LaunchEstimate::reviewer()
        };
        assert!(estimate(&launch, &default_prices())["usd"].is_null());
    }

    #[test]
    fn partial_override_keeps_the_role_defaults() {
        let change: LaunchOverride = toml::from_str("model = 'claude-sonnet-5'").unwrap();
        let reviewer = change.apply(LaunchEstimate::reviewer());
        assert_eq!(reviewer.model, "claude-sonnet-5");
        assert_eq!(reviewer.input_tokens, 1_200_000);
    }
}
