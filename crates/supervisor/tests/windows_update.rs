//! `windows-host update`, `rollback` and `uninstall` on a real machine
//! (phase 7, step 7b).
//!
//! The in-process update test runs the updater over real files in a
//! temporary tree, with the role runner in this test process in place of
//! the runner service: its stop goes through the runner's own lifecycle and
//! drain, and its start through the runner's own start-up settling, exactly
//! as the service runs them. The other three services are stand-ins that
//! write their start lines. Nothing is registered.
//!
//! The uninstall test runs the built binary on a temporary tree with a
//! service prefix nothing uses: no service exists, so none is stopped or
//! deleted.
//!
//! One test **registers, starts, stops and deletes four services**: it
//! installs a throwaway host, updates it to a second build, rolls it back
//! and uninstalls it, all through the built binary. It is ignored and also
//! refuses to run unless `AGENTC_INSTALLER_STARTS_SERVICES=1` is set: a
//! machine's owner must ask for it. Its host has a random service prefix
//! and lives in `C:\agentc-t<id>`, beside any real host. Windows keeps the
//! four service accounts' profiles until a restart; the test prints how to
//! remove them then. All of these need an elevated session.
#![cfg(windows)]

use agentc_supervisor::host_runtime::windows::installer::{WindowsHost, files};
use agentc_supervisor::host_runtime::windows::runner::{Request as RunnerRequest, Runner, answer};
use agentc_supervisor::service::drain::Drain;
use agentc_supervisor::service::lifecycle::{
    Kind, Lifecycle, Limits, Monotonic, Report, Request as Ending, Signal,
};
use agentc_supervisor::service::startup::{self, Settled};
use agentc_supervisor::windows_installer::control::{RunState, Services, start_line};
use agentc_supervisor::windows_installer::model::{Inputs, Layout, Role};
use agentc_supervisor::windows_installer::observed::{
    KeySource, PathState, ProfileState, ServiceState,
};
use agentc_supervisor::windows_installer::plan::Action;
use agentc_supervisor::windows_installer::release::{Release, Releases};
use agentc_supervisor::windows_installer::update::{Request, Run, update};
use agentc_supervisor::windows_installer::{Desired, Host, Machine, config_text};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// A remote for the configuration; nothing ever contacts it.
const REMOTE: &str = "https://github.com/example/project.git";

/// The real files of this machine, with the four services' registrations
/// answered from the model: nothing is registered.
struct Files {
    inner: WindowsHost,
    desired: Desired,
}

impl Host for Files {
    fn elevated(&self) -> anyhow::Result<bool> {
        self.inner.elevated()
    }
    fn path(&self, path: &str) -> anyhow::Result<PathState> {
        self.inner.path(path)
    }
    fn text(&self, path: &str) -> anyhow::Result<Option<String>> {
        self.inner.text(path)
    }
    fn sha256(&self, path: &str) -> anyhow::Result<Option<String>> {
        self.inner.sha256(path)
    }
    fn service(&self, name: &str) -> anyhow::Result<Option<ServiceState>> {
        let spec = self.desired.service(name).cloned();
        Ok(spec.map(|spec| ServiceState {
            spec,
            process: None,
        }))
    }
    fn key_source(&self, source: &str, installed: Option<&str>) -> KeySource {
        self.inner.key_source(source, installed)
    }
    fn privileges(&self, process: u32) -> anyhow::Result<Vec<String>> {
        self.inner.privileges(process)
    }
    fn apply(&mut self, _: &Desired, action: &Action) -> anyhow::Result<()> {
        anyhow::bail!("this test changes nothing through install: {action:?}")
    }
}

