//! `launch-root` without root: both children run as the test's own account
//! (`Plan::switch` is false), with stub programs in place of `agentc-push`
//! and the installed `agentc-supervisor` that record their arguments.
use super::*;
use crate::profile::Harness;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use uuid::Uuid;

/// Stands in for `agentc-push serve`: records its arguments, environment and
/// parent, then behaves per `MODE` (`bind`, `exit`, `silent`, `stubborn`)
/// until stopped or until its parent, the test process, is gone.
const HELPER: &str = r#"#!/usr/bin/python3
import json, os, signal, socket, sys, time
here = os.path.dirname(os.path.abspath(__file__))
args = sys.argv[1:]
MODE = "@MODE@"
path = [a.split("=", 1)[1] for a in args if a.startswith("--socket=")][0]
def stop(*_):
    if MODE == "bind":
        os.unlink(path)
    open(os.path.join(here, "stopped"), "w").close()
    sys.exit(0)
signal.signal(signal.SIGTERM, signal.SIG_IGN if MODE == "stubborn" else stop)
record = {"args": args, "env": dict(os.environ), "pid": os.getpid(), "ppid": os.getppid(), "cwd": os.getcwd()}
json.dump(record, open(os.path.join(here, "helper.json"), "w"))
print("helper started", file=sys.stderr, flush=True)
if MODE == "exit":
    sys.exit(3)
if MODE in ("bind", "stubborn"):
    server = socket.socket(socket.AF_UNIX)
    server.bind(path)
parent = os.getppid()
while os.getppid() == parent:
    time.sleep(0.05)
"#;

/// Stands in for the installed `agentc-supervisor launch`: records its
/// arguments, environment and whether its push socket is live, then exits
/// with `CODE`.
const LAUNCH: &str = r#"#!/usr/bin/python3
import json, os, stat, sys
here = os.path.dirname(os.path.abspath(__file__))
args = sys.argv[1:]
sockets = [a.split("=", 1)[1] for a in args if a.startswith("--push-socket=")]
live = bool(sockets) and stat.S_ISSOCK(os.lstat(sockets[0]).st_mode)
record = {"args": args, "env": dict(os.environ), "socket_live": live, "cwd": os.getcwd()}
json.dump(record, open(os.path.join(here, "launch.json"), "w"))
sys.exit(@CODE@)
"#;

struct Fixture {
    root: tempfile::TempDir,
    config: Config,
    spec: LaunchSpec,
}

/// Whether test `name` must skip because it runs nested inside a launch
/// sandbox: its fixture lives under `/tmp`, for short socket paths, and a
/// launch mounts `/tmp` read-only.
fn skip_without_tmp(name: &str) -> bool {
    crate::test_support::skip_when_nested_because(name, crate::test_support::READ_ONLY_TMP)
}

