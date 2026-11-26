//! Step 7: prove the next session will work, without claiming work or
//! registering a session.
//!
//! Checks 1 to 4 are hard: a failure exits 6 naming the check. The files
//! already written stay (they are correct), and running adopt again repairs
//! and verifies again. The rest are warnings and advice.

use std::path::{Path, PathBuf};

use serde_json::json;

use super::files::{self, Readiness};
use super::host::Host;
use super::preflight::Preflight;
use super::report::{Reporter, Status};
use super::service::{self, Project, Service};
use super::{Harnesses, Options, failure};
use crate::Failure;
use crate::config::{self, RepositoryBinding};

/// Advice printed after every successful verification.
const SANDBOX_HINT: &str = "If the assistant runs in a sandbox that hides $HOME/.config or the network, \
give it the credential through AGENT_COORDINATOR_TOKEN and AGENT_COORDINATOR_ORIGIN in its \
environment, or point --state-dir inside the allowed area (see the CLI guide).";

/// Runs every check and reports the outcome.
pub async fn run(
    pre: &Preflight,
    project: &Project,
    harnesses: Harnesses,
    options: &Options,
    host: &dyn Host,
    reporter: &mut Reporter,
) -> Result<(), Failure> {
    let binding = check_binding(&pre.root, &pre.service_url, project, options)?;
    check_service(pre, &binding, project, options, host).await?;
    check_harnesses(&pre.root, harnesses, host, reporter);
    reporter.event(
        "verify",
        Status::Done,
        "binding, credential, project and compatibility confirmed",
        json!({"sandbox_hint": SANDBOX_HINT}),
    );
    reporter.text(&[format!("Note: {SANDBOX_HINT}")]);
    Ok(())
}

/// Check 1: the CLI's own loader finds the same binding from the root and from a nested directory.
fn check_binding(
    root: &Path,
    service_url: &str,
    project: &Project,
    options: &Options,
) -> Result<RepositoryBinding, Failure> {
    let expected = (service_url, project.id.as_str());
    load_expected(&nested_directory(root), expected, options)?;
    load_expected(root, expected, options)
}

/// Loads the binding a session started in `start` would use and compares it with what was written.
fn load_expected(
    start: &Path,
    expected: (&str, &str),
    options: &Options,
) -> Result<RepositoryBinding, Failure> {
    let loaded =
        config::binding_from(start).map_err(|error| unverified("binding", format!("{error:#}")));
    let (_, binding) = loaded?;
    let allow_loopback = options.allow_insecure_loopback;
    let origin = coordinator_client::normalize_origin(&binding.service_url, allow_loopback).ok();
    if (origin.as_deref(), binding.project_id.as_str()) == (Some(expected.0), expected.1) {
        return Ok(binding);
    }
    let (url, id) = (&binding.service_url, &binding.project_id);
    Err(unverified(
        "binding",
        format!("from {} it names {url} project {id}", start.display()),
    ))
}

/// Picks a subdirectory to load the binding from, as a session started deeper in the tree would.
fn nested_directory(root: &Path) -> PathBuf {
    let entries = std::fs::read_dir(root).into_iter().flatten().flatten();
    entries
        .map(|entry| entry.path())
        .find(|path| path.is_dir() && path.file_name().is_some_and(|name| name != ".git"))
        .unwrap_or_else(|| root.to_path_buf())
}

/// Checks 2 to 4: the credential selected for this binding works, the project
/// is listed, and the client is still compatible.
async fn check_service(
    pre: &Preflight,
    binding: &RepositoryBinding,
    project: &Project,
    options: &Options,
    host: &dyn Host,
) -> Result<(), Failure> {
    let allow_loopback = options.allow_insecure_loopback;
    let selected = host.token(&pre.service_url, binding.project_name.as_deref());
    let token = selected.map_err(|error| unverified("credential", error.to_string()))?;
    let service = Service::new(&pre.service_url, token, allow_loopback)?;
    let refused = |_| unverified("credential", "the service did not accept it".to_owned());
    service.actor().await.map_err(refused)?;
    let unreadable = |_| unverified("project", "the project list could not be read".to_owned());
    let projects = service.projects().await.map_err(unreadable)?;
    if !projects.iter().any(|listed| listed.id == project.id) {
        return Err(unverified(
            "project",
            format!("{} is not listed by the service", project.id),
        ));
    }
    let info = service::info(&pre.service_url, allow_loopback).await?;
    host.compatibility(&info).map(|_| ())
}

/// Check 5: each assistant has instructions to read and is installed (warnings only).
fn check_harnesses(root: &Path, harnesses: Harnesses, host: &dyn Host, reporter: &mut Reporter) {
    let selected = [
        ("claude", harnesses.claude, files::CLAUDE_FILE),
        ("codex", harnesses.codex, files::AGENTS_FILE),
    ];
    for (name, _, file) in selected.into_iter().filter(|(_, wanted, _)| *wanted) {
        if files::readiness(root, name == "claude") == Readiness::Missing {
            reporter.warn(
                "verify",
                format!(
                    "{file} has no coordinator instructions for {name}; see the earlier message."
                ),
            );
        }
        if host.find_program(name).is_none() {
            reporter.warn("verify", format!("{name} is not on PATH."));
        }
    }
}

/// The exit-6 failure naming the check that did not hold.
fn unverified(check: &str, detail: String) -> Failure {
    failure(
        6,
        "verification_failed",
        format!(
            "Verification failed at the {check} check: {detail}. Run adopt again to repair and re-verify."
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adopt::AdoptArgs;
    use crate::adopt::testing::TestRepo;

    /// Options for a run with no flags.
    fn options() -> Options {
        Options {
            args: AdoptArgs::default(),
            allow_insecure_loopback: false,
            start: ".".into(),
        }
    }

    /// A project named "Billing" with the given ID.
    fn project(id: &str) -> Project {
        Project {
            id: id.into(),
            name: "Billing".into(),
            repository_url: String::new(),
            target_branch: "main".into(),
        }
    }

    #[test]
    fn the_binding_must_load_from_the_root_and_a_nested_directory() {
        let repo = TestRepo::new();
        std::fs::create_dir_all(repo.root().join("src/deep")).unwrap();
        repo.write(
            files::BINDING_FILE,
            &files::render_binding("https://agents.example", "p1"),
        );
        assert_ne!(nested_directory(repo.root()), repo.root());
        assert!(
            check_binding(
                repo.root(),
                "https://agents.example",
                &project("p1"),
                &options()
            )
            .is_ok()
        );
        let wrong = check_binding(
            repo.root(),
            "https://agents.example",
            &project("p2"),
            &options(),
        )
        .unwrap_err();
        assert_eq!(wrong.exit, 6);
        assert!(
            wrong.output["error"]["message"]
                .as_str()
                .unwrap()
                .contains("binding check")
        );
    }

    #[test]
    fn a_missing_binding_fails_verification() {
        let repo = TestRepo::new();
        let missing = check_binding(
            repo.root(),
            "https://agents.example",
            &project("p1"),
            &options(),
        )
        .unwrap_err();
        assert_eq!(missing.exit, 6);
    }
}