impl Machine for Files {
    fn copy(&mut self, from: &str, to: &str) -> anyhow::Result<()> {
        self.inner.copy(from, to)
    }
    fn write(&mut self, path: &str, text: &str) -> anyhow::Result<()> {
        self.inner.write(path, text)
    }
    fn remove(&mut self, path: &str) -> anyhow::Result<()> {
        self.inner.remove(path)
    }
    fn make_directory(&mut self, path: &str) -> anyhow::Result<()> {
        self.inner.make_directory(path)
    }
    fn entries(&self, path: &str) -> anyhow::Result<Vec<String>> {
        self.inner.entries(path)
    }
    fn length(&self, path: &str) -> anyhow::Result<u64> {
        self.inner.length(path)
    }
    fn read_after(&self, path: &str, offset: u64) -> anyhow::Result<Option<String>> {
        self.inner.read_after(path, offset)
    }
    fn remove_tree(&mut self, path: &str) -> anyhow::Result<Vec<String>> {
        self.inner.remove_tree(path)
    }
    fn delete_service(&mut self, name: &str) -> anyhow::Result<()> {
        anyhow::bail!("this test deletes no service: {name}")
    }
    fn profile(&self, _: &str) -> anyhow::Result<ProfileState> {
        Ok(ProfileState::Absent)
    }
    fn delete_profile(&mut self, sid: &str) -> anyhow::Result<()> {
        anyhow::bail!("this test deletes no profile: {sid}")
    }
}

/// The limits of a runner whose launches end within a few seconds.
fn limits() -> Limits {
    let seconds = Duration::from_secs;
    Limits {
        stop_grace: seconds(1),
        preshutdown_grace: seconds(1),
        terminate_patience: seconds(20),
        status_interval: seconds(1),
    }
}

/// The runner, as its service runs it, in this process.
struct Live {
    runner: Runner,
    life: Lifecycle<Monotonic>,
    drain: Drain,
    signal: Signal,
    stopping: Option<Instant>,
}

/// The four services: the runner in this process and three stand-ins.
struct InProcess {
    layout: Layout,
    runner: Option<Live>,
    others: BTreeMap<String, RunState>,
    /// What each start of the runner settled.
    settled: Vec<Settled>,
    /// Whether admission ended as soon as each stop was asked for.
    refused_at_stop: Vec<bool>,
    /// How long each runner stop took.
    stops: Vec<Duration>,
    /// Each runner that stopped, kept so the test can clean up after it.
    retired: Vec<Runner>,
}

impl InProcess {
    fn new(layout: &Layout) -> Self {
        Self {
            layout: layout.clone(),
            runner: None,
            others: BTreeMap::new(),
            settled: Vec::new(),
            refused_at_stop: Vec::new(),
            stops: Vec::new(),
            retired: Vec::new(),
        }
    }

    fn is_runner(&self, name: &str) -> bool {
        name == self.layout.service(Role::Runner)
    }

    fn role(&self, name: &str) -> Role {
        Role::ALL
            .into_iter()
            .find(|r| self.layout.service(*r) == name)
            .unwrap()
    }

    /// Appends a start line to `role`'s log, as its service does.
    fn log_start(&self, role: Role) {
        use std::io::Write;
        let path = self.layout.log(role);
        let time = if role == Role::Runner {
            "2026-11-24T04:00:00Z "
        } else {
            ""
        };
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(log, "{time}{}", start_line(role)).unwrap();
    }

    /// Starts the runner over its state directory: start-up settling first,
    /// then the start line, as `runner_service` does.
    fn start_runner(&mut self) -> anyhow::Result<()> {
        let mut runner = Runner::new(Path::new(&self.layout.state("runner")))?;
        self.settled.push(startup::settle(&mut runner)?);
        self.log_start(Role::Runner);
        let life = Lifecycle::new(Monotonic::new(), limits());
        let (drain, signal) = (Drain::new(), Signal::default());
        self.runner = Some(Live {
            runner,
            life,
            drain,
            signal,
            stopping: None,
        });
        Ok(())
    }

    /// One tick of the stopping runner's drain: its state afterwards.
    fn tick(&mut self) -> RunState {
        let Some(live) = self.runner.as_mut() else {
            return RunState::Stopped {
                exit: 0,
                specific: 0,
            };
        };
        let Some(began) = live.stopping else {
            return RunState::Running;
        };
        let step = live
            .drain
            .step(&mut live.life, &live.signal, &mut live.runner);
        let Report::Stopped { quiescent } = step.report else {
            return RunState::Pending;
        };
        self.stops.push(began.elapsed());
        let stopped = self.runner.take().expect("the runner ran");
        self.retired.push(stopped.runner);
        let (exit, specific) = if quiescent { (0, 0) } else { (1066, 1) };
        RunState::Stopped { exit, specific }
    }
}

