//! The fixture's modes: one full launch and the small helpers it spawns.

use crate::checks;
use crate::ffi;
use crate::report::Report;
use crate::spec::Spec;
use crate::victim::Resources;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

/// The first bytes of every canary the driver creates.
const CANARY_MARKER: &[u8] = b"synthetic";
/// How long a launch waits for the driver's `go` and `stop` files.
const DRIVER_WAIT: Duration = Duration::from_secs(300);

/// Runs the mode named by the first argument and returns its exit status.
pub fn dispatch(arguments: &[String]) -> i32 {
    let words: Vec<&str> = arguments.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["launch", name, control, work] => launch(name, Path::new(control), Path::new(work)),
        ["network", control, work] => network(Path::new(control), Path::new(work)),
        ["child", package_sid] => child(package_sid),
        ["sleeper", seconds] => sleeper(seconds),
        ["memhog", ceiling_mb] => memhog(ceiling_mb),
        ["read-handle", value] => read_handle(value),
        ["scheduler-connect"] => checks::scheduler::helper_status(),
        ["exit"] => 0,
        _ => 2,
    }
}

/// One launch: create own resources, wait for the driver, observe, then hold still.
///
/// The resources stay alive until `stop` so every peer can attack them.
fn launch(name: &str, control: &Path, work: &Path) -> i32 {
    let resources = Resources::create(name, work);
    if fs::write(work.join("ready"), std::process::id().to_string()).is_err() {
        return 3;
    }
    if !appeared(&control.join("go")) {
        return 4;
    }
    let rows = observe(control, work, &resources)
        .unwrap_or_else(|message| format!("fixture.error=value:{message}\n"));
    if publish(work, &rows).is_err() {
        return 5;
    }
    appeared(&control.join("stop"));
    0
}

/// One network round: repeat only the direct network attempts and report them.
///
/// The driver runs this once per firewall condition, so a round stays short
/// while the host's policy is changed.
fn network(control: &Path, work: &Path) -> i32 {
    let Ok(text) = fs::read_to_string(control.join("spec.txt")) else {
        return 3;
    };
    let Ok(spec) = Spec::parse(&text) else {
        return 4;
    };
    let mut report = Report::default();
    checks::network_only(&spec, &mut report);
    match publish(work, &report.render()) {
        Ok(()) => 0,
        Err(_) => 5,
    }
}

/// Reads the spec and gathers every row.
fn observe(control: &Path, work: &Path, resources: &Resources) -> Result<String, String> {
    let text = fs::read_to_string(control.join("spec.txt")).map_err(|error| error.to_string())?;
    let spec = Spec::parse(&text)?;
    let mut report = Report::default();
    resources.record(&mut report);
    checks::run(&spec, work, &mut report)?;
    Ok(report.render())
}

/// Writes the rows under a temporary name, then renames, so the driver never reads half.
fn publish(work: &Path, rows: &str) -> std::io::Result<()> {
    let partial = work.join("result.partial");
    fs::write(&partial, rows)?;
    fs::rename(partial, work.join("result.txt"))
}

/// Polls until a driver-written file exists or the wait runs out.
fn appeared(path: &Path) -> bool {
    let deadline = Instant::now() + DRIVER_WAIT;
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// A child reports through its exit status whether it inherited the boundary.
fn child(expected_sid: &str) -> i32 {
    if !checks::token::is_appcontainer().unwrap_or(false) {
        return 10;
    }
    if checks::token::package_sid().ok().as_deref() != Some(expected_sid) {
        return 11;
    }
    if !checks::token::in_job().unwrap_or(false) {
        return 12;
    }
    0
}

/// Reads through a raw handle value; exit status 0 means it reached a driver canary.
///
/// This runs as the first process of its own launch, because that is the
/// process a leaked host handle would land in. A value the launch does not own
/// fails the read or, under strict handle checks, ends the process.
fn read_handle(value: &str) -> i32 {
    let Ok(value) = value.parse::<usize>() else {
        return 2;
    };
    let mut buffer = [0_u8; 16];
    let mut read = 0_u32;
    // SAFETY: the buffer is writable for its length; a foreign value only fails the call.
    let succeeded = unsafe {
        ffi::ReadFile(
            value as ffi::Handle,
            buffer.as_mut_ptr(),
            16,
            &mut read,
            std::ptr::null_mut(),
        )
    };
    match succeeded != 0 && buffer.starts_with(CANARY_MARKER) {
        true => 0,
        false => 6,
    }
}

/// Sleeps, standing in for a long-running descendant.
fn sleeper(seconds: &str) -> i32 {
    std::thread::sleep(Duration::from_secs(seconds.parse().unwrap_or(0)));
    0
}

/// Commits 64 MiB chunks until refused; the exit status is the count, 255 if never refused.
fn memhog(ceiling_mb: &str) -> i32 {
    const CHUNK: usize = 64 << 20;
    const COMMIT_AND_RESERVE: u32 = 0x3000;
    const READ_WRITE: u32 = 0x04;
    let chunks = (ceiling_mb.parse::<usize>().unwrap_or(0) / 64).min(254);
    for committed in 0..chunks {
        // SAFETY: a null base lets the system choose; the memory is never touched or freed.
        let block = unsafe {
            ffi::VirtualAlloc(std::ptr::null_mut(), CHUNK, COMMIT_AND_RESERVE, READ_WRITE)
        };
        if block.is_null() {
            return committed as i32;
        }
    }
    255
}
