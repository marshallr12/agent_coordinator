//! Admission control (P6): agent-created tasks beyond a global weekly budget
//! are held.
//!
//! * Every task records its `origin`: `human` or `agent` from the creating
//!   principal, or `service` when the service created it.
//! * A task may carry an `admission_class` (`revert`, `fix_target`, `deflake`
//!   or `refusal_fix`). Fixes are always admitted and do not use the budget.
//! * An agent-created task without a class is admitted only while fewer than
//!   the weekly budget of such tasks were admitted in the current ISO week
//!   (UTC, Monday through Sunday) across all projects. Beyond it the task is
//!   created `planned`, the response says why, and the digest lists it.
//!   Human-created tasks are never held.
use crate::{error::AppError, mutation::Mutation};
use serde_json::{Value, json};
use sqlx::SqliteConnection;

/// Agent-originated tasks admitted per ISO week unless the service is
/// configured otherwise.
pub const DEFAULT_WEEKLY_BUDGET: i64 = 5;
/// Largest weekly budget the service accepts.
pub const MAX_WEEKLY_BUDGET: i64 = 10_000;
/// Always-admitted fix classes.
pub const CLASSES: [&str; 4] = ["revert", "fix_target", "deflake", "refusal_fix"];
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
        Some(class) if !CLASSES.contains(&class) => Err(AppError::bad_request(
            "admission_class must be revert, fix_target, deflake or refusal_fix.",
        )),
        _ => Ok(()),
    }
}

/// The outcome of admitting one task.
pub struct Admission {
    pub origin: Origin,
    pub class: Option<String>,
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
            "admission_class": self.class,
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
                 already admitted this ISO week (budget {}, all projects together). A human \
                 can release it, or it is admitted when the week rolls over.",
                self.admitted_this_week, self.limit
            ));
        }
        value
    }
}

async fn admitted_in_week(c: &mut SqliteConnection, start: i64) -> Result<i64, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT count(*) FROM tasks WHERE budget_admitted_at>=? AND budget_admitted_at<?",
    )
    .bind(start)
    .bind(start + WEEK_MS)
    .fetch_one(c)
    .await?)
}

/// Decides whether a task created by `m`'s actor is held by the budget. Call
/// it inside the mutation, after the replay check, and then [`record`] the
/// result on the inserted task.
pub async fn admit(
    m: &mut Mutation,
    limit: i64,
    requested_planned: bool,
    class: Option<&str>,
) -> Result<Admission, AppError> {
    let origin = Origin::of_principal(&m.actor.kind);
    let start = week_start(m.now);
    let budgeted = origin == Origin::Agent && class.is_none();
    let admitted_this_week = if budgeted {
        admitted_in_week(&mut m.tx, start).await?
    } else {
        0
    };
    let within = admitted_this_week < limit;
    Ok(Admission {
        origin,
        class: class.map(str::to_owned),
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
        "UPDATE tasks SET origin=?,admission_class=?,budget_admitted_at=?,budget_held_at=? WHERE id=?",
    )
    .bind(admission.origin.as_str())
    .bind(&admission.class)
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
        assert!(validate_class(Some("urgent")).is_err());
    }
}
