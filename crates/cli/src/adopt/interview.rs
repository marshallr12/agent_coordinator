//! Step 3: settle every choice, asking only what could not be detected.
//!
//! Each question has a flag as its unattended form. When prompting is off and
//! something is still open, the run stops with `needs_input` (exit 2) naming
//! every missing flag at once, before anything is opened or written.

use serde_json::{Value, json};

use super::detect::Detected;
use super::host::Host;
use super::preflight::{BindingState, Preflight};
use super::service::Project;
use super::{Harnesses, Options, failure, failure_with};
use crate::Failure;

/// How many times an unclear answer is asked again before the run gives up.
const MAX_ATTEMPTS: usize = 3;

/// The project a run binds to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// An existing project.
    Link(Project),
    /// A project the human still has to create in the dashboard, with these values.
    Create {
        name: String,
        repository_url: String,
        branch: String,
    },
}

/// Every choice of a run, settled.
#[derive(Clone, Debug)]
pub struct Settings {
    pub target: Target,
    pub harnesses: Harnesses,
    pub commit: bool,
}

/// The answers still missing when prompting is off: `(flag, what it settles)`.
#[derive(Default)]
struct Needs(Vec<(String, String)>);

impl Needs {
    /// Records one missing answer.
    fn add(&mut self, flag: &str, reason: impl Into<String>) {
        self.0.push((flag.to_owned(), reason.into()));
    }

    /// Fails with the complete list, so one corrected command line is enough.
    fn check(self) -> Result<(), Failure> {
        if self.0.is_empty() {
            return Ok(());
        }
        let listed: Vec<String> = self
            .0
            .iter()
            .map(|(flag, why)| format!("{flag} ({why})"))
            .collect();
        let message = format!(
            "Input is needed and prompting is off. Pass: {}",
            listed.join("; ")
        );
        Err(failure_with(
            2,
            "needs_input",
            message,
            json!({"missing": self.details()}),
        ))
    }

    /// The missing answers as JSON, for a calling program.
    fn details(&self) -> Vec<Value> {
        let entry = |(flag, reason): &(String, String)| json!({"flag": flag, "reason": reason});
        self.0.iter().map(entry).collect()
    }
}

/// Settles the target project, the assistants and the commit choice.
pub fn run(
    pre: &Preflight,
    detected: &Detected,
    options: &Options,
    host: &mut dyn Host,
) -> Result<Settings, Failure> {
    let harnesses = detected.harnesses;
    if let BindingState::Repair(project) = &pre.binding {
        return Ok(repair_settings(project, harnesses, options));
    }
    let mut needs = Needs::default();
    let target = target(pre, detected, options, host, &mut needs)?;
    let prompting = options.prompting(host);
    if !prompting && !options.accepts_defaults(host) {
        needs.add("--yes", "confirm the plan");
    }
    needs.check()?;
    Ok(Settings {
        target,
        harnesses: choose_harnesses(harnesses, prompting, host)?,
        commit: choose_commit(options, prompting, host)?,
    })
}

/// Repair mode asks nothing: the project is the bound one and defaults apply.
fn repair_settings(project: &Project, harnesses: Option<Harnesses>, options: &Options) -> Settings {
    Settings {
        target: Target::Link(project.clone()),
        harnesses: harnesses.unwrap_or(Harnesses::BOTH),
        commit: options.commit_flag().unwrap_or(true),
    }
}

/// Chooses the project: `--project`, an existing match the person accepts, or a new one.
fn target(
    pre: &Preflight,
    detected: &Detected,
    options: &Options,
    host: &mut dyn Host,
    needs: &mut Needs,
) -> Result<Target, Failure> {
    if let Some(id) = &options.args.project {
        return project_by_id(&pre.projects, id).map(Target::Link);
    }
    let accepted = match detected.matches.as_slice() {
        [] => None,
        [only] => accept_match(only, options, host, needs)?,
        several => choose_match(several, options, host)?,
    };
    match accepted {
        Some(project) => Ok(Target::Link(project)),
        None => new_project(detected, options, host, needs),
    }
}

