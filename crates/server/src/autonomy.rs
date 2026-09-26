//! P1 autonomy support: scoped pinning of submissions to the judged task fields,
//! and the startup steps that keep existing rows consistent with it.

use crate::error::AppError;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection};

/// Digest of the task fields a reviewer judges: title, description, acceptance
/// criteria and kind. Priority, dependencies and planning state are excluded, so
/// editing them never invalidates a candidate or its approvals.
pub(crate) fn task_digest(
    title: &str,
    description: &str,
    acceptance: &Value,
    kind: &str,
) -> String {
    let canonical = json!([title, description, acceptance, kind]).to_string();
    hex::encode(Sha256::digest(canonical.as_bytes()))
}

/// Digest of a task row's current judged fields.
pub(crate) fn row_digest(
    row: &sqlx::sqlite::SqliteRow,
    kind_column: &str,
) -> Result<String, AppError> {
    let acceptance: Value = serde_json::from_str(&row.get::<String, _>("acceptance_json"))?;
    Ok(task_digest(
        &row.get::<String, _>("title"),
        &row.get::<String, _>("description"),
        &acceptance,
        &row.get::<String, _>(kind_column),
    ))
}

/// Current judged-field digest of one task.
pub(crate) async fn current_task_digest(
    c: &mut SqliteConnection,
    task_id: &str,
) -> Result<String, AppError> {
    let row = sqlx::query("SELECT title,description,acceptance_json,kind FROM tasks WHERE id=?")
        .bind(task_id)
        .fetch_one(&mut *c)
        .await?;
    row_digest(&row, "kind")
}

/// Judged-field digest of a task as recorded at `revision`, if that revision was saved.
pub(crate) async fn revision_task_digest(
    c: &mut SqliteConnection,
    task_id: &str,
    revision: i64,
) -> Result<Option<String>, AppError> {
    let data: Option<String> =
        sqlx::query_scalar("SELECT data_json FROM task_revisions WHERE task_id=? AND revision=?")
            .bind(task_id)
            .bind(revision)
            .fetch_optional(&mut *c)
            .await?;
    let Some(data) = data else { return Ok(None) };
    let v: Value = serde_json::from_str(&data)?;
    Ok(Some(task_digest(
        v["title"].as_str().unwrap_or_default(),
        v["description"].as_str().unwrap_or_default(),
        &v["acceptance_criteria"],
        v["kind"].as_str().unwrap_or_default(),
    )))
}

/// Fill `submissions.task_digest` for rows created before the column existed, from
/// the task revision each submission pinned (or the current task when that revision
/// was never saved). Idempotent: only NULL digests are written.
pub(crate) async fn backfill_task_digests(c: &mut SqliteConnection) -> Result<(), AppError> {
    let rows =
        sqlx::query("SELECT id,task_id,task_revision FROM submissions WHERE task_digest IS NULL")
            .fetch_all(&mut *c)
            .await?;
    for row in rows {
        let task: String = row.get("task_id");
        let digest = match revision_task_digest(c, &task, row.get("task_revision")).await? {
            Some(digest) => digest,
            None => current_task_digest(c, &task).await?,
        };
        sqlx::query("UPDATE submissions SET task_digest=? WHERE id=? AND task_digest IS NULL")
            .bind(digest)
            .bind(row.get::<String, _>("id"))
            .execute(&mut *c)
            .await?;
    }
    Ok(())
}

/// Startup steps run once after migrations, before serving.
pub async fn startup(c: &mut SqliteConnection, now: i64) -> Result<(), AppError> {
    backfill_task_digests(c).await?;
    reconcile_required_reviews(c, None, now).await
}

/// Review activity kinds a project review mode requires.
pub(crate) fn required_review_kinds(mode: &str) -> Vec<String> {
    let kinds: &[&str] = match mode {
        "agent" => &["agent_review"],
        "human" => &["human_review"],
        "either" => &["either_review"],
        "both" => &["agent_review", "human_review"],
        _ => &[],
    };
    kinds.iter().map(|kind| (*kind).to_owned()).collect()
}

