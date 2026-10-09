//! How supervision answers failed renewals: an attempt the service says has
//! ended drains the launch at once (without a release when the agent
//! submitted it); a transient failure is retried after a short backoff.
use super::*;

/// A fake whose launch only exits when signalled and whose renewals fail
/// from minute 3 because the attempt ended in `state`.
fn ended_at_minute_3(state: &str) -> Fake {
    let mut fake = Fake::new();
    fake.exit_at = None;
    let ended = renewal::AttemptEnded {
        code: "lease_expired".into(),
        state: Some(state.into()),
    };
    fake.renew_fails = Some((minutes(3), Some(ended)));
    fake
}

#[test]
fn a_submitted_attempt_drains_the_launch_without_a_release() {
    let dir = tempfile::tempdir().unwrap();
    let mut fake = ended_at_minute_3("submitted");
    let outcome = iterate(&mut fake, &config(dir.path()));
    assert!(matches!(outcome, Outcome::Launched { .. }), "{outcome:?}");
    assert_eq!(fake.renewals, [minutes(1), minutes(2), minutes(3)]);
    assert_eq!(fake.steps[5..], ["start", "signal:term"]);
    assert!(fake.releases.is_empty(), "{:?}", fake.releases);
    assert!(fake.now <= minutes(4), "not drained within one cadence");
    assert!(LaunchRecord::load_all(&config(dir.path())).is_empty());
    assert_eq!(
        (left(dir.path(), "clones"), left(dir.path(), "runs")),
        (0, 0)
    );
}

#[test]
fn a_launch_that_exits_after_submitting_is_not_released() {
    let dir = tempfile::tempdir().unwrap();
    let mut fake = Fake::new();
    fake.attempt_state = Some("submitted".into());
    let outcome = iterate(&mut fake, &config(dir.path()));
    let launched = Outcome::Launched {
        task: "t1".into(),
        exit_code: 0,
    };
    assert_eq!(outcome, launched);
    assert!(
        !fake.steps.contains(&"release".to_owned()),
        "{:?}",
        fake.steps
    );
    assert!(fake.releases.is_empty(), "{:?}", fake.releases);
    assert!(LaunchRecord::load_all(&config(dir.path())).is_empty());
    assert_eq!(
        (left(dir.path(), "clones"), left(dir.path(), "runs")),
        (0, 0)
    );
}

#[test]
fn a_launch_that_exits_with_its_attempt_unsubmitted_is_released() {
    for state in [Some("active"), None] {
        let dir = tempfile::tempdir().unwrap();
        let mut fake = Fake::new();
        fake.attempt_state = state.map(String::from);
        iterate(&mut fake, &config(dir.path()));
        assert_eq!(fake.steps[5..], ["start", "release"], "{state:?}");
        assert!(fake.releases[0].contains("the launch exited with code 0"));
    }
}

#[test]
fn an_attempt_ended_otherwise_drains_and_releases_with_the_reason() {
    let dir = tempfile::tempdir().unwrap();
    let mut fake = ended_at_minute_3("expired");
    iterate(&mut fake, &config(dir.path()));
    assert_eq!(fake.steps[5..], ["start", "signal:term", "release"]);
    let reason = "after a drain (attempt a1: the attempt is no longer renewable (lease_expired; state expired))";
    assert!(fake.releases[0].contains(reason), "{}", fake.releases[0]);
    assert!(fake.now <= minutes(4));
}

/// A fake whose claims grant the production cadence (19 min against a 60 min
/// lease), whose harness is always active and whose renewals fail from
/// minute 19, the first renewal.
fn failing_from_the_first_cadence() -> Fake {
    let mut fake = Fake::new();
    fake.renew_after_seconds = 19 * 60;
    fake.event_at = Some(i64::MAX / 2);
    fake.renew_fails = Some((minutes(19), None));
    fake
}

#[test]
fn a_transient_renewal_failure_is_retried_after_a_short_backoff() {
    let dir = tempfile::tempdir().unwrap();
    let mut fake = Fake::new();
    fake.exit_at = Some(minutes(6) + 30_000);
    fake.renew_fails = Some((minutes(2), None));
    iterate(&mut fake, &config(dir.path()));
    let expected = [60_000, 120_000, 150_000, 210_000, 270_000, 330_000];
    assert_eq!(fake.renewals, expected);
    assert_eq!(fake.steps[5..], ["start", "release"]);
    assert!(fake.releases[0].contains("the launch exited with code 0"));
}

#[test]
fn a_lock_busy_failure_is_retried_within_the_backoff_and_then_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let mut fake = failing_from_the_first_cadence();
    fake.exit_at = Some(minutes(40));
    fake.renew_heals = Some(minutes(19) + 20_000);
    iterate(&mut fake, &config(dir.path()));
    let cadence = minutes(19);
    let expected = [cadence, cadence + 30_000, 2 * cadence + 30_000];
    assert_eq!(fake.renewals, expected);
    assert!(fake.releases[0].contains("the launch exited with code 0"));
}

#[test]
fn repeated_transport_failures_are_retried_before_the_lease_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let mut fake = failing_from_the_first_cadence();
    fake.exit_at = Some(minutes(55));
    iterate(&mut fake, &config(dir.path()));
    let seconds: Vec<i64> = fake.renewals.iter().map(|at| at / 1000).collect();
    let head = [19 * 60, 19 * 60 + 30, 20 * 60 + 30, 22 * 60 + 30];
    assert_eq!(seconds[..4], head, "{seconds:?}");
    let gaps: Vec<i64> = seconds.windows(2).map(|w| w[1] - w[0]).collect();
    assert_eq!(gaps[..5], [30, 60, 120, 240, 300], "{seconds:?}");
    assert!(gaps.iter().all(|gap| *gap <= 300), "{gaps:?}");
    let last = *fake.renewals.last().unwrap();
    assert!(minutes(55) - last <= minutes(5), "{seconds:?}");
    assert!(fake.releases[0].contains("the launch exited with code 0"));
}

#[test]
fn retries_stop_at_the_checkpoint_gate() {
    let dir = tempfile::tempdir().unwrap();
    let mut fake = failing_from_the_first_cadence();
    fake.exit_at = None;
    iterate(&mut fake, &config(dir.path()));
    let last = *fake.renewals.last().unwrap();
    assert!(last > minutes(55) && last <= minutes(60), "{last}");
    assert_eq!(fake.steps[5..], ["start", "signal:term", "release"]);
    let reason = "after a drain (no checkpoint for 61 min)";
    assert!(fake.releases[0].contains(reason), "{}", fake.releases[0]);
}

#[test]
fn retries_stop_once_the_launch_is_asked_to_stop() {
    let dir = tempfile::tempdir().unwrap();
    let mut fake = failing_from_the_first_cadence();
    fake.exit_at = None;
    fake.stop_at = Some(minutes(19) + 45_000);
    iterate(&mut fake, &config(dir.path()));
    assert_eq!(fake.renewals, [minutes(19), minutes(19) + 30_000]);
    assert_eq!(fake.steps[5..], ["start", "signal:term", "release"]);
}
