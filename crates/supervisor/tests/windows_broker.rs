//! A launch's broker pipe with real AppContainer launches (phase 6, step
//! 6a).
//!
//! The launcher makes the logon session's window station, which it refuses
//! to do where another program made one, as in an interactive desktop
//! session. So each test starts this program again in a **new logon
//! session** of the same account (`CreateProcessWithLogonW` with
//! network-only credentials: no real credential is used and nothing is
//! elevated), and that child runs the broker and the launches. Inside each
//! launch this program runs once more, copied into a tool directory only the
//! launch's package may read, as the broker's client. This is a development
//! check of the mechanism, not acceptance evidence.
//!
//! The refusals that need no launch (a token without the package, the
//! remote form of the pipe's path) and the generation of a launch opened
//! again are the `broker` module's own tests.
#![cfg(windows)]

use agentc_supervisor::host_runtime::windows::broker::{
    Broker, Grant, Halt, Handler, Role, pipe_name,
};
use agentc_supervisor::host_runtime::windows::launcher::{LaunchSpec, Launcher, Seen};
use agentc_supervisor::host_runtime::windows::pipe::{Line, Received, Session};
use agentc_supervisor::host_runtime::windows::process::Command;
use agentc_supervisor::host_runtime::windows::reader::create_directory;
use agentc_supervisor::host_runtime::windows::{sddl, token};
use agentc_supervisor::service::pipe_policy::{CLIENT_ACCESS, pipe_path};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessWithLogonW, GetExitCodeProcess,
    LOGON_NETCREDENTIALS_ONLY, PROCESS_INFORMATION, STARTUPINFOW, WaitForSingleObject,
};

/// Says which part the child plays; unset in the test run itself.
const ROLE: &str = "AGENTC_BROKER_TEST_ROLE";
/// Where the child writes what happened: it has no console to print to.
const REPORT: &str = "AGENTC_BROKER_TEST_REPORT";
/// The pipes the client in a launch visits, as `label=name;label=name`.
const PIPES: &str = "AGENTC_BROKER_TEST_PIPES";
/// Set when the client keeps its session open until the broker ends it.
const HOLD: &str = "AGENTC_BROKER_TEST_HOLD";
/// The child test's name, as the test harness selects it.
const CHILD: &str = "child_in_a_new_logon_session";
/// The secondary logon service is disabled on this host.
const ERROR_SERVICE_DISABLED: u32 = 1058;
/// The request the client sends: it claims to be another launch, in
/// another role and generation.
const FORGED: &str = r#"{"launch":"forged-launch","role":"implementer","generation":999}"#;
/// The broker service the tests run.
const SERVICE: &str = "test";
/// How long a launch may take to end once it should.
const LAUNCH_PATIENCE: Duration = Duration::from_secs(120);

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

/// A launch's client sends a request that claims another launch, role and
/// generation; the broker's handler answers with the grant its registry
/// holds for the client's package, and the claim changes nothing.
#[test]
fn a_launch_is_served_its_own_grant_whatever_its_request_claims() {
    in_a_new_session("served");
}

/// A launch with another package cannot open the first launch's broker
/// pipe, while it can open its own.
#[test]
fn another_launch_cannot_open_a_launchs_broker_pipe() {
    in_a_new_session("foreign");
}

/// Closing a launch ends its session that is still going, at once, and
/// its pipe's name is gone afterwards.
#[test]
fn closing_a_launch_ends_its_ongoing_session_and_removes_its_pipe() {
    in_a_new_session("closed");
}

/// The child's side. Ignored so that it runs only when a test above starts
/// it, with its role, in a new logon session or inside a launch.
#[test]
#[ignore = "started by the tests above in a new logon session"]
fn child_in_a_new_logon_session() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let report = PathBuf::from(std::env::var(REPORT).unwrap());
    let outcome = std::panic::catch_unwind(|| match role.as_str() {
        "served" => served(),
        "foreign" => foreign(),
        "closed" => closed(),
        "client" => client(),
        other => panic!("no such role: {other}"),
    });
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

/// The broker's handler in these tests: it answers one request line with
/// the grant the broker hands it and the request it reads, then holds the
/// session until the client leaves or the pipe closes.
#[derive(Default)]
struct Echo {
    /// How many requests it answered.
    answered: AtomicUsize,
}

impl Handler for Echo {
    /// Answers with the grant and the claim, then holds the session.
    fn serve(&self, grant: &Grant, session: &mut Session<'_>, halt: &Halt) -> anyhow::Result<()> {
        let Line::Request(claim) = session.read_line(Duration::from_secs(10))? else {
            anyhow::bail!("an admitted client sent no whole request");
        };
        let answer = json!({"launch": grant.launch(), "role": grant.role(),
            "generation": grant.generation(), "package": grant.package(),
            "claimed": String::from_utf8_lossy(&claim)});
        session.write_all(format!("{answer}\n").as_bytes())?;
        self.answered.fetch_add(1, Ordering::SeqCst);
        while !halt.asked() {
            if session.read(1024, Duration::from_millis(500))? == Received::Closed {
                break;
            }
        }
        Ok(())
    }
}

