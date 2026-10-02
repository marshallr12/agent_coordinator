//! Process-level guards for the helper: it never runs as root, it starts
//! from a minimal environment, and it ends when the supervisor that spawned
//! it ends.
use anyhow::{Result, ensure};
use std::ffi::OsString;

/// The inherited variables the helper keeps: the program search path, the
/// home directory and the locale. Everything else, such as
/// `GIT_SSL_NO_VERIFY`, `GIT_CURL_VERBOSE`, `GIT_CONFIG_PARAMETERS`, proxy or
/// CA-bundle settings, is removed, so neither Git nor the GitHub client
/// inherits a setting that weakens or records the token's use.
const KEPT_VARIABLES: &[&str] = &["PATH", "HOME", "LANG", "LC_ALL"];

/// Refuses a real or effective user id of root: the helper holds only the
/// push App key, and running it as root would put every other root-readable
/// secret within reach of a flaw in it.
pub fn refuse_root(uid: u32, euid: u32) -> Result<()> {
    ensure!(
        uid != 0 && euid != 0,
        "agentc-push refuses to run as root; run it as its own unprivileged user"
    );
    Ok(())
}

/// This process's real and effective user ids.
pub fn current_ids() -> (u32, u32) {
    // SAFETY: getuid and geteuid take no arguments, cannot fail and touch no
    // memory.
    unsafe { (libc::getuid(), libc::geteuid()) }
}

/// The names among `names` that are not in [`KEPT_VARIABLES`].
pub fn foreign_variables(names: impl Iterator<Item = OsString>) -> Vec<OsString> {
    names
        .filter(|name| !KEPT_VARIABLES.iter().any(|kept| name == kept))
        .collect()
}

/// Removes every inherited variable outside [`KEPT_VARIABLES`]. Must run
/// before any thread starts, since it mutates the process environment.
pub fn clear_environment() {
    let names = std::env::vars_os().map(|(name, _)| name);
    for name in foreign_variables(names) {
        // SAFETY: called from `main` before the Tokio runtime, the GitHub
        // client or any other thread exists.
        unsafe { std::env::remove_var(name) };
    }
}

/// Asks the kernel to send this process SIGTERM when the thread that spawned
/// it exits, so a crashed supervisor leaves no helper behind, then checks
/// the parent with [`check_parent`].
pub fn exit_with_parent(expected: Option<u32>) -> Result<()> {
    let signal = libc::c_ulong::try_from(libc::SIGTERM)?;
    // SAFETY: PR_SET_PDEATHSIG takes a signal number and touches no memory.
    let status = unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, signal) };
    ensure!(status == 0, "could not request a parent-death signal");
    // SAFETY: getppid takes no arguments, cannot fail and touches no memory.
    let parent = unsafe { libc::getppid() };
    check_parent(u32::try_from(parent)?, expected)
}

/// Fails when the parent has already gone: `actual` differs from the
/// `expected` parent id, or, with none given, is init's.
pub fn check_parent(actual: u32, expected: Option<u32>) -> Result<()> {
    match expected {
        Some(expected) => ensure!(
            actual == expected,
            "the parent process {expected} has already exited"
        ),
        None => ensure!(actual != 1, "the parent process has already exited"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_is_refused_by_real_or_effective_id() {
        assert!(refuse_root(0, 0).is_err());
        assert!(refuse_root(1000, 0).is_err());
        assert!(refuse_root(0, 1000).is_err());
        assert!(refuse_root(1000, 1000).is_ok());
    }

    #[test]
    fn only_the_kept_variables_survive() {
        let names = [
            "PATH",
            "HOME",
            "LANG",
            "LC_ALL",
            "GIT_SSL_NO_VERIFY",
            "GIT_CONFIG_PARAMETERS",
            "https_proxy",
            "PATHX",
        ];
        let foreign = foreign_variables(names.into_iter().map(OsString::from));
        assert_eq!(
            foreign,
            [
                "GIT_SSL_NO_VERIFY",
                "GIT_CONFIG_PARAMETERS",
                "https_proxy",
                "PATHX"
            ]
        );
    }

    #[test]
    fn the_parent_must_match_the_given_id_or_not_be_init() {
        assert!(check_parent(42, Some(42)).is_ok());
        assert!(check_parent(1, Some(42)).is_err());
        assert!(check_parent(77, Some(42)).is_err());
        assert!(check_parent(42, None).is_ok());
        assert!(check_parent(1, None).is_err());
    }

    /// Set in the child run of `children_inherit_only_the_kept_variables`.
    const PROBE: &str = "AGENTC_PUSH_CLEAR_PROBE";

    #[test]
    fn children_inherit_only_the_kept_variables() {
        if std::env::var_os(PROBE).is_some() {
            clear_environment();
            let output = std::process::Command::new("env").output().unwrap();
            let listing = String::from_utf8(output.stdout).unwrap();
            assert!(listing.lines().any(|line| line.starts_with("PATH=")));
            assert!(!listing.contains("GIT_SSL_NO_VERIFY"), "{listing}");
            assert!(!listing.contains(PROBE), "{listing}");
            return;
        }
        let name = "process::tests::children_inherit_only_the_kept_variables";
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--test-threads=1"])
            .env(PROBE, "1")
            .env("GIT_SSL_NO_VERIFY", "1")
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "the child run failed");
    }
}