/// Writes an executable script.
fn script(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

impl Fixture {
    /// An implementer run with a helper stub in `mode` and a launch stub
    /// exiting with `code`. Under `/tmp`, so socket paths stay short.
    fn new(mode: &str, code: i32) -> Self {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let config = Config {
            state_dir: root.path().join("s"),
            bin_dir: root.path().join("bin"),
            push_helper: crate::config::PushHelper {
                program: root.path().join("h/agentc-push"),
                config: root.path().join("push.toml"),
                user: "unused".into(),
                ..Default::default()
            },
            ..Config::default()
        };
        script(&config.push_helper.program, &HELPER.replace("@MODE@", mode));
        script(
            &crate::relay::program(&config),
            &LAUNCH.replace("@CODE@", &code.to_string()),
        );
        let role = config.state_dir.join("impl");
        let spec = LaunchSpec {
            role: Role::Implementer,
            harness: Harness::Claude,
            clone: role.join("clones/c"),
            run: role.join("runs/r"),
            model: "m".into(),
            effort: "high".into(),
            session_id: Uuid::new_v4(),
            project: Some("p1".into()),
            task: Some("task-1".into()),
            push_socket: None,
        };
        fs::create_dir_all(&spec.clone).unwrap();
        fs::create_dir_all(&spec.run).unwrap();
        Self { root, config, spec }
    }

    /// A plan running every child as this test's account.
    fn plan(&self) -> Plan<'_> {
        Plan {
            spec: &self.spec,
            config: &self.config,
            config_path: Some(Path::new("/etc/agentc/supervisor.toml")),
            helper: Account::current(),
            launcher: Account::current(),
            switch: false,
            start_timeout: Duration::from_secs(10),
            stop_timeout: Duration::from_secs(5),
        }
    }

    /// The JSON a stub recorded, if it ran.
    fn record(&self, name: &str) -> Option<Value> {
        let path = match name {
            "helper" => self.root.path().join("h/helper.json"),
            _ => self.config.bin_dir.join("launch.json"),
        };
        let text = fs::read_to_string(path).ok()?;
        Some(serde_json::from_str(&text).unwrap())
    }

    /// The launch's helper directory.
    fn launch_dir(&self) -> PathBuf {
        self.config
            .state_dir
            .join("push")
            .join(self.spec.session_id.to_string())
    }

    /// The names left in the push root besides its `.sweep` lock.
    fn push_entries(&self) -> Vec<String> {
        let entries = fs::read_dir(self.config.state_dir.join("push")).unwrap();
        let names = entries.map(|entry| entry.unwrap().file_name().into_string().unwrap());
        names.filter(|name| name != ".sweep").collect()
    }
}

/// A recorded environment as a sorted map.
fn environment(record: &Value) -> BTreeMap<String, String> {
    serde_json::from_value(record["env"].clone()).unwrap()
}

/// A recorded argument list.
fn arguments(record: &Value) -> Vec<String> {
    serde_json::from_value(record["args"].clone()).unwrap()
}

/// Whether process `pid` still exists.
fn alive(pid: &Value) -> bool {
    let pid = libc::pid_t::try_from(pid.as_i64().unwrap()).unwrap();
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[test]
fn implementer_launch_runs_beside_a_helper_with_exact_arguments_and_environment() {
    if skip_without_tmp(
        "implementer_launch_runs_beside_a_helper_with_exact_arguments_and_environment",
    ) {
        return;
    }
    let fixture = Fixture::new("bind", 7);
    let coordinator = fixture.spec.run.join("state/coordinator");
    fs::create_dir_all(&coordinator).unwrap();
    let credentials = "[[credentials]]\norigin = \"https://c.example\"\ntoken = \"secret-token\"\n";
    fs::write(coordinator.join("credentials.toml"), credentials).unwrap();
    assert_eq!(run(&fixture.plan()).unwrap(), 7, "the launch's exit code");
    let directory = fixture.launch_dir();
    let socket = directory.join("sock/push.sock");
    let helper = fixture.record("helper").unwrap();
    let digest = hex::encode(Sha256::digest(b"secret-token"));
    let expected = [
        "serve".to_owned(),
        format!(
            "--config={}",
            fixture.root.path().join("push.toml").display()
        ),
        format!("--socket={}", socket.display()),
        "--task=task-1".into(),
        format!("--launch={}", fixture.spec.session_id),
        format!("--work-dir={}", directory.join("work/repo").display()),
        format!("--known-digest={digest}"),
        format!("--parent-pid={}", std::process::id()),
    ];
    assert_eq!(arguments(&helper), expected);
    assert_eq!(helper["ppid"].as_u64(), Some(u64::from(std::process::id())));
    assert_eq!(helper["cwd"], "/");
    let home = directory.join("work").display().to_string();
    let wanted = [
        ("HOME", home.as_str()),
        ("LANG", "C.UTF-8"),
        ("PATH", CHILD_PATH),
    ];
    let wanted: BTreeMap<String, String> = wanted
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    assert_eq!(
        environment(&helper),
        wanted,
        "the helper's environment is not cleared"
    );
    let launch = fixture.record("launch").unwrap();
    assert_eq!(
        launch["socket_live"], true,
        "the launch ran without a live socket"
    );
    let spec = &fixture.spec;
    let expected = [
        "--config=/etc/agentc/supervisor.toml".to_owned(),
        "launch".into(),
        "--role=implementer".into(),
        "--harness=claude".into(),
        format!("--clone={}", spec.clone.display()),
        format!("--run={}", spec.run.display()),
        "--model=m".into(),
        "--effort=high".into(),
        format!("--session-id={}", spec.session_id),
        "--project=p1".into(),
        "--task=task-1".into(),
        format!("--push-socket={}", socket.display()),
    ];
    assert_eq!(arguments(&launch), expected);
    let home = fixture
        .config
        .state_dir
        .join("impl/home")
        .display()
        .to_string();
    let wanted = [
        ("HOME", home.as_str()),
        ("LANG", "C.UTF-8"),
        ("PATH", CHILD_PATH),
    ];
    let wanted: BTreeMap<String, String> = wanted
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    assert_eq!(
        environment(&launch),
        wanted,
        "the launch's environment is not cleared"
    );
    assert!(
        fixture.root.path().join("h/stopped").exists(),
        "the helper was not sent SIGTERM"
    );
    assert!(!alive(&helper["pid"]), "the helper was not reaped");
    assert!(
        fixture.push_entries().is_empty(),
        "the launch directory was left behind"
    );
    let log = fs::read_to_string(spec.run.join(LOG_NAME)).unwrap();
    assert!(log.contains("helper started"), "{log}");
    let mode = fs::metadata(spec.run.join(LOG_NAME))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o7777, 0o600);
}

