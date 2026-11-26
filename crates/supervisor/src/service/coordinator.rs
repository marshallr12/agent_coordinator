//! Work from a coordinator project for the Windows supervisor service
//! (phase 8, step 8a).
//!
//! [`CoordinatorWork`] is the [`Work`] the service's loop takes tasks from
//! when `[windows_supervisor.coordinator]` is enabled. It reads `next` and
//! task details over HTTP ([`Api`]) and claims, renews, checkpoints and
//! releases with the pinned coordinator CLI ([`Cli`]) in a session of the
//! launch's own, as the Linux loop does. The CLI runs as the supervisor's
//! own account: its session proofs and mutation journals stay below the
//! protected directory ([`CoordinatorSettings::directory`]) and its token
//! reaches it only through its environment ([`access`]).
//!
//! The workload is still a fixture: a provider harness driver is step 8d.
//! A fixture never submits, so every launch ends with a checkpoint saying
//! how it exited, then a release.
use super::authority::{AttemptReader, AttemptView};
use super::stand_in::Ledger;
use super::supervisor::Work;
use crate::run_loop::Launch;
use crate::run_loop::binding::Binding;
use crate::run_loop::lease::Lease;
use crate::run_loop::renewal::{self, AttemptEnded, CliError};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;

mod access;
pub use access::{Access, ProcessCli, ServiceApi};

/// The CLI's exit code for an HTTP 409: the attempt is no longer active.
const CONFLICT_EXIT: i32 = 5;
/// The harness name the supervisor's launch sessions connect with.
pub const HARNESS: &str = "agentc-supervisor-windows";
/// The longest summary the service accepts.
const MAX_SUMMARY: usize = 8192;

/// The `[windows_supervisor.coordinator]` table. Off unless `enabled`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct CoordinatorSettings {
    /// Whether the supervisor takes its work from the coordinator. While it
    /// is false nothing else here is read.
    pub enabled: bool,
    /// The coordinator's HTTPS origin.
    pub service_url: String,
    /// The one project the supervisor claims from.
    pub project_id: String,
    /// The pinned coordinator CLI.
    pub cli: PathBuf,
    /// The protected directory holding `credentials.toml`, the binding the
    /// CLI reads and the CLI's session state (`state`).
    pub directory: PathBuf,
    /// The command every launch runs, as an argument vector. Empty: the
    /// coordinator cannot be enabled.
    pub fixture: Vec<String>,
    /// An existing directory the runner's account can create launch
    /// directories in.
    pub launch_root: PathBuf,
    /// The ledger of what the loop asked of the runner and the coordinator.
    pub ledger: PathBuf,
}

impl Default for CoordinatorSettings {
    fn default() -> Self {
        // Whole literals, as for the stand-in's defaults.
        Self {
            enabled: false,
            service_url: String::new(),
            project_id: String::new(),
            cli: PathBuf::from(r"C:\Program Files\agentc\agent-coordinator.exe"),
            directory: PathBuf::from(r"C:\ProgramData\agentc\supervisor\coordinator"),
            fixture: Vec::new(),
            launch_root: PathBuf::from(r"C:\ProgramData\agentc\runner\fixture-launches"),
            ledger: PathBuf::from(r"C:\ProgramData\agentc\supervisor\ledger.jsonl"),
        }
    }
}

impl CoordinatorSettings {
    /// Refuses an enabled coordinator with no project, an origin that is
    /// not HTTPS, or nothing to run.
    pub fn validate(&self) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        self.binding().check()?;
        ensure!(
            self.service_url.starts_with("https://"),
            "windows_supervisor.coordinator.service_url must be an https:// origin"
        );
        ensure!(
            !self.fixture.is_empty(),
            "windows_supervisor.coordinator is enabled but names no fixture command"
        );
        Ok(())
    }

    /// The repository binding the CLI is given.
    pub fn binding(&self) -> Binding {
        Binding {
            service_url: self.service_url.clone(),
            project_id: self.project_id.clone(),
            project_name: None,
        }
    }

    /// The credential file the installer puts in place.
    pub fn credentials(&self) -> PathBuf {
        self.directory.join("credentials.toml")
    }

    /// The binding file the service writes for the CLI.
    pub fn binding_file(&self) -> PathBuf {
        self.directory.join("binding.toml")
    }

    /// The CLI's session state and mutation journals.
    pub fn state(&self) -> PathBuf {
        self.directory.join("state")
    }
}

