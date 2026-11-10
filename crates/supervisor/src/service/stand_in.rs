//! A stand-in for the coordinator, for machine acceptance of a supervisor
//! service and for nothing else. It never contacts a coordinator.
//!
//! Work comes from a directory: each file is one item, named after its
//! task. Everything the loop does with an item is appended to a
//! [`Ledger`], one JSON object per line, and each line is flushed to disk
//! **before** the effect it records: a claim before the claim is answered,
//! a start before the launch is asked for. A run that was stopped,
//! crashed or rebooted can therefore be judged afterwards from the ledger
//! alone ([`judge`]): no item may have been launched twice, and none may
//! have been released without its launch having ended.
//!
//! An item is offered until it is claimed and never again, whatever became
//! of the claim, so a restart cannot launch it a second time.
use crate::host_runtime::{HostRuntime, LaunchIdentity, Observation, Recovery, StartIntent};
use crate::run_loop::Launch;
use crate::run_loop::lease::Lease;
use crate::run_loop::record::LaunchRecord;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

/// The `[windows_supervisor.stand_in]` table. Off unless `enabled`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct StandInSettings {
    /// Whether the supervisor takes its work from the stand-in. While it is
    /// false the other entries are not read and the service claims nothing.
    pub enabled: bool,
    /// The directory of queued items, one file each.
    pub queue_dir: PathBuf,
    /// The ledger file.
    pub ledger: PathBuf,
    /// The command every stand-in launch runs, as an argument vector. Empty:
    /// the stand-in cannot be enabled.
    pub fixture: Vec<String>,
    /// An existing directory the runner's account can create launch
    /// directories in.
    pub launch_root: PathBuf,
}

impl Default for StandInSettings {
    fn default() -> Self {
        let base = PathBuf::from(r"C:\ProgramData\agentc");
        Self {
            enabled: false,
            queue_dir: base.join(r"stand-in\queue"),
            ledger: base.join(r"stand-in\ledger.jsonl"),
            fixture: Vec::new(),
            launch_root: base.join(r"runner\stand-in-launches"),
        }
    }
}

impl StandInSettings {
    /// Refuses an enabled stand-in that has nothing to run.
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.enabled || !self.fixture.is_empty(),
            "windows_supervisor.stand_in is enabled but names no fixture command"
        );
        Ok(())
    }
}

/// An append-only file of events.
#[derive(Debug, Clone)]
pub struct Ledger {
    path: PathBuf,
}

impl Ledger {
    /// The ledger at `path`; the file is made by the first event.
    pub fn at(path: &Path) -> Self {
        Self {
            path: path.to_owned(),
        }
    }

    /// Appends `event` with its `fields` and flushes it to disk. Call it
    /// before the effect it records; if it fails, the effect must not
    /// happen.
    pub fn append(&self, event: &str, mut fields: Value) -> Result<()> {
        fields["event"] = json!(event);
        fields["at_ms"] = json!(crate::shadow::now_ms());
        let mut line = serde_json::to_vec(&fields)?;
        line.push(b'\n');
        let mut options = std::fs::OpenOptions::new();
        let opened = options.create(true).append(true).open(&self.path);
        let mut file = opened.with_context(|| format!("open {}", self.path.display()))?;
        file.write_all(&line)?;
        file.sync_all().context("flush the ledger")
    }

    /// Every event so far, in order. A line that is not an event is kept as
    /// `{"event": "unreadable"}` so a judge sees that something was lost.
    pub fn events(&self) -> Vec<Value> {
        let text = std::fs::read_to_string(&self.path).unwrap_or_default();
        let parse = |line: &str| {
            serde_json::from_str(line).unwrap_or_else(|_| json!({"event": "unreadable"}))
        };
        text.lines().filter(|l| !l.is_empty()).map(parse).collect()
    }
}

/// The stand-in's side of the loop's coordinator calls.
#[derive(Debug, Clone)]
pub struct StandIn {
    queue: PathBuf,
    ledger: Ledger,
}

