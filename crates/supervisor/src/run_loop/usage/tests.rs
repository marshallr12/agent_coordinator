use super::*;
use crate::profile::Role;
use serde_json::json;
use std::cell::Cell;

/// A host config rooted in `dir`.
fn config(dir: &std::path::Path) -> Config {
    Config {
        state_dir: dir.to_owned(),
        ..Config::default()
    }
}

fn window(label: &str, used: f64, resets_at_ms: Option<i64>) -> Window {
    Window {
        label: label.into(),
        used_percent: used,
        resets_at_ms,
    }
}

/// The result Codex's `account/rateLimits/read` gives (shape from
/// `codex app-server generate-json-schema`).
fn codex_result() -> Value {
    json!({
        "rateLimits": {"primary": {"usedPercent": 25, "windowDurationMins": 300, "resetsAt": 5000},
                        "secondary": {"usedPercent": 60, "windowDurationMins": 10080, "resetsAt": 9000}},
        "rateLimitsByLimitId": null
    })
}

#[test]
fn codex_windows_come_from_the_rate_limits_answer() {
    let windows = codex_windows(&codex_result());
    assert_eq!(
        windows,
        vec![
            window("5h", 25.0, Some(5_000_000)),
            window("7d", 60.0, Some(9_000_000))
        ]
    );
    assert_eq!(most_used(&windows).unwrap().label, "7d");
}

#[test]
fn codex_prefers_its_own_bucket_and_treats_a_reached_limit_as_used_up() {
    let result = json!({
        "rateLimits": {"primary": {"usedPercent": 1}},
        "rateLimitsByLimitId": {"codex": {"primary": {"usedPercent": 40, "windowDurationMins": 300},
            "rateLimitReachedType": "rate_limit_reached"}}
    });
    let windows = codex_windows(&result);
    assert_eq!(windows[0], window("5h", 40.0, None));
    assert_eq!(most_used(&windows).unwrap().used_percent, 100.0);
}

#[test]
fn only_the_answer_to_the_read_request_is_taken_from_app_server_output() {
    let notice = json!({"method": "remoteControl/status/changed", "params": {}}).to_string();
    assert!(codex_answer(&notice, 2).is_none());
    assert!(codex_answer("not json", 2).is_none());
    let init = json!({"id": 1, "result": {"userAgent": "x"}}).to_string();
    assert!(codex_answer(&init, 2).is_none());
    let line = json!({"id": 2, "result": codex_result()}).to_string();
    assert_eq!(codex_answer(&line, 2).unwrap().unwrap().len(), 2);
    let refused = json!({"id": 2, "error": {"code": -32600,
        "message": "codex account authentication required to read rate limits"}})
    .to_string();
    let error = codex_answer(&refused, 2).unwrap().unwrap_err();
    assert!(format!("{error}").contains("authentication required"));
    let empty = json!({"id": 2, "result": {"rateLimits": {}}}).to_string();
    assert!(codex_answer(&empty, 2).unwrap().is_err());
}

#[test]
fn claude_windows_come_from_the_last_rate_limit_event_with_a_figure() {
    let events = [
        json!({"type": "rate_limit_event", "rate_limit_info": {"status": "allowed",
            "unifiedWindows": {"five_hour": {"utilization": 0.2, "resetsAt": 100},
                               "seven_day": {"utilization": 0.5, "resetsAt": 900}}}}),
        json!({"type": "assistant"}),
        json!({"type": "rate_limit_event", "rate_limit_info": {"status": "allowed"}}),
    ];
    let text: Vec<String> = events.iter().map(Value::to_string).collect();
    let windows = claude_windows(text.join("\n").as_bytes());
    let used: Vec<_> = windows
        .iter()
        .map(|w| (w.label.as_str(), w.used_percent))
        .collect();
    assert_eq!(used, [("5h", 20.0), ("7d", 50.0)]);
    assert_eq!(windows[0].resets_at_ms, Some(100_000));
    let top = json!({"type": "rate_limit_event", "rate_limit_info": {"status": "allowed_warning",
        "rateLimitType": "five_hour", "utilization": 0.9, "resetsAt": 100}});
    let windows = claude_windows(top.to_string().as_bytes());
    assert_eq!(windows, vec![window("5h", 90.0, Some(100_000))]);
    assert!(claude_windows(b"{\"type\":\"result\"}\nnot json").is_empty());
}

/// A settings value with a ten-minute cache and an hour of freshness.
fn settings() -> SelectionConfig {
    SelectionConfig::default()
}

const MIN: i64 = 60_000;

