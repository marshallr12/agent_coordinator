use super::*;
use sha2::{Digest, Sha256};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use tempfile::TempDir;

const REFERENCE: &str = "refs/agent-coordinator/candidates/task-1/launch-1";
/// The helper's local lease and intent refs for [`REFERENCE`].
const LEASE: &str = "refs/agent-coordinator/leases/task-1/launch-1";
const INTENT: &str = "refs/agent-coordinator/intents/task-1/launch-1";

/// A bare "remote" on disk and an implementer clone whose `main` it holds.
struct Fixture {
    directory: TempDir,
    remote: PathBuf,
    source: PathBuf,
    base: String,
}

/// Creates the remote and a clone with one published `base` commit.
fn fixture() -> Fixture {
    let directory = tempfile::Builder::new()
        .prefix("candidate push paths with spaces ")
        .tempdir()
        .unwrap();
    let root = directory.path();
    let remote = root.join("remote with spaces.git");
    let source = root.join("implementer clone");
    git_ok(
        root,
        ["init", "--bare", "--quiet", remote.to_str().unwrap()],
    )
    .unwrap();
    git_ok(
        root,
        [
            "clone",
            "--quiet",
            remote.to_str().unwrap(),
            source.to_str().unwrap(),
        ],
    )
    .unwrap();
    git_ok(&source, ["config", "user.name", "Coordinator Test"]).unwrap();
    git_ok(&source, ["config", "user.email", "test@example.invalid"]).unwrap();
    let fixture = Fixture {
        directory,
        remote,
        source,
        base: String::new(),
    };
    let base = fixture.commit("base.txt", "base\n");
    git_ok(
        &fixture.source,
        ["push", "--quiet", "origin", "HEAD:refs/heads/main"],
    )
    .unwrap();
    Fixture { base, ..fixture }
}

impl Fixture {
    /// The helper for task `task-1`, launch `launch-1`, pushing to the remote.
    fn spec(&self) -> HelperSpec {
        let work_dir = self.directory.path().join("helper work");
        HelperSpec::new(
            self.remote.to_str().unwrap(),
            "task-1",
            "launch-1",
            &work_dir,
        )
        .unwrap()
    }

    /// Commits `content` at `path` on the clone's current branch.
    fn commit(&self, path: &str, content: &str) -> String {
        fs::write(self.source.join(path), content).unwrap();
        git_ok(&self.source, ["add", "--", path]).unwrap();
        git_ok(&self.source, ["commit", "--quiet", "-m", path]).unwrap();
        git_text(&self.source, ["rev-parse", "HEAD"]).unwrap()
    }

    /// The tree of `revision`.
    fn tree(&self, revision: &str) -> String {
        git_text(&self.source, ["rev-parse", &format!("{revision}^{{tree}}")]).unwrap()
    }

    /// What the remote holds at `reference`.
    fn remote_ref(&self, reference: &str) -> Option<String> {
        let remote = self.remote.clone().into_os_string();
        observe_remote_reference_with(&self.source, remote, reference, &[]).unwrap()
    }

    /// Every ref name the remote holds.
    fn remote_ref_names(&self) -> Vec<String> {
        let listing = git_text(&self.remote, ["for-each-ref", "--format=%(refname)"]).unwrap();
        listing.lines().map(str::to_owned).collect()
    }

    /// Sends `revision` through a helper with `spec`, excluding `base`.
    fn send(&self, spec: &HelperSpec, revision: &str) -> (Result<PushReceipt>, PushReply) {
        let base = self.base.clone();
        exchange(spec, |stream| {
            send_candidate(stream, &self.source, revision, &[&base])
        })
    }

