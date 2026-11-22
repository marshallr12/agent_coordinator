//! The publication handler's own decisions. What a launch inside an
//! AppContainer meets on its pipe is shown by the native suite
//! (`tests/windows_publication.rs`), since no token here carries a package.
use super::super::publication_credentials::Credentials;
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

/// Credentials that count mints and revocations, and can be broken.
#[derive(Default)]
struct Counted {
    minted: AtomicU32,
    revoked: AtomicU32,
    broken: std::sync::atomic::AtomicBool,
}

impl Credentials for Arc<Counted> {
    /// `token-<n>`, or an error while broken.
    fn mint(&self) -> Result<String> {
        anyhow::ensure!(!self.broken.load(Ordering::SeqCst), "GitHub is down");
        Ok(format!(
            "token-{}",
            self.minted.fetch_add(1, Ordering::SeqCst)
        ))
    }

    /// Counts the revocation.
    fn revoke(&self, _token: &str) -> Result<()> {
        self.revoked.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// A broker answering `verdict` that pushes with counted credentials.
fn pushing(verdict: Verdict) -> (Publication, Arc<Counted>) {
    let counted = Arc::new(Counted::default());
    let askpass = std::path::Path::new(r"C:\agentc\agentc-supervisor.exe");
    let push = PushAuthority::new(Box::new(counted.clone()), askpass).unwrap();
    let (publication, _) = broker(verdict);
    (publication.with_push(Arc::new(push)), counted)
}

#[test]
fn no_token_is_minted_for_a_launch_without_authority() {
    let (publication, counted) = pushing(Verdict::Refused("released".into()));
    let refused = publication.token_for("l", "t").err().unwrap();
    assert_eq!(refused.code, RefusalCode::NotAuthorized);
    assert_eq!(counted.minted.load(Ordering::SeqCst), 0);
}

#[test]
fn a_push_s_token_reaches_git_through_askpass_and_is_revoked_after() {
    let (publication, counted) = pushing(Verdict::Current);
    let token = publication.token_for("l", "t").unwrap().unwrap();
    let spec = publication
        .spec(&grant(Role::Implementer, Some("t")), "t")
        .unwrap();
    let shown = format!(
        "{:?}",
        spec.with_git_environment(token.git_environment().unwrap())
    );
    assert!(
        shown.contains("GIT_ASKPASS") && !shown.contains("token-0"),
        "{shown}"
    );
    assert_eq!(counted.revoked.load(Ordering::SeqCst), 0);
    drop(token);
    assert_eq!(counted.revoked.load(Ordering::SeqCst), 1);
}

#[test]
fn a_failed_mint_and_a_spent_budget_are_refused_as_the_linux_helper_words_them() {
    let (publication, counted) = pushing(Verdict::Current);
    counted.broken.store(true, Ordering::SeqCst);
    let failed = publication.token_for("l", "t").err().unwrap();
    assert_eq!(
        (failed.code, failed.message.as_str()),
        (RefusalCode::Internal, NO_CREDENTIALS)
    );
    counted.broken.store(false, Ordering::SeqCst);
    // The failed mint spent one of the burst.
    for _ in 1..agentc_integrator::mint_budget::MINT_BURST {
        drop(publication.token_for("l", "t").unwrap());
    }
    let spent = publication.token_for("l", "t").err().unwrap();
    assert_eq!(spent.message, RATE_LIMITED);
}

#[test]
fn without_credentials_no_token_is_asked_for() {
    let (publication, fixed) = broker(Verdict::Current);
    assert!(publication.token_for("l", "t").unwrap().is_none());
    assert_eq!(
        fixed.asked.load(Ordering::SeqCst),
        0,
        "the gate is the protocol's"
    );
}

/// A client's side of a session in memory: what it sent, then what the
/// broker wrote back.
struct Duplex {
    sent: std::io::Cursor<Vec<u8>>,
    written: Vec<u8>,
}

impl Read for Duplex {
    /// The client's bytes, then the end of the stream.
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.sent.read(buffer)
    }
}

impl Write for Duplex {
    /// Keeps what the broker writes.
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.written.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    /// Nothing is buffered.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Serves `sent` as one session of launch `l` on task `t` with counted
/// credentials, returning the reply and the mints made.
fn served(sent: Vec<u8>) -> (PushReply, u32, u32) {
    let (publication, counted) = pushing(Verdict::Current);
    let dir = tempfile::tempdir().unwrap();
    let publication = Publication {
        work_root: dir.path().to_path_buf(),
        ..publication
    };
    let spec = publication
        .spec(&grant(Role::Implementer, Some("t")), "t")
        .unwrap();
    let mut duplex = Duplex {
        sent: std::io::Cursor::new(sent),
        written: Vec::new(),
    };
    let reply = publication
        .serve_stream("l", "t", spec, &mut duplex)
        .unwrap();
    let (minted, revoked) = (
        counted.minted.load(Ordering::SeqCst),
        counted.revoked.load(Ordering::SeqCst),
    );
    (reply, minted, revoked)
}

#[test]
fn a_session_that_is_no_push_request_costs_no_mint() {
    for sent in [
        b"not a push request\n".to_vec(),
        Vec::new(),
        b"{\"version\":9}\n\0\0\0\0\0\0\0\x01".to_vec(),
    ] {
        let (reply, minted, _) = served(sent);
        assert!(matches!(reply, PushReply::Refused(_)), "{reply:?}");
        assert_eq!(minted, 0);
    }
}

#[test]
fn a_well_formed_preamble_costs_one_mint_revoked_whatever_the_bundle() {
    let mut sent = br#"{"version":1,"revision":"0000000000000000000000000000000000000001","tree":"0000000000000000000000000000000000000002"}"#.to_vec();
    sent.push(b'\n');
    sent.extend_from_slice(&4_u64.to_be_bytes());
    sent.extend_from_slice(b"junk");
    let (reply, minted, revoked) = served(sent);
    assert!(matches!(reply, PushReply::Refused(_)), "{reply:?}");
    assert_eq!((minted, revoked), (1, 1));
}

/// A broker that keeps its launches' repositories below `root`.
fn rooted(root: &std::path::Path) -> Publication {
    let (publication, _) = broker(Verdict::Current);
    Publication {
        work_root: root.to_path_buf(),
        ..publication
    }
}

/// Runs Git in `directory` with `input` on its standard input.
fn git(directory: &std::path::Path, arguments: &[&str], input: &[u8]) {
    use std::process::{Command, Stdio};
    let mut child = Command::new("git");
    let child = child.current_dir(directory).args(arguments);
    let child = child.stdin(Stdio::piped()).stdout(Stdio::null());
    let mut child = child.spawn().unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    assert!(child.wait().unwrap().success(), "git {arguments:?}");
}

/// A bare repository named `launch` below `root` holding one object, as a
/// push leaves it: Git makes its object files read-only.
fn repository(root: &std::path::Path, launch: &str) -> PathBuf {
    let path = root.join(launch);
    std::fs::create_dir_all(&path).unwrap();
    git(&path, &["init", "--bare", "--quiet", "."], b"");
    git(&path, &["hash-object", "-w", "--stdin"], b"an object");
    path
}

#[test]
fn a_closed_launch_s_repository_is_removed_and_another_launch_s_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let publication = rooted(dir.path());
    let (closed, open) = (repository(dir.path(), "l"), repository(dir.path(), "m"));
    Handler::closed(&publication, "l");
    assert!(!closed.exists(), "the closed launch's repository stayed");
    assert!(open.join("HEAD").exists(), "another launch's went too");
    Handler::closed(&publication, "never-pushed");
}

#[test]
fn a_repository_whose_push_is_under_way_is_kept_until_a_later_sweep() {
    let dir = tempfile::tempdir().unwrap();
    let publication = rooted(dir.path());
    let (pushing, idle) = (repository(dir.path(), "l"), repository(dir.path(), "m"));
    let turn = Turn::take(&publication.busy, "l").expect("a free launch");
    Handler::closed(&publication, "l");
    publication.sweep();
    assert!(pushing.join("HEAD").exists(), "removed under a push");
    assert!(!idle.exists(), "the sweep left an idle launch's repository");
    drop(turn);
    Handler::stopped(&publication);
    assert!(!pushing.exists(), "the stop left a repository");
    assert!(
        Turn::take(&publication.busy, "l").is_some(),
        "a turn was kept"
    );
}

#[test]
fn a_sweep_empties_the_work_root_without_following_a_link() {
    let dir = tempfile::tempdir().unwrap();
    let (root, outside) = (dir.path().join("work"), dir.path().join("outside"));
    rooted(&root).sweep();
    assert!(!root.exists(), "a sweep made the work root");
    repository(&root, "crashed");
    std::fs::write(root.join("stray.bundle"), b"x").unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("kept"), b"x").unwrap();
    let junction = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .args([root.join("link"), outside.clone()])
        .output()
        .unwrap();
    assert!(junction.status.success(), "{junction:?}");
    rooted(&root).sweep();
    let left: Vec<_> = std::fs::read_dir(&root).unwrap().flatten().collect();
    assert!(left.is_empty(), "the sweep left {left:?}");
    assert!(outside.join("kept").exists(), "the sweep followed a link");
}