/// Finds the project `--project` names; it must exist and not be archived.
fn project_by_id(projects: &[Project], id: &str) -> Result<Project, Failure> {
    projects
        .iter()
        .find(|project| project.id == id)
        .cloned()
        .ok_or_else(|| {
            failure(
                2,
                "project_not_found",
                format!("Project {id} was not found on the service (or is archived)."),
            )
        })
}

/// Asks whether to link the one existing project that matches this repository.
fn accept_match(
    project: &Project,
    options: &Options,
    host: &mut dyn Host,
    needs: &mut Needs,
) -> Result<Option<Project>, Failure> {
    let label = format!("project \"{}\" (id {})", project.name, project.id);
    if options.accepts_defaults(host) {
        return Ok(Some(project.clone()));
    }
    if !options.prompting(host) {
        needs.add(
            &format!("--project {}", project.id),
            format!("link the existing {label}, or pass --yes"),
        );
        return Ok(Some(project.clone()));
    }
    let question = format!("This repository is already {label}. Link to it?");
    Ok(ask_yes_no(host, &question, true)?.then(|| project.clone()))
}

/// Lets the person pick among several matching projects; unattended runs must name one.
pub fn choose_match(
    matches: &[Project],
    options: &Options,
    host: &mut dyn Host,
) -> Result<Option<Project>, Failure> {
    let label = |project: &Project| format!("{} (id {})", project.name, project.id);
    let listed: Vec<String> = matches.iter().map(label).collect();
    if !options.prompting(host) {
        let all = listed.join("; ");
        let message =
            format!("Several projects match this repository: {all}. Pass --project ID to choose.");
        return Err(failure(5, "ambiguous_project", message));
    }
    let menu: Vec<String> = (1..)
        .zip(&listed)
        .map(|(number, item)| format!("  {number}. {item}"))
        .collect();
    let question = format!(
        "Several projects match this repository:\n{}\nNumber to link, or 0 for a new project",
        menu.join("\n")
    );
    let answer = ask_value(host, &question, None)?;
    numbered_choice(&answer, matches)
}

/// Reads the answer to the project menu: 0 is a new project, 1.. is a listed one.
fn numbered_choice(answer: &str, matches: &[Project]) -> Result<Option<Project>, Failure> {
    match answer.parse::<usize>() {
        Ok(0) => Ok(None),
        Ok(number) if number <= matches.len() => Ok(Some(matches[number - 1].clone())),
        _ => Err(failure(
            2,
            "invalid_answer",
            format!("\"{answer}\" is not one of the listed numbers."),
        )),
    }
}

/// Settles the values of a project to create, confirming or editing them when prompting.
fn new_project(
    detected: &Detected,
    options: &Options,
    host: &mut dyn Host,
    needs: &mut Needs,
) -> Result<Target, Failure> {
    let name = detected.name.clone().unwrap_or_default();
    let url = detected.repository_url.clone().unwrap_or_default();
    let mut values = (name, url, detected.branch.clone());
    if options.prompting(host) {
        values = confirm_or_edit(values, host)?;
    } else if values.1.is_empty() {
        needs.add(
            "--repository-url URL",
            "this repository has no remote to detect it from",
        );
    }
    let (name, repository_url, branch) = values;
    validate_project(&name, &repository_url, &branch, needs.0.is_empty())?;
    Ok(Target::Create {
        name,
        repository_url,
        branch,
    })
}

/// Shows the name, repository URL and branch together; the person confirms or edits them.
fn confirm_or_edit(
    (name, url, branch): (String, String, String),
    host: &mut dyn Host,
) -> Result<(String, String, String), Failure> {
    let summary = format!(
        "New project: name \"{name}\", repository {}, branch {branch}.",
        shown(&url)
    );
    if !url.is_empty() && ask_yes_no(host, &format!("{summary} Use these values?"), true)? {
        return Ok((name, url, branch));
    }
    Ok((
        ask_value(host, "Project name", Some(&name))?,
        ask_value(host, "Repository URL", Some(&url))?,
        ask_value(host, "Target branch", Some(&branch))?,
    ))
}

/// Shows a value, or says that it is not set.
fn shown(value: &str) -> &str {
    if value.is_empty() {
        "(none detected)"
    } else {
        value
    }
}