impl Services for InProcess {
    fn state(&mut self, name: &str) -> anyhow::Result<RunState> {
        if self.is_runner(name) {
            return Ok(self.tick());
        }
        Ok(*self.others.get(name).unwrap_or(&RunState::Stopped {
            exit: 0,
            specific: 0,
        }))
    }

    fn start(&mut self, name: &str) -> anyhow::Result<()> {
        if self.is_runner(name) {
            return self.start_runner();
        }
        self.log_start(self.role(name));
        self.others.insert(name.to_owned(), RunState::Running);
        Ok(())
    }

    fn stop(&mut self, name: &str) -> anyhow::Result<()> {
        if !self.is_runner(name) {
            self.others.insert(
                name.to_owned(),
                RunState::Stopped {
                    exit: 0,
                    specific: 0,
                },
            );
            return Ok(());
        }
        let live = self.runner.as_mut().expect("the runner runs");
        live.signal.request(Ending::Stop);
        self.refused_at_stop
            .push(live.life.admit(&live.signal, Kind::Begins).is_err());
        live.stopping = Some(Instant::now());
        Ok(())
    }

    fn pause(&mut self, duration: Duration) {
        std::thread::sleep(duration.min(Duration::from_millis(100)));
    }
}

/// Sends one request to `runner` and returns its answer line.
fn ask(runner: &mut Runner, request: Value) -> Value {
    let parsed = serde_json::from_value::<RunnerRequest>(request).map_err(anyhow::Error::from);
    answer(parsed.and_then(|request| runner.handle(request)))
}

/// The command interpreter, which a less-privileged token may run.
fn cmd() -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    format!("{root}\\System32\\cmd.exe")
}

/// A journal entry an earlier runner left for launch `launch`, its jobs
/// owned by `owner`.
fn left_behind(state: &Path, launch: &str, owner: &str) {
    let entry = json!({"launch": launch, "stage": "started", "profile": "agentc.test.gone",
                       "package_sid": "S-1-15-2-1", "job_owner": owner,
                       "executable": cmd(), "source": null, "attempt": null,
                       "generation": null, "pid": 0xFFFF_FFF3_u32, "created": 1});
    std::fs::write(state.join(format!("{launch}.json")), entry.to_string()).unwrap();
}

/// Starts a launch that never exits, when this session may make one: in an
/// interactive desktop the launcher refuses, and `false` says so.
fn spin(runner: &mut Runner, root: &Path) -> bool {
    let (control, work, logs) = (root.join("control"), root.join("work"), root.join("logs"));
    std::fs::create_dir_all(&logs).unwrap();
    let profile = format!("agentc.test.{}", uuid::Uuid::new_v4().simple());
    let prepared = ask(
        runner,
        json!({"op": "prepare", "launch": "case", "source": "test",
        "attempt": "a1", "generation": 1,
        "spec": {"profile": profile, "less_privileged": true, "capabilities": [],
                 "limits": {"processes": 8, "process_memory_mb": 256, "job_memory_mb": 512},
                 "control": control, "work": work}}),
    );
    if prepared["refusal"] == "station_not_ours" {
        eprintln!("another program made this logon session's window station: no live launch");
        return false;
    }
    assert_eq!(prepared["ok"], true, "{prepared}");
    let root_dir = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    let started = ask(
        runner,
        json!({"op": "start", "launch": "case",
        "argv": [cmd(), "/d", "/c", "for /l %i in () do @rem"], "directory": work,
        "output": logs.join("spin.txt"),
        "environment": {"SystemRoot": root_dir, "LOCALAPPDATA": std::env::var("LOCALAPPDATA").unwrap()}}),
    );
    assert_eq!(started["ok"], true, "{started}");
    true
}

/// A temporary installed host of `old`'s binary with a random prefix, and
/// a second build of the same binary, one byte longer, to update to.
struct Tree {
    dir: tempfile::TempDir,
    layout: Layout,
    old: Release,
    new: Release,
    source: String,
}

