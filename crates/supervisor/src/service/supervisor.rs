//! The supervisor's own service role: the existing live loop
//! ([`crate::run_loop::run`]) over a [`LoopDriver`].
//!
//! The loop already knows how to end: once [`Driver::stopping`] is true it
//! claims nothing more, asks a running launch to stop, terminates it after
//! `[run] drain_seconds`, releases its attempt and returns; after a restart
//! it settles what the earlier run left through the host runtime's
//! `recover`. A service only has to make `stopping` follow the stop or
//! preshutdown its control handler recorded ([`Signal`]) and keep reporting
//! while the loop drains ([`Pace::pause`]).
//!
//! Where the work comes from is a [`Work`]. On Windows there are two:
//! [`NoHarness`], which claims nothing because no provider harness is
//! supported there, and the acceptance stand-in
//! ([`super::stand_in::StandIn`]). Neither contacts a coordinator.
use super::lifecycle::Signal;
use super::pipe_policy::{checked_sid, pipe_path};
use super::stand_in::{StandIn, StandInSettings};
use crate::host_runtime::HostRuntime;
use crate::profile::{Harness, Role};
use crate::run_loop::lease::Lease;
use crate::run_loop::{Driver, Launch};
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Why a Windows supervisor without a stand-in claims nothing.
pub const NO_HARNESS: &str = "no provider harness is supported on Windows; \
     the supervisor service claims nothing";

/// The `[windows_supervisor]` table of the supervisor configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct SupervisorSettings {
    /// The service's name, as registered with the Service Control Manager.
    pub name: String,
    /// Where the loop keeps its launch records and heartbeat. It replaces
    /// the configuration's `state_dir` for this service.
    pub state_dir: PathBuf,
    /// The runner service's pipe, below `\\.\pipe\`.
    pub runner_pipe: String,
    /// The runner service's account, as SID text. A pipe it does not own is
    /// never used. Empty: the service refuses to start.
    pub runner_sid: String,
    /// Seconds between status reports while the service is stopping.
    pub status_interval_seconds: u64,
    /// The acceptance stand-in; off by default.
    pub stand_in: StandInSettings,
}

impl Default for SupervisorSettings {
    fn default() -> Self {
        Self {
            name: "agentc-supervisor".into(),
            state_dir: PathBuf::from(r"C:\ProgramData\agentc\supervisor"),
            runner_pipe: "agentc-runner".into(),
            runner_sid: String::new(),
            status_interval_seconds: 3,
            stand_in: StandInSettings::default(),
        }
    }
}

impl SupervisorSettings {
    /// Refuses settings the service could not honor, before it opens
    /// anything.
    pub fn validate(&self) -> Result<()> {
        ensure!(!self.name.is_empty(), "windows_supervisor.name is empty");
        pipe_path(&self.runner_pipe)?;
        ensure!(
            !self.runner_sid.is_empty(),
            "windows_supervisor.runner_sid names no runner account; no pipe could be trusted"
        );
        checked_sid(&self.runner_sid)?;
        ensure!(
            self.status_interval_seconds >= 1,
            "windows_supervisor.status_interval_seconds must be at least 1"
        );
        self.stand_in.validate()
    }
}

/// Where a supervisor's work comes from.
pub trait Work {
    /// Whether launches can run at all; an error stops every claim and is
    /// the reason the heartbeat shows.
    fn available(&self) -> Result<()>;
    /// The `next` answer.
    fn next(&mut self) -> Result<Value>;
    fn claim(&mut self, launch: &Launch) -> Result<Lease>;
    fn renew(&mut self, lease: &Lease) -> Result<Lease>;
    fn release(&mut self, launch: &Launch, lease: &Lease, summary: &str) -> Result<()>;
}

/// The work of a host with no supported harness: none, ever.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoHarness;

impl Work for NoHarness {
    fn available(&self) -> Result<()> {
        bail!(NO_HARNESS)
    }

    fn next(&mut self) -> Result<Value> {
        Ok(json!({ "action": null }))
    }

    fn claim(&mut self, _launch: &Launch) -> Result<Lease> {
        bail!(NO_HARNESS)
    }

