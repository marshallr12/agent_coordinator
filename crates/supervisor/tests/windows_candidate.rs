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
//! The candidate's shell is the pinned busybox-w32 (see [`SHELL`]), copied
//! as `sh.exe` into a tool directory that only the candidate's package may
//! read: a less-privileged token reads nothing under `Program Files`. Git
//! Bash, the shell first tried, does not start in an AppContainer, which
//! `git_bash_does_not_start_in_an_appcontainer` keeps asserting.
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

/// Runs the child as `role` where the pinned shell is installed, and says
/// so where it is not: nothing was checked there.
fn with_the_pinned_shell(role: &str) {
    if pinned_shell().is_none() {
        eprintln!("{SHELL} is unset; {role} was not checked");
        return;
    }
    in_a_new_session(role);
}

#[test]
fn a_reviewers_candidate_runs_commands_in_its_own_package_and_job() {
    with_the_pinned_shell("commands");
}

#[test]
fn a_candidate_command_past_its_time_limit_is_stopped_and_repeats_are_refused() {
    with_the_pinned_shell("time_limit");
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

/// The pinned shell's bytes, or `None` where [`SHELL`] is unset. A file with
/// another hash is never run: callers write these bytes, not the file again.
fn pinned_shell() -> Option<Vec<u8>> {
    use sha2::{Digest, Sha256};
    let path = PathBuf::from(std::env::var_os(SHELL)?);
    let bytes = std::fs::read(&path).unwrap();
    let hash = hex::encode(Sha256::digest(&bytes));
    assert_eq!(hash, SHELL_SHA256, "{} is not the pin", path.display());
    Some(bytes)
}

/// What the shell probe runs: a pipeline, a subshell, a command
/// substitution and a background job that is waited for. Then what Windows
/// made of the variables the launch set (`LOCALAPPDATA` and `TMP` given,
/// `TEMP` not given, `PROBE_KEPT` as a variable Windows has no use for), and
/// whether a value the shell exports reaches a child shell and a program
/// that is not the shell.
const SHELL_PROBE: &str = "echo started; echo abc | tr a-c A-C | cat; (exit 3); echo \"sub $?\"; \
     x=$(echo inner | sed s/inner/captured/); echo \"$x\"; \
     (sleep 1; echo late > bg) & echo early; wait; echo \"waited $?\"; cat bg; \
     case \"$LOCALAPPDATA\" in *\\\\Packages\\\\*\\\\AC) echo local-replaced;; *) echo local-kept;; esac; \
     case \"$TMP\" in *\\\\AC\\\\Temp) echo tmp-replaced;; *) echo tmp-kept;; esac; \
     case \"$TEMP\" in *\\\\AC\\\\Temp) echo temp-added;; *) echo \"temp=$TEMP\";; esac; \
     [ -d \"$TMP\" ] && echo temp-directory || echo no-temp-directory; \
     echo \"other=$PROBE_KEPT\"; \
     export TMP=C:/zz; sh -c 'echo \"child=$TMP\"'; cmd /c \"echo cmd=%TMP%\"; exit 7";
/// What it must print, in this order.
const SHELL_PROBE_SAYS: [&str; 14] = [
    "started",
    "ABC",
    "sub 3",
    "captured",
    "early",
    "waited 0",
    "late",
    "local-replaced",
    "tmp-replaced",
    "temp-added",
    "no-temp-directory",
    "other=as given",
    "child=C:/zz",
    "cmd=C:/zz",
];

/// The pinned shell starts in an AppContainer, full and less privileged,
/// and runs pipelines, subshells and background jobs there.
#[test]
fn the_pinned_shell_runs_in_an_appcontainer() {
    with_the_pinned_shell("shell_probe");
}

/// A tool directory only `package` may read besides the owner, holding the
/// pinned shell as `sh.exe`.
fn shell_tools(tools: &Path, package: &str) {
    let owner = token::current_user_sid().unwrap();
    let list = sddl::shared_source(&owner, &[package]).unwrap();
    create_directory(tools, &list).unwrap();
    std::fs::write(tools.join("sh.exe"), pinned_shell().unwrap()).unwrap();
}

/// The probe's environment: `LOCALAPPDATA` and `TMP` in `work`, no `TEMP`.
fn probe_environment(tools: &Path, work: &Path) -> std::collections::BTreeMap<String, String> {
    let root = std::env::var("SystemRoot").unwrap();
    let work = work.display().to_string();
    let pairs = [
        ("PATH", format!("{};{root}\\System32", tools.display())),
        ("SystemRoot", root),
        ("LOCALAPPDATA", work.clone()),
        ("TMP", work.replace('\\', "/")),
        ("PROBE_KEPT", "as given".to_owned()),
    ];
    pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}

/// Runs [`SHELL_PROBE`] with the pinned shell in a fresh launch, `less`
/// privileged or not.
fn shell_probe_one(trees: &Trees, launcher: &mut Launcher, less: bool) -> String {
    use agentc_supervisor::host_runtime::windows::process::Command;
    let id = format!("shell-{less}");
    let mut spec = trees.launch(&id);
    spec.less_privileged = less;
    let prepared = launcher.prepare(&id, spec.clone()).unwrap();
    let tools = trees.at(&format!("shell-tools-{less}"));
    shell_tools(&tools, &prepared.package_sid);
    let shell = tools.join("sh.exe").display().to_string();
    let log = trees.at(&format!("shell-{less}.log"));
    let command = Command {
        argv: vec![shell, "-c".into(), SHELL_PROBE.into()],
        environment: probe_environment(&tools, &spec.work),
        directory: spec.work,
        output: log.clone(),
    };
    launcher.start(&id, &command, &[]).unwrap();
    let patience = std::time::Duration::from_secs(30);
    let seen = launcher.observe(&id, patience).unwrap();
    launcher.cleanup(&id).unwrap();
    probe_result(seen, &log, less)
}

/// Checks the probe's status and lines and returns them.
fn probe_result(seen: Seen, log: &Path, less: bool) -> String {
    let said = std::fs::read_to_string(log).unwrap_or_default();
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
        "shim" => shim(),
        "fixture" => fixture(),
        "reach" => reach(),
        "harness_probe" => harness_probe(),
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

    /// The candidate spec of the launch `other`, with trees of its own.
    fn other_candidate(&self) -> CandidateSpec {
        let system = std::env::var("SystemRoot").unwrap() + r"\System32";
        let tools = self.at("other-candidate-tools");
        CandidateSpec {
            launch: self.launch("other-candidate"),
            source: self.at("other-source"),
            shell: tools.join("sh.exe"),
            shim: self.at("other-tools").join("agentc-supervisor.exe"),
            reviewer_temp: self.at("other-work").join("tmp"),
            path: vec![tools, system.into()],
        }
    }

    /// The candidate spec: its shell is the copy in `tools`.
    fn candidate(&self) -> CandidateSpec {
        let system = std::env::var("SystemRoot").unwrap() + r"\System32";
        CandidateSpec {
            launch: self.launch("candidate"),
            source: self.at("source"),
            shell: self.at("tools").join("sh.exe"),
            shim: self.at("reviewer-tools").join("agentc-supervisor.exe"),
            reviewer_temp: self.at("reviewer-work").join("tmp"),
            path: vec![self.at("tools"), system.into()],
        }
    }
}

/// Makes the tool directory, readable by `package` alone besides the
/// owner, and copies the shell into it.
fn tools(trees: &Trees, package: &str) {
    shell_tools(&trees.at("tools"), package);
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
    // The variables are the candidate's own, the three Windows replaced
    // among them; the credential the runner holds must be empty.
    let call = candidate
        .run("echo \"[$HOME][$TEMP][$TMP][$LOCALAPPDATA][$ANTHROPIC_API_KEY]\"; env")
        .unwrap();
    let shown = text(&call);
    let work = trees.at("candidate-work").display().to_string();
    let own = format!(r"[{work}\home][{work}\tmp][{work}\tmp][{work}\local][]");
    assert!(shown.contains(&own), "{shown}");
    assert!(
        !shown.contains(SYNTHETIC),
        "the credential reached the candidate"
    );
    assert!(trees.at("candidate-work").exists());
    let call = candidate
        .run("sh -c 'while :; do :; done' & exit 0")
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

/// Step 5d: harness-shaped commands from a real reviewer launch through the
/// prefix script and the shim into the candidate, the refusals that come
/// before anything runs, a call that finds every command slot taken, and
/// the packages the pipe's access list keeps out.
#[test]
fn a_harness_shaped_command_runs_through_the_shim_and_others_are_refused() {
    with_the_pinned_shell("shim");
}

/// `path` as the pinned shell writes it.
fn slashed(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// The reviewer's tool directory, readable by its `package` alone besides
/// the owner: the pinned shell, the supervisor program as its shim, and
/// one file per harness argument the reviewer's script passes on.
fn reviewer_tools(trees: &Trees, package: &str) {
    use agentc_supervisor::candidate::form::{git_bash_path, recorded};
    let tools = trees.at("reviewer-tools");
    shell_tools(&tools, package);
    let program = env!("CARGO_BIN_EXE_agentc-supervisor");
    std::fs::copy(program, tools.join("agentc-supervisor.exe")).unwrap();
    let temp = trees.at("reviewer-work").join("tmp");
    std::fs::create_dir(&temp).unwrap();
    let temp = temp.display().to_string();
    let inside = format!("{}/claude-1a2b-cwd", git_bash_path(&temp).unwrap());
    let argument = |command: &str, cwd: &str| recorded("/c/none/snapshot.sh", &temp, command, cwd);
    let files = [
        (
            "good",
            argument(
                "'echo out; echo err >&2; echo x > \"$TEMP/good-ran\"; exit 3'",
                &inside,
            ),
        ),
        ("cd", argument("'cd \"$TEMP\" && echo moved'", &inside)),
        ("slow", argument("'sleep 8'", &inside)),
        (
            "outside",
            argument(
                "'echo x > \"$TEMP/outside-ran\"'",
                "/c/Windows/claude-1a2b-cwd",
            ),
        ),
        ("hook", "echo x > hook-ran".to_owned()),
    ];
    for (name, text) in files {
        std::fs::write(tools.join(name), text).unwrap();
    }
}

/// What the reviewer's shell runs: each argument through the prefix script
/// as the harness would pass it, and a fifth call while four slow ones hold
/// every command slot.
fn reviewer_script(trees: &Trees, pipe: &str) -> String {
    let tools = slashed(&trees.at("reviewer-tools"));
    let cwd = slashed(
        &trees
            .at("reviewer-work")
            .join("tmp")
            .join("claude-1a2b-cwd"),
    );
    let prefix = slashed(&trees.at("reviewer-control").join("candidate-shell"));
    let temp = trees.at("reviewer-work").join("tmp").display().to_string();
    let source = trees.at("source").display().to_string();
    format!(
        "p='{prefix}'; t='{tools}'; \
         \"$p\" \"$(cat $t/good)\"; echo \"good=$?\"; cat '{cwd}'; \
         \"$p\" \"$(cat $t/cd)\"; echo \"cd=$?\"; cat '{cwd}'; \
         \"$p\" \"$(cat $t/hook)\"; echo \"hook=$?\"; \
         \"$p\" \"$(cat $t/outside)\"; echo \"outside=$?\"; \
         \"$p\" \"$(cat $t/good)\" extra; echo \"two=$?\"; \
         for i in 1 2 3 4; do \"$p\" \"$(cat $t/slow)\" & done; sleep 3; \
         $t/agentc-supervisor.exe windows-candidate-shell --pipe {pipe} --temp '{temp}' \
         --clone '{source}' --wait-seconds 1 -- \"$(cat $t/good)\"; echo \"busy=$?\"; wait; \
         echo done"
    )
}

/// The environment of a process the test starts in a launch whose working
/// directory is `work`. `LOCALAPPDATA` must be there: Windows refuses to
/// start a process in an AppContainer without it (error 203, seen
/// 2026-11-17), since it derives the package's folders from it.
fn launch_environment(path: &Path, work: &Path) -> serde_json::Map<String, Value> {
    let root = std::env::var("SystemRoot").unwrap();
    let pairs = [
        ("PATH", format!("{};{root}\\System32", path.display())),
        ("SystemRoot", root),
        ("LOCALAPPDATA", work.display().to_string()),
    ];
    pairs
        .into_iter()
        .map(|(k, v)| (k.to_owned(), json!(v)))
        .collect()
}

/// Starts `argv` as the root of `launch`, waits for it and returns its exit
/// code and what it wrote.
fn run_in_launch(runner: &mut Runner, trees: &Trees, launch: &str, argv: Value) -> (u64, String) {
    let log = trees.at(&format!("{launch}.log"));
    let (tools, work) = (format!("{launch}-tools"), format!("{launch}-work"));
    let environment = launch_environment(&trees.at(&tools), &trees.at(&work));
    let start = json!({"op": "start", "launch": launch, "argv": argv,
        "directory": trees.at(&work), "environment": environment, "output": log});
    let started = ask(runner, start);
    assert_eq!(started["ok"], true, "{started}");
    let seen = ask(
        runner,
        json!({"op": "observe", "launch": launch, "wait_ms": 120_000}),
    );
    let said = std::fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(seen["state"], "exited", "{seen}: {said}");
    (seen["code"].as_u64().unwrap(), said)
}

/// Checks what the reviewer's script printed, line by line where the order
/// is fixed.
fn check_reviewer(said: &str, trees: &Trees) {
    use agentc_supervisor::candidate::form::{CWD_OUTSIDE, NOT_THE_FORM, git_bash_path};
    use agentc_supervisor::candidate::shim::NOT_ONE_ARGUMENT;
    let source = git_bash_path(&trees.at("source").display().to_string()).unwrap();
    let refused = |why: &str| format!("candidate-shell: refused: {why}");
    let wanted = [
        "out".to_owned(),
        "err".to_owned(),
        "good=3".to_owned(),
        source.clone(),
        "moved".to_owned(),
        "cd=0".to_owned(),
        source,
        refused(NOT_THE_FORM),
        "hook=126".to_owned(),
        refused(CWD_OUTSIDE),
        "outside=126".to_owned(),
        refused(NOT_ONE_ARGUMENT),
        "two=126".to_owned(),
    ];
    let lines: Vec<&str> = said.lines().map(str::trim).collect();
    assert_eq!(lines[..wanted.len()], wanted, "{said}");
    let rest = lines[wanted.len()..].join("\n");
    assert!(
        rest.contains("every command slot of this review was still taken"),
        "{said}"
    );
    assert!(
        rest.contains("busy=126") && rest.ends_with("done"),
        "{said}"
    );
}

/// The shim's arguments for a caller that is not the reviewer: a call for
/// `pipe` that the shim itself accepts, so only the pipe can refuse it.
fn stranger_arguments(pipe: &str) -> Vec<String> {
    use agentc_supervisor::candidate::form::recorded;
    let temp = r"C:\none\tmp";
    let cwd = "/c/none/tmp/claude-1a2b-cwd";
    let argument = recorded("/c/none/s.sh", temp, "'echo stranger-ran'", cwd);
    let fixed = ["windows-candidate-shell", "--pipe", pipe, "--temp", temp];
    let rest = ["--clone", r"C:\none\src", "--wait-seconds", "1", "--"];
    let words = fixed.iter().chain(&rest).map(|word| word.to_string());
    words.chain([argument]).collect()
}

/// Asserts that a stranger's shim ended refused because Windows would not
/// let it open the pipe, and that its command never ran.
fn assert_kept_out(who: &str, status: u64, said: &str) {
    assert_eq!(status, 126, "{who}: {said}");
    let denied = said.contains("open the candidate pipe") && said.contains("os error 5");
    assert!(denied, "{who} was not refused by the access list: {said}");
    assert!(
        !said.contains("stranger-ran"),
        "{who}'s command ran: {said}"
    );
}

/// The candidate's own package cannot open its reviewer's pipe.
fn candidate_is_kept_out(runner: &Runner, trees: &Trees, pipe: &str) -> String {
    use agentc_supervisor::candidate::form::single_quoted;
    let program = env!("CARGO_BIN_EXE_agentc-supervisor");
    std::fs::copy(program, trees.at("tools").join("agentc-supervisor.exe")).unwrap();
    let words: Vec<String> = stranger_arguments(pipe)
        .iter()
        .map(|word| single_quoted(word))
        .collect();
    let candidate = runner.candidate("reviewer").unwrap();
    let call = candidate
        .run(&format!("agentc-supervisor.exe {}", words.join(" ")))
        .unwrap();
    assert_kept_out("the candidate", u64::from(call.status), &text(&call));
    text(&call).trim().to_owned()
}

/// Another launch's package cannot open the pipe either.
fn another_launch_is_kept_out(runner: &mut Runner, trees: &Trees, pipe: &str) -> String {
    let other = json!({"op": "prepare", "launch": "other", "spec": trees.launch("other")});
    let prepared = ask(runner, other);
    assert_eq!(prepared["ok"], true, "{prepared}");
    let owner = token::current_user_sid().unwrap();
    let package = prepared["package_sid"].as_str().unwrap();
    let list = sddl::shared_source(&owner, &[package]).unwrap();
    create_directory(&trees.at("other-tools"), &list).unwrap();
    let program = trees.at("other-tools").join("agentc-supervisor.exe");
    std::fs::copy(env!("CARGO_BIN_EXE_agentc-supervisor"), &program).unwrap();
    let mut argv = vec![program.display().to_string()];
    argv.extend(stranger_arguments(pipe));
    let (code, said) = run_in_launch(runner, trees, "other", json!(argv));
    assert_kept_out("another launch", code, &said);
    let cleanup = json!({"op": "cleanup", "launch": "other", "remove_directories": true});
    assert_eq!(ask(runner, cleanup)["ok"], true);
    said.trim().to_owned()
}

/// The reviewer's own package was served; the candidate's package and
/// another launch's package cannot even open the pipe.
fn check_strangers(runner: &mut Runner, trees: &Trees, pipe: &str) -> String {
    let own = candidate_is_kept_out(runner, trees, pipe);
    let other = another_launch_is_kept_out(runner, trees, pipe);
    format!("candidate: {own:?}; other launch: {other:?}")
}

/// The shim role: see the test that starts it.
fn shim() -> String {
    let dir = tempfile::tempdir().unwrap();
    let trees = Trees {
        root: dir.path().to_owned(),
    };
    let mut runner = Runner::new(&trees.at("state")).unwrap();
    let request = json!({"op": "prepare", "launch": "reviewer", "spec": trees.launch("reviewer"),
                         "candidate": trees.candidate()});
    let prepared = ask(&mut runner, request);
    assert_eq!(prepared["ok"], true, "{prepared}");
    tools(&trees, prepared["candidate_package_sid"].as_str().unwrap());
    reviewer_tools(&trees, prepared["package_sid"].as_str().unwrap());
    let pipe = "agentc-candidate-reviewer";
    let shell = trees.at("reviewer-tools").join("sh.exe");
    let argv = json!([shell, "-c", reviewer_script(&trees, pipe)]);
    let (code, said) = run_in_launch(&mut runner, &trees, "reviewer", argv);
    assert_eq!(code, 0, "{said}");
    check_reviewer(&said, &trees);
    let ran = trees.at("candidate-work").join("tmp");
    assert!(
        ran.join("good-ran").exists(),
        "the accepted command did not run"
    );
    assert!(!ran.join("outside-ran").exists(), "a refused command ran");
    assert!(!trees.at("source").join("hook-ran").exists(), "a hook ran");
    let strangers = check_strangers(&mut runner, &trees, pipe);
    let cleanup = json!({"op": "cleanup", "launch": "reviewer", "remove_directories": true});
    let cleaned = ask(&mut runner, cleanup);
    assert_eq!(cleaned["ok"], true, "{cleaned}");
    format!("{:?} | {strangers}", said.trim())
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

/// Step 5e: a synthetic credential kept by a reviewer launch (in its
/// environment, its files, its process, a handle it inherited and a
/// registry key) is out of its candidate's reach, and so are the reviewer's
/// pipe, another launch and the runner; a broken or missing candidate
/// runner refuses the launch. The same probe runs first in the reviewer,
/// where it must reach what is the reviewer's own, so a refusal in the
/// candidate is not a probe that cannot work.
#[test]
fn a_reviewers_credential_and_processes_are_out_of_its_candidates_reach() {
    with_the_pinned_shell("fixture");
}

/// Names the file that tells the probe what to try.
const TARGETS: &str = "AGENTC_CANDIDATE_TEST_TARGETS";
/// The registry value the synthetic credential is kept in.
const VALUE: &str = "credential";

/// One line of the probe's report.
fn line(kind: &str, name: &str, outcome: &str) -> String {
    format!("{kind} {name} = {outcome}")
}

/// The Windows error of a failed attempt, as the report spells it.
fn refused(error: &std::io::Error) -> String {
    format!("error {}", error.raw_os_error().unwrap_or(-1))
}

/// Whether the file at `path` can be read, and whether it is the credential.
fn reach_file(path: &str) -> String {
    match std::fs::read_to_string(path) {
        Ok(text) if text.contains(SYNTHETIC) => "read".into(),
        Ok(_) => "read something else".into(),
        Err(error) => refused(&error),
    }
}

/// Whether the process `pid` can be opened to be queried, to have its
/// memory read (its environment is there) and to have a handle taken out.
fn reach_process(pid: u64) -> String {
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_DUP_HANDLE, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
    };
    let rights = [
        ("query", PROCESS_QUERY_LIMITED_INFORMATION),
        ("read", PROCESS_VM_READ),
        ("handles", PROCESS_DUP_HANDLE),
    ];
    let attempt = |(name, right): (&str, u32)| {
        // SAFETY: opens a process by id; a handle that comes back is closed.
        let handle = unsafe { OpenProcess(right, 0, pid as u32) };
        if handle.is_null() {
            return format!("{name}: {}", refused(&std::io::Error::last_os_error()));
        }
        // SAFETY: the handle was just opened and is closed once.
        unsafe { CloseHandle(handle) };
        format!("{name}: opened")
    };
    rights.map(attempt).join(", ")
}

/// Whether the pipe `name` can be opened with a client's rights.
fn reach_pipe(name: &str) -> String {
    use agentc_supervisor::service::pipe_policy::{CLIENT_ACCESS, pipe_path};
    use std::os::windows::fs::OpenOptionsExt;
    let mut options = std::fs::OpenOptions::new();
    options.access_mode(CLIENT_ACCESS);
    match options.open(pipe_path(name).unwrap()) {
        Ok(_) => "opened".into(),
        Err(error) => refused(&error),
    }
}

/// Whether the key `subkey` of the account's hive can be opened and its
/// credential read.
fn reach_key(subkey: &str) -> String {
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
    };
    let (name, value) = (wide(subkey), wide(VALUE));
    let mut key: HKEY = std::ptr::null_mut();
    let mut bytes = [0_u8; 512];
    let mut size = bytes.len() as u32;
    // SAFETY: the strings are NUL-terminated; `key` receives a handle that
    // is closed once; the buffer is writable for `size` bytes.
    unsafe {
        let opened = RegOpenKeyExW(HKEY_CURRENT_USER, name.as_ptr(), 0, KEY_READ, &mut key);
        if opened != 0 {
            return format!("error {opened}");
        }
        let (none, data) = (std::ptr::null_mut(), bytes.as_mut_ptr());
        let read = RegQueryValueExW(key, value.as_ptr(), none, none, data, &mut size);
        RegCloseKey(key);
        if read != 0 {
            return format!("opened, value error {read}");
        }
    }
    "read".into()
}

/// The path of every disk file this process holds a handle to, lower case.
fn file_handles() -> Vec<String> {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_TYPE_DISK, GetFileType, GetFinalPathNameByHandleW,
    };
    let path = |raw: usize| {
        let handle = raw as windows_sys::Win32::Foundation::HANDLE;
        let mut name = [0_u16; 1024];
        // SAFETY: `raw` is a handle of this process, from its own handle
        // table; the buffer is writable for its length.
        let length = unsafe {
            if GetFileType(handle) != FILE_TYPE_DISK {
                return None;
            }
            GetFinalPathNameByHandleW(handle, name.as_mut_ptr(), name.len() as u32, 0)
        };
        let text = String::from_utf16_lossy(&name[..(length as usize).min(name.len())]);
        (length > 0).then(|| text.to_lowercase())
    };
    own_handles().into_iter().filter_map(path).collect()
}

#[link(name = "ntdll")]
unsafe extern "system" {
    /// The native query the handle snapshot comes from.
    fn NtQueryInformationProcess(
        process: windows_sys::Win32::Foundation::HANDLE,
        class: u32,
        information: *mut core::ffi::c_void,
        length: u32,
        returned: *mut u32,
    ) -> i32;
}

/// The value of every handle this process holds, from its own handle
/// table. Guessing values instead ends a process in an AppContainer with
/// an invalid-handle exception (seen 2026-11-17).
fn own_handles() -> Vec<usize> {
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    /// `ProcessHandleInformation`: a count, a reserved word, then one
    /// five-word entry per handle whose first word is the handle's value.
    const HANDLE_INFORMATION: u32 = 51;
    const WORDS_PER_ENTRY: usize = 5;
    let mut words = vec![0_usize; 2 + WORDS_PER_ENTRY * 16_384];
    let bytes = (words.len() * size_of::<usize>()) as u32;
    let mut returned = 0_u32;
    // SAFETY: the buffer is writable for `bytes` bytes and aligned for the
    // words Windows writes into it.
    let status = unsafe {
        let buffer = words.as_mut_ptr().cast();
        let process = GetCurrentProcess();
        NtQueryInformationProcess(process, HANDLE_INFORMATION, buffer, bytes, &mut returned)
    };
    assert_eq!(status, 0, "NtQueryInformationProcess");
    let entries = words[2..].chunks(WORDS_PER_ENTRY).take(words[0]);
    entries.map(|entry| entry[0]).collect()
}

/// The probe: tries everything the targets file names and reports each
/// outcome on its own line.
fn reach() -> String {
    let file = std::env::var(TARGETS).unwrap();
    let targets: Value = serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
    let named = |kind: &str| targets[kind].as_object().cloned().unwrap_or_default();
    let mut lines = Vec::new();
    for (name, path) in named("files") {
        lines.push(line("file", &name, &reach_file(path.as_str().unwrap())));
    }
    for (name, pid) in named("processes") {
        lines.push(line(
            "process",
            &name,
            &reach_process(pid.as_u64().unwrap()),
        ));
    }
    for (name, pipe) in named("pipes") {
        lines.push(line("pipe", &name, &reach_pipe(pipe.as_str().unwrap())));
    }
    for (name, key) in named("keys") {
        lines.push(line("key", &name, &reach_key(key.as_str().unwrap())));
    }
    lines.push(format!("handles = {}", file_handles().join(" | ")));
    lines.join("\n")
}

/// A key of the account's hive holding the synthetic credential, which the
/// owner and `package` may read and nobody else. Deleted when dropped.
struct CredentialKey(String);

impl CredentialKey {
    /// Creates the key with a protected access list.
    fn create(package: &str) -> Self {
        use agentc_supervisor::host_runtime::windows::sys::Descriptor;
        use windows_sys::Win32::System::Registry::{
            HKEY, HKEY_CURRENT_USER, KEY_ALL_ACCESS, REG_SZ, RegCloseKey, RegCreateKeyExW,
            RegSetValueExW,
        };
        let subkey = format!(r"Software\agentc-test-{}", uuid::Uuid::new_v4().simple());
        let owner = token::current_user_sid().unwrap();
        let list = format!("D:P(A;;KA;;;{owner})(A;;KR;;;{package})");
        let security = Descriptor::parse(&list).unwrap();
        let attributes = security.attributes(false);
        let (name, value, data) = (wide(&subkey), wide(VALUE), wide(SYNTHETIC));
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: every string is NUL-terminated; the attributes and their
        // descriptor outlive the call; `key` receives a handle closed once.
        unsafe {
            #[rustfmt::skip]
            let created = RegCreateKeyExW(HKEY_CURRENT_USER, name.as_ptr(), 0, std::ptr::null(),
                0, KEY_ALL_ACCESS, &attributes, &mut key, std::ptr::null_mut());
            assert_eq!(created, 0, "RegCreateKeyExW");
            let bytes = (data.len() * 2) as u32;
            let set = RegSetValueExW(key, value.as_ptr(), 0, REG_SZ, data.as_ptr().cast(), bytes);
            assert_eq!(set, 0, "RegSetValueExW");
            RegCloseKey(key);
        }
        Self(subkey)
    }
}

impl Drop for CredentialKey {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RegDeleteKeyW};
        let name = wide(&self.0);
        // SAFETY: the name is NUL-terminated.
        unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, name.as_ptr()) };
    }
}

