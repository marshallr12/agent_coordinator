//! The broker services' start-up without the Service Control Manager
//! (phase 6, step 6d): `broker_service::open` as the owner, with a state
//! directory and a control pipe of the test's own. Running both brokers as
//! virtual service accounts is the owner-attended run on MiniAir.
#![cfg(windows)]

use agentc_supervisor::host_runtime::windows::broker::{Handler, Role};
use agentc_supervisor::host_runtime::windows::broker_control::{Control, ControlClient};
use agentc_supervisor::host_runtime::windows::broker_service::open;
use agentc_supervisor::host_runtime::windows::egress_broker::{self, Egress};
use agentc_supervisor::host_runtime::windows::token;
use agentc_supervisor::service::broker_settings::EgressSettings;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// A package SID no launch has; a grant may name it all the same.
const PACKAGE: &str = "S-1-15-2-1-2-3-4-5-6-7";

/// The egress handler, allowing nothing.
fn handler() -> anyhow::Result<Arc<dyn Handler>> {
    Ok(Arc::new(Egress::public(Vec::new())))
}

/// Egress settings with the test's state directory and a control pipe of
/// its own, served to `runner`.
fn settings(state: &std::path::Path, runner: &str) -> EgressSettings {
    let id = uuid::Uuid::new_v4().simple();
    EgressSettings {
        state_dir: state.to_path_buf(),
        control_pipe: format!("agentc-test-egress-control-{id}"),
        runner_sid: runner.to_owned(),
        ..EgressSettings::default()
    }
}

/// A control pipe served on a thread until dropped.
struct Serving {
    done: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Serving {
    /// Serves `control` until the value is dropped.
    fn start(mut control: Control) -> Self {
        let done = Arc::new(AtomicBool::new(false));
        let stop = done.clone();
        let thread = std::thread::spawn(move || {
            let stopping = || stop.load(Ordering::SeqCst);
            while !stopping() {
                control
                    .serve_one(Duration::from_millis(100), &stopping)
                    .unwrap();
            }
        });
        let thread = Some(thread);
        Self { done, thread }
    }
}

impl Drop for Serving {
    /// Stops the thread and waits for it.
    fn drop(&mut self) {
        self.done.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

#[test]
fn an_opened_broker_makes_its_directory_and_serves_the_runner_its_launches() {
    let dir = tempfile::tempdir().unwrap();
    let (state, me) = (
        dir.path().join("egress"),
        token::current_user_sid().unwrap(),
    );
    let settings = settings(&state, &me);
    let control = open(settings.core(), egress_broker::SERVICE, handler, None).unwrap();
    assert!(state.is_dir(), "the state directory was not made");
    let _serving = Serving::start(control);
    let mut client = ControlClient::connect(&settings.control_pipe, &me).unwrap();
    let opened = client
        .open("bsv-launch", Role::Implementer, PACKAGE, 1)
        .unwrap();
    assert!(opened.pipe.contains("bsv-launch"), "{opened:?}");
    assert!(client.close("bsv-launch").unwrap());
    assert!(!client.close("bsv-launch").unwrap(), "closed twice");
}

#[test]
fn a_broker_without_a_runner_account_opens_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("egress");
    let settings = settings(&state, "");
    let refused = open(settings.core(), egress_broker::SERVICE, handler, None);
    let refused = refused.err().expect("opened without a runner account");
    assert!(refused.to_string().contains("runner_sid"), "{refused:#}");
    assert!(!state.exists(), "a refused broker made its directory");
}

/// `icacls` output for `path`.
fn icacls(path: &std::path::Path) -> String {
    let output = std::process::Command::new("icacls")
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn a_state_directory_made_before_the_broker_loses_a_looser_access_list() {
    let dir = tempfile::tempdir().unwrap();
    let (state, me) = (
        dir.path().join("egress"),
        token::current_user_sid().unwrap(),
    );
    std::fs::create_dir(&state).unwrap();
    let grant = std::process::Command::new("icacls")
        .arg(&state)
        .args(["/grant", "*S-1-1-0:(OI)(CI)F"])
        .output()
        .unwrap();
    assert!(grant.status.success() && icacls(&state).contains("Everyone"));
    let settings = settings(&state, &me);
    let control = open(settings.core(), egress_broker::SERVICE, handler, None).unwrap();
    assert!(!icacls(&state).contains("Everyone"), "{}", icacls(&state));
    drop(control);
}

#[test]
fn a_state_directory_another_account_owns_opens_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("egress");
    std::fs::create_dir(&state).unwrap();
    let owner = std::process::Command::new("icacls")
        .arg(&state)
        .args(["/setowner", "*S-1-1-0"])
        .output()
        .unwrap();
    if !owner.status.success() {
        // Giving a directory away needs a privilege the account may lack;
        // the owner check is then shown by the state_directory unit test.
        return;
    }
    let settings = settings(&state, &token::current_user_sid().unwrap());
    let refused = open(settings.core(), egress_broker::SERVICE, handler, None);
    let refused = format!("{:#}", refused.err().expect("opened a foreign directory"));
    assert!(refused.contains("is owned by"), "{refused}");
}

#[test]
fn a_control_pipe_another_account_owns_is_never_used_by_the_runner() {
    let dir = tempfile::tempdir().unwrap();
    let me = token::current_user_sid().unwrap();
    let settings = settings(&dir.path().join("egress"), &me);
    let control = open(settings.core(), egress_broker::SERVICE, handler, None).unwrap();
    let _serving = Serving::start(control);
    // The runner names the broker's account; this pipe's owner is not it.
    let refused = ControlClient::connect(&settings.control_pipe, "S-1-5-80-1-2-3-4-5");
    assert!(
        refused.is_err(),
        "a pipe the named broker does not own was used"
    );
}