/// Match required review kinds against approver classes ("human" or "agent").
/// Satisfaction order is human ≥ either ≥ agent: a human approval satisfies any kind,
/// an agent approval satisfies agent or either reviews, and each approval counts once.
/// When both human and agent reviews are required, the agent review needs an agent.
pub(crate) fn reviews_satisfied(required: &[String], approvers: &[String]) -> bool {
    let count = |kind: &str| required.iter().filter(|k| *k == kind).count();
    let (human, agent, either) = (
        count("human_review"),
        count("agent_review"),
        count("either_review"),
    );
    let mut humans = approvers.iter().filter(|a| *a == "human").count();
    let mut agents = approvers.len() - humans;
    if humans < human {
        return false;
    }
    humans -= human;
    let by_agents = agent.min(agents);
    agents -= by_agents;
    let rest = agent - by_agents;
    if rest > 0 && (human > 0 || humans < rest) {
        return false;
    }
    humans -= rest;
    humans + agents >= either
}

/// True when the submission's approvals satisfy its required reviews. Subjects still
/// in review are judged against the current review mode; subjects past review keep
/// the review set they were approved under.
pub(crate) async fn approvals_satisfied(
    c: &mut SqliteConnection,
    submission: &str,
) -> Result<bool, AppError> {
    let row = sqlx::query("SELECT ws.phase,p.review_mode FROM submissions s JOIN projects p ON p.id=s.project_id LEFT JOIN workflow_subjects ws ON ws.current_submission_id=s.id WHERE s.id=?")
        .bind(submission)
        .fetch_one(&mut *c)
        .await?;
    let required = if row.get::<Option<String>, _>("phase").as_deref() == Some("review") {
        required_review_kinds(&row.get::<String, _>("review_mode"))
    } else {
        sqlx::query_scalar("SELECT kind FROM workflow_activities WHERE submission_id=? AND kind!='integration' AND state!='canceled'")
            .bind(submission)
            .fetch_all(&mut *c)
            .await?
    };
    let approvers: Vec<String> = sqlx::query_scalar("SELECT pr.kind FROM review_decisions rd JOIN principals pr ON pr.id=rd.reviewer_id WHERE rd.submission_id=? AND rd.decision='approved'")
        .bind(submission)
        .fetch_all(&mut *c)
        .await?;
    Ok(reviews_satisfied(&required, &approvers))
}

/// Bring every subject in review up to the project's current review mode, so a
/// policy change never grandfathers a tightened `review_mode` and never strands a
/// candidate: add missing review activities, cancel queued ones no longer required,
/// and advance subjects whose approvals already satisfy the mode. Subjects in
/// integration keep the set they were approved under. Idempotent; runs inside the
/// policy-update transaction and once at startup.
pub(crate) async fn reconcile_required_reviews(
    c: &mut SqliteConnection,
    project: Option<&str>,
    now: i64,
) -> Result<(), AppError> {
    let subjects = sqlx::query("SELECT ws.project_id,ws.task_id,ws.current_submission_id,t.title,p.review_mode FROM workflow_subjects ws JOIN tasks t ON t.id=ws.task_id JOIN projects p ON p.id=ws.project_id WHERE ws.phase='review' AND (?1 IS NULL OR ws.project_id=?1)")
        .bind(project)
        .fetch_all(&mut *c)
        .await?;
    for s in subjects {
        let required = required_review_kinds(&s.get::<String, _>("review_mode"));
        let submission: String = s.get("current_submission_id");
        cancel_unrequired_reviews(c, &submission, &required, now).await?;
        if approvals_satisfied(c, &submission).await? {
            advance_if_quiet(c, &s, &submission, now).await?;
        } else {
            add_missing_reviews(c, &s, &submission, &required, now).await?;
        }
    }
    Ok(())
}

/// Cancel queued review activities whose kind the current mode no longer requires.
/// Active ones may finish, and their approvals still count.
async fn cancel_unrequired_reviews(
    c: &mut SqliteConnection,
    submission: &str,
    required: &[String],
    now: i64,
) -> Result<(), AppError> {
    let required = serde_json::to_string(required)?;
    sqlx::query("UPDATE tasks SET lifecycle='canceled',blocked_reason=NULL WHERE id IN (SELECT activity_task_id FROM workflow_activities WHERE submission_id=? AND state='queued' AND kind!='integration' AND kind NOT IN (SELECT value FROM json_each(?)))")
        .bind(submission).bind(&required).execute(&mut *c).await?;
    sqlx::query("UPDATE workflow_activities SET state='canceled',canceled_at=? WHERE submission_id=? AND state='queued' AND kind!='integration' AND kind NOT IN (SELECT value FROM json_each(?))")
        .bind(now).bind(submission).bind(&required).execute(&mut *c).await?;
    Ok(())
}

