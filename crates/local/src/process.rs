#[cfg(any(target_os = "linux", windows))]
use std::io;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::CommandSpec;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub start_identity: String,
}

pub fn capture(pid: u32) -> Result<Option<ProcessIdentity>> {
    capture_platform(pid)
}

pub fn is_alive(identity: &ProcessIdentity) -> Result<bool> {
    Ok(capture(identity.pid)?.as_ref() == Some(identity))
}

pub(crate) fn spawn_producer(command: &CommandSpec, capture_logs: bool) -> Result<Child> {
    let mut child = Command::new(&command.program);
    child
        .args(&command.args)
        .current_dir(&command.working_directory)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(if capture_logs {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(if capture_logs {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    copy_platform_environment(&mut child);
    child.envs(&command.environment);
    configure_producer(&mut child);
    child.spawn().context("launch local producer")
}

pub(crate) fn spawn_guardian_process(
    executable: &Path,
    state_file: &Path,
    mode: crate::GuardianMode,
) -> Result<()> {
    if !executable.is_absolute() {
        bail!("guardian executable must be an absolute path");
    }
    let mut command = Command::new(executable);
    command
        .arg("__job-guardian")
        .arg("--state-file")
        .arg(state_file)
        .arg("--mode")
        .arg(mode.as_str())
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    copy_platform_environment(&mut command);
    configure_guardian(&mut command);
    command
        .spawn()
        .context("start detached local job guardian")?;
    Ok(())
}

fn copy_platform_environment(command: &mut Command) {
    const ALLOWED: &[&str] = &[
        "PATH",
        "SystemRoot",
        "WINDIR",
        "TEMP",
        "TMP",
        "TMPDIR",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
    ];
    for name in ALLOWED {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
}

#[cfg(unix)]
fn configure_producer(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(windows)]
fn configure_producer(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

#[cfg(not(any(unix, windows)))]
fn configure_producer(_command: &mut Command) {}

#[cfg(unix)]
fn configure_guardian(command: &mut Command) {
    configure_producer(command);
}

#[cfg(windows)]
fn configure_guardian(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};
    command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
}

#[cfg(not(any(unix, windows)))]
fn configure_guardian(_command: &mut Command) {}

#[cfg(target_os = "linux")]
fn capture_platform(pid: u32) -> Result<Option<ProcessIdentity>> {
    let stat_path = format!("/proc/{pid}/stat");
    let stat = match std::fs::read_to_string(&stat_path) {
        Ok(value) => value,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {stat_path}")),
    };
    let after_name = stat
        .rfind(") ")
        .and_then(|index| stat.get(index + 2..))
        .context("parse Linux process stat")?;
    let mut fields = after_name.split_whitespace();
    let process_state = fields
        .next()
        .context("Linux process stat omitted its state")?;
    if matches!(process_state, "Z" | "X") {
        return Ok(None);
    }
    let start_ticks = after_name
        .split_whitespace()
        .nth(19)
        .context("Linux process stat omitted its start time")?;
    let boot_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .context("read Linux boot identity")?;
    Ok(Some(ProcessIdentity {
        pid,
        start_identity: format!("linux:{}:{start_ticks}", boot_id.trim()),
    }))
}

#[cfg(windows)]
fn capture_platform(pid: u32) -> Result<Option<ProcessIdentity>> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        let error = io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(5 | 87 | 1168) => Ok(None),
            _ => Err(error).context("open Windows process for identity"),
        };
    }
    let mut creation = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut exit = creation;
    let mut kernel = creation;
    let mut user = creation;
    let success =
        unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) };
    unsafe { CloseHandle(handle) };
    if success == 0 {
        return Err(io::Error::last_os_error()).context("read Windows process identity");
    }
    if exit.dwLowDateTime != 0 || exit.dwHighDateTime != 0 {
        return Ok(None);
    }
    let ticks = (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
    Ok(Some(ProcessIdentity {
        pid,
        start_identity: format!("windows-filetime:{ticks}"),
    }))
}

#[cfg(all(unix, not(target_os = "linux")))]
fn capture_platform(_pid: u32) -> Result<Option<ProcessIdentity>> {
    bail!("strong process identity is currently supported only on Linux and Windows")
}

#[cfg(not(any(unix, windows)))]
fn capture_platform(_pid: u32) -> Result<Option<ProcessIdentity>> {
    bail!("process identity is unsupported on this platform")
}