    fn renew(&mut self, _lease: &Lease) -> Result<Lease> {
        bail!(NO_HARNESS)
    }

    /// Nothing was ever claimed, so there is nothing to release.
    fn release(&mut self, _launch: &Launch, _lease: &Lease, _summary: &str) -> Result<()> {
        Ok(())
    }
}

impl Work for StandIn {
    fn available(&self) -> Result<()> {
        Ok(())
    }

    fn next(&mut self) -> Result<Value> {
        Ok(StandIn::next(self))
    }

    fn claim(&mut self, launch: &Launch) -> Result<Lease> {
        StandIn::claim(self, launch)
    }

    fn renew(&mut self, lease: &Lease) -> Result<Lease> {
        StandIn::renew(self, lease)
    }

    fn release(&mut self, launch: &Launch, lease: &Lease, summary: &str) -> Result<()> {
        StandIn::release(self, launch, lease, summary)
    }
}

/// The host's clock, waits and disk, which a test replaces.
pub trait Pace {
    /// Milliseconds since the epoch.
    fn now_ms(&self) -> i64;
    /// Waits `duration`. `stopping` says whether the service is ending, so
    /// a service can report its progress while the loop drains.
    fn pause(&mut self, duration: Duration, stopping: bool);
    /// Free bytes on the filesystem holding the state directory.
    fn free_bytes(&self) -> Result<u64>;
}

/// The loop's driver for a supervisor service.
pub struct LoopDriver<H: HostRuntime, W: Work, P: Pace> {
    host: H,
    work: W,
    pace: P,
    signal: Arc<Signal>,
}

impl<H: HostRuntime, W: Work, P: Pace> LoopDriver<H, W, P> {
    /// A driver whose launches run on `host`, whose work comes from `work`
    /// and which stops once `signal` holds a stop or preshutdown request.
    pub fn new(host: H, work: W, pace: P, signal: Arc<Signal>) -> Self {
        Self {
            host,
            work,
            pace,
            signal,
        }
    }
}

impl<H: HostRuntime, W: Work, P: Pace> Driver for LoopDriver<H, W, P> {
    /// There is no coordinator project; launch records carry this name.
    fn project(&self) -> &str {
        "local"
    }

    fn next(&mut self, role: Role, _exclude: &[String]) -> Result<Value> {
        match role {
            Role::Implementer => self.work.next(),
            Role::Reviewer => Ok(json!({ "action": null })),
        }
    }

    fn free_bytes(&self) -> Result<u64> {
        self.pace.free_bytes()
    }

    fn host(&mut self) -> &mut dyn HostRuntime {
        &mut self.host
    }

    /// No repository is cloned on the supervisor's side.
    fn instructions(&mut self, _launch: &Launch) -> Vec<(String, String)> {
        Vec::new()
    }

    /// No harness reads a prompt.
    fn install_prompt(&mut self, _launch: &Launch, _prompt: &str) -> Result<()> {
        Ok(())
    }

    fn claim(&mut self, launch: &Launch) -> Result<Lease> {
        self.work.claim(launch)
    }

    /// There is no checkout to register.
    fn register(&mut self, _launch: &Launch, _lease: &Lease) -> Result<()> {
        Ok(())
    }

    /// No harness writes events, so a launch counts as active while it
    /// runs; `[run] budget_minutes` still bounds it.
    fn last_event_ms(&self, _launch: &Launch) -> Option<i64> {
        Some(self.pace.now_ms())
    }

    fn renew(&mut self, _launch: &Launch, lease: &Lease) -> Result<Lease> {
        self.work.renew(lease)
    }

    /// Nothing here submits an attempt.
    fn attempt_state(&mut self, _launch: &Launch, _lease: &Lease) -> Option<String> {
        Some("active".to_owned())
    }

    fn release(&mut self, launch: &Launch, lease: &Lease, summary: &str) -> Result<()> {
        self.work.release(launch, lease, summary)
    }

    /// No harness writes state markers on this host.
    fn has_marker(&self, _launch: &Launch, _name: &str) -> bool {
        false
    }

    fn now_ms(&self) -> i64 {
        self.pace.now_ms()
    }

