//! Host supervisor configuration. Every entry has an in-code default, so a
//! host runs with no configuration file at all; `/etc/agentc/supervisor.toml`
//! (or `--config`) only overrides what differs on that host.
use crate::verification::Verification;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
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
    /// Loopback address of the CONNECT proxy that enforces egress; launches
    /// get it as `HTTPS_PROXY`, and the firewall lets agents reach only it.
    pub egress_listen: String,
    /// Hosts the proxy tunnels to (`.suffix` allows subdomains).
    pub egress_allow: Vec<String>,
    /// Extra hosts for this host (e.g. the coordinator), added to `egress_allow`.
    pub egress_allow_extra: Vec<String>,
    /// Public `ip:port` preflight connects to directly; reaching it means
    /// the firewall is not enforcing (an IP literal, so no DNS is needed).
    pub egress_probe_target: String,
    /// Name preflight asks the proxy to tunnel; it must answer 403.
    pub egress_probe_blocked_host: String,
    /// Root-owned Rust toolchain: `rustup/` (read-only) and `cargo/bin` proxies.
    pub toolchain_dir: PathBuf,
    /// Required root-owned, read-only Cargo baseline copied into each launch.
    /// Preflight checks its contents and the protected parent directory chain.
    pub cargo_config_seed: PathBuf,
    /// Root-owned, non-setuid Bubblewrap binary required for Claude launches.
    pub bubblewrap: PathBuf,
    /// Exact harness versions a launch refuses to run without.
    pub pinned: Pinned,
    /// Root-owned headless browser offered to verifying reviewers.
    pub browser: PathBuf,
    /// UI verification environments by coordinator project id (plan M2).
    pub verification: BTreeMap<String, Verification>,
    /// Shadow mode: read-only `next` polling and the would-launch log (P3a).
    pub shadow: crate::shadow::ShadowConfig,
    /// The candidate-push helper `launch-root` runs beside each implementer
    /// launch (decision U25).
    pub push_helper: PushHelper,
    /// Live mode: the `run` loop that claims and launches work (P3b).
    pub run: crate::run_loop::RunConfig,
    /// Admission before each claim: kill switch, caps, vendor health (P3b).
    pub health: crate::run_loop::health::HealthConfig,
    /// Host-approved setup command and caches by coordinator project id.
    pub setup: BTreeMap<String, crate::setup::ProjectSetup>,
}

/// Where the candidate-push helper is installed and whom it runs as.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct PushHelper {
    /// Root-owned `agentc-push` binary.
    pub program: PathBuf,
    /// The helper's own configuration file, passed to it as `--config`.
    pub config: PathBuf,
    /// Unprivileged account the helper runs as; it alone can read the push
    /// App key.
    pub user: String,
}

impl Default for PushHelper {
    /// The helper beside the other pinned binaries in `/opt/agentc/bin`
    /// (`/usr/local/bin` is group-writable on some distributions, which
    /// `launch-root` refuses), its configuration in `/etc/agentc`, and the
    /// `agentc-push` account.
    fn default() -> Self {
        Self {
            program: PathBuf::from("/opt/agentc/bin/agentc-push"),
            config: PathBuf::from("/etc/agentc/push.toml"),
            user: "agentc-push".into(),
        }
    }
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
            egress_listen: "127.0.0.1:3128".into(),
            egress_allow: crate::egress::DEFAULT_ALLOW
                .iter()
                .map(|host| host.to_string())
                .collect(),
            egress_allow_extra: Vec::new(),
            egress_probe_target: "1.1.1.1:443".into(),
            egress_probe_blocked_host: "blocked.invalid".into(),
            toolchain_dir: PathBuf::from("/opt/agentc"),
            cargo_config_seed: PathBuf::from("/etc/agentc/cargo-config.toml"),
            bubblewrap: PathBuf::from("/usr/bin/bwrap"),
            pinned: Pinned::default(),
            browser: PathBuf::from("/usr/bin/chromium"),
            verification: BTreeMap::new(),
            shadow: crate::shadow::ShadowConfig::default(),
            push_helper: PushHelper::default(),
            run: crate::run_loop::RunConfig::default(),
            health: crate::run_loop::health::HealthConfig::default(),
            setup: BTreeMap::new(),
        }
    }
}

impl Config {
    /// The proxy URL launches use.
    pub fn egress_proxy_url(&self) -> String {
        format!("http://{}", self.egress_listen)
    }

    /// Every host the egress proxy may tunnel to.
    pub fn egress_hosts(&self) -> Vec<String> {
        let mut hosts = self.egress_allow.clone();
        hosts.extend(self.egress_allow_extra.iter().cloned());
        hosts
    }

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
    fn push_helper_defaults_to_the_pinned_binary_dir() {
        let config = Config::default();
        assert_eq!(
            config.push_helper.program,
            config.bin_dir.join("agentc-push")
        );
        assert_eq!(
            config.push_helper.config,
            Path::new("/etc/agentc/push.toml")
        );
        assert_eq!(config.push_helper.user, "agentc-push");
    }

    #[test]
    fn a_run_binding_is_optional_and_must_be_complete() {
        assert_eq!(Config::default().run.binding, None);
        let text = "[run.binding]\nservice_url = \"http://127.0.0.1:18080\"\nproject_id = \"p\"\n";
        let config: Config = toml::from_str(text).unwrap();
        assert_eq!(config.run.binding.unwrap().project_id, "p");
        assert!(toml::from_str::<Config>("[run.binding]\nproject_id = \"p\"\n").is_err());
        let extra = format!("{text}repository = \"x\"\n");
        assert!(toml::from_str::<Config>(&extra).is_err());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(toml::from_str::<Config>("bin_dirr = '/x'").is_err());
    }
}
