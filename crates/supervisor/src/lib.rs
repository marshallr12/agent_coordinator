//! `agentc-supervisor`: host-side containment for supervised agent launches.
//!
//! P2 scope (autonomy plan §2.3): exact launch profiles, generated role
//! settings, hardened per-launch clones, a launch preflight and a single
//! supervised launch. Scheduling, leases and reviews arrive in P3.
//! Claude launches use a read-only Bubblewrap root with narrow writable mounts.
//! Codex retains its native workspace-write profile; every harness runs with
//! `no_new_privs` and its leftover processes are killed when it exits.
pub mod candidate;
pub mod clone;
pub mod config;
pub mod confine;
pub mod egress;
pub mod estimate;
pub mod launch;
pub mod network_probe;
pub mod preflight;
pub mod profile;
#[cfg(target_os = "linux")]
pub mod push_helper;
pub mod reaper;
pub mod relay;
pub mod role_settings;
pub mod sandbox;
pub mod shadow;
pub mod staging_login;
pub mod verification;

/// The root-only directory holding each implementer launch's push-helper
/// directory, `<state_dir>/push`.
pub fn push_helper_root(config: &config::Config) -> std::path::PathBuf {
    config.state_dir.join("push")
}