    /// Runs Git in the clone with `input` on stdin and returns its trimmed
    /// output.
    fn git_input(&self, arguments: &[&str], input: &[u8]) -> String {
        let arguments = arguments.iter().map(OsString::from);
        let output = git_raw(&self.source, arguments, Some(input), &[]).unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    /// A commit on top of `base` whose tree holds `entries` (mode, name,
    /// object) exactly as given, built without a checkout so names a file
    /// system would refuse are possible.
    fn commit_tree(&self, entries: &[(&str, String, String)]) -> String {
        let listing: String = entries
            .iter()
            .map(|(mode, name, oid)| {
                let kind = if *mode == "040000" { "tree" } else { "blob" };
                format!("{mode} {kind} {oid}\t{name}\n")
            })
            .collect();
        let tree = self.git_input(&["mktree"], listing.as_bytes());
        let arguments = [
            "commit-tree",
            tree.as_str(),
            "-p",
            self.base.as_str(),
            "-m",
            "crafted",
        ];
        git_text(&self.source, arguments).unwrap()
    }

    /// Stores `content` as a blob in the clone.
    fn blob(&self, content: &[u8]) -> String {
        self.git_input(&["hash-object", "-w", "--stdin"], content)
    }

    /// A bundle file of the given `tips` (refs in the clone) minus `base`.
    fn bundle(&self, tips: &[&str]) -> Vec<u8> {
        let path = self
            .directory
            .path()
            .join(format!("{}.bundle", Uuid::new_v4()));
        let exclusion = format!("^{}", self.base);
        let arguments = ["bundle", "create", path.to_str().unwrap()]
            .into_iter()
            .chain(tips.iter().copied())
            .chain([exclusion.as_str()]);
        git_ok(&self.source, arguments).unwrap();
        fs::read(path).unwrap()
    }
}

/// Runs `client` against a helper serving one request with `spec` over a
/// socket pair; returns the client's result and the helper's reply.
fn exchange<T>(spec: &HelperSpec, client: impl FnOnce(&mut UnixStream) -> T) -> (T, PushReply) {
    let (mut near, mut far) = UnixStream::pair().unwrap();
    let spec = spec.clone();
    let server = std::thread::spawn(move || serve_one(&mut far, &spec));
    let result = client(&mut near);
    drop(near);
    (result, server.join().unwrap().unwrap())
}

/// Sends hand-built request bytes (line, declared length, body) and returns
/// the parsed reply. Write errors are ignored: the helper may refuse early.
fn send_raw(spec: &HelperSpec, line: &[u8], length: u64, body: &[u8]) -> PushReply {
    let (reply, _) = exchange(spec, |stream| {
        let _ = stream.write_all(line);
        let _ = stream.write_all(&length.to_be_bytes());
        let _ = stream.write_all(body);
        let _ = stream.shutdown(Shutdown::Write);
        PushReply::parse(&read_line(stream, MAX_REPLY_LINE).unwrap()).unwrap()
    });
    reply
}

/// The request line for `revision` and `tree`.
fn request_line(revision: &str, tree: &str) -> Vec<u8> {
    format!("{{\"version\":1,\"revision\":\"{revision}\",\"tree\":\"{tree}\"}}\n").into_bytes()
}

/// The refusal code of a reply, or `None` for an acceptance.
fn code(reply: &PushReply) -> Option<RefusalCode> {
    match reply {
        PushReply::Accepted(_) => None,
        PushReply::Refused(refusal) => Some(refusal.code),
    }
}

/// The refusal carried by a client error.
fn refusal_of(result: Result<PushReceipt>) -> PushRefusal {
    result.unwrap_err().downcast::<PushRefusal>().unwrap()
}

/// Files the helper left in its work directory that are received bundles.
fn leftover_bundles(spec: &HelperSpec) -> usize {
    fs::read_dir(&spec.work_dir)
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".bundle")
        })
        .count()
}

/// A fake AWS key assembled at runtime so this file never matches the scanner.
fn fake_key() -> String {
    format!("{}{}", "AKIA", "ABCDEFGHIJKLMNOP")
}

#[test]
fn round_trip_creates_the_fixed_candidate_ref() {
    let fixture = fixture();
    let spec = fixture.spec();
    let candidate = fixture.commit("feature.txt", "feature\n");
    let (receipt, reply) = fixture.send(&spec, &candidate);
    let expected = PushReceipt {
        reference: REFERENCE.to_owned(),
        revision: candidate.clone(),
        tree: fixture.tree(&candidate),
        previous: None,
    };
    assert_eq!(receipt.unwrap(), expected);
    assert_eq!(reply, PushReply::Accepted(expected));
    assert_eq!(fixture.remote_ref(REFERENCE), Some(candidate));
    assert_eq!(leftover_bundles(&spec), 0);
    let quarantined = git_text(
        &spec.work_dir,
        ["for-each-ref", "refs/agent-coordinator/quarantine/"],
    );
    assert_eq!(quarantined.unwrap(), "");
}