/// A tool directory `package` may read: the pinned shell and this test
/// program as `probe.exe`.
fn probe_tools(tools: &Path, package: &str) {
    shell_tools(tools, package);
    std::fs::copy(std::env::current_exe().unwrap(), tools.join("probe.exe")).unwrap();
}

/// The shell text that runs the probe with its report at `report`.
fn probe_line(targets: &Path, report: &str) -> String {
    format!(
        "{ROLE}=reach {REPORT}=\"{report}\" {TARGETS}='{}' probe.exe {CHILD} --exact --ignored",
        slashed(targets)
    )
}

/// Starts `argv` as the root of `launch` with `extra` variables and the
/// held handles `listed`, and returns its process id.
fn start_in_launch(
    runner: &mut Runner,
    trees: &Trees,
    launch: &str,
    argv: Value,
    (extra, listed): (&[(&str, &str)], &[u64]),
) -> u64 {
    let (tools, work) = (format!("{launch}-tools"), format!("{launch}-work"));
    let mut environment = launch_environment(&trees.at(&tools), &trees.at(&work));
    for (name, value) in extra {
        environment.insert(name.to_string(), json!(value));
    }
    let log = trees.at(&format!("{launch}.log"));
    let start = json!({"op": "start", "launch": launch, "argv": argv, "listed": listed,
        "directory": trees.at(&work), "environment": environment, "output": log});
    let started = ask(runner, start);
    assert_eq!(started["ok"], true, "{started}");
    started["pid"].as_u64().unwrap()
}