/// The client inside a launch: visits every pipe [`PIPES`] names and
/// reports one line per pipe.
fn client() -> String {
    let pipes = std::env::var(PIPES).unwrap();
    let hold = std::env::var_os(HOLD).is_some();
    let visit = |pair: &str| {
        let (label, name) = pair.split_once('=').unwrap();
        format!("{label} = {}", visit(name, hold))
    };
    pipes.split(';').map(visit).collect::<Vec<_>>().join("\n")
}

/// Opens the pipe `name` with a client's rights, sends [`FORGED`] and reads
/// the answer; with `hold`, then reads on until the broker ends the session.
fn visit(name: &str, hold: bool) -> String {
    use std::os::windows::fs::OpenOptionsExt;
    let mut options = std::fs::OpenOptions::new();
    options.access_mode(CLIENT_ACCESS);
    let mut file = match options.open(pipe_path(name).unwrap()) {
        Ok(file) => file,
        Err(error) => return format!("error {}", error.raw_os_error().unwrap_or(-1)),
    };
    file.write_all(format!("{FORGED}\n").as_bytes()).unwrap();
    let mut reader = BufReader::new(file);
    let mut answer = String::new();
    reader.read_line(&mut answer).unwrap();
    if !hold {
        return format!("answer {}", answer.trim());
    }
    let mut rest = Vec::new();
    let ended = match reader.read_to_end(&mut rest) {
        Ok(_) => "closed".to_owned(),
        Err(error) => format!("broken {}", error.raw_os_error().unwrap_or(-1)),
    };
    format!("answer {} then {ended}", answer.trim())
}

/// The trees of the test's launches below `root`.
struct Trees {
    root: PathBuf,
}

impl Trees {
    /// A fresh tree below a directory that lives as long as the test.
    fn new(dir: &tempfile::TempDir) -> Self {
        Self {
            root: dir.path().to_owned(),
        }
    }

