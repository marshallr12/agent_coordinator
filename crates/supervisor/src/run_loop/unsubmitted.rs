//! Launches that ended without a submission. A launch whose harness exits 0
//! although the agent neither submitted nor released its attempt (a broken
//! shell, a refused tool, an agent that gave up) used to be released with a
//! bare exit code, its run directory removed, and the same task claimed again
//! on the next poll, for as long as the host stayed broken. Now each such
//! launch is counted in the root-owned `<state_dir>/unsubmitted.json`
//! (consecutive ones since the last submission, per task and in total), the
//! loop stops claiming a task, or anything, once `[run] empty_exit_limit` or
//! `[run] empty_exit_total_limit` is reached, and the launch's final message
//! goes into the release handoff while a bounded tail of its events and
//! stderr is kept root-only under `<state_dir>/unsubmitted/<session>/`.
use super::{Driver, Launch, RunConfig, lease};
use crate::config::Config;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// The most characters of the agent's final message put in a handoff.
pub const MAX_FINAL_CHARS: usize = 1000;
/// The most bytes kept of a launch's events, and of its stderr.
pub const MAX_RETAINED_BYTES: usize = 1024 * 1024;
/// How many launches' retained logs are kept; older ones are removed.
pub const KEEP_RETAINED: usize = 20;

/// Consecutive launches that ended without a submission, since the last
/// launch that submitted.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Strikes {
    pub total: u32,
    pub tasks: BTreeMap<String, u32>,
}

/// `<state_dir>/unsubmitted.json`.
pub fn path(config: &Config) -> PathBuf {
    config.state_dir.join("unsubmitted.json")
}

/// `<state_dir>/unsubmitted`, where the logs of such launches are kept.
pub fn retained_dir(config: &Config) -> PathBuf {
    config.state_dir.join("unsubmitted")
}

impl Strikes {
    /// The counts on disk; none when the file is missing or unreadable.
    pub fn load(config: &Config) -> Self {
        let Ok(bytes) = fs::read(path(config)) else {
            return Self::default();
        };
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            eprintln!(
                "agentc-supervisor run: unreadable {}: {error}",
                path(config).display()
            );
            Self::default()
        })
    }

    /// Writes the counts atomically (a mode 0600 temporary file, then rename).
    fn save(&self, config: &Config) -> Result<()> {
        let target = path(config);
        fs::create_dir_all(&config.state_dir).context("create the state directory")?;
        let temp = target.with_extension("tmp");
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        std::io::Write::write_all(&mut options.open(&temp)?, &serde_json::to_vec(self)?)?;
        fs::rename(&temp, &target).with_context(|| format!("replace {}", target.display()))
    }

    /// Why `task` must not be claimed, if `[run] empty_exit_limit` launches
    /// of it in a row ended without a submission (0 turns the check off).
    pub fn task_hold(&self, settings: &RunConfig, config: &Config, task: &str) -> Option<String> {
        let count = self.tasks.get(task).copied().unwrap_or(0);
        let limit = settings.empty_exit_limit;
        (limit > 0 && count >= limit).then(|| {
            format!(
                "task {task} is held: {count} launches of it in a row exited without a submission (limit {limit}); remove {} to claim it again",
                path(config).display()
            )
        })
    }

    /// Why nothing must be claimed, if `[run] empty_exit_total_limit`
    /// launches in a row ended without a submission (0 turns the check off).
    pub fn total_hold(&self, settings: &RunConfig, config: &Config) -> Option<String> {
        let limit = settings.empty_exit_total_limit;
        (limit > 0 && self.total >= limit).then(|| {
            format!(
                "{} launches in a row exited without a submission (limit {limit}); fix the host, then remove {} to claim again",
                self.total,
                path(config).display()
            )
        })
    }
}

/// Forgets the counts of `task` and the total: a launch submitted.
pub fn submitted(config: &Config, task: &str) {
    let mut strikes = Strikes::load(config);
    if strikes.total == 0 && !strikes.tasks.contains_key(task) {
        return;
    }
    strikes.total = 0;
    strikes.tasks.remove(task);
    if let Err(error) = strikes.save(config) {
        eprintln!("agentc-supervisor run: {error:#}");
    }
}

