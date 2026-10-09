//! Admission control (P6): agent-created tasks beyond a per-project weekly
//! budget are held.
//!
//! * Every task records its `origin`: `human` or `agent` from the creating
//!   principal, or `service` when the service created it.
//! * A task may carry an `admission_class` (`revert`, `fix_target`, `deflake`
//!   or `refusal_fix`). Fixes are always admitted and do not use the budget.
//! * An agent-created task without a class is admitted only while fewer than
//!   the weekly budget of such tasks were admitted in the task's own project in
//!   the current ISO week (UTC, Monday through Sunday); other projects' tasks
//!   do not count against it. Beyond it the task is
//!   created `planned`, the response says why, and the digest lists it.
//!   Human-created tasks are never held.
//! * The end-to-end canary files one task per host and harness per day, which
//!   alone would use the whole weekly budget. A request-only class, `canary`,
//!   exempts such a task: neither counted nor held, recorded in
//!   `budget_exempt`. Only an agent principal the service names with
//!   `--canary-principals` may use it; anyone else is refused, so the class
//!   is no way around the budget.
use crate::{error::AppError, mutation::Mutation, state::Config};
use serde_json::{Value, json};
use sqlx::SqliteConnection;

/// Agent-originated tasks admitted per ISO week unless the service is
/// configured otherwise.
pub const DEFAULT_WEEKLY_BUDGET: i64 = 5;
/// Largest weekly budget the service accepts.
pub const MAX_WEEKLY_BUDGET: i64 = 10_000;
/// Always-admitted fix classes.
pub const CLASSES: [&str; 4] = ["revert", "fix_target", "deflake", "refusal_fix"];
/// The budget-exempt class of the end-to-end canary; only a designated
/// canary principal may request it.
pub const CANARY_CLASS: &str = "canary";
const DAY_MS: i64 = 86_400_000;
const WEEK_MS: i64 = 7 * DAY_MS;

/// Where a task came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Human,
    Agent,
    Service,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Agent => "agent",
            Self::Service => "service",
        }
    }

    /// The origin of a task created by a principal of this kind.
    pub fn of_principal(kind: &str) -> Self {
        if kind == "agent" {
            Self::Agent
        } else {
            Self::Human
        }
    }
}

/// The start (UTC Monday 00:00, in milliseconds) of the ISO week holding `now`.
pub fn week_start(now: i64) -> i64 {
    let days = now.div_euclid(DAY_MS);
    // 1970-01-01 was a Thursday, three days after that week's Monday.
    (days - (days + 3).rem_euclid(7)) * DAY_MS
}

pub fn validate_class(class: Option<&str>) -> Result<(), AppError> {
    match class {
        Some(class) if class != CANARY_CLASS && !CLASSES.contains(&class) => {
            Err(AppError::bad_request(
                "admission_class must be revert, fix_target, deflake, refusal_fix or canary.",
            ))
        }
        _ => Ok(()),
    }
}

/// The outcome of admitting one task.
pub struct Admission {
    pub origin: Origin,
    pub class: Option<String>,
    /// True for a canary task, which the weekly budget neither counts nor holds.
    exempt: bool,
    /// True when the weekly budget held the task.
    pub held: bool,
    /// True when the task used one place of the weekly budget.
    consumed: bool,
    admitted_this_week: i64,
    limit: i64,
    week_start: i64,
}

impl Admission {
    /// The lifecycle the task is created in.
    pub fn lifecycle(&self, requested_planned: bool) -> &'static str {
        if requested_planned || self.held {
            "planned"
        } else {
            "open"
        }
    }

    /// The `admission` object of the creation response.
    pub fn value(&self) -> Value {
        let mut value = json!({
            "origin": self.origin.as_str(),
            "admission_class": self.class.as_deref().or(self.exempt.then_some(CANARY_CLASS)),
            "held": self.held,
            "weekly_budget": {
                "limit": self.limit,
                "admitted_this_week": self.admitted_this_week,
                "week_start": coordinator_core::timestamp(self.week_start),
            },
        });
        if self.held {
            value["reason"] = json!(format!(
                "This agent-created task was held as planned: {} agent-originated tasks were \
                 already admitted in this project this ISO week (budget {} per project). A human \
                 can release it, or it is admitted when the week rolls over.",
                self.admitted_this_week, self.limit
            ));
        }
        value
    }
}

