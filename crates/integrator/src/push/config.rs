//! `agentc-push` configuration: the push App's identity and the one
//! repository it may write. The App id, installation id and repository have
//! no defaults, because a wrong guess would mint credentials for the wrong
//! App or repository; every other entry defaults in code.
use agentc_integrator::config::GithubConfig;
use agentc_integrator::github::RepoId;
use anyhow::{Context, Result, ensure};
use coordinator_local::candidate_push::DEFAULT_MAX_BUNDLE_BYTES;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Where `agentc-push serve` reads its configuration unless `--config` says.
pub const DEFAULT_CONFIG_PATH: &str = "/etc/agentc/push.toml";
/// The public GitHub API.
const DEFAULT_API_BASE: &str = "https://api.github.com";
/// The push App's private key on the helper host.
const DEFAULT_PRIVATE_KEY: &str = "/etc/agentc/push-app.pem";

/// Settings for the candidate-push helper.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PushConfig {
    /// GitHub REST API base; must be HTTPS, since the App JWT is sent to it.
    #[serde(default = "default_api_base")]
    pub api_base: String,
    /// The push App's id.
    pub app_id: u64,
    /// The push App's installation on the repository.
    pub installation_id: u64,
    /// PEM private key, readable by the helper's uid only.
    #[serde(default = "default_private_key")]
    pub private_key: PathBuf,
    /// `https://github.com/<owner>/<repo>.git`, the only repository written.
    pub repository: String,
    /// The largest candidate bundle accepted, in bytes.
    #[serde(default = "default_max_bundle_bytes")]
    pub max_bundle_bytes: u64,
}

/// Serde default for [`PushConfig::api_base`].
fn default_api_base() -> String {
    DEFAULT_API_BASE.into()
}

/// Serde default for [`PushConfig::private_key`].
fn default_private_key() -> PathBuf {
    PathBuf::from(DEFAULT_PRIVATE_KEY)
}

/// Serde default for [`PushConfig::max_bundle_bytes`].
fn default_max_bundle_bytes() -> u64 {
    DEFAULT_MAX_BUNDLE_BYTES
}

impl PushConfig {
    /// Reads, parses and validates the file at `path`.
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("load {}", path.display()))
    }

    /// Parses and validates configuration text.
    pub fn parse(text: &str) -> Result<Self> {
        let config: Self = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    /// Requires a configured App, an HTTPS API base, a non-zero bundle limit
    /// and a GitHub HTTPS repository URL.
    fn validate(&self) -> Result<()> {
        ensure!(
            self.app_id != 0 && self.installation_id != 0,
            "app_id and installation_id must be set"
        );
        ensure!(
            self.api_base.starts_with("https://"),
            "api_base must be an https:// URL"
        );
        ensure!(
            self.max_bundle_bytes > 0,
            "max_bundle_bytes must be positive"
        );
        self.repository_id().map(drop)
    }

    /// The repository's owner and name, accepted only from
    /// `https://github.com/<owner>/<name>[.git]` with each part made of
    /// letters, digits, `.`, `_` and `-`.
    pub fn repository_id(&self) -> Result<RepoId> {
        let id = RepoId::from_url(&self.repository)
            .context("repository must be https://github.com/<owner>/<repo>.git")?;
        ensure!(
            plain_name(&id.owner) && plain_name(&id.name),
            "repository owner and name may hold only letters, digits, '.', '_' and '-'"
        );
        Ok(id)
    }

    /// The canonical remote URL Git pushes to.
    pub fn remote_url(&self) -> Result<String> {
        let id = self.repository_id()?;
        Ok(format!("https://github.com/{}/{}.git", id.owner, id.name))
    }

    /// The App settings in the form [`agentc_integrator::github::GithubApp`]
    /// takes.
    pub fn github(&self) -> GithubConfig {
        GithubConfig {
            api_base: self.api_base.clone(),
            app_id: self.app_id,
            installation_id: self.installation_id,
            private_key: self.private_key.clone(),
        }
    }
}

/// Whether `part` is a GitHub owner or repository name without any URL
/// syntax: non-empty, of `[A-Za-z0-9._-]`, and not `.` or `..`.
fn plain_name(part: &str) -> bool {
    let allowed = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-');
    !part.is_empty() && part != "." && part != ".." && part.bytes().all(allowed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The required entries with `repository` set to `url`.
    fn minimal(url: &str) -> String {
        format!("app_id = 7\ninstallation_id = 9\nrepository = \"{url}\"\n")
    }

    #[test]
    fn minimal_file_takes_the_defaults() {
        let config = PushConfig::parse(&minimal("https://github.com/o/r.git")).unwrap();
        assert_eq!(config.api_base, "https://api.github.com");
        assert_eq!(config.private_key, Path::new("/etc/agentc/push-app.pem"));
        assert_eq!(config.max_bundle_bytes, DEFAULT_MAX_BUNDLE_BYTES);
        assert_eq!(config.remote_url().unwrap(), "https://github.com/o/r.git");
        assert_eq!(config.repository_id().unwrap().name, "r");
    }

    #[test]
    fn example_file_parses() {
        let text = include_str!("../../push.example.toml");
        let config = PushConfig::parse(text).unwrap();
        let defaults = PushConfig::parse(&minimal(&config.repository)).unwrap();
        assert_eq!(
            PushConfig {
                app_id: 7,
                installation_id: 9,
                ..config
            },
            defaults
        );
    }

    #[test]
    fn required_entries_and_unknown_keys_are_refused() {
        assert!(PushConfig::parse("app_id = 7\ninstallation_id = 9\n").is_err());
        assert!(PushConfig::parse("repository = \"https://github.com/o/r.git\"\n").is_err());
        let zero = "app_id = 0\ninstallation_id = 9\nrepository = \"https://github.com/o/r\"";
        assert!(PushConfig::parse(zero).is_err());
        let typo = minimal("https://github.com/o/r.git") + "privte_key = \"/k\"\n";
        assert!(PushConfig::parse(&typo).is_err());
        let http_api = minimal("https://github.com/o/r.git") + "api_base = \"http://x\"\n";
        assert!(PushConfig::parse(&http_api).is_err());
    }

    #[test]
    fn only_github_https_repositories_are_accepted() {
        for url in [
            "http://github.com/o/r.git",
            "git@github.com:o/r.git",
            "https://gitlab.com/o/r.git",
            "https://github.com.evil.example/o/r.git",
            "https://user@github.com/o/r.git",
            "https://github.com/o/r?x=1",
            "https://github.com/o/../r.git",
            "https://github.com/o",
            "/srv/git/r.git",
        ] {
            assert!(PushConfig::parse(&minimal(url)).is_err(), "{url} accepted");
        }
        let lenient = PushConfig::parse(&minimal("https://github.com/o/r/")).unwrap();
        assert_eq!(lenient.remote_url().unwrap(), "https://github.com/o/r.git");
    }
}
