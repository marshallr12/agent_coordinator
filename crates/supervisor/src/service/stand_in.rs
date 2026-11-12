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
//!
//! A prepare that fails comes before the claim, so the item stays offered
//! and nothing is released. The stand-in then takes nothing for a while
//! ([`StandIn::held`]): 30 seconds after the first failure, twice as long
//! after each further one in a row, at most ten minutes. The wait is read
//! from the ledger, so a restart does not shorten it.
use crate::host_runtime::{
    HostRuntime, LaunchIdentity, Observation, Recovery, Removed, StartIntent,
};
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
        // Whole literals: joining would put this platform's separator into
        // a Windows path when the defaults are read on another system.
        Self {
            enabled: false,
            queue_dir: PathBuf::from(r"C:\ProgramData\agentc\stand-in\queue"),
            ledger: PathBuf::from(r"C:\ProgramData\agentc\stand-in\ledger.jsonl"),
            fixture: Vec::new(),
            launch_root: PathBuf::from(r"C:\ProgramData\agentc\runner\stand-in-launches"),
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

/// How long nothing is taken after the first failed prepare; each further
/// failure in a row doubles it.
const FIRST_HOLD_MS: i64 = 30_000;
/// The longest wait after failed prepares.
const LONGEST_HOLD_MS: i64 = 600_000;

/// How long nothing is taken after `failures` failed prepares in a row.
fn hold_ms(failures: u32) -> i64 {
    let doublings = failures.saturating_sub(1).min(20);
    (FIRST_HOLD_MS << doublings).min(LONGEST_HOLD_MS)
}

/// The prepares that failed in a row at the end of `events`: how many, and
/// the time and reason of the last. A launch that was asked to start ends
/// the row, since its prepare succeeded.
fn failed_in_a_row(events: &[Value]) -> Option<(u32, i64, String)> {
    let mut row: Option<(u32, i64, String)> = None;
    for event in events {
        let failed = event["event"] == "prepare_result" && event["ok"] == false;
        if failed {
            let count = row.map_or(0, |(count, _, _)| count) + 1;
            let reason = event["error"].as_str().unwrap_or("").to_owned();
            row = Some((count, event["at_ms"].as_i64().unwrap_or(0), reason));
        } else if event["event"] == "start" {
            row = None;
        }
    }
    row
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

    /// Why nothing is taken at `now_ms`, if the last prepares failed and
    /// the wait after them is not over. Without this a host that refuses
    /// every prepare is asked again at every poll.
    pub fn held(&self, now_ms: i64) -> Option<String> {
        let (failures, at_ms, reason) = failed_in_a_row(&self.ledger.events())?;
        let left_ms = at_ms + hold_ms(failures) - now_ms;
        let seconds = (left_ms + 999) / 1000;
        (left_ms > 0).then(|| {
            format!(
                "the last {failures} prepares in a row failed; \
                 nothing is taken for another {seconds} s: {reason}"
            )
        })
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
    /// A prepare that fails is recorded with its reason, which for a
    /// refusal names what the runner could not confirm.
    fn prepare(&mut self, launch: &Launch) -> Result<StartIntent> {
        let session = launch.session_id.to_string();
        (self.ledger).append("prepare", json!({"session": session}))?;
        let prepared = self.inner.prepare(launch);
        if let Err(error) = &prepared {
            let outcome = json!({"ok": false, "error": format!("{error:#}")});
            self.note("prepare_result", &session, outcome);
        }
        prepared
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

    /// The ledger says what the cleanup removed: `launch`, or `nothing`
    /// when nothing of the launch existed at the host. A failed cleanup
    /// removed nothing that is known, and its reason is recorded.
    fn cleanup(&mut self, launch: &Launch) -> Result<Removed> {
        let cleaned = self.inner.cleanup(launch);
        let outcome = match &cleaned {
            Ok(removed) => json!({"ok": true, "removed": removed.name()}),
            Err(error) => json!({"ok": false, "error": format!("{error:#}")}),
        };
        self.note("cleanup", &launch.session_id.to_string(), outcome);
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

    /// A host whose cleanup answers what it is told to and nothing else.
    struct Cleans(Option<Removed>);

    impl HostRuntime for Cleans {
        fn prepare(&mut self, _launch: &Launch) -> Result<StartIntent> {
            anyhow::bail!("not prepared here")
        }

        fn start(&mut self, _launch: &Launch) -> Result<LaunchIdentity> {
            anyhow::bail!("not started here")
        }

        fn observe(&mut self, _launch: &Launch) -> Observation {
            Observation::Running
        }

        fn request_stop(&mut self, _launch: &Launch) {}

        fn terminate(&mut self, _launch: &Launch) {}

        fn recover(&self, _record: &LaunchRecord, _now_ms: i64) -> Recovery {
            Recovery::MayBeRunning
        }

        fn cleanup(&mut self, _launch: &Launch) -> Result<Removed> {
            self.0.context("the runner could not be asked")
        }
    }

    #[test]
    fn the_ledger_says_what_a_cleanup_removed_and_claims_nothing_for_a_refused_prepare() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::at(&dir.path().join("ledger.jsonl"));
        let suggestion = crate::run_loop::Suggestion {
            task: "t".into(),
            revision: 1,
            title: String::new(),
        };
        let launch = Launch::plan(&crate::config::Config::default(), "p", suggestion);
        for answer in [Some(Removed::Nothing), Some(Removed::Launch), None] {
            let mut host = Recorded::new(Cleans(answer), Ledger::at(&ledger.path));
            assert!(host.prepare(&launch).is_err(), "a refused prepare");
            assert_eq!(host.cleanup(&launch).ok(), answer);
        }
        let refusals = |event: &Value| event["event"] == "prepare_result";
        let refused: Vec<Value> = ledger.events().into_iter().filter(refusals).collect();
        assert_eq!(refused.len(), 3);
        assert_eq!(refused[0]["error"], "not prepared here");
        let cleanups: Vec<Value> = (ledger.events().into_iter())
            .filter(|event| event["event"] == "cleanup")
            .collect();
        let seen = |i: usize, field: &str| cleanups[i][field].clone();
        assert_eq!(
            (seen(0, "ok"), seen(0, "removed")),
            (json!(true), json!("nothing"))
        );
        assert_eq!(
            (seen(1, "ok"), seen(1, "removed")),
            (json!(true), json!("launch"))
        );
        assert_eq!(
            (seen(2, "ok"), seen(2, "removed")),
            (json!(false), Value::Null)
        );
        assert!(
            seen(2, "error")
                .as_str()
                .unwrap()
                .contains("could not be asked")
        );
    }

    #[test]
    fn failed_prepares_hold_the_stand_in_off_twice_as_long_each_time() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::at(&dir.path().join("ledger.jsonl"));
        let stand_in = StandIn::new(dir.path(), Ledger::at(&ledger.path));
        assert_eq!(stand_in.held(0), None, "nothing failed yet");
        let refused = json!({"ok": false, "error": "station_not_ours"});
        ledger.append("prepare_result", refused.clone()).unwrap();
        let first = ledger.events()[0]["at_ms"].as_i64().unwrap();
        let held = stand_in.held(first + 1_000).unwrap();
        assert!(held.contains("last 1 prepares"), "{held}");
        assert!(held.ends_with("another 29 s: station_not_ours"), "{held}");
        assert_eq!(
            stand_in.held(first + FIRST_HOLD_MS),
            None,
            "the wait is over"
        );
        ledger.append("prepare_result", refused).unwrap();
        let second = ledger.events()[1]["at_ms"].as_i64().unwrap();
        assert!(stand_in.held(second + 59_000).is_some(), "twice as long");
        assert_eq!(stand_in.held(second + 60_000), None);
        ledger.append("start", json!({"task": "a"})).unwrap();
        assert_eq!(stand_in.held(second), None, "a launch that started");
    }

    #[test]
    fn the_wait_after_failed_prepares_doubles_up_to_ten_minutes() {
        let waits: Vec<i64> = [1, 2, 3, 5, 6, 40, u32::MAX]
            .into_iter()
            .map(hold_ms)
            .collect();
        let expected = [30_000, 60_000, 120_000, 480_000, 600_000, 600_000, 600_000];
        assert_eq!(waits, expected);
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