    fn pause(&mut self, duration: Duration) {
        let stopping = self.stopping();
        self.pace.pause(duration, stopping);
    }

    fn stopping(&self) -> bool {
        self.signal.stop_requested() || self.signal.preshutdown_requested()
    }

    fn harness_status(&mut self, _harness: Harness) -> Result<()> {
        self.work.available()
    }

    /// A launch on this host leaves no event stream on the supervisor's
    /// side.
    fn events(&self, _launch: &Launch) -> Option<Vec<u8>> {
        None
    }

    fn stderr(&self, _launch: &Launch) -> Option<Vec<u8>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::super::lifecycle::Request;
    use super::super::stand_in::{Ledger, Recorded, judge};
    use super::*;
    use crate::config::Config;
    use crate::host_runtime::{LaunchIdentity, Observation, Recovery, Removed, StartIntent};
    use crate::run_loop::record::LaunchRecord;
    use crate::run_loop::{Outcome, heartbeat_path, iterate, run};
    use std::cell::Cell;
    use std::collections::BTreeSet;
    use std::path::Path;
    use std::rc::Rc;

    /// How a scripted launch behaves once started.
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Behaviour {
        /// It exits by itself with code 0 at the first look.
        Exits,
        /// It runs until it is terminated.
        Runs,
        /// Looking at it kills the supervisor (a panic).
        KillsTheSupervisor,
    }

    /// A host runtime that follows a script and counts what it was asked.
    struct Scripted {
        behaviour: Behaviour,
        terminated: BTreeSet<uuid::Uuid>,
        /// What `recover` says of every record.
        recovery: Recovery,
        cleaned: u32,
    }

    impl Scripted {
        fn new(behaviour: Behaviour) -> Self {
            Self {
                behaviour,
                terminated: BTreeSet::new(),
                recovery: Recovery::Ended,
                cleaned: 0,
            }
        }
    }

    impl HostRuntime for Scripted {
        fn prepare(&mut self, _launch: &Launch) -> Result<StartIntent> {
            Ok(StartIntent::default())
        }

        fn start(&mut self, _launch: &Launch) -> Result<LaunchIdentity> {
            Ok(LaunchIdentity {
                pid: 4242,
                started: Some(1),
            })
        }

        fn observe(&mut self, launch: &Launch) -> Observation {
            match self.behaviour {
                Behaviour::Exits => Observation::Exited { code: 0 },
                Behaviour::KillsTheSupervisor => panic!("the supervisor died"),
                Behaviour::Runs if self.terminated.contains(&launch.session_id) => {
                    Observation::Exited { code: -1 }
                }
                Behaviour::Runs => Observation::Running,
            }
        }

        fn request_stop(&mut self, _launch: &Launch) {}

        fn terminate(&mut self, launch: &Launch) {
            self.terminated.insert(launch.session_id);
        }

        fn recover(&self, _record: &LaunchRecord, _now_ms: i64) -> Recovery {
            self.recovery
        }

        fn cleanup(&mut self, _launch: &Launch) -> Result<Removed> {
            self.cleaned += 1;
            Ok(Removed::Launch)
        }
    }

    /// A clock that only moves when the loop waits, and that records a stop
    /// request after a set number of waits.
    struct Virtual {
        now: Rc<Cell<i64>>,
        stop_after: Option<u32>,
        waits: u32,
        signal: Arc<Signal>,
        /// How many waits happened while the service was stopping.
        stopping_waits: Rc<Cell<u32>>,
    }

    impl Pace for Virtual {
        fn now_ms(&self) -> i64 {
            self.now.get()
        }

        fn pause(&mut self, duration: Duration, stopping: bool) {
            self.now.set(self.now.get() + duration.as_millis() as i64);
            self.waits += 1;
            if stopping {
                self.stopping_waits.set(self.stopping_waits.get() + 1);
            }
            if self.stop_after == Some(self.waits) {
                self.signal.request(Request::Stop);
            }
        }

        fn free_bytes(&self) -> Result<u64> {
            Ok(u64::MAX)
        }
    }

