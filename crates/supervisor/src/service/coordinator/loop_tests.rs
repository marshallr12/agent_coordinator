//! The coordinator source driven by the real loop ([`iterate`]): a full
//! round, the hold that keeps a released fixture task from being claimed
//! again, and the settling of a launch an earlier run left behind.
use super::*;
use crate::config::Config;
use crate::host_runtime::{
    HostRuntime, LaunchIdentity, Observation, Recovery, Removed, StartIntent,
};
use crate::run_loop::record::LaunchRecord;
use crate::run_loop::{Outcome, iterate};
use crate::service::lifecycle::Signal;
use crate::service::stand_in::{Recorded, judge, judge_coordinator};
use crate::service::supervisor::{LoopDriver, Pace};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::Path;
use std::rc::Rc;
use std::sync::Mutex;
use std::time::Duration;

/// How a launch behaves once started.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Behaviour {
    /// It exits 0 at the first look.
    Exits,
    /// It exits with this code at the first look.
    ExitsWith(i32),
    /// It cannot be started: the runner refuses it after the claim.
    CannotStart,
    /// The supervisor dies while it runs.
    KillsTheSupervisor,
}

/// A host runtime that follows a script.
struct Scripted {
    behaviour: Behaviour,
    /// What `recover` says of every record.
    recovery: Recovery,
}

impl HostRuntime for Scripted {
    fn prepare(&mut self, _launch: &Launch) -> Result<StartIntent> {
        Ok(StartIntent::default())
    }

    fn start(&mut self, _launch: &Launch) -> Result<LaunchIdentity> {
        anyhow::ensure!(
            self.behaviour != Behaviour::CannotStart,
            "the runner refused the launch"
        );
        Ok(LaunchIdentity {
            pid: 4242,
            started: Some(1),
        })
    }

    fn observe(&mut self, _launch: &Launch) -> Observation {
        match self.behaviour {
            Behaviour::Exits => Observation::Exited { code: 0 },
            Behaviour::ExitsWith(code) => Observation::Exited { code },
            Behaviour::CannotStart => unreachable!("a launch that never started is observed"),
            Behaviour::KillsTheSupervisor => panic!("the supervisor died"),
        }
    }

    fn request_stop(&mut self, _launch: &Launch) {}

    fn terminate(&mut self, _launch: &Launch) {}

    fn recover(&self, _record: &LaunchRecord, _now_ms: i64) -> Recovery {
        self.recovery
    }

    fn cleanup(&mut self, _launch: &Launch) -> Result<Removed> {
        Ok(Removed::Launch)
    }
}

/// A clock that moves only when the loop waits.
struct Clock(Cell<i64>);

impl Pace for Clock {
    fn now_ms(&self) -> i64 {
        self.0.get()
    }

    fn pause(&mut self, duration: Duration, _stopping: bool) {
        self.0.set(self.0.get() + duration.as_millis() as i64);
    }

    fn free_bytes(&self) -> Result<u64> {
        Ok(u64::MAX)
    }
}

/// A project whose ready tasks are `tasks`: `next` offers the first one
/// not excluded, as the service does once a task is released.
struct Queue {
    tasks: Vec<String>,
    asked: Mutex<Vec<String>>,
}

impl Api for Queue {
    fn next(&self, _project: &str, exclude: &[String]) -> Result<Value> {
        self.asked.lock().unwrap().push(exclude.join(","));
        let Some(task) = self.tasks.iter().find(|t| !exclude.contains(t)) else {
            return Ok(json!({"action": null}));
        };
        Ok(json!({"action": {"kind": "claim_task", "title": task,
                  "call": {"body": {"task_id": task, "expected_task_revision": 1}}}}))
    }

    fn task(&self, _project: &str, _task: &str) -> Result<Value> {
        anyhow::bail!("no task detail in this test")
    }
}

/// A CLI that answers by command: claims and renewals with a lease,
/// releases from a queue of answers (success once it is empty), anything
/// else with success. Every call is recorded by its command.
#[derive(Default)]
struct Commands {
    releases: RefCell<VecDeque<CliOutput>>,
    calls: Rc<RefCell<Vec<String>>>,
}

