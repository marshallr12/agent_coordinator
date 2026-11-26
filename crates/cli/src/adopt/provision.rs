//! Step 5: resolve the project.
//!
//! An existing project is linked as it is. Otherwise the service's rule that
//! only a human may create a project is kept: adopt hands the person a
//! prefilled dashboard form (and the same values in plain text), then
//! *observes* the result with read-only calls until the project appears.

use std::collections::HashSet;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::json;
use tokio::time::Instant;

use super::host::Host;
use super::interview::{self, Settings, Target};
use super::preflight::Preflight;
use super::report::{Reporter, Status};
use super::service::Project;
use super::{Options, failure, repo_url};
use crate::Failure;

/// The values of the project the person is asked to create.
#[derive(Clone, Copy)]
pub struct Request<'a> {
    pub name: &'a str,
    pub repository_url: &'a str,
    pub branch: &'a str,
}

/// Returns the project to bind to, waiting for its creation when necessary.
pub async fn resolve(
    pre: &Preflight,
    settings: &Settings,
    options: &Options,
    host: &mut dyn Host,
    reporter: &mut Reporter,
) -> Result<Project, Failure> {
    match &settings.target {
        Target::Link(project) => Ok(project.clone()),
        Target::Create {
            name,
            repository_url,
            branch,
        } => {
            let request = Request {
                name,
                repository_url,
                branch,
            };
            create_through_dashboard(pre, request, options, host, reporter).await
        }
    }
}

/// Shows the prefilled link, opens it, and waits for exactly one new matching project.
async fn create_through_dashboard(
    pre: &Preflight,
    request: Request<'_>,
    options: &Options,
    host: &mut dyn Host,
    reporter: &mut Reporter,
) -> Result<Project, Failure> {
    let existing = pre.service.projects().await?;
    let known: HashSet<String> = existing.into_iter().map(|project| project.id).collect();
    let link = dashboard_link(&pre.service_url, request);
    announce(&link, request, reporter);
    if !options.args.no_browser && !host.open_browser(&link) {
        reporter.warn(
            "project",
            "The browser could not be opened; use the link above.",
        );
    }
    let found = wait_for_project(pre, request, &known, options, host).await?;
    accept_created(found, options, host, reporter)
}

/// Builds the dashboard link that opens the project dialog prefilled.
///
/// It carries exactly three non-secret values, each percent-encoded so no raw
/// `/`, `&`, `#` or `?` survives inside a value. It never carries a token, a
/// session name or URL userinfo (the repository URL was stripped on detection).
pub fn dashboard_link(service_url: &str, request: Request<'_>) -> String {
    format!(
        "{service_url}/#/new-project?name={}&repository_url={}&target_branch={}",
        encode_component(request.name),
        encode_component(request.repository_url),
        encode_component(request.branch),
    )
}

/// Percent-encodes one query value with `encodeURIComponent` semantics.
///
/// Only `A-Z a-z 0-9 - _ . ! ~ * ' ( )` stay literal.
fn encode_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// Prints the link with its manual equivalent, so a dashboard without the
/// prefill route (or a terminal without a browser) still works.
fn announce(link: &str, request: Request<'_>, reporter: &mut Reporter) {
    reporter.text(&[
        "Create the project in the dashboard (sign in if asked):".to_owned(),
        format!("  {link}"),
        "If the form does not open prefilled, choose \"New project\" and enter:".to_owned(),
        format!("  Name            {}", request.name),
        format!("  Repository URL  {}", request.repository_url),
        format!("  Target branch   {}", request.branch),
    ]);
    let manual = json!({"name": request.name, "repository_url": request.repository_url,
        "target_branch": request.branch});
    let summary = "Waiting for the project to appear... (Ctrl-C to stop; rerun to continue)";
    reporter.event(
        "project",
        Status::Waiting,
        summary,
        json!({"link": link, "manual": manual}),
    );
}

/// Polls the project list until a project that was not there before appears
/// with the requested repository URL, or the approval timeout elapses (exit 7).
///
/// A transport error only lengthens the pause; any other failure ends the wait.
async fn wait_for_project(
    pre: &Preflight,
    request: Request<'_>,
    known: &HashSet<String>,
    options: &Options,
    host: &dyn Host,
) -> Result<Vec<Project>, Failure> {
    let timeout = options.args.approval_timeout;
    let deadline = Instant::now() + Duration::from_secs(timeout);
    let wanted = repo_url::normalize(request.repository_url);
    loop {
        let pause = match pre.service.projects().await {
            Ok(projects) => match new_matches(projects, known, &wanted) {
                found if !found.is_empty() => return Ok(found),
                _ => host.poll_interval(),
            },
            Err(error) if error.exit == 7 => host.backoff_interval(),
            Err(error) => return Err(error),
        };
        if Instant::now() + pause > deadline {
            return Err(timed_out(request, timeout));
        }
        tokio::time::sleep(pause).await;
    }
}

