//! The role runner's service parts that need no elevation: the refusal from
//! a console, the request gate, start-up judgment of a journal an ended
//! runner left, and a stop carried through a real launch.
//!
//! Nothing here installs, starts or configures a service. A service run
//! under the Service Control Manager is the owner's machine acceptance and
//! is not claimed by these tests. Where a launch needs a window station the
//! launcher made, the test follows `windows_runner.rs`: on an interactive
//! desktop it checks the refusal and nothing else.
#![cfg(windows)]

use agentc_supervisor::host_runtime::windows::runner::{Request, Runner, answer};
use agentc_supervisor::host_runtime::windows::runner_service::{MAX_WAIT_MS, TICK, admitted};
use agentc_supervisor::service::drain::{Drain, Fleet};
use agentc_supervisor::service::lifecycle::{
    Kind, Lifecycle, Limits, Monotonic, Report, Request as Ending, Signal,
};
use agentc_supervisor::service::startup;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Sends one request and returns the runner's answer line.
fn ask(runner: &mut Runner, request: Value) -> Value {
    let parsed = serde_json::from_value::<Request>(request).map_err(anyhow::Error::from);
    answer(parsed.and_then(|request| runner.handle(request)))
}

/// `ask`, failing the test unless the runner answered `ok`.
fn ok(runner: &mut Runner, request: Value) -> Value {
    let reply = ask(runner, request.clone());
    assert_eq!(reply["ok"], true, "{request} -> {reply}");
    reply
}

/// The command interpreter, which a less-privileged token may run.
fn cmd() -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    format!("{root}\\System32\\cmd.exe")
}

#[test]
fn started_from_a_console_the_service_command_refuses_and_says_why() {
    for role in [&[][..], &["--role", "runner"], &["--role", "supervisor"]] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_agentc-supervisor"))
            .arg("windows-service")
            .args(role)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{role:?}");
        let message = String::from_utf8_lossy(&output.stderr);
        assert!(message.contains("service manager"), "{role:?}: {message}");
    }
}

#[test]
fn a_request_that_begins_a_launch_is_refused_once_the_service_is_ending() {
    let begins = [
        json!({"op": "prepare", "launch": "l", "spec": {"profile": "p", "less_privileged": true,
               "capabilities": [], "limits": {"processes": 1, "process_memory_mb": 1,
               "job_memory_mb": 1}, "control": "c", "work": "w"}}),
        json!({"op": "hold_file", "launch": "l", "path": "f"}),
        json!({"op": "start", "launch": "l", "argv": ["a"], "directory": "d",
               "environment": {}, "output": "o"}),
    ];
    let continues = [
        json!({"op": "identity"}),
        json!({"op": "observe", "launch": "l", "wait_ms": 1}),
        json!({"op": "request_stop", "launch": "l"}),
        json!({"op": "terminate", "launch": "l"}),
        json!({"op": "watch", "launch": "l", "pid": 1}),
        json!({"op": "watched_ended", "launch": "l", "wait_ms": 1}),
        json!({"op": "cleanup", "launch": "l"}),
        json!({"op": "read", "root": "r", "relative": "x", "limit": 1}),
        json!({"op": "recover"}),
        json!({"op": "reclaim", "launch": "l"}),
    ];
    let (mut life, signal) = (
        Lifecycle::new(Monotonic::new(), limits()),
        Signal::default(),
    );
    let parse = |value: &Value| serde_json::from_value::<Request>(value.clone()).unwrap();
    for request in begins.iter().chain(&continues) {
        let kind = parse(request).kind();
        assert_eq!(
            life.admit(&signal, kind),
            Ok(()),
            "a running service admits {request}"
        );
    }
    signal.request(Ending::Stop);
    for request in &begins {
        assert_eq!(parse(request).kind(), Kind::Begins, "{request}");
        assert!(
            life.admit(&signal, parse(request).kind()).is_err(),
            "{request}"
        );
    }
    for request in &continues {
        assert_eq!(parse(request).kind(), Kind::Continues, "{request}");
        assert_eq!(
            life.admit(&signal, parse(request).kind()),
            Ok(()),
            "{request}"
        );
    }
}

/// The request the service would carry out for `value`, as debug text.
fn carried_out(life: &mut Lifecycle<Monotonic>, signal: &Signal, value: Value) -> String {
    let line = value.to_string();
    format!("{:?}", admitted(life, signal, line.as_bytes()).unwrap())
}