/// Waits for the probe's report at `path` and returns its lines; the probe
/// writes `ok: ` before them.
fn report_at(path: &Path, log: &Path) -> Vec<String> {
    for _ in 0..600 {
        let said = std::fs::read_to_string(path).unwrap_or_default();
        if said.starts_with("ok: ") {
            // Read again a moment later: the first look may be mid-write.
            std::thread::sleep(std::time::Duration::from_millis(300));
            let whole = std::fs::read_to_string(path).unwrap();
            return whole[4..].lines().map(str::to_owned).collect();
        }
        assert!(said.is_empty(), "the probe failed: {said}");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let said = std::fs::read_to_string(log).unwrap_or_default();
    panic!("no report at {}; the log says: {said}", path.display());
}

/// The outcome the report `lines` give for `kind name`.
fn outcome<'a>(lines: &'a [String], kind: &str, name: &str) -> &'a str {
    let start = format!("{kind} {name} = ");
    let found = lines.iter().find_map(|line| line.strip_prefix(&start));
    found.unwrap_or_else(|| panic!("no line for {kind} {name} in {lines:?}"))
}

/// A broken candidate runner (its shell is not there yet) answers the
/// readiness check as broken and its launch is not started; a launch
/// without a candidate has a missing runner; the capability record refuses
/// a reviewer for both.
fn check_readiness(runner: &mut Runner, trees: &Trees) -> String {
    use agentc_supervisor::candidate::capability::{Runner as Readiness, decide};
    let ready = |runner: &mut Runner, launch: &str| {
        ask(runner, json!({"op": "candidate_ready", "launch": launch}))
    };
    let broken = ready(runner, "reviewer");
    assert_eq!(broken["runner"], "broken", "{broken}");
    let shell = trees.at("reviewer-tools").join("sh.exe");
    let start = json!({"op": "start", "launch": "reviewer", "argv": [shell, "-c", "true"],
        "directory": trees.at("reviewer-work"), "environment": {}, "output": trees.at("no.log")});
    let refused = ask(runner, start);
    assert_eq!(refused["ok"], false, "{refused}");
    let why = refused["error"].as_str().unwrap().to_owned();
    assert!(why.contains("the candidate runner is broken"), "{why}");
    assert!(!trees.at("no.log").exists(), "a refused launch got a log");
    let plain = json!({"op": "prepare", "launch": "plain", "spec": trees.launch("plain")});
    assert_eq!(ask(runner, plain)["ok"], true);
    assert_eq!(ready(runner, "plain")["runner"], "missing");
    let cleanup = json!({"op": "cleanup", "launch": "plain", "remove_directories": true});
    assert_eq!(ask(runner, cleanup)["ok"], true);
    let detail = broken["detail"].as_str().unwrap().to_owned();
    for state in [Readiness::Missing, Readiness::Broken(detail.clone())] {
        let facts = windows_reviewer(state);
        assert!(decide(&facts).is_err(), "{facts:?} was admitted");
    }
    format!("broken: {detail}; start refused: {why}")
}

