//! The repository binding the loop works under: the coordinator origin, the
//! project, and the optional `project_name` credential selector. By default
//! it is the mirror branch's `.agent-coordinator.toml`; `[run.binding]` in
//! the root-owned host configuration overrides it (a staging coordinator),
//! so the role-writable clone is never edited or trusted for it.
//!
//! Plain HTTP is allowed only to a loopback origin and only with `[run]
//! allow_insecure_loopback`; an `https` origin never gets the insecure flag.
use crate::config::Config;
use crate::profile::{LaunchSpec, Role};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::PathBuf;

/// The variable that lets the coordinator CLI use plain HTTP to loopback.
pub const INSECURE_ENV: &str = "AGENT_COORDINATOR_ALLOW_INSECURE_LOOPBACK";
/// The variable naming the repository binding the coordinator CLI reads.
pub const REPO_CONFIG_ENV: &str = "AGENT_COORDINATOR_REPO_CONFIG";

/// One repository binding, as `.agent-coordinator.toml` holds it.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub service_url: String,
    pub project_id: String,
    /// The CLI's per-project credential directory name, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
}

impl Binding {
    /// Parses and checks binding text (see [`Binding::check`]).
    pub fn parse(text: &str) -> Result<Self> {
        let binding: Self = toml::from_str(text).context("parse the repository binding")?;
        binding.check()?;
        Ok(binding)
    }

    /// Requires a non-empty origin and project, and a `project_name` that is
    /// one plain directory name (it becomes a path component).
    pub fn check(&self) -> Result<()> {
        ensure!(
            !self.service_url.trim().is_empty() && !self.project_id.trim().is_empty(),
            "the repository binding needs a service_url and a project_id"
        );
        if let Some(name) = &self.project_name {
            ensure!(
                plain_name(name),
                "project_name {name:?} must be letters, digits, ' ', '.', '_' or '-', not leading '.'"
            );
        }
        Ok(())
    }

    /// The binding as TOML, for the root-owned copy role commands read.
    pub fn to_toml(&self) -> Result<String> {
        toml::to_string(self).context("render the repository binding")
    }

    /// Where the CLI looks for credentials below its `AGENT_COORDINATOR_HOME`:
    /// `credentials.toml`, or `<project_name>/config/credentials.toml`.
    pub fn credentials(&self) -> PathBuf {
        let file = PathBuf::from("credentials.toml");
        match &self.project_name {
            Some(name) => PathBuf::from(name).join("config").join(file),
            None => file,
        }
    }
}

/// Whether `name` is a single, portable directory name.
fn plain_name(name: &str) -> bool {
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-');
    !name.is_empty()
        && name.len() <= 120
        && name.trim() == name
        && !name.starts_with('.')
        && !name.ends_with('.')
        && name.chars().all(allowed)
}

/// Whether coordinator calls to `origin` need the insecure-loopback flag:
/// never for `https`; for plain `http` only to a loopback address, and only
/// when `allowed`. Any other origin is refused.
pub fn insecure(origin: &str, allowed: bool) -> Result<bool> {
    let scheme = origin
        .split_once("://")
        .map(|(s, _)| s.to_ascii_lowercase());
    match scheme.as_deref() {
        Some("https") => Ok(false),
        Some("http") => {
            let loopback = crate::relay::staging_address(origin)?.is_some();
            ensure!(
                loopback,
                "{origin} uses plain http to a host that is not loopback; only https is allowed there"
            );
            ensure!(
                allowed,
                "{origin} is plain http on loopback; set [run] allow_insecure_loopback = true to use it"
            );
            Ok(true)
        }
        _ => bail!("coordinator origin {origin:?} must be an http or https URL"),
    }
}

/// The root-owned copy of the binding that every role CLI command uses,
/// `<state_dir>/coordinator-binding.toml`.
pub fn installed_path(config: &Config) -> PathBuf {
    config.state_dir.join("coordinator-binding.toml")
}

