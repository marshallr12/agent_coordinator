//! The runner: one launcher process driven by typed requests, one JSON
//! object per line on standard input, each answered by one line on standard
//! output. A request names an operation and its typed arguments; none of
//! them is a command line for a shell. The runner offers no way to start a
//! process outside the boundary.
//!
//! Every launch is journaled before it starts ([`super::journal`]). When the
//! runner's input ends, the runner exits, its job handles close and Windows
//! ends every launch it held.
use super::desktop::StationNotOurs;
use super::journal::{Entry, Journal, Stage};
use super::launcher::{LaunchSpec, Launcher, Seen, job_owner};
use super::process::Command;
use super::reader::{PathRefused, validated_read};
use super::recovery;
use super::text::wide;
use crate::host_runtime::Recovery;
use crate::service::drain::Fleet;
use crate::service::lifecycle::{Kind, Refused};
use crate::service::startup::{Judged, Ledger, Verdict};
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use windows_sys::Win32::Security::Isolation::DeleteAppContainerProfile;

/// One request to the runner.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Who the runner is: its account and the owner identity of its jobs.
    Identity,
    /// Create a launch's profile, directories and desktop.
    Prepare {
        launch: String,
        spec: LaunchSpec,
        #[serde(default)]
        source: Option<String>,
        #[serde(default)]
        attempt: Option<String>,
        #[serde(default)]
        generation: Option<u64>,
    },
    /// Open a file with an inheritable handle held for the launch.
    HoldFile { launch: String, path: PathBuf },
    /// Start a process for the prepared launch.
    Start {
        launch: String,
        argv: Vec<String>,
        directory: PathBuf,
        environment: BTreeMap<String, String>,
        output: PathBuf,
        #[serde(default)]
        listed: Vec<u64>,
    },
    /// Wait up to `wait_ms` for the launch's root process to end.
    Observe { launch: String, wait_ms: u64 },
    /// Put the stop marker in the launch's control directory.
    RequestStop { launch: String },
    /// End every process of the launch.
    Terminate { launch: String },
    /// Watch a process the launch reported, if it is in the launch's job.
    Watch { launch: String, pid: u32 },
    /// Whether the watched process ended within `wait_ms`.
    WatchedEnded { launch: String, wait_ms: u64 },
    /// End the launch and delete its desktop and profile; with
    /// `remove_directories`, also its control and working directories.
    Cleanup {
        launch: String,
        #[serde(default)]
        remove_directories: bool,
    },
    /// Read a worker-written file by handle.
    Read {
        root: PathBuf,
        relative: PathBuf,
        limit: usize,
    },
    /// Judge every journaled launch without starting anything.
    Recover,
    /// Delete what an ended launch of an earlier runner left behind.
    Reclaim {
        launch: String,
        #[serde(default)]
        remove_directories: bool,
    },
}

impl Request {
    /// Whether the request begins a launch or acts on one that exists. A
    /// service that is ending refuses what begins one. Every request is
    /// named, so a new one cannot be added without deciding which it is.
    pub fn kind(&self) -> Kind {
        match self {
            Self::Prepare { .. } | Self::HoldFile { .. } | Self::Start { .. } => Kind::Begins,
            Self::Identity
            | Self::Observe { .. }
            | Self::RequestStop { .. }
            | Self::Terminate { .. }
            | Self::Watch { .. }
            | Self::WatchedEnded { .. }
            | Self::Cleanup { .. }
            | Self::Read { .. }
            | Self::Recover
            | Self::Reclaim { .. } => Kind::Continues,
        }
    }

    /// Shortens a wait the request asks for to at most `most_ms`, so a
    /// service that is ending keeps its own time. The driver sees the
    /// launch still running and asks again.
    pub fn limit_wait(&mut self, most_ms: u64) {
        if let Self::Observe { wait_ms, .. } | Self::WatchedEnded { wait_ms, .. } = self {
            *wait_ms = (*wait_ms).min(most_ms);
        }
    }
}

/// A launcher, its journal and the launches this run prepared.
pub struct Runner {
    launcher: Launcher,
    journal: Journal,
    mine: BTreeSet<String>,
}

impl Runner {
    /// A runner journaling into `state`.
    pub fn new(state: &Path) -> Result<Self> {
        Ok(Self {
            launcher: Launcher::new()?,
            journal: Journal::open(state)?,
            mine: BTreeSet::new(),
        })
    }

