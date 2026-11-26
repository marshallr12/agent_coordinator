//! Step 1: checks that must pass before anything is asked, opened or written.
//!
//! Each failure names a concrete fix. Nothing in this step touches the disk.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::host::Host;
use super::report::{Reporter, Status};
use super::service::{self, Project, Service};
use super::{DEFAULT_SERVICE_URL, Options, failure, failure_with, files, git};
use crate::config::{self, RepositoryBinding};
use crate::{Failure, client_failure};

/// What an existing `.agent-coordinator.toml` means for this run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindingState {
    /// No binding yet: a normal first run.
    Absent,
    /// A binding for this service whose project exists: finish or re-verify the work.
    Repair(Project),
    /// A binding that points elsewhere, which `--replace-binding` allowed overwriting.
    Replace,
}

/// The facts established by preflight that later steps rely on.
pub struct Preflight {
    /// The work tree root; every file adopt writes lives under it.
    pub root: PathBuf,
    /// The service origin in normal form (no trailing slash).
    pub service_url: String,
    /// Every unarchived project the credential can see.
    pub projects: Vec<Project>,
    pub binding: BindingState,
}

/// Runs every preflight check in order and reports the result.
pub async fn run(
    options: &Options,
    host: &mut dyn Host,
    reporter: &mut Reporter,
) -> Result<Preflight, Failure> {
    let root = repository_root(&options.start)?;
    let existing = existing_binding(&root)?;
    let service_url = service_url(options, existing.as_ref())?;
    check_compatibility(&service_url, options, host).await?;
    let service = authenticate(&service_url, existing.as_ref(), options, host).await?;
    let projects = service.projects().await?;
    let binding = binding_state(existing.as_ref(), &service_url, &projects, options)?;
    warn_environment(&root, host, reporter);
    let summary = format!("{} with {service_url}", root.display());
    let mode = json!({"repository": root, "service_url": service_url, "repair": matches!(binding, BindingState::Repair(_))});
    reporter.event("preflight", Status::Done, &summary, mode);
    Ok(Preflight {
        root,
        service_url,
        projects,
        binding,
    })
}

/// Finds the work tree root and refuses a repository in the middle of a merge or rebase.
fn repository_root(start: &Path) -> Result<PathBuf, Failure> {
    let root =
        git::toplevel(start).map_err(|error| failure(2, "not_a_repository", error.to_string()))?;
    if let Some(operation) = git::operation_in_progress(&root) {
        return Err(failure(
            6,
            "repository_busy",
            format!(
                "A {operation} is in progress in {}; finish or abort it, then run adopt again.",
                root.display()
            ),
        ));
    }
    Ok(root)
}

/// Reads the binding at the repository root, if there is one; an invalid file stops the run.
fn existing_binding(root: &Path) -> Result<Option<RepositoryBinding>, Failure> {
    let path = root.join(files::BINDING_FILE);
    if std::fs::symlink_metadata(&path).is_err() {
        return Ok(None);
    }
    match config::binding(Some(&path)) {
        Ok((_, binding)) => Ok(Some(binding)),
        Err(error) => Err(failure(
            2,
            "invalid_binding",
            format!(
                "{} cannot be used: {error:#}. Fix or remove it, then run adopt again.",
                files::BINDING_FILE
            ),
        )),
    }
}

/// Chooses the service: the flag, else the existing binding's, else the default.
///
/// Falling back to the binding keeps a bare re-run in repair mode for a
/// repository that was adopted against another service.
fn service_url(options: &Options, existing: Option<&RepositoryBinding>) -> Result<String, Failure> {
    let chosen = options
        .args
        .service_url
        .as_deref()
        .or(existing.map(|binding| binding.service_url.as_str()))
        .unwrap_or(DEFAULT_SERVICE_URL);
    coordinator_client::normalize_origin(chosen, options.allow_insecure_loopback)
        .map_err(client_failure)
}

/// Fetches the anonymous service description and stops (exit 6) on an incompatible client.
async fn check_compatibility(
    service_url: &str,
    options: &Options,
    host: &dyn Host,
) -> Result<(), Failure> {
    let info = service::info(service_url, options.allow_insecure_loopback).await?;
    host.compatibility(&info).map(|_| ())
}