impl Cli for Commands {
    fn run(&self, _session: &str, args: &[String], _input: &[u8]) -> Result<CliOutput> {
        self.calls.borrow_mut().push(args[0].clone());
        let answer = match args[0].as_str() {
            "claim" | "renew" => ok(lease_body()),
            "release" => (self.releases.borrow_mut().pop_front()).unwrap_or_else(|| ok(json!({}))),
            _ => ok(json!({})),
        };
        Ok(answer)
    }
}

fn ok(body: Value) -> CliOutput {
    CliOutput {
        code: Some(0),
        stdout: serde_json::to_vec(&body).unwrap(),
        stderr: Vec::new(),
    }
}

fn refused(code: i32, error: &str) -> CliOutput {
    let body = json!({"error": {"code": error, "message": "refused"}});
    CliOutput {
        code: Some(code),
        ..ok(body)
    }
}

/// A `claim --json` answer.
fn lease_body() -> Value {
    let attempt = json!({"id": "a1", "generation": 1,
                         "expires_at": "2026-11-26T02:00:00Z",
                         "last_progress_at": "2026-11-26T01:00:00Z"});
    json!({"data": {"claim": {"attempt": attempt, "lease_remaining_ms": 3_600_000},
                    "renew_after_seconds": 60}})
}

/// A state directory and the loop settings the service gives a coordinator.
fn setup() -> (tempfile::TempDir, Config) {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config {
        state_dir: dir.path().join("state"),
        ..Config::default()
    };
    std::fs::create_dir(&config.state_dir).unwrap();
    (
        config.run.empty_exit_limit,
        config.run.empty_exit_total_limit,
    ) = (1, 0);
    config.run.hold_every_release = true;
    config.run.drain_seconds = 5;
    (dir, config)
}

type UnderTest = LoopDriver<Recorded<Scripted>, CoordinatorWork<Queue, Commands>, Clock>;

/// A loop over the ready `tasks`, keeping its ledger in `root`, whose
/// launches and recoveries follow `host` and whose CLI is `cli`.
fn driver(root: &Path, tasks: &[&str], host: Scripted, cli: Commands) -> UnderTest {
    let ledger = Ledger::at(&root.join("ledger.jsonl"));
    let api = Queue {
        tasks: tasks.iter().map(|t| (*t).to_owned()).collect(),
        asked: Mutex::default(),
    };
    let work = CoordinatorWork::new("p1", Arc::new(api), cli, ledger.clone());
    let host = Recorded::new(host, ledger);
    LoopDriver::new(
        host,
        work,
        Clock(Cell::new(1_000_000)),
        Arc::new(Signal::default()),
    )
}

fn scripted(behaviour: Behaviour, recovery: Recovery) -> Scripted {
    Scripted {
        behaviour,
        recovery,
    }
}

/// What the judge finds wrong in the coordinator ledger in `root`, which
/// holds checkpoints and attempt fields beside the host's events.
fn judged(root: &Path) -> Vec<String> {
    judge_coordinator(&Ledger::at(&root.join("ledger.jsonl")).events())
}

#[test]
fn the_coordinator_judge_allows_a_relaunch_after_a_release_and_finds_an_overlap() {
    let event =
        |name: &str, session: &str| json!({"event": name, "task": "t1", "session": session});
    let closed = |session: &str| {
        let cleanup = json!({"event": "cleanup", "session": session, "ok": true});
        [event("exited", session), event("release", session), cleanup]
    };
    let mut sequential = vec![event("start", "s1")];
    sequential.extend(closed("s1"));
    sequential.push(event("start", "s2"));
    sequential.extend(closed("s2"));
    assert_eq!(judge_coordinator(&sequential), Vec::<String>::new());
    assert_eq!(judge(&sequential), ["t1 was launched 2 times"]);
    let mut overlap = vec![event("start", "s1"), event("start", "s2")];
    overlap.extend(closed("s1"));
    overlap.extend(closed("s2"));
    let found = judge_coordinator(&overlap);
    assert_eq!(found, ["t1 was started again before s1 was released"]);
}

