//! A reviewer launch's candidate running real commands (phase 5, step 5c).
//!
//! The launcher makes the logon session's window station, which it refuses
//! to do where another program made one, as in an interactive desktop
//! session. So the test starts this program again in a **new logon
//! session** of the same account (`CreateProcessWithLogonW` with
//! network-only credentials: no real credential is used and nothing is
//! elevated), and the child does the work. This is a development check of
//! the mechanism, not acceptance evidence.
//!
//! The candidate's shell is the pinned Git Bash's `bash.exe` and
//! `msys-2.0.dll`, copied into a tool directory that only the candidate's
//! package may read: a less-privileged token reads nothing under
//! `Program Files`.
#![cfg(windows)]

use agentc_supervisor::host_runtime::windows::candidate::{
    self, CandidateSpec, LEFT_RUNNING, LIMIT_STATUS, REFUSED_STATUS, Tuning,
};
use agentc_supervisor::host_runtime::windows::launcher::{LaunchSpec, Launcher, Seen};
use agentc_supervisor::host_runtime::windows::reader::create_directory;
use agentc_supervisor::host_runtime::windows::runner::{Request, Runner, answer};
use agentc_supervisor::host_runtime::windows::{sddl, token};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessWithLogonW, GetExitCodeProcess,
    LOGON_NETCREDENTIALS_ONLY, PROCESS_INFORMATION, STARTUPINFOW, WaitForSingleObject,
};

/// Says which part the child plays; unset in the test run itself.
const ROLE: &str = "AGENTC_CANDIDATE_TEST_ROLE";
/// Where the child writes what happened: it has no console to print to.
const REPORT: &str = "AGENTC_CANDIDATE_TEST_REPORT";
/// The child test's name, as the test harness selects it.
const CHILD: &str = "child_in_a_new_logon_session";
/// The secondary logon service is disabled on this host.
const ERROR_SERVICE_DISABLED: u32 = 1058;
/// Where the pinned Git Bash lives.
const GIT_BIN: &str = r"C:\Program Files\Git\usr\bin";
/// A credential the runner's own environment holds and no candidate may see.
const SYNTHETIC: &str = "synthetic-credential-5c";

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
    // command line is writable; `started` receives two handles.
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

/// Why the two candidate tests do not run: see
/// [`git_bash_does_not_start_in_an_appcontainer`].
const NO_SHELL: &str = "needs a shell that starts in an AppContainer; Git Bash does not";

#[test]
#[ignore = "needs a shell that starts in an AppContainer; Git Bash does not"]
fn a_reviewers_candidate_runs_commands_in_its_own_package_and_job() {
    eprintln!("{NO_SHELL}");
    in_a_new_session("commands");
}

#[test]
#[ignore = "needs a shell that starts in an AppContainer; Git Bash does not"]
fn a_candidate_command_past_its_time_limit_is_stopped_and_repeats_are_refused() {
    in_a_new_session("time_limit");
}

/// The blocker found on 2026-11-14: Git Bash's MSYS2 runtime creates its
/// shared objects under `\BaseNamedObjects` with a native call that an
/// AppContainer token is denied, so it ends during start-up in a full and
/// in a less-privileged AppContainer alike. If a later runtime starts, this
/// test fails and the candidate tests below can be tried again.
#[test]
fn git_bash_does_not_start_in_an_appcontainer() {
    if !Path::new(GIT_BIN).join("bash.exe").exists() {
        eprintln!("no Git Bash at {GIT_BIN}; it was not checked");
        return;
    }
    in_a_new_session("probe");
}