/// Launch environment for an implementer the loop claimed for, when
/// `[run.binding]` overrides the clone's binding: the CLI reads the
/// installed copy, and gets the insecure flag for a permitted loopback
/// `http` origin only.
pub fn launch_env(spec: &LaunchSpec, config: &Config) -> Vec<(String, OsString)> {
    let Some(binding) = &config.run.binding else {
        return Vec::new();
    };
    if spec.role != Role::Implementer || spec.task.is_none() {
        return Vec::new();
    }
    let mut env = vec![(REPO_CONFIG_ENV.into(), installed_path(config).into())];
    if matches!(
        insecure(&binding.service_url, config.run.allow_insecure_loopback),
        Ok(true)
    ) {
        env.push((INSECURE_ENV.into(), "true".into()));
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::Harness;

    /// A staging binding at `origin`.
    fn staging(origin: &str) -> Binding {
        Binding {
            service_url: origin.into(),
            project_id: "p1".into(),
            project_name: None,
        }
    }

    /// An implementer launch of task `t1`.
    fn spec(role: Role) -> LaunchSpec {
        LaunchSpec {
            role,
            harness: Harness::Claude,
            clone: "/w/clone".into(),
            run: "/w/run".into(),
            model: "m".into(),
            effort: "low".into(),
            session_id: uuid::Uuid::nil(),
            project: Some("p1".into()),
            task: Some("t1".into()),
            push_socket: None,
        }
    }

    #[test]
    fn https_never_gets_the_insecure_flag() {
        for allowed in [false, true] {
            assert!(!insecure("https://agents.example.com", allowed).unwrap());
            assert!(!insecure("HTTPS://127.0.0.1:18443", allowed).unwrap());
        }
    }

    #[test]
    fn plain_http_needs_loopback_and_the_opt_in() {
        assert!(insecure("http://127.0.0.1:18080", true).unwrap());
        assert!(insecure("http://localhost:18080/", true).unwrap());
        assert!(insecure("http://127.0.0.1:18080", false).is_err());
        for refused in ["http://10.0.0.5:18080", "http://coordinator.example.com"] {
            let error = insecure(refused, true).unwrap_err().to_string();
            assert!(error.contains("not loopback"), "{refused}: {error}");
        }
        assert!(insecure("ftp://127.0.0.1:18080", true).is_err());
        assert!(insecure("127.0.0.1:18080", true).is_err());
    }

    #[test]
    fn project_names_select_a_credential_directory_and_cannot_escape() {
        let mut binding = staging("http://127.0.0.1:18080");
        assert_eq!(binding.credentials(), PathBuf::from("credentials.toml"));
        binding.project_name = Some("Staging Pilot".into());
        binding.check().unwrap();
        let named = PathBuf::from("Staging Pilot/config/credentials.toml");
        assert_eq!(binding.credentials(), named);
        for bad in ["", "..", ".hidden", "a/b", "a\\b", " pad", "x\n"] {
            binding.project_name = Some(bad.into());
            assert!(binding.check().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn bindings_round_trip_and_reject_unknown_or_empty_fields() {
        let text =
            "service_url = \"http://127.0.0.1:18080\"\nproject_id = \"p1\"\nproject_name = \"S\"\n";
        let binding = Binding::parse(text).unwrap();
        assert_eq!(
            Binding::parse(&binding.to_toml().unwrap()).unwrap(),
            binding
        );
        assert!(!staging("x").to_toml().unwrap().contains("project_name"));
        assert!(Binding::parse("service_url = \"x\"\nproject_id = \"p\"\nextra = 1").is_err());
        assert!(Binding::parse("service_url = \"\"\nproject_id = \"p\"").is_err());
    }

    #[test]
    fn only_an_overridden_loopback_implementer_launch_gets_the_staging_env() {
        let mut config = Config::default();
        assert!(launch_env(&spec(Role::Implementer), &config).is_empty());
        config.run.binding = Some(staging("http://127.0.0.1:18080"));
        config.run.allow_insecure_loopback = true;
        let env = launch_env(&spec(Role::Implementer), &config);
        let repo = (REPO_CONFIG_ENV.into(), installed_path(&config).into());
        assert_eq!(env, [repo.clone(), (INSECURE_ENV.into(), "true".into())]);
        assert!(launch_env(&spec(Role::Reviewer), &config).is_empty());
        config.run.binding = Some(staging("https://staging.example.com"));
        assert_eq!(launch_env(&spec(Role::Implementer), &config), [repo]);
    }
}