impl Tree {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4().simple().to_string();
        let layout = Layout {
            program_dir: dir.path().join("program").display().to_string(),
            state_root: dir.path().join("state").display().to_string(),
            prefix: format!("agentc-u{}", &id[..10]),
        };
        let build = dir.path().join("build");
        std::fs::create_dir(&build).unwrap();
        let source = build.join("agentc-supervisor.exe");
        let mut bytes = std::fs::read(env!("CARGO_BIN_EXE_agentc-supervisor")).unwrap();
        bytes.extend_from_slice(b"second build of step 7b");
        std::fs::write(&source, &bytes).unwrap();
        let digest = |path: &str| files::sha256(path).unwrap().unwrap();
        let source = source.display().to_string();
        let new = Release {
            sha256: digest(&source),
            commit: "b".repeat(40),
        };
        let old = Release {
            sha256: String::new(),
            commit: "a".repeat(40),
        };
        Self {
            dir,
            layout,
            old,
            new,
            source,
        }
    }
}

/// The model of the tree as the new release builds it.
fn model(tree: &Tree) -> Desired {
    let inputs = Inputs {
        remote: REMOTE.into(),
        ..Inputs::default()
    };
    Desired::new(tree.layout.clone(), inputs, tree.new.sha256.clone(), None).unwrap()
}

/// Makes every modeled directory as `install` makes it: owned by the
/// Administrators group, with its access list. A directory this session
/// made plainly would be owned by the account, which the installer refuses.
fn make_tree(desired: &Desired) {
    use agentc_supervisor::host_runtime::windows::reader::create_directory;
    for directory in &desired.directories {
        if !Path::new(&directory.path).exists() {
            let sddl = directory.access.creation_sddl().unwrap();
            create_directory(Path::new(&directory.path), &sddl).unwrap();
        }
    }
}

/// Lays the tree out as an installed host of the test binary would be.
fn lay_out(tree: &mut Tree, desired: &Desired) {
    make_tree(desired);
    let layout = &tree.layout;
    std::fs::copy(env!("CARGO_BIN_EXE_agentc-supervisor"), layout.binary()).unwrap();
    tree.old.sha256 = files::sha256(&layout.binary()).unwrap().unwrap();
    let config = config_text::merged(None, &desired.settings).unwrap();
    std::fs::write(layout.config(), config).unwrap();
    std::fs::write(layout.manifest(), &desired.manifest).unwrap();
    let releases = Releases {
        current: Some(tree.old.clone()),
        ..Releases::default()
    };
    std::fs::write(layout.releases(), releases.text().unwrap()).unwrap();
}

/// Whether the test may run here: the updater refuses a session that is not
/// elevated.
fn elevated(test: &str) -> bool {
    let elevated = agentc_supervisor::host_runtime::windows::installer::tokens::elevated().unwrap();
    if !elevated {
        eprintln!("skipped {test}: this session is not elevated");
    }
    elevated
}

