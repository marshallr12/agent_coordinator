//! Thin agent lifecycle commands: `revise`, `unblock`, `cancel`, `archive`, `restore`
//! and `delete`. Each maps to one
//! service mutation and uses the shared durable mutation journal, so an interrupted
//! call is retried with the same idempotency key.

use crate::{ContextData, Failure, mutate, mutate_method, validate_segment};
use clap::Args;
use coordinator_client::HttpMethod;
use serde_json::{Value, json};

/// Revise (reopen) a task's current submission with a reason code and evidence.
#[derive(Args)]
pub(crate) struct ReviseArgs {
    #[arg(long)]
    task: String,
    /// The exact current submission id.
    #[arg(long)]
    submission: String,
    /// conflict, check_failed, candidate_missing, requirements_changed or author_withdraw.
    #[arg(long)]
    reason_code: String,
    /// Human-readable reason recorded with the reopen.
    #[arg(long)]
    reason: String,
    /// Evidence for the reason code (required for conflict and check_failed).
    #[arg(long)]
    evidence: Option<String>,
}

/// Act on a task at an exact revision with a recorded rationale.
#[derive(Args)]
pub(crate) struct LifecycleArgs {
    #[arg(long)]
    task: String,
    /// The task revision this action applies to.
    #[arg(long)]
    expected_revision: i64,
    #[arg(long)]
    reason: String,
}

/// Cancel a task, optionally naming the task that replaces it.
#[derive(Args)]
pub(crate) struct CancelArgs {
    #[command(flatten)]
    lifecycle: LifecycleArgs,
    /// Task that replaces the canceled one (e.g. a wrong-kind task).
    #[arg(long)]
    replacement: Option<String>,
}

/// POST `workflow/reopen` with the agent revise reason.
pub(crate) async fn revise(
    cli: &crate::Cli,
    context: &ContextData,
    args: &ReviseArgs,
) -> Result<Value, Failure> {
    let path = task_path(context, &args.task, "workflow/reopen")?;
    let body = json!({"submission_id":args.submission,"reason":args.reason,
        "reason_code":args.reason_code,"evidence":args.evidence});
    mutate(cli, context, &path, body, true).await
}

/// POST `unblock` for a blocked task.
pub(crate) async fn unblock(
    cli: &crate::Cli,
    context: &ContextData,
    args: &LifecycleArgs,
) -> Result<Value, Failure> {
    let path = task_path(context, &args.task, "unblock")?;
    let body = json!({"expected_revision":args.expected_revision,"reason":args.reason});
    mutate(cli, context, &path, body, true).await
}

/// POST `cancel`, recording an optional replacement task.
pub(crate) async fn cancel(
    cli: &crate::Cli,
    context: &ContextData,
    args: &CancelArgs,
) -> Result<Value, Failure> {
    let path = task_path(context, &args.lifecycle.task, "cancel")?;
    let mut body = lifecycle_body(&args.lifecycle);
    if let Some(replacement) = &args.replacement {
        validate_segment("replacement", replacement).map_err(Failure::invalid)?;
        body["replacement_task_id"] = json!(replacement);
    }
    mutate(cli, context, &path, body, true).await
}

/// POST `archive`, hiding the task from the queue while keeping its records.
pub(crate) async fn archive(
    cli: &crate::Cli,
    context: &ContextData,
    args: &LifecycleArgs,
) -> Result<Value, Failure> {
    let path = task_path(context, &args.task, "archive")?;
    mutate(cli, context, &path, lifecycle_body(args), true).await
}

/// POST `restore`, returning an archived task to the queue.
pub(crate) async fn restore(
    cli: &crate::Cli,
    context: &ContextData,
    args: &LifecycleArgs,
) -> Result<Value, Failure> {
    let path = task_path(context, &args.task, "restore")?;
    mutate(cli, context, &path, lifecycle_body(args), true).await
}

/// DELETE the task itself; the service refuses tasks that have history.
pub(crate) async fn delete(
    cli: &crate::Cli,
    context: &ContextData,
    args: &LifecycleArgs,
) -> Result<Value, Failure> {
    let path = task_path(context, &args.task, "")?;
    mutate_method(
        cli,
        context,
        HttpMethod::Delete,
        &path,
        lifecycle_body(args),
        true,
    )
    .await
}

fn lifecycle_body(args: &LifecycleArgs) -> Value {
    json!({"expected_revision":args.expected_revision,"reason":args.reason})
}

