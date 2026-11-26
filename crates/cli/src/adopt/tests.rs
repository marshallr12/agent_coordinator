//! Whole-command tests: the real `execute` against a loopback stand-in for
//! the service, a scripted host and a throwaway Git repository.

use clap::Parser;
use serde_json::Value;

use super::testing::{Capture, FakeService, TestHost, TestRepo};
use super::{AdoptArgs, Options, execute, files, report::Reporter, text};
use crate::Failure;

/// The credential in every test; it must never appear in output or in a file.
pub const TOKEN: &str = "fixture-secret-token-9f31c2";
/// The remote of the test repository.
const REMOTE: &str = "https://example.com/acme/billing.git";
/// The project that matches [`REMOTE`] after normalisation.
const PROJECT: &str = "11111111-1111-4111-8111-111111111111";

/// A repository with one commit and an `origin` remote, and a service that has its project.
fn fixture() -> (TestRepo, FakeService) {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "# Billing\n");
    repo.git(&["remote", "add", "origin", REMOTE]);
    let service = FakeService::start(TOKEN);
    service.add_project(PROJECT, "Billing", "git@example.com:acme/billing");
    (repo, service)
}

/// Flags that accept every default.
fn yes() -> AdoptArgs {
    AdoptArgs {
        yes: true,
        ..AdoptArgs::default()
    }
}

/// Runs adopt in `repo` against `service`; returns the result and everything it printed.
async fn adopt(
    repo: &TestRepo,
    service: &FakeService,
    host: &mut TestHost,
    mut args: AdoptArgs,
    json: bool,
) -> (Result<Value, Failure>, String) {
    if args.service_url.is_none() && !repo.root().join(files::BINDING_FILE).exists() {
        args.service_url = Some(service.origin.clone());
    }
    let options = Options {
        args,
        allow_insecure_loopback: true,
        start: repo.root().to_path_buf(),
    };
    let capture = Capture::default();
    let mut reporter = Reporter::new(json, Box::new(capture.clone()));
    let result = execute(&options, host, &mut reporter).await;
    let output = format!("{}{}", capture.text(), host.prompts.join("\n"));
    (result, output)
}

/// The error code of a failed run.
fn code(failure: &Failure) -> &str {
    failure.output["error"]["code"].as_str().unwrap_or_default()
}

/// Asserts that the credential is in no output, result or repository file.
fn assert_no_token(repo: &TestRepo, output: &str, result: &Result<Value, Failure>) {
    let rendered = match result {
        Ok(value) => value.to_string(),
        Err(failure) => failure.output.to_string(),
    };
    assert!(
        !output.contains(TOKEN) && !rendered.contains(TOKEN),
        "the credential leaked into output"
    );
    for (name, bytes) in repo.files() {
        assert!(
            !String::from_utf8_lossy(&bytes).contains(TOKEN),
            "the credential leaked into {name}"
        );
    }
}

#[test]
fn adopt_and_its_alias_parse_without_a_binding_or_session() {
    for name in ["adopt", "scaffold"] {
        let cli = crate::Cli::try_parse_from(["agent-coordinator", name]).unwrap();
        assert!(matches!(cli.command, crate::Command::Adopt(_)), "{name}");
    }
    let flags = [
        "adopt",
        "some/path",
        "--service-url",
        "https://agents.example",
        "--name",
        "Billing",
        "--repository-url",
        "https://example.com/a/b",
        "--branch",
        "main",
        "--project",
        "p1",
        "--harness",
        "both",
        "--no-commit",
        "--no-browser",
        "--approval-timeout",
        "30",
        "--migrate",
        "off",
        "--migration-agent",
        "codex",
        "--replace-binding",
        "--force-instructions",
        "--non-interactive",
        "--yes",
        "--dry-run",
        "--json",
    ];
    let cli =
        crate::Cli::try_parse_from(std::iter::once("agent-coordinator").chain(flags)).unwrap();
    let crate::Command::Adopt(args) = cli.command else {
        panic!("expected adopt")
    };
    assert_eq!(
        args.path.as_deref(),
        Some(std::path::Path::new("some/path"))
    );
    assert!(args.no_commit && args.yes && args.dry_run && cli.json);
    assert!(args.no_browser && args.approval_timeout == 30);
    assert_eq!(args.migrate, super::MigrateArg::Off);
    assert_eq!(args.migration_agent, Some(super::AgentArg::Codex));
    assert_eq!(AdoptArgs::default().approval_timeout, 900);
    assert!(
        crate::Cli::try_parse_from(["agent-coordinator", "adopt", "--commit", "--no-commit"])
            .is_err()
    );
}

