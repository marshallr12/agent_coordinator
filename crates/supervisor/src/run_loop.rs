//! Live mode (autonomy plan P3b pilot core): poll `next` as the implementer,
//! claim the suggested task, create the per-launch clone and `$RUN`, render
//! the role contract into the prompt, run `launch-root`, then remove the clone
//! and run directory once the launch is terminal.
//!
//! The loop only sequences steps; everything that talks to the coordinator or
//! runs commands sits behind [`Driver`], so tests drive it with a fake.
//! Extension points for the follow-up tasks: [`refusal`] (disk),
//! [`health::admit`] (kill switch, caps, vendor health) and [`review_hook`]
//! (reviewer launches and verdicts, in [`review`]); [`cost`] records what
//! each launch used.
//! [`lease`] renews a running launch's attempt; [`record`] persists launch
//! identity and releases attempts on exit, failure, drain and recovery;
//! [`renewal`] tells a renewal refused for good from a transient failure.
pub mod binding;
pub mod cost;
pub mod health;
pub mod lease;
#[cfg(target_os = "linux")]
pub mod live;
#[cfg(target_os = "linux")]
mod live_health;
#[cfg(target_os = "linux")]
mod live_review;
pub mod record;
pub mod renewal;
pub mod review;
pub mod review_cost;
#[cfg(target_os = "linux")]
mod rooted;

use crate::config::Config;
use crate::profile::{Harness, Role};
use anyhow::{Context, Result, ensure};
use lease::Lease;
use record::LaunchRecord;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

/// The implementer role contract; `render_prompt` fills its placeholders.
pub const IMPLEMENTER_CONTRACT: &str = include_str!("../contracts/implementer.md");
/// Repository files copied into the prompt as delimited data.
pub const INSTRUCTION_FILES: [&str; 2] = ["AGENTS.md", "CONTRIBUTING.md"];
/// The marker `launch` writes in `$RUN` before spawning the harness.
pub const STARTED_MARKER: &str = ".state-started";
/// The marker `launch` writes in `$RUN` once the harness has exited.
pub const TERMINAL_MARKER: &str = ".state-terminal.json";
/// The most bytes of one instruction file copied into the prompt.
pub const MAX_INSTRUCTION_BYTES: usize = 32 * 1024;

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
    /// Permit plain HTTP to a loopback coordinator (staging); an `https`
    /// or non-loopback origin never gets the flag ([`binding::insecure`]).
    pub allow_insecure_loopback: bool,
    /// The coordinator and project to work on instead of the mirror
    /// branch's `.agent-coordinator.toml` (a staging coordinator).
    pub binding: Option<binding::Binding>,
    /// A second project, normally the host's canary project, claimed from
    /// alongside the main one on the same coordinator. Its ready tasks are
    /// claimed first. It needs its own repository mirror, and an integrator
    /// instance of its own (see the deployment chapter).
    pub canary_binding: Option<binding::CanaryBinding>,
    /// Commit author and committer name set in each implementer clone.
    pub git_name: String,
    /// Commit author and committer email set in each implementer clone.
    pub git_email: String,
    /// Also claim, launch and decide reviewer work (see [`review`]).
    pub reviewer: bool,
    /// Stop claiming a submission after this many failed verdicts in a row
    /// (until the loop restarts).
    pub review_attempts: u32,
    /// Stop renewing a launch's attempt after this many minutes.
    pub budget_minutes: u64,
    /// On stop, how long a launch may take to exit after SIGTERM before
    /// SIGKILL.
    pub drain_seconds: u64,
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
            binding: None,
            canary_binding: None,
            git_name: "agentc implementer".into(),
            git_email: "agentc-impl@agentc.invalid".into(),
            reviewer: false,
            review_attempts: 3,
            budget_minutes: 240,
            drain_seconds: 30,
        }
    }
}

/// The heartbeat file `run` rewrites after every poll and, while a launch
/// runs, every [`lease::BEAT_EVERY`].
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
    /// The harness, model and effort it runs with ([`health::admit`]).
    pub vendor: health::Vendor,
}

impl Launch {
    /// The attempt's prepared, registered worktree inside the clone.
    pub fn checkout(&self) -> PathBuf {
        self.clone.join(crate::clone::CHECKOUT_DIR)
    }

    /// A launch with a fresh session id, its clone and `$RUN` under the
    /// implementer's `clones/` and `runs/`.
    pub fn plan(config: &Config, project: &str, suggestion: Suggestion) -> Self {
        Self::at(config, project, suggestion, Uuid::new_v4())
    }