    /// Carries out one request; the answer's fields depend on the request.
    pub fn handle(&mut self, request: Request) -> Result<Value> {
        match request {
            Request::Identity => self.identity(),
            Request::Prepare {
                launch,
                spec,
                source,
                attempt,
                generation,
            } => self.prepare(&launch, spec, (source, attempt, generation)),
            Request::HoldFile { launch, path } => {
                let handle = self.launcher.hold_file(&launch, &path)?;
                Ok(json!({ "handle": handle }))
            }
            Request::Start {
                launch,
                argv,
                directory,
                environment,
                output,
                listed,
            } => {
                let command = Command {
                    argv,
                    directory,
                    environment,
                    output,
                };
                self.start(&launch, &command, &listed)
            }
            other => self.handle_running(other),
        }
    }

    /// The requests that act on a launch already prepared or journaled.
    fn handle_running(&mut self, request: Request) -> Result<Value> {
        match request {
            Request::Observe { launch, wait_ms } => self.observe(&launch, wait_ms),
            Request::RequestStop { launch } => {
                self.launcher.request_stop(&launch)?;
                Ok(json!({}))
            }
            Request::Terminate { launch } => self.terminate(&launch),
            Request::Watch { launch, pid } => {
                let member = self.launcher.watch(&launch, pid)?;
                Ok(json!({ "alive_in_job": member }))
            }
            Request::WatchedEnded { launch, wait_ms } => {
                let patience = Duration::from_millis(wait_ms);
                let ended = self.launcher.watched_ended(&launch, patience)?;
                Ok(json!({ "ended": ended }))
            }
            Request::Cleanup {
                launch,
                remove_directories,
            } => self.cleanup(&launch, remove_directories),
            Request::Read {
                root,
                relative,
                limit,
            } => {
                let bytes = validated_read(&root, &relative, limit)?;
                Ok(json!({ "hex": hex::encode(bytes) }))
            }
            Request::Recover => self.recover(),
            Request::Reclaim {
                launch,
                remove_directories,
            } => self.reclaim(&launch, remove_directories),
            _ => bail!("the request is not handled here"),
        }
    }

    /// The runner's account and the owner identity of its jobs.
    fn identity(&self) -> Result<Value> {
        Ok(
            json!({ "account_sid": self.launcher.owner(), "job_owner": job_owner()?,
                   "pid": std::process::id() }),
        )
    }

    /// Prepares a launch and journals it. An id an earlier runner journaled
    /// is refused: that launch is judged by `recover`, never started again.
    fn prepare(
        &mut self,
        launch: &str,
        spec: LaunchSpec,
        (source, attempt, generation): (Option<String>, Option<String>, Option<u64>),
    ) -> Result<Value> {
        ensure!(
            self.journal.load(launch).is_none(),
            "launch {launch} is already journaled; recover it instead of preparing it again"
        );
        let profile = spec.profile.clone();
        let (control, work) = (spec.control.clone(), spec.work.clone());
        let prepared = self.launcher.prepare(launch, spec)?;
        let entry = Entry {
            launch: launch.to_owned(),
            stage: Stage::Prepared,
            profile,
            package_sid: prepared.package_sid.clone(),
            job_owner: prepared.job_owner.clone(),
            executable: None,
            source,
            attempt,
            generation,
            pid: None,
            created: None,
            control: Some(control),
            work: Some(work),
        };
        self.journal_or_undo(launch, &entry)?;
        Ok(serde_json::to_value(prepared)?)
    }

    /// Journals a just prepared launch; if that fails the launch is undone,
    /// so nothing exists that the journal does not name.
    fn journal_or_undo(&mut self, launch: &str, entry: &Entry) -> Result<()> {
        if let Err(error) = self.journal.save(entry) {
            let _ = self.launcher.cleanup(launch);
            return Err(error);
        }
        self.mine.insert(launch.to_owned());
        Ok(())
    }

    /// This run's journal entry for `launch`.
    fn entry(&self, launch: &str) -> Result<Entry> {
        ensure!(
            self.mine.contains(launch),
            "launch {launch} was not prepared by this runner"
        );
        (self.journal.load(launch)).with_context(|| format!("launch {launch} has no journal entry"))
    }

