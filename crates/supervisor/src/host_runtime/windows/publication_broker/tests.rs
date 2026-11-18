//! The publication handler's own decisions. What a launch inside an
//! AppContainer meets on its pipe is shown by the native suite
//! (`tests/windows_publication.rs`), since no token here carries a package.
use super::*;
use std::sync::atomic::{AtomicU32, Ordering};

/// A launch's account and package in these tests.
const ACCOUNT: &str = "S-1-5-21-1-2-3-1001";
const PACKAGE: &str = "S-1-15-2-1-2-3-4-5-6-7";

/// A gatekeeper that gives one verdict and counts the questions.
struct Fixed {
    verdict: Verdict,
    asked: AtomicU32,
}

impl Gatekeeper for Fixed {
    fn ask(&self, launch: &str, task: &str) -> Verdict {
        assert_eq!((launch, task), ("l", "t"), "asked about the grant's launch");
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.verdict.clone()
    }
}

/// A grant of launch `l` working as `role`, with `task` when given.
fn grant(role: Role, task: Option<&str>) -> Grant {
    let grant = Grant::new("l", role, ACCOUNT, PACKAGE, 1).unwrap();
    match task {
        Some(task) => grant.with_task(task).unwrap(),
        None => grant,
    }
}

/// A broker whose gatekeeper always answers `verdict`.
fn broker(verdict: Verdict) -> (Publication, Arc<Fixed>) {
    let fixed = Arc::new(Fixed {
        verdict,
        asked: AtomicU32::new(0),
    });
    let root = std::env::temp_dir().join("agentc-publication-unit");
    let publication = Publication::new("unused-remote", root, fixed.clone());
    (publication, fixed)
}

#[test]
fn only_an_implementer_launch_with_a_task_may_publish() {
    let implementer = grant(Role::Implementer, Some("t"));
    assert_eq!(Publication::permitted(&implementer).unwrap(), "t");
    for (refused, why) in [
        (grant(Role::Reviewer, Some("t")), "reviewer"),
        (grant(Role::Reviewer, None), "reviewer"),
        (grant(Role::Implementer, None), "no task"),
    ] {
        let refusal = Publication::permitted(&refused).unwrap_err();
        assert_eq!(refusal.code, RefusalCode::NotAuthorized);
        assert!(refusal.message.contains(why), "{}", refusal.message);
    }
}

#[test]
fn the_gate_passes_only_a_current_launch_and_tells_it_no_reason() {
    let (current, asked) = broker(Verdict::Current);
    assert_eq!(current.gate("l", "t"), Ok(()));
    let (lost, _) = broker(Verdict::Refused(r"pipe \\.\pipe\x is gone".into()));
    let refusal = lost.gate("l", "t").unwrap_err();
    assert_eq!(refusal.code, RefusalCode::NotAuthorized);
    assert!(!refusal.message.contains("pipe"), "{}", refusal.message);
    assert_eq!(asked.asked.load(Ordering::SeqCst), 1);
}

#[test]
fn a_launch_has_one_push_at_a_time_and_gets_its_turn_back() {
    let busy = Mutex::default();
    let first = Turn::take(&busy, "l").expect("a free launch");
    assert!(Turn::take(&busy, "l").is_none(), "a second push at once");
    let other = Turn::take(&busy, "m");
    assert!(other.is_some(), "another launch waits for nobody");
    drop(first);
    assert!(Turn::take(&busy, "l").is_some(), "the turn came back");
}

#[test]
fn a_launch_is_published_under_its_own_ref_and_repository() {
    let (publication, _) = broker(Verdict::Current);
    let spec = publication.spec(&grant(Role::Implementer, Some("t")), "t");
    let spec = spec.unwrap();
    assert_eq!(spec.reference(), "refs/agent-coordinator/candidates/t/l");
    let reserved = publication.spec(&grant(Role::Implementer, Some("wip")), "wip");
    assert!(reserved.is_err(), "the WIP namespace is not a task");
}

#[test]
fn a_launch_s_credential_digests_reach_its_secret_scan() {
    let (publication, _) = broker(Verdict::Current);
    let plain = publication
        .spec(&grant(Role::Implementer, Some("t")), "t")
        .unwrap();
    assert!(
        format!("{plain:?}").contains("known_digests: 0"),
        "{plain:?}"
    );
    let digests = vec!["ab".repeat(32), "cd".repeat(32)];
    let given = grant(Role::Implementer, Some("t")).with_known_digests(digests);
    let spec = publication.spec(&given.unwrap(), "t").unwrap();
    assert!(format!("{spec:?}").contains("known_digests: 2"), "{spec:?}");
}