/// The coordinator's read side, which a test replaces.
pub trait Api: Send + Sync {
    /// `next` for the implementer role of `project`, leaving `exclude` out.
    fn next(&self, project: &str, exclude: &[String]) -> Result<Value>;
    /// The detail of `task` in `project`.
    fn task(&self, project: &str, task: &str) -> Result<Value>;
}

/// One finished run of the coordinator CLI.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CliOutput {
    /// The exit code; `None` when the process had none.
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl CliOutput {
    /// Whether the CLI exited 0.
    fn succeeded(&self) -> bool {
        self.code == Some(0)
    }

    /// The failure this run reports.
    fn failure(&self) -> CliError {
        let stderr = String::from_utf8_lossy(&self.stderr);
        CliError::parse(self.code, &self.stdout, &stderr)
    }
}

/// Runs the coordinator CLI in one of the supervisor's sessions, which a
/// test replaces.
pub trait Cli {
    /// Runs `args` in session `session` with `input` on standard input.
    fn run(&self, session: &str, args: &[String], input: &[u8]) -> Result<CliOutput>;
}

/// Work from one coordinator project.
pub struct CoordinatorWork<A: Api, C: Cli> {
    project: String,
    api: Arc<A>,
    cli: C,
    ledger: Ledger,
}

impl<A: Api, C: Cli> CoordinatorWork<A, C> {
    /// Work from `project`, read through `api`, changed through `cli`, with
    /// what it does appended to `ledger`.
    pub fn new(project: &str, api: Arc<A>, cli: C, ledger: Ledger) -> Self {
        Self {
            project: project.to_owned(),
            api,
            cli,
            ledger,
        }
    }

    /// Runs `args` in `launch`'s session and parses its JSON, failing with
    /// the CLI's own error when it did not succeed.
    fn call(&self, launch: &Launch, args: &[String], input: &[u8]) -> Result<Value> {
        let output = self.cli.run(&session(launch), args, input)?;
        ensure!(
            output.succeeded(),
            "{} failed: {}",
            args[0],
            output.failure()
        );
        serde_json::from_slice(&output.stdout).context("the CLI printed no JSON")
    }

    /// Appends `event` about `launch`'s attempt to the ledger.
    fn record(&self, event: &str, launch: &Launch, lease: &Lease, extra: Value) -> Result<()> {
        let mut fields = json!({"task": launch.suggestion.task,
                                "session": session(launch),
                                "attempt": lease.attempt, "generation": lease.generation});
        if let (Some(fields), Value::Object(extra)) = (fields.as_object_mut(), extra) {
            fields.extend(extra);
        }
        self.ledger.append(event, fields)
    }

    /// Saves a checkpoint holding `summary`. A refusal is recorded, not
    /// returned: the release that follows must still be attempted.
    fn checkpoint(&self, launch: &Launch, lease: &Lease, summary: &str) -> Result<()> {
        let input = json!({"summary": bounded(summary),
                           "current_action": "Releasing after the fixture launch ended."});
        let args = attempt_args("checkpoint", lease, true);
        let saved = self.call(launch, &args, &serde_json::to_vec(&input)?);
        let ok = json!({"ok": saved.is_ok()});
        if let Err(error) = &saved {
            eprintln!("agentc-supervisor windows-service: checkpoint: {error:#}");
        }
        self.record("checkpoint", launch, lease, ok)
    }

    /// Reads `lease`'s attempt from `launch`'s task once.
    fn read_state(&self, launch: &Launch, lease: &Lease) -> Option<String> {
        let task = &launch.suggestion.task;
        let detail = self.api.task(&self.project, task).ok()?;
        renewal::attempt_state(&detail, &lease.attempt)
    }
}

impl<A: Api, C: Cli> Work for CoordinatorWork<A, C> {
    fn project(&self) -> &str {
        &self.project
    }

    /// The coordinator decides what may be claimed; nothing here holds
    /// claims back.
    fn available(&self, _now_ms: i64) -> Result<()> {
        Ok(())
    }

    fn next(&mut self, exclude: &[String]) -> Result<Value> {
        self.api.next(&self.project, exclude)
    }