    /// Journals the intent to start, starts, then journals the identity
    /// Windows gave the launch. A refused start leaves nothing running.
    fn start(&mut self, launch: &str, command: &Command, listed: &[u64]) -> Result<Value> {
        let mut entry = self.entry(launch)?;
        ensure!(!self.launcher.runs(launch)?, "launch {launch} already runs");
        entry.stage = Stage::Starting;
        entry.executable = command.argv.first().cloned();
        (entry.pid, entry.created) = (None, None);
        self.journal.save(&entry)?;
        let started = match self.launcher.start(launch, command, listed) {
            Ok(started) => started,
            Err(error) => {
                entry.stage = Stage::Prepared;
                self.journal.save(&entry)?;
                return Err(error);
            }
        };
        entry.stage = Stage::Started;
        (entry.pid, entry.created) = (Some(started.pid), Some(started.created));
        self.journal.save(&entry)?;
        Ok(serde_json::to_value(started)?)
    }

    /// Whether the launch's root process ended within `wait_ms`.
    fn observe(&mut self, launch: &str, wait_ms: u64) -> Result<Value> {
        match self
            .launcher
            .observe(launch, Duration::from_millis(wait_ms))?
        {
            Seen::Running => Ok(json!({ "state": "running" })),
            Seen::Exited(code) => Ok(json!({ "state": "exited", "code": code })),
        }
    }

    /// Empties the launch's job and journals the end once Windows reports
    /// the job empty.
    fn terminate(&mut self, launch: &str) -> Result<Value> {
        let mut entry = self.entry(launch)?;
        let quiescent = self.launcher.terminate(launch)?;
        if quiescent {
            entry.stage = Stage::Ended;
            self.journal.save(&entry)?;
        }
        Ok(json!({ "quiescent": quiescent }))
    }

    /// Ends the launch, deletes its desktop and profile, then its entry.
    /// With `remove`, its two directories go first, and only once the
    /// launcher no longer holds the launch: a launch that could not be
    /// ended keeps its directories and its entry, and the request fails.
    fn cleanup(&mut self, launch: &str, remove: bool) -> Result<Value> {
        let entry = self.entry(launch)?;
        let deleted = self.launcher.cleanup(launch);
        let profile_deleted = deleted.is_ok();
        let detail = deleted.err().map(|error| format!("{error:#}"));
        if remove {
            let held = self.launcher.held().iter().any(|id| id == launch);
            ensure!(!held, "launch {launch} was not ended: {detail:?}");
            remove_directories(&entry)?;
        }
        self.journal.remove(launch)?;
        self.mine.remove(launch);
        Ok(
            json!({ "profile_deleted": profile_deleted, "detail": detail,
                   "directories_removed": remove }),
        )
    }

    /// Judges every journaled launch. This run's launches are reported as
    /// held; an earlier runner's are judged from their saved identities.
    fn recover(&self) -> Result<Value> {
        let (entries, unreadable) = self.journal.load_all()?;
        let mut launches = Vec::new();
        for entry in entries {
            let verdict = if self.mine.contains(&entry.launch) {
                "held"
            } else {
                verdict(&entry)
            };
            launches.push(json!({ "launch": entry.launch, "stage": entry.stage,
                                  "verdict": verdict, "pid": entry.pid }));
        }
        Ok(json!({ "launches": launches, "unreadable": unreadable }))
    }

    /// Deletes the profile and the entry of an earlier runner's launch,
    /// only once that launch is judged ended; with `remove`, also the
    /// directories its entry names.
    fn reclaim(&mut self, launch: &str, remove: bool) -> Result<Value> {
        ensure!(
            !self.mine.contains(launch),
            "launch {launch} is held; clean it up"
        );
        let entry = self
            .journal
            .load(launch)
            .context("no such journaled launch")?;
        ensure!(
            verdict(&entry) == "ended",
            "launch {launch} may still run; nothing of it is removed"
        );
        let name = wide(&entry.profile)?;
        // SAFETY: `name` is NUL-terminated.
        let result = unsafe { DeleteAppContainerProfile(name.as_ptr()) };
        if remove {
            remove_directories(&entry)?;
        }
        self.journal.remove(launch)?;
        Ok(json!({ "profile_deleted": result == 0, "directories_removed": remove }))
    }
}

