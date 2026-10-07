//! Shared settings for the tests that run the real Bubblewrap (the unit tests
//! in `sandbox` and the `launch_relay` / `netns_relay` integration tests).
//! Production code never calls these; they are public only so integration
//! tests can share them.
use crate::config::Config;
use std::ffi::OsString;
use std::path::PathBuf;

/// Names the Bubblewrap the real-sandbox tests run. The containment suite sets
/// it to the host's agentc copy (`/opt/agentc/bin/bwrap`), which Ubuntu's
/// AppArmor user-namespace restriction lets the role accounts use.
pub const BUBBLEWRAP_ENV: &str = "AGENTC_TEST_BWRAP";

/// Set (non-empty) by the containment suite when it runs the tests inside
/// another sandbox, such as `codex sandbox`, where Bubblewrap cannot work.
pub const NESTED_SANDBOX_ENV: &str = "AGENTC_TEST_NESTED_SANDBOX";

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
    let marker = NESTED_MARKERS
        .into_iter()
        .find(|key| non_empty(key).is_some());
    if let Some(marker) = marker {
        note(&format!(
            "note: skipping {name}: it runs the real Bubblewrap, which cannot run \
             nested inside another sandbox ({marker} is set)"
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