#[test]
fn second_distinct_commit_is_refused_and_the_remote_is_unchanged() {
    let fixture = fixture();
    let spec = fixture.spec();
    let first = fixture.commit("first.txt", "first\n");
    fixture.send(&spec, &first).0.unwrap();
    let second = fixture.commit("second.txt", "second\n");
    let refusal = refusal_of(fixture.send(&spec, &second).0);
    assert_eq!(refusal.code, RefusalCode::CandidateAlreadyPublished);
    assert_eq!(fixture.remote_ref(REFERENCE), Some(first));
    let imported = git_ok(&spec.work_dir, ["cat-file", "-e", &second]);
    assert!(imported.is_err(), "the refused bundle was imported");
}

#[test]
fn same_commit_retry_is_accepted_without_moving_the_ref() {
    let fixture = fixture();
    let spec = fixture.spec();
    let first = fixture.commit("first.txt", "first\n");
    fixture.send(&spec, &first).0.unwrap();
    let replay = fixture.send(&spec, &first).0.unwrap();
    assert_eq!(replay.previous.as_deref(), Some(first.as_str()));
    assert_eq!(fixture.remote_ref(REFERENCE), Some(first));
}

#[test]
fn remote_moved_elsewhere_is_a_lease_conflict() {
    let fixture = fixture();
    let spec = fixture.spec();
    let first = fixture.commit("first.txt", "first\n");
    fixture.send(&spec, &first).0.unwrap();
    let moved = fixture.commit("moved.txt", "moved\n");
    let force = format!("+{moved}:{REFERENCE}");
    git_ok(
        &fixture.source,
        ["push", "--quiet", "origin", force.as_str()],
    )
    .unwrap();
    let refusal = refusal_of(fixture.send(&spec, &first).0);
    assert_eq!(refusal.code, RefusalCode::LeaseConflict);
    assert_eq!(fixture.remote_ref(REFERENCE), Some(moved));
}

#[test]
fn ref_created_by_someone_else_is_a_lease_conflict() {
    let fixture = fixture();
    let spec = fixture.spec();
    let planted = fixture.commit("planted.txt", "planted\n");
    let create = format!("{planted}:{REFERENCE}");
    git_ok(
        &fixture.source,
        ["push", "--quiet", "origin", create.as_str()],
    )
    .unwrap();
    let candidate = fixture.commit("candidate.txt", "candidate\n");
    let refusal = refusal_of(fixture.send(&spec, &candidate).0);
    assert_eq!(refusal.code, RefusalCode::LeaseConflict);
    assert_eq!(fixture.remote_ref(REFERENCE), Some(planted));
}

#[test]
fn mismatched_tree_or_revision_is_refused() {
    let fixture = fixture();
    let spec = fixture.spec();
    let candidate = fixture.commit("feature.txt", "feature\n");
    git_ok(&fixture.source, ["branch", "candidate", &candidate]).unwrap();
    let bundle = fixture.bundle(&["refs/heads/candidate"]);
    let length = bundle.len() as u64;

    let wrong_tree = request_line(&candidate, &fixture.tree(&fixture.base));
    let reply = send_raw(&spec, &wrong_tree, length, &bundle);
    assert_eq!(code(&reply), Some(RefusalCode::RevisionMismatch));

    let not_a_tip = request_line(&fixture.base, &fixture.tree(&fixture.base));
    let reply = send_raw(&spec, &not_a_tip, length, &bundle);
    assert_eq!(code(&reply), Some(RefusalCode::RevisionMismatch));
    assert_eq!(fixture.remote_ref(REFERENCE), None);
}

#[test]
fn planted_secret_is_refused_and_nothing_is_pushed() {
    let fixture = fixture();
    let spec = fixture.spec();
    let key = fake_key();
    let leak = fixture.commit("config.txt", &format!("key = {key}\n"));
    let refusal = refusal_of(fixture.send(&spec, &leak).0);
    assert_eq!(refusal.code, RefusalCode::SecretDetected);
    assert!(
        refusal.message.contains("aws_access_key"),
        "{}",
        refusal.message
    );
    assert!(!refusal.message.contains(&key), "{}", refusal.message);
    assert_eq!(fixture.remote_ref(REFERENCE), None);
}