#[test]
fn the_launch_directory_has_the_documented_owners_and_modes() {
    if skip_without_tmp("the_launch_directory_has_the_documented_owners_and_modes") {
        return;
    }
    let fixture = Fixture::new("bind", 0);
    let owners = Owners {
        supervisor: accounts::effective_uid(),
        helper: (Account::current().uid, Account::current().gid),
        implementer_group: Account::current().gid,
    };
    let root = fixture.config.state_dir.join("push");
    let directory = LaunchDir::create(&root, "l1", &owners).unwrap();
    let mode = |path: PathBuf| fs::metadata(path).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode(root.clone()), 0o711);
    assert_eq!(mode(root.join("l1")), 0o711);
    assert_eq!(mode(root.join("l1/sock")), 0o2750);
    assert_eq!(mode(root.join("l1/work")), 0o700);
    assert_eq!(directory.socket(), root.join("l1/sock/push.sock"));
    directory.remove().unwrap();
    assert!(!root.join("l1").exists());
}

#[test]
fn a_failing_launch_still_stops_the_helper_and_returns_its_code() {
    if skip_without_tmp("a_failing_launch_still_stops_the_helper_and_returns_its_code") {
        return;
    }
    let fixture = Fixture::new("bind", 3);
    assert_eq!(run(&fixture.plan()).unwrap(), 3);
    let helper = fixture.record("helper").unwrap();
    assert!(fixture.root.path().join("h/stopped").exists());
    assert!(!alive(&helper["pid"]));
    assert!(fixture.push_entries().is_empty());
}

#[test]
fn a_launch_that_cannot_start_still_stops_the_helper_and_cleans_up() {
    if skip_without_tmp("a_launch_that_cannot_start_still_stops_the_helper_and_cleans_up") {
        return;
    }
    let fixture = Fixture::new("bind", 0);
    fs::remove_file(crate::relay::program(&fixture.config)).unwrap();
    let error = run(&fixture.plan()).unwrap_err();
    assert!(
        format!("{error:#}").contains("agentc-supervisor"),
        "{error:#}"
    );
    let helper = fixture.record("helper").unwrap();
    assert!(fixture.root.path().join("h/stopped").exists());
    assert!(!alive(&helper["pid"]));
    assert!(fixture.push_entries().is_empty());
}