/// What a "no usable credential" message needs: the service and where its credential belongs.
struct CredentialSetup<'a> {
    service_url: &'a str,
    allow_loopback: bool,
    location: String,
}

impl CredentialSetup<'_> {
    /// Builds the exit-3 failure, with the service's own setup help attached.
    async fn failure(&self, reason: &str) -> Failure {
        service::credential_failure(
            self.service_url,
            self.allow_loopback,
            reason,
            &self.location,
        )
        .await
    }

    /// The failure for a workstation that holds no credential for this service.
    async fn missing(&self, error: &anyhow::Error) -> Failure {
        let reason = format!("No usable credential for {} ({error})", self.service_url);
        self.failure(&reason).await
    }

    /// The failure for a credential the service will not accept for this work.
    async fn rejected(&self, problem: &str) -> Failure {
        self.failure(&format!(
            "The credential for {} {problem}",
            self.service_url
        ))
        .await
    }
}

/// Loads the credential the CLI already holds and proves the service accepts it.
async fn authenticate(
    service_url: &str,
    existing: Option<&RepositoryBinding>,
    options: &Options,
    host: &dyn Host,
) -> Result<Service, Failure> {
    let allow_loopback = options.allow_insecure_loopback;
    let location = host.credential_location();
    let setup = CredentialSetup {
        service_url,
        allow_loopback,
        location,
    };
    let project_name = existing.and_then(|binding| binding.project_name.as_deref());
    let token = match host.token(service_url, project_name) {
        Ok(token) => token,
        Err(error) => return Err(setup.missing(&error).await),
    };
    let service = Service::new(service_url, token, allow_loopback)?;
    match service.actor().await {
        Ok(actor) if is_agent_writer(&actor) => Ok(service),
        Ok(_) => Err(setup.rejected("lacks write access as an agent").await),
        Err(error) if error.exit == 3 => Err(setup.rejected("was rejected").await),
        Err(error) => Err(error),
    }
}

/// True for an agent actor whose credential may write (create tasks, checkpoint, submit).
fn is_agent_writer(actor: &Value) -> bool {
    actor["kind"] == "agent" && actor["credential_attributes"]["access"] == "write"
}

/// Decides between a first run, repair mode, replacement and refusal (exit 5).
fn binding_state(
    existing: Option<&RepositoryBinding>,
    service_url: &str,
    projects: &[Project],
    options: &Options,
) -> Result<BindingState, Failure> {
    let Some(binding) = existing else {
        return Ok(BindingState::Absent);
    };
    let same_target = targets_binding(binding, service_url, options);
    let project = projects
        .iter()
        .find(|project| project.id == binding.project_id);
    match project {
        Some(project) if same_target => Ok(BindingState::Repair(project.clone())),
        _ if options.args.replace_binding => Ok(BindingState::Replace),
        _ => Err(binding_conflict(binding, service_url, same_target, options)),
    }
}

/// True when this run targets the service and project the binding already names.
fn targets_binding(binding: &RepositoryBinding, service_url: &str, options: &Options) -> bool {
    let allow_loopback = options.allow_insecure_loopback;
    let bound_origin =
        coordinator_client::normalize_origin(&binding.service_url, allow_loopback).ok();
    let wanted_project = options.args.project.as_deref();
    bound_origin.as_deref() == Some(service_url)
        && wanted_project.is_none_or(|id| id == binding.project_id)
}

/// The exit-5 failure for a binding that points somewhere else, showing old and new values.
///
/// `missing` says the binding matches this run but its project is gone from the service.
fn binding_conflict(
    binding: &RepositoryBinding,
    service_url: &str,
    missing: bool,
    options: &Options,
) -> Failure {
    let project = options.args.project.as_deref();
    let why = match (missing, project) {
        (true, _) => "its project was not found on the service (deleted or archived)".to_owned(),
        (false, Some(id)) => format!("this run targets {service_url}, project {id}"),
        (false, None) => format!("this run targets {service_url}"),
    };
    let (file, old_url, old_id) = (
        files::BINDING_FILE,
        &binding.service_url,
        &binding.project_id,
    );
    let fix = "Pass --replace-binding to overwrite it.";
    let message = format!("{file} points to {old_url}, project {old_id}, but {why}. {fix}");
    let old = json!({"service_url": old_url, "project_id": old_id});
    let new = json!({"service_url": service_url, "project_id": project});
    failure_with(
        5,
        "binding_conflict",
        message,
        json!({"old": old, "new": new}),
    )
}

