//! Process access, child containment, job limits and process-creation escapes.

use crate::ffi;
use crate::report::Report;
use crate::spec::Spec;
use std::io;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

/// Records host process attacks, own-child controls, escapes and job limits.
pub fn run(spec: &Spec, report: &mut Report) -> Result<(), String> {
    let host: u32 = number(spec, "host_pid")?;
    report.attack("host.process_open_vm", open(ffi::PROCESS_VM_ALL, host));
    report.attack(
        "host.process_open_query",
        open(ffi::PROCESS_QUERY_LIMITED, host),
    );
    report.attack(
        "host.process_open_terminate",
        open(ffi::PROCESS_TERMINATE, host),
    );
    report.attack("host.process_open_dup", open(ffi::PROCESS_DUP_HANDLE, host));
    report.control(
        "own.child_contained",
        child_contained(spec.require("package_sid")?),
    );
    report.control("own.child_open", open_own_child());
    report.attack("escape.breakaway", breakaway());
    limits(spec, report)
}

/// Opens a process with one access right.
fn open(access: u32, pid: u32) -> io::Result<()> {
    ffi::open_process(access, pid).map(|_| ())
}

/// This executable, re-run in a helper mode.
pub fn helper(mode: &[&str]) -> io::Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(mode)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Ok(command)
}

/// A child must start inside the same package and job; it checks that itself.
fn child_contained(package_sid: &str) -> io::Result<()> {
    let status = helper(&["child", package_sid])?.status()?;
    match status.code() {
        Some(0) => Ok(()),
        code => Err(io::Error::from_raw_os_error(code.unwrap_or(-1))),
    }
}

/// Positive control for the process-open code path: an own child can be opened.
fn open_own_child() -> io::Result<()> {
    let mut child = helper(&["sleeper", "60"])?.spawn()?;
    let opened = open(ffi::PROCESS_VM_ALL, child.id());
    stop(&mut child);
    opened
}

/// Kills and reaps a helper child.
fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Asks for a child outside the job; the job must refuse.
fn breakaway() -> io::Result<()> {
    let mut child = helper(&["exit"])?
        .creation_flags(CREATE_BREAKAWAY_FROM_JOB)
        .spawn()?;
    stop(&mut child);
    Ok(())
}

/// A program in the Windows system directory named by the launch environment.
pub fn system_tool(name: &str) -> PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    PathBuf::from(root).join("System32").join(name)
}

/// Records how the job bounds process count and memory, and leaves one orphan behind.
fn limits(spec: &Spec, report: &mut Report) -> Result<(), String> {
    let ceiling: usize = number(spec, "process_limit")?;
    let (spawned, refusal) = spawn_until_refused(ceiling + 8);
    report.value("job.spawned_before_refusal", &spawned.to_string());
    report.attack("job.exceed_process_limit", refusal);
    let megabytes: usize = number(spec, "memory_limit_mb")?;
    report.value("job.memory_refused_at_mb", &memory_refusal(megabytes * 2));
    report.value("job.orphan_pid", &orphan());
    Ok(())
}

/// Spawns sleepers until the job refuses one; reaching `ceiling` means no limit applied.
fn spawn_until_refused(ceiling: usize) -> (usize, io::Result<()>) {
    let mut children = Vec::new();
    let mut refusal = Ok(());
    while children.len() < ceiling {
        match helper(&["sleeper", "60"]).and_then(|mut command| command.spawn()) {
            Ok(child) => children.push(child),
            Err(error) => {
                refusal = Err(error);
                break;
            }
        }
    }
    let spawned = children.len();
    children.iter_mut().for_each(stop);
    (spawned, refusal)
}

/// Asks a child to commit memory until refused; its exit code counts 64 MiB chunks.
fn memory_refusal(ceiling_mb: usize) -> String {
    let ceiling = ceiling_mb.to_string();
    let status = helper(&["memhog", &ceiling]).and_then(|mut command| command.status());
    match status.map(|status| status.code()) {
        Ok(Some(255)) => "unbounded".to_string(),
        Ok(Some(chunks)) => (chunks as usize * 64).to_string(),
        Ok(None) => "no-exit-code".to_string(),
        Err(error) => format!("spawn-failed:{}", crate::report::code(&error)),
    }
}

/// Leaves a long sleeper running so the driver can prove the job reaps descendants.
fn orphan() -> String {
    match helper(&["sleeper", "900"]).and_then(|mut command| command.spawn()) {
        Ok(child) => child.id().to_string(),
        Err(error) => format!("spawn-failed:{}", crate::report::code(&error)),
    }
}

/// Parses a required numeric spec value.
fn number<T: std::str::FromStr>(spec: &Spec, key: &str) -> Result<T, String> {
    spec.require(key)?
        .parse()
        .map_err(|_| format!("spec value {key} is not a number"))
}