#[test]
fn a_helper_that_exits_early_fails_the_launch_before_it_runs() {
    if skip_without_tmp("a_helper_that_exits_early_fails_the_launch_before_it_runs") {
        return;
    }
    let fixture = Fixture::new("exit", 0);
    let error = run(&fixture.plan()).unwrap_err();
    assert!(
        format!("{error:#}").contains("exited before serving"),
        "{error:#}"
    );
    assert!(
        fixture.record("launch").is_none(),
        "the launch ran without a helper"
    );
    assert!(fixture.push_entries().is_empty());
}

#[test]
fn a_helper_that_never_publishes_its_socket_times_out_and_is_stopped() {
    if skip_without_tmp("a_helper_that_never_publishes_its_socket_times_out_and_is_stopped") {
        return;
    }
    let fixture = Fixture::new("silent", 0);
    let plan = Plan {
        start_timeout: Duration::from_millis(500),
        ..fixture.plan()
    };
    let error = run(&plan).unwrap_err();
    assert!(
        format!("{error:#}").contains("did not publish its socket"),
        "{error:#}"
    );
    assert!(fixture.record("launch").is_none());
    let helper = fixture.record("helper").unwrap();
    assert!(!alive(&helper["pid"]), "the helper was left running");
    assert!(fixture.push_entries().is_empty());
}

#[test]
fn a_helper_ignoring_sigterm_is_killed_after_the_stop_timeout() {
    if skip_without_tmp("a_helper_ignoring_sigterm_is_killed_after_the_stop_timeout") {
        return;
    }
    let fixture = Fixture::new("stubborn", 0);
    let plan = Plan {
        stop_timeout: Duration::from_millis(300),
        ..fixture.plan()
    };
    assert_eq!(run(&plan).unwrap(), 0);
    let helper = fixture.record("helper").unwrap();
    assert!(!fixture.root.path().join("h/stopped").exists());
    assert!(!alive(&helper["pid"]), "the helper was not killed");
    assert!(fixture.push_entries().is_empty());
}

#[test]
fn a_reviewer_runs_without_a_helper() {
    if skip_without_tmp("a_reviewer_runs_without_a_helper") {
        return;
    }
    let mut fixture = Fixture::new("bind", 0);
    let role = fixture.config.state_dir.join("rev");
    fixture.spec.role = Role::Reviewer;
    fixture.spec.run = role.join("runs/r");
    fixture.spec.clone = role.join("clones/c");
    fixture.spec.task = None;
    assert_eq!(run(&fixture.plan()).unwrap(), 0);
    assert!(
        fixture.record("helper").is_none(),
        "a reviewer got a helper"
    );
    let launch = fixture.record("launch").unwrap();
    let args = arguments(&launch);
    assert!(args.contains(&"--role=reviewer".to_owned()));
    assert!(
        !args.iter().any(|arg| arg.starts_with("--push-socket")),
        "{args:?}"
    );
    assert!(!fixture.config.state_dir.join("push").exists());
}

#[test]
fn launch_root_assigns_the_socket_itself_and_needs_a_safe_task() {
    if skip_without_tmp("launch_root_assigns_the_socket_itself_and_needs_a_safe_task") {
        return;
    }
    let mut fixture = Fixture::new("bind", 0);
    fixture.spec.push_socket = Some("/tmp/other.sock".into());
    assert!(format!("{:#}", run(&fixture.plan()).unwrap_err()).contains("--push-socket"));
    fixture.spec.push_socket = None;
    for task in [None, Some(""), Some("-x"), Some("a/b"), Some(".hidden")] {
        fixture.spec.task = task.map(str::to_owned);
        let error = run(&fixture.plan()).unwrap_err();
        assert!(
            format!("{error:#}").contains("--task"),
            "{task:?}: {error:#}"
        );
    }
    assert!(fixture.record("helper").is_none());
}

