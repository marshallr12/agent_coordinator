//! Admission, routing and cost tests for the live loop, on the shared fake.
use super::*;
use crate::run_loop::health::{HealthConfig, Vendor};

/// A host config rooted in `dir` that falls back to Codex.
fn with_fallback(dir: &Path) -> Config {
    let fallback = Vendor {
        harness: Harness::Codex,
        model: "gpt-5.5".into(),
        effort: "high".into(),
    };
    Config {
        health: HealthConfig {
            fallback: Some(fallback),
            ..HealthConfig::default()
        },
        ..config(dir)
    }
}

/// Every ledger entry the loop recorded under `dir`.
fn ledger(config: &Config) -> Vec<Value> {
    let text = fs::read_to_string(cost::ledger(config)).unwrap_or_default();
    text.lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// A Claude `rate_limit_event` rejecting the session until `resets` (s).
fn rejected(resets: i64) -> String {
    json!({"type": "rate_limit_event",
           "rate_limit_info": {"status": "rejected", "resetsAt": resets}})
    .to_string()
}

#[test]
fn the_kill_switch_stops_new_claims_within_one_poll() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let mut fake = Fake::new();
    fs::write(health::kill_switch(&config), "").unwrap();
    let outcome = iterate(&mut fake, &config);
    assert!(
        matches!(&outcome, Outcome::Refused(r) if r.contains("kill switch")),
        "{outcome:?}"
    );
    assert!(fake.steps.is_empty(), "{:?}", fake.steps);
    fs::remove_file(health::kill_switch(&config)).unwrap();
    assert!(matches!(
        iterate(&mut fake, &config),
        Outcome::Launched { .. }
    ));
}

#[test]
fn a_configured_kill_switch_path_is_honoured() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(dir.path());
    let switch = dir.path().join("elsewhere");
    config.health.kill_switch = Some(switch.clone());
    fs::write(&switch, "").unwrap();
    let outcome = iterate(&mut Fake::new(), &config);
    assert!(matches!(outcome, Outcome::Refused(r) if r.contains("elsewhere")));
}

#[test]
fn a_429_routes_to_the_other_vendor_until_exhausted_until() {
    let dir = tempfile::tempdir().unwrap();
    let config = with_fallback(dir.path());
    let mut fake = Fake::new();
    fake.events = rejected(3_600);
    iterate(&mut fake, &config);
    fake.events.clear();
    iterate(&mut fake, &config);
    fake.now = 3_600_000;
    iterate(&mut fake, &config);
    let harnesses: Vec<_> = ledger(&config)
        .iter()
        .map(|e| e["harness"].clone())
        .collect();
    assert_eq!(harnesses, ["claude", "codex", "claude"]);
    assert_eq!(ledger(&config)[1]["model"], "gpt-5.5");
}

#[test]
fn a_429_without_a_fallback_refuses_until_the_reset() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let mut fake = Fake::new();
    fake.events = json!({"type": "result", "is_error": true,
        "result": "Claude AI usage limit reached|7200"})
    .to_string();
    iterate(&mut fake, &config);
    let outcome = iterate(&mut fake, &config);
    assert!(
        matches!(&outcome, Outcome::Refused(r) if r.contains("claude is exhausted")),
        "{outcome:?}"
    );
    fake.now = 7_200_000;
    assert!(matches!(
        iterate(&mut fake, &config),
        Outcome::Launched { .. }
    ));
}

#[test]
fn a_vendor_that_is_not_signed_in_is_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let mut fake = Fake::new();
    fake.unhealthy = vec![Harness::Claude];
    let outcome = iterate(&mut fake, &config(dir.path()));
    assert!(
        matches!(&outcome, Outcome::Refused(r) if r.contains("not signed in")),
        "{outcome:?}"
    );
    assert!(fake.steps.is_empty());
    let config = with_fallback(dir.path());
    assert!(matches!(
        iterate(&mut fake, &config),
        Outcome::Launched { .. }
    ));
    assert_eq!(ledger(&config)[0]["harness"], "codex");
}

#[test]
fn an_expired_credential_refuses_claims() {
    let dir = tempfile::tempdir().unwrap();
    let mut fake = Fake::new();
    fake.now = 1_000;
    fake.expiry = Some(500);
    let outcome = iterate(&mut fake, &config(dir.path()));
    assert!(matches!(outcome, Outcome::Refused(r) if r.contains("credential expired")));
}