/// One-line guidance for a lifecycle refusal code the service can return.
pub(crate) fn refusal_hint(code: &str) -> Option<&'static str> {
    Some(match code {
        "task_revision_changed" => {
            "the task changed; run `tasks show` and retry with its current revision"
        }
        "task_attempt_protected" => {
            "a live attempt owns the task; release or resolve the attempt first"
        }
        "task_workflow_protected" => {
            "review or integration work is active on this task; resolve it first"
        }
        "task_archived" => "the task is archived; run `restore` before any other action",
        "task_lifecycle_invalid" => "the task's lifecycle state does not permit this action",
        "task_history_protected" => {
            "the task has attempts, dependencies or workflow links and cannot be deleted; archive it instead"
        }
        _ => return None,
    })
}

/// Build `/api/v1/projects/{project}/tasks/{task}[/{action}]` after validating the id.
fn task_path(context: &ContextData, task: &str, action: &str) -> Result<String, Failure> {
    validate_segment("task", task).map_err(Failure::invalid)?;
    let mut path = format!(
        "/api/v1/projects/{}/tasks/{task}",
        context.binding.project_id
    );
    if !action.is_empty() {
        path.push('/');
        path.push_str(action);
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use crate::Cli;
    use clap::Parser;
    use serde_json::json;

    /// Parse a command line, reporting only whether clap accepted it.
    fn parses(args: &[&str]) -> bool {
        Cli::try_parse_from(std::iter::once("agent-coordinator").chain(args.iter().copied()))
            .is_ok()
    }

    #[test]
    fn revise_requires_task_submission_code_and_reason() {
        assert!(parses(&[
            "revise",
            "--task",
            "t",
            "--submission",
            "s",
            "--reason-code",
            "conflict",
            "--reason",
            "r",
            "--evidence",
            "CONFLICT a.rs"
        ]));
        assert!(!parses(&[
            "revise",
            "--task",
            "t",
            "--submission",
            "s",
            "--reason",
            "r"
        ]));
    }

    #[test]
    fn unblock_and_cancel_require_an_exact_revision() {
        assert!(parses(&[
            "unblock",
            "--task",
            "t",
            "--expected-revision",
            "3",
            "--reason",
            "resource exists"
        ]));
        assert!(parses(&[
            "cancel",
            "--task",
            "t",
            "--expected-revision",
            "3",
            "--reason",
            "wrong kind",
            "--replacement",
            "u"
        ]));
        assert!(!parses(&[
            "cancel",
            "--task",
            "t",
            "--reason",
            "wrong kind"
        ]));
    }

    #[test]
    fn archive_restore_delete_require_an_exact_revision_and_reason() {
        for action in ["archive", "restore", "delete"] {
            assert!(parses(&[
                action,
                "--task",
                "t",
                "--expected-revision",
                "3",
                "--reason",
                "cleanup"
            ]));
            assert!(!parses(&[action, "--task", "t", "--reason", "cleanup"]));
            assert!(!parses(&[
                action,
                "--task",
                "t",
                "--expected-revision",
                "3"
            ]));
            assert!(!parses(&[
                action,
                "--task",
                "t",
                "--expected-revision",
                "3",
                "--reason",
                "cleanup",
                "--replacement",
                "u"
            ]));
        }
    }

    #[test]
    fn help_names_the_origin_rule() {
        use clap::CommandFactory;
        for action in ["cancel", "archive", "restore", "delete"] {
            let help = Cli::command()
                .find_subcommand_mut(action)
                .unwrap()
                .render_long_help()
                .to_string();
            for needle in ["origin", "`agent`", "`service`", "human"] {
                assert!(
                    help.contains(needle),
                    "{action} help lacks {needle}: {help}"
                );
            }
        }
    }

    #[test]
    fn every_lifecycle_refusal_code_has_a_hint() {
        for code in [
            "task_revision_changed",
            "task_attempt_protected",
            "task_workflow_protected",
            "task_archived",
            "task_lifecycle_invalid",
            "task_history_protected",
        ] {
            assert!(super::refusal_hint(code).is_some(), "{code}");
        }
        assert!(super::refusal_hint("record_not_found").is_none());
    }

    #[test]
    fn task_summary_shows_the_origin() {
        let lines = crate::task_summary(&json!({"id":"t","title":"T","origin":"service",
            "lifecycle":"open","work_status":"ready","revision":4}));
        assert!(lines.contains(&"Origin: service".to_owned()), "{lines:?}");
    }

    /// Tests that drive the real commands and mutation journal against a loopback
    /// stand-in for the service, which answers an agent session as the service does.
    mod service {
        use crate::{Cli, ContextData, Failure, config, retry, state};
        use clap::Parser;
        use coordinator_client::{CoordinatorClient, SessionAuth};
        use serde_json::{Value, json};
        use sha2::{Digest, Sha256};
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};

        #[derive(Clone, Debug)]
        struct Seen {
            method: String,
            path: String,
            key: String,
            body: Value,
        }

        struct Fixture {
            _state: tempfile::TempDir,
            cli_args: Vec<String>,
            origin: String,
            seen: Arc<Mutex<Vec<Seen>>>,
            flaky: Arc<AtomicBool>,
        }

        /// The service's answer for a task id of the form `{origin}-{case}`.
        fn answer(method: &str, path: &str) -> (u16, Value) {
            let id = path
                .rsplit('/')
                .find(|segment| segment.contains('-'))
                .unwrap_or_default();
            let (origin, case) = id.split_once('-').unwrap_or_default();
            let action = match method {
                "DELETE" => "delete",
                _ => path.rsplit('/').next().unwrap_or_default(),
            };
            let error = |status: u16, code: &str, details: Value| {
                (
                    status,
                    json!({"error":{"code":code,"message":"refused","details":details,
                        "next_actions":[],"retryable":false}}),
                )
            };
            // An agent session on a human-origin task has no delegation here.
            if origin == "human" {
                return error(
                    403,
                    "forbidden",
                    json!({"required_actor":"human","gate":format!("task_{action}")}),
                );
            }
            match case {
                "stale" => error(409, "task_revision_changed", json!({})),
                "attempt" => error(409, "task_attempt_protected", json!({})),
                "workflow" => error(409, "task_workflow_protected", json!({})),
                "archived" => error(409, "task_archived", json!({})),
                "invalid" => error(409, "task_lifecycle_invalid", json!({})),
                "history" => error(409, "task_history_protected", json!({})),
                _ => (
                    200,
                    json!({"data":{"id":id,"deleted":action == "delete",
                        "archived":action == "archive"}}),
                ),
            }
        }

        fn serve(mut stream: TcpStream, seen: &Mutex<Vec<Seen>>, flaky: &AtomicBool) {
            let mut raw = Vec::new();
            let mut chunk = [0u8; 4096];
            let (head, mut body) = loop {
                let n = stream.read(&mut chunk).unwrap();
                raw.extend_from_slice(&chunk[..n]);
                if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                    break (
                        String::from_utf8_lossy(&raw[..end]).into_owned(),
                        raw[end + 4..].to_vec(),
                    );
                }
                assert!(n > 0, "request ended before its headers");
            };
            let mut lines = head.lines();
            let mut request = lines.next().unwrap().split(' ');
            let (method, path) = (request.next().unwrap(), request.next().unwrap());
            let header = |name: &str| {
                lines
                    .clone()
                    .filter_map(|line| line.split_once(": "))
                    .find(|(k, _)| k.eq_ignore_ascii_case(name))
                    .map(|(_, v)| v.to_owned())
                    .unwrap_or_default()
            };
            let length: usize = header("content-length").parse().unwrap_or(0);
            while body.len() < length {
                let n = stream.read(&mut chunk).unwrap();
                assert!(n > 0, "request ended before its body");
                body.extend_from_slice(&chunk[..n]);
            }
            seen.lock().unwrap().push(Seen {
                method: method.into(),
                path: path.into(),
                key: header("idempotency-key"),
                body: serde_json::from_slice(&body).unwrap_or(Value::Null),
            });
            let (status, value) = if flaky.swap(false, Ordering::SeqCst) {
                (
                    503,
                    json!({"error":{"code":"unavailable","message":"later"}}),
                )
            } else {
                answer(method, path)
            };
            let mut value = value;
            value["request_id"] = json!("request");
            value["server_time"] = json!("2026-01-01T00:00:00.000Z");
            let payload = value.to_string();
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                payload.len()
            );
        }

        fn fixture() -> (Fixture, ContextData) {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let seen = Arc::new(Mutex::new(Vec::new()));
            let flaky = Arc::new(AtomicBool::new(false));
            let (thread_seen, thread_flaky) = (seen.clone(), flaky.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    serve(stream, &thread_seen, &thread_flaky);
                }
            });
            let directory = tempfile::tempdir().unwrap();
            let context = ContextData {
                binding_path: "binding.toml".into(),
                binding: config::RepositoryBinding {
                    service_url: origin.clone(),
                    project_id: "project".into(),
                    project_name: None,
                },
                origin: origin.clone(),
                client: CoordinatorClient::new(&origin, "fixture-token", true).unwrap(),
                credential_digest: hex::encode(Sha256::digest(b"fixture-token")),
            };
            let path =
                state::path_for(Some(directory.path()), &origin, "project", "local").unwrap();
            let saved = state::SessionState::new(
                origin.clone(),
                "project".into(),
                "local".into(),
                context.credential_digest.clone(),
                SessionAuth {
                    id: "session-id".into(),
                    proof: "fixture-proof".into(),
                },
                "machine".into(),
                "harness".into(),
                vec!["code".into()],
            );
            state::save(&path, &saved).unwrap();
            let cli_args = vec![
                "agent-coordinator".into(),
                "--session".into(),
                "local".into(),
                "--state-dir".into(),
                directory.path().to_str().unwrap().into(),
            ];
            (
                Fixture {
                    _state: directory,
                    cli_args,
                    origin,
                    seen,
                    flaky,
                },
                context,
            )
        }

        impl Fixture {
            fn cli(&self, extra: &[&str]) -> Cli {
                Cli::try_parse_from(
                    self.cli_args
                        .iter()
                        .map(String::as_str)
                        .chain(extra.iter().copied()),
                )
                .unwrap()
            }

            async fn act(
                &self,
                context: &ContextData,
                action: &str,
                task: &str,
            ) -> Result<Value, Failure> {
                let cli = self.cli(&[
                    action,
                    "--task",
                    task,
                    "--expected-revision",
                    "2",
                    "--reason",
                    "Cleaning up.",
                ]);
                match &cli.command {
                    crate::Command::Cancel(args) => super::super::cancel(&cli, context, args).await,
                    crate::Command::Archive(args) => {
                        super::super::archive(&cli, context, args).await
                    }
                    crate::Command::Restore(args) => {
                        super::super::restore(&cli, context, args).await
                    }
                    crate::Command::Delete(args) => super::super::delete(&cli, context, args).await,
                    _ => unreachable!(),
                }
            }
        }

        #[tokio::test]
        async fn agent_sessions_succeed_on_agent_and_service_tasks() {
            let (f, context) = fixture();
            for action in ["cancel", "archive", "restore", "delete"] {
                for origin in ["agent", "service"] {
                    let task = format!("{origin}-ok");
                    let value = f.act(&context, action, &task).await.unwrap();
                    assert_eq!(value["data"]["id"], task);
                    let seen = f.seen.lock().unwrap().last().cloned().unwrap();
                    let (method, path) = if action == "delete" {
                        ("DELETE", format!("/api/v1/projects/project/tasks/{task}"))
                    } else {
                        (
                            "POST",
                            format!("/api/v1/projects/project/tasks/{task}/{action}"),
                        )
                    };
                    assert_eq!((seen.method.as_str(), seen.path), (method, path));
                    assert_eq!(
                        seen.body,
                        json!({"expected_revision":2,"reason":"Cleaning up."})
                    );
                    assert!(!seen.key.is_empty());
                }
            }
            assert!(f.origin.starts_with("http://127.0.0.1:"));
        }

        #[tokio::test]
        async fn a_human_task_is_a_human_gate_for_every_action() {
            let (f, context) = fixture();
            for action in ["cancel", "archive", "restore", "delete"] {
                let failure = f.act(&context, action, "human-ok").await.unwrap_err();
                assert_eq!(failure.exit, 4, "{action}");
                let error = &failure.output["error"];
                assert_eq!(error["details"]["required_actor"], "human");
                assert_eq!(error["details"]["gate"], format!("task_{action}"));
            }
        }

        #[tokio::test]
        async fn each_refusal_code_surfaces_on_agent_and_service_tasks() {
            let (f, context) = fixture();
            for (case, code) in [
                ("stale", "task_revision_changed"),
                ("attempt", "task_attempt_protected"),
                ("workflow", "task_workflow_protected"),
                ("archived", "task_archived"),
                ("invalid", "task_lifecycle_invalid"),
                ("history", "task_history_protected"),
            ] {
                for origin in ["agent", "service"] {
                    for action in ["cancel", "archive", "restore", "delete"] {
                        let failure = f
                            .act(&context, action, &format!("{origin}-{case}"))
                            .await
                            .unwrap_err();
                        assert_eq!(failure.exit, 5, "{action} {origin} {case}");
                        assert_eq!(failure.output["error"]["code"], code);
                        assert!(super::super::refusal_hint(code).is_some());
                    }
                }
            }
        }

        #[tokio::test]
        async fn retry_replays_a_failed_lifecycle_action_with_the_same_key() {
            let (f, context) = fixture();
            for action in ["archive", "restore", "delete"] {
                f.seen.lock().unwrap().clear();
                f.flaky.store(true, Ordering::SeqCst);
                let failure = f.act(&context, action, "agent-ok").await.unwrap_err();
                assert_eq!(failure.exit, 7, "{action}");
                let cli = f.cli(&["retry"]);
                let value = retry(&cli, &context).await.unwrap();
                assert_eq!(value["data"]["id"], "agent-ok");
                let seen = f.seen.lock().unwrap().clone();
                assert_eq!(seen.len(), 2, "{action}");
                assert_eq!(seen[0].key, seen[1].key);
                assert_eq!(seen[0].method, seen[1].method);
                assert_eq!(seen[0].path, seen[1].path);
                assert_eq!(seen[0].body, seen[1].body);
                assert_eq!(seen[1].method == "DELETE", action == "delete");
            }
        }
    }
}
