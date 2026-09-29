//! Serialize-before-park for the integrator's own revises (planning
//! p4-design §2 "Revise", step S4; plan-v3 §2.2: ping-pong ⇒ serialize
//! first, park only on a persistent cycle).
//!
//! Only the integrator knows which landing moved the target, so a `conflict`
//! revise may cite that landing's published result as `moved_by_result_id`.
//! A revise that leaves the subject below [`REVISE_LIMIT`] only records the
//! citation. The revise that would reach it is serialized instead when it
//! is a `conflict` citing a landing task that no serialized revise of the
//! subject cited in the window: the subject then depends on that task, and
//! the revise does not count toward the limit, so the subject stays
//! claimable. Any other revise at that point applies and parks the subject,
//! recording why; [`SERIALIZED_REVISE_CAP`] revises of any kind park it too.
//! A subject already parked refuses further revises.
use crate::{
    autonomy::{
        DAY_MS, REVISE_LIMIT, SERIALIZED_REVISE_CAP, agent_revise_count, park_revise_count,
    },
    coordination::{dependency_cycles, save_task_revision},
    error::AppError,
    mutation::Mutation,
};
use serde_json::json;
use sqlx::SqliteConnection;

/// One integrator revise about to apply.
pub(crate) struct ReviseCase<'a> {
    pub project: &'a str,
    pub task: &'a str,
    pub submission: &'a str,
    pub reason_code: &'a str,
    /// The published result the integrator says moved the target.
    pub moved_by: Option<&'a str>,
}

/// A cited result resolved to the task whose landing it published.
pub(crate) struct Landing {
    result: String,
    pub task: String,
}

/// What the gate admitted: the resolved landing, if any, the task the
/// subject is serialized after when the revise reached the limit that way,
/// and why applying the revise parks the subject, when it does.
pub(crate) struct Admission {
    pub landing: Option<Landing>,
    pub serialized_after: Option<String>,
    pub park_reason: Option<&'static str>,
}

/// Admits the revise, serializing it when it would reach the limit and can
/// be, or refuses it when the subject is already parked.
pub(crate) async fn admit(
    c: &mut SqliteConnection,
    case: &ReviseCase<'_>,
    now: i64,
) -> Result<Admission, AppError> {
    let landing = match case.moved_by {
        Some(result) => cited_landing(c, case.project, result).await?,
        None => None,
    };
    let (parking, total) = (
        park_revise_count(c, case.task, now).await?,
        agent_revise_count(c, case.task, now).await?,
    );
    refuse_if_parked(parking, total)?;
    let at_cap = (total + 1 >= SERIALIZED_REVISE_CAP).then_some("serialized_cap");
    let (serialized_after, park_reason) = if parking + 1 < REVISE_LIMIT {
        (None, at_cap)
    } else {
        match serializable(c, case, landing.as_ref(), now).await? {
            Ok(task) => (Some(task), at_cap),
            Err(reason) => (None, Some(reason)),
        }
    };
    Ok(Admission {
        landing,
        serialized_after,
        park_reason,
    })
}

/// Refuses a revise of a subject the limits already parked.
fn refuse_if_parked(parking: i64, total: i64) -> Result<(), AppError> {
    if total >= SERIALIZED_REVISE_CAP {
        return Err(parked("serialized_cap"));
    }
    if parking >= REVISE_LIMIT {
        return Err(parked("subject_parked"));
    }
    Ok(())
}

/// Resolves a cited result to its task; `None` unless it is a result of this
/// project with a `published` observation.
async fn cited_landing(
    c: &mut SqliteConnection,
    project: &str,
    result: &str,
) -> Result<Option<Landing>, AppError> {
    let task: Option<String> = sqlx::query_scalar("SELECT s.task_id FROM integrator_results r JOIN submissions s ON s.id=r.submission_id WHERE r.id=? AND r.project_id=? AND EXISTS(SELECT 1 FROM integrator_observations o WHERE o.result_id=r.id AND o.disposition='published')")
        .bind(result).bind(project).fetch_optional(&mut *c).await?;
    Ok(task.map(|task| Landing {
        result: result.to_owned(),
        task,
    }))
}