    /// The path of `name` inside the test's tree.
    fn at(&self, name: &str) -> PathBuf {
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
struct Launch {
    id: String,
    spec: LaunchSpec,
    package: String,
}

/// Prepares the launch `id`.
fn prepare(launcher: &mut Launcher, trees: &Trees, id: &str) -> Launch {
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
fn client_tools(trees: &Trees, packages: &[&str]) -> PathBuf {
    let tools = trees.at("tools");
    let owner = token::current_user_sid().unwrap();
    create_directory(&tools, &sddl::shared_source(&owner, packages).unwrap()).unwrap();
    let exe = std::env::current_exe().unwrap();
    std::fs::copy(exe, tools.join("client.exe")).unwrap();
    tools
}

/// Starts the client in `launch`, visiting `pipes` (label and pipe name);
/// with `hold` it keeps its first session open. Returns its report's path.
fn start_client(
    launcher: &mut Launcher,
    launch: &Launch,
    tools: &Path,
    (pipes, hold): (&[(&str, String)], bool),
) -> PathBuf {
    let work = &launch.spec.work;
    let report = work.join("report.txt");
    let root = std::env::var("SystemRoot").unwrap();
    let listed: Vec<String> = pipes.iter().map(|(l, n)| format!("{l}={n}")).collect();
    let mut environment = vec![
        ("PATH", format!("{};{root}\\System32", tools.display())),
        ("SystemRoot", root),
        ("LOCALAPPDATA", work.display().to_string()),
        (ROLE, "client".to_owned()),
        (REPORT, report.display().to_string()),
        (PIPES, listed.join(";")),
    ];
    if hold {
        environment.push((HOLD, "1".to_owned()));
    }
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
fn finish(launcher: &mut Launcher, launch: &Launch, report: &Path) -> Vec<String> {
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
fn outcome<'a>(lines: &'a [String], label: &str) -> &'a str {
    let start = format!("{label} = ");
    let found = lines.iter().find_map(|line| line.strip_prefix(&start));
    found.unwrap_or_else(|| panic!("no line for {label} in {lines:?}"))
}

/// The answer an outcome carries, as JSON.
fn answer_in(outcome: &str) -> Value {
    let text = outcome
        .strip_prefix("answer ")
        .unwrap_or_else(|| panic!("{outcome}"));
    let text = text.split(" then ").next().unwrap();
    serde_json::from_str(text).unwrap_or_else(|error| panic!("{error}: {outcome}"))
}

/// Checks that `answer` is `launch`'s registry grant as a reviewer of
/// `generation`, and that the handler read the forged claim.
fn assert_grant(answer: &Value, launch: &Launch, generation: u64) {
    assert_eq!(answer["launch"], launch.id.as_str(), "{answer}");
    assert_eq!(answer["role"], "reviewer", "{answer}");
    assert_eq!(answer["generation"], generation, "{answer}");
    assert_eq!(answer["package"], launch.package.as_str(), "{answer}");
    assert_eq!(answer["claimed"], FORGED, "{answer}");
}

/// A broker running as this account whose sessions go to `echo`.
fn broker(echo: &Arc<Echo>) -> Broker {
    Broker::new(SERVICE, &token::current_user_sid().unwrap(), echo.clone()).unwrap()
}

/// Opens `launch` in `broker` as a reviewer asking for `generation`.
fn open(broker: &mut Broker, launch: &Launch, generation: u64) -> Arc<Grant> {
    let me = token::current_user_sid().unwrap();
    let grant = Grant::new(&launch.id, Role::Reviewer, &me, &launch.package, generation);
    broker.open(grant.unwrap()).unwrap()
}

/// Ends `launches` and their pipes.
fn clean(launcher: &mut Launcher, broker: &mut Broker, launches: &[&Launch]) {
    for launch in launches {
        broker.close(&launch.id).unwrap();
        launcher.cleanup(&launch.id).unwrap();
    }
}

/// Test (a): the forged request reaches the handler and the answer is the
/// registry's grant.
fn served() -> String {
    let dir = tempfile::tempdir().unwrap();
    let (trees, mut launcher, echo) = (Trees::new(&dir), Launcher::new().unwrap(), Arc::default());
    let mut broker = broker(&echo);
    let own = prepare(&mut launcher, &trees, "brk-served");
    let granted = open(&mut broker, &own, 7);
    let tools = client_tools(&trees, &[&own.package]);
    let pipes = [("own", pipe_name(SERVICE, &own.id))];
    let report = start_client(&mut launcher, &own, &tools, (&pipes, false));
    let lines = finish(&mut launcher, &own, &report);
    assert_grant(
        &answer_in(outcome(&lines, "own")),
        &own,
        granted.generation(),
    );
    assert_eq!(echo.answered.load(Ordering::SeqCst), 1);
    clean(&mut launcher, &mut broker, &[&own]);
    lines.join("; ")
}

/// Test (b): another launch's package is denied the pipe by its access
/// list, while that launch's own pipe serves it.
fn foreign() -> String {
    let dir = tempfile::tempdir().unwrap();
    let (trees, mut launcher, echo) = (Trees::new(&dir), Launcher::new().unwrap(), Arc::default());
    let mut broker = broker(&echo);
    let first = prepare(&mut launcher, &trees, "brk-first");
    let other = prepare(&mut launcher, &trees, "brk-other");
    open(&mut broker, &first, 1);
    open(&mut broker, &other, 1);
    let tools = client_tools(&trees, &[&first.package, &other.package]);
    let pipes = [
        ("own", pipe_name(SERVICE, &other.id)),
        ("foreign", pipe_name(SERVICE, &first.id)),
    ];
    let report = start_client(&mut launcher, &other, &tools, (&pipes, false));
    let lines = finish(&mut launcher, &other, &report);
    assert_grant(&answer_in(outcome(&lines, "own")), &other, 1);
    assert_eq!(outcome(&lines, "foreign"), "error 5", "{lines:?}");
    assert_eq!(
        echo.answered.load(Ordering::SeqCst),
        1,
        "the first launch served nobody"
    );
    clean(&mut launcher, &mut broker, &[&first, &other]);
    lines.join("; ")
}

/// Test (e): the client holds its session; closing the launch ends it at
/// once and removes the pipe's name.
fn closed() -> String {
    let dir = tempfile::tempdir().unwrap();
    let (trees, mut launcher, echo) = (Trees::new(&dir), Launcher::new().unwrap(), Arc::default());
    let mut broker = broker(&echo);
    let own = prepare(&mut launcher, &trees, "brk-closed");
    open(&mut broker, &own, 3);
    let tools = client_tools(&trees, &[&own.package]);
    let name = pipe_name(SERVICE, &own.id);
    let pipes = [("own", name.clone())];
    let report = start_client(&mut launcher, &own, &tools, (&pipes, true));
    wait_for(
        || echo.answered.load(Ordering::SeqCst) == 1,
        "the client's answer",
    );
    let started = Instant::now();
    assert!(broker.close(&own.id).unwrap());
    let took = started.elapsed();
    // Below the serving threads' 500 ms tick, so the close must come from
    // the halt event waking them, not from their next poll.
    assert!(took < Duration::from_millis(250), "closing took {took:?}");
    let gone = std::fs::File::open(pipe_path(&name).unwrap()).unwrap_err();
    assert_eq!(gone.raw_os_error(), Some(2), "{gone}");
    let lines = finish(&mut launcher, &own, &report);
    let said = outcome(&lines, "own");
    assert_grant(&answer_in(said), &own, 3);
    assert!(
        said.ends_with(" then closed") || said.contains(" then broken"),
        "{said}"
    );
    launcher.cleanup(&own.id).unwrap();
    format!("closed in {took:?}; {}", lines.join("; "))
}

/// Waits up to [`LAUNCH_PATIENCE`] for `ready`, failing with `what`.
fn wait_for(ready: impl Fn() -> bool, what: &str) {
    let started = Instant::now();
    while !ready() {
        assert!(started.elapsed() < LAUNCH_PATIENCE, "no {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}
