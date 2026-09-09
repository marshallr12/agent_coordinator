use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use directories::ProjectDirs;
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryBinding {
    pub service_url: String,
    pub project_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialStore {
    #[serde(default)]
    credentials: Vec<CredentialEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialEntry {
    origin: String,
    token: String,
}

pub fn binding(explicit: Option<&Path>) -> Result<(PathBuf, RepositoryBinding)> {
    let path = match explicit {
        Some(path) => path.to_path_buf(),
        None => find_binding(&env::current_dir().context("read current directory")?).ok_or_else(
            || anyhow!("could not find .agent-coordinator.toml in this directory or its parents"),
        )?,
    };
    let input = fs::read_to_string(&path)
        .with_context(|| format!("read repository binding {}", path.display()))?;
    let binding: RepositoryBinding = toml::from_str(&input)
        .with_context(|| format!("parse repository binding {}", path.display()))?;
    if binding.service_url.trim().is_empty() || binding.project_id.trim().is_empty() {
        bail!("repository binding requires non-empty service_url and project_id");
    }
    Ok((path, binding))
}

fn find_binding(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .map(|directory| directory.join(".agent-coordinator.toml"))
        .find(|candidate| candidate.is_file())
}

pub fn coordinator_home() -> Result<PathBuf> {
    if let Some(value) = env::var_os("AGENT_COORDINATOR_HOME") {
        if value.is_empty() {
            bail!("AGENT_COORDINATOR_HOME must not be empty");
        }
        #[cfg(windows)]
        bail!(
            "AGENT_COORDINATOR_HOME is not supported on Windows because an arbitrary directory may not have a private user ACL"
        );
        #[cfg(not(windows))]
        return Ok(PathBuf::from(value));
    }
    ProjectDirs::from("dev", "Agent Coordinator", "agent-coordinator")
        .map(|dirs| dirs.config_dir().to_path_buf())
        .ok_or_else(|| anyhow!("could not determine the local configuration directory"))
}

pub fn token(origin: &str, allow_insecure_loopback: bool) -> Result<String> {
    if let Ok(token) = env::var("AGENT_COORDINATOR_TOKEN") {
        if token.is_empty() {
            bail!("AGENT_COORDINATOR_TOKEN is empty");
        }
        let configured_origin = env::var("AGENT_COORDINATOR_ORIGIN").map_err(|_| {
            anyhow!("AGENT_COORDINATOR_ORIGIN must be set when AGENT_COORDINATOR_TOKEN is used")
        })?;
        validate_environment_origin(&configured_origin, origin, allow_insecure_loopback)?;
        return Ok(token);
    }

    let path = coordinator_home()?.join("credentials.toml");
    check_protected_file(&path)?;
    let input = fs::read_to_string(&path).with_context(|| {
        format!(
            "no AGENT_COORDINATOR_TOKEN is set and {} could not be read",
            path.display()
        )
    })?;
    let store: CredentialStore = toml::from_str(&input)
        .with_context(|| format!("parse credential configuration {}", path.display()))?;
    let mut matches = store.credentials.into_iter().filter_map(|entry| {
        let normalized =
            coordinator_client::normalize_origin(&entry.origin, allow_insecure_loopback).ok()?;
        (normalized == origin).then_some(entry.token)
    });
    let token = matches.next().ok_or_else(|| {
        anyhow!(
            "no credential is configured for {origin}; set AGENT_COORDINATOR_TOKEN or add it to {}",
            path.display()
        )
    })?;
    if matches.next().is_some() {
        bail!("more than one credential is configured for {origin}");
    }
    if token.is_empty() {
        bail!("the configured credential for {origin} is empty");
    }
    Ok(token)
}

fn validate_environment_origin(
    configured: &str,
    expected: &str,
    allow_insecure_loopback: bool,
) -> Result<()> {
    let configured = coordinator_client::normalize_origin(configured, allow_insecure_loopback)
        .context("validate AGENT_COORDINATOR_ORIGIN")?;
    if configured != expected {
        bail!("AGENT_COORDINATOR_ORIGIN does not match the repository's service origin");
    }
    Ok(())
}

#[cfg(unix)]
fn check_protected_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::metadata(path)
        .with_context(|| format!("inspect credential configuration {}", path.display()))?;
    if metadata.permissions().mode() & 0o077 != 0 {
        bail!(
            "credential configuration {} must not be readable by group or other users (use mode 0600)",
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_protected_file(path: &Path) -> Result<()> {
    if !path.is_file() {
        bail!("credential configuration {} is not a file", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn repository_binding_is_non_secret_and_exact() {
        let valid: RepositoryBinding =
            toml::from_str("service_url = 'https://coordinator.example'\nproject_id = 'p1'\n")
                .unwrap();
        assert_eq!(valid.project_id, "p1");
        assert!(
            toml::from_str::<RepositoryBinding>(
                "service_url = 'https://coordinator.example'\nproject_id = 'p1'\ntoken = 'secret'\n"
            )
            .is_err()
        );
    }

    #[test]
    fn binding_discovery_walks_parents() {
        let directory = tempdir().unwrap();
        let nested = directory.path().join("one/two");
        fs::create_dir_all(&nested).unwrap();
        let expected = directory.path().join(".agent-coordinator.toml");
        fs::write(&expected, "").unwrap();
        assert_eq!(find_binding(&nested), Some(expected));
    }

    #[test]
    fn environment_credential_origin_must_match_binding() {
        assert!(
            validate_environment_origin(
                "https://coordinator.example/",
                "https://coordinator.example",
                false
            )
            .is_ok()
        );
        assert!(
            validate_environment_origin(
                "https://attacker.example",
                "https://coordinator.example",
                false
            )
            .is_err()
        );
    }
}
