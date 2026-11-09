//! Drives real contained launches through the Windows runner's requests:
//! exit codes, the exact environment, write confinement, termination,
//! journaling and cleanup.
//!
//! A logon session has one unnamed window station, so one process can hold
//! one launcher; everything here is a single test. On a host where another
//! program already made the station, the runner must refuse every launch,
//! and the test checks exactly that instead.
#![cfg(windows)]

use agentc_supervisor::host_runtime::windows::runner::{Request, Runner, answer};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

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

/// The directories and names of one test launch below `root`.
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

    /// The `prepare` request: a less-privileged token with no network
    /// capability and small job limits.
    fn prepare(&self, profile: &str) -> Value {
        json!({"op": "prepare", "launch": "case", "source": "test", "attempt": "a1",
               "generation": 1,
               "spec": {"profile": profile, "less_privileged": true,
                        "capabilities": ["registryRead"],
                        "limits": {"processes": 8, "process_memory_mb": 256, "job_memory_mb": 512},
                        "control": self.control, "work": self.work}})
    }

    /// A `start` request running `cmd /d /c <script>` with log `name`.
    fn start(&self, script: &str, name: &str) -> Value {
        let local = std::env::var("LOCALAPPDATA").unwrap();
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        json!({"op": "start", "launch": "case", "argv": [cmd(), "/d", "/c", script],
               "directory": self.work, "output": self.logs.join(name),
               "environment": {"SystemRoot": root, "LOCALAPPDATA": local, "AGENTC_MARK": "kept"}})
    }

    /// Runs `script` to its end and returns its exit code and its log.
    fn run(&self, runner: &mut Runner, script: &str, name: &str) -> (u64, String) {
        ok(runner, self.start(script, name));
        let seen = ok(
            runner,
            json!({"op": "observe", "launch": "case", "wait_ms": 30_000}),
        );
        assert_eq!(seen["state"], "exited", "{script}: {seen}");
        let ended = ok(runner, json!({"op": "terminate", "launch": "case"}));
        assert_eq!(ended["quiescent"], true);
        let log = std::fs::read(self.logs.join(name)).unwrap();
        (
            seen["code"].as_u64().unwrap(),
            String::from_utf8_lossy(&log).into_owned(),
        )
    }
}

/// The stage the journal in `state` records for the test launch.
fn stage(state: &Path) -> String {
    let bytes = std::fs::read(state.join("case.json")).unwrap();
    let entry: Value = serde_json::from_slice(&bytes).unwrap();
    entry["stage"].as_str().unwrap().to_owned()
}

#[test]
fn a_launch_runs_contained_is_journaled_and_ends_with_its_job() {
    let dir = tempfile::tempdir().unwrap();
    let (state, case) = (dir.path().join("state"), Case::new(dir.path()));
    let mut runner = Runner::new(&state).unwrap();
    let profile = format!("agentc.test.{}", uuid::Uuid::new_v4().simple());
    let prepared = ask(&mut runner, case.prepare(&profile));
    if prepared["refusal"] == "station_not_ours" {
        refuses_everything(&mut runner, &case, &state);
        return;
    }
    assert_eq!(prepared["ok"], true, "{prepared}");
    let package = prepared["package_sid"].as_str().unwrap();
    assert!(package.starts_with("S-1-15-2-"), "{prepared}");
    assert_eq!(stage(&state), "prepared");
    exits_and_environment(&mut runner, &case);
    assert_eq!(stage(&state), "ended");
    write_confinement(&mut runner, &case);
    termination(&mut runner, &case, &state);
    let again = ask(&mut runner, case.prepare(&profile));
    assert_eq!(
        again["ok"], false,
        "a journaled launch is never prepared twice"
    );
    let cleaned = ok(&mut runner, json!({"op": "cleanup", "launch": "case"}));
    assert_eq!(cleaned["profile_deleted"], true, "{cleaned}");
    assert!(!state.join("case.json").exists());
}

/// A station this runner did not make: nothing is prepared or journaled.
fn refuses_everything(runner: &mut Runner, case: &Case, state: &Path) {
    eprintln!("another program made this logon session's window station; checking the refusal");
    assert!(!state.join("case.json").exists());
    let started = ask(runner, case.start("exit 0", "refused.txt"));
    assert_eq!(started["ok"], false, "{started}");
    assert!(!case.logs.join("refused.txt").exists());
}

/// The exit code comes back, and the process sees exactly the environment
/// it was given (the interpreter adds only its own defaults).
fn exits_and_environment(runner: &mut Runner, case: &Case) {
    let (code, _) = case.run(runner, "exit 7", "exit.txt");
    assert_eq!(code, 7);
    let (code, log) = case.run(runner, "set", "set.txt");
    assert_eq!(code, 0, "{log}");
    assert!(log.contains("AGENTC_MARK=kept"), "{log}");
    let names: Vec<&str> = (log.lines())
        .filter_map(|line| line.split_once('=').map(|(name, _)| name))
        .collect();
    // Windows itself points LOCALAPPDATA, TEMP and TMP of an AppContainer
    // process at the package's own folder; the interpreter adds the rest.
    let expected = [
        "AGENTC_MARK",
        "COMSPEC",
        "LOCALAPPDATA",
        "PATHEXT",
        "PROMPT",
        "SystemRoot",
        "TEMP",
        "TMP",
    ];
    assert_eq!(names, expected, "{log}");
}