impl StandIn {
    /// A stand-in over `queue` that records in `ledger`.
    pub fn new(queue: &Path, ledger: Ledger) -> Self {
        Self {
            queue: queue.to_owned(),
            ledger,
        }
    }

    /// The tasks claimed at any time, by this run or an earlier one.
    fn claimed(&self) -> BTreeSet<String> {
        let events = self.ledger.events();
        let claims = events.iter().filter(|event| event["event"] == "claim");
        claims
            .filter_map(|event| event["task"].as_str().map(str::to_owned))
            .collect()
    }

    /// The queued items in name order: plain file names only, so an item's
    /// name can be a task id and a ledger key.
    fn queued(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.queue) else {
            return Vec::new();
        };
        let names = entries.flatten().filter(|entry| entry.path().is_file());
        let plain = |name: &String| {
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        };
        let mut items: Vec<String> = names
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(plain)
            .collect();
        items.sort();
        items
    }

    /// A `next` answer: the first queued item nobody ever claimed, as the
    /// claim suggestion the loop expects, or no action.
    pub fn next(&self) -> Value {
        let claimed = self.claimed();
        let Some(task) = self.queued().into_iter().find(|t| !claimed.contains(t)) else {
            return json!({ "action": null });
        };
        json!({"action": {"kind": "claim_task", "title": task,
                          "call": {"body": {"task_id": task, "expected_task_revision": 1}}}})
    }

    /// Records the claim, then answers it with a lease of its own making.
    pub fn claim(&self, launch: &Launch) -> Result<Lease> {
        let attempt = format!("stand-in-{}", launch.session_id);
        let fields = json!({"task": launch.suggestion.task,
                            "session": launch.session_id.to_string(), "attempt": attempt});
        self.ledger.append("claim", fields)?;
        Ok(lease(&attempt))
    }

    /// Records the renewal and answers with a fresh lease.
    pub fn renew(&self, lease_held: &Lease) -> Result<Lease> {
        (self.ledger).append("renew", json!({"attempt": lease_held.attempt}))?;
        Ok(lease(&lease_held.attempt))
    }

    /// Records the release with the loop's handoff summary.
    pub fn release(&self, launch: &Launch, lease_held: &Lease, summary: &str) -> Result<()> {
        let fields = json!({"task": launch.suggestion.task,
                            "session": launch.session_id.to_string(),
                            "attempt": lease_held.attempt, "summary": summary});
        self.ledger.append("release", fields)
    }
}

/// A lease on `attempt` that is always fresh: the stand-in has no clock of
/// its own to age it by.
fn lease(attempt: &str) -> Lease {
    Lease {
        attempt: attempt.to_owned(),
        generation: 1,
        renew_after_seconds: 60,
        progress_age_ms: 0,
    }
}

/// A host runtime that records what the loop asks of `inner` in a ledger,
/// each request before it is passed on.
pub struct Recorded<H: HostRuntime> {
    inner: H,
    ledger: Ledger,
    /// Launches whose exit was recorded, so it is recorded once.
    exited: BTreeSet<String>,
}

impl<H: HostRuntime> Recorded<H> {
    /// `inner`, recorded in `ledger`.
    pub fn new(inner: H, ledger: Ledger) -> Self {
        Self {
            inner,
            ledger,
            exited: BTreeSet::new(),
        }
    }

    /// Records `event` about `session`; a ledger that cannot be written is
    /// reported and, for events that are not gates, not fatal.
    fn note(&self, event: &str, session: &str, mut fields: Value) {
        fields["session"] = json!(session);
        if let Err(error) = self.ledger.append(event, fields) {
            eprintln!("agentc-supervisor run: ledger: {error:#}");
        }
    }
}

impl<H: HostRuntime> HostRuntime for Recorded<H> {
    fn prepare(&mut self, launch: &Launch) -> Result<StartIntent> {
        let session = launch.session_id.to_string();
        (self.ledger).append("prepare", json!({"session": session}))?;
        self.inner.prepare(launch)
    }

