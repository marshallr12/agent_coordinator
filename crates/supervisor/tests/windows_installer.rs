//! `agentc-supervisor windows-host install` and `status` on a real machine
//! (phase 7, step 7a), run as the built binary.
//!
//! The tests that only run `status` change nothing and run anywhere. The
//! one that installs **registers four services** with the Service Control
//! Manager, so it is ignored and also refuses to run unless
//! `AGENTC_INSTALLER_REGISTERS_SERVICES=1` is set: a machine's owner must
//! ask for it. It needs an elevated session. It installs beside any real
//! host, never over it: a random service prefix and a temporary program
//! and state tree. The services are never started, the key is synthetic,
//! and the services and the tree are removed when the test ends.
#![cfg(windows)]

use agentc_supervisor::host_runtime::windows::installer::tokens;
use agentc_supervisor::host_runtime::windows::scm;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// The variable an owner sets to let the installing test register services.
const GATE: &str = "AGENTC_INSTALLER_REGISTERS_SERVICES";
/// The body of the synthetic key: no output may ever contain it.
const KEY_BODY: &str = "c3ludGhldGljLWtleS1mb3ItdGFzay03YS1uZXZlci1hLXJlYWwta2V5";
/// The four roles, in start order.
const ROLES: [&str; 4] = ["egress", "publication", "runner", "supervisor"];
/// A remote for the configuration; nothing ever contacts it.
const REMOTE: &str = "https://github.com/example/project.git";

/// One throwaway installation and everything the binary printed for it.
struct Fixture {
    dir: tempfile::TempDir,
    prefix: String,
    printed: String,
}

/// What one run of the binary ended with.
struct Run {
    code: i32,
    report: Value,
    errors: String,
}

impl Fixture {
    /// A new installation's place; nothing is made but a temporary directory.
    fn new() -> Self {
        let id = uuid::Uuid::new_v4().simple().to_string();
        Self {
            dir: tempfile::tempdir().unwrap(),
            prefix: format!("agentc-t{}", &id[..10]),
            printed: String::new(),
        }
    }

    /// `rest` below the temporary directory.
    fn path(&self, rest: &str) -> PathBuf {
        self.dir.path().join(rest)
    }

    /// `rest` below the temporary directory, as text.
    fn text(&self, rest: &str) -> String {
        self.path(rest).display().to_string()
    }

    /// The name of `role`'s service.
    fn service(&self, role: &str) -> String {
        format!("{}-{role}", self.prefix)
    }

    /// Runs `windows-host <verb>` on this installation with `more` arguments.
    fn run(&mut self, verb: &str, more: &[&str]) -> Run {
        let mut command = Command::new(env!("CARGO_BIN_EXE_agentc-supervisor"));
        command.args(["windows-host", verb, "--program-dir", &self.text("program")]);
        command.args([
            "--state-dir",
            &self.text("state"),
            "--service-prefix",
            &self.prefix,
        ]);
        let output = command.args(more).output().unwrap();
        let out = String::from_utf8_lossy(&output.stdout).into_owned();
        let errors = String::from_utf8_lossy(&output.stderr).into_owned();
        self.printed.push_str(&format!("{out}\n{errors}\n"));
        let report = serde_json::from_str(&out).unwrap_or(Value::Null);
        Run {
            code: output.status.code().unwrap_or(-1),
            report,
            errors,
        }
    }

    /// Writes the synthetic key file and returns its path.
    fn key_file(&self) -> String {
        let text = format!("synthetic key file of step 7a\n{KEY_BODY}\nend of key\n");
        std::fs::write(self.path("downloaded.pem"), text).unwrap();
        self.text("downloaded.pem")
    }

    /// Whether any of the four services is registered.
    fn any_registered(&self) -> bool {
        ROLES
            .iter()
            .any(|role| sc(&["query", &self.service(role)]).0 == 0)
    }
}

impl Drop for Fixture {
    /// Deletes whatever services the test registered; the temporary
    /// directory removes the tree.
    fn drop(&mut self) {
        for role in ROLES {
            sc(&["delete", &self.service(role)]);
        }
    }
}