/// A Windows Claude reviewer of the recorded build with only routed tools,
/// whose candidate runner is in `runner`'s state.
fn windows_reviewer(
    runner: agentc_supervisor::candidate::capability::Runner,
) -> agentc_supervisor::candidate::capability::Facts<'static> {
    use agentc_supervisor::candidate::capability::{Facts, Host, ROUTED_TOOLS, WINDOWS_CLAUDE};
    use agentc_supervisor::profile::{Harness, Role};
    let build = &WINDOWS_CLAUDE[0];
    Facts {
        host: Host::Windows,
        role: Role::Reviewer,
        harness: Harness::Claude,
        build: Some((build.version, build.sha256)),
        tools: ROUTED_TOOLS,
        runner,
    }
}

/// The two launches of the fixture and what each keeps.
struct Fixture {
    /// The reviewer's root process.
    reviewer: u64,
    /// The other launch's root process.
    other: u64,
    /// Kept until the fixture ends, which deletes the key.
    key: CredentialKey,
}

/// Prepares the reviewer and another reviewer launch, each with its
/// candidate, and returns the reviewer's and its candidate's packages.
fn prepare_both(runner: &mut Runner, trees: &Trees) -> (String, String) {
    let request = json!({"op": "prepare", "launch": "reviewer", "spec": trees.launch("reviewer"),
                         "candidate": trees.candidate()});
    let prepared = ask(runner, request);
    assert_eq!(prepared["ok"], true, "{prepared}");
    let other = json!({"op": "prepare", "launch": "other", "spec": trees.launch("other"),
                       "candidate": trees.other_candidate()});
    let second = ask(runner, other);
    assert_eq!(second["ok"], true, "{second}");
    probe_tools(
        &trees.at("other-tools"),
        second["package_sid"].as_str().unwrap(),
    );
    let package = second["candidate_package_sid"].as_str().unwrap();
    shell_tools(&trees.at("other-candidate-tools"), package);
    let sid = |name: &str| prepared[name].as_str().unwrap().to_owned();
    (sid("package_sid"), sid("candidate_package_sid"))
}

