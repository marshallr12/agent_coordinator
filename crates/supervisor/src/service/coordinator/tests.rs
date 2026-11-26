use super::*;
use crate::config::Config;
use crate::run_loop::Suggestion;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::Mutex;

/// A coordinator whose `next` and task details are fixed, recording the
/// projects it was asked about.
#[derive(Default)]
struct FakeApi {
    next: Value,
    detail: Option<Value>,
    asked: Mutex<Vec<String>>,
}

impl Api for FakeApi {
    fn next(&self, project: &str, exclude: &[String]) -> Result<Value> {
        let asked = format!("next {project} {}", exclude.join(","));
        self.asked.lock().unwrap().push(asked);
        Ok(self.next.clone())
    }

    fn task(&self, project: &str, task: &str) -> Result<Value> {
        self.asked
            .lock()
            .unwrap()
            .push(format!("task {project} {task}"));
        self.detail.clone().context("the detail could not be read")
    }
}

/// A CLI that answers each call with the next queued output (success with
/// `{}` when none is left) and records the calls.
#[derive(Default)]
struct FakeCli {
    answers: RefCell<VecDeque<CliOutput>>,
    calls: RefCell<Vec<(String, Vec<String>, String)>>,
}

impl FakeCli {
    fn answering(answers: Vec<CliOutput>) -> Self {
        Self {
            answers: RefCell::new(answers.into()),
            ..Self::default()
        }
    }

    /// The commands run, by their first argument.
    fn commands(&self) -> Vec<String> {
        self.calls.borrow().iter().map(|c| c.1[0].clone()).collect()
    }
}

impl Cli for FakeCli {
    fn run(&self, session: &str, args: &[String], input: &[u8]) -> Result<CliOutput> {
        let input = String::from_utf8_lossy(input).into_owned();
        (self.calls.borrow_mut()).push((session.to_owned(), args.to_vec(), input));
        let ok = || ok(json!({}));
        Ok(self.answers.borrow_mut().pop_front().unwrap_or_else(ok))
    }
}

fn ok(body: Value) -> CliOutput {
    CliOutput {
        code: Some(0),
        stdout: serde_json::to_vec(&body).unwrap(),
        stderr: Vec::new(),
    }
}

fn refused(code: i32, error: &str) -> CliOutput {
    let body = json!({"error": {"code": error, "message": "refused"}});
    CliOutput {
        code: Some(code),
        ..ok(body)
    }
}

/// A `claim --json` answer for attempt `a1` at `generation`.
fn claimed(generation: u64) -> Value {
    let attempt = json!({"id": "a1", "generation": generation,
                         "expires_at": "2026-11-26T02:00:00Z",
                         "last_progress_at": "2026-11-26T01:00:00Z"});
    json!({"data": {"claim": {"attempt": attempt, "lease_remaining_ms": 3_600_000},
                    "renew_after_seconds": 60}})
}

fn launch() -> Launch {
    let suggestion = Suggestion {
        task: "t1".into(),
        revision: 3,
        title: String::new(),
    };
    Launch::plan(&Config::default(), "p1", suggestion)
}

fn lease() -> Lease {
    Lease::parse(&claimed(2)).unwrap()
}

/// Work over `api` and `cli`, with a ledger in `dir`.
fn work(dir: &tempfile::TempDir, api: FakeApi, cli: FakeCli) -> CoordinatorWork<FakeApi, FakeCli> {
    let ledger = Ledger::at(&dir.path().join("ledger.jsonl"));
    CoordinatorWork::new("p1", Arc::new(api), cli, ledger)
}

