//! The `launch-root` and `launch` command lines: `launch-root` refuses to
//! run without root, and `launch` passes `--push-socket` and `--session-id`
//! into the launch it describes, changing nothing else without them.
#![cfg(target_os = "linux")]

use serde_json::Value;
use std::process::{Command, Output};

/// Runs the built supervisor with an empty configuration file.
fn supervisor(args: &[&str]) -> Output {
    let config = tempfile::NamedTempFile::new().unwrap();
    Command::new(env!("CARGO_BIN_EXE_agentc-supervisor"))
        .arg("--config")
        .arg(config.path())
        .args(args)
        .output()
        .unwrap()
}

/// The common arguments naming one implementer launch.
const SPEC: &[&str] = &[
    "--role=implementer",
    "--harness=claude",
    "--clone=/var/lib/agentc/impl/clones/c",
    "--run=/var/lib/agentc/impl/runs/r",
];

/// The `launch --dry-run` description with `extra` arguments.
fn described(extra: &[&str]) -> Value {
    let args = [&["launch", "--dry-run"], SPEC, extra].concat();
    let output = supervisor(&args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// A described list as strings.
fn strings(value: &Value) -> Vec<String> {
    serde_json::from_value(value.clone()).unwrap()
}

#[test]
fn launch_root_refuses_to_run_without_root() {
    // SAFETY: geteuid takes no arguments and touches no memory.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let output = supervisor(&[&["launch-root", "--task=t1"], SPEC].concat());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("launch-root must run as root"), "{stderr}");
}

#[test]
fn launch_passes_the_push_socket_and_session_id_and_nothing_else_changes() {
    let session = "0b5e3c4e-8d0a-4c4e-9f0e-3b1f8f1d2a10";
    let socket = "/var/lib/agentc/push/l1/sock/push.sock";
    let plain = described(&[&format!("--session-id={session}")]);
    let pushed = described(&[
        &format!("--session-id={session}"),
        &format!("--push-socket={socket}"),
        "--task=t1",
    ]);
    let args = strings(&plain["args"]);
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--session-id", session]),
        "{args:?}"
    );
    let env = strings(&plain["env"]);
    assert!(
        !env.iter().any(|entry| entry.contains("PUSH_SOCKET")),
        "{env:?}"
    );
    let mut pushed_args = strings(&pushed["args"]);
    let directory = "/var/lib/agentc/push/l1/sock";
    let mask = [
        "--tmpfs",
        "/var/lib/agentc/push",
        "--ro-bind",
        directory,
        directory,
    ];
    let at = pushed_args
        .windows(5)
        .position(|window| window == mask)
        .expect("the push root is not masked right before the socket bind");
    pushed_args.drain(at..at + 5);
    assert_eq!(pushed_args, args);
    let mut pushed_env = strings(&pushed["env"]);
    let entry = format!("AGENT_COORDINATOR_CANDIDATE_PUSH_SOCKET={socket}");
    let index = pushed_env
        .iter()
        .position(|e| *e == entry)
        .expect("no socket variable");
    pushed_env.remove(index);
    // An implementer task launch also joins the session `run` claimed in.
    let session_entry = format!("AGENT_COORDINATOR_SESSION={session}");
    let index = pushed_env
        .iter()
        .position(|e| *e == session_entry)
        .expect("no session variable");
    pushed_env.remove(index);
    assert_eq!(pushed_env, env);
}