/// Starts `bash -c 'echo started'` in a fresh launch, `less` or not, and
/// checks that it ends at start-up with the denied object directory.
fn probe_one(trees: &Trees, launcher: &mut Launcher, less: bool) -> String {
    use agentc_supervisor::host_runtime::windows::process::Command;
    let id = format!("probe-{less}");
    let mut spec = trees.launch(&id);
    spec.less_privileged = less;
    let prepared = launcher.prepare(&id, spec.clone()).unwrap();
    let tools = trees.at(&format!("tools-{less}"));
    let owner = token::current_user_sid().unwrap();
    create_directory(
        &tools,
        &sddl::shared_source(&owner, &[&prepared.package_sid]).unwrap(),
    )
    .unwrap();
    for file in ["bash.exe", "msys-2.0.dll"] {
        std::fs::copy(Path::new(GIT_BIN).join(file), tools.join(file)).unwrap();
    }
    let root = std::env::var("SystemRoot").unwrap();
    let environment = [
        ("SystemRoot", root.clone()),
        ("LOCALAPPDATA", spec.work.display().to_string()),
        ("TMP", spec.work.display().to_string()),
        ("PATH", format!("{root}\\System32")),
    ];
    let log = trees.at(&format!("probe-{less}.log"));
    let command = Command {
        argv: vec![
            tools.join("bash.exe").display().to_string(),
            "--noprofile".into(),
            "--norc".into(),
            "-c".into(),
            "echo started".into(),
        ],
        directory: spec.work.clone(),
        environment: environment
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect(),
        output: log.clone(),
    };
    launcher.start(&id, &command, &[]).unwrap();
    let seen = launcher
        .observe(&id, std::time::Duration::from_secs(30))
        .unwrap();
    launcher.cleanup(&id).unwrap();
    let said = std::fs::read_to_string(&log).unwrap_or_default();
    const DLL_INIT_FAILED: u32 = 0xC000_0142;
    assert_eq!(seen, Seen::Exited(DLL_INIT_FAILED), "less={less}: {said}");
    assert!(
        !said.contains("started"),
        "less={less}: Git Bash started: {said}"
    );
    for part in [
        r"NtCreateDirectoryObject(\BaseNamedObjects\msys-2.0",
        "0xC0000022",
    ] {
        assert!(said.contains(part), "less={less}: {said}");
    }
    format!("less_privileged={less}: {:?}", said.trim())
}

/// Both probes.
fn probe() -> String {
    let dir = tempfile::tempdir().unwrap();
    let trees = Trees {
        root: dir.path().to_owned(),
    };
    let mut launcher = Launcher::new().unwrap();
    let full = probe_one(&trees, &mut launcher, false);
    let less = probe_one(&trees, &mut launcher, true);
    format!("{full} | {less}")
}

/// Names the pinned POSIX shell's file on this host; unset where the owner
/// has not installed it.
const SHELL: &str = "AGENTC_TEST_POSIX_SHELL";
/// busybox-w32 FRP-6354-g5f09fb446 (BusyBox 1.39.0), the 64-bit Unicode
/// build `busybox-w64u-FRP-6354-g5f09fb446.exe`.
const SHELL_SHA256: &str = "36b266923fc5765b42f5f981319f39568498fba60b53ccf26e8fc1e575e2a9a9";

/// The pinned shell's file, or `None` where [`SHELL`] is unset. A file with
/// another hash is never run.
fn pinned_shell() -> Option<PathBuf> {
    use sha2::{Digest, Sha256};
    let path = PathBuf::from(std::env::var_os(SHELL)?);
    let bytes = std::fs::read(&path).unwrap();
    let hash = hex::encode(Sha256::digest(&bytes));
    assert_eq!(hash, SHELL_SHA256, "{} is not the pin", path.display());
    Some(path)
}

/// What the shell probe runs: a pipeline, a subshell, a command
/// substitution and a background job that is waited for. Then what Windows
/// made of the `TMP` the launch set, and whether a value the shell exports
/// reaches a child shell and a program that is not the shell.
const SHELL_PROBE: &str = "echo started; echo abc | tr a-c A-C | cat; (exit 3); echo \"sub $?\"; \
     x=$(echo inner | sed s/inner/captured/); echo \"$x\"; \
     (sleep 1; echo late > bg) & echo early; wait; echo \"waited $?\"; cat bg; \
     case \"$TMP\" in *\\\\AC\\\\Temp) echo rewritten;; *) echo kept;; esac; \
     export TMP=C:/zz; sh -c 'echo \"child=$TMP\"'; cmd /c \"echo cmd=%TMP%\"; exit 7";
/// What it must print, in this order.
const SHELL_PROBE_SAYS: [&str; 10] = [
    "started",
    "ABC",
    "sub 3",
    "captured",
    "early",
    "waited 0",
    "late",
    "rewritten",
    "child=C:/zz",
    "cmd=C:/zz",
];

/// The pinned shell starts in an AppContainer, full and less privileged,
/// and runs pipelines, subshells and background jobs there.
#[test]
fn the_pinned_shell_runs_in_an_appcontainer() {
    if pinned_shell().is_none() {
        eprintln!("{SHELL} is unset; the pinned shell was not checked");
        return;
    }
    in_a_new_session("shell_probe");
}