/// The landing task a revise reaching the limit serializes the subject
/// after, or the reason the subject parks instead.
async fn serializable(
    c: &mut SqliteConnection,
    case: &ReviseCase<'_>,
    landing: Option<&Landing>,
    now: i64,
) -> Result<Result<String, &'static str>, AppError> {
    if case.reason_code != "conflict" {
        return Ok(Err("not_a_conflict"));
    }
    let Some(landing) = landing else {
        return Ok(Err("landing_unknown"));
    };
    if landing.task == case.task {
        return Ok(Err("landing_is_subject"));
    }
    if cited_before(c, case.task, &landing.task, now).await? {
        return Ok(Err("landing_repeated"));
    }
    if dependency_cycles(c, case.task, &landing.task).await? {
        return Ok(Err("dependency_cycle"));
    }
    Ok(Ok(landing.task.clone()))
}

/// True when an earlier revise of `task` in the last 24 hours is recorded as
/// serialized after `landing`: the same landing again is a persistent cycle.
async fn cited_before(
    c: &mut SqliteConnection,
    task: &str,
    landing: &str,
    now: i64,
) -> Result<bool, AppError> {
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM integrator_revises WHERE task_id=? AND serialized_after=? AND revised_at>?")
        .bind(task).bind(landing).bind(now - DAY_MS).fetch_one(&mut *c).await?;
    Ok(n > 0)
}

/// The `revise_limit_reached` human gate, naming why the subject is parked.
fn parked(reason: &str) -> AppError {
    AppError::forbidden(
        "Agents revised this task too often in 24 hours (three revises not serialized after a new landing, or six in all); a human must look at it.",
    )
    .with_details(json!({"required_actor":"human","gate":"revise_limit_reached","park_reason":reason}))
}

/// Records the applied revise; a serialized revise also records the
/// subject's dependency on the landing task.
pub(crate) async fn record(
    m: &mut Mutation,
    case: &ReviseCase<'_>,
    admission: &Admission,
) -> Result<(), AppError> {
    if let Some(landing) = &admission.serialized_after {
        depend_on(m, case, landing).await?;
    }
    let landing = admission.landing.as_ref();
    sqlx::query("INSERT INTO integrator_revises(submission_id,task_id,reason_code,moved_by_result_id,landing_task_id,serialized_after,park_reason,revised_at) VALUES(?,?,?,?,?,?,?,?)")
        .bind(case.submission).bind(case.task).bind(case.reason_code)
        .bind(landing.map(|l| &l.result)).bind(landing.map(|l| &l.task))
        .bind(&admission.serialized_after).bind(admission.park_reason)
        .bind(m.now).execute(&mut *m.tx).await?;
    Ok(())
}

/// Makes the subject depend on `landing`, as a new task revision when the
/// dependency is new. A landing task that is already done satisfies it at
/// once; the dependency then records the ordering.
async fn depend_on(m: &mut Mutation, case: &ReviseCase<'_>, landing: &str) -> Result<(), AppError> {
    let added = sqlx::query(
        "INSERT OR IGNORE INTO task_dependencies(project_id,task_id,prerequisite_id) VALUES(?,?,?)",
    )
    .bind(case.project)
    .bind(case.task)
    .bind(landing)
    .execute(&mut *m.tx)
    .await?
    .rows_affected();
    if added == 0 {
        return Ok(());
    }
    sqlx::query("UPDATE tasks SET revision=revision+1 WHERE id=?")
        .bind(case.task)
        .execute(&mut *m.tx)
        .await?;
    save_task_revision(m, case.project, case.task)
        .await
        .map(drop)
}
