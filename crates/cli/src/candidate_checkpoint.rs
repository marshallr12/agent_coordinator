//! The candidate checkpoint a code submission records: either a direct push
//! from the prepared checkout, or, when [`CANDIDATE_PUSH_SOCKET_ENV`] names a
//! socket, a push through the per-launch candidate push helper listening on
//! it. Both routes end with the same readback verification.

use std::path::{Path, PathBuf};

#[cfg(unix)]
use coordinator_local::candidate_push::{PushRefusal, RefusalCode};
use coordinator_local::git_workflow::{
    CandidateCheckpoint, CleanSnapshot, checkpoint_candidate, scan_clean_candidate,
    verify_published_candidate,
};
#[cfg(unix)]
use serde_json::json;

use crate::Failure;

/// Names the Unix socket of the candidate push helper. When it holds an
/// absolute path (surrounding whitespace is trimmed), code submissions push
/// through the helper instead of running `git push` themselves.
pub const CANDIDATE_PUSH_SOCKET_ENV: &str = "AGENT_COORDINATOR_CANDIDATE_PUSH_SOCKET";

/// What a code submission checkpoints: the prepared checkout, the configured
/// repository, the launch's base commit, the caller's `--candidate-ref` (if
/// any), the ref used when none is given, and the caller's credential digest.
pub struct CheckpointRequest<'a> {
    pub checkout: &'a Path,
    pub repository: &'a str,
    pub base_revision: &'a str,
    pub explicit_ref: Option<&'a str>,
    pub default_ref: &'a str,
    pub credential_digest: &'a str,
}

/// The helper socket named by [`CANDIDATE_PUSH_SOCKET_ENV`], if any.
pub fn helper_socket_from_env() -> Result<Option<PathBuf>, Failure> {
    helper_socket(std::env::var_os(CANDIDATE_PUSH_SOCKET_ENV))
}

/// Trims the value and treats an absent, empty or whitespace-only one as "no
/// helper configured". Anything else must be an absolute UTF-8 path.
fn helper_socket(value: Option<std::ffi::OsString>) -> Result<Option<PathBuf>, Failure> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value
        .into_string()
        .map_err(|_| Failure::invalid(format!("{CANDIDATE_PUSH_SOCKET_ENV} is not valid UTF-8")))?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if !Path::new(trimmed).is_absolute() {
        return Err(Failure::invalid(format!(
            "{CANDIDATE_PUSH_SOCKET_ENV} must be an absolute socket path"
        )));
    }
    Ok(Some(PathBuf::from(trimmed)))
}

/// Publishes and verifies the candidate, through the helper at
/// `helper_socket` when one is given and directly otherwise. Off Unix a
/// configured helper is refused before the checkout is read.
pub fn checkpoint(
    request: &CheckpointRequest<'_>,
    helper_socket: Option<&Path>,
) -> Result<CandidateCheckpoint, Failure> {
    match helper_socket {
        None => checkpoint_directly(request),
        Some(socket) if cfg!(unix) => checkpoint_through_helper(request, socket),
        Some(_) => Err(unsupported_platform()),
    }
}

/// Pushes from the checkout itself with [`checkpoint_candidate`], to the
/// caller's ref or the default one.
fn checkpoint_directly(request: &CheckpointRequest<'_>) -> Result<CandidateCheckpoint, Failure> {
    checkpoint_candidate(
        request.checkout,
        request.repository,
        request.explicit_ref.unwrap_or(request.default_ref),
        &[request.credential_digest],
    )
    .map_err(Failure::invalid)
}

/// Scans the clean snapshot with the caller's credential digest, checks the
/// base commit, sends the candidate to the helper, and verifies the ref the
/// helper reports by reading it back. A caller-selected ref is refused before
/// anything is read or sent.
fn checkpoint_through_helper(
    request: &CheckpointRequest<'_>,
    socket: &Path,
) -> Result<CandidateCheckpoint, Failure> {
    refuse_explicit_ref(request)?;
    let snapshot = scan_clean_candidate(
        request.checkout,
        request.repository,
        &[request.credential_digest],
    )
    .map_err(Failure::invalid)?;
    require_base_commit(&snapshot, request.base_revision)?;
    let reference = send_to_helper(socket, &snapshot, request.base_revision)?;
    verify_published_candidate(&snapshot, request.repository, &reference).map_err(Failure::invalid)
}

/// The helper fixes the ref, so any `--candidate-ref` is refused.
fn refuse_explicit_ref(request: &CheckpointRequest<'_>) -> Result<(), Failure> {
    if request.explicit_ref.is_none() {
        return Ok(());
    }
    Err(Failure::local(
        2,
        "candidate_ref_fixed_by_helper",
        format!(
            "--candidate-ref cannot be used while {CANDIDATE_PUSH_SOCKET_ENV} is set; \
             the candidate push helper chooses the ref"
        ),
        false,
    ))
}

/// Requires the launch's base commit, the bundle's prerequisite, to exist in
/// the checkout.
fn require_base_commit(snapshot: &CleanSnapshot, base_revision: &str) -> Result<(), Failure> {
    let object = format!("{base_revision}^{{commit}}");
    crate::worktree::git_ok(&snapshot.checkout, ["cat-file", "-e", object.as_str()]).map_err(|_| {
        Failure::invalid(format!(
            "the launch's base revision {base_revision} is not a commit in the checkout; \
                 the candidate push helper needs it as the bundle prerequisite"
        ))
    })
}

