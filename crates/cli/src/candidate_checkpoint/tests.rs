use super::*;
use coordinator_local::candidate_push::{HelperSpec, PushReceipt, PushReply, serve_one};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::Command;
use std::thread::JoinHandle;
use tempfile::TempDir;

/// The ref the test helper is fixed to.
const HELPER_REF: &str = "refs/agent-coordinator/candidates/task-1/launch-1";
/// The ref a direct checkpoint uses when no `--candidate-ref` is given.
const DEFAULT_REF: &str = "refs/agent-coordinator/candidates/attempt-1";

/// A bare remote, a clean checkout holding a `candidate` commit on top of the
/// published `base`, and the caller's credential digest.
struct Fixture {
    directory: TempDir,
    remote: String,
    checkout: PathBuf,
    base: String,
    candidate: String,
    digest: String,
}

/// Runs Git in `directory` and returns its trimmed standard output.
fn git(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(directory)
        .args(arguments)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {arguments:?} failed");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// Builds the fixture, committing `content` as the candidate's `file.txt`.
fn fixture(content: &str) -> Fixture {
    let directory = tempfile::Builder::new()
        .prefix("candidate checkpoint ")
        .tempdir()
        .unwrap();
    let remote = directory.path().join("remote.git");
    let checkout = directory.path().join("checkout");
    let (remote, path) = (
        remote.to_str().unwrap().to_owned(),
        checkout.to_str().unwrap(),
    );
    git(directory.path(), &["init", "--bare", "--quiet", &remote]);
    git(directory.path(), &["clone", "--quiet", &remote, path]);
    git(&checkout, &["config", "user.name", "Coordinator Test"]);
    git(&checkout, &["config", "user.email", "test@example.invalid"]);
    let base = commit(&checkout, "base.txt", "base\n");
    git(
        &checkout,
        &["push", "--quiet", "origin", "HEAD:refs/heads/main"],
    );
    let candidate = commit(&checkout, "file.txt", content);
    let digest = hex::encode(Sha256::digest(token().as_bytes()));
    Fixture {
        directory,
        remote,
        checkout,
        base,
        candidate,
        digest,
    }
}

/// Commits `content` at `path` and returns the new commit.
fn commit(checkout: &Path, path: &str, content: &str) -> String {
    std::fs::write(checkout.join(path), content).unwrap();
    git(checkout, &["add", "--", path]);
    git(checkout, &["commit", "--quiet", "-m", path]);
    git(checkout, &["rev-parse", "HEAD"])
}

/// The caller's coordinator token: a 64-hex word the helper alone cannot
/// recognise, since it never learns the caller's credential digest.
fn token() -> String {
    "cd".repeat(32)
}

impl Fixture {
    /// A request for this fixture with the caller's `explicit_ref`.
    fn request<'a>(&'a self, explicit_ref: Option<&'a str>) -> CheckpointRequest<'a> {
        CheckpointRequest {
            checkout: &self.checkout,
            repository: &self.remote,
            base_revision: &self.base,
            explicit_ref,
            default_ref: DEFAULT_REF,
            credential_digest: &self.digest,
        }
    }

    /// The tree of the candidate commit.
    fn candidate_tree(&self) -> String {
        git(
            &self.checkout,
            &["rev-parse", &format!("{}^{{tree}}", self.candidate)],
        )
    }

    /// Every ref the remote holds under the candidate namespace, with its commit.
    fn remote_candidates(&self) -> String {
        let format = "--format=%(refname) %(objectname)";
        let prefix = "refs/agent-coordinator/candidates/";
        git(Path::new(&self.remote), &["for-each-ref", format, prefix])
    }

    /// Points `reference` on the remote at `revision`, bypassing any helper.
    fn plant(&self, reference: &str, revision: &str) {
        let refspec = format!("{revision}:{reference}");
        git(&self.checkout, &["push", "--quiet", "origin", &refspec]);
    }

    /// The helper fixed to [`HELPER_REF`], with one work directory per
    /// fixture.
    fn spec(&self) -> HelperSpec {
        let work_dir = self.directory.path().join("helper work");
        HelperSpec::new(&self.remote, "task-1", "launch-1", &work_dir).unwrap()
    }

    /// A helper that serves one connection.
    fn helper(&self) -> FakeHelper {
        let spec = self.spec();
        FakeHelper::spawn(self, move |stream| serve_one(stream, &spec).ok())
    }

    /// A helper that serves one connection fully, pushing as usual, but
    /// closes it without sending its reply.
    fn helper_losing_its_reply(&self) -> FakeHelper {
        let spec = self.spec();
        FakeHelper::spawn(self, move |stream| {
            serve_one(&mut ReplyDropped(stream), &spec).ok()
        })
    }
}

/// A thread answering one connection on a Unix socket in the fixture's
/// directory.
struct FakeHelper {
    socket: PathBuf,
    thread: JoinHandle<Option<PushReply>>,
}

impl FakeHelper {
    /// Binds the socket, replacing an earlier helper's, and runs `serve` on
    /// the first connection.
    fn spawn<F>(fixture: &Fixture, serve: F) -> Self
    where
        F: FnOnce(&mut UnixStream) -> Option<PushReply> + Send + 'static,
    {
        let socket = fixture.directory.path().join("helper.sock");
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).unwrap();
        let thread = std::thread::spawn(move || serve(&mut listener.accept().unwrap().0));
        Self { socket, thread }
    }

    /// What the helper replied to its first connection. Connects once
    /// itself, so a helper nobody contacted sees an empty request instead
    /// of waiting forever.
    fn finish(self) -> Option<PushReply> {
        let _ = UnixStream::connect(&self.socket);
        self.thread.join().unwrap()
    }
}