#[test]
fn known_credential_digest_is_refused() {
    let fixture = fixture();
    let token = "ab".repeat(32);
    let digest = hex::encode(Sha256::digest(token.as_bytes()));
    let spec = fixture.spec().with_known_digests(vec![digest]);
    let leak = fixture.commit("notes.txt", &format!("token {token}\n"));
    let refusal = refusal_of(fixture.send(&spec, &leak).0);
    assert_eq!(refusal.code, RefusalCode::SecretDetected);
    assert!(!refusal.message.contains(&token), "{}", refusal.message);
    assert_eq!(fixture.remote_ref(REFERENCE), None);
}

#[test]
fn oversize_bundle_is_too_large() {
    let fixture = fixture();
    let spec = fixture.spec().with_max_bundle_bytes(64);
    let candidate = fixture.commit("feature.txt", "feature\n");
    let line = request_line(&candidate, &fixture.tree(&candidate));
    assert_eq!(
        code(&send_raw(&spec, &line, 65, &[])),
        Some(RefusalCode::TooLarge)
    );
    assert_eq!(
        code(&send_raw(&spec, &line, u64::MAX, &[])),
        Some(RefusalCode::TooLarge)
    );

    // The client reports a refusal the helper sends before reading the bundle.
    let refusal = refusal_of(fixture.send(&spec, &candidate).0);
    assert_eq!(refusal.code, RefusalCode::TooLarge);
    assert_eq!(fixture.remote_ref(REFERENCE), None);
}

#[test]
fn malformed_requests_are_bad_requests() {
    let fixture = fixture();
    let spec = fixture.spec();
    let candidate = fixture.commit("feature.txt", "feature\n");
    let tree = fixture.tree(&candidate);
    let lines = [
        format!("{{\"version\":2,\"revision\":\"{candidate}\",\"tree\":\"{tree}\"}}\n"),
        format!(
            "{{\"version\":1,\"revision\":\"{candidate}\",\"tree\":\"{tree}\",\"ref\":\"refs/heads/main\"}}\n"
        ),
        format!(
            "{{\"version\":1,\"revision\":\"{}\",\"tree\":\"{tree}\"}}\n",
            &candidate[..12]
        ),
        "not json\n".to_owned(),
        format!("{{\"version\":1,\"revision\":\"{candidate}\""),
    ];
    for line in lines {
        let reply = send_raw(&spec, line.as_bytes(), 0, &[]);
        assert_eq!(code(&reply), Some(RefusalCode::BadRequest), "{line}");
    }
    let truncated = send_raw(
        &spec,
        &request_line(&candidate, &tree),
        1000,
        b"# v2 git bundle\n",
    );
    assert_eq!(code(&truncated), Some(RefusalCode::BundleInvalid));
    assert_eq!(leftover_bundles(&spec), 0);
    assert_eq!(fixture.remote_ref(REFERENCE), None);
}

#[test]
fn bundle_refs_cannot_choose_the_written_ref() {
    let fixture = fixture();
    let spec = fixture.spec();
    let candidate = fixture.commit("feature.txt", "feature\n");
    let other = fixture.commit("other.txt", "other\n");
    let decoys = [
        "refs/heads/evil",
        "refs/agent-coordinator/candidates/task-1/other",
    ];
    git_ok(&fixture.source, ["update-ref", decoys[0], &other]).unwrap();
    git_ok(&fixture.source, ["update-ref", decoys[1], &candidate]).unwrap();
    let bundle = fixture.bundle(&decoys);
    let line = request_line(&candidate, &fixture.tree(&candidate));
    let reply = send_raw(&spec, &line, bundle.len() as u64, &bundle);
    assert_eq!(code(&reply), None, "{reply:?}");
    assert_eq!(
        fixture.remote_ref_names(),
        [
            "refs/agent-coordinator/candidates/task-1/launch-1",
            "refs/heads/main"
        ]
    );
    assert_eq!(fixture.remote_ref(REFERENCE), Some(candidate));
}

