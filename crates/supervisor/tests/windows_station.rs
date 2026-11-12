//! The launcher in a logon session whose window station exists before the
//! launcher runs, which is how a service finds its session.
//!
//! Each test starts this test program again in a **new logon session** of
//! the same account (`CreateProcessWithLogonW` with network-only
//! credentials: no real credential is used and nothing is elevated). The
//! child leaves its console, so it is alone in that session, and makes the
//! session's unnamed station before any launcher code runs, as Windows does
//! for a service's process. It is not a run under the Service Control
//! Manager: the station there is made by Windows with Windows' own access
//! list, which only an installed service can show.
//!
//! - Attached to that station and alone: the launcher takes the station
//!   over, and Windows reports that the station and the launch's desktop
//!   admit only the runner's account and that launch's package.
//! - Not attached to it, or not alone: the launcher refuses, changes
//!   nothing in the station and leaves no launch directory.
#![cfg(windows)]

use agentc_supervisor::host_runtime::windows::adoption::{allowed, companions, station_name};
use agentc_supervisor::host_runtime::windows::inspection::{EVERY_RIGHT, granted};
use agentc_supervisor::host_runtime::windows::runner::{Request, Runner, answer};
use agentc_supervisor::host_runtime::windows::sys::Descriptor;
use agentc_supervisor::host_runtime::windows::token;
use serde_json::{Value, json};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_ALL, GetLastError, HANDLE, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Console::FreeConsole;
use windows_sys::Win32::System::StationsAndDesktops::{
    CloseDesktop, CreateWindowStationW, HWINSTA, OpenDesktopW, SetProcessWindowStation,
};
use windows_sys::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessWithLogonW, DETACHED_PROCESS, GetExitCodeProcess,
    LOGON_NETCREDENTIALS_ONLY, PROCESS_INFORMATION, STARTUPINFOW, WaitForSingleObject,
};
use windows_sys::Win32::UI::WindowsAndMessaging::CWF_CREATE_ONLY;

/// Says which part the child plays; unset in the test run itself.
const ROLE: &str = "AGENTC_STATION_TEST_ROLE";
/// Where the child writes what happened: it has no console to print to.
const REPORT: &str = "AGENTC_STATION_TEST_REPORT";
/// The child test's name, as the test harness selects it.
const CHILD: &str = "child_in_a_service_like_session";
/// The secondary logon service is disabled on this host.
const ERROR_SERVICE_DISABLED: u32 = 1058;
/// Read the security descriptor of an object.
const READ_CONTROL: u32 = 0x0002_0000;

/// `text` as NUL-terminated UTF-16.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// This process's environment plus `extra`, as a Windows environment block.
fn environment_block(extra: &[(&str, &str)]) -> Vec<u16> {
    let mut block = Vec::new();
    let own = std::env::vars().filter(|(name, _)| name != ROLE && name != REPORT);
    let extra = extra.iter().map(|(n, v)| (n.to_string(), v.to_string()));
    for (name, value) in own.chain(extra) {
        block.extend(format!("{name}={value}").encode_utf16().chain([0]));
    }
    block.push(0);
    block
}

/// Starts this test program's child test in a new logon session of the
/// same account. `None` when the host has no secondary logon service.
fn start_in_new_session(role: &str, report: &Path) -> Option<PROCESS_INFORMATION> {
    let exe = std::env::current_exe().unwrap();
    let line = format!("\"{}\" {CHILD} --exact --ignored", exe.display());
    let block = environment_block(&[(ROLE, role), (REPORT, &report.display().to_string())]);
    let (user, domain, password) = (wide("agentc-test"), wide("."), wide("unused"));
    let (startup, mut started) = (startup_info(), PROCESS_INFORMATION::default());
    // SAFETY: every string is NUL-terminated and outlives the call; the
    // command line is writable; `started` receives two handles.
    let created = unsafe {
        let (none, here) = (std::ptr::null(), std::ptr::null());
        let (flags, mut line) = (CREATE_UNICODE_ENVIRONMENT, wide(&line));
        #[rustfmt::skip]
        let created = CreateProcessWithLogonW(user.as_ptr(), domain.as_ptr(), password.as_ptr(),
            LOGON_NETCREDENTIALS_ONLY, none, line.as_mut_ptr(), flags, block.as_ptr().cast(),
            here, &startup, &mut started);
        created
    };
    if created == 0 {
        // SAFETY: GetLastError reads thread-local state.
        let code = unsafe { GetLastError() };
        assert_eq!(code, ERROR_SERVICE_DISABLED, "CreateProcessWithLogonW");
        return None;
    }
    Some(started)
}