/// A connection whose reads pass through and whose writes are discarded.
struct ReplyDropped<'a>(&'a mut UnixStream);

impl Read for ReplyDropped<'_> {
    /// Reads from the connection.
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.0.read(buffer)
    }
}

impl Write for ReplyDropped<'_> {
    /// Accepts and discards `buffer`.
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        Ok(buffer.len())
    }

    /// Nothing is buffered.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Reads a whole request (line, length, bundle) and replies `reply` without
/// pushing anything.
fn reply_without_pushing(stream: &mut UnixStream, reply: PushReply) -> Option<PushReply> {
    let mut byte = [0_u8; 1];
    while byte[0] != b'\n' {
        stream.read_exact(&mut byte).unwrap();
    }
    let mut length = [0_u8; 8];
    stream.read_exact(&mut length).unwrap();
    let length = u64::from_be_bytes(length);
    io::copy(&mut stream.take(length), &mut io::sink()).unwrap();
    stream.write_all(&reply.to_line()).unwrap();
    Some(reply)
}

/// The failure's `error.code`.
fn code(failure: &Failure) -> &str {
    failure.output["error"]["code"].as_str().unwrap()
}

#[test]
fn helper_socket_trims_and_requires_an_absolute_path() {
    for unset in [None, Some(""), Some(" \t\n ")] {
        let value = unset.map(OsString::from);
        assert_eq!(helper_socket(value).ok().unwrap(), None, "{unset:?}");
    }
    let named = helper_socket(Some(OsString::from(" /run/push.sock\n")));
    assert_eq!(named.ok().unwrap(), Some(PathBuf::from("/run/push.sock")));
    for relative in ["push.sock", " run/push.sock "] {
        let failure = helper_socket(Some(OsString::from(relative))).unwrap_err();
        assert_eq!(code(&failure), "invalid_client_input", "{relative:?}");
        assert_eq!(failure.exit, 2);
    }
}

#[test]
fn without_a_helper_the_candidate_is_pushed_directly() {
    let fixture = fixture("feature\n");
    let defaulted = checkpoint(&fixture.request(None), None).unwrap();
    assert_eq!(defaulted.reference, DEFAULT_REF);
    assert_eq!(defaulted.revision, fixture.candidate);
    let expected = format!("{DEFAULT_REF} {}", fixture.candidate);
    assert_eq!(fixture.remote_candidates(), expected);
    let chosen = "refs/agent-coordinator/candidates/chosen";
    let explicit = checkpoint(&fixture.request(Some(chosen)), None).unwrap();
    assert_eq!(explicit.reference, chosen);
}

#[test]
fn helper_pushes_the_candidate_and_its_ref_is_recorded() {
    let fixture = fixture("feature\n");
    let helper = fixture.helper();
    let socket = helper.socket.clone();
    let pushed = checkpoint(&fixture.request(None), Some(&socket)).unwrap();
    assert_eq!(pushed.reference, HELPER_REF);
    assert_eq!(pushed.revision, fixture.candidate);
    assert_eq!(pushed.tree, fixture.candidate_tree());
    assert!(matches!(helper.finish(), Some(PushReply::Accepted(_))));
    let expected = format!("{HELPER_REF} {}", fixture.candidate);
    assert_eq!(fixture.remote_candidates(), expected);
}

#[test]
fn explicit_candidate_ref_is_refused_before_anything_is_sent() {
    let fixture = fixture("feature\n");
    let helper = fixture.helper();
    let socket = helper.socket.clone();
    let failure = checkpoint(&fixture.request(Some(HELPER_REF)), Some(&socket)).unwrap_err();
    assert_eq!(code(&failure), "candidate_ref_fixed_by_helper");
    assert_eq!(failure.exit, 2);
    assert!(!matches!(helper.finish(), Some(PushReply::Accepted(_))));
    assert_eq!(fixture.remote_candidates(), "");
}