/// The ledger's event names, in order, leaving out the host's
/// observations and recoveries.
fn events(root: &Path) -> Vec<String> {
    let all = Ledger::at(&root.join("ledger.jsonl")).events();
    let names = all.iter().map(|e| e["event"].as_str().unwrap().to_owned());
    let kept = ["claim", "start", "checkpoint", "release"];
    names.filter(|n| kept.contains(&n.as_str())).collect()
}

#[test]
fn a_round_claims_launches_checkpoints_and_releases_then_the_task_is_left_out() {
    let (dir, config) = setup();
    let calls = Rc::new(RefCell::new(Vec::new()));
    let cli = Commands {
        calls: calls.clone(),
        ..Commands::default()
    };
    let host = scripted(Behaviour::Exits, Recovery::Ended);
    let mut driver = driver(dir.path(), &["t1"], host, cli);
    let launched = Outcome::Launched {
        task: "t1".into(),
        exit_code: 0,
    };
    assert_eq!(iterate(&mut driver, &config), launched);
    assert_eq!(
        *calls.borrow(),
        ["connect", "claim", "checkpoint", "release"]
    );
    assert_eq!(
        events(dir.path()),
        ["claim", "start", "checkpoint", "release"]
    );
    // The clean exit without a submission holds the task: the next poll
    // asks `next` to leave it out, and nothing more is claimed.
    assert_eq!(iterate(&mut driver, &config), Outcome::Idle);
    assert_eq!(calls.borrow().len(), 4);
    assert!(LaunchRecord::load_all(&config).is_empty());
    assert_eq!(judged(dir.path()), Vec::<String>::new());
}

/// Whether each start the ledger in `root` asked of the runner succeeded.
fn start_results(root: &Path) -> Vec<bool> {
    let all = Ledger::at(&root.join("ledger.jsonl")).events();
    (all.iter().filter(|e| e["event"] == "start_result"))
        .map(|e| e["ok"].as_bool().unwrap())
        .collect()
}

/// What two polls did: their outcomes, the CLI's commands, the ledger's
/// events and whether each start succeeded.
type TwoPolls = (Outcome, Outcome, Vec<String>, Vec<String>, Vec<bool>);

/// Polls twice over one ready task whose launch behaves as `behaviour`. A
/// broken fixture must not churn claims: the task is claimed and released
/// once, then held.
fn polled_twice(behaviour: Behaviour) -> TwoPolls {
    let (dir, config) = setup();
    let calls = Rc::new(RefCell::new(Vec::new()));
    let cli = Commands {
        calls: calls.clone(),
        ..Commands::default()
    };
    let host = scripted(behaviour, Recovery::Ended);
    let mut driver = driver(dir.path(), &["t1"], host, cli);
    let first = iterate(&mut driver, &config);
    let second = iterate(&mut driver, &config);
    assert!(LaunchRecord::load_all(&config).is_empty());
    assert_eq!(judged(dir.path()), Vec::<String>::new());
    let calls = calls.borrow().clone();
    let started = start_results(dir.path());
    (first, second, calls, events(dir.path()), started)
}

#[test]
fn a_fixture_that_exits_non_zero_is_claimed_once_then_held() {
    let (first, second, calls, events, started) = polled_twice(Behaviour::ExitsWith(3));
    let launched = Outcome::Launched {
        task: "t1".into(),
        exit_code: 3,
    };
    assert_eq!(first, launched);
    assert_eq!(second, Outcome::Idle);
    assert_eq!(calls, ["connect", "claim", "checkpoint", "release"]);
    assert_eq!(events, ["claim", "start", "checkpoint", "release"]);
    assert_eq!(started, [true]);
}

#[test]
fn a_start_that_fails_after_the_claim_is_claimed_once_then_held() {
    let (first, second, calls, events, started) = polled_twice(Behaviour::CannotStart);
    assert!(
        matches!(&first, Outcome::Failed(why) if why.contains("the runner refused the launch")),
        "{first:?}"
    );
    assert_eq!(second, Outcome::Idle);
    assert_eq!(calls, ["connect", "claim", "checkpoint", "release"]);
    // The ledger shows the start asked for, and refused.
    assert_eq!(events, ["claim", "start", "checkpoint", "release"]);
    assert_eq!(started, [false]);
}