/// Runs [`SHELL_PROBE`] with the pinned shell, copied as `sh.exe` into a
/// tool directory of a fresh launch, `less` privileged or not.
fn shell_probe_one(trees: &Trees, launcher: &mut Launcher, less: bool) -> String {
    use agentc_supervisor::host_runtime::windows::process::Command;
    let id = format!("shell-{less}");
    let mut spec = trees.launch(&id);
    spec.less_privileged = less;
    let prepared = launcher.prepare(&id, spec.clone()).unwrap();
    let tools = trees.at(&format!("shell-tools-{less}"));
    let owner = token::current_user_sid().unwrap();
    let list = sddl::shared_source(&owner, &[&prepared.package_sid]).unwrap();
    create_directory(&tools, &list).unwrap();
    std::fs::copy(pinned_shell().unwrap(), tools.join("sh.exe")).unwrap();
    let root = std::env::var("SystemRoot").unwrap();
    let work = spec.work.display().to_string();
    let environment = [
        ("SystemRoot", root.clone()),
        ("LOCALAPPDATA", work.clone()),
        ("TMP", work.replace('\\', "/")),
        ("PATH", format!("{};{root}\\System32", tools.display())),
    ];
    let log = trees.at(&format!("shell-{less}.log"));
    let command = Command {
        argv: vec![
            tools.join("sh.exe").display().to_string(),
            "-c".into(),
            SHELL_PROBE.into(),
        ],
        directory: spec.work.clone(),
        environment: environment
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect(),
        output: log.clone(),
    };
    launcher.start(&id, &command, &[]).unwrap();
    let seen = launcher
        .observe(&id, std::time::Duration::from_secs(30))
        .unwrap();
    launcher.cleanup(&id).unwrap();
    let said = std::fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(seen, Seen::Exited(7), "less={less}: {said}");
    let lines: Vec<&str> = said.lines().map(str::trim).collect();
    assert_eq!(lines, SHELL_PROBE_SAYS, "less={less}: {said}");
    format!("less_privileged={less}: {lines:?}")
}

/// Both shell probes.
fn shell_probe() -> String {
    let dir = tempfile::tempdir().unwrap();
    let trees = Trees {
        root: dir.path().to_owned(),
    };
    let mut launcher = Launcher::new().unwrap();
    let full = shell_probe_one(&trees, &mut launcher, false);
    let less = shell_probe_one(&trees, &mut launcher, true);
    format!("{full} | {less}")
}