#[test]
fn helper_spec_rejects_unsafe_ids() {
    let work_dir = Path::new("/var/lib/agentc-push/run");
    let long = "a".repeat(MAX_ID_LENGTH + 1);
    let unsafe_ids = [
        "",
        ".hidden",
        "-flag",
        "a..b",
        "a/b",
        "a b",
        "a:b",
        "a~b",
        "a^b",
        "a@{1}",
        "end.",
        "x.lock",
        "a\nb",
        "\u{e9}",
        long.as_str(),
    ];
    for id in unsafe_ids {
        assert!(
            HelperSpec::new("/srv/remote.git", id, "launch", work_dir).is_err(),
            "{id:?}"
        );
        assert!(
            HelperSpec::new("/srv/remote.git", "task", id, work_dir).is_err(),
            "{id:?}"
        );
    }
    let longest = "a".repeat(MAX_ID_LENGTH);
    for id in [
        "a",
        "A-1_b.c",
        "0d4f2c9e-7a51-4c8e-9b0f-3e2d1c0b9a87",
        longest.as_str(),
    ] {
        let spec = HelperSpec::new("/srv/remote.git", id, id, work_dir).unwrap();
        assert_eq!(spec.reference(), format!("{CANDIDATE_REF_PREFIX}{id}/{id}"));
    }
    assert!(HelperSpec::new("/srv/remote.git", "task", "launch", Path::new("relative")).is_err());
    assert!(HelperSpec::new("--upload-pack=x", "task", "launch", work_dir).is_err());
}