// The service has one thread. A wait longer than a tick would keep a stop
// recorded meanwhile from being acted on, so no wait is ever longer: not
// while the service runs, and not while it ends.
#[test]
fn no_wait_a_driver_asks_for_holds_the_service_longer_than_a_tick() {
    let (mut life, signal) = (
        Lifecycle::new(Monotonic::new(), limits()),
        Signal::default(),
    );
    assert!(Duration::from_millis(MAX_WAIT_MS) <= TICK);
    let long = [
        json!({"op": "observe", "launch": "l", "wait_ms": 3_600_000}),
        json!({"op": "watched_ended", "launch": "l", "wait_ms": u64::MAX}),
    ];
    for ending in [false, true] {
        if ending {
            signal.request(Ending::Stop);
        }
        for request in &long {
            let carried = carried_out(&mut life, &signal, request.clone());
            let cut = format!("wait_ms: {MAX_WAIT_MS}");
            assert!(carried.contains(&cut), "ending {ending}: {carried}");
        }
        let short = json!({"op": "observe", "launch": "l", "wait_ms": 10});
        let carried = carried_out(&mut life, &signal, short);
        assert!(carried.contains("wait_ms: 10"), "{carried}");
    }
}

#[test]
fn a_line_that_is_no_request_or_begins_a_launch_while_ending_is_not_carried_out() {
    let (mut life, signal) = (
        Lifecycle::new(Monotonic::new(), limits()),
        Signal::default(),
    );
    assert!(admitted(&mut life, &signal, b"{\"op\": \"run_anything\"}").is_err());
    signal.request(Ending::Preshutdown);
    let hold = json!({"op": "hold_file", "launch": "l", "path": "f"}).to_string();
    let refused = admitted(&mut life, &signal, hold.as_bytes()).unwrap_err();
    assert_eq!(answer(Err(refused))["refusal"], "service_stopping");
}

/// The limits of a service whose launches end within a few seconds.
fn limits() -> Limits {
    let seconds = Duration::from_secs;
    Limits {
        stop_grace: seconds(1),
        preshutdown_grace: seconds(1),
        terminate_patience: seconds(20),
        status_interval: seconds(1),
    }
}

/// A journal entry an earlier runner left for launch `launch`: its jobs
/// owned by `owner`, and started as process `pid` unless that is `None`.
fn left_behind(state: &Path, launch: &str, owner: &str, pid: Option<u32>) {
    std::fs::create_dir_all(state).unwrap();
    let stage = if pid.is_some() { "started" } else { "prepared" };
    let entry = json!({"launch": launch, "stage": stage, "profile": "agentc.test.gone",
                       "package_sid": "S-1-15-2-1", "job_owner": owner,
                       "executable": cmd(), "source": null, "attempt": null,
                       "generation": null, "pid": pid, "created": pid.map(|_| 1)});
    std::fs::write(state.join(format!("{launch}.json")), entry.to_string()).unwrap();
}

#[test]
fn start_up_reclaims_the_launches_that_ended_and_leaves_the_rest_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    // An odd pid names no process: both the owner and the root are gone.
    left_behind(
        &state,
        "gone-started",
        "runner:4294967281:1",
        Some(0xFFFF_FFF3),
    );
    left_behind(&state, "gone-prepared", "runner:4294967281:1", None);
    // This test process is alive, so a launch whose jobs it owns may run.
    let mut other = Runner::new(&dir.path().join("other")).unwrap();
    let me = ok(&mut other, json!({"op": "identity"}));
    let owner = me["job_owner"].as_str().unwrap();
    left_behind(&state, "live", owner, Some(0xFFFF_FFF3));
    // An owner Windows cannot be asked about is never taken for gone.
    left_behind(&state, "unknown", "garbage", Some(0xFFFF_FFF3));
    std::fs::write(state.join("torn.json"), b"{").unwrap();

    let mut runner = Runner::new(&state).unwrap();
    let settled = startup::settle(&mut runner).unwrap();
    assert_eq!(settled.reclaimed, ["gone-prepared", "gone-started"]);
    assert_eq!(settled.may_run, ["live", "unknown"]);
    assert_eq!(settled.unreadable.len(), 1);
    assert!(settled.failed.is_empty(), "{:?}", settled.failed);
    assert!(!state.join("gone-started.json").exists());
    assert!(!state.join("gone-prepared.json").exists());
    for kept in ["live", "unknown"] {
        assert!(
            state.join(format!("{kept}.json")).exists(),
            "{kept} was touched"
        );
    }
    assert!(
        state.join("torn.json").exists(),
        "an unreadable file is left as it is"
    );
    // Nothing is held by this runner, so nothing is waiting to be ended.
    assert!(runner.unconfirmed().is_empty());
}

