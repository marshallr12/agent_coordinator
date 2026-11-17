use super::*;
use crate::profile::Role;
use crate::run_loop::health::HealthConfig;
use crate::run_loop::usage::Window;
use anyhow::Result;
use std::collections::BTreeMap;

fn vendor(harness: Harness) -> Vendor {
    let model = if harness == Harness::Claude {
        "opus"
    } else {
        "gpt"
    };
    Vendor {
        harness,
        model: model.into(),
        effort: "high".into(),
    }
}

fn known(harness: Harness, remaining: f64) -> Candidate {
    let reading = Reading::Known {
        remaining_percent: remaining,
        window: "5h".into(),
        age_ms: 60_000,
        source: "test".into(),
    };
    Candidate {
        vendor: vendor(harness),
        reading,
    }
}

fn unknown(harness: Harness, why: &str) -> Candidate {
    Candidate {
        vendor: vendor(harness),
        reading: Reading::Unknown(why.into()),
    }
}

fn usage_mode() -> SelectionConfig {
    SelectionConfig {
        mode: Mode::Usage,
        ..SelectionConfig::default()
    }
}

/// The rule and harness `decide` picks among Claude then Codex candidates.
fn pick(settings: &SelectionConfig, claude: Candidate, codex: Candidate) -> (Harness, String) {
    let choice = decide(settings, &[claude, codex], &[]).unwrap();
    (choice.vendor.harness, choice.rule)
}

#[test]
fn the_vendor_clearly_ahead_is_chosen_even_against_the_order() {
    let settings = usage_mode();
    let (harness, rule) = pick(
        &settings,
        known(Harness::Claude, 20.0),
        known(Harness::Codex, 70.0),
    );
    assert_eq!((harness, rule.as_str()), (Harness::Codex, "more_remaining"));
    let (harness, _) = pick(
        &settings,
        known(Harness::Claude, 90.0),
        known(Harness::Codex, 30.0),
    );
    assert_eq!(harness, Harness::Claude);
}

#[test]
fn the_choice_records_the_readings_and_the_rule() {
    let choice = decide(
        &usage_mode(),
        &[known(Harness::Claude, 20.0), known(Harness::Codex, 70.0)],
        &["x".into()],
    )
    .unwrap();
    assert_eq!(choice.readings.len(), 3);
    assert!(choice.readings[0].starts_with("claude 20% left (5h window"));
    assert!(choice.readings[2].starts_with("ruled out: x"));
    assert!(choice.sentence().contains("by rule more_remaining"));
}

#[test]
fn both_below_the_minimum_still_choose_the_one_with_more_left() {
    let settings = usage_mode();
    let (harness, rule) = pick(
        &settings,
        known(Harness::Claude, 4.0),
        known(Harness::Codex, 8.0),
    );
    assert_eq!((harness, rule.as_str()), (Harness::Codex, "more_remaining"));
}

#[test]
fn a_vendor_below_the_minimum_is_passed_over_while_another_is_above_it() {
    // Claude is first in the order and has more than Codex would need to
    // tie, but Codex is the only one above the 50% minimum.
    let settings = SelectionConfig {
        min_remaining_percent: 50.0,
        ..usage_mode()
    };
    let (harness, rule) = pick(
        &settings,
        known(Harness::Claude, 45.0),
        known(Harness::Codex, 55.0),
    );
    assert_eq!(
        (harness, rule.as_str()),
        (Harness::Codex, "below_minimum_skipped")
    );
    // With the other known to be above, an unknown one is passed over too
    // only when it is known low; an unknown vendor is not excluded.
    let (harness, rule) = pick(
        &settings,
        unknown(Harness::Claude, "none"),
        known(Harness::Codex, 55.0),
    );
    assert_eq!(
        (harness, rule.as_str()),
        (Harness::Claude, "unknown_by_order")
    );
}

#[test]
fn a_tie_is_settled_by_the_configured_order() {
    let settings = usage_mode();
    let (harness, rule) = pick(
        &settings,
        known(Harness::Claude, 60.0),
        known(Harness::Codex, 60.5),
    );
    assert_eq!((harness, rule.as_str()), (Harness::Claude, "tie_by_order"));
    let reversed = [known(Harness::Codex, 60.0), known(Harness::Claude, 60.0)];
    let choice = decide(&usage_mode(), &reversed, &[]).unwrap();
    assert_eq!(choice.vendor.harness, Harness::Codex);
}

