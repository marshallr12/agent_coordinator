//! The runner-crash round of the third phase 4c machine run, end to end
//! through the supervisor's loop, without a real launch (an interactive
//! desktop cannot make one).
//!
//! The first runner journals the launch as a real one does, prepared and
//! then started, and "crashes". The restarted runner is a real [`Runner`]
//! over the same journal, settled as the runner service settles it at
//! start; it answers every later request. On the machine the supervisor
//! then asked about the launch once a second for twelve hours. Here it
//! must see the launch gone, release its attempt and clean it up, and the
//! launch's directories must be gone.
use super::super::job::Limits;
use super::super::journal::{Entry, Journal, Stage};
use super::super::launcher::LaunchSpec;
use super::super::runner::{Runner, answer};
use super::*;
use crate::config::Config;
use crate::run_loop::{Outcome, UNSEEN_EXIT_CODE, iterate};
use crate::service::lifecycle::Signal;
use crate::service::stand_in::{Ledger, Recorded, StandIn, judge};
use crate::service::startup;
use crate::service::supervisor::{LoopDriver, Pace};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// A pid no process has: Windows says so (see the recovery tests).
const DEAD_PID: u32 = 0xFFFF_FFF1;

/// A runner that crashes once its launch started; see the module.
struct Crashing {
    /// The runner's journal.
    state: PathBuf,
    /// Where launch directories are made.
    launches: PathBuf,
    /// The runner the service manager started again, once it has.
    restarted: Option<Runner>,
}

impl Crashing {
    fn journal(&self) -> Journal {
        Journal::open(&self.state).unwrap()
    }

    /// What the first runner leaves for `prepare`: the launch's two
    /// directories, each with a file in it, and a journal entry naming them.
    fn prepare(&self, launch: &str) -> Value {
        let control = self.launches.join(format!("{launch}-control"));
        let work = self.launches.join(format!("{launch}-work"));
        for directory in [&control, &work] {
            std::fs::create_dir_all(directory).unwrap();
            std::fs::write(directory.join("left.txt"), b"x").unwrap();
        }
        let owner = format!("runner:{DEAD_PID}:1");
        self.journal()
            .save(&entry(launch, &owner, control, work))
            .unwrap();
        json!({"ok": true, "package_sid": "S-1-15-2-7", "job_owner": owner})
    }

    /// What the first runner journals once Windows gave the launch a root.
    fn start(&self, launch: &str) -> Value {
        let mut entry = self.journal().load(launch).unwrap();
        (entry.stage, entry.pid, entry.created) = (Stage::Started, Some(DEAD_PID), Some(1));
        self.journal().save(&entry).unwrap();
        json!({"ok": true, "pid": DEAD_PID, "created": 1})
    }

    /// The service manager starts the runner again; the service settles its
    /// journal before it serves anyone.
    fn restart(&mut self) {
        let mut runner = Runner::new(&self.state).unwrap();
        let settled = startup::settle(&mut runner).unwrap();
        assert_eq!(settled.reclaimed.len(), 1, "{:?}", settled.lines());
        self.restarted = Some(runner);
    }
}

/// The first runner's journal entry for a prepared `launch`.
fn entry(launch: &str, owner: &str, control: PathBuf, work: PathBuf) -> Entry {
    Entry {
        launch: launch.to_owned(),
        stage: Stage::Prepared,
        profile: format!("agentc.test.{launch}"),
        package_sid: "S-1-15-2-7".into(),
        job_owner: owner.to_owned(),
        executable: None,
        source: None,
        attempt: None,
        generation: None,
        pid: None,
        created: None,
        control: Some(control),
        work: Some(work),
    }
}