/// Plants the credential in the reviewer's and the other launch's files and
/// in a file only the runner's account can open, and writes the targets
/// file into the source tree, which both packages can read.
fn plant(trees: &Trees, fixture: &Fixture) -> PathBuf {
    for tree in [
        "reviewer-control",
        "reviewer-work",
        "other-control",
        "other-work",
    ] {
        std::fs::write(trees.at(tree).join("secret.txt"), SYNTHETIC).unwrap();
    }
    let secret = |tree: &str| trees.at(tree).join("secret.txt");
    let targets = json!({
        "files": {"reviewer-control": secret("reviewer-control"),
                  "reviewer-work": secret("reviewer-work"), "held": trees.at("held.txt"),
                  "other-control": secret("other-control"), "other-work": secret("other-work")},
        "processes": {"reviewer": fixture.reviewer, "other": fixture.other,
                      "runner": std::process::id()},
        "pipes": {"reviewer": "agentc-candidate-reviewer", "other": "agentc-candidate-other"},
        "keys": {"reviewer": fixture.key.0},
    });
    let file = trees.at("source").join("targets.json");
    std::fs::write(&file, targets.to_string()).unwrap();
    file
}

/// Starts both launches' root processes: each holds the credential in its
/// environment, and the reviewer's also inherits a handle to a file only
/// the runner's account can open. They sleep until they are ended.
fn start_both(runner: &mut Runner, trees: &Trees, package: &str) -> Fixture {
    std::fs::write(trees.at("held.txt"), SYNTHETIC).unwrap();
    let hold = json!({"op": "hold_file", "launch": "reviewer", "path": trees.at("held.txt")});
    let held = ask(runner, hold);
    assert_eq!(held["ok"], true, "{held}");
    let listed = [held["handle"].as_u64().unwrap()];
    let secret = [("ANTHROPIC_API_KEY", SYNTHETIC)];
    let sleeper = |launch: &str| {
        let shell = trees.at(&format!("{launch}-tools")).join("sh.exe");
        json!([
            shell,
            "-c",
            "while [ ! -f go ]; do sleep 1; done; sh go; sleep 600"
        ])
    };
    let reviewer = start_in_launch(
        runner,
        trees,
        "reviewer",
        sleeper("reviewer"),
        (&secret, &listed),
    );
    let other = start_in_launch(runner, trees, "other", sleeper("other"), (&secret, &[]));
    Fixture {
        key: CredentialKey::create(package),
        reviewer,
        other,
    }
}