/// Start-up information that asks for nothing.
fn startup_info() -> STARTUPINFOW {
    STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    }
}

/// Waits for the child and returns its exit code.
fn exit_code(started: &PROCESS_INFORMATION) -> u32 {
    let mut code = 0_u32;
    // SAFETY: both handles came from process creation and are closed once.
    unsafe {
        let ended = WaitForSingleObject(started.hProcess, 180_000);
        assert_eq!(ended, WAIT_OBJECT_0, "the child did not end");
        GetExitCodeProcess(started.hProcess, &mut code);
        CloseHandle(started.hThread);
        CloseHandle(started.hProcess);
    }
    code
}

/// Held while a child runs: Windows starts the secondary logon service on
/// first use and refuses a second request that arrives meanwhile.
static ONE_CHILD: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Runs the child as `role` in a new logon session and fails unless it
/// reports success.
fn in_a_new_session(role: &str) {
    let _one = ONE_CHILD.lock().unwrap_or_else(|held| held.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let report = dir.path().join("report.txt");
    let Some(started) = start_in_new_session(role, &report) else {
        eprintln!("no secondary logon service on this host; {role} was not checked");
        return;
    };
    let code = exit_code(&started);
    let said = std::fs::read_to_string(&report).unwrap_or_default();
    assert!(
        code == 0 && said.starts_with("ok"),
        "{role}: exit {code}: {said}"
    );
    eprintln!("{role}: {said}");
}

#[test]
fn a_station_made_for_this_process_alone_is_taken_over_and_lists_only_the_launch() {
    in_a_new_session("adopts");
}

#[test]
fn a_station_this_process_does_not_run_in_is_refused_and_nothing_is_left() {
    in_a_new_session("not_attached");
}

#[test]
fn a_station_in_a_session_shared_with_another_process_is_refused_and_nothing_is_left() {
    in_a_new_session("not_alone");
}

/// The child's side. Ignored so that it runs only when a test above
/// starts it, with its role, in a new logon session.
#[test]
#[ignore = "started by the tests above in a new logon session"]
fn child_in_a_service_like_session() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    if role == "sleeper" {
        std::thread::sleep(Duration::from_secs(120));
        return;
    }
    let report = PathBuf::from(std::env::var(REPORT).unwrap());
    let outcome = std::panic::catch_unwind(|| play(&role));
    let said = match &outcome {
        Ok(said) => format!("ok: {said}"),
        Err(panic) => format!("failed: {}", panic_text(panic.as_ref())),
    };
    std::fs::write(report, said).unwrap();
    assert!(outcome.is_ok());
}

/// What a caught panic said.
fn panic_text(panic: &(dyn std::any::Any + Send)) -> String {
    let text = panic.downcast_ref::<String>().cloned();
    text.or_else(|| panic.downcast_ref::<&str>().map(|text| text.to_string()))
        .unwrap_or_else(|| "a panic without text".into())
}