/// Warns about things that do not stop the run but would surprise the next session.
fn warn_environment(root: &Path, host: &dyn Host, reporter: &mut Reporter) {
    if host.find_program("agent-coordinator").is_none() {
        reporter.warn(
            "preflight",
            "agent-coordinator is not on PATH; the next session would not find the client.",
        );
    }
    if git::is_linked_worktree(root) {
        reporter.warn(
            "preflight",
            "This is a linked worktree: other worktrees receive the binding when they check out the commit.",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adopt::AdoptArgs;

    /// Options for a run started anywhere, with the given flags.
    fn options(args: AdoptArgs) -> Options {
        Options {
            args,
            allow_insecure_loopback: false,
            start: PathBuf::from("."),
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

    /// A binding to the given service and project.
    fn binding(url: &str, id: &str) -> RepositoryBinding {
        RepositoryBinding {
            service_url: url.into(),
            project_id: id.into(),
            project_name: None,
        }
    }

    #[test]
    fn the_service_is_the_flag_then_the_binding_then_the_default() {
        let bound = binding("https://bound.example", "p1");
        let flagged = options(AdoptArgs {
            service_url: Some("https://flag.example/".into()),
            ..AdoptArgs::default()
        });
        assert_eq!(
            service_url(&flagged, Some(&bound)).unwrap(),
            "https://flag.example"
        );
        let bare = options(AdoptArgs::default());
        assert_eq!(
            service_url(&bare, Some(&bound)).unwrap(),
            "https://bound.example"
        );
        assert_eq!(service_url(&bare, None).unwrap(), DEFAULT_SERVICE_URL);
        let insecure = options(AdoptArgs {
            service_url: Some("http://plain.example".into()),
            ..AdoptArgs::default()
        });
        assert_eq!(service_url(&insecure, None).unwrap_err().exit, 2);
    }

    #[test]
    fn an_existing_binding_is_repaired_replaced_or_refused() {
        let bound = binding("https://agents.example", "p1");
        let projects = [project("p1"), project("p2")];
        let bare = options(AdoptArgs::default());
        let state =
            |options: &Options, url: &str| binding_state(Some(&bound), url, &projects, options);
        assert_eq!(
            state(&bare, "https://agents.example").unwrap(),
            BindingState::Repair(project("p1"))
        );
        assert_eq!(
            binding_state(None, "https://agents.example", &projects, &bare).unwrap(),
            BindingState::Absent
        );
        assert_eq!(state(&bare, "https://other.example").unwrap_err().exit, 5);
        let other = options(AdoptArgs {
            project: Some("p2".into()),
            ..AdoptArgs::default()
        });
        assert_eq!(state(&other, "https://agents.example").unwrap_err().exit, 5);
        let replace = options(AdoptArgs {
            project: Some("p2".into()),
            replace_binding: true,
            ..AdoptArgs::default()
        });
        assert_eq!(
            state(&replace, "https://agents.example").unwrap(),
            BindingState::Replace
        );
        let gone = binding_state(
            Some(&bound),
            "https://agents.example",
            &[project("p2")],
            &bare,
        )
        .unwrap_err();
        assert!(
            gone.output["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not found")
        );
    }

    #[test]
    fn only_a_writing_agent_credential_is_accepted() {
        assert!(is_agent_writer(
            &json!({"kind": "agent", "credential_attributes": {"access": "write"}})
        ));
        assert!(!is_agent_writer(
            &json!({"kind": "agent", "credential_attributes": {"access": "read"}})
        ));
        assert!(!is_agent_writer(&json!({"kind": "human", "role": "admin"})));
    }
}