#[test]
fn one_unknown_reading_is_neither_preferred_nor_excluded() {
    let settings = usage_mode();
    let (harness, rule) = pick(
        &settings,
        known(Harness::Claude, 10.0),
        unknown(Harness::Codex, "no reading yet"),
    );
    assert_eq!(
        (harness, rule.as_str()),
        (Harness::Claude, "unknown_by_order")
    );
    let (harness, _) = pick(
        &settings,
        unknown(Harness::Claude, "none"),
        known(Harness::Codex, 99.0),
    );
    assert_eq!(
        harness,
        Harness::Claude,
        "the known 99% is not preferred over the order"
    );
}

#[test]
fn both_unknown_follow_the_order() {
    let settings = usage_mode();
    let (harness, rule) = pick(
        &settings,
        unknown(Harness::Claude, "a"),
        unknown(Harness::Codex, "b"),
    );
    assert_eq!(
        (harness, rule.as_str()),
        (Harness::Claude, "unknown_by_order")
    );
    let codex_first = SelectionConfig {
        order: vec![Harness::Codex, Harness::Claude],
        ..usage_mode()
    };
    let choice = decide(
        &codex_first,
        &[unknown(Harness::Codex, "b"), unknown(Harness::Claude, "a")],
        &[],
    );
    assert_eq!(choice.unwrap().vendor.harness, Harness::Codex);
}

#[test]
fn a_single_candidate_is_chosen_and_none_is_refused_with_every_reason() {
    let only = decide(
        &usage_mode(),
        &[known(Harness::Codex, 1.0)],
        &["claude is exhausted".into()],
    )
    .unwrap();
    assert_eq!(only.rule, "only_usable");
    let refused = decide(&usage_mode(), &[], &["a".into(), "b".into()]).unwrap_err();
    assert_eq!(refused, "no vendor is usable: a; b");
}

/// A probe for the account of `role`, with scripted answers.
struct Probe {
    role: Role,
    now: i64,
    missing: Vec<Harness>,
    signed_out: Vec<Harness>,
    expiry: Option<i64>,
    windows: BTreeMap<&'static str, Vec<Window>>,
    queries: Vec<Harness>,
}

impl Probe {
    fn new(role: Role) -> Self {
        Self {
            role,
            now: 1_000_000,
            missing: Vec::new(),
            signed_out: Vec::new(),
            expiry: None,
            windows: BTreeMap::new(),
            queries: Vec::new(),
        }
    }
}

impl health::Probe for Probe {
    fn role(&self) -> Role {
        self.role
    }

    fn now_ms(&self) -> i64 {
        self.now
    }

    fn installed(&self, harness: Harness) -> bool {
        !self.missing.contains(&harness)
    }

    fn signed_in(&mut self, harness: Harness) -> Result<()> {
        anyhow::ensure!(!self.signed_out.contains(&harness), "exit status 1");
        Ok(())
    }

    fn expiry_ms(&self, _harness: Harness) -> Option<i64> {
        self.expiry
    }

    fn poll_usage(&mut self, harness: Harness) -> Result<Option<Vec<Window>>> {
        self.queries.push(harness);
        Ok(self.windows.get(health::name(harness)).cloned())
    }
}

fn used(percent: f64) -> Vec<Window> {
    vec![Window {
        label: "5h".into(),
        used_percent: percent,
        resets_at_ms: None,
    }]
}

/// A host config rooted in `dir` that runs Claude and has a Codex fallback.
fn host(dir: &std::path::Path) -> Config {
    Config {
        state_dir: dir.to_owned(),
        health: HealthConfig {
            fallback: Some(Vendor {
                harness: Harness::Codex,
                model: "gpt-5.5".into(),
                effort: "high".into(),
            }),
            ..HealthConfig::default()
        },
        ..Config::default()
    }
}

fn run(config: &Config, probe: &mut Probe, settings: &SelectionConfig) -> Result<Choice, String> {
    choose(probe, config, settings, &VendorState::load(config))
}