#[test]
fn reply_lines_have_the_documented_shape() {
    let accepted = PushReply::Accepted(PushReceipt {
        reference: REFERENCE.to_owned(),
        revision: "1".repeat(40),
        tree: "2".repeat(40),
        previous: None,
    });
    let line = accepted.to_line();
    assert_eq!(line.last(), Some(&b'\n'));
    let value: serde_json::Value = serde_json::from_slice(&line).unwrap();
    let expected = json!({"ok": true, "reference": REFERENCE, "revision": "1".repeat(40), "tree": "2".repeat(40), "previous": null});
    assert_eq!(value, expected);
    assert_eq!(PushReply::parse(&line).unwrap(), accepted);

    let refused = PushReply::Refused(refusal(RefusalCode::LeaseConflict, "moved"));
    let value: serde_json::Value = serde_json::from_slice(&refused.to_line()).unwrap();
    assert_eq!(
        value,
        json!({"ok": false, "code": "lease_conflict", "message": "moved"})
    );
    assert_eq!(PushReply::parse(&refused.to_line()).unwrap(), refused);
    assert!(PushReply::parse(br#"{"ok":false,"code":"lease_conflict"}"#).is_err());
    assert!(PushReply::parse(br#"{"ok":true,"code":"push_failed","message":"x"}"#).is_err());
}

#[test]
fn debug_output_hides_remote_and_credentials() {
    let spec = HelperSpec::new(
        "https://x-access-token:remote-secret@github.example/r.git",
        "t",
        "l",
        Path::new("/w"),
    )
    .unwrap()
    .with_git_environment(vec![(
        "GIT_PUSH_TOKEN".to_owned(),
        "environment-secret".to_owned(),
    )]);
    let shown = format!("{spec:?}");
    assert!(shown.contains("GIT_PUSH_TOKEN"), "{shown}");
    assert!(
        !shown.contains("remote-secret") && !shown.contains("environment-secret"),
        "{shown}"
    );
}

#[test]
fn foreign_ref_naming_the_requested_revision_is_not_adopted() {
    let fixture = fixture();
    let spec = fixture.spec();
    let foreign = fixture.commit("foreign.txt", "foreign\n");
    let create = format!("{foreign}:{REFERENCE}");
    git_ok(
        &fixture.source,
        ["push", "--quiet", "origin", create.as_str()],
    )
    .unwrap();
    let refusal = refusal_of(fixture.send(&spec, &foreign).0);
    assert_eq!(refusal.code, RefusalCode::LeaseConflict);
    let next = fixture.commit("next.txt", "next\n");
    let refusal = refusal_of(fixture.send(&spec, &next).0);
    assert_eq!(refusal.code, RefusalCode::LeaseConflict);
    assert_eq!(fixture.remote_ref(REFERENCE), Some(foreign));
}

/// Pushes a first commit through `spec`, then rewinds the helper's
/// bookkeeping to what a crash between the push and the lease write leaves:
/// no lease, and an intent naming the commit the remote now holds. Returns
/// that commit.
fn landed_push_without_a_lease(fixture: &Fixture, spec: &HelperSpec) -> String {
    let first = fixture.commit("first.txt", "first\n");
    fixture.send(spec, &first).0.unwrap();
    git_ok(&spec.work_dir, ["update-ref", "-d", LEASE]).unwrap();
    git_ok(&spec.work_dir, ["update-ref", INTENT, &first]).unwrap();
    first
}

#[test]
fn landed_push_with_unrecorded_lease_recovers_through_the_intent() {
    let fixture = fixture();
    let spec = fixture.spec();
    let first = landed_push_without_a_lease(&fixture, &spec);

    let retry = fixture.send(&spec, &first).0.unwrap();
    assert_eq!(retry.previous.as_deref(), Some(first.as_str()));
    assert_eq!(
        git_text(&spec.work_dir, ["rev-parse", LEASE]).unwrap(),
        first
    );
    assert!(git_ok(&spec.work_dir, ["rev-parse", "--verify", "--quiet", INTENT]).is_err());
    let next = fixture.commit("next.txt", "next\n");
    let refusal = refusal_of(fixture.send(&spec, &next).0);
    assert_eq!(refusal.code, RefusalCode::CandidateAlreadyPublished);
    assert_eq!(fixture.remote_ref(REFERENCE), Some(first));
}

#[test]
fn landed_push_with_unrecorded_lease_refuses_another_commit() {
    let fixture = fixture();
    let spec = fixture.spec();
    let first = landed_push_without_a_lease(&fixture, &spec);
    let next = fixture.commit("next.txt", "next\n");
    let refusal = refusal_of(fixture.send(&spec, &next).0);
    assert_eq!(refusal.code, RefusalCode::CandidateAlreadyPublished);
    assert_eq!(fixture.remote_ref(REFERENCE), Some(first));
}

#[test]
fn malformed_objects_are_refused() {
    let fixture = fixture();
    let spec = fixture.spec();
    let config = fixture.blob(b"[core]\n");
    let inner = fixture.git_input(
        &["mktree"],
        format!("100644 blob {config}\tconfig\n").as_bytes(),
    );
    let crafted = fixture.commit_tree(&[("040000", ".git".to_owned(), inner)]);
    let refusal = refusal_of(fixture.send(&spec, &crafted).0);
    assert_eq!(
        refusal.code,
        RefusalCode::BundleInvalid,
        "{}",
        refusal.message
    );
    assert_eq!(fixture.remote_ref(REFERENCE), None);
}

#[test]
fn long_credential_paths_still_fit_the_reply() {
    let fixture = fixture();
    let spec = fixture.spec();
    let blob = fixture.blob(b"not a key\n");
    let entries: Vec<_> = (0..5)
        .map(|index| {
            (
                "100644",
                format!("{}{index}.pem", "p".repeat(20_000)),
                blob.clone(),
            )
        })
        .collect();
    let crafted = fixture.commit_tree(&entries);
    let (result, reply) = fixture.send(&spec, &crafted);
    let refusal = refusal_of(result);
    assert_eq!(refusal.code, RefusalCode::SecretDetected);
    assert!(
        refusal.message.contains("credential_file"),
        "{}",
        refusal.message
    );
    assert!(reply.to_line().len() <= MAX_REPLY_LINE);
    assert_eq!(fixture.remote_ref(REFERENCE), None);
}

#[test]
fn oversized_refusal_keeps_its_code() {
    let long = refusal(RefusalCode::SecretDetected, &"x".repeat(MAX_REPLY_LINE));
    let fitted = fit_reply(PushReply::Refused(long));
    assert_eq!(
        fitted,
        PushReply::Refused(refusal(RefusalCode::SecretDetected, OVERSIZED_MESSAGE))
    );
    let short = PushReply::Refused(refusal(RefusalCode::LeaseConflict, "moved"));
    assert_eq!(fit_reply(short.clone()), short);
}

#[test]
fn secrets_in_messages_and_identities_are_refused() {
    let fixture = fixture();
    let spec = fixture.spec();
    let key = fake_key();
    fs::write(fixture.source.join("message.txt"), "clean\n").unwrap();
    git_ok(&fixture.source, ["add", "message.txt"]).unwrap();
    let subject = format!("rotate {key}");
    git_ok(
        &fixture.source,
        ["commit", "--quiet", "-m", subject.as_str()],
    )
    .unwrap();
    let in_message = git_text(&fixture.source, ["rev-parse", "HEAD"]).unwrap();
    let refusal = refusal_of(fixture.send(&spec, &in_message).0);
    assert_eq!(refusal.code, RefusalCode::SecretDetected);
    assert!(!refusal.message.contains(&key), "{}", refusal.message);

    let author = format!("user.email={key}@example.invalid");
    git_ok(
        &fixture.source,
        ["reset", "--quiet", "--hard", fixture.base.as_str()],
    )
    .unwrap();
    fs::write(fixture.source.join("ident.txt"), "clean\n").unwrap();
    git_ok(&fixture.source, ["add", "ident.txt"]).unwrap();
    git_ok(
        &fixture.source,
        ["-c", author.as_str(), "commit", "--quiet", "-m", "clean"],
    )
    .unwrap();
    let in_ident = git_text(&fixture.source, ["rev-parse", "HEAD"]).unwrap();
    let refusal = refusal_of(fixture.send(&spec, &in_ident).0);
    assert_eq!(refusal.code, RefusalCode::SecretDetected);
    assert_eq!(fixture.remote_ref(REFERENCE), None);
}

/// A raw commit on `base` in the clone with an optional `encoding` header and
/// `message` bytes, written without Git re-encoding anything.
fn raw_commit(fixture: &Fixture, encoding: Option<&str>, message: &[u8]) -> String {
    let tree = fixture.tree(&fixture.base);
    let ident = "A <a@example.invalid> 0 +0000";
    let mut object = format!(
        "tree {tree}\nparent {}\nauthor {ident}\ncommitter {ident}\n",
        fixture.base
    )
    .into_bytes();
    if let Some(encoding) = encoding {
        object.extend_from_slice(format!("encoding {encoding}\n").as_bytes());
    }
    object.push(b'\n');
    object.extend_from_slice(message);
    fixture.git_input(
        &[
            "hash-object",
            "-t",
            "commit",
            "--literally",
            "-w",
            "--stdin",
        ],
        &object,
    )
}

#[test]
fn secrets_in_raw_commits_are_refused_whatever_their_encoding() {
    let fixture = fixture();
    let spec = fixture.spec();
    let message = format!("rotate {}\n", fake_key());
    let wide: Vec<u8> = message.encode_utf16().flat_map(u16::to_be_bytes).collect();
    // NUL bytes in a commit are refused by the object check before the scan.
    let cases = [
        (
            None,
            message.as_bytes().to_vec(),
            RefusalCode::SecretDetected,
        ),
        (
            Some("UTF-16BE"),
            message.as_bytes().to_vec(),
            RefusalCode::SecretDetected,
        ),
        (Some("UTF-16BE"), wide, RefusalCode::BundleInvalid),
    ];
    for (encoding, bytes, expected) in cases {
        let commit = raw_commit(&fixture, encoding, &bytes);
        let refusal = refusal_of(fixture.send(&spec, &commit).0);
        assert_eq!(refusal.code, expected, "{encoding:?}: {}", refusal.message);
        if expected == RefusalCode::BundleInvalid {
            assert_eq!(refusal.message, "bundle contains malformed objects");
        }
    }
    assert_eq!(fixture.remote_ref(REFERENCE), None);
}

#[test]
fn failed_push_does_not_leave_an_intent_to_adopt() {
    let fixture = fixture();
    let spec = fixture.spec();
    let hook = fixture.remote.join("hooks").join("pre-receive");
    fs::create_dir_all(hook.parent().unwrap()).unwrap();
    fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    let executable = <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755);
    fs::set_permissions(&hook, executable).unwrap();
    let failed = fixture.commit("failed.txt", "failed\n");
    let refusal = refusal_of(fixture.send(&spec, &failed).0);
    assert_eq!(refusal.code, RefusalCode::PushFailed);
    assert!(git_ok(&spec.work_dir, ["rev-parse", "--verify", "--quiet", INTENT]).is_err());

    fs::remove_file(&hook).unwrap();
    let foreign = format!("{failed}:{REFERENCE}");
    git_ok(
        &fixture.source,
        ["push", "--quiet", "origin", foreign.as_str()],
    )
    .unwrap();
    let next = fixture.commit("next.txt", "next\n");
    let refusal = refusal_of(fixture.send(&spec, &next).0);
    assert_eq!(refusal.code, RefusalCode::LeaseConflict);
    assert_eq!(fixture.remote_ref(REFERENCE), Some(failed));
}