#[test]
fn an_update_drains_the_runner_switches_and_settles_its_journal_on_the_new_binary() {
    if !elevated("the in-process update") {
        return;
    }
    let mut tree = Tree::new();
    let desired = model(&tree);
    lay_out(&mut tree, &desired);
    let runner_state = PathBuf::from(tree.layout.state("runner"));
    let mut services = InProcess::new(&tree.layout);
    for (_, name) in tree.layout.named(&Role::ALL) {
        services.start(&name).unwrap();
    }
    // A launch an ended runner left, and one this process may still run.
    let me = ask(
        &mut services.runner.as_mut().unwrap().runner,
        json!({"op": "identity"}),
    );
    left_behind(&runner_state, "gone", "runner:4294967281:1");
    left_behind(&runner_state, "live", me["job_owner"].as_str().unwrap());
    let launched = spin(
        &mut services.runner.as_mut().unwrap().runner,
        tree.dir.path(),
    );

    let request = Request {
        to: tree.new.clone(),
        dirty: false,
        built_from: tree.new.commit.clone(),
        retry: false,
    };
    let mut host = Files {
        inner: WindowsHost::new().unwrap(),
        desired: desired.clone(),
    };
    let source = tree.source.clone();
    let mut run = Run {
        host: &mut host,
        services: &mut services,
        desired: &desired,
        source,
    };
    let report = update(&mut run, &request).unwrap();

    assert_eq!(report.outcome, "updated", "{report:?}");
    let installed = files::sha256(&tree.layout.binary()).unwrap().unwrap();
    assert_eq!(
        installed, tree.new.sha256,
        "the services come back on the new binary"
    );
    let kept = files::sha256(&tree.layout.kept_binary(&tree.old.sha256)).unwrap();
    assert_eq!(
        kept.as_deref(),
        Some(tree.old.sha256.as_str()),
        "the old release is kept"
    );
    assert_eq!(
        services.refused_at_stop,
        [true],
        "admission ends as soon as the stop is asked"
    );
    assert_eq!(
        services.settled.len(),
        2,
        "started once before and once after the switch"
    );
    let after = &services.settled[1];
    assert!(
        after.reclaimed.contains(&"gone".to_owned()),
        "{:?}",
        after.reclaimed
    );
    assert!(
        after.may_run.contains(&"live".to_owned()),
        "{:?}",
        after.may_run
    );
    let runner = &mut services
        .runner
        .as_mut()
        .expect("the runner runs again")
        .runner;
    let again = ask(
        runner,
        json!({"op": "prepare", "launch": "live",
        "spec": {"profile": "agentc.test.unused", "less_privileged": true, "capabilities": [],
                 "limits": {"processes": 1, "process_memory_mb": 64, "job_memory_mb": 64},
                 "control": tree.dir.path().join("c"), "work": tree.dir.path().join("w")}}),
    );
    assert!(
        again["error"]
            .as_str()
            .unwrap_or_default()
            .contains("already journaled"),
        "{again}"
    );
    if launched {
        assert!(
            services.stops[0] >= limits().stop_grace,
            "the drain ran to its deadline"
        );
        let entry: Value =
            serde_json::from_slice(&std::fs::read(runner_state.join("case.json")).unwrap())
                .unwrap();
        assert_eq!(entry["stage"], "ended", "only a confirmed end is journaled");
        // The new runner leaves it: its jobs' owner, this process, still
        // runs. A service's next start finds that owner gone and reclaims.
        assert!(after.may_run.contains(&"case".to_owned()), "{after:?}");
        let again = ask(
            runner,
            json!({"op": "prepare", "launch": "case",
            "spec": {"profile": "agentc.test.unused", "less_privileged": true, "capabilities": [],
                     "limits": {"processes": 1, "process_memory_mb": 64, "job_memory_mb": 64},
                     "control": tree.dir.path().join("c2"), "work": tree.dir.path().join("w2")}}),
        );
        assert!(
            again["ok"] == false,
            "the drained launch is never started twice: {again}"
        );
        let retired = services.retired.first_mut().expect("the stopped runner");
        let cleaned = ask(retired, json!({"op": "cleanup", "launch": "case"}));
        assert_eq!(cleaned["profile_deleted"], true, "{cleaned}");
    }
}

/// What WSL is like on this machine: the per-user configuration, the
/// machine-wide one and the names of the distributions. `wsl.exe` prints
/// UTF-16.
fn wsl_snapshot() -> (Option<Vec<u8>>, Option<Vec<u8>>, Vec<String>) {
    let profile = std::env::var("USERPROFILE").unwrap();
    let user = std::fs::read(Path::new(&profile).join(".wslconfig")).ok();
    let root = std::env::var("ProgramData").unwrap_or_else(|_| "C:\\ProgramData".into());
    let machine = std::fs::read(
        Path::new(&root)
            .join("Microsoft")
            .join("WSL")
            .join("wsl.conf"),
    )
    .ok();
    let listed = Command::new("wsl.exe").args(["--list", "--quiet"]).output();
    let names = listed.map(|o| utf16_lines(&o.stdout)).unwrap_or_default();
    (user, machine, names)
}

/// The non-empty lines of UTF-16 text.
fn utf16_lines(bytes: &[u8]) -> Vec<String> {
    let (pairs, _) = bytes.as_chunks::<2>();
    let units: Vec<u16> = pairs.iter().map(|p| u16::from_le_bytes(*p)).collect();
    let text = String::from_utf16_lossy(&units);
    let mut names: Vec<String> = text
        .lines()
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
        .collect();
    names.sort();
    names
}

/// Makes `link` a junction to `target`.
fn junction(link: &Path, target: &Path) {
    let made = Command::new("cmd.exe")
        .arg("/c")
        .arg("mklink")
        .arg("/J")
        .arg(link)
        .arg(target)
        .output()
        .unwrap();
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
}

