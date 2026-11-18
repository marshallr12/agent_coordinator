//! The Windows candidate-checkpoint client against a helper served on a
//! local named pipe, as the publication broker serves it.
use super::*;
use coordinator_local::candidate_push::{HelperSpec, PushReply, serve_one};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs::File;
use std::os::windows::io::FromRawHandle;
use std::process::Command;
use std::thread::JoinHandle;
use tempfile::TempDir;
use windows_sys::Win32::Foundation::{ERROR_PIPE_CONNECTED, GetLastError, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};

/// The ref the test helper is fixed to.
const HELPER_REF: &str = "refs/agent-coordinator/candidates/task-1/launch-1";

/// A bare remote and a clean checkout with a candidate on a published base.
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

/// Commits `content` at `path` and returns the new commit.
fn commit(checkout: &Path, path: &str, content: &str) -> String {
    std::fs::write(checkout.join(path), content).unwrap();
    git(checkout, &["add", "--", path]);
    git(checkout, &["commit", "--quiet", "-m", path]);
    git(checkout, &["rev-parse", "HEAD"])
}

/// Builds the fixture with `file.txt` as the candidate's change.
fn fixture() -> Fixture {
    let directory = tempfile::Builder::new().prefix("ckpt ").tempdir().unwrap();
    let remote = directory.path().join("remote.git");
    let checkout = directory.path().join("checkout");
    let remote = remote.to_str().unwrap().to_owned();
    git(directory.path(), &["init", "--bare", "--quiet", &remote]);
    let path = checkout.to_str().unwrap();
    git(directory.path(), &["clone", "--quiet", &remote, path]);
    git(&checkout, &["config", "user.name", "Coordinator Test"]);
    git(&checkout, &["config", "user.email", "test@example.invalid"]);
    let base = commit(&checkout, "base.txt", "base\n");
    git(
        &checkout,
        &["push", "--quiet", "origin", "HEAD:refs/heads/main"],
    );
    let candidate = commit(&checkout, "file.txt", "feature\n");
    let digest = hex::encode(Sha256::digest("cd".repeat(32).as_bytes()));
    Fixture {
        directory,
        remote,
        checkout,
        base,
        candidate,
        digest,
    }
}

impl Fixture {
    /// A request for this fixture with no explicit ref.
    fn request(&self) -> CheckpointRequest<'_> {
        CheckpointRequest {
            checkout: &self.checkout,
            repository: &self.remote,
            base_revision: &self.base,
            explicit_ref: None,
            default_ref: "refs/agent-coordinator/candidates/attempt-1",
            credential_digest: &self.digest,
        }
    }

    /// The helper's spec, with a work directory of its own.
    fn spec(&self) -> HelperSpec {
        let work_dir = self.directory.path().join("helper work");
        HelperSpec::new(&self.remote, "task-1", "launch-1", &work_dir).unwrap()
    }
}

/// A local pipe name no other test or run uses.
fn pipe_path() -> String {
    format!(
        r"\\.\pipe\agentc-cli-test-{}",
        uuid::Uuid::new_v4().simple()
    )
}

/// Creates one instance of the pipe at `path`, rejecting remote clients, and
/// serves its first client with `serve` on a thread.
fn serving(
    path: &str,
    serve: impl FnOnce(&mut File) -> Option<PushReply> + Send + 'static,
) -> JoinHandle<Option<PushReply>> {
    let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
    let mode = PIPE_TYPE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
    // SAFETY: the name is NUL-terminated; default security for a test pipe.
    let handle = unsafe {
        CreateNamedPipeW(
            wide.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            mode,
            1,
            65536,
            65536,
            0,
            std::ptr::null(),
        )
    };
    assert_ne!(handle, INVALID_HANDLE_VALUE, "CreateNamedPipeW failed");
    let raw = handle as usize;
    std::thread::spawn(move || {
        let handle = raw as windows_sys::Win32::Foundation::HANDLE;
        // SAFETY: the handle is the live pipe instance created above.
        let connected = unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) };
        // SAFETY: reading the calling thread's last error is always safe.
        assert!(connected != 0 || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED);
        // SAFETY: this thread owns the handle from here on.
        let mut file = unsafe { File::from_raw_handle(handle as _) };
        serve(&mut file)
    })
}

#[test]
fn the_pipe_variable_must_name_a_local_pipe() {
    let local = helper_socket(Some(OsString::from(
        r" \\.\pipe\agentc-broker-publication-l ",
    )));
    let expected = PathBuf::from(r"\\.\pipe\agentc-broker-publication-l");
    assert_eq!(local.ok().unwrap(), Some(expected));
    for refused in [
        r"\\host\pipe\agentc",
        r"\\.\pipe\",
        r"\\.\pipe\a\b",
        r"C:\agentc.sock",
        r"\\?\pipe\agentc",
    ] {
        let failure = helper_socket(Some(OsString::from(refused))).unwrap_err();
        assert_eq!(failure.exit, 2, "{refused:?}");
    }
}

#[test]
fn a_candidate_is_published_through_the_pipe_and_its_receipt_checked() {
    let fixture = fixture();
    let (path, spec) = (pipe_path(), fixture.spec());
    let server = serving(&path, move |stream| serve_one(stream, &spec).ok());
    let pushed = checkpoint(&fixture.request(), Some(Path::new(&path))).unwrap();
    assert_eq!(pushed.reference, HELPER_REF);
    assert_eq!(pushed.revision, fixture.candidate);
    assert!(matches!(
        server.join().unwrap(),
        Some(PushReply::Accepted(_))
    ));
    let format = "--format=%(refname) %(objectname)";
    let prefix = "refs/agent-coordinator/candidates/";
    let refs = git(
        Path::new(&fixture.remote),
        &["for-each-ref", format, prefix],
    );
    assert_eq!(refs, format!("{HELPER_REF} {}", fixture.candidate));
}

#[test]
fn a_refusal_for_lost_authority_is_a_non_retryable_state_conflict() {
    let fixture = fixture();
    let (path, spec) = (pipe_path(), fixture.spec());
    let refuse = || {
        Err(PushRefusal {
            code: RefusalCode::NotAuthorized,
            message: "the attempt\u{1b}[31m is no longer current".into(),
        })
    };
    let server = serving(&path, move |stream| {
        coordinator_local::candidate_push::serve_one_gated(stream, &spec, &refuse).ok()
    });
    let failure = checkpoint(&fixture.request(), Some(Path::new(&path))).unwrap_err();
    let refused = server.join().unwrap();
    assert!(
        matches!(refused, Some(PushReply::Refused(_))),
        "{refused:?}"
    );
    assert_eq!(failure.exit, 5);
    assert_eq!(failure.output["error"]["code"], "candidate_push_refused");
    assert_eq!(
        failure.output["error"]["details"]["refusal_code"],
        "not_authorized"
    );
    let message = failure.output["error"]["message"].as_str().unwrap();
    assert!(!message.contains('\u{1b}'), "{message:?}");
}

#[test]
fn a_pipe_nobody_serves_is_a_retryable_unavailable_helper() {
    let fixture = fixture();
    let missing = pipe_path();
    let failure = checkpoint(&fixture.request(), Some(Path::new(&missing))).unwrap_err();
    assert_eq!(failure.exit, 7);
    assert_eq!(
        failure.output["error"]["code"],
        "candidate_push_unavailable"
    );
}