#[test]
fn a_vendor_that_is_not_installed_is_never_a_candidate() {
    let dir = tempfile::tempdir().unwrap();
    let config = host(dir.path());
    let mut probe = Probe::new(Role::Implementer);
    probe.missing = vec![Harness::Claude];
    probe.windows.insert("codex", used(99.0));
    let choice = run(&config, &mut probe, &usage_mode()).unwrap();
    assert_eq!(
        (choice.vendor.harness, choice.rule.as_str()),
        (Harness::Codex, "only_usable")
    );
    assert!(
        choice
            .readings
            .iter()
            .any(|r| r == "ruled out: claude is not installed")
    );
    assert_eq!(
        probe.queries,
        [Harness::Codex],
        "an absent vendor is not queried"
    );
    probe.missing.push(Harness::Codex);
    let refused = run(&config, &mut probe, &usage_mode()).unwrap_err();
    assert!(
        refused.contains("claude is not installed") && refused.contains("codex is not installed")
    );
}

#[test]
fn an_exhausted_mark_an_expired_credential_and_a_failed_sign_in_rule_a_vendor_out() {
    let dir = tempfile::tempdir().unwrap();
    let config = host(dir.path());
    let mut probe = Probe::new(Role::Implementer);
    probe.windows.insert("claude", used(80.0));
    probe.windows.insert("codex", used(10.0));
    health::mark_exhausted(&config, Harness::Codex, probe.now + 60_000);
    let choice = run(&config, &mut probe, &usage_mode()).unwrap();
    assert_eq!(choice.vendor.harness, Harness::Claude);
    assert!(
        choice
            .readings
            .iter()
            .any(|r| r.starts_with("ruled out: codex is exhausted until"))
    );
    // The mark passed: Codex, with more left, wins.
    probe.now += 120_000;
    assert_eq!(
        run(&config, &mut probe, &usage_mode())
            .unwrap()
            .vendor
            .harness,
        Harness::Codex
    );
    // Every credential expired: nothing is usable.
    probe.expiry = Some(probe.now - 1);
    let refused = run(&config, &mut probe, &usage_mode()).unwrap_err();
    assert!(
        refused.contains("claude's credential expired")
            && refused.contains("codex's credential expired")
    );
    probe.expiry = None;
    probe.signed_out = vec![Harness::Codex];
    let choice = run(&config, &mut probe, &usage_mode()).unwrap();
    assert_eq!(choice.vendor.harness, Harness::Claude);
    assert!(
        choice
            .readings
            .iter()
            .any(|r| r.contains("codex is not signed in"))
    );
}

#[test]
fn a_role_fixed_to_one_harness_is_never_switched() {
    let dir = tempfile::tempdir().unwrap();
    let config = host(dir.path());
    for (mode, fixed, other) in [
        (Mode::Claude, Harness::Claude, Harness::Codex),
        (Mode::Codex, Harness::Codex, Harness::Claude),
    ] {
        let settings = SelectionConfig {
            mode,
            ..SelectionConfig::default()
        };
        let mut probe = Probe::new(Role::Implementer);
        probe.windows.insert(health::name(other), used(0.0));
        probe.windows.insert(health::name(fixed), used(99.0));
        let choice = run(&config, &mut probe, &settings).unwrap();
        assert_eq!(
            (choice.vendor.harness, choice.rule.as_str()),
            (fixed, "fixed")
        );
        assert!(
            probe.queries.is_empty(),
            "a fixed role asks nobody for usage"
        );
        probe.signed_out = vec![fixed];
        let refused = run(&config, &mut probe, &settings).unwrap_err();
        assert!(refused.contains("not signed in"), "{refused}");
    }
}

#[test]
fn a_vendor_with_no_configured_model_is_ruled_out() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        state_dir: dir.path().to_owned(),
        ..Config::default()
    };
    let mut probe = Probe::new(Role::Implementer);
    let choice = run(&config, &mut probe, &usage_mode()).unwrap();
    assert_eq!(choice.vendor.harness, Harness::Claude);
    assert!(
        choice
            .readings
            .iter()
            .any(|r| r == "ruled out: no model is configured for codex")
    );
    let own = SelectionConfig {
        codex: Some(Model {
            model: "gpt-x".into(),
            effort: "low".into(),
        }),
        ..usage_mode()
    };
    let vendor = own.vendor(&config, Harness::Codex).unwrap();
    assert_eq!(
        (vendor.model.as_str(), vendor.effort.as_str()),
        ("gpt-x", "low")
    );
}

