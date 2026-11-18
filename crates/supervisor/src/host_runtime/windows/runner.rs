//! The runner: one launcher process driven by typed requests, one JSON
//! object per line on standard input, each answered by one line on standard
//! output. A request names an operation and its typed arguments; none of
//! them is a command line for a shell. The runner offers no way to start a
//! process outside the boundary.
//!
//! Every launch is journaled before it starts ([`super::journal`]). When the
//! runner's input ends, the runner exits, its job handles close and Windows
//! ends every launch it held.
use super::broker::Role;
use super::candidate::{self, Candidate, CandidateSpec, Tuning};
use super::candidate_pipe::{self, Server};
use super::desktop::StationNotOurs;
use super::egress_admission::EgressRoute;
use super::journal::{CandidateRecord, Entry, Journal, Stage};
use super::launcher::{LaunchSpec, Launcher, Prepared, Seen, job_owner};
use super::process::Command;
use super::reader::{PathRefused, validated_read};
use super::recovery;
use super::text::wide;
use crate::candidate::capability::Runner as Readiness;
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
use std::sync::Arc;
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
        /// The side of the review the launch works on, journaled with it.
        #[serde(default)]
        role: Option<Role>,
        /// A reviewer launch's candidate, prepared with it.
        #[serde(default)]
        candidate: Option<Box<CandidateSpec>>,
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
    /// Whether the launch's candidate runner can run commands now: the
    /// readiness check of the capability record.
    CandidateReady { launch: String },
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
            | Self::CandidateReady { .. }
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

/// What the caller of `prepare` says a launch is for: the exact source
/// revision, the coordinator attempt and generation, and the side of the
/// review. Journaled, not checked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Purpose {
    source: Option<String>,
    attempt: Option<String>,
    generation: Option<u64>,
    role: Option<Role>,
}

impl Purpose {
    /// The journal entry of `launch`, just `prepared` in the AppContainer
    /// profile `profile` for this purpose, with no directories, candidate
    /// or start recorded yet.
    fn entry(self, launch: &str, profile: String, prepared: &Prepared) -> Entry {
        Entry {
            launch: launch.to_owned(),
            stage: Stage::Prepared,
            profile,
            package_sid: prepared.package_sid.clone(),
            job_owner: prepared.job_owner.clone(),
            executable: None,
            source: self.source,
            attempt: self.attempt,
            generation: self.generation,
            role: self.role,
            pid: None,
            created: None,
            control: None,
            work: None,
            candidate: None,
        }
    }
}

/// A launcher, its journal and the launches this run prepared.
pub struct Runner {
    launcher: Launcher,
    journal: Journal,
    mine: BTreeSet<String>,
    /// The candidates of this run's reviewer launches.
    candidates: BTreeMap<String, Served>,
    /// The egress broker every launch must be admitted by, if any.
    egress: Option<EgressRoute>,
    /// This run's launches the egress broker admitted.
    egressing: BTreeSet<String>,
}

/// A reviewer launch's candidate and the threads that serve its pipe.
struct Served {
    candidate: Arc<Candidate>,
    /// `None` once the threads were stopped and the candidate could not be
    /// retired yet.
    server: Option<Server>,
}

impl Served {
    /// Stops the pipe's threads, which ends a running command, and hands
    /// the candidate back alone when nothing else holds it.
    fn into_candidate(mut self) -> Result<Candidate, Self> {
        if let Some(server) = self.server.take() {
            server.stop();
        }
        Arc::try_unwrap(self.candidate).map_err(|candidate| Self {
            candidate,
            server: None,
        })
    }
}

impl Runner {
    /// A runner journaling into `state`.
    pub fn new(state: &Path) -> Result<Self> {
        Ok(Self {
            launcher: Launcher::new()?,
            journal: Journal::open(state)?,
            mine: BTreeSet::new(),
            candidates: BTreeMap::new(),
            egress: None,
            egressing: BTreeSet::new(),
        })
    }

    /// This runner, having every launch admitted by the egress broker
    /// `route` before it starts; `None` offers no launch brokered egress.
    pub fn with_egress(mut self, route: Option<EgressRoute>) -> Self {
        self.egress = route;
        self
    }

    /// The candidate of this run's reviewer launch `launch`, if it has one.
    pub fn candidate(&self, launch: &str) -> Option<Arc<Candidate>> {
        let served = self.candidates.get(launch)?;
        Some(served.candidate.clone())
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
                role,
                candidate,
            } => {
                let purpose = Purpose {
                    source,
                    attempt,
                    generation,
                    role,
                };
                self.prepare(&launch, spec, purpose, candidate.map(|spec| *spec))
            }
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
            Request::CandidateReady { launch } => Ok(ready_answer(&self.readiness(&launch))),
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