#[test]
fn a_poll_within_the_cache_period_makes_no_vendor_query() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let queries = Cell::new(0);
    let mut ask = || {
        queries.set(queries.get() + 1);
        Ok(Some(vec![window("5h", 30.0, None)]))
    };
    let first = reading(
        &config,
        Role::Implementer,
        Harness::Codex,
        &settings(),
        100 * MIN,
        &mut ask,
    );
    assert_eq!(first.remaining(), Some(70.0));
    let again = reading(
        &config,
        Role::Implementer,
        Harness::Codex,
        &settings(),
        105 * MIN,
        &mut ask,
    );
    assert_eq!(
        queries.get(),
        1,
        "a poll inside the cache period asked again"
    );
    assert!(matches!(&again, Reading::Known { age_ms, .. } if *age_ms == 5 * MIN));
    let later = reading(
        &config,
        Role::Implementer,
        Harness::Codex,
        &settings(),
        110 * MIN,
        &mut ask,
    );
    assert_eq!(queries.get(), 2);
    assert_eq!(later.remaining(), Some(70.0));
}

#[test]
fn a_failed_query_is_not_repeated_within_the_cache_period_and_reads_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let queries = Cell::new(0);
    let mut ask = || {
        queries.set(queries.get() + 1);
        anyhow::bail!("codex app-server gave no usage answer in time")
    };
    for at in [100, 101, 105] {
        let got = reading(
            &config,
            Role::Implementer,
            Harness::Codex,
            &settings(),
            at * MIN,
            &mut ask,
        );
        assert!(matches!(got, Reading::Unknown(why) if why == "no reading yet"));
    }
    assert_eq!(queries.get(), 1);
}

#[test]
fn a_stale_reading_counts_as_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let mut ask = || Ok(Some(vec![window("5h", 30.0, None)]));
    reading(
        &config,
        Role::Implementer,
        Harness::Codex,
        &settings(),
        100 * MIN,
        &mut ask,
    );
    let mut failing = || anyhow::bail!("offline");
    let old = reading(
        &config,
        Role::Implementer,
        Harness::Codex,
        &settings(),
        161 * MIN,
        &mut failing,
    );
    assert!(
        matches!(&old, Reading::Unknown(why) if why.contains("stale: read 61 min ago")),
        "{old:?}"
    );
}

#[test]
fn a_reading_whose_window_has_reset_counts_as_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let reset = 120 * MIN;
    let mut ask = || Ok(Some(vec![window("5h", 95.0, Some(reset))]));
    let before = reading(
        &config,
        Role::Implementer,
        Harness::Codex,
        &settings(),
        100 * MIN,
        &mut ask,
    );
    assert_eq!(before.remaining(), Some(5.0));
    let mut none = || Ok(None);
    let after = reading(
        &config,
        Role::Implementer,
        Harness::Codex,
        &settings(),
        121 * MIN,
        &mut none,
    );
    assert!(
        matches!(&after, Reading::Unknown(why) if why.contains("has reset")),
        "{after:?}"
    );
}

#[test]
fn claude_has_no_query_and_reads_only_what_its_launch_events_showed() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let queries = Cell::new(0);
    let mut none = || {
        queries.set(queries.get() + 1);
        Ok(None)
    };
    let nothing = reading(
        &config,
        Role::Implementer,
        Harness::Claude,
        &settings(),
        10 * MIN,
        &mut none,
    );
    assert!(matches!(nothing, Reading::Unknown(_)));
    let event = json!({"type": "rate_limit_event", "rate_limit_info": {"status": "allowed",
        "unifiedWindows": {"five_hour": {"utilization": 0.75, "resetsAt": 100_000}}}});
    record_events(
        &config,
        Role::Implementer,
        Harness::Claude,
        event.to_string().as_bytes(),
        20 * MIN,
    );
    let got = reading(
        &config,
        Role::Implementer,
        Harness::Claude,
        &settings(),
        30 * MIN,
        &mut none,
    );
    assert_eq!(got.remaining(), Some(25.0));
    assert!(got.describe(Harness::Claude).contains("rate_limit_event"));
    // Another role's account has its own reading.
    let reviewer = reading(
        &config,
        Role::Reviewer,
        Harness::Claude,
        &settings(),
        30 * MIN,
        &mut none,
    );
    assert!(matches!(reviewer, Reading::Unknown(_)));
    // Codex events carry no figure and are never recorded.
    record_events(
        &config,
        Role::Implementer,
        Harness::Codex,
        event.to_string().as_bytes(),
        20 * MIN,
    );
    let codex = reading(
        &config,
        Role::Implementer,
        Harness::Codex,
        &settings(),
        30 * MIN,
        &mut none,
    );
    assert!(matches!(codex, Reading::Unknown(_)));
}