#[test]
fn every_finished_launch_records_its_cost_against_the_task() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let mut fake = Fake::new();
    fake.events = json!({"type": "result", "total_cost_usd": 1.234,
        "usage": {"input_tokens": 10, "cache_creation_input_tokens": 5,
                  "cache_read_input_tokens": 900, "output_tokens": 70}})
    .to_string();
    iterate(&mut fake, &config);
    let entry = &ledger(&config)[0];
    assert_eq!(
        (&entry["task"], &entry["attempt"]),
        (&json!("t1"), &json!("a1"))
    );
    assert_eq!(
        (&entry["usd"], &entry["input_tokens"]),
        (&json!(1.234), &json!(15))
    );
    assert!(fake.releases[0].contains("Cost: $1.23 (15 input, 900 cached, 70 output tokens)."));
    fake.fail_start = true;
    iterate(&mut fake, &config);
    assert_eq!(
        ledger(&config).len(),
        2,
        "a launch that failed to start still records a cost"
    );
    assert!(fake.releases[1].contains("Cost: unpriced"));
}

#[test]
fn a_ledger_row_of_the_canary_project_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(dir.path());
    config.run.canary_binding = Some(super::binding::CanaryBinding {
        service_url: "https://agents.example.com".into(),
        project_id: "canary".into(),
        project_name: None,
        mirror: None,
    });
    let mut fake = Fake::with_canary();
    iterate(&mut fake, &config);
    fake.nexts = vec![("canary", json!({"action": null}))];
    iterate(&mut fake, &config);
    let rows = ledger(&config);
    assert_eq!(
        (&rows[0]["project"], &rows[0]["task"], &rows[0]["canary"]),
        (&json!("canary"), &json!("c1"), &json!(true))
    );
    assert_eq!(
        (&rows[1]["project"], &rows[1].get("canary")),
        (&json!("p1"), &None)
    );
}

#[test]
fn a_killed_launch_without_a_result_is_estimated_and_counts_toward_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(dir.path());
    config.health.implementer_daily_usd = 2.0;
    config.run.harness = Harness::Claude;
    config.run.model = "claude-opus-5-5".into();
    let mut fake = Fake::new();
    let usage = json!({"input_tokens": 10, "cache_creation_input_tokens": 5,
        "cache_read_input_tokens": 900, "output_tokens": 1_000_000});
    fake.events = json!({"type": "assistant", "message": {"id": "m1", "usage": usage}}).to_string();
    iterate(&mut fake, &config);
    let entry = &ledger(&config)[0];
    assert_eq!(
        (&entry["input_tokens"], &entry["output_tokens"]),
        (&json!(15), &json!(1_000_000))
    );
    assert_eq!(entry["usd_estimated"], true);
    assert_eq!(entry["usd"], 20.0);
    assert!(
        fake.releases[0].contains("estimated)."),
        "{:?}",
        fake.releases
    );
    let outcome = iterate(&mut fake, &config);
    assert!(
        matches!(&outcome, Outcome::Refused(r) if r.contains("cap")),
        "{outcome:?}"
    );
}

#[test]
fn the_daily_cap_refuses_claims_once_spent() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(dir.path());
    config.health.implementer_daily_usd = 2.0;
    let mut fake = Fake::new();
    fake.events = json!({"type": "result", "total_cost_usd": 2.5}).to_string();
    iterate(&mut fake, &config);
    let outcome = iterate(&mut fake, &config);
    assert!(
        matches!(&outcome, Outcome::Refused(r) if r.contains("cap")),
        "{outcome:?}"
    );
    fake.now += health::DAY_MS + 1;
    assert!(matches!(
        iterate(&mut fake, &config),
        Outcome::Launched { .. }
    ));
}

#[test]
fn an_empty_health_section_equals_defaults() {
    let health: HealthConfig = toml::from_str("").unwrap();
    assert_eq!(health, HealthConfig::default());
    assert_eq!(health.kill_switch, None);
    assert_eq!(health.fallback, None);
}

#[test]
fn a_recovered_launch_records_its_cost_and_429_once_under_its_own_vendor() {
    let dir = tempfile::tempdir().unwrap();
    let config = with_fallback(dir.path());
    let mut launch = planned(dir.path());
    launch.vendor = config.health.fallback.clone().unwrap();
    fs::create_dir_all(&launch.run).unwrap();
    let events = [
        json!({"type": "turn.completed", "usage": {"input_tokens": 9, "output_tokens": 4}}),
        json!({"type": "turn.failed", "error": {"message": "stream error: Too Many Requests"}}),
    ];
    let lines: Vec<String> = events.iter().map(Value::to_string).collect();
    fs::write(launch.run.join("events.jsonl"), lines.join("\n")).unwrap();
    let mut record = LaunchRecord::new(&launch, &Fake::lease(), &on_boot("boot-0"), 0);
    (record.pid, record.start_ticks) = (Some(4242), Some(77));
    record.save(&config).unwrap();
    let mut fake = Fake::new();
    fake.fail_release = true;
    record::recover(&mut fake, &config);
    fake.fail_release = false;
    record::recover(&mut fake, &config);
    let entries = ledger(&config);
    assert_eq!(
        entries.len(),
        1,
        "the retried release counted the cost twice"
    );
    assert_eq!(
        (&entries[0]["harness"], &entries[0]["model"]),
        (&json!("codex"), &json!("gpt-5.5"))
    );
    assert_eq!(entries[0]["output_tokens"], 4);
    let vendors = fs::read_to_string(dir.path().join("vendors.json")).unwrap();
    assert!(vendors.contains("\"codex\""), "{vendors}");
    assert!(LaunchRecord::load_all(&config).is_empty());
}