    /// Prepares a launch, and a reviewer launch's candidate with it, and
    /// journals both. An id an earlier runner journaled is refused: that
    /// launch is judged by `recover`, never started again.
    fn prepare(
        &mut self,
        launch: &str,
        spec: LaunchSpec,
        purpose: Purpose,
        candidate: Option<CandidateSpec>,
    ) -> Result<Value> {
        ensure!(
            self.journal.load(launch).is_none(),
            "launch {launch} is already journaled; recover it instead of preparing it again"
        );
        let profile = spec.profile.clone();
        let (control, work) = (spec.control.clone(), spec.work.clone());
        let prepared = self.launcher.prepare(launch, spec)?;
        let candidate = match candidate {
            Some(spec) => {
                let dirs = (control.as_path(), work.as_path());
                let reviewer = prepared.package_sid.as_str();
                let candidate = self.prepare_candidate(launch, spec, reviewer, dirs)?;
                Some(self.serve_candidate(launch, candidate, reviewer, dirs)?)
            }
            None => None,
        };
        let record = candidate
            .as_ref()
            .map(|served| record_of(&served.candidate));
        let entry = Entry {
            control: Some(control),
            work: Some(work),
            candidate: record,
            ..purpose.entry(launch, profile, &prepared)
        };
        self.journal_or_undo(launch, &entry, candidate)?;
        let mut answer = serde_json::to_value(prepared)?;
        if let Some(candidate) = &entry.candidate {
            answer["candidate_package_sid"] = json!(candidate.package_sid);
        }
        Ok(answer)
    }

    /// Prepares `launch`'s candidate for the reviewer package `reviewer`;
    /// if that fails the reviewer launch is undone as well.
    fn prepare_candidate(
        &mut self,
        launch: &str,
        spec: CandidateSpec,
        reviewer: &str,
        (control, work): (&Path, &Path),
    ) -> Result<Candidate> {
        let prepared = candidate::prepare(
            &mut self.launcher,
            launch,
            spec,
            reviewer,
            Tuning::default(),
        );
        if prepared.is_err() {
            let _ = self.launcher.cleanup(launch);
            for directory in [control, work] {
                let _ = std::fs::remove_dir_all(directory);
            }
        }
        prepared
    }

    /// Writes the reviewer launch's prefix script into its control
    /// directory and starts the threads of its candidate pipe, which admits
    /// only the package `reviewer`. If either fails, the candidate and the
    /// reviewer launch are undone, directories included.
    fn serve_candidate(
        &mut self,
        launch: &str,
        candidate: Candidate,
        reviewer: &str,
        (control, work): (&Path, &Path),
    ) -> Result<Served> {
        let candidate = Arc::new(candidate);
        let owner = self.launcher.owner().to_owned();
        let served = write_script(launch, candidate.spec(), control)
            .and_then(|()| Server::start(launch, &owner, reviewer, candidate.clone()));
        match served {
            Ok(server) => Ok(Served {
                candidate,
                server: Some(server),
            }),
            Err(error) => {
                self.undo_candidate(launch, candidate, &[control, work]);
                Err(error)
            }
        }
    }

    /// Retires a candidate nothing serves yet, removes its trees, and ends
    /// its reviewer launch and removes `directories`.
    fn undo_candidate(&mut self, launch: &str, candidate: Arc<Candidate>, directories: &[&Path]) {
        let spec = candidate.spec().clone();
        if let Ok(candidate) = Arc::try_unwrap(candidate) {
            let _ = candidate.retire(&mut self.launcher);
        }
        candidate::remove_trees(&spec);
        let _ = self.launcher.cleanup(launch);
        for directory in directories {
            let _ = std::fs::remove_dir_all(directory);
        }
    }

    /// Journals a just prepared launch; if that fails the launch and its
    /// candidate are undone, directories included, so nothing exists that
    /// the journal does not name.
    fn journal_or_undo(
        &mut self,
        launch: &str,
        entry: &Entry,
        candidate: Option<Served>,
    ) -> Result<()> {
        if let Err(error) = self.journal.save(entry) {
            if let Some(Ok(candidate)) = candidate.map(Served::into_candidate) {
                let _ = candidate.retire(&mut self.launcher);
            }
            let _ = self.launcher.cleanup(launch);
            let _ = remove_directories(entry);
            return Err(error);
        }
        if let Some(served) = candidate {
            self.candidates.insert(launch.to_owned(), served);
        }
        self.mine.insert(launch.to_owned());
        Ok(())
    }

