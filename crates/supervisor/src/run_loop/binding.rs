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

/// Directory names below the verdict home that are not credential
/// directories: `checkouts` holds the claim checkouts, and stale-credential
/// cleanup never removes it, so a binding must not share it.
const RESERVED_NAMES: [&str; 1] = ["checkouts"];

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
    /// one plain directory name (it becomes a path component) and not a
    /// reserved one.
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
            ensure!(
                !RESERVED_NAMES.contains(&name.as_str()),
                "project_name {name:?} is reserved for the verdict home's own directories"
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

/// `[run.canary_binding]`: a second project the loop claims from alongside
/// its main one, normally the host's canary project (decision U31). It is
/// served by the same coordinator as the main binding (the loop refuses a
/// different origin), with the same implementer and reviewer credentials,
/// from its own repository mirror.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CanaryBinding {
    pub service_url: String,
    pub project_id: String,
    /// The CLI's per-project credential directory name, if any.
    #[serde(default)]
    pub project_name: Option<String>,
    /// The read-only mirror of the canary repository clones start from;
    /// default `<state_dir>/mirror-canary.git`.
    #[serde(default)]
    pub mirror: Option<PathBuf>,
}

impl CanaryBinding {
    /// The repository binding the CLI reads for this project.
    pub fn binding(&self) -> Binding {
        Binding {
            service_url: self.service_url.clone(),
            project_id: self.project_id.clone(),
            project_name: self.project_name.clone(),
        }
    }

    /// The mirror clones of this project start from.
    pub fn mirror_path(&self, config: &Config) -> PathBuf {
        (self.mirror.clone()).unwrap_or_else(|| config.state_dir.join("mirror-canary.git"))
    }

    /// Checks the binding itself, and that it cannot be mistaken for `main`:
    /// another project on the same coordinator.
    pub fn check_against(&self, main: &Binding) -> Result<()> {
        self.binding().check().context("[run.canary_binding]")?;
        ensure!(
            self.project_id != main.project_id,
            "[run.canary_binding] names the main project {}; it must be another project",
            main.project_id
        );
        ensure!(
            same_origin(&self.service_url, &main.service_url),
            "[run.canary_binding] service_url {} differs from the main binding's {}; both projects must share one coordinator",
            self.service_url,
            main.service_url
        );
        Ok(())
    }
}

