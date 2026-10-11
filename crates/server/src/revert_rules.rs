//! Rules that keep a mechanical revert (planning plan-final §2.4a "M6")
//! in the integrator's hands and its decision independently judged.
//!
//! While a revert's `mode` is `mechanical` only the integrator produces its
//! candidate: agents and humans cannot claim or unblock it, and a revise
//! that reopens it blocks it again until the integrator records a new
//! candidate. Its creator (for an automatic revert, the author whose
//! withdraw lost to the push) and every contributor of the reverted task
//! are recorded contributors, so they cannot review it. A review that requests changes on a mechanical revert rejects
//! the decision to revert: the revert task is canceled with the review's
//! rationale. Once the integrator reports it cannot revert mechanically the
//! task is ordinary implementation work and none of these rules apply.
//! Canceling, archiving, restoring or deleting a revert follows the origin
//! rule for any task; the integrator ignores a revert that
//! is canceled or archived. An agent that is not independent of the reverted
//! change may not cancel or archive the revert (`revert_author_conflict`).
use crate::{error::AppError, mutation::Mutation, reverts::AWAITING_CANDIDATE};
use serde_json::json;
use sqlx::SqliteConnection;

/// The contributors a new revert task records.
pub(crate) struct Creator<'a> {
    /// The revert task.
    pub task: &'a str,
    /// The principal that decided the revert.
    pub principal: &'a str,
    /// Their session, when the request carried one.
    pub session: Option<&'a str>,
    /// The reverted task, whose contributors are copied.
    pub original: &'a str,
}

/// Records the revert's creator as a contributor to it, with their session
/// (or a stand-in when there is none), and every contributor of the
/// reverted task, so neither the decider nor the reverted work's authors
/// can review (and so veto) the revert.
pub(crate) async fn record_creator(
    c: &mut SqliteConnection,
    creator: &Creator<'_>,
    now: i64,
) -> Result<(), AppError> {
    let session = creator.session.map_or_else(
        || format!("revert-creator:{}", creator.principal),
        str::to_owned,
    );
    crate::workflow::record_contributor(c, creator.task, creator.principal, &session, now).await?;
    sqlx::query("INSERT OR IGNORE INTO task_contributors(task_id,principal_id,session_id,first_contributed_at) SELECT ?,principal_id,session_id,? FROM task_contributors WHERE task_id=?")
        .bind(creator.task).bind(now).bind(creator.original)
        .execute(&mut *c).await?;
    Ok(())
}

/// True when `task` is a revert whose candidate the integrator computes.
pub(crate) async fn is_mechanical(c: &mut SqliteConnection, task: &str) -> Result<bool, AppError> {
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM task_reverts WHERE task_id=? AND mode='mechanical'",
    )
    .bind(task)
    .fetch_one(&mut *c)
    .await?;
    Ok(n > 0)
}

/// Refuses claiming or unblocking a mechanical revert.
pub(crate) async fn ensure_not_mechanical(
    c: &mut SqliteConnection,
    task: &str,
) -> Result<(), AppError> {
    if is_mechanical(c, task).await? {
        return Err(AppError::conflict(
            "revert_awaits_integrator",
            "The integrator computes this revert's candidate; it cannot be claimed or unblocked.",
        ));
    }
    Ok(())
}

/// The tasks whose work the revert of `task` undoes: the reverted task, the
/// task of every submission whose candidate or landing range shares a commit
/// with the reverted result's landing range, and the other tasks the result
/// recorded as contributors. Empty when `task` is not a revert.
async fn reverted_tasks(
    c: &mut SqliteConnection,
    project: &str,
    task: &str,
) -> Result<Vec<String>, AppError> {
    Ok(sqlx::query_scalar("SELECT tr.original_task_id FROM task_reverts tr WHERE tr.project_id=? AND tr.task_id=? \
        UNION SELECT s.task_id FROM task_reverts tr JOIN integrator_results r ON r.id=tr.result_id JOIN submissions s ON s.project_id=tr.project_id \
        WHERE tr.project_id=? AND tr.task_id=? AND (s.id=r.submission_id OR s.candidate_revision IN (SELECT value FROM json_each(r.landing_range_json)) \
        OR s.id IN (SELECT o.submission_id FROM integrator_results o, json_each(o.landing_range_json) j \
        WHERE o.project_id=tr.project_id AND j.value IN (SELECT value FROM json_each(r.landing_range_json)))) \
        UNION SELECT j.value FROM task_reverts tr JOIN integrator_results r ON r.id=tr.result_id, json_each(r.contributor_tasks_json) j \
        WHERE tr.project_id=? AND tr.task_id=? AND r.contributor_tasks_json IS NOT NULL")
        .bind(project).bind(task).bind(project).bind(task).bind(project).bind(task)
        .fetch_all(&mut *c).await?)
}

