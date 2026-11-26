//! `agent-coordinator adopt`: connect an existing Git repository to a
//! coordinator project in one guided run, so the next Claude Code or Codex
//! session started there connects and can claim work.
//!
//! The design is the book chapter `project-scaffolding-design.md`. Two rules
//! shape everything here: `adopt` sends only `GET` requests (see [`service`]),
//! and it never loses or silently rewrites the user's files (see [`files`]).

mod commit;
mod detect;
mod files;
mod git;
mod host;
mod interview;
mod migrate;
mod plan;
mod preflight;
mod provision;
mod repo_url;
mod report;
mod service;
mod text;
mod verify;

#[cfg(test)]
mod testing;
#[cfg(test)]
mod tests;

use std::env;
use std::path::{Path, PathBuf};

use clap::{Args, ValueEnum};
use serde_json::{Value, json};

use crate::{Cli, Failure};
use files::{FileChange, InstructionPlan};
use host::{Host, RealHost};
use interview::{Settings, Target};
use preflight::{BindingState, Preflight};
use report::{Reporter, Status};
use service::Project;

/// The service used when neither `--service-url` nor an existing binding names one.
pub const DEFAULT_SERVICE_URL: &str = "https://agents.sithbit.com";

/// Options of `agent-coordinator adopt`. Every one has a default, so the bare
/// command is the normal call; each prompt also has a flag for unattended use.
#[derive(Args, Clone, Debug)]
pub struct AdoptArgs {
    /// Repository to adopt; any directory inside its work tree [default: current directory].
    pub path: Option<PathBuf>,

    /// Service to connect to [default: the existing binding's, else https://agents.sithbit.com].
    #[arg(long, value_name = "URL")]
    pub service_url: Option<String>,

    /// Project display name [default: the repository name].
    #[arg(long)]
    pub name: Option<String>,

    /// Repository URL, overriding the detected remote.
    #[arg(long, value_name = "URL")]
    pub repository_url: Option<String>,

    /// Target branch [default: the remote's default branch, else the current branch].
    #[arg(long)]
    pub branch: Option<String>,

    /// Link to this existing project instead of matching by repository URL.
    #[arg(long, value_name = "ID")]
    pub project: Option<String>,

    /// Assistants whose instruction files to prepare.
    #[arg(long, value_enum, default_value_t = HarnessArg::Auto)]
    pub harness: HarnessArg,

    /// Print the dashboard link instead of also opening it in a browser.
    #[arg(long)]
    pub no_browser: bool,

    /// Seconds to wait for the project to appear after the link is shown.
    #[arg(long, value_name = "SECS", default_value_t = 900)]
    pub approval_timeout: u64,

    /// Commit the files adopt wrote (the default); never pushes.
    #[arg(long, conflicts_with = "no_commit")]
    pub commit: bool,

    /// Leave the files adopt wrote uncommitted.
    #[arg(long)]
    pub no_commit: bool,

    /// Migrate HANDOFF.md / BACKLOG.md in an interactive assistant session: ask first, start now, or never.
    #[arg(long, value_enum, default_value_t = MigrateArg::Ask)]
    pub migrate: MigrateArg,

    /// Assistant that runs the migration [default: the first selected one that is installed].
    #[arg(long, value_enum)]
    pub migration_agent: Option<AgentArg>,

    /// Overwrite a binding that points to another service or project.
    #[arg(long)]
    pub replace_binding: bool,

    /// Insert the instruction block even if the file already mentions the binding.
    #[arg(long)]
    pub force_instructions: bool,

    /// Never prompt; report every missing answer at once as needs_input.
    #[arg(long)]
    pub non_interactive: bool,

    /// Accept every default and confirmation (implies --non-interactive).
    #[arg(long)]
    pub yes: bool,

    /// Print the plan and the file diffs; write nothing and open nothing.
    #[arg(long)]
    pub dry_run: bool,
}

/// Test code builds flags with struct-update syntax; the base is what clap
/// produces for a bare `adopt`, so the tests see the real defaults.
#[cfg(test)]
impl Default for AdoptArgs {
    /// The flags of a bare `adopt`.
    fn default() -> Self {
        use clap::Parser;
        match Cli::try_parse_from(["agent-coordinator", "adopt"]).map(|cli| cli.command) {
            Ok(crate::Command::Adopt(args)) => args,
            _ => unreachable!("a bare adopt always parses"),
        }
    }
}

/// Which assistants' instruction files `--harness` selects.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum HarnessArg {
    Claude,
    Codex,
    Both,
    /// Decide from what is installed and which files the repository has.
    #[default]
    Auto,
}

/// When the backlog migration session starts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum MigrateArg {
    /// Offer it after the repository is connected.
    #[default]
    Ask,
    /// Start it without asking.
    Now,
    /// Never start it.
    Off,
}

/// The assistant that runs the backlog migration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum AgentArg {
    Claude,
    Codex,
}

/// The assistants that will work in the repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Harnesses {
    pub claude: bool,
    pub codex: bool,
}

impl Harnesses {
    /// Both assistants: the default when nothing indicates otherwise.
    pub const BOTH: Self = Self::new(true, true);

