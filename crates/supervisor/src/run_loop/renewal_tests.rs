//! How supervision answers failed renewals: an attempt the service says has
//! ended drains the launch at once (without a release when the agent
//! submitted it); a transient failure is retried at the next cadence.
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

#[test]
fn a_transient_renewal_failure_is_retried_every_cadence() {
    let dir = tempfile::tempdir().unwrap();
    let mut fake = Fake::new();
    fake.exit_at = Some(minutes(6) + 30_000);
    fake.renew_fails = Some((minutes(2), None));
    iterate(&mut fake, &config(dir.path()));
    let expected: Vec<i64> = (1..=6).map(minutes).collect();
    assert_eq!(fake.renewals, expected);
    assert_eq!(fake.steps[5..], ["start", "release"]);
    assert!(fake.releases[0].contains("the launch exited with code 0"));
}