/// Runs `sc.exe`; its exit code and what it printed.
fn sc(arguments: &[&str]) -> (i32, String) {
    let output = Command::new("sc.exe").args(arguments).output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    (output.status.code().unwrap_or(-1), text)
}

/// What `icacls` prints for `path`.
fn icacls(path: &Path) -> String {
    let output = Command::new("icacls").arg(path).output().unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The `action` of every change in `list`.
fn actions(list: &Value) -> Vec<String> {
    let action = |change: &Value| change["action"].as_str().unwrap_or("?").to_owned();
    list.as_array()
        .map(|changes| changes.iter().map(action).collect())
        .unwrap_or_default()
}

/// Whether this session is elevated; says so when a test is skipped.
fn elevated(test: &str) -> bool {
    let elevated = tokens::elevated().unwrap();
    if !elevated {
        eprintln!("{test}: this session is not elevated; only the refusal was checked");
    }
    elevated
}

#[test]
fn status_refuses_a_session_that_is_not_elevated_and_reports_no_host_otherwise() {
    let mut fixture = Fixture::new();
    let run = fixture.run("status", &[]);
    assert_eq!(run.code, 1, "{}", run.errors);
    let refusal = run.report["refusals"][0].as_str().unwrap().to_owned();
    match elevated("status") {
        true => assert!(refusal.starts_with("nothing is installed"), "{refusal}"),
        false => assert!(refusal.contains("not elevated"), "{refusal}"),
    }
    assert!(!fixture.path("state").exists() && !fixture.path("program").exists());
}

/// Makes `rest` a junction to a new, empty directory beside it.
fn junction(fixture: &Fixture, rest: &str) {
    let target = fixture.path(&format!("{rest}-elsewhere"));
    std::fs::create_dir(&target).unwrap();
    let mut link = Command::new("cmd");
    link.args(["/C", "mklink", "/J"]);
    link.arg(fixture.path(rest)).arg(&target);
    assert!(link.output().unwrap().status.success());
}

#[test]
fn status_refuses_a_state_directory_that_is_a_junction() {
    let mut fixture = Fixture::new();
    junction(&fixture, "state");
    let run = fixture.run("status", &[]);
    assert_eq!(run.code, 1, "{}", run.errors);
    if elevated("junction") {
        let refusal = run.report["refusals"][0].as_str().unwrap();
        assert!(refusal.ends_with("is a link or reparse point"), "{refusal}");
    }
}

#[test]
fn status_refuses_a_state_directory_this_session_does_not_trust() {
    // A directory made here without elevation belongs to this account; made
    // with elevation it is given to an account that is neither the system
    // nor administrators.
    let mut fixture = Fixture::new();
    std::fs::create_dir(fixture.path("state")).unwrap();
    if !elevated("owner") {
        return;
    }
    let mut give = Command::new("icacls");
    give.arg(fixture.path("state"))
        .args(["/setowner", "*S-1-5-19"]);
    assert!(give.output().unwrap().status.success());
    let run = fixture.run("status", &[]);
    assert_eq!(run.code, 1, "{}", run.errors);
    let refusal = run.report["refusals"][0].as_str().unwrap();
    assert!(refusal.contains("is owned by S-1-5-19"), "{refusal}");
}

/// Stops the installing test unless its owner asked for it.
fn asked_for() -> bool {
    let asked = std::env::var(GATE).is_ok_and(|value| value == "1");
    if !asked {
        eprintln!("skipped: set {GATE}=1 to let this test register four services");
    }
    asked
}

/// The first install's arguments.
fn first_install(fixture: &Fixture) -> Vec<String> {
    let key = fixture.key_file();
    [
        "--remote",
        REMOTE,
        "--app-id",
        "7",
        "--installation-id",
        "9",
        "--key-source",
        &key,
    ]
    .map(String::from)
    .into()
}

/// Checks that `install` refuses each bad host and registers nothing.
fn refusals_change_nothing() {
    let mut fixture = Fixture::new();
    let arguments = first_install(&fixture);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    let mut missing_key = arguments.clone();
    *missing_key.last_mut().unwrap() = r"C:\no\such\key.pem";
    let run = fixture.run("install", &missing_key);
    assert_eq!(run.code, 1, "{}", run.errors);
    let refusal = run.report["refusals"][0].as_str().unwrap().to_owned();
    assert!(
        refusal.starts_with("the key file cannot be used"),
        "{refusal}"
    );
    assert!(!fixture.path("state").exists() && !fixture.path("program").exists());
    std::fs::create_dir(fixture.path("state")).unwrap();
    let mut give = Command::new("icacls");
    give.arg(fixture.path("state"))
        .args(["/setowner", "*S-1-5-19"]);
    assert!(give.output().unwrap().status.success());
    let run = fixture.run("install", &arguments);
    assert_eq!(run.code, 1, "{}", run.errors);
    assert!(
        run.report["refusals"][0]
            .as_str()
            .unwrap()
            .contains("is owned by")
    );
    assert_eq!(actions(&run.report["applied"]), Vec::<String>::new());
    assert!(!fixture.any_registered() && !fixture.path("program").exists());
    assert_eq!(std::fs::read_dir(fixture.path("state")).unwrap().count(), 0);
    assert!(
        !fixture.printed.contains(KEY_BODY),
        "a refusal printed the key"
    );
}

/// Checks that `install` refuses a program or a state directory that is a
/// junction, registers nothing and makes nothing through the link.
fn junctions_are_refused() {
    for (rest, other) in [("program", "state"), ("state", "program")] {
        let mut fixture = Fixture::new();
        junction(&fixture, rest);
        let arguments = first_install(&fixture);
        let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
        let run = fixture.run("install", &arguments);
        assert_eq!(run.code, 1, "{rest}: {}", run.errors);
        let refusal = run.report["refusals"][0].as_str().unwrap().to_owned();
        assert!(refusal.ends_with("is a link or reparse point"), "{refusal}");
        assert_eq!(actions(&run.report["applied"]), Vec::<String>::new());
        assert!(!fixture.any_registered() && !fixture.path(other).exists());
        let beyond = fixture.path(&format!("{rest}-elsewhere"));
        assert_eq!(std::fs::read_dir(beyond).unwrap().count(), 0, "{rest}");
        assert!(
            !fixture.printed.contains(KEY_BODY),
            "a refusal printed the key"
        );
    }
}

/// Checks the four registrations against what `sc.exe` itself prints.
fn registered_as_designed(fixture: &Fixture) {
    for role in ROLES {
        let name = fixture.service(role);
        let (_, config) = sc(&["qc", &name]);
        assert!(
            config.contains("DEMAND_START") && config.contains("WIN32_OWN_PROCESS"),
            "{config}"
        );
        assert!(config.contains(&format!(r"NT SERVICE\{name}")), "{config}");
        let binary = format!(
            r#""{}" --config "{}" windows-service"#,
            fixture.text(r"program\agentc-supervisor.exe"),
            fixture.text(r"program\supervisor.toml")
        );
        assert!(config.contains(&binary), "{config}");
        assert_eq!(
            config.contains(&format!("--role {role}")),
            role != "runner",
            "{config}"
        );
        let waits = |line: &&str| line.trim_start().starts_with("DEPENDENCIES");
        let depends = config.lines().find(waits).unwrap_or_default();
        let depends = depends.contains(&fixture.service("runner"));
        assert_eq!(depends, role == "supervisor", "{config}");
        assert!(
            sc(&["query", &name]).1.contains("STOPPED"),
            "{name} must be registered stopped"
        );
        let (_, failure) = sc(&["qfailure", &name]);
        assert!(failure.contains("86400"), "{failure}");
        assert_eq!(
            failure.matches("RESTART -- Delay = 60000").count(),
            2,
            "{failure}"
        );
        assert!(!failure.contains(".exe"), "no failure program: {failure}");
        let (_, privileges) = sc(&["qprivs", &name]);
        assert!(
            privileges.contains("SeChangeNotifyPrivilege"),
            "{privileges}"
        );
        assert!(
            privileges.contains("SeImpersonatePrivilege"),
            "{privileges}"
        );
        assert_eq!(privileges.matches("Privilege").count(), 2, "{privileges}");
        assert_eq!(
            scm::preshutdown_timeout(&name).unwrap(),
            Duration::from_secs(120)
        );
    }
}

/// Checks the tree and the key's access list against what `icacls` prints.
fn tree_as_designed(fixture: &Fixture) {
    for rest in [
        "installer",
        "runner",
        r"runner\stand-in-launches",
        "supervisor",
        "stand-in",
        r"stand-in\queue",
        "egress",
        "publication",
        "publication-key",
    ] {
        assert!(fixture.path("state").join(rest).is_dir(), "{rest}");
    }
    let key = icacls(&fixture.path(r"state\publication-key\push-app.pem"));
    let publication = format!(r"NT SERVICE\{}:(R)", fixture.service("publication"));
    assert!(key.contains(&publication), "{key}");
    assert!(
        key.contains(r"NT AUTHORITY\SYSTEM:(F)") && key.contains(r"BUILTIN\Administrators:(F)"),
        "{key}"
    );
    assert_eq!(
        key.matches(":(").count(),
        3,
        "three entries and no more: {key}"
    );
    let root = icacls(&fixture.path("state"));
    assert_eq!(
        root.matches(":(").count(),
        2,
        "the system and administrators only: {root}"
    );
    let runner = icacls(&fixture.path(r"state\runner"));
    assert!(
        runner.contains(&format!(
            r"NT SERVICE\{}:(OI)(CI)(M)",
            fixture.service("runner")
        )),
        "{runner}"
    );
    let egress = icacls(&fixture.path(r"state\egress"));
    assert!(
        egress.contains(&format!(
            r"NT SERVICE\{}:(OI)(CI)(F)",
            fixture.service("egress")
        )),
        "{egress}"
    );
}

/// Drifts a service setting, an access list and a configuration entry.
fn drift(fixture: &Fixture) {
    let (code, said) = sc(&["config", &fixture.service("runner"), "start=", "auto"]);
    assert_eq!(code, 0, "{said}");
    let mut grant = Command::new("icacls");
    grant
        .arg(fixture.path(r"state\egress"))
        .args(["/grant", "*S-1-1-0:(OI)(CI)RX"]);
    assert!(grant.output().unwrap().status.success());
    let config = fixture.path(r"program\supervisor.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(text.contains(REMOTE), "{text}");
    std::fs::write(&config, text.replace("example/project", "example/other")).unwrap();
}

/// Checks that `status` names the three drifts and changes none of them,
/// and that `install` repairs exactly those.
fn drift_is_reported_then_repaired(fixture: &mut Fixture) {
    let expected = ["set_access_list", "write_config", "set_start_type"];
    let status = fixture.run("status", &[]);
    assert_eq!(status.code, 1, "{}", status.errors);
    assert_eq!(
        actions(&status.report["pending"]),
        expected,
        "{}",
        status.report
    );
    let entries = &status.report["pending"][1]["entries"];
    assert_eq!(
        entries[0], "windows_publication.remote",
        "{}",
        status.report
    );
    assert!(
        sc(&["qc", &fixture.service("runner")])
            .1
            .contains("AUTO_START"),
        "status changed it"
    );
    assert!(
        icacls(&fixture.path(r"state\egress")).contains("Everyone"),
        "status changed it"
    );
    let repair = fixture.run("install", &[]);
    assert_eq!(repair.code, 0, "{}", repair.errors);
    assert_eq!(
        actions(&repair.report["applied"]),
        expected,
        "{}",
        repair.report
    );
    assert!(
        sc(&["qc", &fixture.service("runner")])
            .1
            .contains("DEMAND_START")
    );
    assert!(!icacls(&fixture.path(r"state\egress")).contains("Everyone"));
    assert_eq!(fixture.run("status", &[]).code, 0);
}

/// Drifts a command line, a privilege list, an account, a recovery
/// setting and a dependency, each with `sc.exe`. No service is started,
/// so none of it ever takes effect.
fn drift_services(fixture: &Fixture) {
    let (egress, publication) = (fixture.service("egress"), fixture.service("publication"));
    let (runner, supervisor) = (fixture.service("runner"), fixture.service("supervisor"));
    let program = r"C:\Windows\System32\cmd.exe /c exit";
    let failure = [
        "reset=",
        "60",
        "actions=",
        "restart/1000",
        "command=",
        program,
    ];
    let commands: [Vec<&str>; 5] = [
        vec!["config", &egress, "binPath=", program],
        vec!["privs", &egress, "SeChangeNotifyPrivilege/SeDebugPrivilege"],
        vec!["config", &publication, "obj=", "LocalSystem"],
        [&["failure", &runner][..], &failure[..]].concat(),
        vec!["config", &supervisor, "depend=", "/"],
    ];
    for command in commands {
        let (code, said) = sc(&command);
        assert_eq!(code, 0, "{command:?}: {said}");
    }
}

/// Checks that `status` names each drifted service setting and that
/// `install` repairs exactly those, back to the designed registration.
fn service_drift_is_reported_then_repaired(fixture: &mut Fixture) {
    let expected = [
        "set_bin_path",
        "set_privileges",
        "set_account",
        "set_recovery",
        "set_dependencies",
    ];
    let status = fixture.run("status", &[]);
    assert_eq!(status.code, 1, "{}", status.errors);
    let pending = actions(&status.report["pending"]);
    assert_eq!(pending, expected, "{}", status.report);
    let repair = fixture.run("install", &[]);
    assert_eq!(repair.code, 0, "{} {}", repair.errors, repair.report);
    let applied = actions(&repair.report["applied"]);
    assert_eq!(applied, expected, "{}", repair.report);
    registered_as_designed(fixture);
    assert_eq!(fixture.run("status", &[]).code, 0);
}

/// Checks that nothing printed or written holds the synthetic key.
fn nothing_holds_the_key(fixture: &Fixture) {
    assert!(!fixture.printed.contains(KEY_BODY), "the key was printed");
    for rest in [
        r"state\installer\journal.jsonl",
        r"state\installer\manifest.json",
        r"program\supervisor.toml",
    ] {
        let text = std::fs::read_to_string(fixture.path(rest)).unwrap();
        assert!(!text.contains(KEY_BODY), "{rest} holds the key");
        assert!(!text.is_empty(), "{rest}");
    }
    let installed = std::fs::read_to_string(fixture.path(r"state\publication-key\push-app.pem"));
    assert!(
        installed.unwrap().contains(KEY_BODY),
        "the key itself was copied"
    );
    assert!(
        fixture.path("downloaded.pem").exists(),
        "the key file is copied, not moved"
    );
}

/// Registers four throwaway services, so it runs only when the machine's
/// owner asks: `AGENTC_INSTALLER_REGISTERS_SERVICES=1 cargo test -p
/// agentc-supervisor --test windows_installer -- --ignored`.
#[test]
#[ignore = "registers four services; needs the owner's consent and an elevated session"]
fn install_registers_the_host_once_and_repairs_only_what_drifts() {
    if !asked_for() {
        return;
    }
    assert!(
        tokens::elevated().unwrap(),
        "this test needs an elevated session"
    );
    refusals_change_nothing();
    junctions_are_refused();
    let mut fixture = Fixture::new();
    let arguments = first_install(&fixture);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    let first = fixture.run("install", &arguments);
    assert_eq!(first.code, 0, "{}\n{}", first.errors, first.report);
    assert_eq!(
        actions(&first.report["applied"]).len(),
        31,
        "{}",
        first.report
    );
    let stopped = |token: &Value| token["state"] == "stopped";
    assert!(
        first.report["tokens"]
            .as_array()
            .unwrap()
            .iter()
            .all(stopped),
        "{}",
        first.report
    );
    registered_as_designed(&fixture);
    tree_as_designed(&fixture);
    let second = fixture.run("install", &[]);
    assert_eq!(second.code, 0, "{}", second.errors);
    assert_eq!(
        actions(&second.report["applied"]),
        Vec::<String>::new(),
        "{}",
        second.report
    );
    assert_eq!(fixture.run("status", &[]).code, 0);
    drift(&fixture);
    drift_is_reported_then_repaired(&mut fixture);
    drift_services(&fixture);
    service_drift_is_reported_then_repaired(&mut fixture);
    nothing_holds_the_key(&fixture);
}