/// What the handoff of a launch that ended without a submission adds: the
/// strike it counts (when it exited 0 by itself), where its logs are kept
/// and the agent's final message. Empty for a launch that never ran.
pub fn evidence(
    driver: &impl Driver,
    config: &Config,
    launch: &Launch,
    result: &Result<lease::Ended>,
) -> String {
    let Ok(ended) = result else {
        return String::new();
    };
    let events = driver.events(launch).unwrap_or_default();
    let stderr = driver.stderr(launch).unwrap_or_default();
    let mut text = String::new();
    match final_message(&events) {
        Some(message) => text.push_str(&format!(" The agent's final message: \"{message}\".")),
        None => text.push_str(" The launch wrote no final message."),
    }
    match retain(config, launch, &events, &stderr) {
        Ok(dir) => text.push_str(&format!(
            " Its events and stderr (tails) are kept, root-only, in {} on the host.",
            dir.display()
        )),
        Err(error) => eprintln!("agentc-supervisor run: keep launch logs: {error:#}"),
    }
    if ended.code == 0 && ended.drained.is_none() {
        text.push_str(&strike(config, launch));
    }
    text
}

/// Counts an exit without a submission against the launch's task; returns
/// the sentence saying so, and when the task or the host is now held.
fn strike(config: &Config, launch: &Launch) -> String {
    let task = &launch.suggestion.task;
    let mut strikes = Strikes::load(config);
    strikes.total += 1;
    let count = strikes.tasks.entry(task.clone()).or_insert(0);
    *count += 1;
    let count = *count;
    if let Err(error) = strikes.save(config) {
        eprintln!("agentc-supervisor run: {error:#}");
    }
    let mut text = format!(
        " Launch {count} in a row for this task (and {} in total on this host) exited 0 without submitting.",
        strikes.total
    );
    let held = strikes.task_hold(&config.run, config, task);
    if let Some(held) = held.or_else(|| strikes.total_hold(&config.run, config)) {
        eprintln!("agentc-supervisor run: {held}");
        text.push_str(" The supervisor will not claim it again: the host may be broken.");
    }
    text
}

/// The agent's last message in a harness event stream: Claude's final
/// `result` (else its last `assistant` text) or Codex's last `agent_message`,
/// with control characters replaced and cut to [`MAX_FINAL_CHARS`].
pub fn final_message(events: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(events);
    let mut last = None;
    for event in text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
    {
        let message = match event["type"].as_str() {
            Some("result") => event["result"].as_str().map(str::to_owned),
            Some("assistant") => event["message"]["content"]
                .as_array()
                .and_then(|blocks| blocks.iter().rev().find(|b| b["type"] == "text"))
                .and_then(|block| block["text"].as_str().map(str::to_owned)),
            Some("item.completed") if event["item"]["type"] == "agent_message" => {
                event["item"]["text"].as_str().map(str::to_owned)
            }
            _ => None,
        };
        last = message.filter(|m| !m.trim().is_empty()).or(last);
    }
    let clean: String = last?
        .trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut shown: String = clean.chars().take(MAX_FINAL_CHARS).collect();
    if clean.chars().count() > MAX_FINAL_CHARS {
        shown.push('…');
    }
    Some(shown)
}

/// The last [`MAX_RETAINED_BYTES`] of `bytes`, starting at a line.
fn tail(bytes: &[u8]) -> &[u8] {
    if bytes.len() <= MAX_RETAINED_BYTES {
        return bytes;
    }
    let cut = &bytes[bytes.len() - MAX_RETAINED_BYTES..];
    cut.iter()
        .position(|b| *b == b'\n')
        .map_or(cut, |at| &cut[at + 1..])
}

/// Keeps a tail of the launch's events and stderr in
/// `<state_dir>/unsubmitted/<session>/` (mode 0700, files 0600, owned by
/// the supervisor), then removes all but the newest [`KEEP_RETAINED`] such
/// directories. Only the two logs are copied, never the role's home or
/// credentials.
fn retain(config: &Config, launch: &Launch, events: &[u8], stderr: &[u8]) -> Result<PathBuf> {
    let dir = retained_dir(config).join(launch.session_id.to_string());
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
        .create(&dir)
        .with_context(|| format!("create {}", dir.display()))?;
    for (name, bytes) in [("events.jsonl", events), ("stderr.log", stderr)] {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let file = dir.join(name);
        let mut out = options
            .open(&file)
            .with_context(|| format!("create {}", file.display()))?;
        std::io::Write::write_all(&mut out, tail(bytes))
            .with_context(|| format!("write {}", file.display()))?;
    }
    prune(config);
    Ok(dir)
}

/// Removes the oldest retained directories beyond [`KEEP_RETAINED`].
fn prune(config: &Config) {
    let Ok(entries) = fs::read_dir(retained_dir(config)) else {
        return;
    };
    let mut dirs: Vec<_> = entries
        .flatten()
        .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
        .collect();
    dirs.sort();
    let extra = dirs.len().saturating_sub(KEEP_RETAINED);
    for (_, old) in dirs.into_iter().take(extra) {
        if let Err(error) = fs::remove_dir_all(&old) {
            eprintln!("agentc-supervisor run: prune {}: {error}", old.display());
        }
    }
}