/// Applies the service's own limits (counted in bytes, as the service counts
/// them), so a bad value fails here and not later in the dashboard.
fn validate_project(name: &str, url: &str, branch: &str, require_url: bool) -> Result<(), Failure> {
    let invalid = |message: &str| Err(failure(2, "invalid_client_input", message));
    let unprintable = |value: &str| value.chars().any(char::is_control);
    if name.trim().is_empty() || name.len() > 120 || unprintable(name) {
        return invalid(
            "The project name must have 1 to 120 bytes and no control characters (--name).",
        );
    }
    if (require_url && url.trim().is_empty()) || url.len() > 2048 || unprintable(url) {
        return invalid(
            "The repository URL must have 1 to 2048 bytes and no control characters (--repository-url).",
        );
    }
    let unusable =
        branch.starts_with('-') || branch.chars().any(|c| c.is_whitespace() || c.is_control());
    if branch.is_empty() || branch.len() > 255 || unusable {
        return invalid(
            "The target branch must be a Git branch name of at most 255 bytes without whitespace or a leading dash (--branch).",
        );
    }
    Ok(())
}

/// Uses the detected assistants, asking only when nothing was detected.
fn choose_harnesses(
    detected: Option<Harnesses>,
    prompting: bool,
    host: &mut dyn Host,
) -> Result<Harnesses, Failure> {
    match detected {
        Some(found) => Ok(found),
        None if !prompting => Ok(Harnesses::BOTH),
        None => {
            let question = "Which assistants will work here? (claude/codex/both)";
            parse_harnesses(&ask_value(host, question, Some("both"))?)
        }
    }
}

/// Reads the answer to the assistants question.
fn parse_harnesses(answer: &str) -> Result<Harnesses, Failure> {
    match answer.to_ascii_lowercase().as_str() {
        "claude" => Ok(Harnesses::new(true, false)),
        "codex" => Ok(Harnesses::new(false, true)),
        "both" => Ok(Harnesses::BOTH),
        _ => Err(failure(
            2,
            "invalid_answer",
            format!("\"{answer}\" is not claude, codex or both."),
        )),
    }
}

/// Uses `--commit`/`--no-commit`, else asks, else commits (the default).
fn choose_commit(options: &Options, prompting: bool, host: &mut dyn Host) -> Result<bool, Failure> {
    match options.commit_flag() {
        Some(choice) => Ok(choice),
        None if prompting => ask_yes_no(host, "Commit the changes?", true),
        None => Ok(true),
    }
}

/// Step 4's single confirmation. Repair mode and `--yes` need none; a silent
/// run without `--yes` was already stopped by `needs_input`.
pub fn confirm_plan(
    pre: &Preflight,
    options: &Options,
    host: &mut dyn Host,
) -> Result<(), Failure> {
    let repair = matches!(pre.binding, BindingState::Repair(_));
    if repair || !options.prompting(host) || ask_yes_no(host, "Proceed?", true)? {
        return Ok(());
    }
    Err(failure(
        2,
        "cancelled",
        "Cancelled at the plan. Nothing was written.",
    ))
}

/// Asks a yes/no question, showing the default in brackets.
pub fn ask_yes_no(host: &mut dyn Host, question: &str, default: bool) -> Result<bool, Failure> {
    let hint = if default { "[Y/n]" } else { "[y/N]" };
    for _ in 0..MAX_ATTEMPTS {
        let answer = ask(host, &format!("{question} {hint} "))?;
        match answer.to_ascii_lowercase().as_str() {
            "" => return Ok(default),
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => {}
        }
    }
    Err(failure(2, "invalid_answer", "Expected yes or no."))
}

/// Asks for a value, showing its default in brackets; an empty answer accepts the default.
pub fn ask_value(
    host: &mut dyn Host,
    label: &str,
    default: Option<&str>,
) -> Result<String, Failure> {
    let hint = default
        .filter(|value| !value.is_empty())
        .map(|value| format!(" [{value}]"));
    let answer = ask(host, &format!("{label}{}: ", hint.unwrap_or_default()))?;
    if answer.is_empty() {
        Ok(default.unwrap_or_default().to_owned())
    } else {
        Ok(answer)
    }
}

