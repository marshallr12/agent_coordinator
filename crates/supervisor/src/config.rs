//! Host supervisor configuration. Every entry has an in-code default, so a
//! host runs with no configuration file at all; `/etc/agentc/supervisor.toml`
//! (or `--config`) only overrides what differs on that host.
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Default location of the optional configuration file.
pub const DEFAULT_CONFIG_PATH: &str = "/etc/agentc/supervisor.toml";

/// Paths, identities and pinned binaries for one host.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Root-owned prefix holding the pinned harness and CLI binaries.
    pub bin_dir: PathBuf,
    /// Per-role state (config dirs, clones, run dirs) lives under here.
    pub state_dir: PathBuf,
    /// Unix account that runs implementer launches.
    pub implementer_user: String,
    /// Unix account that runs reviewer launches.
    pub reviewer_user: String,
    /// Loopback CONNECT proxy that enforces the egress allowlist.
    pub egress_proxy: String,
    /// Exact harness versions a launch refuses to run without.
    pub pinned: Pinned,
}

/// Exact version strings reported by `--version`; empty means "not pinned".
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Pinned {
    pub claude: String,
    pub codex: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bin_dir: PathBuf::from("/opt/agentc/bin"),
            state_dir: PathBuf::from("/var/lib/agentc"),
            implementer_user: "agentc-impl".into(),
            reviewer_user: "agentc-rev".into(),
            egress_proxy: "http://127.0.0.1:3128".into(),
            pinned: Pinned::default(),
        }
    }
}

impl Config {
    /// Loads `path` if given, else the default path if it exists, else defaults.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let default = Path::new(DEFAULT_CONFIG_PATH);
        let chosen = match path {
            Some(path) => path,
            None if default.exists() => default,
            None => return Ok(Self::default()),
        };
        let text = std::fs::read_to_string(chosen)
            .with_context(|| format!("read {}", chosen.display()))?;
        toml::from_str(&text).with_context(|| format!("parse {}", chosen.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_equals_defaults() {
        assert_eq!(toml::from_str::<Config>("").unwrap(), Config::default());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(toml::from_str::<Config>("bin_dirr = '/x'").is_err());
    }
}