/// Queue a review activity for each required kind that has no live activity.
async fn add_missing_reviews(
    c: &mut SqliteConnection,
    s: &sqlx::sqlite::SqliteRow,
    submission: &str,
    required: &[String],
    now: i64,
) -> Result<(), AppError> {
    for kind in required {
        let live: i64 = sqlx::query_scalar("SELECT count(*) FROM workflow_activities WHERE submission_id=? AND kind=? AND state!='canceled'")
            .bind(submission).bind(kind).fetch_one(&mut *c).await?;
        if live == 0 {
            crate::workflow::add_review_activity(
                c,
                &s.get::<String, _>("project_id"),
                &s.get::<String, _>("task_id"),
                &s.get::<String, _>("title"),
                submission,
                kind,
                now,
            )
            .await?;
        }
    }
    Ok(())
}

/// Advance a satisfied subject unless a review is still active; that review's
/// decision advances it later.
async fn advance_if_quiet(
    c: &mut SqliteConnection,
    s: &sqlx::sqlite::SqliteRow,
    submission: &str,
    now: i64,
) -> Result<(), AppError> {
    let active: i64 = sqlx::query_scalar("SELECT count(*) FROM workflow_activities WHERE submission_id=? AND kind!='integration' AND state='active'")
        .bind(submission).fetch_one(&mut *c).await?;
    if active == 0 {
        crate::workflow::advance_approved_subject(
            c,
            &s.get::<String, _>("project_id"),
            &s.get::<String, _>("task_id"),
            submission,
            now,
        )
        .await?;
    }
    Ok(())
}

/// Agent revises allowed per subject in any 24 hours before the subject parks in
/// the human queue.
const REVISE_LIMIT: i64 = 3;
const DAY_MS: i64 = 86_400_000;

/// Number of agent revises (agent-called reopens) of a task's submissions in the
/// last 24 hours.
pub(crate) async fn agent_revise_count(
    c: &mut SqliteConnection,
    task: &str,
    now: i64,
) -> Result<i64, AppError> {
    Ok(sqlx::query_scalar("SELECT count(*) FROM events e JOIN principals pr ON pr.id=e.actor_id JOIN submissions s ON s.id=e.record_id WHERE e.kind='submission.reopened' AND pr.kind='agent' AND s.task_id=? AND e.created_at>?")
        .bind(task).bind(now - DAY_MS).fetch_one(&mut *c).await?)
}

/// True when agents may no longer revise this task until a human looks at it.
pub(crate) async fn revise_limit_reached(
    c: &mut SqliteConnection,
    task: &str,
    now: i64,
) -> Result<bool, AppError> {
    Ok(agent_revise_count(c, task, now).await? >= REVISE_LIMIT)
}

/// What an agent revise needs to be checked against.
pub(crate) struct ReviseRequest<'a> {
    pub project: &'a str,
    pub task: &'a str,
    pub submission: &'a str,
    pub code: Option<&'a str>,
    pub evidence: Option<&'a str>,
}

/// Authorize an agent `revise` (an agent-called reopen) and return the reason record
/// to attach to the result. Requires `recovery_mode=agent`, a closed reason code with
/// its allowed actor, and at most three agent revises per subject per 24 hours.
pub(crate) async fn authorize_revise(
    c: &mut SqliteConnection,
    actor: &crate::auth::Actor,
    r: &ReviseRequest<'_>,
    now: i64,
) -> Result<Value, AppError> {
    let code = r.code.ok_or_else(|| AppError::bad_request("Agents must give reason_code: conflict, check_failed, candidate_missing, requirements_changed or author_withdraw."))?;
    let recovery: String = sqlx::query_scalar("SELECT recovery_mode FROM projects WHERE id=?")
        .bind(r.project)
        .fetch_one(&mut *c)
        .await?;
    if recovery != "agent" {
        return Err(AppError::human_gate(
            "human_reopen_required",
            "This project reserves reopening submissions to a human (recovery_mode is manual).",
        ));
    }
    if revise_limit_reached(c, r.task, now).await? {
        return Err(AppError::new(
            axum::http::StatusCode::FORBIDDEN,
            "revise_limit_reached",
            "Agents revised this task three times in 24 hours; a human must look at it.",
        )
        .with_details(json!({"required_actor":"human","gate":"revise_limit_reached"})));
    }
    ensure_revise_actor(c, actor, r, code, now).await?;
    Ok(json!({"reason_code":code,"evidence":r.evidence}))
}