    /// The launch of session `session_id`, whose clone and `$RUN` are
    /// named after it.
    pub fn at(config: &Config, project: &str, suggestion: Suggestion, session_id: Uuid) -> Self {
        let role = config.state_dir.join(Role::Implementer.slug());
        Self {
            project: project.to_owned(),
            suggestion,
            session_id,
            clone: role.join("clones").join(session_id.to_string()),
            run: role.join("runs").join(session_id.to_string()),
            vendor: health::Vendor::primary(&config.run),
        }
    }
}

/// What the loop needs from the coordinator and the host.
pub trait Driver {
    /// The project this host works on: the selected one (see [`Driver::select`]).
    fn project(&self) -> &str;
    /// Every project the host claims from, the main one first; more than one
    /// only with `[run.canary_binding]`.
    fn projects(&self) -> Vec<String> {
        vec![self.project().to_owned()]
    }
    /// Makes `project` (one of [`Driver::projects`]) the one `project`,
    /// `next` and the reviewer side act on. A single-project driver has
    /// nothing to switch.
    fn select(&mut self, _project: &str) {}
    /// `next` data for `role`.
    fn next(&mut self, role: Role) -> Result<Value>;
    /// Free bytes on the filesystem holding the state directory.
    fn free_bytes(&self) -> Result<u64>;
    /// Creates the clone and the prepared `$RUN`, owned by the role.
    fn create(&mut self, launch: &Launch) -> Result<()>;
    /// The repository's instruction files (name, text) at the cloned
    /// revision, read from a source the role cannot change.
    fn instructions(&mut self, launch: &Launch) -> Vec<(String, String)>;
    /// Writes the rendered prompt into `$RUN`, readable by the role.
    fn install_prompt(&mut self, launch: &Launch, prompt: &str) -> Result<()>;
    /// Claims the task in the launch's own coordinator session.
    fn claim(&mut self, launch: &Launch) -> Result<Lease>;
    /// Prepares and registers [`Launch::checkout`] for the claimed attempt
    /// in the launch's session, so the agent can submit code from it.
    fn register(&mut self, launch: &Launch, lease: &Lease) -> Result<()>;
    /// Spawns `launch-root` for the launch; returns its pid and `/proc`
    /// start time.
    fn start(&mut self, launch: &Launch) -> Result<(u32, Option<u64>)>;
    /// The running launch's exit code once it has exited.
    fn exited(&mut self) -> Option<i32>;
    /// Asks the running launch to stop (SIGTERM), or kills it (SIGKILL).
    fn signal(&mut self, kill: bool);
    /// When the launch's harness last wrote an event (ms since the epoch).
    fn last_event_ms(&self, launch: &Launch) -> Option<i64>;
    /// Rewrites the heartbeat while `launch` runs ([`beat_busy`]); a failure
    /// is logged, not fatal.
    fn heartbeat(&mut self, _launch: &Launch) {}
    /// Renews the attempt in the launch's session.
    fn renew(&mut self, launch: &Launch, lease: &Lease) -> Result<Lease>;
    /// The launch attempt's state (`active`, `submitted`, `released`, ...)
    /// from the task detail; `None` when it cannot be read.
    fn attempt_state(&mut self, launch: &Launch, lease: &Lease) -> Option<String>;
    /// Releases the attempt in the launch's session with a handoff
    /// `summary`; an attempt that is no longer active counts as released.
    fn release(&mut self, launch: &Launch, lease: &Lease, summary: &str) -> Result<()>;
    /// This boot's id.
    fn boot_id(&self) -> String;
    /// Whether a recorded launch may still be running.
    fn may_be_alive(&self, record: &LaunchRecord) -> bool;
    /// Whether the launch's `$RUN` holds the regular file `name` (a state
    /// marker), never following a symlink.
    fn has_marker(&self, launch: &Launch, name: &str) -> bool;
    /// Removes the launch's clone and run directory.
    fn discard(&mut self, launch: &Launch) -> Result<()>;
    /// Milliseconds since the epoch.
    fn now_ms(&self) -> i64;
    /// Sleeps for `duration`.
    fn pause(&mut self, duration: Duration);
    /// Whether the host asked the loop to stop (SIGTERM or SIGINT).
    fn stopping(&self) -> bool;
    /// The reviewer side, when this host runs reviews.
    fn reviewer(&mut self) -> Option<&mut dyn review::ReviewDriver> {
        None
    }
    /// Whether `harness` is signed in (`claude auth status` / `codex login
    /// status`); hosts without the check report healthy.
    fn harness_status(&mut self, _harness: Harness) -> Result<()> {
        Ok(())
    }
    /// When `harness`'s credential expires (ms since the epoch), if known.
    fn credential_expiry_ms(&self, _harness: Harness) -> Option<i64> {
        None
    }
    /// The launch's harness event stream, `$RUN/events.jsonl`.
    fn events(&self, launch: &Launch) -> Option<Vec<u8>> {
        std::fs::read(launch.run.join("events.jsonl")).ok()
    }
}