/// Whether two origins are the same, ignoring case and a trailing slash.
fn same_origin(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.trim().trim_end_matches('/').to_ascii_lowercase();
    norm(a) == norm(b)
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

/// The root-owned copy of the `[run.canary_binding]` binding,
/// `<state_dir>/coordinator-binding-canary.toml`.
pub fn canary_installed_path(config: &Config) -> PathBuf {
    config.state_dir.join("coordinator-binding-canary.toml")
}

/// The installed binding copy for work in `project`: the canary's for the
/// `[run.canary_binding]` project, else the main one.
pub fn installed_path_for(config: &Config, project: &str) -> PathBuf {
    match &config.run.canary_binding {
        Some(canary) if canary.project_id == project => canary_installed_path(config),
        _ => installed_path(config),
    }
}

/// Launch environment for an implementer the loop claimed for, when
/// `[run.binding]` overrides the clone's binding or the task belongs to the
/// `[run.canary_binding]` project: the CLI reads the installed copy, and
/// gets the insecure flag for a permitted loopback `http` origin only.
pub fn launch_env(spec: &LaunchSpec, config: &Config) -> Vec<(String, OsString)> {
    if spec.role != Role::Implementer || spec.task.is_none() {
        return Vec::new();
    }
    let project = spec.project.as_deref().unwrap_or_default();
    let canary = (config.run.canary_binding.as_ref()).filter(|c| c.project_id == project);
    let origin = match (canary, &config.run.binding) {
        (Some(canary), _) => &canary.service_url,
        (None, Some(binding)) => &binding.service_url,
        (None, None) => return Vec::new(),
    };
    let path = installed_path_for(config, project);
    let mut env = vec![(REPO_CONFIG_ENV.into(), path.into())];
    if matches!(
        insecure(origin, config.run.allow_insecure_loopback),
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
    fn a_reserved_project_name_is_refused_at_load() {
        let text = "service_url = \"http://127.0.0.1:18080\"\nproject_id = \"p1\"\nproject_name = \"checkouts\"\n";
        let error = Binding::parse(text).unwrap_err().to_string();
        assert!(error.contains("reserved"), "{error}");
        let mut binding = staging("http://127.0.0.1:18080");
        binding.project_name = Some("checkouts".into());
        assert!(binding.check().is_err());
        binding.project_name = Some("checkouts2".into());
        binding.check().unwrap();
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

    /// A canary binding at `origin`.
    fn canary(origin: &str) -> CanaryBinding {
        CanaryBinding {
            service_url: origin.into(),
            project_id: "canary-1".into(),
            project_name: None,
            mirror: None,
        }
    }

    /// An implementer launch of task `t1` in `project`.
    fn spec_in(project: &str) -> LaunchSpec {
        LaunchSpec {
            project: Some(project.into()),
            ..spec(Role::Implementer)
        }
    }

    #[test]
    fn a_canary_binding_is_another_project_on_the_same_coordinator() {
        let main = staging("https://agents.example.com");
        canary("https://agents.example.com")
            .check_against(&main)
            .unwrap();
        canary("HTTPS://agents.example.com/")
            .check_against(&main)
            .unwrap();
        let other = canary("https://elsewhere.example.com");
        let error = other.check_against(&main).unwrap_err().to_string();
        assert!(error.contains("share one coordinator"), "{error}");
        let mut same = canary("https://agents.example.com");
        same.project_id = main.project_id.clone();
        let error = same.check_against(&main).unwrap_err().to_string();
        assert!(error.contains("another project"), "{error}");
        let mut named = canary("https://agents.example.com");
        named.project_name = Some("checkouts".into());
        assert!(named.check_against(&main).is_err());
    }

    #[test]
    fn a_canary_binding_has_its_own_mirror_and_installed_copy() {
        let mut config = Config::default();
        let mut extra = canary("https://agents.example.com");
        assert_eq!(
            extra.mirror_path(&config),
            config.state_dir.join("mirror-canary.git")
        );
        extra.mirror = Some("/srv/canary.git".into());
        assert_eq!(extra.mirror_path(&config), PathBuf::from("/srv/canary.git"));
        assert_eq!(
            installed_path_for(&config, "canary-1"),
            installed_path(&config)
        );
        config.run.canary_binding = Some(extra);
        assert_eq!(
            installed_path_for(&config, "canary-1"),
            config.state_dir.join("coordinator-binding-canary.toml")
        );
        assert_eq!(installed_path_for(&config, "p1"), installed_path(&config));
    }

    #[test]
    fn a_canary_launch_always_reads_the_installed_canary_copy() {
        let mut config = Config::default();
        config.run.canary_binding = Some(canary("https://agents.example.com"));
        // No `[run.binding]`: the main project's launches keep the clone's
        // own binding, the canary project's never do.
        assert!(launch_env(&spec_in("p1"), &config).is_empty());
        let repo = (
            REPO_CONFIG_ENV.into(),
            canary_installed_path(&config).into(),
        );
        assert_eq!(launch_env(&spec_in("canary-1"), &config), [repo.clone()]);
        assert!(launch_env(&spec(Role::Reviewer), &config).is_empty());
        // With a staging `[run.binding]` the main project reads its own copy.
        config.run.binding = Some(staging("https://agents.example.com"));
        let main = (REPO_CONFIG_ENV.into(), installed_path(&config).into());
        assert_eq!(launch_env(&spec_in("p1"), &config), [main]);
        assert_eq!(launch_env(&spec_in("canary-1"), &config), [repo]);
    }

    #[test]
    fn a_loopback_canary_launch_gets_the_insecure_flag_only_when_allowed() {
        let mut config = Config::default();
        config.run.canary_binding = Some(canary("http://127.0.0.1:18080"));
        assert_eq!(launch_env(&spec_in("canary-1"), &config).len(), 1);
        config.run.allow_insecure_loopback = true;
        let env = launch_env(&spec_in("canary-1"), &config);
        assert_eq!(env[1], (INSECURE_ENV.into(), "true".into()));
    }
}
