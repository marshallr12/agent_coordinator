//! Thin agent lifecycle commands: `revise`, `unblock` and `cancel`. Each maps to one
//! service mutation and uses the shared durable mutation journal, so an interrupted
//! call is retried with the same idempotency key.

use crate::{ContextData, Failure, mutate, validate_segment};
use clap::Args;
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

/// Unblock or cancel a task at an exact revision with a recorded rationale.
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
    let mut body = json!({"expected_revision":args.lifecycle.expected_revision,
        "reason":args.lifecycle.reason});
    if let Some(replacement) = &args.replacement {
        validate_segment("replacement", replacement).map_err(Failure::invalid)?;
        body["replacement_task_id"] = json!(replacement);
    }
    mutate(cli, context, &path, body, true).await
}

/// Build `/api/v1/projects/{project}/tasks/{task}/{action}` after validating the id.
fn task_path(context: &ContextData, task: &str, action: &str) -> Result<String, Failure> {
    validate_segment("task", task).map_err(Failure::invalid)?;
    Ok(format!(
        "/api/v1/projects/{}/tasks/{task}/{action}",
        context.binding.project_id
    ))
}

#[cfg(test)]
mod tests {
    use crate::Cli;
    use clap::Parser;

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
}