/// What one poll did; recorded in the heartbeat.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Idle,
    Refused(String),
    Launched { task: String, exit_code: i32 },
    Failed(String),
}

/// Polls until stopped (or once), writing the heartbeat after every poll
/// (and while a launch runs, see [`beat_busy`]).
pub fn run(driver: &mut impl Driver, config: &Config, once: bool) -> Result<()> {
    sweep_terminal(driver, config);
    record::reset_retries(config);
    for iteration in 1_u64.. {
        let outcome = iterate(driver, config);
        if outcome != Outcome::Idle {
            eprintln!("agentc-supervisor run: {outcome:?}");
        }
        if let Err(error) = beat(&heartbeat_path(config), iteration, &outcome) {
            eprintln!("agentc-supervisor run: heartbeat: {error:#}");
        }
        if once || idle(driver, config.run.poll_seconds.max(5)) {
            break;
        }
    }
    Ok(())
}

/// Waits `seconds` between polls; true when a stop request ends the wait.
fn idle(driver: &mut impl Driver, seconds: u64) -> bool {
    for _ in 0..seconds {
        if driver.stopping() {
            return true;
        }
        driver.pause(Duration::from_secs(1));
    }
    driver.stopping()
}

/// Removes every terminal implementer run, with its clone, that an earlier
/// loop left behind (it stopped between a launch's exit and its cleanup).
/// Runs with a launch record are left to [`record::recover`].
pub fn sweep_terminal(driver: &mut impl Driver, config: &Config) {
    let role = config.state_dir.join(Role::Implementer.slug());
    let Ok(entries) = std::fs::read_dir(role.join("runs")) else {
        return;
    };
    let names = entries.flatten().map(|entry| entry.file_name());
    for session in names.filter_map(|name| Uuid::parse_str(name.to_str()?).ok()) {
        let unknown = Suggestion {
            task: String::new(),
            revision: 0,
            title: String::new(),
        };
        let launch = Launch::at(config, driver.project(), unknown, session);
        if LaunchRecord::load(config, &session).is_none()
            && driver.has_marker(&launch, TERMINAL_MARKER)
            && let Err(error) = driver.discard(&launch)
        {
            eprintln!("agentc-supervisor run: sweep: {error:#}");
        }
    }
}

