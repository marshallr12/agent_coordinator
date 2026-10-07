//! Launch identity and crash release (autonomy plan §2.3). After the claim
//! and before spawning `launch-root`, the loop writes a root-owned record,
//! `<state_dir>/launches/<session>.json`, holding the attempt and this
//! boot's id; once spawned it adds the launch's pid and `/proc` start time.
//! The record goes once the attempt is released and the run removed. Each
//! poll settles leftover records: a launch that may still run on this boot
//! is never respawned and blocks new claims; any other has its attempt
//! released and its clone and run removed. After [`MAX_RELEASE_TRIES`]
//! failed releases a record is left alone until the loop restarts.
//!
//! A record without a pid (the loop died between writing it and spawning,
//! or the spawn failed and so did the release) counts as alive only for
//! [`NO_PID_GRACE_MS`]. A record that stays stuck can be cleared by hand:
//! check that no `launch-root` for its session runs, release the attempt (or
//! let its lease lapse), then remove the record and the session's
//! `impl/clones/` and `impl/runs/` directories.
use super::health::Vendor;
use super::lease::Lease;
use super::{Driver, Launch, Suggestion};
use crate::config::Config;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use uuid::Uuid;

/// How long a record without a pid may still belong to a launch being
/// spawned.
pub const NO_PID_GRACE_MS: i64 = 2 * 60 * 1000;
/// Failed releases of one record before the loop stops retrying it.
pub const MAX_RELEASE_TRIES: u32 = 5;

/// The handoff a recovered launch's attempt is released with.
const RECOVERED: &str = "agentc-supervisor released the attempt: its launch ended while no \
supervisor watched it (crash or restart). Read the last checkpoint before resuming.";

/// One launch the loop claimed for, as persisted before and at spawn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchRecord {
    pub session_id: Uuid,
    pub task: String,
    pub attempt: String,
    pub generation: u64,
    pub boot_id: String,
    pub pid: Option<u32>,
    /// The pid's start time in clock ticks since boot (`/proc/<pid>/stat`).
    pub start_ticks: Option<u64>,
    pub released: bool,
    /// When the record was first written (ms since the epoch).
    #[serde(default)]
    pub recorded_ms: i64,
    /// Failed releases since the loop started.
    #[serde(default)]
    pub release_failures: u32,
    /// The vendor the launch ran with (absent in older records: `[run]`).
    #[serde(default)]
    pub vendor: Option<Vendor>,
    /// Whether the launch's cost is in the ledger, so it is never counted
    /// twice when a failed release is retried by recovery.
    #[serde(default)]
    pub cost_recorded: bool,
}

/// The root-owned directory of launch records, `<state_dir>/launches`.
pub fn dir(config: &Config) -> PathBuf {
    config.state_dir.join("launches")
}

impl LaunchRecord {
    /// The record of a launch claimed at `now` on boot `boot_id`, not
    /// spawned yet.
    pub fn new(launch: &Launch, lease: &Lease, boot_id: String, now: i64) -> Self {
        Self {
            session_id: launch.session_id,
            task: launch.suggestion.task.clone(),
            attempt: lease.attempt.clone(),
            generation: lease.generation,
            boot_id,
            pid: None,
            start_ticks: None,
            released: false,
            recorded_ms: now,
            release_failures: 0,
            vendor: Some(launch.vendor.clone()),
            cost_recorded: false,
        }
    }

    /// `<state_dir>/launches/<session>.json`.
    fn path(config: &Config, session: &Uuid) -> PathBuf {
        dir(config).join(format!("{session}.json"))
    }

    /// Writes the record atomically (a mode 0600 temporary file, then rename).
    pub fn save(&self, config: &Config) -> Result<()> {
        let path = Self::path(config, &self.session_id);
        fs::create_dir_all(dir(config)).context("create the launch record directory")?;
        let temp = path.with_extension("tmp");
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        std::io::Write::write_all(&mut options.open(&temp)?, &serde_json::to_vec(self)?)?;
        fs::rename(&temp, &path).with_context(|| format!("replace {}", path.display()))
    }

    /// The record of `session`, if one is readable.
    pub fn load(config: &Config, session: &Uuid) -> Option<Self> {
        serde_json::from_slice(&fs::read(Self::path(config, session)).ok()?).ok()
    }

    /// Every readable record; unreadable ones are logged and left alone.
    pub fn load_all(config: &Config) -> Vec<Self> {
        let Ok(entries) = fs::read_dir(dir(config)) else {
            return Vec::new();
        };
        let paths = entries.flatten().map(|entry| entry.path());
        let records = paths.filter(|path| path.extension().is_some_and(|e| e == "json"));
        let parse = |path: PathBuf| {
            let record = serde_json::from_slice(&fs::read(&path).ok()?).ok();
            record.or_else(|| {
                eprintln!("agentc-supervisor run: unreadable {}", path.display());
                None
            })
        };
        records.filter_map(parse).collect()
    }

    /// Removes the record of `session`.
    pub fn remove(config: &Config, session: &Uuid) {
        match fs::remove_file(Self::path(config, session)) {
            Err(error) if error.kind() != ErrorKind::NotFound => {
                eprintln!("agentc-supervisor run: remove launch record {session}: {error}");
            }
            _ => {}
        }
    }