/// Keeps the projects that are new since the request and name the same repository.
fn new_matches(projects: Vec<Project>, known: &HashSet<String>, wanted: &str) -> Vec<Project> {
    projects
        .into_iter()
        .filter(|project| !known.contains(&project.id))
        .filter(|project| repo_url::normalize(&project.repository_url) == wanted)
        .collect()
}

/// The exit-7 failure when no matching project appeared in time. Nothing was
/// written, and a rerun finds the project by its repository URL once it exists.
fn timed_out(request: Request<'_>, timeout: u64) -> Failure {
    failure(
        7,
        "project_not_created",
        format!(
            "No new project for {} appeared within {timeout} seconds. Nothing was written. \
             Run adopt again once it exists; if the repository URL was changed in the form, \
             pass --project ID.",
            request.repository_url
        ),
    )
}

/// Confirms the project that appeared; several candidates need a choice (exit 5 unattended).
fn accept_created(
    mut found: Vec<Project>,
    options: &Options,
    host: &mut dyn Host,
    reporter: &mut Reporter,
) -> Result<Project, Failure> {
    let cancelled = || failure(2, "cancelled", "Cancelled. Nothing was written.");
    if found.len() > 1 {
        return interview::choose_match(&found, options, host)?.ok_or_else(cancelled);
    }
    let project = found.remove(0);
    let label = format!("\"{}\" (id {})", project.name, project.id);
    let fields = json!({"project_id": project.id, "project_name": project.name});
    reporter.event("project", Status::Done, &format!("found {label}"), fields);
    let question = format!("Use project {label}?");
    if options.prompting(host) && !interview::ask_yes_no(host, &question, true)? {
        return Err(cancelled());
    }
    Ok(project)
}

/// Starts the platform's opener for `url`; best effort, output discarded.
///
/// Windows uses the URL protocol handler directly, not `cmd /C start`,
/// because `&` in the link is a command separator to `cmd`.
pub fn open_in_browser(url: &str) -> bool {
    let (program, first): (&str, Option<&str>) = if cfg!(target_os = "macos") {
        ("open", None)
    } else if cfg!(windows) {
        ("rundll32", Some("url.dll,FileProtocolHandler"))
    } else {
        ("xdg-open", None)
    };
    let mut command = Command::new(program);
    command.args(first).arg(url);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.spawn().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A project with the given ID and repository URL.
    fn project(id: &str, url: &str) -> Project {
        Project {
            id: id.into(),
            name: id.into(),
            repository_url: url.into(),
            target_branch: "main".into(),
        }
    }

    #[test]
    fn components_are_encoded_like_encode_uri_component() {
        assert_eq!(
            encode_component("https://github.com/acme/billing"),
            "https%3A%2F%2Fgithub.com%2Facme%2Fbilling"
        );
        assert_eq!(encode_component("a&b#c?d e"), "a%26b%23c%3Fd%20e");
        assert_eq!(encode_component("équipe"), "%C3%A9quipe");
        assert_eq!(encode_component("ok-_.!~*'()"), "ok-_.!~*'()");
    }

    #[test]
    fn the_link_matches_the_design_example() {
        let request = Request {
            name: "billing",
            repository_url: "https://github.com/acme/billing",
            branch: "main",
        };
        assert_eq!(
            dashboard_link("https://agents.sithbit.com", request),
            "https://agents.sithbit.com/#/new-project?name=billing&repository_url=https%3A%2F%2Fgithub.com%2Facme%2Fbilling&target_branch=main"
        );
    }

    #[test]
    fn no_separator_survives_inside_a_value() {
        let request = Request {
            name: "R&D #1 / Ünïcode?",
            repository_url: "git@host:a/b.git",
            branch: "feat/x&y",
        };
        let link = dashboard_link("https://agents.example", request);
        let query = link.split_once("#/new-project?").unwrap().1;
        let values: Vec<&str> = query.split('&').collect();
        assert_eq!(values.len(), 3, "{link}");
        for value in values {
            let (_, encoded) = value.split_once('=').unwrap();
            assert!(!encoded.contains(['/', '&', '#', '?', ' ']), "{value}");
        }
        assert!(
            link.contains("name=R%26D%20%231%20%2F%20%C3%9Cn%C3%AFcode%3F"),
            "{link}"
        );
    }

    #[test]
    fn only_new_projects_with_the_same_repository_count() {
        let known: HashSet<String> = ["old".to_owned()].into();
        let projects = vec![
            project("old", "https://example.com/acme/billing"),
            project("other", "https://example.com/acme/else"),
            project("new", "git@example.com:acme/billing.git"),
        ];
        let found = new_matches(
            projects,
            &known,
            &repo_url::normalize("https://example.com/acme/billing.git"),
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "new");
    }
}