    /// The start is recorded before it is asked for and refused if it
    /// cannot be recorded; what came of it is recorded after.
    fn start(&mut self, launch: &Launch) -> Result<LaunchIdentity> {
        let session = launch.session_id.to_string();
        let fields = json!({"session": session, "task": launch.suggestion.task});
        self.ledger.append("start", fields)?;
        let started = self.inner.start(launch);
        let outcome = match &started {
            Ok(identity) => json!({"ok": true, "pid": identity.pid}),
            Err(error) => json!({"ok": false, "error": format!("{error:#}")}),
        };
        self.note("start_result", &session, outcome);
        started
    }

    fn observe(&mut self, launch: &Launch) -> Observation {
        let seen = self.inner.observe(launch);
        let session = launch.session_id.to_string();
        if let Observation::Exited { code } = seen
            && self.exited.insert(session.clone())
        {
            self.note("exited", &session, json!({"code": code}));
        }
        seen
    }

    fn request_stop(&mut self, launch: &Launch) {
        self.note("stop_requested", &launch.session_id.to_string(), json!({}));
        self.inner.request_stop(launch);
    }

    fn terminate(&mut self, launch: &Launch) {
        self.note("terminate", &launch.session_id.to_string(), json!({}));
        self.inner.terminate(launch);
    }

    fn recover(&self, record: &LaunchRecord, now_ms: i64) -> Recovery {
        let judged = self.inner.recover(record, now_ms);
        let verdict = match judged {
            Recovery::Ended => "ended",
            Recovery::MayBeRunning => "may_be_running",
        };
        let session = record.session_id.to_string();
        self.note("recover", &session, json!({"verdict": verdict}));
        judged
    }

    fn cleanup(&mut self, launch: &Launch) -> Result<()> {
        let cleaned = self.inner.cleanup(launch);
        let session = launch.session_id.to_string();
        self.note("cleanup", &session, json!({"ok": cleaned.is_ok()}));
        cleaned
    }
}

