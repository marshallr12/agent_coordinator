//! `agentc-push`: the candidate-push helper. The supervisor spawns one per
//! implementer launch, as the unprivileged user holding the push App key,
//! listening on a Unix socket bound into the launch's sandbox. The helper
//! publishes the implementer's candidate bundles to the one ref fixed by its
//! task and launch ids (see `coordinator_local::candidate_push`), pushing
//! with an installation token minted for each worthwhile connection and
//! revoked after it.
//!
//! The helper asks for SIGTERM when the thread that spawned it exits
//! (`PR_SET_PDEATHSIG` follows the spawning thread, not its process), so the
//! supervisor must spawn it from a thread that lives as long as the launch;
//! a short-lived worker thread would end the helper early. Passing
//! `--parent-pid` lets the helper confirm at startup that this parent is
//! still the one it has.
//!
//! Everything it relies on (Unix sockets, `PR_SET_PDEATHSIG`, user ids) is
//! Linux, so the helper itself is built only there; elsewhere the binary
//! refuses to run.
#[cfg(target_os = "linux")]
mod askpass;
#[cfg(target_os = "linux")]
mod budget;
#[cfg(target_os = "linux")]
mod cli;
#[cfg(target_os = "linux")]
mod config;
#[cfg(target_os = "linux")]
mod preamble;
#[cfg(target_os = "linux")]
mod process;
#[cfg(target_os = "linux")]
mod serve;
#[cfg(target_os = "linux")]
mod socket;

use std::process::ExitCode;

/// Runs the helper's command line.
#[cfg(target_os = "linux")]
fn main() -> ExitCode {
    cli::main()
}

/// The helper has no implementation off Linux: it says so and fails.
#[cfg(not(target_os = "linux"))]
fn main() -> ExitCode {
    eprintln!("agentc-push: the candidate-push helper requires Linux");
    ExitCode::from(1)
}