#[test]
fn launch_root_requires_root() {
    assert!(accounts::require_root(1000).is_err());
    assert!(accounts::require_root(0).is_ok());
    if skip_without_tmp("launch_root_requires_root") {
        return;
    }
    let fixture = Fixture::new("bind", 0);
    if accounts::effective_uid() != 0 {
        let error = launch_root(&fixture.spec, None, &fixture.config).unwrap_err();
        assert!(
            format!("{error:#}").contains("must run as root"),
            "{error:#}"
        );
        assert!(fixture.record("helper").is_none());
    }
}

#[test]
fn host_problems_name_a_missing_helper_program_configuration_and_account() {
    if skip_without_tmp("host_problems_name_a_missing_helper_program_configuration_and_account") {
        return;
    }
    let mut fixture = Fixture::new("bind", 0);
    fixture.config.push_helper.user = "agentc-no-such-user".into();
    fs::remove_file(&fixture.config.push_helper.program).unwrap();
    let problems = host_problems(&fixture.spec, &fixture.config).join("\n");
    assert!(problems.contains("push helper:"), "{problems}");
    assert!(problems.contains("push helper configuration"), "{problems}");
    assert!(problems.contains("push helper account"), "{problems}");
    fixture.spec.role = Role::Reviewer;
    let problems = host_problems(&fixture.spec, &fixture.config).join("\n");
    assert!(!problems.contains("push helper"), "{problems}");
}

#[test]
fn a_project_with_its_own_helper_configuration_is_served_with_it() {
    if skip_without_tmp("a_project_with_its_own_helper_configuration_is_served_with_it") {
        return;
    }
    let mut fixture = Fixture::new("bind", 0);
    let own = fixture.root.path().join("push-canary.toml");
    let project = fixture.spec.project.clone().unwrap();
    (fixture.config.push_helper.project_configs).insert(project, own.clone());
    // The shared configuration is not what this project needs.
    let problems = host_problems(&fixture.spec, &fixture.config).join("\n");
    assert!(problems.contains("push-canary.toml"), "{problems}");
    assert!(!problems.contains("/push.toml"), "{problems}");
    let coordinator = fixture.spec.run.join("state/coordinator");
    fs::create_dir_all(&coordinator).unwrap();
    fs::write(coordinator.join("credentials.toml"), "").unwrap();
    assert_eq!(run(&fixture.plan()).unwrap(), 0);
    let helper = fixture.record("helper").unwrap();
    assert_eq!(arguments(&helper)[1], format!("--config={}", own.display()));
}

#[test]
fn socket_paths_the_helper_cannot_bind_are_refused_before_anything_starts() {
    let at_limit = PathBuf::from(format!("/{}", "a".repeat(MAX_SOCKET_PATH - 1)));
    assert!(check_socket_path(&at_limit).is_ok());
    let over = PathBuf::from(format!("/{}", "a".repeat(MAX_SOCKET_PATH)));
    assert!(check_socket_path(&over).is_err());
    assert!(check_socket_path(Path::new("relative/push.sock")).is_err());
    if skip_without_tmp("socket_paths_the_helper_cannot_bind_are_refused_before_anything_starts") {
        return;
    }
    let mut fixture = Fixture::new("bind", 0);
    let long = fixture.root.path().join("s".repeat(40));
    fs::rename(&fixture.config.state_dir, &long).unwrap();
    fixture.config.state_dir = long.clone();
    fixture.spec.run = long.join("impl/runs/r");
    fixture.spec.clone = long.join("impl/clones/c");
    let error = run(&fixture.plan()).unwrap_err();
    assert!(format!("{error:#}").contains("93 bytes"), "{error:#}");
    assert!(fixture.record("helper").is_none());
    assert!(
        !long.join("push").exists(),
        "created a directory for an unusable path"
    );
}

