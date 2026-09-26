//! Launch preflight (autonomy plan §2.3, B7a): refuse to launch unless the
//! exact containment the profile assumes is in place. Every check reports a
//! human-readable problem instead of failing fast, so one run lists them all.
use crate::clone;
use crate::config::Config;
use crate::profile::{self, Harness, LaunchSpec, Role, run_files};
use crate::role_settings;
use std::path::Path;
use std::process::Command;

/// All problems that block `spec` on this host; empty means launchable.
pub fn check(spec: &LaunchSpec, config: &Config) -> Vec<String> {
    let command = profile::command(spec, config);
    let mut problems = Vec::new();
    problems.extend(account_problem(spec.role, config));
    problems.extend(binary_problems(
        &command.program,
        pinned(spec.harness, config),
    ));
    problems.extend(layout_problems(spec));
    problems.extend(settings_problem(spec));
    match clone::hardening_problems(&spec.clone) {
        Ok(found) => problems.extend(found),
        Err(error) => problems.push(format!("clone {}: {error:#}", spec.clone.display())),
    }
    problems
}

/// The pinned version string for a harness.
fn pinned(harness: Harness, config: &Config) -> &str {
    match harness {
        Harness::Claude => &config.pinned.claude,
        Harness::Codex => &config.pinned.codex,
    }
}

/// The launch must run as the role's dedicated account, never the owner's.
fn account_problem(role: Role, config: &Config) -> Option<String> {
    let current = Command::new("id").arg("-un").output().ok()?;
    let current = String::from_utf8_lossy(&current.stdout).trim().to_owned();
    let expected = role.user(config);
    (current != expected)
        .then(|| format!("running as {current:?}; {role:?} launches run as {expected:?}"))
}

/// The harness binary must exist, be pinned, report the pinned version, and
/// not be writable by the agent account.
fn binary_problems(program: &Path, pinned: &str) -> Vec<String> {
    if !program.exists() {
        return vec![format!("{} is not installed", program.display())];
    }
    let mut problems = writable_problem(program).into_iter().collect::<Vec<_>>();
    if pinned.is_empty() {
        problems.push(format!(
            "no pinned version configured for {}",
            program.display()
        ));
        return problems;
    }
    let reported = Command::new(program).arg("--version").output();
    let reported = reported.map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    if !reported.as_deref().is_ok_and(|text| text.contains(pinned)) {
        problems.push(format!(
            "{} does not report pinned version {pinned:?}",
            program.display()
        ));
    }
    problems
}

/// Binaries must be root-owned and not group- or world-writable.
#[cfg(unix)]
fn writable_problem(program: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(program).ok()?;
    let unsafe_mode = metadata.mode() & 0o022 != 0;
    (metadata.uid() != 0 || unsafe_mode).then(|| {
        format!(
            "{} must be root-owned and not group/world-writable",
            program.display()
        )
    })
}

#[cfg(not(unix))]
fn writable_problem(_program: &Path) -> Option<String> {
    None
}

/// `$RUN` and the clone must be disjoint, and the prompt must exist.
fn layout_problems(spec: &LaunchSpec) -> Vec<String> {
    let mut problems = Vec::new();
    if profile::is_within(&spec.run, &spec.clone) || profile::is_within(&spec.clone, &spec.run) {
        problems.push("the run directory and the clone must not contain each other".into());
    }
    if !spec.run.join(run_files::PROMPT).is_file() {
        problems.push(format!(
            "{} is missing",
            spec.run.join(run_files::PROMPT).display()
        ));
    }
    if spec.role == Role::Reviewer && !spec.run.join(run_files::SCHEMA).is_file() {
        problems.push("reviewer launches need result.schema.json in the run directory".into());
    }
    problems
}

/// Claude launches need the generated role settings, byte-for-byte in meaning.
fn settings_problem(spec: &LaunchSpec) -> Option<String> {
    if spec.harness != Harness::Claude {
        return None;
    }
    let path = spec.run.join(run_files::SETTINGS);
    let installed = std::fs::read_to_string(&path).unwrap_or_default();
    (!role_settings::matches(spec.role, &installed)).then(|| {
        format!(
            "{} differs from the generated {:?} settings",
            path.display(),
            spec.role
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn missing_containment_is_reported_not_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let spec = LaunchSpec {
            role: Role::Reviewer,
            harness: Harness::Claude,
            clone: dir.path().join("clone"),
            run: dir.path().join("clone/run"),
            model: "m".into(),
            effort: "low".into(),
            session_id: Uuid::nil(),
        };
        let config = Config {
            bin_dir: dir.path().into(),
            ..Config::default()
        };
        let problems = check(&spec, &config).join("\n");
        for expected in [
            "launches run as",
            "not installed",
            "must not contain",
            "prompt.md",
            "schema",
            "differs",
            "clone",
        ] {
            assert!(problems.contains(expected), "{expected}: {problems}");
        }
    }
}