    /// The launch the record describes, for releasing and removing it.
    pub fn launch(&self, config: &Config, project: &str) -> Launch {
        let suggestion = Suggestion {
            task: self.task.clone(),
            revision: 0,
            title: String::new(),
        };
        let launch = Launch::at(config, project, suggestion, self.session_id);
        match &self.vendor {
            Some(vendor) => Launch {
                vendor: vendor.clone(),
                ..launch
            },
            None => launch,
        }
    }

    /// The recorded attempt as a lease to release.
    pub fn lease(&self) -> Lease {
        Lease {
            attempt: self.attempt.clone(),
            generation: self.generation,
            renew_after_seconds: 60,
            progress_age_ms: 0,
        }
    }
}

/// This boot's id (`/proc/sys/kernel/random/boot_id`), empty if unreadable.
pub fn boot_id() -> String {
    let id = fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap_or_default();
    id.trim().to_owned()
}

/// When `pid` started, in clock ticks since boot: field 22 of
/// `/proc/<pid>/stat`, counted after the parenthesised command name.
pub fn start_ticks(pid: u32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields = stat.get(stat.rfind(')')? + 1..)?;
    fields.split_whitespace().nth(19)?.parse().ok()
}

/// Whether a recorded launch may still run at `now`: it was recorded on
/// this boot and its pid lives with the recorded start time. An unknown boot
/// or start time may be alive, and so may a record without a pid within
/// [`NO_PID_GRACE_MS`]; such launches are never respawned.
pub fn may_be_alive(
    record: &LaunchRecord,
    boot: &str,
    ticks: impl Fn(u32) -> Option<u64>,
    now: i64,
) -> bool {
    if boot.is_empty() || record.boot_id.is_empty() {
        return true;
    }
    if record.boot_id != boot {
        return false;
    }
    match (record.pid, record.start_ticks) {
        (Some(pid), Some(started)) => ticks(pid) == Some(started),
        (Some(_), None) => true,
        (None, _) => now.saturating_sub(record.recorded_ms) <= NO_PID_GRACE_MS,
    }
}

/// Releases the recorded attempt with `summary` and marks the record
/// released; a failure is counted on the record for the next poll to retry,
/// up to [`MAX_RELEASE_TRIES`].
pub fn release(
    driver: &mut impl Driver,
    config: &Config,
    launch: &Launch,
    record: &mut LaunchRecord,
    summary: &str,
) -> bool {
    let released = driver.release(launch, &record.lease(), summary);
    if let Err(error) = &released {
        eprintln!(
            "agentc-supervisor run: release {}: {error:#}",
            record.attempt
        );
        record.release_failures += 1;
        if record.release_failures == MAX_RELEASE_TRIES {
            eprintln!(
                "agentc-supervisor run: ERROR: gave up releasing attempt {} of launch {} after {MAX_RELEASE_TRIES} tries; it is retried after a restart, or clear it by hand",
                record.attempt, record.session_id
            );
        }
    }
    record.released = released.is_ok();
    if let Err(error) = record.save(config) {
        eprintln!("agentc-supervisor run: {error:#}");
    }
    record.released
}

/// Lets every record's release be retried again (the loop is starting).
pub fn reset_retries(config: &Config) {
    for mut record in LaunchRecord::load_all(config) {
        if record.release_failures > 0 {
            record.release_failures = 0;
            if let Err(error) = record.save(config) {
                eprintln!("agentc-supervisor run: {error:#}");
            }
        }
    }
}

/// Settles every launch an earlier poll or loop left recorded; returns why
/// claims must wait while one of them may still run.
pub fn recover(driver: &mut impl Driver, config: &Config) -> Option<String> {
    let mut running = Vec::new();
    for mut record in LaunchRecord::load_all(config) {
        if driver.may_be_alive(&record) {
            running.push(record.session_id.to_string());
        } else if record.release_failures < MAX_RELEASE_TRIES {
            settle(driver, config, &mut record);
        }
    }
    let list = running.join(", ");
    (!running.is_empty()).then(|| format!("an earlier launch may still run ({list})"))
}

/// Records a dead launch's cost (and any 429) unless that is done, releases
/// its attempt unless that is done, then removes its clone, run and record.
fn settle(driver: &mut impl Driver, config: &Config, record: &mut LaunchRecord) {
    let launch = record.launch(config, driver.project());
    let cost = settle_cost(driver, config, &launch, record);
    let summary = format!("{RECOVERED} {cost}").trim_end().to_owned();
    if !record.released && !release(driver, config, &launch, record, &summary) {
        return;
    }
    match driver.discard(&launch) {
        Ok(()) => LaunchRecord::remove(config, &record.session_id),
        Err(error) => eprintln!("agentc-supervisor run: recovery cleanup: {error:#}"),
    }
}

/// Settles the launch's cost from its events once (see [`super::cost::settle`])
/// and marks the record so a retry never counts it again; returns the
/// handoff's cost sentence (empty when it was already recorded).
pub fn settle_cost(
    driver: &impl Driver,
    config: &Config,
    launch: &Launch,
    record: &mut LaunchRecord,
) -> String {
    if record.cost_recorded {
        return String::new();
    }
    let sentence = super::cost::settle(driver, config, launch, record);
    record.cost_recorded = true;
    if let Err(error) = record.save(config) {
        eprintln!("agentc-supervisor run: {error:#}");
    }
    sentence
}