/// Refuses an agent canceling or archiving a revert task when it is not
/// independent of the reverted change, with `revert_author_conflict`. It applies
/// the checks a reviewer of the reverted work must pass: no recorded contribution
/// to the reverted task or any task in the revert's landing range (by principal,
/// session or shared subagent identity), and a launch no such contributor
/// shares (`workflow::shares_launch`, fail closed). Humans, other actions and
/// tasks that are not reverts pass.
pub(crate) async fn ensure_independent_revert_exit(
    c: &mut SqliteConnection,
    actor: &crate::auth::Actor,
    (p, task): (&str, &str),
    action: &str,
) -> Result<(), AppError> {
    if actor.kind == "human" || !matches!(action, "cancel" | "archive") {
        return Ok(());
    }
    let tasks = reverted_tasks(c, p, task).await?;
    let session = actor.session_id.as_deref().unwrap_or_default();
    for reverted in &tasks {
        if crate::workflow::contributed(c, p, reverted, &actor.id, session).await?
            || crate::workflow::shares_launch(c, reverted, session).await?
        {
            return Err(AppError::conflict(
                "revert_author_conflict",
                "This agent contributed to the change this revert undoes, or shares a launch with a contributor. An independent agent or a human must cancel or archive the revert.",
            ));
        }
    }
    Ok(())
}

/// Blocks a reopened mechanical revert again until the integrator records
/// a new candidate.
pub(crate) async fn restore_awaiting(c: &mut SqliteConnection, task: &str) -> Result<(), AppError> {
    sqlx::query("UPDATE tasks SET blocked_reason=? WHERE id=? AND EXISTS(SELECT 1 FROM task_reverts WHERE task_id=? AND mode='mechanical')")
        .bind(AWAITING_CANDIDATE).bind(task).bind(task).execute(&mut *c).await?;
    Ok(())
}

/// The review that requested changes on a subject.
pub(crate) struct Rejection<'a> {
    /// The subject task.
    pub task: &'a str,
    /// The review activity that decided.
    pub activity: &'a str,
    /// The reviewer principal.
    pub reviewer: &'a str,
    /// The review summary: the rationale for rejecting the revert.
    pub summary: &'a str,
}

/// When the subject is a mechanical revert, cancels it (a new task
/// revision) and records the review that rejected the decision to revert;
/// other subjects are left to the ordinary revision flow.
pub(crate) async fn reject_mechanical(
    m: &mut Mutation,
    p: &str,
    r: &Rejection<'_>,
) -> Result<(), AppError> {
    if !is_mechanical(&mut m.tx, r.task).await? {
        return Ok(());
    }
    let record = json!({"activity_id": r.activity, "reviewer_id": r.reviewer,
        "summary": r.summary, "rejected_at": coordinator_core::timestamp(m.now)});
    sqlx::query("UPDATE task_reverts SET rejection_json=? WHERE task_id=?")
        .bind(record.to_string())
        .bind(r.task)
        .execute(&mut *m.tx)
        .await?;
    sqlx::query(
        "UPDATE tasks SET lifecycle='canceled',blocked_reason=NULL,revision=revision+1 WHERE id=?",
    )
    .bind(r.task)
    .execute(&mut *m.tx)
    .await?;
    crate::coordination::save_task_revision(m, p, r.task).await?;
    Ok(())
}
