//! Integrator configuration. Every entry has an in-code default, so a host
//! runs against the loopback staging coordinator with no file at all;
//! `/etc/agentc/integrator.toml` (or `--config`) overrides what differs.
//! Production needs only the service origin, projects and the GitHub App.
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Default location of the optional configuration file.
pub const DEFAULT_CONFIG_PATH: &str = "/etc/agentc/integrator.toml";

/// Where check results and branch rules come from.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChecksKind {
    /// GitHub Actions check runs and the branch rules API.
    Github,
    /// A local JSON file (staging and tests; see `checks::FakeChecks`).
    Fake,
}

/// Settings for one integrator host.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// CLI-format credentials file (`[[credentials]] origin, token`) holding
    /// the `class=integrator` credential; readable only by the integrator uid.
    pub credential_file: PathBuf,
    /// Origin to pick from that file; empty means its first entry.
    pub origin: String,
    /// Permit plain HTTP to a loopback origin (the staging coordinator).
    pub allow_insecure_loopback: bool,
    /// Coordinator project ids this integrator owns; empty means idle.
    pub projects: Vec<String>,
    /// Mirrors, result worktrees, intents and the loop state live here.
    pub state_dir: PathBuf,
    pub poll_seconds: u64,
    /// Prefix of the branches result commits are pushed to for checks.
    pub result_branch_prefix: String,
    /// Check source. Defaults to `fake`: with no fake file there are no
    /// branch rules and no runs, so nothing can publish until a host is
    /// configured on purpose (`github` + an App, or a staging fake file).
    pub checks: ChecksKind,
    /// Fake check runs and branch rules (used when `checks = "fake"`).
    pub fake_checks_file: PathBuf,
    /// Branch rule types the target must carry; a missing one freezes pushes.
    pub required_rules: Vec<String>,
    /// Target-repository file mapping roster identities to check runs.
    pub roster_path: String,
    pub github: GithubConfig,
}

/// GitHub App identity used for Git pushes and the REST API.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct GithubConfig {
    pub api_base: String,
    /// App id; 0 means "not configured" (fine for fake checks + local remotes).
    pub app_id: u64,
    pub installation_id: u64,
    /// PEM private key, root-owned 0400 and readable by the integrator uid only.
    pub private_key: PathBuf,
}

/// Host defaults: loopback staging origin, `/var/lib/agentc/integrator`.
impl Default for Config {
    fn default() -> Self {
        Self {
            credential_file: PathBuf::from("/etc/agentc/integrator-credentials.toml"),
            origin: String::new(),
            allow_insecure_loopback: true,
            projects: Vec::new(),
            state_dir: PathBuf::from("/var/lib/agentc/integrator"),
            poll_seconds: 30,
            result_branch_prefix: "ac/results/".into(),
            checks: ChecksKind::Fake,
            fake_checks_file: PathBuf::from("/var/lib/agentc/integrator/fake-checks.json"),
            required_rules: vec!["non_fast_forward".into(), "required_status_checks".into()],
            roster_path: ".agent-coordinator/roster.toml".into(),
            github: GithubConfig::default(),
        }
    }
}

/// Public GitHub API; no App configured.
impl Default for GithubConfig {
    fn default() -> Self {
        Self {
            api_base: "https://api.github.com".into(),
            app_id: 0,
            installation_id: 0,
            private_key: PathBuf::from("/etc/agentc/integrator-app.pem"),
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

    /// True when a GitHub App is configured (pushes and REST authenticate).
    pub fn has_app(&self) -> bool {
        self.github.app_id != 0 && self.github.installation_id != 0
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
        assert!(toml::from_str::<Config>("state_dirr = '/x'").is_err());
    }

    #[test]
    fn example_file_parses_to_defaults() {
        let text = include_str!("../integrator.example.toml");
        assert_eq!(toml::from_str::<Config>(text).unwrap(), Config::default());
    }
}