/// What the reviewer's own probe must reach, and must not: its files, its
/// pipe, its key and the handle it inherited are its own; the other launch
/// and the runner are not.
fn check_control(lines: &[String]) {
    for (kind, name, wanted) in [
        ("file", "reviewer-control", "read"),
        ("file", "reviewer-work", "read"),
        ("pipe", "reviewer", "opened"),
        ("key", "reviewer", "read"),
    ] {
        assert_eq!(
            outcome(lines, kind, name),
            wanted,
            "{kind} {name}: {lines:?}"
        );
    }
    let handles = lines.last().unwrap();
    assert!(
        handles.contains("held.txt"),
        "the reviewer holds no handle: {lines:?}"
    );
    for (kind, name) in [
        ("file", "held"),
        ("file", "other-control"),
        ("file", "other-work"),
    ] {
        assert_eq!(
            outcome(lines, kind, name),
            "error 5",
            "{kind} {name}: {lines:?}"
        );
    }
    assert_eq!(outcome(lines, "pipe", "other"), "error 5", "{lines:?}");
}

/// Nothing of the fixture is within the candidate's reach: every file,
/// pipe and key is refused, no process can be opened in any way, and it
/// holds no handle to a file of the fixture outside its own trees.
fn check_candidate(lines: &[String], trees: &Trees) {
    let (handles, attempts) = lines.split_last().unwrap();
    for attempt in attempts {
        let (what, outcome) = attempt.split_once(" = ").unwrap();
        let reached = ["read", "opened"]
            .iter()
            .any(|word| outcome.split([',', ' ']).any(|part| part == *word));
        assert!(
            !reached && outcome.contains("error"),
            "the candidate reached {what}: {outcome}"
        );
    }
    let root = trees.root.display().to_string().to_lowercase();
    let own = [
        r"\candidate-control",
        r"\candidate-work",
        r"\source",
        r"\tools",
    ];
    let held = handles.strip_prefix("handles = ").unwrap();
    for path in held.split(" | ").filter(|path| path.contains(&root)) {
        let tree = &path[path.find(&root).unwrap() + root.len()..];
        let allowed = own.iter().any(|name| tree.starts_with(name));
        assert!(allowed, "the candidate holds a handle to {path}");
    }
}