#[tokio::test]
async fn yes_links_the_matching_project_writes_verifies_and_commits() {
    let (repo, service) = fixture();
    let mut host = TestHost::new(TOKEN).silent();
    let (result, output) = adopt(&repo, &service, &mut host, yes(), false).await;
    let value = result.as_ref().unwrap();
    assert_eq!(value["result"]["status"], "connected");
    assert_eq!(value["result"]["project_id"], PROJECT);
    assert_eq!(
        repo.read(files::BINDING_FILE),
        files::render_binding(&service.origin, PROJECT)
    );
    assert_eq!(repo.read(files::AGENTS_FILE), format!("{}\n", text::BLOCK));
    assert_eq!(repo.read(files::CLAUDE_FILE), "@AGENTS.md\n");
    assert_eq!(
        repo.git(&["status", "--porcelain"]),
        "",
        "everything adopt wrote is committed"
    );
    let committed = repo.git(&["show", "--name-only", "--format=", "HEAD"]);
    assert_eq!(
        committed.lines().collect::<Vec<_>>(),
        [".agent-coordinator.toml", "AGENTS.md", "CLAUDE.md"]
    );
    assert_eq!(
        value["result"]["commit"],
        repo.git(&["rev-parse", "HEAD"]).trim()
    );
    assert!(output.contains("Will not touch credentials"), "{output}");
    assert_no_token(&repo, &output, &result);
}

#[tokio::test]
async fn only_get_requests_are_ever_sent() {
    let (repo, service) = fixture();
    let mut host = TestHost::new(TOKEN).silent();
    adopt(&repo, &service, &mut host, yes(), false)
        .await
        .0
        .unwrap();
    adopt(&repo, &service, &mut host, AdoptArgs::default(), false)
        .await
        .0
        .unwrap();
    let seen = service.seen();
    assert!(seen.len() >= 8, "{seen:?}");
    assert!(
        seen.iter().all(|request| request.method == "GET"),
        "{seen:?}"
    );
}

#[tokio::test]
async fn the_project_flag_links_without_a_remote() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "# Local only\n");
    let service = FakeService::start(TOKEN);
    service.add_project(PROJECT, "Billing", "https://example.com/acme/billing");
    let mut host = TestHost::new(TOKEN).silent();
    let args = AdoptArgs {
        project: Some(PROJECT.into()),
        ..yes()
    };
    let (result, _) = adopt(&repo, &service, &mut host, args, false).await;
    assert_eq!(result.unwrap()["result"]["project_id"], PROJECT);
    let unknown = AdoptArgs {
        project: Some("no-such-project".into()),
        replace_binding: true,
        ..yes()
    };
    let (result, _) = adopt(&repo, &service, &mut host, unknown, false).await;
    assert_eq!(code(&result.unwrap_err()), "project_not_found");
}

#[tokio::test]
async fn a_dry_run_prints_the_plan_and_diffs_and_writes_nothing() {
    let (repo, service) = fixture();
    let before = (repo.files(), repo.git(&["rev-parse", "HEAD"]));
    let mut host = TestHost::new(TOKEN).silent();
    let args = AdoptArgs {
        dry_run: true,
        ..AdoptArgs::default()
    };
    let (result, output) = adopt(&repo, &service, &mut host, args, false).await;
    assert_eq!(result.as_ref().unwrap()["result"]["status"], "dry_run");
    assert_eq!((repo.files(), repo.git(&["rev-parse", "HEAD"])), before);
    for expected in [
        "link existing \"Billing\"",
        "+++ b/.agent-coordinator.toml",
        "+++ b/AGENTS.md",
        "+@AGENTS.md",
    ] {
        assert!(
            output.contains(expected),
            "missing {expected:?} in {output}"
        );
    }
    assert_no_token(&repo, &output, &result);
}

