//! What the native broker tests share: running a child of the test program
//! in a **new logon session** of the same account, and preparing launches
//! whose client is this test program again.
//!
//! The launcher makes the logon session's window station, which it refuses
//! to do where another program made one, as in an interactive desktop
//! session. So each test starts this program again in a new logon session
//! (`CreateProcessWithLogonW` with network-only credentials: no real
//! credential is used and nothing is elevated), and that child runs the
//! broker and the launches. Inside each launch this program runs once more,
//! copied into a tool directory only the launches' packages may read.
//!
//! Each test file defines the ignored test [`CHILD`], which calls
//! [`child_main`] with its roles.
#![cfg(windows)]
#![allow(dead_code)]

use agentc_supervisor::host_runtime::windows::launcher::{LaunchSpec, Launcher, Seen};
use agentc_supervisor::host_runtime::windows::process::Command;
use agentc_supervisor::host_runtime::windows::reader::create_directory;
use agentc_supervisor::host_runtime::windows::{sddl, token};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessWithLogonW, GetExitCodeProcess,
    LOGON_NETCREDENTIALS_ONLY, PROCESS_INFORMATION, STARTUPINFOW, WaitForSingleObject,
};

/// Says which part the child plays; unset in the test run itself.
pub const ROLE: &str = "AGENTC_BROKER_TEST_ROLE";
/// Where the child writes what happened: it has no console to print to.
pub const REPORT: &str = "AGENTC_BROKER_TEST_REPORT";
/// The child test's name, as the test harness selects it.
pub const CHILD: &str = "child_in_a_new_logon_session";
/// The secondary logon service is disabled on this host.
const ERROR_SERVICE_DISABLED: u32 = 1058;
/// How long a launch may take to end once it should.
pub const LAUNCH_PATIENCE: Duration = Duration::from_secs(120);

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