/// What a ledger shows went wrong; empty when nothing did.
///
/// - A task whose launch was asked to start more than once.
/// - A release of a launch that was asked to start and is not known to
///   have ended: no exit was seen, no recovery judged it ended, and its
///   start is not recorded as failed.
/// - A line that cannot be read, since it may hide either of the above.
pub fn judge(events: &[Value]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut starts: BTreeMap<String, u32> = BTreeMap::new();
    let (mut asked, mut ended) = (BTreeSet::new(), BTreeSet::new());
    for event in events {
        let session = event["session"].as_str().unwrap_or("").to_owned();
        let over = event["event"] == "exited"
            || (event["event"] == "recover" && event["verdict"] == "ended")
            || (event["event"] == "start_result" && event["ok"] == false);
        match event["event"].as_str() {
            Some("unreadable") => problems.push("the ledger has an unreadable line".to_owned()),
            Some("start") => {
                let task = event["task"].as_str().unwrap_or("").to_owned();
                *starts.entry(task).or_default() += 1;
                asked.insert(session);
            }
            Some("release") if asked.contains(&session) && !ended.contains(&session) => {
                problems.push(format!("{session} was released before its launch ended"));
            }
            _ if over => {
                ended.insert(session);
            }
            _ => {}
        }
    }
    let twice = starts.into_iter().filter(|(_, count)| *count > 1);
    problems.extend(twice.map(|(task, count)| format!("{task} was launched {count} times")));
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(name: &str, session: &str, extra: Value) -> Value {
        let mut event = extra;
        (event["event"], event["session"]) = (json!(name), json!(session));
        event
    }

    #[test]
    fn a_clean_run_and_a_recovered_one_show_no_problem() {
        let clean = [
            event("start", "s1", json!({"task": "a"})),
            event("start_result", "s1", json!({"ok": true})),
            event("exited", "s1", json!({"code": 0})),
            event("release", "s1", json!({})),
            event("start", "s2", json!({"task": "b"})),
            event("recover", "s2", json!({"verdict": "may_be_running"})),
            event("recover", "s2", json!({"verdict": "ended"})),
            event("release", "s2", json!({})),
            // Claimed and never started: nothing ran, so nothing to wait for.
            event("release", "s3", json!({})),
            // A start that is recorded as failed did not leave a launch.
            event("start", "s4", json!({"task": "d"})),
            event("start_result", "s4", json!({"ok": false})),
            event("release", "s4", json!({})),
        ];
        assert_eq!(judge(&clean), Vec::<String>::new());
    }

    #[test]
    fn a_second_start_and_a_release_without_an_end_are_found() {
        let twice = [
            event("start", "s1", json!({"task": "a"})),
            event("exited", "s1", json!({})),
            event("start", "s2", json!({"task": "a"})),
            event("exited", "s2", json!({})),
        ];
        assert_eq!(judge(&twice), ["a was launched 2 times"]);
        let early = [
            event("start", "s1", json!({"task": "a"})),
            event("recover", "s1", json!({"verdict": "may_be_running"})),
            event("release", "s1", json!({})),
            event("exited", "s1", json!({})),
        ];
        assert_eq!(judge(&early), ["s1 was released before its launch ended"]);
        assert_eq!(judge(&[json!({"event": "unreadable"})]).len(), 1);
    }

    #[test]
    fn the_ledger_keeps_every_event_in_order_and_shows_a_torn_line() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::at(&dir.path().join("ledger.jsonl"));
        assert!(ledger.events().is_empty());
        ledger.append("claim", json!({"task": "a"})).unwrap();
        ledger.append("release", json!({"task": "a"})).unwrap();
        let names = |events: &[Value]| -> Vec<String> {
            let name = |e: &Value| e["event"].as_str().unwrap().to_owned();
            events.iter().map(name).collect()
        };
        assert_eq!(names(&ledger.events()), ["claim", "release"]);
        assert!(ledger.events()[0]["at_ms"].as_i64().unwrap() > 0);
        let mut file = std::fs::OpenOptions::new();
        let mut file = file
            .append(true)
            .open(dir.path().join("ledger.jsonl"))
            .unwrap();
        file.write_all(b"{\"event\": \"cla\n").unwrap();
        assert_eq!(names(&ledger.events()), ["claim", "release", "unreadable"]);
    }

    #[test]
    fn an_item_is_offered_until_it_is_claimed_and_never_again() {
        let dir = tempfile::tempdir().unwrap();
        let queue = dir.path().join("queue");
        std::fs::create_dir(&queue).unwrap();
        for name in ["b-item", "a-item", "not plain.txt"] {
            std::fs::write(queue.join(name), b"").unwrap();
        }
        let stand_in = StandIn::new(&queue, Ledger::at(&dir.path().join("ledger.jsonl")));
        let offered = |stand_in: &StandIn| {
            let next = stand_in.next();
            crate::run_loop::suggestion(&next).map(|s| s.task)
        };
        assert_eq!(offered(&stand_in).as_deref(), Some("a-item"));
        let ledger = Ledger::at(&dir.path().join("ledger.jsonl"));
        ledger.append("claim", json!({"task": "a-item"})).unwrap();
        assert_eq!(offered(&stand_in).as_deref(), Some("b-item"));
        ledger.append("claim", json!({"task": "b-item"})).unwrap();
        ledger.append("release", json!({"task": "b-item"})).unwrap();
        assert_eq!(offered(&stand_in), None, "released or not, never again");
    }

    #[test]
    fn the_stand_in_is_off_by_default_and_needs_a_fixture_to_be_enabled() {
        let mut settings = StandInSettings::default();
        assert!(!settings.enabled);
        settings.validate().unwrap();
        settings.enabled = true;
        assert!(settings.validate().is_err());
        settings.fixture = vec!["fixture.exe".into()];
        settings.validate().unwrap();
    }
}