/// The fixture role: see the test that starts it.
fn fixture() -> String {
    let dir = tempfile::tempdir().unwrap();
    let trees = Trees {
        root: dir.path().to_owned(),
    };
    let mut runner = Runner::new(&trees.at("state")).unwrap();
    let (package, candidate_package) = prepare_both(&mut runner, &trees);
    let readiness = check_readiness(&mut runner, &trees);
    probe_tools(&trees.at("tools"), &candidate_package);
    probe_tools(&trees.at("reviewer-tools"), &package);
    let ready = ask(
        &mut runner,
        json!({"op": "candidate_ready", "launch": "reviewer"}),
    );
    assert_eq!(ready["runner"], "ready", "{ready}");
    let fixture = start_both(&mut runner, &trees, &package);
    let targets = plant(&trees, &fixture);
    let report = slashed(&trees.at("reviewer-work").join("reach.txt"));
    let go = trees.at("reviewer-work").join("go");
    std::fs::write(go, probe_line(&targets, &report)).unwrap();
    let log = trees.at("reviewer.log");
    let control = report_at(&trees.at("reviewer-work").join("reach.txt"), &log);
    check_control(&control);
    let candidate = runner.candidate("reviewer").unwrap();
    let probed = candidate
        .run(&probe_line(&targets, "$TEMP/reach.txt"))
        .unwrap();
    let shown = candidate.run("env").unwrap();
    drop(candidate);
    assert!(
        !text(&shown).contains(SYNTHETIC),
        "the credential is in the environment"
    );
    let report = trees.at("candidate-work").join("tmp").join("reach.txt");
    assert!(
        report.exists(),
        "the candidate's probe did not run: {}",
        text(&probed)
    );
    let reached = report_at(&report, &log);
    check_candidate(&reached, &trees);
    for launch in ["reviewer", "other"] {
        let cleanup = json!({"op": "cleanup", "launch": launch, "remove_directories": true});
        let cleaned = ask(&mut runner, cleanup);
        assert_eq!(cleaned["ok"], true, "{cleaned}");
    }
    drop(fixture);
    format!("{readiness} || reviewer: {control:?} || candidate: {reached:?}")
}