#[test]
fn the_reviewer_chooses_over_its_own_account_while_the_implementer_is_set_differently() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = host(dir.path());
    config.health.selection = SelectionConfig {
        mode: Mode::Claude,
        ..SelectionConfig::default()
    };
    config.health.reviewer_selection = usage_mode();
    // The implementer's Claude account ran out; the reviewer's did not.
    health::mark_exhausted(&config, Harness::Claude, 2_000_000);
    let mut reviewer = Probe::new(Role::Reviewer);
    reviewer.windows.insert("claude", used(70.0));
    reviewer.windows.insert("codex", used(20.0));
    let settings = config.health.selection(Role::Reviewer).clone();
    let by_usage = run(&config, &mut reviewer, &settings).unwrap();
    assert_eq!(
        (by_usage.vendor.harness, by_usage.rule.as_str()),
        (Harness::Codex, "more_remaining")
    );
    reviewer.now += 11 * 60_000; // past the cache period: asked again
    reviewer.windows.insert("claude", used(5.0));
    reviewer.windows.insert("codex", used(60.0));
    let by_usage = run(&config, &mut reviewer, &settings).unwrap();
    assert_eq!(
        by_usage.vendor.harness,
        Harness::Claude,
        "the implementer's mark does not bind the reviewer"
    );
    // The implementer, fixed to Claude, is refused by its own mark.
    let mut implementer = Probe::new(Role::Implementer);
    let refused = run(
        &config,
        &mut implementer,
        config.health.selection(Role::Implementer),
    )
    .unwrap_err();
    assert!(refused.contains("claude is exhausted"), "{refused}");
    // A reviewer fixed to Codex, whatever the implementer runs.
    let fixed = SelectionConfig {
        mode: Mode::Codex,
        ..SelectionConfig::default()
    };
    let choice = run(&config, &mut Probe::new(Role::Reviewer), &fixed).unwrap();
    assert_eq!(choice.vendor.harness, Harness::Codex);
    // The reviewer's own exhausted mark rules out its Claude.
    health::mark_exhausted_for(&config, Role::Reviewer, Harness::Claude, 2_000_000);
    let choice = run(&config, &mut Probe::new(Role::Reviewer), &settings).unwrap();
    assert_eq!(choice.vendor.harness, Harness::Codex);
}

#[test]
fn selection_settings_default_to_off_with_documented_values() {
    let health: HealthConfig = toml::from_str("").unwrap();
    for settings in [&health.selection, &health.reviewer_selection] {
        assert_eq!(settings.mode, Mode::Off);
        assert_eq!(settings.order, [Harness::Claude, Harness::Codex]);
        assert_eq!(settings.min_remaining_percent, 10.0);
        assert_eq!((settings.cache_minutes, settings.stale_minutes), (10, 60));
        assert_eq!((&settings.claude, &settings.codex), (&None, &None));
    }
    let text = "[reviewer_selection]\nmode = \"usage\"\norder = [\"codex\"]\n[reviewer_selection.codex]\nmodel = \"m\"\n";
    let health: HealthConfig = toml::from_str(text).unwrap();
    assert_eq!(health.selection.mode, Mode::Off);
    let reviewer = &health.reviewer_selection;
    assert_eq!(reviewer.mode, Mode::Usage);
    assert_eq!(reviewer.ordered(), [Harness::Codex, Harness::Claude]);
    assert_eq!(reviewer.codex.as_ref().unwrap().effort, "high");
    assert!(toml::from_str::<HealthConfig>("[selection]\nmoed = \"usage\"\n").is_err());
}

#[test]
fn the_host_setup_block_shows_every_selection_setting_at_its_default() {
    let text = include_str!("../../../../../deploy/agentc/host-setup.sh");
    let start = text.find("# [health.selection]\n").unwrap();
    let end = text.find("# Host-approved project setup").unwrap();
    let block: Vec<String> = text[start..end]
        .lines()
        .filter_map(|line| line.strip_prefix("# "))
        .filter(|line| {
            !line.contains('<')
                && !line.starts_with("[health.selection.")
                && !line.starts_with("effort")
        })
        .filter(|line| line.starts_with('[') || line.contains(" = "))
        .map(|line| line.split(" #").next().unwrap().to_owned())
        .collect();
    let block = block
        .join("\n")
        .replace("[health.selection]", "[selection]");
    let block = block.replace("[health.reviewer_selection]", "[reviewer_selection]");
    let health: HealthConfig = toml::from_str(&block).unwrap();
    assert_eq!(health, HealthConfig::default());
    // Every setting is shown: five keys for each of the two tables.
    assert_eq!(block.lines().filter(|l| l.contains(" = ")).count(), 10);
}