#[test]
fn uninstall_removes_both_trees_never_through_a_junction_and_leaves_wsl_alone() {
    if !elevated("the uninstall") {
        return;
    }
    let tree = Tree::new();
    let desired = model(&tree);
    make_tree(&desired);
    let outside = tree.dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("keep.txt"), "kept").unwrap();
    junction(
        &Path::new(&tree.layout.state("runner")).join("planted"),
        &outside,
    );
    let readonly = PathBuf::from(tree.layout.state(r"supervisor\ledger.jsonl"));
    std::fs::write(&readonly, "line\n").unwrap();
    let mut permissions = std::fs::metadata(&readonly).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&readonly, permissions).unwrap();
    let before = wsl_snapshot();

    let run = |verb: &str| {
        let output = Command::new(env!("CARGO_BIN_EXE_agentc-supervisor"))
            .args([
                "windows-host",
                verb,
                "--program-dir",
                &tree.layout.program_dir,
            ])
            .args([
                "--state-dir",
                &tree.layout.state_root,
                "--service-prefix",
                &tree.layout.prefix,
            ])
            .output()
            .unwrap();
        let report: Value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
        (
            output.status.code(),
            report,
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    };
    let (code, report, errors) = run("uninstall");
    assert_eq!(code, Some(0), "{report} {errors}");
    assert!(!Path::new(&tree.layout.program_dir).exists());
    assert!(!Path::new(&tree.layout.state_root).exists());
    assert_eq!(
        std::fs::read_to_string(outside.join("keep.txt")).unwrap(),
        "kept"
    );
    let (code, again, errors) = run("uninstall");
    assert_eq!(code, Some(0), "a second run is safe: {again} {errors}");
    assert_eq!(again["removed"], json!([]), "{again}");
    assert_eq!(wsl_snapshot(), before, "WSL is not touched");
}

/// The variable an owner sets to let the registered-host test register,
/// start, stop and delete four services.
const STARTS: &str = "AGENTC_INSTALLER_STARTS_SERVICES";

/// A throwaway host registered with the Service Control Manager: a random
/// prefix and a root beside the drive's root, outside any user profile, so
/// the services' accounts can reach their binary.
struct Registered {
    root: PathBuf,
    layout: Layout,
}

impl Registered {
    fn new() -> Self {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        let root = PathBuf::from(format!(r"{drive}\agentc-t{}", &id[..10]));
        std::fs::create_dir(&root).unwrap();
        let layout = Layout {
            program_dir: root.join("program").display().to_string(),
            state_root: root.join("state").display().to_string(),
            prefix: format!("agentc-t{}", &id[..10]),
        };
        Self { root, layout }
    }

    /// Runs `binary windows-host <verb>` on this host with `more` arguments.
    fn run(&self, binary: &str, verb: &str, more: &[&str]) -> (i32, Value, String) {
        let output = Command::new(binary)
            .args([
                "windows-host",
                verb,
                "--program-dir",
                &self.layout.program_dir,
            ])
            .args([
                "--state-dir",
                &self.layout.state_root,
                "--service-prefix",
                &self.layout.prefix,
            ])
            .args(more)
            .output()
            .unwrap();
        let report = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
        let errors = String::from_utf8_lossy(&output.stderr).into_owned();
        (output.status.code().unwrap_or(-1), report, errors)
    }

    /// The four services' names.
    fn names(&self) -> Vec<String> {
        self.layout
            .named(&Role::ALL)
            .into_iter()
            .map(|(_, name)| name)
            .collect()
    }
}

impl Drop for Registered {
    /// Stops and deletes whatever is left of the services, and the tree.
    fn drop(&mut self) {
        for name in self.names().iter().rev() {
            let _ = Command::new("sc.exe").args(["stop", name]).output();
        }
        std::thread::sleep(Duration::from_secs(5));
        for name in self.names() {
            let _ = Command::new("sc.exe").args(["delete", &name]).output();
        }
        let _ = files::remove_tree(&self.root.display().to_string());
    }
}

/// Stops the registered-host test unless its owner asked for it.
fn asked_to_start() -> bool {
    let asked = std::env::var(STARTS).is_ok_and(|value| value == "1");
    if !asked {
        eprintln!("skipped: set {STARTS}=1 to let this test register and start four services");
    }
    asked
}

