//! Shared settings for the tests that run the real Bubblewrap (the unit tests
//! in `sandbox` and the `launch_relay` / `netns_relay` integration tests) or
//! that need host resources a supervised launch lacks (a writable `/tmp`).
//! Production code calls none of the functions; it only exports
//! [`NESTED_SANDBOX_ENV`] to every launch (see `profile`), so the gate a role
//! runs inside its sandbox skips these tests with a note. They are public so
//! integration tests can share them.
use crate::config::Config;
use std::ffi::OsString;
use std::path::PathBuf;

/// Names the Bubblewrap the real-sandbox tests run. The containment suite sets
/// it to the host's agentc copy (`/opt/agentc/bin/bwrap`), which Ubuntu's
/// AppArmor user-namespace restriction lets the role accounts use.
pub const BUBBLEWRAP_ENV: &str = "AGENTC_TEST_BWRAP";

/// Set (non-empty) by the containment suite when it runs the tests inside
/// another sandbox, such as `codex sandbox`, where Bubblewrap cannot work, and
/// by the supervisor in every launch's environment.
pub const NESTED_SANDBOX_ENV: &str = "AGENTC_TEST_NESTED_SANDBOX";

/// Why a test whose fixture lives under `/tmp` (to keep Unix socket paths
/// short) skips inside a launch, whose `/tmp` is the host's, read-only.
pub const READ_ONLY_TMP: &str =
    "it needs a writable /tmp for short socket paths, which a launch mounts read-only";

/// Why a test that binds a Unix socket deep inside its `$TMPDIR` fixture
/// skips inside a launch, whose long `$TMPDIR` pushes the path past the
/// kernel's 108-byte socket path limit.
pub const LONG_SOCKET_PATH: &str =
    "it binds a socket deep under $TMPDIR, past the socket path limit inside a launch";

/// Markers that say the tests run nested inside another sandbox: the suite's
/// own, then the ones Codex sets for commands in its sandbox.
const NESTED_MARKERS: [&str; 3] = [
    NESTED_SANDBOX_ENV,
    "CODEX_SANDBOX",
    "CODEX_SANDBOX_NETWORK_DISABLED",
];

/// The Bubblewrap the real-sandbox tests run: `$AGENTC_TEST_BWRAP` when set
/// and non-empty, else the configuration default (`/usr/bin/bwrap`).
pub fn bubblewrap() -> PathBuf {
    non_empty(BUBBLEWRAP_ENV).map_or_else(|| Config::default().bubblewrap, PathBuf::from)
}

/// Whether test `name` must skip because it runs nested inside another
/// sandbox, where the uid mapping hides Bubblewrap's root ownership and user
/// namespaces are unavailable. Only an explicit marker variable skips, so a
/// broken Bubblewrap on an ordinary host still fails the test. Prints a note
/// naming the test and the marker when it skips.
pub fn skip_when_nested(name: &str) -> bool {
    skip_when_nested_because(
        name,
        "it runs the real Bubblewrap, which cannot run nested inside another sandbox",
    )
}

/// Whether test `name` must skip because it runs nested inside another
/// sandbox (such as a supervised launch) that lacks something it needs, which
/// `reason` names: for example a writable `/tmp`, which a launch mounts
/// read-only. Only an explicit marker variable skips, so the test still runs,
/// and fails loudly, everywhere else. Prints a note naming the test, the
/// reason and the marker when it skips.
pub fn skip_when_nested_because(name: &str, reason: &str) -> bool {
    let marker = NESTED_MARKERS
        .into_iter()
        .find(|key| non_empty(key).is_some());
    if let Some(marker) = marker {
        note(&format!(
            "note: skipping {name}: {reason} ({marker} is set)"
        ));
    }
    marker.is_some()
}

/// Writes `line` straight to the process's stderr. The test harness captures
/// only `print!`/`eprint!` output, so the note reaches a log (such as the
/// containment suite's) without `--nocapture`.
fn note(line: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "{line}");
}

/// The value of environment variable `key` when it is set and non-empty.
fn non_empty(key: &str) -> Option<OsString> {
    std::env::var_os(key).filter(|value| !value.is_empty())
}