impl Link for Crashing {
    /// The first runner answers `prepare` and `start`; it crashes before
    /// the next request, which the restarted runner answers.
    fn ask(&mut self, request: &Value) -> Result<Value> {
        let launch = request["launch"].as_str().unwrap_or("").to_owned();
        if self.restarted.is_none() {
            match request["op"].as_str() {
                Some("prepare") => return Ok(self.prepare(&launch)),
                Some("start") => return Ok(self.start(&launch)),
                _ => self.restart(),
            }
        }
        let runner = self.restarted.as_mut().expect("restarted above");
        let parsed = serde_json::from_value(request.clone()).map_err(anyhow::Error::from);
        Ok(answer(parsed.and_then(|request| runner.handle(request))))
    }
}

/// Plans one fixed boundary and command; the link above ignores both.
struct Fixed;

impl Planner for Fixed {
    fn plan(&self, launch: &Launch) -> Result<(LaunchSpec, Command)> {
        let spec = LaunchSpec {
            profile: format!("agentc.test.{}", launch.session_id.simple()),
            less_privileged: true,
            capabilities: vec![],
            limits: Limits {
                processes: 4,
                process_memory_mb: 64,
                job_memory_mb: 128,
            },
            control: "c".into(),
            work: "w".into(),
        };
        let command = Command {
            argv: vec!["fixture.exe".into()],
            directory: "w".into(),
            environment: BTreeMap::new(),
            output: "w/out.log".into(),
        };
        Ok((spec, command))
    }
}

/// A clock that moves only when the loop waits.
struct Steps(Cell<i64>);

impl Pace for Steps {
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

/// A queue holding `item`, a loop configuration over `root` and a driver
/// whose runner crashes once the launch started.
fn setup(root: &Path, item: &str) -> (Config, impl crate::run_loop::Driver) {
    for directory in ["queue", "state", "runner", "launches"] {
        std::fs::create_dir(root.join(directory)).unwrap();
    }
    std::fs::write(root.join("queue").join(item), b"").unwrap();
    let mut config = Config {
        state_dir: root.join("state"),
        ..Config::default()
    };
    (
        config.run.empty_exit_limit,
        config.run.empty_exit_total_limit,
    ) = (0, 0);
    let link = Crashing {
        state: root.join("runner"),
        launches: root.join("launches"),
        restarted: None,
    };
    let ledger = || Ledger::at(&root.join("ledger.jsonl"));
    let host = Recorded::new(RemoteHost::new(Fixed, link), ledger());
    let work = StandIn::new(&root.join("queue"), ledger());
    let pace = Steps(Cell::new(1_000_000));
    let driver = LoopDriver::new(host, work, pace, Arc::new(Signal::default()));
    (config, driver)
}

#[test]
fn a_launch_the_restarted_runner_reclaimed_is_released_and_cleaned_up() {
    let root = tempfile::tempdir().unwrap();
    let (config, mut driver) = setup(root.path(), "item-a");
    let launched = Outcome::Launched {
        task: "item-a".into(),
        exit_code: UNSEEN_EXIT_CODE,
    };
    assert_eq!(iterate(&mut driver, &config), launched);
    let events = Ledger::at(&root.path().join("ledger.jsonl")).events();
    let names: Vec<&str> = events.iter().filter_map(|e| e["event"].as_str()).collect();
    assert_eq!(
        names,
        [
            "prepare",
            "claim",
            "start",
            "start_result",
            "gone",
            "release",
            "cleanup"
        ]
    );
    let summary = events[5]["summary"].as_str().unwrap();
    assert!(summary.contains("ended unseen"), "{summary}");
    assert_eq!(events[6]["removed"], "nothing", "the runner removed it all");
    assert_eq!(judge(&events), Vec::<String>::new());
    // The supervisor's record is gone, and so is everything at the runner.
    assert!(crate::run_loop::record::LaunchRecord::load_all(&config).is_empty());
    let left = std::fs::read_dir(root.path().join("launches")).unwrap();
    assert_eq!(left.count(), 0, "the launch's directories are removed");
    let journal = Journal::open(&root.path().join("runner")).unwrap();
    assert!(journal.load_all().unwrap().0.is_empty());
    assert_eq!(iterate(&mut driver, &config), Outcome::Idle);
}