/// Removes the control and working directories `entry` names. One that is
/// already gone counts as removed.
fn remove_directories(entry: &Entry) -> Result<()> {
    for directory in [&entry.control, &entry.work].into_iter().flatten() {
        match std::fs::remove_dir_all(directory) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                Err(error).with_context(|| format!("remove {}", directory.display()))?;
            }
            _ => {}
        }
    }
    Ok(())
}

impl Ledger for Runner {
    /// Every journaled launch with its verdict. A launch this run holds may
    /// run; any other is judged from its saved identities.
    fn judge_all(&self) -> Result<(Vec<Judged>, Vec<String>)> {
        let (entries, unreadable) = self.journal.load_all()?;
        let judged = entries.into_iter().map(|entry| {
            let ended = !self.mine.contains(&entry.launch) && verdict(&entry) == "ended";
            Judged {
                verdict: if ended {
                    Verdict::Ended
                } else {
                    Verdict::MayRun
                },
                launch: entry.launch,
            }
        });
        Ok((judged.collect(), unreadable))
    }

    fn reclaim(&mut self, launch: &str) -> Result<()> {
        Runner::reclaim(self, launch, false).map(|_| ())
    }
}

impl Fleet for Runner {
    /// The launches this run prepared whose end is not journaled.
    fn unconfirmed(&self) -> Vec<String> {
        let open = |launch: &&String| {
            let entry = self.journal.load(launch);
            entry.is_none_or(|entry| entry.stage != Stage::Ended)
        };
        self.mine.iter().filter(open).cloned().collect()
    }

    fn ask_stop(&mut self, launch: &str) -> Result<()> {
        self.launcher.request_stop(launch)
    }

    /// A launch never started has no root to wait for.
    fn root_ended(&mut self, launch: &str) -> bool {
        let unstarted = self
            .journal
            .load(launch)
            .is_some_and(|e| e.stage == Stage::Prepared);
        unstarted
            || matches!(
                self.launcher.observe(launch, Duration::ZERO),
                Ok(Seen::Exited(_))
            )
    }

    fn kill(&mut self, launch: &str) -> Result<()> {
        self.launcher.kill(launch)
    }

    /// Confirmed only when Windows reports the launch's job empty; the end
    /// is journaled before it is confirmed.
    fn emptied(&mut self, launch: &str) -> Result<bool> {
        if !self.launcher.emptied(launch)? {
            return Ok(false);
        }
        let mut entry = self.entry(launch)?;
        entry.stage = Stage::Ended;
        self.journal.save(&entry)?;
        Ok(true)
    }
}

/// `ended` or `may_be_running` for an entry this run does not hold.
fn verdict(entry: &Entry) -> &'static str {
    let root = entry.pid.map(|pid| (pid, entry.created));
    match recovery::judge(root, Some(&entry.job_owner), recovery::lookup) {
        Recovery::Ended => "ended",
        Recovery::MayBeRunning => "may_be_running",
    }
}

/// The answer line for a request's outcome: `ok` with the answer's fields,
/// or the error and, when it is one of the launcher's typed refusals, which.
pub fn answer(outcome: Result<Value>) -> Value {
    match outcome {
        Ok(Value::Object(mut fields)) => {
            fields.insert("ok".into(), json!(true));
            Value::Object(fields)
        }
        Ok(other) => json!({ "ok": true, "value": other }),
        Err(error) => {
            let refusal = if error.downcast_ref::<StationNotOurs>().is_some() {
                Some("station_not_ours")
            } else if error.downcast_ref::<PathRefused>().is_some() {
                Some("path_refused")
            } else if error.downcast_ref::<Refused>().is_some() {
                Some("service_stopping")
            } else {
                None
            };
            json!({ "ok": false, "error": format!("{error:#}"), "refusal": refusal })
        }
    }
}

/// Serves requests from `input` until it ends, answering each on `output`.
pub fn serve(input: impl BufRead, mut output: impl Write, state: &Path) -> Result<()> {
    let mut runner = Runner::new(state)?;
    for line in input.lines() {
        let line = line.context("read a request")?;
        if line.trim().is_empty() {
            continue;
        }
        let outcome = serde_json::from_str::<Request>(&line)
            .context("the request is not one the runner knows")
            .and_then(|request| runner.handle(request));
        writeln!(output, "{}", answer(outcome))?;
        output.flush()?;
    }
    Ok(())
}