/// The launch may write below its working directory and nowhere else it
/// was shown; the host reads what it wrote by handle.
/// The paths go to the interpreter unquoted: it does not parse quotes the
/// way the C runtime does, and a temporary directory has no spaces.
fn write_confinement(runner: &mut Runner, case: &Case) {
    let inside = format!("echo inside> {}", case.work.join("w.txt").display());
    let (code, log) = case.run(runner, &inside, "work.txt");
    let acl = std::process::Command::new("icacls")
        .arg(&case.work)
        .output()
        .unwrap();
    assert_eq!(
        code,
        0,
        "{log}
{}",
        String::from_utf8_lossy(&acl.stdout)
    );
    let read = ok(
        runner,
        json!({"op": "read", "root": case.work, "relative": "w.txt", "limit": 64}),
    );
    assert_eq!(read["hex"], hex::encode("inside\r\n"));
    let control = format!("echo x> {}", case.control.join("c.txt").display());
    assert_ne!(case.run(runner, &control, "control.txt").0, 0);
    assert!(!case.control.join("c.txt").exists());
    let outside = format!("echo x> {}", case.logs.join("o.txt").display());
    assert_ne!(case.run(runner, &outside, "outside.txt").0, 0);
    assert!(!case.logs.join("o.txt").exists());
}

/// A launch that never exits is seen running, is a member of its job, and
/// is gone once the job is terminated.
fn termination(runner: &mut Runner, case: &Case, state: &Path) {
    let started = ok(runner, case.start("for /l %i in () do @rem", "spin.txt"));
    assert_eq!(stage(state), "started");
    let seen = ok(
        runner,
        json!({"op": "observe", "launch": "case", "wait_ms": 200}),
    );
    assert_eq!(seen["state"], "running");
    let twice = ask(runner, case.start("exit 0", "twice.txt"));
    assert_eq!(twice["ok"], false, "one start at a time: {twice}");
    let watch = json!({"op": "watch", "launch": "case", "pid": started["pid"]});
    assert_eq!(ok(runner, watch)["alive_in_job"], true);
    let ended = ok(runner, json!({"op": "terminate", "launch": "case"}));
    assert_eq!(ended["quiescent"], true);
    let gone = ok(
        runner,
        json!({"op": "watched_ended", "launch": "case", "wait_ms": 10_000}),
    );
    assert_eq!(gone["ended"], true);
    assert_eq!(stage(state), "ended");
}

/// A journal entry an earlier runner left for launch `launch`: started as
/// process `pid`, its jobs owned by `owner`.
fn left_behind(state: &Path, launch: &str, owner: &str, pid: u32) {
    std::fs::create_dir_all(state).unwrap();
    let entry = json!({"launch": launch, "stage": "started", "profile": "agentc.test.gone",
                       "package_sid": "S-1-15-2-1", "job_owner": owner,
                       "executable": cmd(), "source": null, "attempt": null,
                       "generation": null, "pid": pid, "created": 1});
    std::fs::write(state.join(format!("{launch}.json")), entry.to_string()).unwrap();
}

#[test]
fn a_restarted_runner_judges_saved_launches_and_never_starts_them_again() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    // An odd pid names no process, so both the owner and the root are gone.
    left_behind(&state, "gone", "runner:4294967281:1", 0xFFFF_FFF3);
    // This test process is alive: a launch it owned may still run.
    let mut other = Runner::new(&dir.path().join("other")).unwrap();
    let me = ok(&mut other, json!({"op": "identity"}));
    left_behind(
        &state,
        "live",
        me["job_owner"].as_str().unwrap(),
        0xFFFF_FFF3,
    );
    std::fs::write(state.join("torn.json"), b"{").unwrap();
    let mut runner = Runner::new(&state).unwrap();
    let judged = ok(&mut runner, json!({"op": "recover"}));
    let verdicts: Vec<(&str, &str)> = (judged["launches"].as_array().unwrap().iter())
        .map(|l| {
            (
                l["launch"].as_str().unwrap(),
                l["verdict"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(verdicts, [("gone", "ended"), ("live", "may_be_running")]);
    assert_eq!(judged["unreadable"].as_array().unwrap().len(), 1);
    never_started_again(&mut runner, dir.path());
    let kept = ask(&mut runner, json!({"op": "reclaim", "launch": "live"}));
    assert_eq!(kept["ok"], false, "{kept}");
    assert!(state.join("live.json").exists());
    ok(&mut runner, json!({"op": "reclaim", "launch": "gone"}));
    assert!(!state.join("gone.json").exists());
}

/// Neither saved launch can be prepared or started by the new runner.
fn never_started_again(runner: &mut Runner, root: &Path) {
    let case = Case::new(root);
    for launch in ["gone", "live"] {
        let mut prepare = case.prepare("agentc.test.unused");
        prepare["launch"] = json!(launch);
        let refused = ask(runner, prepare);
        assert_eq!(refused["ok"], false, "{launch}: {refused}");
        let why = refused["error"].as_str().unwrap();
        assert!(why.contains("already journaled"), "{why}");
        let mut start = case.start("exit 0", "again.txt");
        start["launch"] = json!(launch);
        assert_eq!(ask(runner, start)["ok"], false, "{launch}");
    }
    assert!(!case.control.exists() && !case.logs.join("again.txt").exists());
}