    /// Connects `launch`'s session, then claims the suggested task at the
    /// suggested revision.
    fn claim(&mut self, launch: &Launch) -> Result<Lease> {
        let connect = ["connect".to_owned(), format!("--harness={HARNESS}")];
        self.call(launch, &connect, b"")?;
        let lease = Lease::parse(&self.call(launch, &claim_args(launch), b"")?)?;
        self.record("claim", launch, &lease, json!({}))?;
        Ok(lease)
    }

    /// Renews through the CLI. A refusal that ends the attempt for good
    /// fails with an [`AttemptEnded`] carrying the attempt's state.
    fn renew(&mut self, launch: &Launch, lease: &Lease) -> Result<Lease> {
        let args = attempt_args("renew", lease, false);
        let output = self.cli.run(&session(launch), &args, b"")?;
        if output.succeeded() {
            let renewed = Lease::parse(&serde_json::from_slice(&output.stdout)?)?;
            self.record("renew", launch, &renewed, json!({}))?;
            return Ok(renewed);
        }
        let failure = output.failure();
        ensure!(failure.ends_attempt(), "renew failed: {failure}");
        let state = self.attempt_state(launch, lease);
        Err(AttemptEnded {
            code: failure.code,
            state,
        }
        .into())
    }

    /// Reads the attempt's state, once more if the first read fails.
    fn attempt_state(&mut self, launch: &Launch, lease: &Lease) -> Option<String> {
        renewal::retry_once(|| self.read_state(launch, lease))
    }

    /// Checkpoints, then releases with the handoff on standard input; a 409
    /// means the attempt had already ended.
    fn release(&mut self, launch: &Launch, lease: &Lease, summary: &str) -> Result<()> {
        self.checkpoint(launch, lease, summary)?;
        let args = attempt_args("release", lease, true);
        let input = serde_json::to_vec(&json!({"summary": bounded(summary)}))?;
        let output = self.cli.run(&session(launch), &args, &input)?;
        let ended = output.code == Some(CONFLICT_EXIT);
        ensure!(
            output.succeeded() || ended,
            "release failed: {}",
            output.failure()
        );
        self.record(
            "release",
            launch,
            lease,
            json!({"summary": summary, "ended": ended}),
        )
    }
}

/// Reads attempts for the authority pipe from one coordinator project.
pub struct Reader<A: Api> {
    project: String,
    api: Arc<A>,
}

impl<A: Api> Reader<A> {
    /// A reader of `project`'s attempts through `api`.
    pub fn new(project: &str, api: Arc<A>) -> Self {
        Self {
            project: project.to_owned(),
            api,
        }
    }
}

impl<A: Api> AttemptReader for Reader<A> {
    /// The attempt as `task`'s detail describes it now; an unreadable
    /// detail is an error, so authority fails closed.
    fn read(&self, task: &str, attempt: &str) -> Result<Option<AttemptView>> {
        let detail = self.api.task(&self.project, task)?;
        Ok(view(&detail, attempt))
    }
}

/// `attempt` in a task detail's `attempts` history.
fn view(detail: &Value, attempt: &str) -> Option<AttemptView> {
    let attempts = detail["attempts"].as_array()?;
    let found = attempts.iter().find(|a| a["id"] == attempt)?;
    Some(AttemptView {
        state: found["state"].as_str()?.to_owned(),
        generation: found["generation"].as_u64()?,
    })
}

/// The session a launch's coordinator calls run in: one of its own.
fn session(launch: &Launch) -> String {
    launch.session_id.to_string()
}

/// `claim` for the launch's suggested task and revision.
fn claim_args(launch: &Launch) -> Vec<String> {
    let suggestion = &launch.suggestion;
    vec![
        "claim".to_owned(),
        format!("--task={}", suggestion.task),
        format!("--revision={}", suggestion.revision),
    ]
}

/// `command` on `lease`'s attempt, reading its body from standard input
/// when `input` is set.
fn attempt_args(command: &str, lease: &Lease, input: bool) -> Vec<String> {
    let mut args = vec![
        command.to_owned(),
        format!("--attempt={}", lease.attempt),
        format!("--generation={}", lease.generation),
    ];
    if input {
        args.push("--input=-".to_owned());
    }
    args
}

/// `summary` cut to what the service accepts, on a character boundary.
fn bounded(summary: &str) -> String {
    let mut end = summary.len().min(MAX_SUMMARY);
    while !summary.is_char_boundary(end) {
        end -= 1;
    }
    summary[..end].to_owned()
}

#[cfg(test)]
mod tests;
