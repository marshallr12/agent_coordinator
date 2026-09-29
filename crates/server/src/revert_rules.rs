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
use crate::{error::AppError, reverts::AWAITING_CANDIDATE};
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

/// When the subject is a mechanical revert, cancels it and records the
/// review that rejected the decision to revert; other subjects are left
/// to the ordinary revision flow.
pub(crate) async fn reject_mechanical(
    c: &mut SqliteConnection,
    r: &Rejection<'_>,
    now: i64,
) -> Result<(), AppError> {
    if !is_mechanical(c, r.task).await? {
        return Ok(());
    }
    let record = json!({"activity_id": r.activity, "reviewer_id": r.reviewer,
        "summary": r.summary, "rejected_at": coordinator_core::timestamp(now)});
    sqlx::query("UPDATE task_reverts SET rejection_json=? WHERE task_id=?")
        .bind(record.to_string())
        .bind(r.task)
        .execute(&mut *c)
        .await?;
    sqlx::query("UPDATE tasks SET lifecycle='canceled',blocked_reason=NULL WHERE id=?")
        .bind(r.task)
        .execute(&mut *c)
        .await?;
    Ok(())
}
