//! Drives the Windows backend through the `HostRuntime` interface the live
//! loop uses: prepare, start, observe, request_stop, terminate, recover and
//! cleanup, with the identities each returns.
//!
//! One process can hold one launcher, so this is a single test. Where
//! another program already made the logon session's window station, the
//! backend must refuse to prepare, and the test checks that instead.
#![cfg(windows)]

use agentc_supervisor::config::Config;
use agentc_supervisor::host_runtime::windows::desktop::StationNotOurs;
use agentc_supervisor::host_runtime::windows::job::Limits;
use agentc_supervisor::host_runtime::windows::launcher::{LaunchSpec, STOP_MARKER};
use agentc_supervisor::host_runtime::windows::process::Command;
use agentc_supervisor::host_runtime::windows::{Planner, WindowsHost};
use agentc_supervisor::host_runtime::{HostRuntime, Observation, Recovery, Removed, StartIntent};
use agentc_supervisor::run_loop::lease::Lease;
use agentc_supervisor::run_loop::record::LaunchRecord;
use agentc_supervisor::run_loop::{Launch, Suggestion};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Plans `cmd /d /c <script>` for every launch, with its directories and
/// log below `root`. The script comes from the launch's task id.
struct Scripted {
    root: PathBuf,
}

impl Planner for Scripted {
    fn plan(&self, launch: &Launch) -> anyhow::Result<(LaunchSpec, Command)> {
        let base = self.root.join(launch.session_id.to_string());
        let windows = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        let spec = LaunchSpec {
            profile: format!("agentc.test.{}", launch.session_id.simple()),
            less_privileged: true,
            capabilities: vec!["registryRead".into()],
            limits: Limits {
                processes: 8,
                process_memory_mb: 256,
                job_memory_mb: 512,
            },
            control: base.with_extension("control"),
            work: base.with_extension("work"),
        };
        let interpreter = format!("{windows}\\System32\\cmd.exe");
        let script = launch.suggestion.task.clone();
        let command = Command {
            argv: vec![interpreter, "/d".into(), "/c".into(), script],
            directory: spec.work.clone(),
            environment: BTreeMap::from([
                ("SystemRoot".to_owned(), windows),
                ("LOCALAPPDATA".to_owned(), std::env::var("LOCALAPPDATA")?),
            ]),
            output: base.with_extension("log"),
        };
        Ok((spec, command))
    }
}

/// A launch whose task id is the script to run.
fn launch(script: &str) -> Launch {
    let suggestion = Suggestion {
        task: script.into(),
        revision: 1,
        title: String::new(),
    };
    Launch::plan(&Config::default(), "p", suggestion)
}

/// Observes `launch` until it exits or `seconds` pass.
fn exit_code(host: &mut impl HostRuntime, launch: &Launch, seconds: u64) -> Option<i32> {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline {
        if let Observation::Exited { code } = host.observe(launch) {
            return Some(code);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

/// The record the loop would save for `launch` from `intent`.
fn record(launch: &Launch, intent: &StartIntent) -> LaunchRecord {
    let lease = Lease {
        attempt: "a1".into(),
        generation: 1,
        renew_after_seconds: 60,
        progress_age_ms: 0,
    };
    LaunchRecord::new(launch, &lease, intent, 0)
}

#[test]
fn the_loop_interface_runs_stops_and_recovers_a_contained_launch() {
    let dir = tempfile::tempdir().unwrap();
    let planner = Scripted {
        root: dir.path().to_owned(),
    };
    let mut host = WindowsHost::new(planner).unwrap();
    assert!(host.account_sid().starts_with("S-1-5-"));
    let quick = launch("exit 3");
    let intent = match host.prepare(&quick) {
        Ok(intent) => intent,
        Err(error) => {
            assert!(
                error.downcast_ref::<StationNotOurs>().is_some(),
                "{error:#}"
            );
            eprintln!("another program made this session's window station; refusal checked");
            assert!(host.start(&quick).is_err());
            let left = std::fs::read_dir(dir.path()).unwrap().count();
            assert_eq!(left, 0, "a refused prepare leaves no launch directory");
            assert_eq!(host.cleanup(&quick).unwrap(), Removed::Nothing);
            return;
        }
    };
    intent_names_the_launch(&intent);
    runs_to_its_exit(&mut host, &quick, &intent);
    is_stopped_and_judged(&mut host, dir.path());
}

/// The intent carries what identifies the launch before it starts.
fn intent_names_the_launch(intent: &StartIntent) {
    let container = intent.container.as_deref().unwrap();
    assert!(container.starts_with("S-1-15-2-"), "{container}");
    assert!(intent.job.as_deref().unwrap().starts_with("runner:"));
    assert!(intent.executable.as_deref().unwrap().ends_with("cmd.exe"));
    assert_eq!(intent.boot_id, "");
}

/// A launch that exits by itself reports its code, and cleanup removes its
/// directories.
fn runs_to_its_exit(host: &mut impl HostRuntime, quick: &Launch, intent: &StartIntent) {
    let mut saved = record(quick, intent);
    assert_eq!(saved.job, intent.job);
    assert_eq!(host.recover(&saved, 0), Recovery::MayBeRunning);
    let identity = host.start(quick).unwrap();
    saved.identify(&identity);
    assert!(identity.pid > 0 && identity.started.unwrap() > 0);
    assert_eq!(exit_code(host, quick, 30), Some(3));
    assert_eq!(host.cleanup(quick).unwrap(), Removed::Launch);
    assert_eq!(
        host.cleanup(quick).unwrap(),
        Removed::Nothing,
        "already gone"
    );
}

/// A launch that never exits is asked to stop (the marker appears), is
/// terminated, reports an exit, and is then judged from its saved record.
fn is_stopped_and_judged(host: &mut impl HostRuntime, root: &Path) {
    let spin = launch("for /l %i in () do @rem");
    let intent = host.prepare(&spin).unwrap();
    let mut saved = record(&spin, &intent);
    saved.identify(&host.start(&spin).unwrap());
    assert_eq!(host.observe(&spin), Observation::Running);
    host.request_stop(&spin);
    let base = root.join(spin.session_id.to_string());
    let control = base.with_extension("control");
    assert!(control.join(STOP_MARKER).is_file());
    host.terminate(&spin);
    assert!(exit_code(host, &spin, 10).is_some());
    host.cleanup(&spin).unwrap();
    assert!(!control.exists());
    // The launch is gone but this process, its job owner, still runs.
    assert_eq!(host.recover(&saved, 0), Recovery::MayBeRunning);
    saved.job = Some("runner:4294967281:1".into());
    assert_eq!(host.recover(&saved, 0), Recovery::Ended);
}