#[test]
fn caller_credential_is_refused_before_anything_is_sent() {
    let fixture = fixture(&format!("token {}\n", token()));
    let helper = fixture.helper();
    let socket = helper.socket.clone();
    let failure = checkpoint(&fixture.request(None), Some(&socket)).unwrap_err();
    assert_eq!(code(&failure), "invalid_client_input");
    let message = failure.output["error"]["message"].as_str().unwrap();
    assert!(message.contains("coordinator_credential"), "{message}");
    assert!(!message.contains(&token()), "{message}");
    assert!(!matches!(helper.finish(), Some(PushReply::Accepted(_))));
    assert_eq!(fixture.remote_candidates(), "");
}

#[test]
fn helper_receipt_is_verified_by_reading_the_ref_back() {
    for planted in [false, true] {
        let fixture = fixture("feature\n");
        if planted {
            fixture.plant(HELPER_REF, &fixture.base);
        }
        let receipt = PushReceipt {
            reference: HELPER_REF.to_owned(),
            revision: fixture.candidate.clone(),
            tree: fixture.candidate_tree(),
            previous: None,
        };
        let reply = PushReply::Accepted(receipt);
        let helper = FakeHelper::spawn(&fixture, move |s| reply_without_pushing(s, reply));
        let socket = helper.socket.clone();
        let failure = checkpoint(&fixture.request(None), Some(&socket)).unwrap_err();
        assert_eq!(code(&failure), "invalid_client_input", "planted: {planted}");
        assert!(matches!(helper.finish(), Some(PushReply::Accepted(_))));
    }
}

#[test]
fn helper_lease_conflict_is_a_non_retryable_refusal() {
    let fixture = fixture("feature\n");
    fixture.plant(HELPER_REF, &fixture.base);
    let helper = fixture.helper();
    let socket = helper.socket.clone();
    let failure = checkpoint(&fixture.request(None), Some(&socket)).unwrap_err();
    assert_eq!(code(&failure), "candidate_push_refused");
    assert_eq!(
        failure.output["error"]["details"]["refusal_code"],
        "lease_conflict"
    );
    assert_eq!(failure.output["error"]["retryable"], false);
    assert_eq!(failure.exit, 5);
    helper.finish();
}

#[test]
fn second_candidate_through_the_same_helper_is_a_non_retryable_refusal() {
    let fixture = fixture("feature\n");
    let helper = fixture.helper();
    let socket = helper.socket.clone();
    checkpoint(&fixture.request(None), Some(&socket)).unwrap();
    helper.finish();
    commit(&fixture.checkout, "file.txt", "revised\n");
    let helper = fixture.helper();
    let socket = helper.socket.clone();
    let failure = checkpoint(&fixture.request(None), Some(&socket)).unwrap_err();
    assert_eq!(code(&failure), "candidate_push_refused");
    assert_eq!(
        failure.output["error"]["details"]["refusal_code"],
        "candidate_already_published"
    );
    assert_eq!(failure.output["error"]["retryable"], false);
    assert_eq!(failure.exit, 5);
    helper.finish();
    let expected = format!("{HELPER_REF} {}", fixture.candidate);
    assert_eq!(fixture.remote_candidates(), expected);
}

#[test]
fn refusal_codes_map_to_stable_cli_failures() {
    let cases = [
        (RefusalCode::BadRequest, "bad_request", 2, false),
        (RefusalCode::TooLarge, "too_large", 2, false),
        (RefusalCode::BundleInvalid, "bundle_invalid", 2, false),
        (RefusalCode::RevisionMismatch, "revision_mismatch", 2, false),
        (RefusalCode::SecretDetected, "secret_detected", 2, false),
        (RefusalCode::LeaseConflict, "lease_conflict", 5, false),
        (
            RefusalCode::CandidateAlreadyPublished,
            "candidate_already_published",
            5,
            false,
        ),
        (RefusalCode::PushFailed, "push_failed", 7, true),
        (RefusalCode::Internal, "internal", 7, true),
    ];
    for (refusal_code, wire, exit, retryable) in cases {
        let refusal = PushRefusal {
            code: refusal_code,
            message: "helper said no".to_owned(),
        };
        let failure = send_failure(&anyhow::Error::new(refusal), &stream(true, true));
        let error = &failure.output["error"];
        assert_eq!(error["code"], "candidate_push_refused");
        assert_eq!(error["message"], "helper said no");
        assert_eq!(error["details"], json!({ "refusal_code": wire }));
        assert_eq!(
            (failure.exit, error["retryable"].clone()),
            (exit, json!(retryable))
        );
    }
}

/// An observed stream with the given flags.
fn stream(used: bool, failed: bool) -> ObservedStream<()> {
    ObservedStream {
        inner: (),
        used,
        failed,
        eof: false,
    }
}