/// Names a native Claude Code executable on this host; unset where the
/// owner has none to probe.
const CLAUDE: &str = "AGENTC_TEST_CLAUDE";

/// Step 5f groundwork: whether native Claude Code starts at all inside a
/// launch, full and less privileged. It only asks for the version: no
/// network, no credential and no session.
#[test]
fn native_claude_code_reports_its_version_inside_a_launch() {
    if std::env::var_os(CLAUDE).is_none() {
        eprintln!("{CLAUDE} is unset; harness_probe was not checked");
        return;
    }
    in_a_new_session("harness_probe");
}

/// Runs `claude.exe --version` in a fresh launch, `less` privileged or
/// not, from a copy only that launch's package may read, and returns how
/// it ended and what it printed.
fn harness_probe_one(trees: &Trees, runner: &mut Runner, less: bool) -> String {
    let launch = format!("harness-{less}");
    let mut spec = trees.launch(&launch);
    spec.less_privileged = less;
    let prepared = ask(
        runner,
        json!({"op": "prepare", "launch": launch, "spec": spec}),
    );
    assert_eq!(prepared["ok"], true, "{prepared}");
    let tools = trees.at(&format!("{launch}-tools"));
    let owner = token::current_user_sid().unwrap();
    let package = prepared["package_sid"].as_str().unwrap();
    create_directory(&tools, &sddl::shared_source(&owner, &[package]).unwrap()).unwrap();
    let program = tools.join("claude.exe");
    std::fs::copy(std::env::var_os(CLAUDE).unwrap(), &program).unwrap();
    let work = trees.at(&format!("{launch}-work")).display().to_string();
    let home = [
        ("USERPROFILE", work.as_str()),
        ("HOME", &work),
        ("APPDATA", &work),
    ];
    start_in_launch(
        runner,
        trees,
        &launch,
        json!([program, "--version"]),
        (&home, &[]),
    );
    let seen = ask(
        runner,
        json!({"op": "observe", "launch": launch, "wait_ms": 120_000}),
    );
    let said = std::fs::read_to_string(trees.at(&format!("{launch}.log"))).unwrap_or_default();
    let cleanup = json!({"op": "cleanup", "launch": launch, "remove_directories": true});
    assert_eq!(ask(runner, cleanup)["ok"], true);
    let ended_well = seen["state"] == "exited" && seen["code"] == 0;
    assert!(ended_well, "less_privileged={less}: {seen}: {said}");
    assert!(
        said.contains("(Claude Code)"),
        "less_privileged={less}: {said}"
    );
    format!("less_privileged={less}: {seen} said {:?}", said.trim())
}

/// Both harness probes. Each must end by itself with status 0 and a
/// version line, as observed on MiniAir on 2026-11-17.
fn harness_probe() -> String {
    let dir = tempfile::tempdir().unwrap();
    let trees = Trees {
        root: dir.path().to_owned(),
    };
    let mut runner = Runner::new(&trees.at("state")).unwrap();
    let full = harness_probe_one(&trees, &mut runner, false);
    let less = harness_probe_one(&trees, &mut runner, true);
    format!("{full} | {less}")
}