/// Starts this program's child test in a new logon session of the same
/// account. `None` when the host has no secondary logon service.
fn start_in_new_session(role: &str, report: &Path) -> Option<PROCESS_INFORMATION> {
    let exe = std::env::current_exe().unwrap();
    let line = format!("\"{}\" {CHILD} --exact --ignored", exe.display());
    let report = report.display().to_string();
    let block = environment_block(&[(ROLE, role), (REPORT, &report)]);
    let (user, domain, password) = (wide("agentc-test"), wide("."), wide("unused"));
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut started = PROCESS_INFORMATION::default();
    // SAFETY: every string is NUL-terminated and outlives the call; the
    // command line is writable; `started` receives the process's and its
    // thread's handles.
    let created = unsafe {
        let (none, mut line) = (std::ptr::null(), wide(&line));
        #[rustfmt::skip]
        let created = CreateProcessWithLogonW(user.as_ptr(), domain.as_ptr(), password.as_ptr(),
            LOGON_NETCREDENTIALS_ONLY, none, line.as_mut_ptr(), CREATE_UNICODE_ENVIRONMENT,
            block.as_ptr().cast(), none, &startup, &mut started);
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

/// Waits for the child and returns its exit code.
fn exit_code(started: &PROCESS_INFORMATION) -> u32 {
    let mut code = 0_u32;
    // SAFETY: both handles came from process creation and are closed once.
    unsafe {
        let ended = WaitForSingleObject(started.hProcess, 300_000);
        assert_eq!(ended, WAIT_OBJECT_0, "the child did not end");
        GetExitCodeProcess(started.hProcess, &mut code);
        CloseHandle(started.hThread);
        CloseHandle(started.hProcess);
    }
    code
}

/// Held while a child runs: Windows refuses a second secondary-logon
/// request that arrives while it starts the service.
static ONE_CHILD: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Runs the child as `role` in a new logon session and fails unless it
/// reports success. Where the host has no secondary logon service it says
/// so on standard error: nothing is checked there.
pub fn in_a_new_session(role: &str) {
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

/// The child's side: runs `play` with the role [`ROLE`] names and writes
/// its outcome to [`REPORT`]. Does nothing when the test was not started
/// as a child.
pub fn child_main(play: impl Fn(&str) -> String + std::panic::RefUnwindSafe) {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
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

/// The trees of the test's launches below `root`.
pub struct Trees {
    root: PathBuf,
}

impl Trees {
    /// A fresh tree below a directory that lives as long as the test.
    pub fn new(dir: &tempfile::TempDir) -> Self {
        Self {
            root: dir.path().to_owned(),
        }
    }

    /// The path of `name` inside the test's tree.
    pub fn at(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// A launch spec with a fresh profile, its directories named `prefix`.
    fn launch(&self, prefix: &str) -> LaunchSpec {
        LaunchSpec {
            profile: format!("agentc.test.{}", uuid::Uuid::new_v4().simple()),
            less_privileged: true,
            capabilities: vec!["registryRead".into()],
            limits: serde_json::from_value(
                json!({"processes": 16, "process_memory_mb": 512, "job_memory_mb": 1024}),
            )
            .unwrap(),
            control: self.at(&format!("{prefix}-control")),
            work: self.at(&format!("{prefix}-work")),
        }
    }
}

/// One prepared launch: its id, its spec and its package.
pub struct Launch {
    pub id: String,
    pub spec: LaunchSpec,
    pub package: String,
}

/// Prepares the launch `id`.
pub fn prepare(launcher: &mut Launcher, trees: &Trees, id: &str) -> Launch {
    let spec = trees.launch(id);
    let prepared = launcher.prepare(id, spec.clone()).unwrap();
    Launch {
        id: id.to_owned(),
        spec,
        package: prepared.package_sid,
    }
}

/// A tool directory that `packages` may read besides the owner, holding
/// this test program as `client.exe`.
pub fn client_tools(trees: &Trees, packages: &[&str]) -> PathBuf {
    let tools = trees.at("tools");
    let owner = token::current_user_sid().unwrap();
    create_directory(&tools, &sddl::shared_source(&owner, packages).unwrap()).unwrap();
    let exe = std::env::current_exe().unwrap();
    std::fs::copy(exe, tools.join("client.exe")).unwrap();
    tools
}

/// Starts the client in `launch` with the role `client`, the variables
/// `extra` besides the ones it always gets. Returns its report's path.
pub fn start_client(
    launcher: &mut Launcher,
    launch: &Launch,
    tools: &Path,
    extra: Vec<(&str, String)>,
) -> PathBuf {
    let work = &launch.spec.work;
    let report = work.join("report.txt");
    let root = std::env::var("SystemRoot").unwrap();
    let mut environment = vec![
        ("PATH", format!("{};{root}\\System32", tools.display())),
        ("SystemRoot", root),
        ("LOCALAPPDATA", work.display().to_string()),
        (ROLE, "client".to_owned()),
        (REPORT, report.display().to_string()),
    ];
    environment.extend(extra);
    let program = tools.join("client.exe").display().to_string();
    let command = Command {
        argv: vec![program, CHILD.into(), "--exact".into(), "--ignored".into()],
        directory: work.clone(),
        environment: environment
            .into_iter()
            .map(|(k, v)| (k.into(), v))
            .collect(),
        output: work.join("client.log"),
    };
    launcher.start(&launch.id, &command, &[]).unwrap();
    report
}

/// Waits for `launch`'s client to end and returns its report's lines; the
/// client writes `ok: ` before them.
pub fn finish(launcher: &mut Launcher, launch: &Launch, report: &Path) -> Vec<String> {
    let seen = launcher.observe(&launch.id, LAUNCH_PATIENCE).unwrap();
    let said = std::fs::read_to_string(report).unwrap_or_default();
    let log = std::fs::read_to_string(launch.spec.work.join("client.log")).unwrap_or_default();
    assert_eq!(seen, Seen::Exited(0), "{said}\n{log}");
    let lines = said
        .strip_prefix("ok: ")
        .unwrap_or_else(|| panic!("{said}\n{log}"));
    lines.lines().map(str::to_owned).collect()
}

/// The outcome the report `lines` give for `label`.
pub fn outcome<'a>(lines: &'a [String], label: &str) -> &'a str {
    let start = format!("{label} = ");
    let found = lines.iter().find_map(|line| line.strip_prefix(&start));
    found.unwrap_or_else(|| panic!("no line for {label} in {lines:?}"))
}

/// Waits up to [`LAUNCH_PATIENCE`] for `ready`, failing with `what`.
pub fn wait_for(ready: impl Fn() -> bool, what: &str) {
    let started = Instant::now();
    while !ready() {
        assert!(started.elapsed() < LAUNCH_PATIENCE, "no {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}