#[test]
fn other_send_errors_are_attributed_to_their_source() {
    let cases = [
        (stream(false, false), "invalid_client_input", 2, false),
        (stream(true, true), "candidate_push_failed", 7, true),
        (
            stream(true, false),
            "candidate_push_protocol_violation",
            5,
            false,
        ),
    ];
    for (observed, expected, exit, retryable) in cases {
        let failure = send_failure(&anyhow::anyhow!("send failed"), &observed);
        assert_eq!(code(&failure), expected);
        assert_eq!(failure.exit, exit, "{expected}");
        assert_eq!(
            failure.output["error"]["retryable"], retryable,
            "{expected}"
        );
    }
}

#[test]
fn refusal_messages_cannot_carry_terminal_controls() {
    let refusal = PushRefusal {
        code: RefusalCode::SecretDetected,
        message: "a\u{1b}[2Jb\u{7}c\u{9b}d\u{202e}e\nf".to_owned(),
    };
    let failure = refusal_failure(&refusal);
    let message = failure.output["error"]["message"].as_str().unwrap();
    assert_eq!(message, "a [2Jb c d e f");
}

#[test]
fn missing_base_commit_is_refused_before_sending_and_not_retryable() {
    let fixture = fixture("feature\n");
    let helper = fixture.helper();
    let socket = helper.socket.clone();
    let missing = "1".repeat(40);
    let request = CheckpointRequest {
        base_revision: &missing,
        ..fixture.request(None)
    };
    let failure = checkpoint(&request, Some(&socket)).unwrap_err();
    assert_eq!(code(&failure), "invalid_client_input");
    assert_eq!(failure.output["error"]["retryable"], false);
    let message = failure.output["error"]["message"].as_str().unwrap();
    assert!(message.contains("base revision"), "{message}");
    assert!(!matches!(helper.finish(), Some(PushReply::Accepted(_))));
    assert_eq!(fixture.remote_candidates(), "");
}

#[test]
fn mismatched_receipt_is_a_non_retryable_protocol_violation() {
    let fixture = fixture("feature\n");
    let candidate_tree = fixture.candidate_tree();
    let receipts = [
        (HELPER_REF, fixture.base.clone(), candidate_tree.clone()),
        (HELPER_REF, fixture.candidate.clone(), fixture.base.clone()),
        ("refs/heads/main", fixture.candidate.clone(), candidate_tree),
    ];
    for (reference, revision, tree) in receipts {
        let receipt = PushReceipt {
            reference: reference.to_owned(),
            revision,
            tree,
            previous: None,
        };
        let reply = PushReply::Accepted(receipt);
        let helper = FakeHelper::spawn(&fixture, move |s| reply_without_pushing(s, reply));
        let socket = helper.socket.clone();
        let failure = checkpoint(&fixture.request(None), Some(&socket)).unwrap_err();
        assert_eq!(
            code(&failure),
            "candidate_push_protocol_violation",
            "{reference}"
        );
        assert_eq!(failure.exit, 5);
        assert_eq!(failure.output["error"]["retryable"], false);
        helper.finish();
    }
}

#[test]
fn helper_closing_without_a_reply_is_a_retryable_connection_failure() {
    let fixture = fixture("feature\n");
    let helper = FakeHelper::spawn(&fixture, |stream| {
        let mut request = [0_u8; 1];
        stream.read_exact(&mut request).unwrap();
        None
    });
    let socket = helper.socket.clone();
    let failure = checkpoint(&fixture.request(None), Some(&socket)).unwrap_err();
    assert_eq!(code(&failure), "candidate_push_failed");
    assert_eq!(failure.exit, 7);
    assert_eq!(failure.output["error"]["retryable"], true);
    helper.finish();
}

#[test]
fn helper_closing_after_a_full_exchange_is_retryable_and_the_retry_succeeds() {
    let fixture = fixture("feature\n");
    let helper = fixture.helper_losing_its_reply();
    let socket = helper.socket.clone();
    let failure = checkpoint(&fixture.request(None), Some(&socket)).unwrap_err();
    assert_eq!(code(&failure), "candidate_push_failed");
    assert_eq!(failure.exit, 7);
    assert_eq!(failure.output["error"]["retryable"], true);
    assert!(matches!(helper.finish(), Some(PushReply::Accepted(_))));
    let pushed = format!("{HELPER_REF} {}", fixture.candidate);
    assert_eq!(fixture.remote_candidates(), pushed);
    let retry = fixture.helper();
    let checkpointed = checkpoint(&fixture.request(None), Some(&retry.socket.clone())).unwrap();
    assert_eq!(checkpointed.reference, HELPER_REF);
    let Some(PushReply::Accepted(receipt)) = retry.finish() else {
        panic!("the retry was not accepted");
    };
    assert_eq!(receipt.previous, Some(fixture.candidate.clone()));
    assert_eq!(fixture.remote_candidates(), pushed);
}