    /// A state directory, a queue of `items` and the settings of a loop
    /// that never holds claims for launches without a submission.
    fn fixture(items: &[&str]) -> (tempfile::TempDir, Config) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("queue")).unwrap();
        for item in items {
            std::fs::write(dir.path().join("queue").join(item), b"").unwrap();
        }
        let mut config = Config {
            state_dir: dir.path().join("state"),
            ..Config::default()
        };
        std::fs::create_dir(&config.state_dir).unwrap();
        (
            config.run.empty_exit_limit,
            config.run.empty_exit_total_limit,
        ) = (0, 0);
        config.run.drain_seconds = 5;
        (dir, config)
    }

    fn ledger(root: &Path) -> Ledger {
        Ledger::at(&root.join("ledger.jsonl"))
    }

    type StandInDriver = LoopDriver<Recorded<Scripted>, StandIn, Virtual>;

    /// A stand-in driver over `root` whose launches follow `behaviour`.
    fn driver(root: &Path, behaviour: Behaviour, stop_after: Option<u32>) -> StandInDriver {
        let signal = Arc::new(Signal::default());
        let pace = Virtual {
            now: Rc::new(Cell::new(1_000_000)),
            stop_after,
            waits: 0,
            signal: signal.clone(),
            stopping_waits: Rc::new(Cell::new(0)),
        };
        let host = Recorded::new(Scripted::new(behaviour), ledger(root));
        let work = StandIn::new(&root.join("queue"), ledger(root));
        LoopDriver::new(host, work, pace, signal)
    }

    /// The ledger's events named `name`.
    fn events(root: &Path, name: &str) -> Vec<Value> {
        let all = ledger(root).events();
        all.into_iter().filter(|e| e["event"] == name).collect()
    }

    #[test]
    fn without_a_harness_the_service_claims_nothing_and_still_writes_its_heartbeat() {
        let (_dir, config) = fixture(&[]);
        let signal = Arc::new(Signal::default());
        let pace = Virtual {
            now: Rc::new(Cell::new(1_000_000)),
            stop_after: None,
            waits: 0,
            signal: signal.clone(),
            stopping_waits: Rc::new(Cell::new(0)),
        };
        let host = crate::host_runtime::Refusing::new(NO_HARNESS);
        let mut driver = LoopDriver::new(host, NoHarness, pace, signal);
        let outcome = iterate(&mut driver, &config);
        assert!(
            matches!(&outcome, Outcome::Refused(why) if why.contains("supported on Windows")),
            "{outcome:?}"
        );
        run(&mut driver, &config, true).unwrap();
        let beat = std::fs::read_to_string(heartbeat_path(&config)).unwrap();
        let beat: Value = serde_json::from_str(&beat).unwrap();
        assert_eq!(beat["state"], "stopped");
        assert!(
            beat["stop"]
                .as_str()
                .unwrap()
                .contains("supported on Windows")
        );
    }

    #[test]
    fn the_stand_in_feeds_the_loop_one_item_at_a_time_and_the_ledger_is_clean() {
        let (dir, config) = fixture(&["item-a", "item-b"]);
        let mut driver = driver(dir.path(), Behaviour::Exits, None);
        for expected in ["item-a", "item-b"] {
            let outcome = iterate(&mut driver, &config);
            let launched = Outcome::Launched {
                task: expected.into(),
                exit_code: 0,
            };
            assert_eq!(outcome, launched);
        }
        assert_eq!(iterate(&mut driver, &config), Outcome::Idle);
        assert_eq!(judge(&ledger(dir.path()).events()), Vec::<String>::new());
        for name in ["claim", "start", "exited", "release", "cleanup"] {
            assert_eq!(events(dir.path(), name).len(), 2, "{name}");
        }
    }

    #[test]
    fn a_stop_ends_admission_drains_the_running_launch_and_reports_meanwhile() {
        let (dir, config) = fixture(&["item-a", "item-b"]);
        let mut driver = driver(dir.path(), Behaviour::Runs, Some(3));
        let stopping_waits = driver.pace.stopping_waits.clone();
        run(&mut driver, &config, false).unwrap();
        // The launch was asked to stop, then ended at the drain's deadline.
        assert_eq!(events(dir.path(), "stop_requested").len(), 1);
        assert_eq!(events(dir.path(), "terminate").len(), 1);
        let released = events(dir.path(), "release");
        assert_eq!(released.len(), 1);
        let summary = released[0]["summary"].as_str().unwrap();
        assert!(summary.contains("the host supervisor stopped"), "{summary}");
        // The second item was never claimed, and the loop returned.
        assert_eq!(events(dir.path(), "claim").len(), 1);
        assert!(stopping_waits.get() >= config.run.drain_seconds as u32);
        assert_eq!(judge(&ledger(dir.path()).events()), Vec::<String>::new());
    }

    #[test]
    fn an_interrupted_run_is_settled_without_a_second_launch_or_a_false_release() {
        let (dir, config) = fixture(&["item-a", "item-b"]);
        // The supervisor dies while its first launch runs.
        let died = std::panic::catch_unwind(|| {
            let mut driver = driver(dir.path(), Behaviour::KillsTheSupervisor, None);
            iterate(&mut driver, &config)
        });
        assert!(died.is_err());
        assert_eq!(events(dir.path(), "start").len(), 1);
        assert!(
            events(dir.path(), "release").is_empty(),
            "nothing was released yet"
        );

        // Restarted while the launch may still run: nothing is claimed,
        // nothing is released, nothing is started.
        let mut waiting = driver(dir.path(), Behaviour::Exits, None);
        waiting.host = Recorded::new(
            Scripted {
                recovery: Recovery::MayBeRunning,
                ..Scripted::new(Behaviour::Exits)
            },
            ledger(dir.path()),
        );
        let outcome = iterate(&mut waiting, &config);
        assert!(matches!(&outcome, Outcome::Refused(why) if why.contains("may still run")));
        assert_eq!(events(dir.path(), "start").len(), 1);

        // Once the earlier launch is judged ended, its attempt is released
        // and only the other item is launched.
        let mut restarted = driver(dir.path(), Behaviour::Exits, None);
        let outcome = iterate(&mut restarted, &config);
        let second = Outcome::Launched {
            task: "item-b".into(),
            exit_code: 0,
        };
        assert_eq!(outcome, second);
        assert_eq!(iterate(&mut restarted, &config), Outcome::Idle);
        let starts = events(dir.path(), "start");
        let tasks: Vec<&str> = starts.iter().map(|e| e["task"].as_str().unwrap()).collect();
        assert_eq!(tasks, ["item-a", "item-b"]);
        assert_eq!(events(dir.path(), "release").len(), 2);
        assert!(LaunchRecord::load_all(&config).is_empty());
        assert_eq!(judge(&ledger(dir.path()).events()), Vec::<String>::new());
        // The first item's release came only after it was judged ended.
        let all = ledger(dir.path()).events();
        let at = |name: &str, verdict: Option<&str>| {
            let wanted =
                |e: &Value| e["event"] == name && verdict.is_none_or(|v| e["verdict"] == v);
            all.iter().position(wanted).unwrap()
        };
        assert!(at("recover", Some("may_be_running")) < at("recover", Some("ended")));
        assert!(at("recover", Some("ended")) < at("release", None));
    }

    #[test]
    fn the_settings_default_to_a_supervisor_that_names_no_runner_and_no_stand_in() {
        let settings = SupervisorSettings::default();
        assert!(!settings.stand_in.enabled);
        let refused = settings.validate().unwrap_err().to_string();
        assert!(refused.contains("runner_sid"), "{refused}");
        let named = SupervisorSettings {
            runner_sid: "S-1-5-21-1-2-3-1001".into(),
            ..settings
        };
        named.validate().unwrap();
        let broken: [fn(&mut SupervisorSettings); 5] = [
            |s| s.runner_sid = "everyone".into(),
            |s| s.runner_pipe = r"..\x".into(),
            |s| s.name.clear(),
            |s| s.status_interval_seconds = 0,
            |s| s.stand_in.enabled = true,
        ];
        for (index, change) in broken.iter().enumerate() {
            let mut settings = named.clone();
            change(&mut settings);
            assert!(settings.validate().is_err(), "case {index}");
        }
    }
}