#[test]
fn stale_launch_directories_are_swept_only_when_owned_unlocked_directories() {
    let root = tempfile::tempdir().unwrap();
    let owner = accounts::effective_uid();
    let stale = root.path().join("stale");
    fs::create_dir_all(stale.join("sock")).unwrap();
    fs::write(stale.join(".lock"), "").unwrap();
    let _listener = std::os::unix::net::UnixListener::bind(stale.join("sock/push.sock")).unwrap();
    let live = root.path().join("live");
    fs::create_dir(&live).unwrap();
    let held = fs::File::create(live.join(".lock")).unwrap();
    held.lock().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join(".lock"), "").unwrap();
    fs::write(outside.path().join("keep"), "").unwrap();
    symlink(outside.path(), root.path().join("linked")).unwrap();
    for name in [".new-x", ".other", "unlocked-but-no-lock"] {
        fs::create_dir(root.path().join(name)).unwrap();
    }
    fs::write(root.path().join(".new-x/.lock"), "").unwrap();
    fs::write(root.path().join("file"), "").unwrap();
    layout::sweep_stale(root.path(), owner).unwrap();
    let mut left: Vec<_> = fs::read_dir(root.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    left.sort();
    assert_eq!(
        left,
        [".other", "file", "linked", "live", "unlocked-but-no-lock"]
    );
    assert!(outside.path().join("keep").exists(), "followed a symlink");
    layout::sweep_stale(root.path(), owner + 1).unwrap();
    drop(held);
    layout::sweep_stale(root.path(), owner + 1).unwrap();
    assert!(live.exists(), "removed a directory another account owns");
    layout::sweep_stale(root.path(), owner).unwrap();
    assert!(!live.exists(), "kept a directory whose lock was released");
}

#[test]
fn a_new_launch_sweeps_what_a_killed_one_left() {
    if skip_without_tmp("a_new_launch_sweeps_what_a_killed_one_left") {
        return;
    }
    let fixture = Fixture::new("bind", 0);
    let stale = fixture.config.state_dir.join("push/old-launch");
    fs::create_dir_all(stale.join("sock")).unwrap();
    fs::set_permissions(stale.parent().unwrap(), fs::Permissions::from_mode(0o711)).unwrap();
    fs::write(stale.join(".lock"), "").unwrap();
    let _listener = std::os::unix::net::UnixListener::bind(stale.join("sock/push.sock")).unwrap();
    assert_eq!(run(&fixture.plan()).unwrap(), 0);
    assert!(
        fixture.push_entries().is_empty(),
        "{:?}",
        fixture.push_entries()
    );
}

#[test]
fn credential_digests_follow_no_symlink_and_never_quote_tokens() {
    let base = tempfile::tempdir().unwrap();
    let owner = Account::current().uid;
    let one = Path::new("runs/r/state/coordinator/credentials.toml");
    let two = Path::new("coordinator/credentials.toml");
    let missing = Path::new("runs/none/credentials.toml");
    fs::create_dir_all(base.path().join(one).parent().unwrap()).unwrap();
    fs::create_dir_all(base.path().join(two).parent().unwrap()).unwrap();
    let entry = |token: &str| format!("[[credentials]]\norigin = \"o\"\ntoken = \"{token}\"\n");
    fs::write(base.path().join(one), entry("alpha") + &entry("beta")).unwrap();
    fs::write(base.path().join(two), entry("alpha")).unwrap();
    let digests = files::credential_digests(base.path(), &[one, two, missing], owner).unwrap();
    let mut expected = ["alpha", "beta"].map(|t| hex::encode(Sha256::digest(t.as_bytes())));
    expected.sort();
    assert_eq!(digests, expected);
    fs::write(base.path().join(two), "token = \"gamma-secret\" [").unwrap();
    let error = files::credential_digests(base.path(), &[two], owner).unwrap_err();
    assert!(!format!("{error:#}").contains("gamma"), "{error:#}");
    let target = tempfile::tempdir().unwrap();
    fs::write(target.path().join("credentials.toml"), entry("delta")).unwrap();
    fs::remove_dir_all(base.path().join("runs/r/state/coordinator")).unwrap();
    symlink(target.path(), base.path().join("runs/r/state/coordinator")).unwrap();
    assert!(
        files::credential_digests(base.path(), &[one], owner).is_err(),
        "followed a directory symlink"
    );
    let fifo = base.path().join("coordinator/fifo");
    let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: `name` is a NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let error = files::credential_digests(base.path(), &[Path::new("coordinator/fifo")], owner);
    assert!(error.is_err(), "read a FIFO");
    fs::write(base.path().join(two), entry("alpha")).unwrap();
    assert!(files::credential_digests(base.path(), &[two], owner).is_ok());
    let foreign = files::credential_digests(base.path(), &[two], owner + 1);
    assert!(foreign.is_err(), "read a file another account owns");
}

