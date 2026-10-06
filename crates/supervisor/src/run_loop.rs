//! Live mode (autonomy plan P3b pilot core): poll `next` as the implementer,
//! claim the suggested task, create the per-launch clone and `$RUN`, render
//! the role contract into the prompt, run `launch-root`, then remove the clone
//! and run directory once the launch is terminal.
//!
//! The loop only sequences steps; everything that talks to the coordinator or
//! runs commands sits behind [`Driver`], so tests drive it with a fake.
//! Extension points for the follow-up tasks: [`refusal`] (health, cost and
//! the kill switch), [`review_hook`] (reviewer launches and verdicts) and
//! [`finish`] (lease release on a failed launch, recovery evidence).
#[cfg(target_os = "linux")]
pub mod live;

use crate::config::Config;
use crate::profile::{Harness, Role};
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

/// The implementer role contract; `render_prompt` fills its placeholders.
pub const IMPLEMENTER_CONTRACT: &str = include_str!("../contracts/implementer.md");
/// Repository files copied into the prompt as delimited data.
const INSTRUCTION_FILES: [&str; 2] = ["AGENTS.md", "CONTRIBUTING.md"];
/// The most bytes of one instruction file copied into the prompt.
const MAX_INSTRUCTION_BYTES: usize = 32 * 1024;

/// `[run]` settings; every entry has a default.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct RunConfig {
    /// Seconds between polls of `next` (at least 5).
    pub poll_seconds: u64,
    pub harness: Harness,
    pub model: String,
    pub effort: String,
    /// Refuse to claim while the state filesystem has less free space (MiB).
    pub min_free_mib: u64,
    /// Branch of the host mirror (`<state_dir>/mirror.git`) clones start from.
    pub branch: String,
    /// Permit plain HTTP to a loopback coordinator (staging).
    pub allow_insecure_loopback: bool,
    /// Also poll reviewer work (a stub until reviewer launches land).
    pub reviewer: bool,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            poll_seconds: 60,
            harness: Harness::Claude,
            model: "default".into(),
            effort: "high".into(),
            min_free_mib: 20 * 1024,
            branch: "main".into(),
            allow_insecure_loopback: false,
            reviewer: false,
        }
    }
}

/// The heartbeat file `run` rewrites every poll.
pub fn heartbeat_path(config: &Config) -> PathBuf {
    config.state_dir.join("heartbeat.json")
}

/// The task `next` suggests claiming.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub task: String,
    pub revision: u64,
    pub title: String,
}

/// One planned launch: the suggestion and where it runs.
#[derive(Debug, Clone)]
pub struct Launch {
    pub project: String,
    pub suggestion: Suggestion,
    pub session_id: Uuid,
    pub clone: PathBuf,
    pub run: PathBuf,
}

impl Launch {
    /// A launch with a fresh session id, its clone and `$RUN` under the
    /// implementer's `clones/` and `runs/`.
    pub fn plan(config: &Config, project: &str, suggestion: Suggestion) -> Self {
        let session_id = Uuid::new_v4();
        let role = config.state_dir.join(Role::Implementer.slug());
        Self {
            project: project.to_owned(),
            suggestion,
            session_id,
            clone: role.join("clones").join(session_id.to_string()),
            run: role.join("runs").join(session_id.to_string()),
        }
    }
}

/// What the loop needs from the coordinator and the host.
pub trait Driver {
    /// The project this host works on.
    fn project(&self) -> &str;
    /// `next` data for `role`.
    fn next(&mut self, role: Role) -> Result<Value>;
    /// Free bytes on the filesystem holding the state directory.
    fn free_bytes(&self) -> Result<u64>;
    /// Creates the clone and the prepared `$RUN`, owned by the role.
    fn create(&mut self, launch: &Launch) -> Result<()>;
    /// Writes the rendered prompt into `$RUN`, readable by the role.
    fn install_prompt(&mut self, launch: &Launch, prompt: &str) -> Result<()>;
    /// Claims the task in the launch's own coordinator session.
    fn claim(&mut self, launch: &Launch) -> Result<()>;
    /// Runs the launch through `launch-root`; returns its exit code.
    fn launch(&mut self, launch: &Launch) -> Result<i32>;
}

/// What one poll did; recorded in the heartbeat.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Idle,
    Refused(String),
    Launched { task: String, exit_code: i32 },
    Failed(String),
}

/// Polls until stopped (or once), writing the heartbeat after every poll.
pub fn run(driver: &mut impl Driver, config: &Config, once: bool) -> Result<()> {
    let settings = &config.run;
    sweep_terminal(config);
    for iteration in 1_u64.. {
        let outcome = iterate(driver, config);
        if outcome != Outcome::Idle {
            eprintln!("agentc-supervisor run: {outcome:?}");
        }
        if let Err(error) = beat(&heartbeat_path(config), iteration, &outcome) {
            eprintln!("agentc-supervisor run: heartbeat: {error:#}");
        }
        if once {
            break;
        }
        std::thread::sleep(Duration::from_secs(settings.poll_seconds.max(5)));
    }
    Ok(())
}

/// Removes every terminal implementer run, with its clone, that an earlier
/// loop left behind (it stopped between a launch's exit and its cleanup).
pub fn sweep_terminal(config: &Config) {
    let role = config.state_dir.join(Role::Implementer.slug());
    let Ok(entries) = std::fs::read_dir(role.join("runs")) else {
        return;
    };
    for run in entries.flatten().map(|entry| entry.path()) {
        let clone = role
            .join("clones")
            .join(run.file_name().unwrap_or_default());
        if run.join(".state-terminal.json").exists()
            && let Err(error) = remove_finished(&clone, &run)
        {
            eprintln!("agentc-supervisor run: sweep: {error:#}");
        }
    }
}