/// Leaves the console and waits until this process is alone in its logon
/// session: the console's host process leaves with the console.
fn become_alone() {
    // SAFETY: FreeConsole takes no arguments.
    unsafe { FreeConsole() };
    let session = token::current_logon_session().unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !companions(session).unwrap().is_empty() {
        assert!(Instant::now() < deadline, "the console's host stayed");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The security of the station the Service Control Manager made for
/// `NT SERVICE\agentc-runner` on MiniAir on 2026-11-12, with `me` in the
/// service account's place: the account owns it and may read and rewrite
/// its security, but is not granted every right (`WINSTA_ENUMDESKTOPS`,
/// `WINSTA_WRITEATTRIBUTES`, `WINSTA_ENUMERATE` and `WINSTA_READSCREEN` are
/// missing), and administrators may enumerate it.
fn observed_security(me: &str) -> String {
    format!(
        "O:{me}G:{me}D:(A;;DCLCSWWPDTSDRCWDWO;;;{me})\
         (A;OINPIO;CCDCLCSWDTLOSDRCWDWO;;;{me})(A;;CR;;;BA)(A;OINPIO;CCDTLO;;;BA)"
    )
}

/// Makes the session's unnamed station with the security the Service
/// Control Manager gives a service's station ([`observed_security`]), as
/// something other than the launcher would; with `attach`, this process
/// then runs in it.
fn premade_station(attach: bool) -> HWINSTA {
    let me = token::current_user_sid().unwrap();
    let descriptor = Descriptor::parse(&observed_security(&me)).unwrap();
    let attributes = descriptor.attributes(false);
    // SAFETY: an unnamed station; `attributes` and the descriptor it
    // borrows outlive the call.
    let station = unsafe {
        CreateWindowStationW(std::ptr::null(), CWF_CREATE_ONLY, GENERIC_ALL, &attributes)
    };
    assert!(!station.is_null(), "the new session already had a station");
    if attach {
        // SAFETY: the station handle is live.
        assert_ne!(unsafe { SetProcessWindowStation(station) }, 0);
    }
    station
}

/// One request to `runner` and its answer line.
fn ask(runner: &mut Runner, request: Value) -> Value {
    let parsed = serde_json::from_value::<Request>(request).map_err(anyhow::Error::from);
    answer(parsed.and_then(|request| runner.handle(request)))
}

/// The directories of the test launch below `root`.
struct Case {
    control: PathBuf,
    work: PathBuf,
    log: PathBuf,
}

impl Case {
    fn new(root: &Path) -> Self {
        Self {
            control: root.join("control"),
            work: root.join("work"),
            log: root.join("log.txt"),
        }
    }

    /// The `prepare` request: a less-privileged token with no capability.
    fn prepare(&self) -> Value {
        let profile = format!("agentc.test.{}", uuid::Uuid::new_v4().simple());
        json!({"op": "prepare", "launch": "case",
               "spec": {"profile": profile, "less_privileged": true, "capabilities": [],
                        "limits": {"processes": 8, "process_memory_mb": 256, "job_memory_mb": 512},
                        "control": self.control, "work": self.work}})
    }

    /// A `start` request running `cmd /d /c exit 7`.
    fn start(&self) -> Value {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        let local = std::env::var("LOCALAPPDATA").unwrap();
        json!({"op": "start", "launch": "case",
               "argv": [format!("{root}\\System32\\cmd.exe"), "/d", "/c", "exit 7"],
               "directory": self.work, "output": self.log,
               "environment": {"SystemRoot": root, "LOCALAPPDATA": local}})
    }

    /// Whether either launch directory exists.
    fn left(&self) -> bool {
        self.control.exists() || self.work.exists()
    }
}

/// The access list of `object` as Windows reports it, or why it has none.
fn listed(object: HANDLE) -> String {
    format!("{:?}", allowed(object).map_err(|error| error.to_string()))
}

/// The access list of the desktop `name` (`station\desktop`) in this
/// process's station.
fn desktop_list(name: &str) -> Vec<String> {
    let label = name.rsplit('\\').next().unwrap();
    // SAFETY: the name is NUL-terminated; the handle is closed below.
    let desktop = unsafe { OpenDesktopW(wide(label).as_ptr(), 0, 0, READ_CONTROL) };
    assert!(!desktop.is_null(), "open the desktop {name}");
    let list = allowed(desktop).unwrap();
    // SAFETY: the handle is owned here and closed once.
    unsafe { CloseDesktop(desktop) };
    list
}

/// Plays `role` and says what was seen.
fn play(role: &str) -> String {
    become_alone();
    let dir = tempfile::tempdir().unwrap();
    let case = Case::new(dir.path());
    let mut runner = Runner::new(&dir.path().join("state")).unwrap();
    match role {
        "adopts" => adopts(&mut runner, &case),
        "not_attached" => refused(&mut runner, &case, premade_station(false), "runs in"),
        "not_alone" => not_alone(&mut runner, &case),
        other => panic!("no such role: {other}"),
    }
}

/// The station is taken over; the station and the desktop admit only this
/// account and the launch's package; a launch runs; cleanup withdraws the
/// package and removes the directories.
fn adopts(runner: &mut Runner, case: &Case) -> String {
    let station = premade_station(true);
    let me = token::current_user_sid().unwrap();
    let name = station_name(token::current_logon_session().unwrap());
    assert_ne!(
        granted(&name) & 0x000F_0000,
        0,
        "the account may rewrite it"
    );
    assert_ne!(
        granted(&name),
        EVERY_RIGHT,
        "and is not granted every right"
    );
    let prepared = ask(runner, case.prepare());
    assert_eq!(prepared["ok"], true, "{prepared}");
    let package = prepared["package_sid"].as_str().unwrap().to_owned();
    let both = vec![me.clone(), package];
    assert_eq!(allowed(station).unwrap(), both, "the station's list");
    let desktop = prepared["desktop"].as_str().unwrap();
    assert_eq!(desktop_list(desktop), both, "the desktop's list");
    runs_to_exit_7(runner, case);
    let cleanup = json!({"op": "cleanup", "launch": "case", "remove_directories": true});
    assert_eq!(ask(runner, cleanup)["ok"], true);
    assert_eq!(allowed(station).unwrap(), vec![me], "after cleanup");
    assert!(!case.left(), "cleanup removes the directories");
    format!("station and desktop {desktop} list {both:?}; a launch exited 7")
}

/// A launch in the taken-over station starts and its exit code comes back.
fn runs_to_exit_7(runner: &mut Runner, case: &Case) {
    let started = ask(runner, case.start());
    assert_eq!(started["ok"], true, "{started}");
    let observe = json!({"op": "observe", "launch": "case", "wait_ms": 30_000});
    let seen = ask(runner, observe);
    assert_eq!(
        (&seen["state"], &seen["code"]),
        (&json!("exited"), &json!(7))
    );
}

/// The prepare is refused for a reason containing `why`; the station's
/// access list is as it was, and no launch directory or journal entry is
/// left.
fn refused(runner: &mut Runner, case: &Case, station: HWINSTA, why: &str) -> String {
    let before = listed(station);
    let prepared = ask(runner, case.prepare());
    assert_eq!(prepared["refusal"], "station_not_ours", "{prepared}");
    let error = prepared["error"].as_str().unwrap();
    assert!(error.contains(why), "{error}");
    assert!(shows_the_station(error), "{error}");
    assert_eq!(listed(station), before, "a refused station is not changed");
    assert!(!case.left(), "a refused prepare leaves no directory");
    let journal = ask(runner, json!({"op": "recover"}));
    assert_eq!(journal["launches"], json!([]), "{journal}");
    format!("refused: {error}")
}

/// Whether the refusal `error` says what Windows shows about the premade
/// station: this account, the observed security and the rights that
/// security withholds from the account.
fn shows_the_station(error: &str) -> bool {
    let me = token::current_user_sid().unwrap();
    error.contains(&format!("Windows shows: this process is {me} in"))
        && error.contains(&format!("has security \"{}", observed_security(&me)))
        && error.contains("(not WINSTA_ENUMDESKTOPS, WINSTA_WRITEATTRIBUTES, ")
}

/// With a second process in the logon session the station is refused.
fn not_alone(runner: &mut Runner, case: &Case) -> String {
    let station = premade_station(true);
    let mut sleeper = std::process::Command::new(std::env::current_exe().unwrap())
        .args([CHILD, "--exact", "--ignored"])
        .env(ROLE, "sleeper")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(DETACHED_PROCESS)
        .spawn()
        .unwrap();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        refused(runner, case, station, "share the logon session")
    }));
    let _ = sleeper.kill();
    let _ = sleeper.wait();
    outcome.unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}