#[test]
fn a_codex_fallback_is_skipped_before_claiming_for_a_project_with_setup() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = with_fallback(dir.path());
    let setup = crate::setup::ProjectSetup {
        command: vec!["cargo".into(), "fetch".into()],
        ..Default::default()
    };
    config.setup.insert("p1".into(), setup);
    let mut fake = Fake::new();
    fake.unhealthy = vec![Harness::Claude];
    let outcome = iterate(&mut fake, &config);
    let expected = "codex cannot run project p1's setup command";
    assert!(
        matches!(&outcome, Outcome::Refused(r) if r.contains(expected)),
        "{outcome:?}"
    );
    assert!(fake.steps.is_empty(), "{:?}", fake.steps);
}

#[test]
fn an_absent_kill_switch_below_missing_directories_is_not_set() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(dir.path());
    config.health.kill_switch = Some(dir.path().join("not-made/yet/switch"));
    assert!(matches!(
        iterate(&mut Fake::new(), &config),
        Outcome::Launched { .. }
    ));
}

// Linux reports a path below a plain file as "not a directory"; Windows
// reports it as not found, like an absent switch, and the ancestor check
// tells the two apart.
#[test]
fn a_kill_switch_that_cannot_be_checked_counts_as_set() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(dir.path());
    let file = dir.path().join("plain-file");
    fs::write(&file, "").unwrap();
    config.health.kill_switch = Some(file.join("switch"));
    let mut fake = Fake::new();
    let outcome = iterate(&mut fake, &config);
    assert!(
        matches!(&outcome, Outcome::Refused(r) if r.contains("cannot be checked")),
        "{outcome:?}"
    );
    assert!(fake.steps.is_empty());
}

/// A host with a Codex fallback model and `mode` selection for implementers.
fn selecting(dir: &Path, mode: select::Mode) -> Config {
    let mut config = with_fallback(dir);
    config.health.selection.mode = mode;
    config
}

/// `percent` of a five-hour window used.
fn used(percent: f64) -> Vec<usage::Window> {
    vec![usage::Window {
        label: "5h".into(),
        used_percent: percent,
        resets_at_ms: None,
    }]
}

#[test]
fn with_selection_off_the_vendor_and_the_records_are_as_before() {
    let dir = tempfile::tempdir().unwrap();
    let config = with_fallback(dir.path());
    assert_eq!(config.health.selection.mode, select::Mode::Off);
    let mut fake = Fake::new();
    fake.usage = vec![(Harness::Claude, used(99.0)), (Harness::Codex, used(0.0))];
    fake.missing = vec![Harness::Codex];
    let (outcome, skipped) = iterate_reporting(&mut fake, &config);
    assert!(matches!(outcome, Outcome::Launched { .. }));
    assert!(fake.polls.is_empty(), "no vendor is asked for usage");
    let entry = &ledger(&config)[0];
    assert_eq!(entry["harness"], "claude");
    assert!(entry.get("selection").is_none());
    assert_eq!(skipped.choice, None);
    let path = dir.path().join("heartbeat.json");
    beat_with(&path, 1, &outcome, &skipped).unwrap();
    let beat: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert!(beat.get("vendor").is_none());
    assert!(!dir.path().join("usage.json").exists());
}