#[test]
fn an_interrupted_launch_is_released_only_once_judged_ended_and_relaunched_only_after() {
    let (dir, config) = setup();
    let died = std::panic::catch_unwind(|| {
        let host = scripted(Behaviour::KillsTheSupervisor, Recovery::Ended);
        let mut driver = driver(dir.path(), &["t1"], host, Commands::default());
        iterate(&mut driver, &config)
    });
    assert!(died.is_err());
    assert_eq!(events(dir.path()), ["claim", "start"]);
    // Restarted while the launch may still run: nothing is released,
    // claimed or started, and `next` is not even asked.
    let calls = Rc::new(RefCell::new(Vec::new()));
    let cli = || Commands {
        calls: calls.clone(),
        ..Commands::default()
    };
    let waiting = scripted(Behaviour::Exits, Recovery::MayBeRunning);
    let mut driver = driver(dir.path(), &["t1"], waiting, cli());
    let outcome = iterate(&mut driver, &config);
    assert!(matches!(&outcome, Outcome::Refused(why) if why.contains("may still run")));
    assert!(calls.borrow().is_empty());
    // Judged ended: its attempt is released first, then the task, which no
    // clean exit held, is claimed and launched once more.
    let ended = scripted(Behaviour::Exits, Recovery::Ended);
    let mut driver = self::driver(dir.path(), &["t1"], ended, cli());
    let launched = Outcome::Launched {
        task: "t1".into(),
        exit_code: 0,
    };
    assert_eq!(iterate(&mut driver, &config), launched);
    let expected = [
        "checkpoint",
        "release",
        "connect",
        "claim",
        "checkpoint",
        "release",
    ];
    assert_eq!(*calls.borrow(), expected);
    let order = [
        "claim",
        "start",
        "checkpoint",
        "release",
        "claim",
        "start",
        "checkpoint",
        "release",
    ];
    assert_eq!(events(dir.path()), order);
    assert!(LaunchRecord::load_all(&config).is_empty());
    assert_eq!(judged(dir.path()), Vec::<String>::new());
}

#[test]
fn a_recovered_attempt_that_already_ended_counts_as_released() {
    let (dir, config) = setup();
    let died = std::panic::catch_unwind(|| {
        let host = scripted(Behaviour::KillsTheSupervisor, Recovery::Ended);
        let mut driver = driver(dir.path(), &["t1"], host, Commands::default());
        iterate(&mut driver, &config)
    });
    assert!(died.is_err());
    // The lease ran out while the host was down: the release is refused
    // with a conflict, which settles the record all the same.
    let cli = Commands {
        releases: RefCell::new(VecDeque::from([refused(5, "lease_expired")])),
        ..Commands::default()
    };
    let host = scripted(Behaviour::Exits, Recovery::Ended);
    let mut driver = driver(dir.path(), &[], host, cli);
    assert_eq!(iterate(&mut driver, &config), Outcome::Idle);
    assert!(LaunchRecord::load_all(&config).is_empty());
}

#[test]
fn a_release_that_cannot_reach_the_coordinator_keeps_the_record_for_the_next_poll() {
    let (dir, config) = setup();
    let died = std::panic::catch_unwind(|| {
        let host = scripted(Behaviour::KillsTheSupervisor, Recovery::Ended);
        let mut driver = driver(dir.path(), &["t1"], host, Commands::default());
        iterate(&mut driver, &config)
    });
    assert!(died.is_err());
    // Booted before the network: the release fails and the record stays.
    let cli = Commands {
        releases: RefCell::new(VecDeque::from([refused(7, "transport_failure")])),
        ..Commands::default()
    };
    let host = scripted(Behaviour::Exits, Recovery::Ended);
    let mut driver = driver(dir.path(), &[], host, cli);
    iterate(&mut driver, &config);
    let kept = LaunchRecord::load_all(&config);
    assert_eq!(kept.len(), 1);
    assert!(!kept[0].released);
    // The next poll, with the network back, releases and settles it.
    assert_eq!(iterate(&mut driver, &config), Outcome::Idle);
    assert!(LaunchRecord::load_all(&config).is_empty());
    let releases = events(dir.path())
        .iter()
        .filter(|e| *e == "release")
        .count();
    assert_eq!(releases, 1, "only the release that succeeded is recorded");
}