/// Puts one prompt to the person; a closed input ends the run instead of assuming an answer.
fn ask(host: &mut dyn Host, prompt: &str) -> Result<String, Failure> {
    host.ask(prompt).map_err(|error| {
        failure(
            2,
            "input_closed",
            format!("Could not read an answer ({error}). Nothing was written."),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adopt::AdoptArgs;
    use crate::adopt::testing::TestHost;

    /// Options for a run with the given flags.
    fn options(args: AdoptArgs) -> Options {
        Options {
            args,
            allow_insecure_loopback: false,
            start: ".".into(),
        }
    }

    /// Detection results for a repository named "billing" on branch `main`.
    fn detected(url: Option<&str>, matches: Vec<Project>) -> Detected {
        Detected {
            repository_url: url.map(str::to_owned),
            branch: "main".into(),
            name: Some("billing".into()),
            matches,
            harnesses: Some(Harnesses {
                claude: true,
                codex: true,
            }),
        }
    }

    /// A project named "Billing" with the given ID.
    fn project(id: &str) -> Project {
        Project {
            id: id.into(),
            name: "Billing".into(),
            repository_url: "https://example.com/acme/billing".into(),
            target_branch: "main".into(),
        }
    }

    #[test]
    fn yes_no_answers_use_the_default_and_retry_unclear_input() {
        let mut host = TestHost::new("token").answering(&["", "maybe", "N"]);
        assert!(ask_yes_no(&mut host, "Proceed?", true).unwrap());
        assert!(!ask_yes_no(&mut host, "Proceed?", true).unwrap());
        assert_eq!(host.prompts[0], "Proceed? [Y/n] ");
        let mut closed = TestHost::new("token");
        assert_eq!(
            ask_yes_no(&mut closed, "Proceed?", true).unwrap_err().exit,
            2
        );
    }

    #[test]
    fn values_show_and_accept_their_default() {
        let mut host = TestHost::new("token").answering(&["", "trunk"]);
        assert_eq!(
            ask_value(&mut host, "Target branch", Some("main")).unwrap(),
            "main"
        );
        assert_eq!(
            ask_value(&mut host, "Target branch", Some("main")).unwrap(),
            "trunk"
        );
        assert_eq!(host.prompts[0], "Target branch [main]: ");
    }

    #[test]
    fn a_new_project_can_be_edited_at_the_prompt() {
        let mut host = TestHost::new("token").answering(&["n", "Atlas", "", "trunk"]);
        let mut needs = Needs::default();
        let found = detected(Some("https://example.com/acme/billing"), Vec::new());
        let target = new_project(
            &found,
            &options(AdoptArgs::default()),
            &mut host,
            &mut needs,
        )
        .unwrap();
        let expected = Target::Create {
            name: "Atlas".into(),
            repository_url: "https://example.com/acme/billing".into(),
            branch: "trunk".into(),
        };
        assert_eq!(target, expected);
    }

    #[test]
    fn several_matches_need_a_choice() {
        let matches = vec![project("p1"), project("p2")];
        let mut silent = TestHost::new("token").silent();
        let bare = options(AdoptArgs::default());
        assert_eq!(
            choose_match(&matches, &bare, &mut silent).unwrap_err().exit,
            5
        );
        let mut host = TestHost::new("token").answering(&["2", "0"]);
        assert_eq!(
            choose_match(&matches, &bare, &mut host).unwrap(),
            Some(project("p2"))
        );
        assert_eq!(choose_match(&matches, &bare, &mut host).unwrap(), None);
    }

    #[test]
    fn project_values_follow_the_service_limits() {
        assert!(validate_project("billing", "https://example.com/a/b", "main", true).is_ok());
        assert!(validate_project("", "https://example.com/a/b", "main", true).is_err());
        assert!(
            validate_project(&"x".repeat(121), "https://example.com/a/b", "main", true).is_err()
        );
        assert!(validate_project("billing", "", "main", true).is_err());
        assert!(validate_project("billing", "u", "-main", true).is_err());
        assert!(validate_project("billing", "u", "my branch", true).is_err());
        assert!(
            validate_project(&"é".repeat(61), "u", "main", true).is_err(),
            "limits are bytes"
        );
        assert!(validate_project("bill\u{7}ing", "u", "main", true).is_err());
    }
}
