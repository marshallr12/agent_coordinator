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
    /// Stable local credential directory name; independent of checkout location.
    pub project_name: Option<String>,
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
    if let Some(name) = &binding.project_name {
        validate_project_name(name)?;
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

fn validate_project_name(name: &str) -> Result<()> {
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if name.is_empty()
        || name.len() > 120
        || name.trim() != name
        || name.ends_with('.')
        || name
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
        || matches!(stem.as_str(), "" | "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
    {
        bail!(
            "project_name must be a portable directory name without separators, reserved names, or trailing dots/spaces"
        );
    }
    Ok(())
}

fn credential_path(project_name: Option<&str>) -> Result<PathBuf> {
    let Some(name) = project_name else {
        return Ok(coordinator_home()?.join("credentials.toml"));
    };
    validate_project_name(name)?;
    // Validate the existing Windows override restriction for both lookup modes.
    let home = coordinator_home()?;
    #[cfg(windows)]
    let directory = {
        let _ = home;
        ProjectDirs::from("dev", "Agent Coordinator", name)
            .ok_or_else(|| anyhow!("could not determine the project configuration directory"))?
            .config_dir()
            .to_path_buf()
    };
    #[cfg(not(windows))]
    let directory = home.join(name).join("config");
    Ok(directory.join("credentials.toml"))
}

pub fn token(
    origin: &str,
    project_name: Option<&str>,
    allow_insecure_loopback: bool,
) -> Result<String> {
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

    if env::var_os("AGENT_COORDINATOR_MCP_TOKEN").is_some() {
        let token = env::var("AGENT_COORDINATOR_MCP_TOKEN")
            .map_err(|_| anyhow!("AGENT_COORDINATOR_MCP_TOKEN must contain valid Unicode"))?;
        if token.is_empty() {
            bail!("AGENT_COORDINATOR_MCP_TOKEN is empty");
        }
        validate_mcp_url(
            &env::var("AGENT_COORDINATOR_MCP_URL").unwrap_or_default(),
            origin,
        )?;
        return Ok(token);
    }

    let path = credential_path(project_name)?;
    token_from_file(&path, origin, allow_insecure_loopback)
}

fn token_from_file(path: &Path, origin: &str, allow_insecure_loopback: bool) -> Result<String> {
    check_protected_file(path)?;
    let input = fs::read_to_string(path).with_context(|| {
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

pub fn validate_mcp_url(configured: &str, origin: &str) -> Result<()> {
    if configured != format!("{origin}/mcp") {
        bail!(
            "AGENT_COORDINATOR_MCP_URL must equal the repository service origin followed by /mcp"
        );
    }
    Ok(())
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
    fn project_names_are_portable_single_directories() {
        for name in ["Agent Coordinator", "billing", "project-one", "équipe"] {
            assert!(validate_project_name(name).is_ok(), "{name}");
        }
        for name in [
            "",
            ".",
            "..",
            "../other",
            "a/b",
            "a\\b",
            "C:\\secret",
            "a:secret",
            "CON",
            "nul.txt",
            "LPT1",
            "COM9.txt",
            "foo.",
            " foo",
            "foo ",
            "a\nb",
            "a?b",
        ] {
            assert!(validate_project_name(name).is_err(), "{name:?}");
        }
    }

    #[test]
    fn explicit_project_path_does_not_depend_on_checkout() {
        let path = credential_path(Some("Billing")).unwrap();
        assert!(path.ends_with(Path::new("Billing").join("config").join("credentials.toml")));
        assert_ne!(path, credential_path(None).unwrap());
        let binding: RepositoryBinding = toml::from_str(
            "service_url='https://example.test'\nproject_id='p'\nproject_name='Billing'",
        )
        .unwrap();
        assert_eq!(binding.project_name.as_deref(), Some("Billing"));
    }

    #[test]
    fn selected_credential_store_is_origin_bound_and_fails_closed() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("credentials.toml");
        fs::write(&path, "[[credentials]]\norigin='https://one.test'\ntoken='fixture-one'\n[[credentials]]\norigin='https://two.test'\ntoken='fixture-two'").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert_eq!(
            token_from_file(&path, "https://one.test", false).unwrap(),
            "fixture-one"
        );
        assert_eq!(
            token_from_file(&path, "https://two.test", false).unwrap(),
            "fixture-two"
        );
        assert!(token_from_file(&path, "https://missing.test", false).is_err());
        assert!(
            token_from_file(&dir.path().join("missing.toml"), "https://one.test", false).is_err()
        );
        fs::write(&path, "[[credentials]]\norigin='https://one.test'\ntoken='one'\n[[credentials]]\norigin='https://one.test/'\ntoken='two'").unwrap();
        assert!(token_from_file(&path, "https://one.test", false).is_err());
    }

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
