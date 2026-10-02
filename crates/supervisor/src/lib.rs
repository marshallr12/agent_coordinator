//! `agentc-supervisor`: host-side containment for supervised agent launches.
//!
//! P2 scope (autonomy plan §2.3): exact launch profiles, generated role
//! settings, hardened per-launch clones, a launch preflight and a single
//! supervised launch. Scheduling, leases and reviews arrive in P3.
//! Claude launches use a read-only Bubblewrap root with narrow writable mounts.
//! Codex retains its native workspace-write profile.
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
pub mod relay;
pub mod role_settings;
pub mod sandbox;
pub mod shadow;
pub mod staging_login;
pub mod verification;