/// The ledger's event names.
fn events(work: &CoordinatorWork<FakeApi, FakeCli>) -> Vec<String> {
    let names = work.ledger.events().into_iter();
    names
        .map(|e| e["event"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn settings_are_off_by_default_and_an_enabled_coordinator_needs_https_a_project_and_a_fixture() {
    let defaults = CoordinatorSettings::default();
    assert!(!defaults.enabled);
    defaults.validate().unwrap();
    let good = CoordinatorSettings {
        enabled: true,
        service_url: "https://agents.example.com".into(),
        project_id: "p1".into(),
        fixture: vec!["probe.exe".into()],
        ..defaults.clone()
    };
    good.validate().unwrap();
    let plain = CoordinatorSettings {
        service_url: "http://agents.example.com".into(),
        ..good.clone()
    };
    assert!(plain.validate().is_err());
    let no_project = CoordinatorSettings {
        project_id: String::new(),
        ..good.clone()
    };
    assert!(no_project.validate().is_err());
    let no_fixture = CoordinatorSettings {
        fixture: Vec::new(),
        ..good
    };
    assert!(no_fixture.validate().is_err());
}

#[test]
fn the_credential_binding_and_cli_state_live_in_the_protected_directory() {
    let settings = CoordinatorSettings::default();
    let directory = &settings.directory;
    assert_eq!(settings.credentials(), directory.join("credentials.toml"));
    assert_eq!(settings.binding_file(), directory.join("binding.toml"));
    assert_eq!(settings.state(), directory.join("state"));
}

#[test]
fn next_asks_the_project_leaving_the_hosts_holds_out() {
    let dir = tempfile::tempdir().unwrap();
    let api = FakeApi {
        next: json!({"action": null}),
        ..FakeApi::default()
    };
    let mut work = work(&dir, api, FakeCli::default());
    assert_eq!(Work::project(&work), "p1");
    let held = ["t7".to_owned(), "t8".to_owned()];
    assert_eq!(work.next(&held).unwrap(), json!({"action": null}));
    assert_eq!(*work.api.asked.lock().unwrap(), ["next p1 t7,t8"]);
}

#[test]
fn a_claim_connects_the_launchs_own_session_then_claims_at_the_suggested_revision() {
    let dir = tempfile::tempdir().unwrap();
    let cli = FakeCli::answering(vec![ok(json!({})), ok(claimed(1))]);
    let mut work = work(&dir, FakeApi::default(), cli);
    let launch = launch();
    let lease = work.claim(&launch).unwrap();
    assert_eq!((lease.attempt.as_str(), lease.generation), ("a1", 1));
    let calls = work.cli.calls.borrow();
    let session = launch.session_id.to_string();
    assert!(calls.iter().all(|call| call.0 == session));
    assert_eq!(
        calls[0].1,
        ["connect", "--harness=agentc-supervisor-windows"]
    );
    assert_eq!(calls[1].1, ["claim", "--task=t1", "--revision=3"]);
    drop(calls);
    assert_eq!(events(&work), ["claim"]);
}

#[test]
fn a_refused_claim_fails_and_records_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let cli = FakeCli::answering(vec![ok(json!({})), refused(5, "task_revision_conflict")]);
    let mut work = work(&dir, FakeApi::default(), cli);
    let error = work.claim(&launch()).unwrap_err().to_string();
    assert!(error.contains("task_revision_conflict"), "{error}");
    assert!(events(&work).is_empty());
}

#[test]
fn a_renewal_returns_the_new_lease() {
    let dir = tempfile::tempdir().unwrap();
    let mut work = work(
        &dir,
        FakeApi::default(),
        FakeCli::answering(vec![ok(claimed(2))]),
    );
    let renewed = work.renew(&launch(), &lease()).unwrap();
    assert_eq!(renewed.generation, 2);
    assert_eq!(
        work.cli.calls.borrow()[0].1,
        ["renew", "--attempt=a1", "--generation=2"]
    );
    assert_eq!(events(&work), ["renew"]);
}

#[test]
fn a_renewal_refused_for_good_ends_the_attempt_with_its_state() {
    let dir = tempfile::tempdir().unwrap();
    let api = FakeApi {
        detail: Some(json!({"attempts": [{"id": "a1", "state": "expired", "generation": 2}]})),
        ..FakeApi::default()
    };
    let cli = FakeCli::answering(vec![refused(5, "lease_expired")]);
    let mut work = work(&dir, api, cli);
    let error = work.renew(&launch(), &lease()).unwrap_err();
    let ended = error.downcast_ref::<AttemptEnded>().unwrap();
    assert_eq!(ended.code, "lease_expired");
    assert_eq!(ended.state.as_deref(), Some("expired"));
}

#[test]
fn a_transient_renewal_failure_is_an_ordinary_error() {
    let dir = tempfile::tempdir().unwrap();
    let cli = FakeCli::answering(vec![refused(7, "transport_failure")]);
    let mut work = work(&dir, FakeApi::default(), cli);
    let error = work.renew(&launch(), &lease()).unwrap_err();
    assert!(error.downcast_ref::<AttemptEnded>().is_none());
    assert!(work.api.asked.lock().unwrap().is_empty());
}

#[test]
fn the_attempt_state_is_read_from_the_task_and_once_more_when_unreadable() {
    let dir = tempfile::tempdir().unwrap();
    let api = FakeApi {
        detail: Some(json!({"attempts": [{"id": "a1", "state": "released", "generation": 2}]})),
        ..FakeApi::default()
    };
    let mut work = work(&dir, api, FakeCli::default());
    let state = work.attempt_state(&launch(), &lease());
    assert_eq!(state.as_deref(), Some("released"));
    let mut unreadable = super::tests::work(&dir, FakeApi::default(), FakeCli::default());
    assert_eq!(unreadable.attempt_state(&launch(), &lease()), None);
    assert_eq!(unreadable.api.asked.lock().unwrap().len(), 2);
}

#[test]
fn a_release_checkpoints_the_summary_then_releases() {
    let dir = tempfile::tempdir().unwrap();
    let mut work = work(&dir, FakeApi::default(), FakeCli::default());
    work.release(&launch(), &lease(), "fixture exited 0")
        .unwrap();
    assert_eq!(work.cli.commands(), ["checkpoint", "release"]);
    let calls = work.cli.calls.borrow();
    let expected = ["checkpoint", "--attempt=a1", "--generation=2", "--input=-"];
    assert_eq!(calls[0].1, expected);
    let checkpoint: Value = serde_json::from_str(&calls[0].2).unwrap();
    assert_eq!(checkpoint["summary"], "fixture exited 0");
    let release: Value = serde_json::from_str(&calls[1].2).unwrap();
    assert_eq!(release, json!({"summary": "fixture exited 0"}));
    drop(calls);
    assert_eq!(events(&work), ["checkpoint", "release"]);
}

#[test]
fn a_refused_checkpoint_still_releases_and_a_conflict_counts_as_released() {
    let dir = tempfile::tempdir().unwrap();
    let cli = FakeCli::answering(vec![
        refused(5, "lease_expired"),
        refused(5, "lease_expired"),
    ]);
    let mut work = work(&dir, FakeApi::default(), cli);
    work.release(&launch(), &lease(), "drained").unwrap();
    assert_eq!(work.cli.commands(), ["checkpoint", "release"]);
    let ledger = work.ledger.events();
    assert_eq!(
        (ledger[0]["ok"].clone(), ledger[1]["ended"].clone()),
        (json!(false), json!(true))
    );
}

#[test]
fn any_other_release_failure_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let cli = FakeCli::answering(vec![ok(json!({})), refused(7, "transport_failure")]);
    let mut work = work(&dir, FakeApi::default(), cli);
    assert!(work.release(&launch(), &lease(), "x").is_err());
    assert_eq!(events(&work), ["checkpoint"]);
}

#[test]
fn the_authority_reader_reports_the_attempts_state_and_generation_and_fails_closed() {
    let api = FakeApi {
        detail: Some(json!({"attempts": [{"id": "a1", "state": "active", "generation": 4}]})),
        ..FakeApi::default()
    };
    let reader = Reader::new("p1", Arc::new(api));
    let view = reader.read("t1", "a1").unwrap().unwrap();
    assert_eq!((view.state.as_str(), view.generation), ("active", 4));
    assert_eq!(reader.read("t1", "a9").unwrap(), None);
    assert_eq!(
        *reader.api.asked.lock().unwrap(),
        ["task p1 t1", "task p1 t1"]
    );
    let unreadable = Reader::new("p1", Arc::new(FakeApi::default()));
    assert!(unreadable.read("t1", "a1").is_err());
}

#[test]
fn a_summary_is_cut_to_what_the_service_accepts_on_a_character_boundary() {
    assert_eq!(bounded("short"), "short");
    let long = "é".repeat(MAX_SUMMARY);
    let cut = bounded(&long);
    assert!(cut.len() <= MAX_SUMMARY && cut.len() >= MAX_SUMMARY - 1);
    assert!(cut.chars().all(|c| c == 'é'));
}

#[test]
fn the_credential_is_read_for_the_configured_origin_and_the_binding_written_beside_it() {
    let dir = tempfile::tempdir().unwrap();
    let settings = CoordinatorSettings {
        enabled: true,
        service_url: "https://agents.example.com".into(),
        project_id: "p1".into(),
        directory: dir.path().to_owned(),
        ..CoordinatorSettings::default()
    };
    let text = "[[credentials]]\norigin = \"https://other.example.com\"\ntoken = \"wrong-token\"\n\
                [[credentials]]\norigin = \"https://agents.example.com\"\ntoken = \"secret-token\"\n";
    std::fs::write(settings.credentials(), text).unwrap();
    let access = Access::open(&settings).unwrap();
    let binding = std::fs::read_to_string(settings.binding_file()).unwrap();
    assert_eq!(Binding::parse(&binding).unwrap(), settings.binding());
    let cli = access.cli(&settings);
    for shown in [format!("{access:?}"), format!("{cli:?}")] {
        assert!(!shown.contains("secret-token"), "{shown}");
    }
    let command = cli.command("s1", &["renew".to_owned()]);
    let token = command
        .get_envs()
        .find(|(k, _)| *k == "AGENT_COORDINATOR_TOKEN");
    assert_eq!(
        token.and_then(|(_, v)| v),
        Some(std::ffi::OsStr::new("secret-token"))
    );
}

#[test]
fn a_cli_child_gets_the_global_arguments_and_only_the_kept_variables_and_credential() {
    let dir = tempfile::tempdir().unwrap();
    let settings = CoordinatorSettings {
        service_url: "https://agents.example.com".into(),
        project_id: "p1".into(),
        directory: dir.path().to_owned(),
        ..CoordinatorSettings::default()
    };
    let text = "[[credentials]]\norigin = \"https://agents.example.com\"\ntoken = \"t\"\n";
    std::fs::write(settings.credentials(), text).unwrap();
    let cli = Access::open(&settings).unwrap().cli(&settings);
    let command = cli.command("s1", &["claim".to_owned()]);
    let args: Vec<String> = command
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let state = format!("--state-dir={}", settings.state().display());
    let binding = format!("--repo-config={}", settings.binding_file().display());
    assert_eq!(args, ["--json", "--session=s1", &state, &binding, "claim"]);
    let names: Vec<String> = command
        .get_envs()
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect();
    let allowed = |name: &String| {
        access::KEPT.contains(&name.as_str()) || name.starts_with("AGENT_COORDINATOR_")
    };
    assert!(names.iter().all(allowed), "{names:?}");
    assert!(names.contains(&"AGENT_COORDINATOR_ORIGIN".to_owned()));
}

#[test]
fn an_unreadable_or_invalid_credential_file_is_refused_without_quoting_it() {
    let dir = tempfile::tempdir().unwrap();
    let settings = CoordinatorSettings {
        service_url: "https://agents.example.com".into(),
        project_id: "p1".into(),
        directory: dir.path().to_owned(),
        ..CoordinatorSettings::default()
    };
    assert!(Access::open(&settings).is_err());
    std::fs::write(settings.credentials(), "token = \"leaked-value\" [").unwrap();
    let error = format!("{:#}", Access::open(&settings).unwrap_err());
    assert!(!error.contains("leaked-value"), "{error}");
    let other = "[[credentials]]\norigin = \"https://other.example.com\"\ntoken = \"t\"\n";
    std::fs::write(settings.credentials(), other).unwrap();
    assert!(Access::open(&settings).is_err());
}