#[test]
fn a_launch_the_runner_may_not_judge_ended_is_never_prepared_again() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let me = {
        let mut other = Runner::new(&dir.path().join("other")).unwrap();
        ok(&mut other, json!({"op": "identity"}))
    };
    left_behind(
        &state,
        "live",
        me["job_owner"].as_str().unwrap(),
        Some(0xFFFF_FFF3),
    );
    let mut runner = Runner::new(&state).unwrap();
    startup::settle(&mut runner).unwrap();
    let prepare = json!({"op": "prepare", "launch": "live",
        "spec": {"profile": "agentc.test.unused", "less_privileged": true,
                 "capabilities": [], "limits": {"processes": 1, "process_memory_mb": 64,
                 "job_memory_mb": 64},
                 "control": dir.path().join("c"), "work": dir.path().join("w")}});
    let refused = ask(&mut runner, prepare);
    assert_eq!(refused["ok"], false, "{refused}");
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("already journaled")
    );
    assert!(!dir.path().join("c").exists(), "nothing was made for it");
}

/// The directories of one test launch below `root`.
struct Case {
    control: PathBuf,
    work: PathBuf,
    logs: PathBuf,
}

impl Case {
    fn new(root: &Path) -> Self {
        let logs = root.join("logs");
        std::fs::create_dir(&logs).unwrap();
        Self {
            control: root.join("control"),
            work: root.join("work"),
            logs,
        }
    }

    fn prepare(&self, profile: &str) -> Value {
        json!({"op": "prepare", "launch": "case", "source": "test", "attempt": "a1",
               "generation": 1,
               "spec": {"profile": profile, "less_privileged": true,
                        "capabilities": ["registryRead"],
                        "limits": {"processes": 8, "process_memory_mb": 256, "job_memory_mb": 512},
                        "control": self.control, "work": self.work}})
    }

    /// A start of a command that never exits.
    fn spin(&self) -> Value {
        let local = std::env::var("LOCALAPPDATA").unwrap();
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        json!({"op": "start", "launch": "case",
               "argv": [cmd(), "/d", "/c", "for /l %i in () do @rem"],
               "directory": self.work, "output": self.logs.join("spin.txt"),
               "environment": {"SystemRoot": root, "LOCALAPPDATA": local}})
    }
}

/// The stage the journal in `state` records for the test launch.
fn stage(state: &Path) -> String {
    let bytes = std::fs::read(state.join("case.json")).unwrap();
    let entry: Value = serde_json::from_slice(&bytes).unwrap();
    entry["stage"].as_str().unwrap().to_owned()
}

#[test]
fn a_stop_asks_the_launch_ends_it_at_the_deadline_and_reports_stopped_only_when_confirmed() {
    let dir = tempfile::tempdir().unwrap();
    let (state, case) = (dir.path().join("state"), Case::new(dir.path()));
    let mut runner = Runner::new(&state).unwrap();
    let profile = format!("agentc.test.{}", uuid::Uuid::new_v4().simple());
    let prepared = ask(&mut runner, case.prepare(&profile));
    if prepared["refusal"] == "station_not_ours" {
        eprintln!("another program made this logon session's window station; checking the refusal");
        assert!(!state.join("case.json").exists());
        assert!(
            runner.unconfirmed().is_empty(),
            "nothing was prepared, so nothing is held"
        );
        return;
    }
    assert_eq!(prepared["ok"], true, "{prepared}");
    ok(&mut runner, case.spin());
    assert_eq!(runner.unconfirmed(), ["case"]);

    let signal = Signal::default();
    let mut life = Lifecycle::new(Monotonic::new(), limits());
    let mut drain = Drain::new();
    // A running service leaves the launch alone.
    let step = drain.step(&mut life, &signal, &mut runner);
    assert_eq!(step.report, Report::Running);
    assert!(!case.control.join("stop").exists());
    assert_eq!(stage(&state), "started");

    signal.request(Ending::Stop);
    assert!(
        life.admit(&signal, Kind::Begins).is_err(),
        "admission ends at once"
    );
    let mut last = Report::Running;
    for _ in 0..400 {
        let step = drain.step(&mut life, &signal, &mut runner);
        assert!(step.problems.is_empty(), "{:?}", step.problems);
        last = step.report;
        if matches!(last, Report::Stopped { .. }) {
            break;
        }
        assert!(
            runner.unconfirmed() == ["case"],
            "stopped is reported only after the launch is confirmed ended"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(last, Report::Stopped { quiescent: true });
    assert!(
        case.control.join("stop").exists(),
        "the launch was asked to stop first"
    );
    assert_eq!(
        stage(&state),
        "ended",
        "only a confirmed end is journaled as one"
    );
    assert!(runner.unconfirmed().is_empty());
    let cleaned = ok(&mut runner, json!({"op": "cleanup", "launch": "case"}));
    assert_eq!(cleaned["profile_deleted"], true, "{cleaned}");
}