    /// Retires this run's candidate of `launch`, if it has one: its pipe's
    /// threads end, a command still running is ended, and the candidate
    /// goes. A candidate something else still holds is refused, and kept
    /// with its pipe closed.
    fn retire_candidate(&mut self, launch: &str) -> Result<()> {
        let Some(served) = self.candidates.remove(launch) else {
            return Ok(());
        };
        match served.into_candidate() {
            Ok(candidate) => candidate.retire(&mut self.launcher),
            Err(served) => {
                self.candidates.insert(launch.to_owned(), served);
                bail!("launch {launch}'s candidate is still in use")
            }
        }
    }

    /// Whether `launch`'s candidate runner can run commands now. A launch
    /// prepared without a candidate has none; one whose pipe is no longer
    /// served, or whose shell does not answer, is broken.
    fn readiness(&self, launch: &str) -> Readiness {
        match self.candidates.get(launch) {
            None => Readiness::Missing,
            Some(served) if served.server.is_none() => {
                Readiness::Broken("its pipe is no longer served".into())
            }
            Some(served) => served.candidate.ready(),
        }
    }

    /// Refuses to start a launch whose candidate runner is broken: a
    /// reviewer must never run where its commands cannot reach a candidate.
    /// A launch without a candidate is not a reviewer launch to the runner;
    /// the driver's capability record refuses a reviewer without one.
    fn refuse_broken_candidate(&self, launch: &str) -> Result<()> {
        if let Readiness::Broken(why) = self.readiness(launch) {
            bail!("the candidate runner is broken ({why}); launch {launch} is refused");
        }
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

    /// Starts a prepared launch once its candidate runner and, when one is
    /// configured, the egress broker allow it. A start that fails gives up
    /// the broker's admission again.
    fn start(&mut self, launch: &str, command: &Command, listed: &[u64]) -> Result<Value> {
        let entry = self.entry(launch)?;
        ensure!(!self.launcher.runs(launch)?, "launch {launch} already runs");
        self.refuse_broken_candidate(launch)?;
        let command = self.admit_egress(&entry, command)?;
        let started = self.start_admitted(entry, &command, listed);
        if started.is_err() {
            self.release_egress(launch);
        }
        started
    }

    /// `command`, admitted by the egress broker when one is configured: a
    /// launch the broker does not admit is refused here.
    fn admit_egress(&mut self, entry: &Entry, command: &Command) -> Result<Command> {
        let Some(route) = &self.egress else {
            return Ok(command.clone());
        };
        let admitted = route.admit(entry, command)?;
        self.egressing.insert(entry.launch.clone());
        Ok(admitted)
    }

    /// Asks the egress broker to stop serving `launch` if it admitted it in
    /// this run; what went wrong, if anything. The broker ends the launch's
    /// tunnels and removes its pipe.
    fn release_egress(&mut self, launch: &str) -> Option<String> {
        let route = self.egress.as_ref()?;
        if !self.egressing.remove(launch) {
            return None;
        }
        let released = route.release(launch);
        released.err().map(|error| format!("{error:#}"))
    }

    /// Journals the intent to start, starts, then journals the identity
    /// Windows gave the launch. A refused start leaves nothing running.
    fn start_admitted(
        &mut self,
        mut entry: Entry,
        command: &Command,
        listed: &[u64],
    ) -> Result<Value> {
        entry.stage = Stage::Starting;
        entry.executable = command.argv.first().cloned();
        (entry.pid, entry.created) = (None, None);
        self.journal.save(&entry)?;
        let started = match self.launcher.start(&entry.launch, command, listed) {
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
        self.retire_candidate(launch)?;
        let egress = self.release_egress(launch);
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
                   "directories_removed": remove, "egress_detail": egress }),
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
        let mut deleted = delete_profile(&entry.profile)?;
        if let Some(candidate) = &entry.candidate {
            deleted &= delete_profile(&candidate.profile)?;
        }
        if remove {
            remove_directories(&entry)?;
        }
        self.journal.remove(launch)?;
        let egress = self.release_earlier_egress(launch);
        Ok(
            json!({ "profile_deleted": deleted, "directories_removed": remove,
                   "egress_detail": egress }),
        )
    }

    /// Asks the egress broker, if one is configured, to stop serving an
    /// earlier runner's ended launch, which it may still list; what went
    /// wrong, if anything. A broker that never served it says so and that
    /// is no error.
    fn release_earlier_egress(&self, launch: &str) -> Option<String> {
        let released = self.egress.as_ref()?.release(launch);
        released.err().map(|error| format!("{error:#}"))
    }
}

/// Deletes the AppContainer profile `name`; true when Windows did.
fn delete_profile(name: &str) -> Result<bool> {
    let name = wide(name)?;
    // SAFETY: `name` is NUL-terminated.
    Ok(unsafe { DeleteAppContainerProfile(name.as_ptr()) } == 0)
}