    /// Builds a selection; shorter than a struct literal where several are listed.
    pub const fn new(claude: bool, codex: bool) -> Self {
        Self { claude, codex }
    }
}

/// One run's inputs that do not come from the machine (see [`Host`] for those).
pub struct Options {
    pub args: AdoptArgs,
    /// Whether plain HTTP to a loopback service was explicitly permitted.
    pub allow_insecure_loopback: bool,
    /// The directory the run starts from: `PATH`, or the current directory.
    pub start: PathBuf,
}

impl Options {
    /// True when questions may be put to the person at the terminal.
    pub fn prompting(&self, host: &dyn Host) -> bool {
        host.interactive() && !self.args.non_interactive && !self.args.yes
    }

    /// True when open confirmations take their defaults without asking: with
    /// `--yes`, and in a dry run that cannot prompt (it changes nothing, so
    /// there is nothing to confirm).
    pub fn accepts_defaults(&self, host: &dyn Host) -> bool {
        self.args.yes || (self.args.dry_run && !self.prompting(host))
    }

    /// The commit choice the flags settle, if any.
    pub fn commit_flag(&self) -> Option<bool> {
        match (self.args.commit, self.args.no_commit) {
            (true, _) => Some(true),
            (_, true) => Some(false),
            _ => None,
        }
    }
}

/// Builds an `adopt` failure in the CLI's standard error envelope.
pub(crate) fn failure(exit: u8, code: &str, message: impl Into<String>) -> Failure {
    Failure::local(exit, code, message, exit == 7)
}

/// Like [`failure`], with machine-readable details.
pub(crate) fn failure_with(
    exit: u8,
    code: &str,
    message: impl Into<String>,
    details: Value,
) -> Failure {
    let mut failure = failure(exit, code, message);
    failure.output["error"]["details"] = details;
    failure
}

/// Runs `adopt` on the real machine. Dispatched before any binding is looked
/// up, because the command's whole purpose is to create one.
pub async fn run(cli: &Cli, args: &AdoptArgs) -> Result<Value, Failure> {
    let start = match &args.path {
        Some(path) => path.clone(),
        None => env::current_dir().map_err(Failure::invalid)?,
    };
    let options = Options {
        args: args.clone(),
        allow_insecure_loopback: cli.allow_insecure_loopback,
        start,
    };
    let mut host = RealHost {
        allow_insecure_loopback: cli.allow_insecure_loopback,
    };
    let mut reporter = Reporter::stdout(cli.json);
    execute(&options, &mut host, &mut reporter).await
}

/// Runs the steps in order. Nothing is written before the plan is confirmed,
/// and nothing here ever changes the service.
pub async fn execute(
    options: &Options,
    host: &mut dyn Host,
    reporter: &mut Reporter,
) -> Result<Value, Failure> {
    let pre = preflight::run(options, host, reporter).await?;
    let detected = detect::run(&pre, options, host, reporter);
    let settings = interview::run(&pre, &detected, options, host)?;
    let force = options.args.force_instructions;
    let instructions = files::plan_instructions(&pre.root, settings.harnesses, force);
    let preview = binding_change(&pre, preview_project_id(&settings));
    require_clean(&pre, &preview, &instructions)?;
    plan::present(&pre, &settings, &preview, &instructions, options, reporter);
    if options.args.dry_run {
        return Ok(outcome(
            "dry_run",
            &pre,
            None,
            &Written::default(),
            reporter,
        ));
    }
    interview::confirm_plan(&pre, options, host)?;
    let project = provision::resolve(&pre, &settings, options, host, reporter).await?;
    let binding = binding_change(&pre, &project.id);
    let mut done = Written {
        files: write_files(&binding, &instructions, reporter)?,
        ..Written::default()
    };
    verify::run(&pre, &project, settings.harnesses, options, host, reporter).await?;
    let managed = commit_paths(&pre.root, &instructions, &done.files);
    done.commit = commit::run(&pre.root, &managed, &done.files, settings.commit, reporter);
    done.migration = migrate::run(&pre.root, settings.harnesses, options, host, reporter);
    let status = if done.files.is_empty() {
        "already_connected"
    } else {
        "connected"
    };
    Ok(outcome(status, &pre, Some(&project), &done, reporter))
}

/// What a run changed, for the final report.
#[derive(Default)]
struct Written {
    /// Git paths of the files written.
    files: Vec<String>,
    /// The commit adopt made, if any.
    commit: Option<String>,
    /// How the migration step ended (`null` when it was never reached).
    migration: Value,
}

/// The project ID shown in a plan: the real one when linking, a placeholder when creating.
fn preview_project_id(settings: &Settings) -> &str {
    match &settings.target {
        Target::Link(project) => &project.id,
        Target::Create { .. } => "<assigned when the project is created>",
    }
}

/// Plans the binding file. Repair mode keeps the existing file (and its
/// `project_name`) exactly as it is.
fn binding_change(pre: &Preflight, project_id: &str) -> Option<FileChange> {
    match pre.binding {
        BindingState::Repair(_) => None,
        BindingState::Absent => Some(files::plan_binding(
            &pre.root,
            &pre.service_url,
            project_id,
            false,
        )),
        BindingState::Replace => Some(files::plan_binding(
            &pre.root,
            &pre.service_url,
            project_id,
            true,
        )),
    }
}