/// The child's side. Ignored so that it runs only when a test above starts
/// it, with its role, in a new logon session.
#[test]
#[ignore = "started by the tests above in a new logon session"]
fn child_in_a_new_logon_session() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let report = PathBuf::from(std::env::var(REPORT).unwrap());
    let outcome = std::panic::catch_unwind(|| match role.as_str() {
        "commands" => commands(),
        "time_limit" => time_limit(),
        "probe" => probe(),
        "shell_probe" => shell_probe(),
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

/// The trees of one reviewer launch and its candidate below `root`.
struct Trees {
    root: PathBuf,
}

impl Trees {
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

    /// The candidate spec: its shell is the copy in `tools`.
    fn candidate(&self) -> CandidateSpec {
        let system = std::env::var("SystemRoot").unwrap() + r"\System32";
        CandidateSpec {
            launch: self.launch("candidate"),
            source: self.at("source"),
            bash: self.at("tools").join("bash.exe"),
            path: vec![self.at("tools"), system.into()],
        }
    }
}

/// Makes the tool directory, readable by `package` alone besides the
/// owner, and copies the shell into it.
fn tools(trees: &Trees, package: &str) {
    let owner = token::current_user_sid().unwrap();
    let list = sddl::shared_source(&owner, &[package]).unwrap();
    create_directory(&trees.at("tools"), &list).unwrap();
    for file in ["bash.exe", "msys-2.0.dll"] {
        std::fs::copy(Path::new(GIT_BIN).join(file), trees.at("tools").join(file)).unwrap();
    }
}

/// One request to `runner` and its answer line.
fn ask(runner: &mut Runner, request: Value) -> Value {
    let parsed = serde_json::from_value::<Request>(request).map_err(anyhow::Error::from);
    answer(parsed.and_then(|request| runner.handle(request)))
}

/// The output of `call` as text.
fn text(call: &candidate::Call) -> String {
    String::from_utf8_lossy(&call.output).into_owned()
}

/// Commands through the runner's reviewer launch and its candidate: output
/// and status come back; the environment is exact; the candidate writes
/// only its own tree, reads the source and not the reviewer's control
/// directory; a background process is reported and ended; cleanup removes
/// everything.
fn commands() -> String {
    // SAFETY: the child is single-threaded until the runner starts.
    unsafe { std::env::set_var("ANTHROPIC_API_KEY", SYNTHETIC) };
    let dir = tempfile::tempdir().unwrap();
    let trees = Trees {
        root: dir.path().to_owned(),
    };
    let mut runner = Runner::new(&trees.at("state")).unwrap();
    let request = json!({"op": "prepare", "launch": "rev", "spec": trees.launch("reviewer"),
                         "candidate": trees.candidate()});
    let prepared = ask(&mut runner, request);
    assert_eq!(prepared["ok"], true, "{prepared}");
    let package = prepared["candidate_package_sid"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(package, prepared["package_sid"].as_str().unwrap());
    tools(&trees, &package);
    std::fs::write(trees.at("source").join("planted.txt"), "from the source").unwrap();
    std::fs::write(trees.at("reviewer-control").join("secret.txt"), SYNTHETIC).unwrap();
    let candidate = runner.candidate("rev").unwrap();
    let mut seen = vec![run_basics(&candidate, &trees)];
    seen.push(run_grants(&candidate, &trees));
    drop(candidate);
    let cleanup = json!({"op": "cleanup", "launch": "rev", "remove_directories": true});
    let cleaned = ask(&mut runner, cleanup);
    assert_eq!(cleaned["ok"], true, "{cleaned}");
    for tree in [
        "candidate-control",
        "candidate-work",
        "source",
        "reviewer-work",
    ] {
        assert!(!trees.at(tree).exists(), "{tree} was left");
    }
    seen.join("; ")
}

/// Output, status, the environment and a background process.
fn run_basics(candidate: &candidate::Candidate, trees: &Trees) -> String {
    let call = candidate.run("echo out; echo err >&2; exit 3").unwrap();
    assert_eq!(call.status, 3, "{}", text(&call));
    assert!(
        text(&call).contains("out") && text(&call).contains("err"),
        "{}",
        text(&call)
    );
    assert_eq!((call.truncated, call.note.as_deref()), (false, None));
    // Git Bash shows Windows paths in its own form, so only the tree is
    // compared; the credential the runner holds must be empty.
    let call = candidate
        .run("echo \"[$HOME][$ANTHROPIC_API_KEY]\"; env")
        .unwrap();
    let shown = text(&call);
    assert!(
        shown.contains("candidate-work/home]") && shown.contains("[]"),
        "{shown}"
    );
    assert!(
        !shown.contains(SYNTHETIC),
        "the credential reached the candidate"
    );
    assert!(trees.at("candidate-work").exists());
    let call = candidate
        .run("bash --noprofile --norc -c 'while :; do :; done' & exit 0")
        .unwrap();
    assert_eq!(call.status, 0, "{}", text(&call));
    assert_eq!(call.note.as_deref(), Some(LEFT_RUNNING), "{}", text(&call));
    format!("environment {:?}", text(&call))
}

/// What the candidate may and may not touch.
fn run_grants(candidate: &candidate::Candidate, trees: &Trees) -> String {
    let wrote = candidate
        .run("echo x > \"$TEMP/f\" && cat \"$TEMP/f\"")
        .unwrap();
    assert_eq!(wrote.status, 0, "{}", text(&wrote));
    let read = candidate.run("cat planted.txt").unwrap();
    assert_eq!(
        (read.status, text(&read).trim()),
        (0, "from the source"),
        "{}",
        text(&read)
    );
    let into_source = candidate.run("echo x > planted-by-candidate.txt").unwrap();
    assert_ne!(into_source.status, 0, "the candidate wrote the source");
    assert!(!trees.at("source").join("planted-by-candidate.txt").exists());
    let secret = trees.at("reviewer-control").join("secret.txt");
    let path = secret.display().to_string().replace('\\', "/");
    let stolen = candidate.run(&format!("cat '{path}'")).unwrap();
    assert_ne!(
        stolen.status, 0,
        "the candidate read the reviewer's control directory"
    );
    assert!(!text(&stolen).contains(SYNTHETIC));
    format!("source write refused: {}", text(&into_source).trim())
}

/// A command past a short time limit is stopped with the limit's status;
/// after two in a row the candidate refuses; the candidate retires.
fn time_limit() -> String {
    let dir = tempfile::tempdir().unwrap();
    let trees = Trees {
        root: dir.path().to_owned(),
    };
    let mut launcher = Launcher::new().unwrap();
    let reviewer = launcher.prepare("rev", trees.launch("reviewer")).unwrap();
    let tuning = Tuning {
        call_seconds: 2,
        stuck_calls: 2,
    };
    let spec = trees.candidate();
    let prepared = candidate::prepare(
        &mut launcher,
        "rev",
        spec.clone(),
        &reviewer.package_sid,
        tuning,
    );
    let candidate = prepared.unwrap();
    tools(&trees, candidate.package_sid());
    for _ in 0..2 {
        let call = candidate.run("while :; do :; done").unwrap();
        assert_eq!(call.status, LIMIT_STATUS);
        assert!(call.note.unwrap().contains("time limit of 2 seconds"));
    }
    let refused = candidate.run("echo never").unwrap();
    assert_eq!(refused.status, REFUSED_STATUS);
    assert!(!text(&refused).contains("never"));
    candidate.retire(&mut launcher).unwrap();
    launcher.cleanup("rev").unwrap();
    candidate::remove_trees(&spec);
    "two stopped at 2 s, the third refused".into()
}