/// Writes `launch`'s prefix script ([`shim::script`]) into the reviewer's
/// control directory `control`, which the reviewer may read and not change.
fn write_script(launch: &str, spec: &CandidateSpec, control: &Path) -> Result<()> {
    use crate::candidate::shim;
    let text = shim::script(
        &spec.shim.display().to_string(),
        &candidate_pipe::pipe_name(launch),
        &spec.reviewer_temp.display().to_string(),
        &spec.source.display().to_string(),
    );
    let script = control.join(shim::SCRIPT_NAME);
    std::fs::write(&script, text).with_context(|| format!("write {}", script.display()))
}

/// The answer to a readiness request: `ready`, `missing` or `broken`, and
/// for a broken runner why.
fn ready_answer(readiness: &Readiness) -> Value {
    match readiness {
        Readiness::Ready => json!({ "runner": "ready" }),
        Readiness::Missing => json!({ "runner": "missing" }),
        Readiness::Broken(why) => json!({ "runner": "broken", "detail": why }),
    }
}

/// What the journal keeps of a prepared candidate.
fn record_of(candidate: &Candidate) -> CandidateRecord {
    let spec = candidate.spec();
    CandidateRecord {
        profile: spec.launch.profile.clone(),
        package_sid: candidate.package_sid().to_owned(),
        control: spec.launch.control.clone(),
        work: spec.launch.work.clone(),
        source: spec.source.clone(),
    }
}

/// Removes the control and working directories `entry` names, and its
/// candidate's three trees. One that is already gone counts as removed.
fn remove_directories(entry: &Entry) -> Result<()> {
    let candidate = entry
        .candidate
        .iter()
        .flat_map(|c| [&c.control, &c.work, &c.source]);
    let own = [&entry.control, &entry.work].into_iter().flatten();
    for directory in own.chain(candidate) {
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

    /// Removes the ended launch's profile, its control and working
    /// directories and its entry. The directories go too because the entry
    /// is the only record of them: once it is removed, a supervisor that
    /// asks to clean the launch up is told nothing of it exists, and
    /// nothing reads a launch's directories after it ended.
    fn reclaim(&mut self, launch: &str) -> Result<()> {
        Runner::reclaim(self, launch, true).map(|_| ())
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A `prepare` request with `extra` fields besides the required ones,
    /// as the runner parses it.
    fn parsed(extra: Value) -> serde_json::Result<Request> {
        let mut request = json!({"op": "prepare", "launch": "l1",
            "spec": {"profile": "agentc.test", "less_privileged": true, "capabilities": [],
                     "limits": {"processes": 4, "process_memory_mb": 64, "job_memory_mb": 128},
                     "control": "c", "work": "w"}});
        for (name, value) in extra.as_object().unwrap() {
            request[name] = value.clone();
        }
        serde_json::from_value(request)
    }

    /// [`parsed`], which must succeed.
    fn prepare(extra: Value) -> Request {
        parsed(extra).unwrap()
    }

    /// The role a `prepare` request carries.
    fn role_of(request: Request) -> Option<Role> {
        let Request::Prepare { role, .. } = request else {
            panic!("not a prepare request");
        };
        role
    }

    #[test]
    fn a_prepare_request_may_name_a_role_and_older_ones_name_none() {
        assert_eq!(role_of(prepare(json!({}))), None);
        let named = prepare(json!({"role": "reviewer", "attempt": "a1", "generation": 2}));
        assert_eq!(role_of(named), Some(Role::Reviewer));
        assert!(parsed(json!({"role": "owner"})).is_err(), "an unknown role");
    }

    #[test]
    fn the_purpose_of_a_launch_is_journaled_as_given() {
        let prepared = Prepared {
            package_sid: "S-1-15-2-1".into(),
            desktop: r"s\d".into(),
            job_owner: "runner:10:500".into(),
        };
        let purpose = Purpose {
            source: Some("6be041f".into()),
            attempt: Some("a1".into()),
            generation: Some(4),
            role: Some(Role::Implementer),
        };
        let entry = purpose.entry("impl-1", "agentc.test".into(), &prepared);
        assert_eq!(entry.role, Some(Role::Implementer));
        let kept = (
            entry.source.as_deref(),
            entry.attempt.as_deref(),
            entry.generation,
        );
        assert_eq!(kept, (Some("6be041f"), Some("a1"), Some(4)));
        assert_eq!(
            (entry.launch.as_str(), entry.profile.as_str()),
            ("impl-1", "agentc.test")
        );
        assert_eq!(entry.package_sid, prepared.package_sid);
        assert_eq!(entry.stage, Stage::Prepared);
    }
}