/// Refuses to run (exit 6) when a file adopt would modify has uncommitted changes.
///
/// Files that are already current are not checked, so a repository adopted
/// with `--no-commit` can be re-run safely.
fn require_clean(
    pre: &Preflight,
    binding: &Option<FileChange>,
    instructions: &InstructionPlan,
) -> Result<(), Failure> {
    let modified: Vec<String> = binding
        .iter()
        .chain(&instructions.changes)
        .filter(|change| change.is_change() && change.original.is_some())
        .map(|change| change.git_path.clone())
        .collect();
    let dirty = git::dirty_paths(&pre.root, &modified).map_err(Failure::invalid)?;
    if dirty.is_empty() {
        return Ok(());
    }
    let message = format!(
        "{} has uncommitted changes; commit or stash them first, then run adopt again.",
        dirty.join(", ")
    );
    Err(failure_with(
        6,
        "target_files_dirty",
        message,
        json!({"paths": dirty}),
    ))
}

/// Writes the binding and the instruction files; returns the Git paths actually written.
fn write_files(
    binding: &Option<FileChange>,
    instructions: &InstructionPlan,
    reporter: &mut Reporter,
) -> Result<Vec<String>, Failure> {
    let mut written = Vec::new();
    for change in binding.iter().chain(&instructions.changes) {
        if files::apply(change)? {
            written.push(change.git_path.clone());
        }
    }
    for (file, reason) in &instructions.skipped {
        reporter.warn("write", format!("{file} was left alone: {reason}"));
    }
    let summary = if written.is_empty() {
        "every file is already current".to_owned()
    } else {
        format!("wrote {}", written.join(", "))
    };
    reporter.event("write", Status::Done, &summary, json!({"files": written}));
    Ok(written)
}

/// The Git paths the commit may include: what this run wrote, the binding,
/// and any instruction file an interrupted earlier run left holding nothing
/// but adopt's text. A file with someone's own uncommitted edits never qualifies.
fn commit_paths(root: &Path, instructions: &InstructionPlan, written: &[String]) -> Vec<String> {
    let mut paths = written.to_vec();
    let earlier = instructions
        .changes
        .iter()
        .filter(|change| !written.contains(&change.git_path));
    for change in earlier {
        let committed = git::head_content(root, &change.git_path);
        if files::holds_only_adopt_text(change, committed.as_deref()) {
            paths.push(change.git_path.clone());
        }
    }
    if !paths.iter().any(|path| path == files::BINDING_FILE) {
        paths.push(files::BINDING_FILE.to_owned());
    }
    paths
}

/// Builds the final `{"result": ...}` envelope; no credential ever enters it.
fn outcome(
    status: &str,
    pre: &Preflight,
    project: Option<&Project>,
    done: &Written,
    reporter: &Reporter,
) -> Value {
    json!({"result": {
        "status": status,
        "repository": pre.root.display().to_string(),
        "service_url": pre.service_url,
        "project_id": project.map(|project| project.id.clone()),
        "project_name": project.map(|project| project.name.clone()),
        "files_written": done.files,
        "commit": done.commit,
        "migration": done.migration,
        "warnings": reporter.warnings(),
        "next_session": next_session(status),
    }})
}

/// The guidance for what to do after this run.
fn next_session(status: &str) -> &'static str {
    if status == "dry_run" {
        "Nothing was written. Run adopt again without --dry-run to connect the repository."
    } else {
        "Start Claude Code or Codex in this repository: it reads the instruction block, \
         connects to the service and can claim work."
    }
}

/// Prints the final report for a person (`--json` prints the envelope instead).
pub fn print_report(value: &Value) {
    for line in report_lines(value) {
        println!("{line}");
    }
}

/// The lines of the human final report.
fn report_lines(value: &Value) -> Vec<String> {
    let result = &value["result"];
    let text = |name: &str| result[name].as_str().unwrap_or("-").to_owned();
    let headline = match result["status"].as_str() {
        Some("dry_run") => "Dry run".to_owned(),
        Some("already_connected") => "Already connected".to_owned(),
        _ => "Connected".to_owned(),
    };
    let mut lines = vec![format!("{headline}: {}", text("repository"))];
    if result["project_id"].is_string() {
        lines.push(format!(
            "Project: {} ({}) at {}",
            text("project_name"),
            text("project_id"),
            text("service_url")
        ));
    }
    lines.push(format!(
        "Files written: {}",
        joined(&result["files_written"])
    ));
    if let Some(commit) = result["commit"].as_str() {
        lines.push(format!("Commit: {commit} (local only; nothing was pushed)"));
    }
    lines.push(format!("Next: {}", text("next_session")));
    lines
}

/// Joins a JSON array of strings for display, or says "none".
fn joined(list: &Value) -> String {
    let items: Vec<&str> = list
        .as_array()
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if items.is_empty() {
        "none".to_owned()
    } else {
        items.join(", ")
    }
}