#[test]
fn the_helper_log_is_new_and_never_follows_a_planted_link() {
    let base = tempfile::tempdir().unwrap();
    fs::create_dir_all(base.path().join("runs/r")).unwrap();
    let log = Path::new("runs/r/push-helper.log");
    drop(files::helper_log(base.path(), log).unwrap());
    assert!(
        files::helper_log(base.path(), log).is_err(),
        "reused an existing log"
    );
    let target = tempfile::tempdir().unwrap();
    symlink(
        target.path().join("planted"),
        base.path().join("runs/r/linked.log"),
    )
    .unwrap();
    assert!(files::helper_log(base.path(), Path::new("runs/r/linked.log")).is_err());
    assert!(
        !target.path().join("planted").exists(),
        "followed a file symlink"
    );
    symlink(target.path(), base.path().join("runs/moved")).unwrap();
    assert!(files::helper_log(base.path(), Path::new("runs/moved/push-helper.log")).is_err());
    assert!(
        !target.path().join("push-helper.log").exists(),
        "followed a directory symlink"
    );
}

#[test]
fn a_vanished_entry_is_not_stale_and_concurrent_launches_never_break_each_other() {
    let owner = accounts::effective_uid();
    if skip_without_tmp(
        "a_vanished_entry_is_not_stale_and_concurrent_launches_never_break_each_other",
    ) {
        return;
    }
    let root = tempfile::tempdir_in("/tmp").unwrap();
    assert!(!layout::is_stale(&root.path().join("gone"), owner).unwrap());
    let push = root.path().join("push");
    let me = Account::current();
    let owners = Owners {
        supervisor: owner,
        helper: (me.uid, me.gid),
        implementer_group: me.gid,
    };
    for round in 0..20 {
        let stale = push.join(format!("stale-{round}"));
        fs::create_dir_all(&stale).unwrap();
        fs::write(stale.join(".lock"), "").unwrap();
        let built: Vec<LaunchDir> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|n| {
                    let (push, owners) = (&push, &owners);
                    scope.spawn(move || LaunchDir::create(push, &format!("r{round}-{n}"), owners))
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap().unwrap())
                .collect()
        });
        assert!(
            !stale.exists(),
            "round {round}: the stale directory survived"
        );
        for directory in built {
            assert!(directory.socket().parent().unwrap().is_dir());
            directory.remove().unwrap();
        }
    }
}

#[test]
fn racing_first_launches_both_accept_the_push_root() {
    let owner = accounts::effective_uid();
    for _ in 0..20 {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("push");
        let results: Vec<_> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| layout::prepare_root(&root, owner)))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for result in results {
            result.unwrap();
        }
        let mode = fs::metadata(&root).unwrap().permissions().mode();
        assert_eq!(mode & 0o7777, 0o711);
    }
}