/// Connects to the helper, sends the snapshot's commit with the base commit
/// as the bundle prerequisite, and returns the ref the helper pushed.
#[cfg(unix)]
fn send_to_helper(
    socket: &Path,
    snapshot: &CleanSnapshot,
    base_revision: &str,
) -> Result<String, Failure> {
    let stream = std::os::unix::net::UnixStream::connect(socket).map_err(|error| {
        Failure::local(
            7,
            "candidate_push_unavailable",
            format!("could not connect to the candidate push helper socket: {error}"),
            true,
        )
    })?;
    let mut stream = ObservedStream::new(stream);
    coordinator_local::candidate_push::send_candidate(
        &mut stream,
        &snapshot.checkout,
        &snapshot.revision,
        &[base_revision],
    )
    .map(|receipt| receipt.reference)
    .map_err(|error| send_failure(&error, &stream))
}

/// Helper sockets are Unix sockets, so off Unix sending always fails.
#[cfg(not(unix))]
fn send_to_helper(
    _socket: &Path,
    _snapshot: &CleanSnapshot,
    _base_revision: &str,
) -> Result<String, Failure> {
    Err(unsupported_platform())
}

/// The invalid-input failure for a helper configured off Unix.
fn unsupported_platform() -> Failure {
    Failure::invalid(format!(
        "{CANDIDATE_PUSH_SOCKET_ENV} is set, but candidate push helpers need Unix sockets"
    ))
}

/// A stream that records whether it was used, whether an I/O call on it
/// failed, and whether a read met the end of the stream, so a failed send can
/// be attributed to the checkout, the connection, or the helper.
#[cfg(unix)]
struct ObservedStream<S> {
    inner: S,
    used: bool,
    failed: bool,
    eof: bool,
}

#[cfg(unix)]
impl<S> ObservedStream<S> {
    /// Wraps `inner`, not yet used, failed or at its end.
    fn new(inner: S) -> Self {
        Self {
            inner,
            used: false,
            failed: false,
            eof: false,
        }
    }

    /// Records one I/O call's outcome; an interrupted call is retried by its
    /// caller and is not a failure.
    fn observe<T>(&mut self, result: std::io::Result<T>) -> std::io::Result<T> {
        self.used = true;
        if let Err(error) = &result {
            self.failed |= error.kind() != std::io::ErrorKind::Interrupted;
        }
        result
    }
}

#[cfg(unix)]
impl<S: std::io::Read> std::io::Read for ObservedStream<S> {
    /// Reads from the inner stream, recording the outcome; reading nothing
    /// into a non-empty buffer marks the end of the stream.
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let result = self.inner.read(buffer);
        self.eof |= matches!(result, Ok(0)) && !buffer.is_empty();
        self.observe(result)
    }
}

#[cfg(unix)]
impl<S: std::io::Write> std::io::Write for ObservedStream<S> {
    /// Writes to the inner stream, recording the outcome.
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let result = self.inner.write(buffer);
        self.observe(result)
    }

    /// Flushes the inner stream, recording the outcome.
    fn flush(&mut self) -> std::io::Result<()> {
        let result = self.inner.flush();
        self.observe(result)
    }
}

/// Maps a failed send. A helper refusal goes through [`refusal_failure`]. A
/// failed socket read or write, or a connection the helper closed before
/// replying, is a retryable `candidate_push_failed` (exit 7). An error before the stream was used came from preparing the
/// bundle in the checkout: invalid input (exit 2). Anything else, such as a
/// malformed reply or a receipt naming another commit, tree or ref, is a
/// non-retryable `candidate_push_protocol_violation` (exit 5).
#[cfg(unix)]
fn send_failure<S>(error: &anyhow::Error, stream: &ObservedStream<S>) -> Failure {
    if let Some(refusal) = error.downcast_ref::<PushRefusal>() {
        return refusal_failure(refusal);
    }
    if stream.failed || stream.eof {
        let message = format!("the candidate push helper connection failed: {error}");
        return Failure::local(7, "candidate_push_failed", message, true);
    }
    if !stream.used {
        return Failure::invalid(format!("could not bundle the candidate: {error}"));
    }
    let message = format!("the candidate push helper broke the protocol: {error}");
    Failure::local(5, "candidate_push_protocol_violation", message, false)
}

/// `candidate_push_refused` carrying the helper's message, with
/// [`printable`] applied, and its refusal code under `details.refusal_code`. A lease conflict is a state conflict
/// (exit 5); push and internal helper failures are temporary and retryable
/// (exit 7); every other refusal is invalid input (exit 2).
#[cfg(unix)]
fn refusal_failure(refusal: &PushRefusal) -> Failure {
    let (exit, retryable) = match refusal.code {
        RefusalCode::LeaseConflict => (5, false),
        RefusalCode::PushFailed | RefusalCode::Internal => (7, true),
        RefusalCode::BadRequest
        | RefusalCode::TooLarge
        | RefusalCode::BundleInvalid
        | RefusalCode::RevisionMismatch
        | RefusalCode::SecretDetected => (2, false),
    };
    let mut failure = Failure::local(
        exit,
        "candidate_push_refused",
        printable(&refusal.message),
        retryable,
    );
    failure.output["error"]["details"] = json!({ "refusal_code": refusal.code });
    failure
}

/// `message` with every control character and bidirectional formatting
/// character replaced by a space, so helper text cannot drive a terminal.
#[cfg(unix)]
fn printable(message: &str) -> String {
    let unsafe_char = |c: char| {
        c.is_control()
            || matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    };
    message
        .chars()
        .map(|c| if unsafe_char(c) { ' ' } else { c })
        .collect()
}

#[cfg(all(test, unix))]
mod tests;