#[test]
fn with_selection_on_the_vendor_with_more_left_runs_and_the_records_say_why() {
    let dir = tempfile::tempdir().unwrap();
    let config = selecting(dir.path(), select::Mode::Usage);
    let mut fake = Fake::new();
    fake.now = 1_000_000;
    fake.usage = vec![(Harness::Claude, used(80.0)), (Harness::Codex, used(10.0))];
    let (outcome, skipped) = iterate_reporting(&mut fake, &config);
    assert!(matches!(outcome, Outcome::Launched { .. }), "{outcome:?}");
    let entry = &ledger(&config)[0];
    assert_eq!(
        (&entry["harness"], &entry["model"]),
        (&json!("codex"), &json!("gpt-5.5"))
    );
    assert_eq!(entry["selection"]["rule"], "more_remaining");
    let readings = entry["selection"]["readings"].to_string();
    assert!(
        readings.contains("claude 20% left") && readings.contains("codex 90% left"),
        "{readings}"
    );
    // The poll's heartbeat names the vendor and why.
    let choice = skipped.choice.clone().unwrap();
    assert_eq!(choice.vendor.harness, Harness::Codex);
    let path = dir.path().join("heartbeat.json");
    beat_with(&path, 1, &outcome, &skipped).unwrap();
    let beat: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(beat["vendor"]["harness"], "codex");
    assert_eq!(beat["vendor"]["rule"], "more_remaining");
    // So does the heartbeat while the launch runs.
    beat_busy_with(&path, "implementer", "t1", Some(&choice)).unwrap();
    let busy: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(busy["launch"]["vendor"]["model"], "gpt-5.5");
    assert!(busy["launch"]["vendor"]["readings"].is_array());
}

#[test]
fn a_poll_within_the_cache_period_asks_no_vendor_again() {
    let dir = tempfile::tempdir().unwrap();
    let config = selecting(dir.path(), select::Mode::Usage);
    let mut fake = Fake::new();
    fake.now = 1_000_000;
    fake.usage = vec![(Harness::Codex, used(10.0))];
    iterate(&mut fake, &config);
    iterate(&mut fake, &config);
    assert_eq!(
        fake.polls,
        [Harness::Claude, Harness::Codex],
        "one query per vendor"
    );
    assert_eq!(ledger(&config).len(), 2);
    fake.now += 11 * 60_000;
    iterate(&mut fake, &config);
    assert_eq!(
        fake.polls.len(),
        4,
        "asked again once the cache period passed"
    );
}

#[test]
fn unknown_readings_follow_the_order_and_a_429_still_moves_the_work() {
    let dir = tempfile::tempdir().unwrap();
    let config = selecting(dir.path(), select::Mode::Usage);
    let mut fake = Fake::new();
    fake.events = rejected(7_200);
    iterate(&mut fake, &config);
    fake.events.clear();
    iterate(&mut fake, &config);
    let rows = ledger(&config);
    assert_eq!(
        rows[0]["harness"], "claude",
        "no figures: the order decides"
    );
    assert_eq!(rows[0]["selection"]["rule"], "unknown_by_order");
    assert_eq!(rows[1]["harness"], "codex", "the 429 mark rules Claude out");
    assert_eq!(rows[1]["selection"]["rule"], "only_usable");
}

#[test]
fn a_role_fixed_to_one_harness_is_refused_rather_than_switched() {
    let dir = tempfile::tempdir().unwrap();
    let config = selecting(dir.path(), select::Mode::Claude);
    let mut fake = Fake::new();
    fake.unhealthy = vec![Harness::Claude];
    let outcome = iterate(&mut fake, &config);
    assert!(
        matches!(&outcome, Outcome::Refused(r) if r.contains("claude is not signed in")),
        "{outcome:?}"
    );
    assert!(fake.steps.is_empty());
    fake.unhealthy.clear();
    iterate(&mut fake, &config);
    assert_eq!(ledger(&config)[0]["selection"]["rule"], "fixed");
}

#[test]
fn a_vendor_that_is_not_installed_is_not_a_candidate_before_claiming() {
    let dir = tempfile::tempdir().unwrap();
    let config = selecting(dir.path(), select::Mode::Usage);
    let mut fake = Fake::new();
    fake.missing = vec![Harness::Claude, Harness::Codex];
    let outcome = iterate(&mut fake, &config);
    assert!(
        matches!(&outcome, Outcome::Refused(r) if r.contains("claude is not installed")),
        "{outcome:?}"
    );
    assert!(fake.steps.is_empty());
}

#[test]
fn the_launch_record_keeps_the_choice_for_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let config = selecting(dir.path(), select::Mode::Usage);
    let mut launch = planned(dir.path());
    let choice = select::Choice {
        vendor: config.health.fallback.clone().unwrap(),
        rule: "tie_by_order".into(),
        readings: vec!["codex 50% left".into()],
    };
    launch.vendor = choice.vendor.clone();
    launch.choice = Some(choice.clone());
    let record = LaunchRecord::new(&launch, &Fake::lease(), &on_boot("boot-0"), 0);
    record.save(&config).unwrap();
    let loaded = LaunchRecord::load(&config, &launch.session_id).unwrap();
    assert_eq!(loaded.launch(&config, "p1").choice, Some(choice));
    let off = Launch::plan(&config, "p1", launch.suggestion.clone());
    let record = LaunchRecord::new(&off, &Fake::lease(), &on_boot("boot-0"), 0);
    assert!(!serde_json::to_string(&record).unwrap().contains("choice"));
}