/// One poll: recovery, admission, `next`, then the launch it suggests. With
/// more than one project, reviews are polled in each, then `next` in each
/// of the extra (canary) projects before the main one, so a ready canary
/// task is claimed ahead of any other work.
pub fn iterate(driver: &mut impl Driver, config: &Config) -> Outcome {
    let projects = driver.projects();
    let main = projects.first().cloned().unwrap_or_default();
    driver.select(&main);
    if let Some(reason) = record::recover(driver, config) {
        return Outcome::Refused(reason);
    }
    driver.select(&main);
    if let Some(reason) = refusal(driver, &config.run) {
        return Outcome::Refused(reason);
    }
    let vendor = match health::admit(driver, config) {
        Ok(vendor) => vendor,
        Err(reason) => return Outcome::Refused(reason),
    };
    if config.run.reviewer {
        for project in &projects {
            driver.select(project);
            review_hook(driver, config);
        }
    }
    let mut failure = None;
    for project in projects.iter().skip(1).chain(projects.first()) {
        driver.select(project);
        let next = match driver.next(Role::Implementer) {
            Ok(next) => next,
            Err(error) => {
                failure.get_or_insert(format!("next: {error:#}"));
                continue;
            }
        };
        let Some(suggestion) = suggestion(&next) else {
            continue;
        };
        let launch = Launch {
            vendor,
            ..Launch::plan(config, project, suggestion)
        };
        let result = work(driver, config, &launch);
        let outcome = finish(driver, config, &launch, result);
        driver.select(&main);
        return outcome;
    }
    driver.select(&main);
    failure.map_or(Outcome::Idle, Outcome::Failed)
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

/// Runs one review poll when the driver has a reviewer side and the
/// reviewer's daily cap allows it; without a side it only reports reviewer
/// work it cannot take.
pub fn review_hook(driver: &mut impl Driver, config: &Config) {
    if let Some(reviewer) = driver.reviewer() {
        reviewer.recover();
    }
    if let Err(reason) = health::capped(config, Role::Reviewer, driver.now_ms()) {
        eprintln!("agentc-supervisor run: no review: {reason}");
        return;
    }
    let Some(reviewer) = driver.reviewer() else {
        if let Ok(next) = driver.next(Role::Reviewer)
            && !next["action"].is_null()
        {
            eprintln!("agentc-supervisor run: reviewer work needs a reviewer side");
        }
        return;
    };
    let outcome = review::review(reviewer, &config.run);
    if outcome != review::ReviewOutcome::Idle {
        eprintln!("agentc-supervisor run: review: {outcome:?}");
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

/// Creates, claims, registers the checkout, prompts and launches, stopping
/// at the first failure. Once claimed, the attempt is recorded before
/// anything else and released with a handoff however the launch ends,
/// unless the agent submitted it (the submission ended it already).
fn work(driver: &mut impl Driver, config: &Config, launch: &Launch) -> Result<i32> {
    driver.create(launch).context("create clone and run")?;
    ensure!(!driver.stopping(), "the loop is stopping; no new claims");
    let lease = driver.claim(launch).context("claim")?;
    let mut record = LaunchRecord::new(launch, &lease, driver.boot_id(), driver.now_ms());
    let result = (record.save(config).context("record the launch"))
        .and_then(|()| run_claimed(driver, config, launch, &mut record, lease));
    let cost = record::settle_cost(driver, config, launch, &mut record);
    if submitted(driver, launch, &record, &result) {
        record::mark_submitted(config, &mut record);
    } else {
        let summary = handoff(&result, &cost);
        record::release(driver, config, launch, &mut record, &summary);
    }
    result.map(|ended| ended.code)
}

/// Whether the agent submitted the attempt: a refused renewal showed it, or
/// the attempt's state says so once the launch exited by itself. A launch
/// that never ran (`Err`) cannot have submitted.
fn submitted(
    driver: &mut impl Driver,
    launch: &Launch,
    record: &LaunchRecord,
    result: &Result<lease::Ended>,
) -> bool {
    let Ok(ended) = result else {
        return false;
    };
    ended.submitted
        || driver
            .attempt_state(launch, &record.lease())
            .is_some_and(|state| state == "submitted")
}

/// Registers the claimed attempt's checkout and installs the prompt that
/// names it and the attempt.
fn prepare_claimed(driver: &mut impl Driver, launch: &Launch, lease: &Lease) -> Result<()> {
    driver
        .register(launch, lease)
        .context("register the checkout")?;
    let prompt = render_prompt(launch, lease, &driver.instructions(launch));
    driver.install_prompt(launch, &prompt).context("prompt")
}

/// Prepares the claimed launch, spawns it, adds its identity to the record
/// and supervises it until it exits.
fn run_claimed(
    driver: &mut impl Driver,
    config: &Config,
    launch: &Launch,
    record: &mut LaunchRecord,
    lease: Lease,
) -> Result<lease::Ended> {
    prepare_claimed(driver, launch, &lease)?;
    let (pid, ticks) = driver.start(launch).context("launch")?;
    (record.pid, record.start_ticks) = (Some(pid), ticks);
    if let Err(error) = record.save(config) {
        eprintln!("agentc-supervisor run: {error:#}");
    }
    Ok(lease::supervise(driver, launch, lease, &config.run))
}

/// The handoff summary a launch's attempt is released with, carrying the
/// launch's `cost` sentence.
fn handoff(result: &Result<lease::Ended>, cost: &str) -> String {
    let (how, why) = match result {
        Ok(ended) => (
            format!("the launch exited with code {}", ended.code),
            (ended.drained.as_ref()).map_or(String::new(), |why| format!(" after a drain ({why})")),
        ),
        Err(error) => (format!("the launch failed: {error:#}"), String::new()),
    };
    let text = format!(
        "agentc-supervisor released the attempt: {how}{why}. {cost} Read the last checkpoint before resuming."
    );
    text.chars().take(4000).collect()
}

/// Removes the launch's clone and run directory when it is safe, with its
/// record, and turns the launch result into an outcome. An attempt whose
/// release failed keeps all three for [`record::recover`] to retry.
fn finish(
    driver: &mut impl Driver,
    config: &Config,
    launch: &Launch,
    result: Result<i32>,
) -> Outcome {
    let unreleased = LaunchRecord::load(config, &launch.session_id).is_some_and(|r| !r.released);
    if !unreleased {
        match remove_finished(driver, launch) {
            Ok(true) => LaunchRecord::remove(config, &launch.session_id),
            Ok(false) => {}
            Err(error) => eprintln!("agentc-supervisor run: cleanup: {error:#}"),
        }
    }
    match result {
        Ok(exit_code) => Outcome::Launched {
            task: launch.suggestion.task.clone(),
            exit_code,
        },
        Err(error) => Outcome::Failed(format!("{}: {error:#}", launch.suggestion.task)),
    }
}

/// Removes the launch's clone and run when the run is terminal or never
/// started. A started run without a terminal record may still have a live
/// harness, so both are kept for recovery.
pub fn remove_finished(driver: &mut impl Driver, launch: &Launch) -> Result<bool> {
    if driver.has_marker(launch, STARTED_MARKER) && !driver.has_marker(launch, TERMINAL_MARKER) {
        return Ok(false);
    }
    driver.discard(launch)?;
    Ok(true)
}

/// The prompt: the implementer contract with the launch and its claimed
/// attempt filled in (the title inside `<task-title>` tags), then each
/// repository file inside `<repository-instructions>` tags. Closing tags in
/// the data are defused, whatever their case, so the data cannot end its
/// own block.
pub fn render_prompt(launch: &Launch, lease: &Lease, files: &[(String, String)]) -> String {
    let s = &launch.suggestion;
    let mut prompt = IMPLEMENTER_CONTRACT
        .replace("{{project}}", &launch.project)
        .replace("{{attempt}}", &lease.attempt)
        .replace("{{generation}}", &lease.generation.to_string())
        .replace("{{checkout}}", &launch.checkout().display().to_string())
        .replace("{{task_id}}", &s.task)
        .replace("{{task_revision}}", &s.revision.to_string())
        .replace(
            "{{task_title}}",
            &defuse(&s.title.replace(['\n', '\r'], " ")),
        )
        .replace("{{session}}", &launch.session_id.to_string());
    prompt.push_str(&instruction_blocks(files));
    prompt
}

/// Each repository file inside defused `<repository-instructions>` tags.
pub fn instruction_blocks(files: &[(String, String)]) -> String {
    let block = |(name, text): &(String, String)| {
        let text = defuse(text);
        format!("\n<repository-instructions file=\"{name}\">\n{text}\n</repository-instructions>\n")
    };
    files.iter().map(block).collect()
}

/// `text` with every `</` that opens a closing tag (any case) turned into
/// `<\/`, so neither data delimiter can be closed from inside the data.
pub fn defuse(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (at, _) in lower.match_indices("</") {
        let rest = &lower[at..];
        if rest.starts_with("</repository-instructions") || rest.starts_with("</task-title") {
            out.push_str(&text[last..at]);
            out.push_str("<\\/");
            last = at + 2;
        }
    }
    out.push_str(&text[last..]);
    out
}

/// Rewrites the heartbeat atomically: poll count, time and last outcome.
pub fn beat(path: &Path, iteration: u64, outcome: &Outcome) -> Result<()> {
    let record = json!({"pid": std::process::id(), "iteration": iteration,
                        "at_ms": crate::shadow::now_ms(), "outcome": format!("{outcome:?}")});
    write_beat(path, &record)
}

/// Refreshes the heartbeat's time while a `role` launch for `task` runs,
/// adding a `launch` entry that says so. The poll count and last outcome are
/// kept as the previous poll left them, and the next [`beat`] drops the
/// entry.
pub fn beat_busy(path: &Path, role: &str, task: &str) -> Result<()> {
    let last: Value = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let record = json!({"pid": std::process::id(),
                        "iteration": last["iteration"].as_u64().unwrap_or(0),
                        "at_ms": crate::shadow::now_ms(),
                        "outcome": last["outcome"].as_str().unwrap_or("Idle"),
                        "launch": {"role": role, "task": task}});
    write_beat(path, &record)
}

/// Replaces the heartbeat file with `record`.
fn write_beat(path: &Path, record: &Value) -> Result<()> {
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