/// The commit this checkout is at, which the built binary embeds.
fn head() -> String {
    let output = Command::new("git")
        .args(["-C", env!("CARGO_MANIFEST_DIR"), "rev-parse", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// A second build of the test binary, one trailer longer, outside the host.
fn second_build(host: &Registered) -> (String, String) {
    let dir = host.root.join("build");
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("agentc-supervisor.exe");
    let mut bytes = std::fs::read(env!("CARGO_BIN_EXE_agentc-supervisor")).unwrap();
    bytes.extend_from_slice(b"second build of step 7b");
    std::fs::write(&path, bytes).unwrap();
    let path = path.display().to_string();
    let sha256 = files::sha256(&path).unwrap().unwrap();
    (path, sha256)
}

/// Whether each service's log holds its start line.
fn every_start_logged(host: &Registered) -> bool {
    Role::ALL.into_iter().all(|role| {
        let text = std::fs::read_to_string(host.layout.log(role)).unwrap_or_default();
        agentc_supervisor::windows_installer::control::has_start_line(role, &text)
    })
}

#[test]
#[ignore = "registers, starts, stops and deletes four services; needs the owner's consent and an elevated session"]
fn a_registered_host_updates_rolls_back_and_uninstalls() {
    if !asked_to_start() || !elevated("the registered host") {
        return;
    }
    let host = Registered::new();
    let before = wsl_snapshot();
    let old = env!("CARGO_BIN_EXE_agentc-supervisor");
    let (code, report, errors) = host.run(old, "install", &["--remote", REMOTE]);
    assert_eq!(code, 0, "install: {report} {errors}");
    let old_sha = files::sha256(&host.layout.binary()).unwrap().unwrap();

    let (new, new_sha) = second_build(&host);
    let commit = head();
    let update_args = [
        "--sha256",
        new_sha.as_str(),
        "--source-commit",
        commit.as_str(),
    ];
    let (code, report, errors) = host.run(&new, "update", &update_args);
    assert_eq!(code, 0, "update: {report} {errors}");
    assert_eq!(report["outcome"], "updated", "{report}");
    assert_eq!(
        files::sha256(&host.layout.binary()).unwrap().unwrap(),
        new_sha
    );
    assert!(
        every_start_logged(&host),
        "each service ran on the new binary and logged its start"
    );
    let (code, report, _) = host.run(&new, "update", &update_args);
    assert_eq!(
        (code, report["outcome"].clone()),
        (0, json!("unchanged")),
        "{report}"
    );
    let (code, report, _) = host.run(&new, "status", &[]);
    assert_eq!(code, 0, "an updated host is an installed host: {report}");

    let (code, report, errors) = host.run(old, "rollback", &[]);
    assert_eq!(code, 0, "rollback: {report} {errors}");
    assert_eq!(report["outcome"], "returned", "{report}");
    assert_eq!(
        files::sha256(&host.layout.binary()).unwrap().unwrap(),
        old_sha
    );
    let (code, report, _) = host.run(old, "rollback", &[]);
    assert_eq!(code, 1, "nothing is kept to roll back to: {report}");
    let (code, report, _) = host.run(&new, "update", &update_args);
    assert_eq!(code, 1, "a rolled-back release is refused: {report}");
    assert!(
        report["refusals"][0].as_str().unwrap().contains("--retry"),
        "{report}"
    );

    let (code, report, errors) = host.run(old, "uninstall", &[]);
    eprintln!("uninstall: {report}");
    let pending = report["pending"].as_array().cloned().unwrap_or_default();
    assert!(
        code == 0 || !pending.is_empty(),
        "uninstall: {report} {errors}"
    );
    assert_eq!(report["left"], json!([]), "{report}");
    for name in host.names() {
        let query = Command::new("sc.exe")
            .args(["query", &name])
            .output()
            .unwrap();
        assert_eq!(query.status.code(), Some(1060), "{name} is deleted");
    }
    assert!(!Path::new(&host.layout.program_dir).exists());
    assert!(!Path::new(&host.layout.state_root).exists());
    assert_eq!(wsl_snapshot(), before, "WSL is not touched");
    if !pending.is_empty() {
        eprintln!(
            "after a restart, remove the profiles Windows still holds with: \
             agentc-supervisor.exe windows-host uninstall --program-dir {} --state-dir {} \
             --service-prefix {}",
            host.layout.program_dir, host.layout.state_root, host.layout.prefix
        );
    }
}