/// The tasks of `project` that used a place of the budget in the ISO week
/// starting at `start`.
pub(crate) async fn admitted_in_week(
    c: &mut SqliteConnection,
    project: &str,
    start: i64,
) -> Result<i64, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT count(*) FROM tasks WHERE project_id=? AND budget_admitted_at>=? AND budget_admitted_at<?",
    )
    .bind(project)
    .bind(start)
    .bind(start + WEEK_MS)
    .fetch_one(c)
    .await?)
}

/// Decides whether a task created by `m`'s actor in `project` is held by that
/// project's budget. Call
/// it inside the mutation, after the replay check, and then [`record`] the
/// result on the inserted task.
pub async fn admit(
    m: &mut Mutation,
    config: &Config,
    project: &str,
    requested_planned: bool,
    class: Option<&str>,
) -> Result<Admission, AppError> {
    let limit = config.agent_task_weekly_budget;
    let origin = Origin::of_principal(&m.actor.kind);
    let exempt = class == Some(CANARY_CLASS);
    if exempt && !(origin == Origin::Agent && config.canary_principals.contains(&m.actor.name)) {
        return Err(AppError::forbidden(
            "Only an agent principal the service designates as a canary may use the canary admission class.",
        ));
    }
    let class = class.filter(|_| !exempt);
    let start = week_start(m.now);
    let budgeted = origin == Origin::Agent && class.is_none() && !exempt;
    let admitted_this_week = if budgeted {
        admitted_in_week(&mut m.tx, project, start).await?
    } else {
        0
    };
    let within = admitted_this_week < limit;
    Ok(Admission {
        origin,
        class: class.map(str::to_owned),
        exempt,
        held: budgeted && !requested_planned && !within,
        consumed: budgeted && !requested_planned && within,
        admitted_this_week: admitted_this_week
            + i64::from(budgeted && !requested_planned && within),
        limit,
        week_start: start,
    })
}

/// Stores the admission outcome on the inserted task.
pub async fn record(m: &mut Mutation, task: &str, admission: &Admission) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE tasks SET origin=?,admission_class=?,budget_exempt=?,budget_admitted_at=?,budget_held_at=? WHERE id=?",
    )
    .bind(admission.origin.as_str())
    .bind(&admission.class)
    .bind(admission.exempt.then_some(CANARY_CLASS))
    .bind(admission.consumed.then_some(m.now))
    .bind(admission.held.then_some(m.now))
    .bind(task)
    .execute(&mut *m.tx)
    .await?;
    Ok(())
}

/// Marks a task the service created itself (a revert, a follow-up, a re-land);
/// a fix carries its class and is never held.
pub async fn record_service(
    m: &mut Mutation,
    task: &str,
    class: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query("UPDATE tasks SET origin='service',admission_class=? WHERE id=?")
        .bind(class)
        .bind(task)
        .execute(&mut *m.tx)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weeks_start_on_monday_utc() {
        // 2026-10-05 is a Monday.
        let monday = 1_791_158_400_000;
        assert_eq!(week_start(monday), monday);
        assert_eq!(week_start(monday + 6 * DAY_MS + 86_399_999), monday);
        assert_eq!(week_start(monday + 7 * DAY_MS), monday + WEEK_MS);
        assert_eq!(week_start(monday - 1), monday - WEEK_MS);
        // 1970-01-01 (a Thursday) belongs to the week of Monday 1969-12-29.
        assert_eq!(week_start(0), -3 * DAY_MS);
    }

    #[test]
    fn classes_are_validated() {
        assert!(validate_class(None).is_ok());
        assert!(validate_class(Some("revert")).is_ok());
        assert!(validate_class(Some("canary")).is_ok());
        assert!(validate_class(Some("urgent")).is_err());
    }
}
