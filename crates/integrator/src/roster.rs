//! The required-check roster sent with each result. Check-run names and
//! workflow paths come from the target repository at T0
//! (`.agent-coordinator/roster.toml`, plan §2.4 "roster from target"); the
//! service's stored identities are the floor that file must cover. Each
//! entry is bound to its workflow file's blob in T0, so a result that edits
//! a required workflow cannot satisfy that check with its own definition.
use crate::checks::RosterCheck;
use crate::git;
use crate::service::ServiceRoster;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RosterFile {
    #[serde(default)]
    required_checks: Vec<RosterCheck>,
}

/// A roster ready to pin: the JSON body and its checks.
pub struct Roster {
    pub json: Value,
    pub checks: Vec<RosterCheck>,
}

/// Builds the roster at `t0`, failing when a service identity is unmapped
/// or a mapped workflow file is missing from T0.
pub fn at_target(mirror: &Path, t0: &str, path: &str, service: &ServiceRoster) -> Result<Roster> {
    let checks = file_checks(mirror, t0, path)?;
    for identity in service_identities(service) {
        if !checks.iter().any(|check| check.identity == identity) {
            bail!("required check {identity:?} has no entry in {path} at {t0}");
        }
    }
    let entries = checks
        .iter()
        .map(|check| entry(mirror, t0, check))
        .collect::<Result<Vec<_>>>()?;
    let json = json!({"revision": service.revision, "required_checks": entries});
    Ok(Roster { json, checks })
}

/// Entries of the roster file at `t0`; an absent file is an empty roster.
fn file_checks(mirror: &Path, t0: &str, path: &str) -> Result<Vec<RosterCheck>> {
    let Some(text) = git::file_at(mirror, t0, path)? else {
        return Ok(Vec::new());
    };
    let file: RosterFile =
        toml::from_str(&text).with_context(|| format!("parse {path} at {t0}"))?;
    Ok(file.required_checks)
}

/// Identities the service requires (its policy's `required_checks`).
fn service_identities(service: &ServiceRoster) -> impl Iterator<Item = &str> {
    service
        .required_checks
        .iter()
        .filter_map(|check| check["identity"].as_str())
}

/// One roster entry with its workflow blob from T0.
fn entry(mirror: &Path, t0: &str, check: &RosterCheck) -> Result<Value> {
    let blob = git::blob_at(mirror, t0, &check.workflow_path)?
        .with_context(|| format!("workflow {} is missing at {t0}", check.workflow_path))?;
    Ok(
        json!({"identity": check.identity, "check_name": check.check_name,
        "workflow_path": check.workflow_path, "workflow_blob": blob}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::testing::{commit, remote};

    const FILE: &str = "[[required_checks]]\nidentity = \"tests\"\ncheck_name = \"Linux tests\"\nworkflow_path = \".github/workflows/ci.yml\"\n";

    fn service(identities: &[&str]) -> ServiceRoster {
        let checks = identities
            .iter()
            .map(|id| json!({"identity": id}))
            .collect();
        ServiceRoster {
            revision: 3,
            required_checks: checks,
        }
    }

    #[test]
    fn roster_binds_workflow_blobs_and_covers_service_identities() {
        let remote = remote();
        commit(&remote.source, ".github/workflows/ci.yml", "on: push\n");
        let t0 = commit(&remote.source, ".agent-coordinator/roster.toml", FILE);
        let path = ".agent-coordinator/roster.toml";
        let roster = at_target(&remote.source, &t0, path, &service(&["tests"])).unwrap();
        assert_eq!(roster.json["revision"], 3);
        assert_eq!(
            roster.json["required_checks"][0]["workflow_blob"]
                .as_str()
                .unwrap()
                .len(),
            40
        );
        assert!(at_target(&remote.source, &t0, path, &service(&["other"])).is_err());
    }

    #[test]
    fn absent_file_is_empty_unless_the_service_requires_checks() {
        let remote = remote();
        let t0 = crate::git::testing::git(&remote.source, &["rev-parse", "HEAD"]);
        let path = ".agent-coordinator/roster.toml";
        assert!(
            at_target(&remote.source, &t0, path, &service(&[]))
                .unwrap()
                .checks
                .is_empty()
        );
        assert!(at_target(&remote.source, &t0, path, &service(&["tests"])).is_err());
    }
}