/// One poll: admission, `next`, then the launch it suggests.
pub fn iterate(driver: &mut impl Driver, config: &Config) -> Outcome {
    if let Some(reason) = refusal(driver, &config.run) {
        return Outcome::Refused(reason);
    }
    if config.run.reviewer {
        review_hook(driver);
    }
    let next = match driver.next(Role::Implementer) {
        Ok(next) => next,
        Err(error) => return Outcome::Failed(format!("next: {error:#}")),
    };
    let Some(suggestion) = suggestion(&next) else {
        return Outcome::Idle;
    };
    let launch = Launch::plan(config, driver.project(), suggestion);
    let result = work(driver, &launch);
    finish(&launch, result)
}

/// Why the host must not claim now, if it must not. The disk high-water
/// mark lives here; health, cost and the kill switch belong here too.
pub fn refusal(driver: &impl Driver, settings: &RunConfig) -> Option<String> {
    let needed = settings.min_free_mib.saturating_mul(1024 * 1024);
    match driver.free_bytes() {
        Ok(free) if free >= needed => None,
        Ok(free) => Some(format!(
            "free disk {} MiB is below the {} MiB high-water mark",
            free / (1024 * 1024),
            settings.min_free_mib
        )),
        Err(error) => Some(format!("free disk unknown: {error:#}")),
    }
}

/// Reviewer launches arrive with task 6cf630c0; until then the hook only
/// reports that reviewer work is not taken.
pub fn review_hook(driver: &mut impl Driver) {
    if let Ok(next) = driver.next(Role::Reviewer)
        && !next["action"].is_null()
    {
        eprintln!("agentc-supervisor run: reviewer work is not launched yet");
    }
}

/// The task to claim from a `next` response: only ordinary `claim_task`
/// actions (recovery claims need recovery evidence first).
pub fn suggestion(next: &Value) -> Option<Suggestion> {
    let action = next.get("action").filter(|a| a["kind"] == "claim_task")?;
    let body = &action["call"]["body"];
    Some(Suggestion {
        task: body["task_id"].as_str()?.to_owned(),
        revision: body["expected_task_revision"].as_u64()?,
        title: action["title"].as_str().unwrap_or("").to_owned(),
    })
}

/// Creates, prompts, claims and launches, stopping at the first failure.
fn work(driver: &mut impl Driver, launch: &Launch) -> Result<i32> {
    driver.create(launch).context("create clone and run")?;
    let prompt = render_prompt(launch, &instruction_files(&launch.clone));
    driver.install_prompt(launch, &prompt).context("prompt")?;
    driver.claim(launch).context("claim")?;
    driver.launch(launch).context("launch")
}

/// Removes the launch's clone and run directory when it is safe, and turns
/// the launch result into an outcome. A claimed task whose launch failed
/// keeps its lease until it expires (crash release is a follow-up task).
fn finish(launch: &Launch, result: Result<i32>) -> Outcome {
    if let Err(error) = remove_finished(&launch.clone, &launch.run) {
        eprintln!("agentc-supervisor run: cleanup: {error:#}");
    }
    match result {
        Ok(exit_code) => Outcome::Launched {
            task: launch.suggestion.task.clone(),
            exit_code,
        },
        Err(error) => Outcome::Failed(format!("{}: {error:#}", launch.suggestion.task)),
    }
}

/// Removes `clone` and `run` when the run is terminal or never started. A
/// started run without a terminal record may still have a live harness, so
/// both are kept for recovery.
pub fn remove_finished(clone: &Path, run: &Path) -> Result<bool> {
    let started = run.join(".state-started").exists();
    if started && !run.join(".state-terminal.json").exists() {
        return Ok(false);
    }
    for path in [clone, run] {
        match std::fs::remove_dir_all(path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(error).with_context(|| format!("remove {}", path.display()));
            }
            _ => {}
        }
    }
    Ok(true)
}

/// The repository's instruction files present in `clone`, truncated.
fn instruction_files(clone: &Path) -> Vec<(String, String)> {
    INSTRUCTION_FILES
        .iter()
        .filter_map(|name| {
            let bytes = std::fs::read(clone.join(name)).ok()?;
            let text = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_INSTRUCTION_BYTES)]);
            Some(((*name).to_owned(), text.into_owned()))
        })
        .collect()
}

/// The prompt: the implementer contract with the launch filled in, then each
/// repository file inside `<repository-instructions>` tags. A closing tag in
/// a file is defused so the data cannot end its own block.
pub fn render_prompt(launch: &Launch, files: &[(String, String)]) -> String {
    let s = &launch.suggestion;
    let mut prompt = IMPLEMENTER_CONTRACT
        .replace("{{project}}", &launch.project)
        .replace("{{task_id}}", &s.task)
        .replace("{{task_revision}}", &s.revision.to_string())
        .replace("{{task_title}}", &s.title.replace(['\n', '\r'], " "))
        .replace("{{session}}", &launch.session_id.to_string());
    for (name, text) in files {
        let text = text.replace("</repository-instructions", "<\\/repository-instructions");
        prompt.push_str(&format!(
            "\n<repository-instructions file=\"{name}\">\n{text}\n</repository-instructions>\n"
        ));
    }
    prompt
}

/// Rewrites the heartbeat atomically: poll count, time and last outcome.
pub fn beat(path: &Path, iteration: u64, outcome: &Outcome) -> Result<()> {
    let record = json!({"pid": std::process::id(), "iteration": iteration,
                        "at_ms": crate::shadow::now_ms(), "outcome": format!("{outcome:?}")});
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, record.to_string())
        .with_context(|| format!("write {}", temp.display()))?;
    std::fs::rename(&temp, path).with_context(|| format!("replace {}", path.display()))
}

#[cfg(test)]
mod tests;