#[tokio::test]
async fn a_silent_run_lists_every_missing_flag_at_once() {
    let (repo, service) = fixture();
    let before = repo.files();
    let mut host = TestHost::new(TOKEN).silent();
    let (result, _) = adopt(&repo, &service, &mut host, AdoptArgs::default(), false).await;
    let failure = result.unwrap_err();
    assert_eq!((failure.exit, code(&failure)), (2, "needs_input"));
    let message = failure.output["error"]["message"].as_str().unwrap();
    assert!(
        message.contains(&format!("--project {PROJECT}")) && message.contains("--yes"),
        "{message}"
    );
    assert_eq!(
        failure.output["error"]["details"]["missing"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(repo.files(), before);

    let bare = TestRepo::new();
    let args = AdoptArgs {
        non_interactive: true,
        ..AdoptArgs::default()
    };
    let mut terminal = TestHost::new(TOKEN);
    let (result, _) = adopt(&bare, &service, &mut terminal, args, false).await;
    let message = result.unwrap_err().output["error"]["message"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        message.contains("--repository-url URL") && message.contains("--yes"),
        "{message}"
    );
    assert!(
        terminal.prompts.is_empty(),
        "--non-interactive never prompts"
    );
}

#[tokio::test]
async fn a_completed_repository_reports_already_connected_without_writing() {
    let (repo, service) = fixture();
    let mut host = TestHost::new(TOKEN).silent();
    adopt(&repo, &service, &mut host, yes(), false)
        .await
        .0
        .unwrap();
    let before = (repo.files(), repo.git(&["rev-parse", "HEAD"]));
    let (result, output) = adopt(&repo, &service, &mut host, AdoptArgs::default(), false).await;
    let value = result.unwrap();
    assert_eq!(value["result"]["status"], "already_connected");
    assert_eq!(
        value["result"]["files_written"].as_array().unwrap().len(),
        0
    );
    assert_eq!((repo.files(), repo.git(&["rev-parse", "HEAD"])), before);
    assert!(output.contains("every file is current"), "{output}");
}

#[tokio::test]
async fn an_uncommitted_adoption_can_be_rerun_and_stays_uncommitted() {
    let (repo, service) = fixture();
    let mut host = TestHost::new(TOKEN).silent();
    let args = AdoptArgs {
        no_commit: true,
        ..yes()
    };
    let (result, _) = adopt(&repo, &service, &mut host, args, false).await;
    assert_eq!(result.unwrap()["result"]["commit"], Value::Null);
    let head = repo.git(&["rev-parse", "HEAD"]);
    let (result, _) = adopt(&repo, &service, &mut host, AdoptArgs::default(), false).await;
    assert_eq!(result.unwrap()["result"]["status"], "already_connected");
    assert_eq!(repo.git(&["rev-parse", "HEAD"]), head);
    assert!(
        repo.git(&["status", "--porcelain"])
            .contains("?? AGENTS.md")
    );
}

#[tokio::test]
async fn an_interrupted_run_is_finished_by_running_again() {
    let (repo, service) = fixture();
    repo.write(
        files::BINDING_FILE,
        &files::render_binding(&service.origin, PROJECT),
    );
    let mut host = TestHost::new(TOKEN).silent();
    let (result, _) = adopt(&repo, &service, &mut host, AdoptArgs::default(), false).await;
    let value = result.unwrap();
    assert_eq!(value["result"]["status"], "connected");
    assert_eq!(
        value["result"]["files_written"],
        serde_json::json!(["AGENTS.md", "CLAUDE.md"])
    );
    assert_eq!(
        repo.git(&["status", "--porcelain"]),
        "",
        "the earlier binding is committed with the rest"
    );
}

#[tokio::test]
async fn a_run_interrupted_after_the_first_instruction_file_commits_all_of_adopts_files() {
    let (repo, service) = fixture();
    repo.write(
        files::BINDING_FILE,
        &files::render_binding(&service.origin, PROJECT),
    );
    repo.write(files::AGENTS_FILE, &format!("{}\n", text::BLOCK));
    let mut host = TestHost::new(TOKEN).silent();
    let args = AdoptArgs {
        harness: super::HarnessArg::Both,
        ..AdoptArgs::default()
    };
    let (result, _) = adopt(&repo, &service, &mut host, args, false).await;
    assert_eq!(
        result.unwrap()["result"]["files_written"],
        serde_json::json!(["CLAUDE.md"])
    );
    assert_eq!(
        repo.git(&["status", "--porcelain"]),
        "",
        "the earlier files hold only adopt's text"
    );
}

#[tokio::test]
async fn a_persons_uncommitted_edits_are_never_swept_into_the_commit() {
    let (repo, service) = fixture();
    repo.commit_file(files::AGENTS_FILE, &format!("{}\n\nRules.\n", text::BLOCK));
    let edited = format!("{}\n\nRules.\n\nAn unsaved thought.\n", text::BLOCK);
    repo.write(files::AGENTS_FILE, &edited);
    let mut host = TestHost::new(TOKEN).silent();
    let (result, _) = adopt(&repo, &service, &mut host, yes(), false).await;
    assert_eq!(
        result.unwrap()["result"]["files_written"],
        serde_json::json!([".agent-coordinator.toml"])
    );
    assert_eq!(
        repo.git(&["show", "--name-only", "--format=", "HEAD"])
            .trim(),
        ".agent-coordinator.toml"
    );
    assert_eq!(
        repo.git(&["status", "--porcelain"]).trim(),
        "M AGENTS.md",
        "their edit is still theirs to commit"
    );
    assert_eq!(repo.read(files::AGENTS_FILE), edited);
}

#[tokio::test]
async fn a_missing_or_rejected_credential_exits_3_with_nothing_written() {
    let (repo, service) = fixture();
    let before = repo.files();
    let mut without = TestHost::new(TOKEN).silent();
    without.token = None;
    let (result, _) = adopt(&repo, &service, &mut without, yes(), false).await;
    let failure = result.unwrap_err();
    assert_eq!(
        (failure.exit, code(&failure)),
        (3, "authentication_required")
    );
    let message = failure.output["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("credentials.toml") && message.contains("Nothing was written"),
        "{message}"
    );
    assert!(failure.output["error"]["details"]["service_help"].is_object());

    let mut wrong = TestHost::new("not-the-token").silent();
    let (result, output) = adopt(&repo, &service, &mut wrong, yes(), false).await;
    assert_eq!(result.as_ref().unwrap_err().exit, 3);
    assert!(!output.contains("not-the-token"));
    assert_eq!(repo.files(), before);
}

#[tokio::test]
async fn a_read_only_credential_is_not_enough() {
    let (repo, service) = fixture();
    service.state.lock().unwrap().actor["credential_attributes"]["access"] = "read".into();
    let mut host = TestHost::new(TOKEN).silent();
    let (result, _) = adopt(&repo, &service, &mut host, yes(), false).await;
    let failure = result.unwrap_err();
    assert_eq!(failure.exit, 3);
    assert!(
        failure.output["error"]["message"]
            .as_str()
            .unwrap()
            .contains("write access")
    );
}

#[tokio::test]
async fn a_binding_that_points_elsewhere_is_refused_unless_replaced() {
    let (repo, service) = fixture();
    let old = files::render_binding(&service.origin, "22222222-2222-4222-8222-222222222222");
    repo.commit_file(files::BINDING_FILE, &old);
    let mut host = TestHost::new(TOKEN).silent();
    let (result, _) = adopt(&repo, &service, &mut host, yes(), false).await;
    let failure = result.unwrap_err();
    assert_eq!((failure.exit, code(&failure)), (5, "binding_conflict"));
    assert_eq!(repo.read(files::BINDING_FILE), old);

    let args = AdoptArgs {
        replace_binding: true,
        ..yes()
    };
    let (result, _) = adopt(&repo, &service, &mut host, args, false).await;
    assert_eq!(result.unwrap()["result"]["project_id"], PROJECT);
    assert_eq!(
        repo.read(files::BINDING_FILE),
        files::render_binding(&service.origin, PROJECT)
    );
}

#[tokio::test]
async fn an_invalid_binding_is_refused_with_the_parser_error() {
    let (repo, service) = fixture();
    repo.commit_file(
        files::BINDING_FILE,
        "service_url = \"https://agents.example\"\ntoken = \"x\"\n",
    );
    let mut host = TestHost::new(TOKEN).silent();
    let (result, _) = adopt(&repo, &service, &mut host, yes(), false).await;
    let failure = result.unwrap_err();
    assert_eq!((failure.exit, code(&failure)), (2, "invalid_binding"));
    assert!(
        failure.output["error"]["message"]
            .as_str()
            .unwrap()
            .contains("parse repository binding")
    );
}

#[tokio::test]
async fn an_instruction_file_with_uncommitted_changes_stops_the_run() {
    let (repo, service) = fixture();
    repo.commit_file(files::AGENTS_FILE, "# Rules\n");
    repo.write(files::AGENTS_FILE, "# Rules\n\nUnsaved thought.\n");
    let before = repo.files();
    let mut host = TestHost::new(TOKEN).silent();
    let (result, _) = adopt(&repo, &service, &mut host, yes(), false).await;
    let failure = result.unwrap_err();
    assert_eq!((failure.exit, code(&failure)), (6, "target_files_dirty"));
    assert_eq!(
        failure.output["error"]["details"]["paths"],
        serde_json::json!(["AGENTS.md"])
    );
    assert_eq!(repo.files(), before);
}

#[tokio::test]
async fn an_interactive_run_asks_only_what_was_not_detected() {
    let (repo, service) = fixture();
    let mut host = TestHost::new(TOKEN).answering(&["", "codex", "n", ""]);
    let (result, _) = adopt(&repo, &service, &mut host, AdoptArgs::default(), false).await;
    let value = result.unwrap();
    assert_eq!(value["result"]["commit"], Value::Null);
    assert!(
        !repo.root().join(files::CLAUDE_FILE).exists(),
        "only Codex was chosen"
    );
    let asked: Vec<&str> = host.prompts.iter().map(String::as_str).collect();
    assert_eq!(asked.len(), 4, "{asked:?}");
    assert!(
        asked[0].contains("Link to it? [Y/n]") && asked[0].contains(PROJECT),
        "{asked:?}"
    );
    assert!(
        asked[1].starts_with("Which assistants will work here?"),
        "{asked:?}"
    );
    assert_eq!(
        asked[2..],
        ["Commit the changes? [Y/n] ", "Proceed? [Y/n] "]
    );
}

#[tokio::test]
async fn declining_the_plan_writes_nothing() {
    let (repo, service) = fixture();
    let before = repo.files();
    let mut host = TestHost::new(TOKEN).answering(&["y", "both", "y", "n"]);
    let (result, _) = adopt(&repo, &service, &mut host, AdoptArgs::default(), false).await;
    let failure = result.unwrap_err();
    assert_eq!((failure.exit, code(&failure)), (2, "cancelled"));
    assert_eq!(repo.files(), before);
}

#[tokio::test]
async fn an_incompatible_client_stops_before_anything_else() {
    let (repo, service) = fixture();
    let mut host = TestHost::new(TOKEN).silent();
    host.compatible = false;
    let (result, _) = adopt(&repo, &service, &mut host, yes(), false).await;
    let failure = result.unwrap_err();
    assert_eq!(
        (failure.exit, code(&failure)),
        (6, "client_upgrade_required")
    );
    assert!(
        service
            .seen()
            .iter()
            .all(|request| request.target == "/api/v1/info")
    );
}

#[tokio::test]
async fn projects_are_matched_across_pages() {
    let (repo, service) = fixture();
    {
        let mut state = service.state.lock().unwrap();
        state.page_size = 1;
        let matching = state.projects.remove(0);
        for id in [
            "00000000-0000-4000-8000-000000000001",
            "00000000-0000-4000-8000-000000000002",
        ] {
            state.projects.push(serde_json::json!({"id": id, "name": id, "repository_url": "https://example.com/x/y", "target_branch": "main"}));
        }
        state.projects.push(matching);
    }
    let mut host = TestHost::new(TOKEN).silent();
    let (result, _) = adopt(&repo, &service, &mut host, yes(), false).await;
    assert_eq!(result.unwrap()["result"]["project_id"], PROJECT);
    assert!(
        service
            .seen()
            .iter()
            .any(|request| request.target.contains("cursor="))
    );
}

#[tokio::test]
async fn json_mode_prints_one_event_object_per_line() {
    let (repo, service) = fixture();
    let mut host = TestHost::new(TOKEN).silent();
    let (result, output) = adopt(&repo, &service, &mut host, yes(), true).await;
    let steps: Vec<String> = output
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line).unwrap()["step"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    for step in ["preflight", "plan", "write", "verify", "commit"] {
        assert!(
            steps.iter().any(|seen| seen == step),
            "missing {step} in {steps:?}"
        );
    }
    assert_no_token(&repo, &output, &result);
}

#[tokio::test]
async fn sign_in_details_in_the_remote_never_reach_output_or_files() {
    let (repo, service) = fixture();
    repo.git(&[
        "remote",
        "set-url",
        "origin",
        "https://ci-user:hunter2-remote-secret@example.com/acme/billing.git",
    ]);
    let mut host = TestHost::new(TOKEN).silent();
    let args = AdoptArgs {
        dry_run: true,
        ..AdoptArgs::default()
    };
    let (result, output) = adopt(&repo, &service, &mut host, args, true).await;
    let rendered = format!("{output}{}", result.unwrap());
    assert!(
        !rendered.contains("hunter2-remote-secret") && !rendered.contains("ci-user"),
        "{rendered}"
    );
    assert!(
        rendered.contains("were removed from the repository URL"),
        "{rendered}"
    );
}

#[tokio::test]
async fn a_merge_in_progress_or_a_plain_directory_is_refused() {
    let (repo, service) = fixture();
    std::fs::write(repo.root().join(".git/MERGE_HEAD"), "0\n").unwrap();
    let mut host = TestHost::new(TOKEN).silent();
    let (result, _) = adopt(&repo, &service, &mut host, yes(), false).await;
    assert_eq!(result.unwrap_err().exit, 6);
    let plain = tempfile::tempdir().unwrap();
    let options = Options {
        args: yes(),
        allow_insecure_loopback: true,
        start: plain.path().to_path_buf(),
    };
    let mut reporter = Reporter::new(false, Box::new(Capture::default()));
    let failure = execute(&options, &mut host, &mut reporter)
        .await
        .unwrap_err();
    assert_eq!((failure.exit, code(&failure)), (2, "not_a_repository"));
}

/// The project a person creates in the dashboard while adopt waits.
const CREATED: &str = "33333333-3333-4333-8333-333333333333";

/// A repository whose remote has no project on the service yet.
fn unmatched_fixture() -> (TestRepo, FakeService) {
    let (repo, service) = fixture();
    service.state.lock().unwrap().projects.clear();
    service.add_project(
        "00000000-0000-4000-8000-00000000000a",
        "Other",
        "https://example.com/acme/other",
    );
    (repo, service)
}

/// How many project listings (first pages) the service has answered or dropped.
fn listings(service: &FakeService) -> usize {
    let first_page =
        |target: &str| target.starts_with("/api/v1/projects") && !target.contains("cursor=");
    service
        .seen()
        .iter()
        .filter(|request| first_page(&request.target))
        .count()
}

#[tokio::test]
async fn a_new_project_is_found_by_polling_after_the_link_is_shown() {
    let (repo, service) = unmatched_fixture();
    service.appear_after(4, CREATED, "billing", "git@example.com:acme/billing.git");
    let mut host = TestHost::new(TOKEN).silent();
    let (result, output) = adopt(&repo, &service, &mut host, yes(), false).await;
    let value = result.as_ref().unwrap();
    assert_eq!(value["result"]["project_id"], CREATED);
    assert_eq!(
        repo.read(files::BINDING_FILE),
        files::render_binding(&service.origin, CREATED)
    );
    let link = format!(
        "{}/#/new-project?name=billing&repository_url=https%3A%2F%2Fexample.com%2Facme%2Fbilling.git&target_branch=main",
        service.origin
    );
    assert_eq!(host.opened, std::slice::from_ref(&link));
    for expected in [
        link.as_str(),
        "Repository URL  https://example.com/acme/billing.git",
        "Waiting for the project to appear",
    ] {
        assert!(
            output.contains(expected),
            "missing {expected:?} in {output}"
        );
    }
    assert!(
        listings(&service) >= 5,
        "the project appeared on the third poll"
    );
    assert!(service.seen().iter().all(|request| request.method == "GET"));
    assert_no_token(&repo, &output, &result);
}

#[tokio::test]
async fn no_browser_prints_the_link_and_opens_nothing() {
    let (repo, service) = unmatched_fixture();
    service.appear_after(2, CREATED, "billing", REMOTE);
    let mut host = TestHost::new(TOKEN).silent();
    let args = AdoptArgs {
        no_browser: true,
        ..yes()
    };
    let (result, output) = adopt(&repo, &service, &mut host, args, true).await;
    assert_eq!(result.unwrap()["result"]["project_id"], CREATED);
    assert!(host.opened.is_empty());
    let waiting = output
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|event| event["status"] == "waiting")
        .expect("a waiting event");
    assert!(
        waiting["link"]
            .as_str()
            .unwrap()
            .contains("#/new-project?name=billing&")
    );
    assert_eq!(waiting["manual"]["target_branch"], "main");
}

#[tokio::test]
async fn a_timeout_exits_7_with_nothing_written() {
    let (repo, service) = unmatched_fixture();
    let before = repo.files();
    let mut host = TestHost::new(TOKEN).silent();
    let args = AdoptArgs {
        approval_timeout: 0,
        ..yes()
    };
    let (result, _) = adopt(&repo, &service, &mut host, args, false).await;
    let failure = result.unwrap_err();
    assert_eq!((failure.exit, code(&failure)), (7, "project_not_created"));
    let message = failure.output["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("--project ID") && message.contains("Nothing was written"),
        "{message}"
    );
    assert_eq!(repo.files(), before);
}

#[tokio::test]
async fn two_new_matching_projects_need_a_choice() {
    let (repo, service) = unmatched_fixture();
    service.appear_after(2, CREATED, "billing", REMOTE);
    service.appear_after(
        2,
        "44444444-4444-4444-8444-444444444444",
        "billing-2",
        REMOTE,
    );
    let before = repo.files();
    let mut host = TestHost::new(TOKEN).silent();
    let (result, _) = adopt(&repo, &service, &mut host, yes(), false).await;
    let failure = result.unwrap_err();
    assert_eq!((failure.exit, code(&failure)), (5, "ambiguous_project"));
    assert!(
        failure.output["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--project ID")
    );
    assert_eq!(repo.files(), before);
}

#[tokio::test]
async fn an_older_project_with_the_same_url_is_never_taken_for_the_new_one() {
    let (repo, service) = fixture();
    service.appear_after(3, CREATED, "billing-two", REMOTE);
    let answers = ["n", "", "both", "", "", ""];
    let mut host = TestHost::new(TOKEN).answering(&answers);
    let (result, _) = adopt(&repo, &service, &mut host, AdoptArgs::default(), false).await;
    assert_eq!(result.unwrap()["result"]["project_id"], CREATED);
    let asked = host.prompts.join("\n");
    assert!(asked.contains("Use these values? [Y/n]"), "{asked}");
    assert!(
        asked.contains(&format!(
            "Use project \"billing-two\" (id {CREATED})? [Y/n]"
        )),
        "{asked}"
    );
}

#[tokio::test]
async fn declining_the_created_project_writes_nothing() {
    let (repo, service) = unmatched_fixture();
    service.appear_after(2, CREATED, "billing", REMOTE);
    let before = repo.files();
    let mut host = TestHost::new(TOKEN).answering(&["", "both", "", "", "n"]);
    let (result, _) = adopt(&repo, &service, &mut host, AdoptArgs::default(), false).await;
    let failure = result.unwrap_err();
    assert_eq!((failure.exit, code(&failure)), (2, "cancelled"));
    assert_eq!(repo.files(), before);
}

#[tokio::test]
async fn transport_errors_while_waiting_back_off_and_do_not_end_the_wait() {
    let (repo, service) = unmatched_fixture();
    {
        let mut state = service.state.lock().unwrap();
        state.drop_after = 2;
        state.drop_listings = 3;
    }
    service.appear_after(2, CREATED, "billing", REMOTE);
    let mut host = TestHost::new(TOKEN).silent();
    let (result, _) = adopt(&repo, &service, &mut host, yes(), false).await;
    assert_eq!(result.unwrap()["result"]["project_id"], CREATED);
    assert_eq!(
        service.state.lock().unwrap().drop_listings,
        0,
        "every dropped listing was attempted"
    );
}

#[tokio::test]
async fn a_silent_run_without_yes_opens_nothing() {
    let (repo, service) = unmatched_fixture();
    let mut host = TestHost::new(TOKEN).silent();
    let (result, _) = adopt(&repo, &service, &mut host, AdoptArgs::default(), false).await;
    assert_eq!(code(&result.unwrap_err()), "needs_input");
    assert!(host.opened.is_empty());
    assert_eq!(
        listings(&service),
        1,
        "only preflight read the project list"
    );
}

/// Tests of the backlog migration step. They start real (fake) assistant
/// executables, so they are limited to Unix, where a shell script is one.
#[cfg(unix)]
mod migration {
    use std::path::{Path, PathBuf};

    use super::super::{AgentArg, MigrateArg, migrate};
    use super::*;

    const CLAUDE_HELP: &str = "Usage: claude [options] [command] [prompt]\n  --add-dir <directories...>  More directories";
    const CODEX_HELP: &str = "Usage: codex [OPTIONS] [PROMPT]\n      --add-dir <DIR>";

    /// Writes a fake assistant that prints `help` for `--help` and otherwise
    /// records its arguments, working directory and session variable, then exits 3.
    fn fake_assistant(host: &mut TestHost, name: &str, help: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let record = host.state.path().join(format!("{name}.record"));
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then\ncat <<'HELP'\n{help}\nHELP\nexit 0\nfi\n{{\n\
             for argument in \"$@\"; do printf 'arg=%s\\n' \"$argument\"; done\n\
             printf 'cwd=%s\\n' \"$(pwd)\"\nprintf 'session=%s\\n' \"$AGENT_COORDINATOR_SESSION\"\n\
             }} > '{}'\nexit 3\n",
            record.display()
        );
        let program = host.state.path().join(name);
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        host.program_paths.push((name.to_owned(), program));
        record
    }

    /// An interactive host with python3 and a fake Claude Code; returns the fake's record file.
    fn host_with_claude(answers: &[&str]) -> (TestHost, PathBuf) {
        let mut host = TestHost::new(TOKEN).answering(answers);
        host.programs.push("python3".to_owned());
        let record = fake_assistant(&mut host, "claude", CLAUDE_HELP);
        (host, record)
    }

    /// A repository with a committed handoff file and its matching project.
    fn fixture_with_handoff() -> (TestRepo, FakeService) {
        let (repo, service) = fixture();
        repo.commit_file("HANDOFF.md", "# Handoff\n\nNext: ship it.\n");
        (repo, service)
    }

    /// The recorded lines of a fake assistant run.
    fn recorded(record: &Path) -> Vec<String> {
        std::fs::read_to_string(record)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[tokio::test]
    async fn an_accepted_offer_starts_claude_with_the_kit_and_no_bypass_option() {
        let (repo, service) = fixture_with_handoff();
        let (mut host, record) = host_with_claude(&["", "", "", ""]);
        let (result, output) = adopt(&repo, &service, &mut host, AdoptArgs::default(), false).await;
        let value = result.as_ref().unwrap();
        assert_eq!(
            value["result"]["status"], "connected",
            "the migration never changes the outcome"
        );
        assert_eq!(
            value["result"]["migration"],
            serde_json::json!({"status": "finished", "exit_status": 3})
        );
        let kit = migrate::materialize_kit(&host).unwrap();
        let prompt = migrate::prompt(&kit);
        let launch = &host.launched[0];
        assert_eq!(
            launch.args,
            [
                prompt.clone(),
                "--add-dir".to_owned(),
                kit.display().to_string()
            ]
        );
        let lines = recorded(&record);
        let expected = [
            format!("arg={prompt}"),
            "arg=--add-dir".to_owned(),
            format!("arg={}", kit.display()),
            format!("cwd={}", repo.root().display()),
            "session=adopt-migrate-0123abcd".to_owned(),
        ];
        assert_eq!(lines, expected);
        let forbidden = [
            "dangerous",
            "bypass",
            "skip-permissions",
            "permission-mode",
            "yolo",
            "full-auto",
        ];
        assert!(
            !launch
                .args
                .iter()
                .any(|arg| forbidden.iter().any(|word| arg.contains(word)))
        );
        assert!(
            host.prompts
                .last()
                .unwrap()
                .starts_with("Start a session now to migrate HANDOFF.md (1 KiB)?")
        );
        assert!(
            output.contains("Migration      offered afterwards: HANDOFF.md (1 KiB)"),
            "{output}"
        );
        assert_eq!(
            repo.git(&["status", "--porcelain"]),
            "",
            "adopt committed before the session started"
        );
        assert_no_token(&repo, &output, &result);
    }

    #[tokio::test]
    async fn migrate_now_in_a_connected_repository_starts_codex_without_asking() {
        let (repo, service) = fixture_with_handoff();
        let mut first = TestHost::new(TOKEN).silent();
        adopt(&repo, &service, &mut first, yes(), false)
            .await
            .0
            .unwrap();
        let mut host = TestHost::new(TOKEN);
        host.programs.push("python3".to_owned());
        host.session = Some("my-own-session".to_owned());
        let record = fake_assistant(&mut host, "codex", CODEX_HELP);
        let args = AdoptArgs {
            migrate: MigrateArg::Now,
            migration_agent: Some(AgentArg::Codex),
            ..AdoptArgs::default()
        };
        let (result, _) = adopt(&repo, &service, &mut host, args, false).await;
        assert_eq!(result.unwrap()["result"]["status"], "already_connected");
        assert!(
            host.prompts.is_empty(),
            "repair mode and --migrate now ask nothing: {:?}",
            host.prompts
        );
        let kit = migrate::materialize_kit(&host).unwrap();
        assert_eq!(
            host.launched[0].args,
            [migrate::prompt(&kit)],
            "codex gets the prompt alone"
        );
        assert!(
            host.launched[0].env.is_empty(),
            "a session name already set is left alone"
        );
        assert_eq!(
            recorded(&record)[1],
            format!("cwd={}", repo.root().display())
        );
    }

    /// Runs adopt on a repository with a handoff file and returns the migration summary and output.
    async fn migration_of(host: &mut TestHost, args: AdoptArgs) -> (Value, String) {
        let (repo, service) = fixture_with_handoff();
        let (result, output) = adopt(&repo, &service, host, args, false).await;
        (result.unwrap()["result"]["migration"].clone(), output)
    }

    #[tokio::test]
    async fn every_skip_gives_a_reason_and_the_command_for_later() {
        let off = AdoptArgs {
            migrate: MigrateArg::Off,
            ..AdoptArgs::default()
        };
        let codex = AdoptArgs {
            migration_agent: Some(AgentArg::Codex),
            ..AdoptArgs::default()
        };
        let (mut silent, _) = host_with_claude(&[]);
        silent.interactive = false;
        let (mut declined, _) = host_with_claude(&["", "", "", "n"]);
        let (mut switched_off, _) = host_with_claude(&["", "", ""]);
        let (mut no_python, _) = host_with_claude(&["", "", ""]);
        no_python.programs.retain(|program| program != "python3");
        let (mut no_codex, _) = host_with_claude(&["", "", ""]);
        let cases: [(&mut TestHost, AdoptArgs, &str); 5] = [
            (&mut silent, yes(), "needs a terminal"),
            (&mut declined, AdoptArgs::default(), "declined"),
            (&mut switched_off, off, "--migrate off"),
            (
                &mut no_python,
                AdoptArgs::default(),
                "python3 is not on PATH",
            ),
            (&mut no_codex, codex, "codex is not on PATH"),
        ];
        for (host, args, reason) in cases {
            let (migration, output) = migration_of(host, args).await;
            assert_eq!(migration["status"], "skipped", "{reason}");
            assert!(
                migration["reason"].as_str().unwrap().contains(reason),
                "{migration}"
            );
            assert!(
                output.contains("Start it later with: agent-coordinator adopt --migrate now"),
                "{output}"
            );
            assert!(host.launched.is_empty(), "{reason}");
        }
    }

    #[tokio::test]
    async fn nothing_is_offered_without_sources_or_after_a_migration() {
        let (plain, service) = fixture();
        let (mut host, _) = host_with_claude(&["", "", ""]);
        let (result, _) = adopt(&plain, &service, &mut host, AdoptArgs::default(), false).await;
        assert_eq!(result.unwrap()["result"]["migration"]["status"], "skipped");
        assert_eq!(
            host.prompts.len(),
            3,
            "no migration question: {:?}",
            host.prompts
        );

        let (migrated, service) = fixture_with_handoff();
        migrated.commit_file("docs/pre-coordinator/BACKLOG.md", "archived\n");
        let (mut host, _) = host_with_claude(&["", "", ""]);
        let (result, output) =
            adopt(&migrated, &service, &mut host, AdoptArgs::default(), false).await;
        assert!(
            result.unwrap()["result"]["migration"]["reason"]
                .as_str()
                .unwrap()
                .contains("already migrated")
        );
        assert!(host.launched.is_empty() && !output.contains("Migration      offered"));
    }

    #[tokio::test]
    async fn an_assistant_without_a_prompt_argument_gets_the_prompt_to_paste() {
        let (repo, service) = fixture_with_handoff();
        let mut host = TestHost::new(TOKEN).answering(&["", "", "", ""]);
        host.programs.push("python3".to_owned());
        fake_assistant(&mut host, "claude", "Usage: claude [options]");
        let (result, output) = adopt(&repo, &service, &mut host, AdoptArgs::default(), false).await;
        assert_eq!(result.unwrap()["result"]["migration"]["status"], "skipped");
        assert!(host.launched.is_empty());
        let kit = migrate::materialize_kit(&host).unwrap();
        assert!(
            output.contains("paste this prompt:") && output.contains(&migrate::prompt(&kit)),
            "{output}"
        );
    }
}