/// Check the reason code's allowed actor and its service-verifiable evidence.
async fn ensure_revise_actor(
    c: &mut SqliteConnection,
    actor: &crate::auth::Actor,
    r: &ReviseRequest<'_>,
    code: &str,
    now: i64,
) -> Result<(), AppError> {
    let allowed = match code {
        "conflict" | "check_failed" => {
            require_evidence(r.evidence)?;
            holds_integration(c, actor, r.submission, now).await?
        }
        "candidate_missing" => candidate_missing(c, r.submission).await?,
        "requirements_changed" => {
            !is_contributor(c, r.task, &actor.id).await?
                && digest_changed(c, r.submission, r.task).await?
        }
        "author_withdraw" => submission_author(c, r.submission).await? == actor.id,
        _ => return Err(AppError::bad_request("Unknown reason_code.")),
    };
    if !allowed {
        return Err(AppError::conflict(
            "revise_not_permitted",
            "This caller or the recorded state does not permit this revise reason.",
        ));
    }
    Ok(())
}

fn require_evidence(evidence: Option<&str>) -> Result<(), AppError> {
    if evidence.is_none_or(|value| value.trim().is_empty() || value.len() > 16384) {
        return Err(AppError::bad_request(
            "This reason_code needs 1–16384 bytes of evidence.",
        ));
    }
    Ok(())
}

/// True when the caller's session owns the submission's live integration attempt.
async fn holds_integration(
    c: &mut SqliteConnection,
    actor: &crate::auth::Actor,
    submission: &str,
    now: i64,
) -> Result<bool, AppError> {
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM workflow_activities wa JOIN tasks t ON t.id=wa.activity_task_id JOIN attempts a ON a.id=t.current_attempt_id WHERE wa.submission_id=? AND wa.kind='integration' AND wa.state='active' AND a.state='active' AND a.owner_id=? AND a.session_id=? AND a.expires_at>?")
        .bind(submission).bind(&actor.id).bind(actor.session_id.as_deref().unwrap_or("")).bind(now).fetch_one(&mut *c).await?;
    Ok(n > 0)
}

/// True for a code submission without a durable candidate ref (including legacy rows).
async fn candidate_missing(c: &mut SqliteConnection, submission: &str) -> Result<bool, AppError> {
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM submissions WHERE id=? AND kind='code' AND (candidate_ref IS NULL OR candidate_ref='')")
        .bind(submission).fetch_one(&mut *c).await?;
    Ok(n > 0)
}

async fn is_contributor(
    c: &mut SqliteConnection,
    task: &str,
    principal: &str,
) -> Result<bool, AppError> {
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM task_contributors WHERE task_id=? AND principal_id=?",
    )
    .bind(task)
    .bind(principal)
    .fetch_one(&mut *c)
    .await?;
    Ok(n > 0)
}

/// True when the task's judged fields differ from the digest the submission pinned.
pub(crate) async fn digest_changed(
    c: &mut SqliteConnection,
    submission: &str,
    task: &str,
) -> Result<bool, AppError> {
    let pinned: Option<String> =
        sqlx::query_scalar("SELECT task_digest FROM submissions WHERE id=?")
            .bind(submission)
            .fetch_one(&mut *c)
            .await?;
    let current = current_task_digest(c, task).await?;
    Ok(pinned.is_some_and(|pinned| pinned != current))
}

async fn submission_author(c: &mut SqliteConnection, submission: &str) -> Result<String, AppError> {
    Ok(
        sqlx::query_scalar("SELECT created_by FROM submissions WHERE id=?")
            .bind(submission)
            .fetch_one(&mut *c)
            .await?,
    )
}

#[cfg(test)]
mod tests {
    use super::reviews_satisfied;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn human_approval_satisfies_every_kind_but_counts_once() {
        assert!(reviews_satisfied(&v(&["agent_review"]), &v(&["human"])));
        assert!(reviews_satisfied(&v(&["either_review"]), &v(&["human"])));
        assert!(!reviews_satisfied(
            &v(&["human_review", "either_review"]),
            &v(&["human"])
        ));
    }

    #[test]
    fn agent_approval_never_satisfies_a_human_review() {
        assert!(!reviews_satisfied(&v(&["human_review"]), &v(&["agent"])));
        assert!(reviews_satisfied(&v(&["either_review"]), &v(&["agent"])));
    }

    #[test]
    fn both_needs_one_human_and_one_agent() {
        let both = v(&["agent_review", "human_review"]);
        assert!(!reviews_satisfied(&both, &v(&["human", "human"])));
        assert!(reviews_satisfied(&both, &v(&["human", "agent"])));
        assert!(reviews_satisfied(&[], &[]));
    }
}
