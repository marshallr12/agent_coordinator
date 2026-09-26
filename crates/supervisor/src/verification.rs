//! Per-project UI verification environments (autonomy plan §2.3, M2).
//!
//! A project may name an environment its reviewers check UI acceptance
//! criteria against (for this project, the staging coordinator) and whether
//! the host's headless browser is offered. The environment's test login is a
//! host-owned secret only the reviewer account can read, at
//! `<state_dir>/rev/verification/<project>.json`; the supervisor never copies
//! it, it only points `$RUN/verification.json` at it. Implementer launches
//! and projects without an entry get nothing.
use crate::config::Config;
use crate::profile::{LaunchSpec, Role, run_files};
use serde::Deserialize;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::path::PathBuf;

/// One project's verification environment (`[verification.<project-id>]`).
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Verification {
    /// Base URL under test; loopback, or a host in the egress allowlist.
    pub url: String,
    /// Offer the host's headless browser as `CHROME_BIN` (default on).
    #[serde(default = "enabled")]
    pub browser: bool,
}

/// Serde default for `browser`.
fn enabled() -> bool {
    true
}

/// The environment a launch verifies against: reviewer launches of a project
/// that has an entry, nothing otherwise.
pub fn for_launch<'a>(spec: &LaunchSpec, config: &'a Config) -> Option<&'a Verification> {
    if spec.role != Role::Reviewer {
        return None;
    }
    config.verification.get(spec.project.as_deref()?)
}

/// Where the host owner installs `project`'s test login (reviewer-only, 0600).
pub fn credential_file(config: &Config, project: &str) -> PathBuf {
    config
        .state_dir
        .join(Role::Reviewer.slug())
        .join("verification")
        .join(format!("{project}.json"))
}

/// Contents of `$RUN/verification.json`, or `None` when the launch has none.
pub fn describe(spec: &LaunchSpec, config: &Config) -> Option<Value> {
    let verification = for_launch(spec, config)?;
    let project = spec.project.as_deref()?;
    Some(json!({
        "project_id": project,
        "url": verification.url,
        "credential_file": credential_file(config, project),
        "browser": verification.browser.then_some(&config.browser),
    }))
}

/// Environment added to a verifying launch: where to find the description
/// and, when offered, the browser (the repo's UI scripts read `CHROME_BIN`).
pub fn environment(spec: &LaunchSpec, config: &Config) -> Vec<(String, OsString)> {
    let Some(verification) = for_launch(spec, config) else {
        return Vec::new();
    };
    let mut env = vec![(
        "AGENTC_VERIFICATION".to_string(),
        spec.run.join(run_files::VERIFICATION).into_os_string(),
    )];
    if verification.browser {
        env.push(("CHROME_BIN".into(), config.browser.clone().into_os_string()));
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::Harness;

    /// A launch spec for `role` on project "p1".
    fn spec(role: Role) -> LaunchSpec {
        LaunchSpec {
            role,
            harness: Harness::Codex,
            clone: "/w/clone".into(),
            run: "/w/run".into(),
            model: "m".into(),
            effort: "low".into(),
            session_id: uuid::Uuid::nil(),
            project: Some("p1".into()),
        }
    }

    /// A config whose project "p1" verifies against a loopback URL.
    fn config() -> Config {
        toml::from_str("[verification.p1]\nurl = \"http://127.0.0.1:18080\"").unwrap()
    }

    #[test]
    fn only_reviewers_of_configured_projects_verify() {
        let config = config();
        assert!(describe(&spec(Role::Reviewer), &config).is_some());
        assert!(describe(&spec(Role::Implementer), &config).is_none());
        let other = LaunchSpec {
            project: Some("p2".into()),
            ..spec(Role::Reviewer)
        };
        assert!(environment(&other, &config).is_empty());
    }

    #[test]
    fn description_points_at_the_login_without_copying_it() {
        let described = describe(&spec(Role::Reviewer), &config()).unwrap();
        assert_eq!(
            described["credential_file"],
            "/var/lib/agentc/rev/verification/p1.json"
        );
        assert_eq!(described["browser"], "/usr/bin/chromium");
        let names: Vec<_> = environment(&spec(Role::Reviewer), &config())
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(names, ["AGENTC_VERIFICATION", "CHROME_BIN"]);
    }

    #[test]
    fn browser_can_be_switched_off() {
        let config: Config =
            toml::from_str("[verification.p1]\nurl = \"http://127.0.0.1:18080\"\nbrowser = false")
                .unwrap();
        let described = describe(&spec(Role::Reviewer), &config).unwrap();
        assert!(described["browser"].is_null());
        assert_eq!(environment(&spec(Role::Reviewer), &config).len(), 1);
    }
}
