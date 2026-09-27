//! `next`: the single next action for the caller's role (autonomy plan §2.2
//! item 8).
//!
//! The endpoint is read-only. It evaluates the same preconditions a claim
//! would, so a shadow supervisor holding a read-access credential can log
//! what it would launch, and a live supervisor gets a ready-made call
//! template. Caller-local steps (such as acknowledging the current
//! instructions) are reported separately instead of disqualifying work, and
//! candidates only a human can unblock are counted as the human queue.
use crate::{auth::Actor, auth::Auth, error::AppError, response, state::AppState};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
};
use coordinator_core::INSTRUCTION_VERSION;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection};
use std::collections::BTreeMap;

type Reply = Result<Json<Value>, AppError>;

/// How many candidates of each kind one call inspects.
const CANDIDATE_LIMIT: i64 = 50;
/// Poll interval suggested when nothing is eligible.
const RETRY_AFTER_SECONDS: i64 = 30;
/// Unmet codes the caller can clear itself right before claiming.
const CALLER_LOCAL: &[&str] = &["instructions_required"];

/// Which kind of launch the caller would perform.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Implementer,
    Reviewer,
}

impl Role {
    /// Parses the `role` query value; implementer when omitted.
    fn parse(value: Option<&str>) -> Result<Self, AppError> {
        match value.unwrap_or("implementer") {
            "implementer" => Ok(Self::Implementer),
            "reviewer" => Ok(Self::Reviewer),
            _ => Err(AppError::bad_request(
                "role must be implementer or reviewer.",
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Implementer => "implementer",
            Self::Reviewer => "reviewer",
        }
    }
}

#[derive(Deserialize)]
struct NextQuery {
    role: Option<String>,
}

/// What one scan over the candidates found.
#[derive(Default)]
struct Scan {
    action: Option<Value>,
    caller_steps: Vec<Value>,
    inspected: u64,
    human_gated: u64,
    skipped: BTreeMap<String, u64>,
}

impl Scan {
    /// Records one candidate's unmet preconditions; returns true when the
    /// candidate is eligible apart from caller-local steps.
    fn consider(&mut self, unmet: &[Value]) -> bool {
        self.inspected += 1;
        let (local, blocking): (Vec<&Value>, Vec<&Value>) = unmet
            .iter()
            .partition(|item| CALLER_LOCAL.contains(&item["code"].as_str().unwrap_or("")));
        if blocking.is_empty() {
            self.caller_steps = local.into_iter().cloned().collect();
            return true;
        }
        if blocking
            .iter()
            .any(|item| item["required_actor"] == "human")
        {
            self.human_gated += 1;
        }
        for item in blocking {
            let code = item["code"].as_str().unwrap_or("unknown").to_owned();
            *self.skipped.entry(code).or_default() += 1;
        }
        false
    }

    /// The response body for `role`.
    fn into_value(self, role: Role) -> Value {
        let found = self.action.is_some();
        json!({
            "role": role.as_str(),
            "action": self.action,
            "caller_steps": self.caller_steps,
            "inspected": self.inspected,
            "human_queue": self.human_gated,
            "skipped": self.skipped,
            "retry_after_seconds": if found { 0 } else { RETRY_AFTER_SECONDS },
        })
    }
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/projects/{project}/next", get(next))
}

/// `GET /api/v1/projects/{project}/next?role=implementer|reviewer`.
async fn next(
    State(s): State<AppState>,
    auth: Auth,
    Path(p): Path<String>,
    Query(query): Query<NextQuery>,
) -> Reply {
    let role = Role::parse(query.role.as_deref())?;
    let mut c = s.pool.acquire().await?;
    let policy = crate::coordination::current_policy_revision(&mut c, &p).await?;
    let now = s.now();
    let scan = match role {
        Role::Implementer => scan_tasks(&mut c, &p, &auth.actor, policy, now).await?,
        Role::Reviewer => scan_reviews(&mut c, &p, &auth.actor, now).await?,
    };
    Ok(response(scan.into_value(role)))
}

/// Finds the first task the caller could claim (recovery before new work).
async fn scan_tasks(
    c: &mut SqliteConnection,
    p: &str,
    actor: &Actor,
    policy: i64,
    now: i64,
) -> Result<Scan, AppError> {
    let mut scan = Scan::default();
    for id in crate::coordination::next_task_candidates(c, p, now, CANDIDATE_LIMIT).await? {
        let task = crate::coordination::task_preconditions_snapshot(c, p, &id, actor, now).await?;
        let unmet = task["unmet_preconditions"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if scan.consider(&unmet) {
            scan.action = Some(task_action(p, &task, policy));
            break;
        }
    }
    Ok(scan)
}

/// The claim call for an eligible task.
fn task_action(p: &str, task: &Value, policy: i64) -> Value {
    let mode = task["claim_mode"].as_str().unwrap_or("work");
    let id = &task["id"];
    json!({
        "kind": if mode == "recovery" { "recover_task" } else { "claim_task" },
        "task_id": id,
        "task_kind": task["kind"],
        "title": task["title"],
        "priority": task["priority"],
        "task_revision": task["revision"],
        "call": {
            "method": "POST",
            "path": format!("/api/v1/projects/{p}/claims"),
            "body": {"task_id": id, "expected_task_revision": task["revision"], "mode": mode,
                     "policy_revision": policy, "instruction_version": INSTRUCTION_VERSION},
        },
        "cli": format!("agent-coordinator claim --task {} --revision {} --mode {mode}",
                       id.as_str().unwrap_or(""), task["revision"]),
    })
}

/// Finds the first agent-claimable review, highest subject priority first.
async fn scan_reviews(
    c: &mut SqliteConnection,
    p: &str,
    actor: &Actor,
    now: i64,
) -> Result<Scan, AppError> {
    let mut scan = Scan::default();
    for row in review_candidates(c, p).await? {
        let id: String = row.get("id");
        let checked = crate::workflow::activity_preconditions(c, p, &id, actor, now).await?;
        let unmet = checked["unmet_preconditions"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if scan.consider(&unmet) {
            scan.action = Some(review_action(p, &row));
            break;
        }
    }
    Ok(scan)
}

/// Open agent-claimable review activities in subject priority order.
async fn review_candidates(
    c: &mut SqliteConnection,
    p: &str,
) -> Result<Vec<sqlx::sqlite::SqliteRow>, AppError> {
    Ok(sqlx::query(
        "SELECT wa.id,wa.kind,wa.subject_task_id,wa.submission_id,s.kind AS submission_kind,\
         s.project_policy_revision,s.workflow_policy_revision,t.title,t.priority \
         FROM workflow_activities wa JOIN submissions s ON s.id=wa.submission_id \
         JOIN tasks t ON t.id=wa.subject_task_id \
         WHERE wa.project_id=? AND wa.kind IN ('agent_review','either_review') \
         AND wa.state IN ('queued','active','recovery_required') \
         ORDER BY t.priority,wa.created_at,wa.id LIMIT ?",
    )
    .bind(p)
    .bind(CANDIDATE_LIMIT)
    .fetch_all(&mut *c)
    .await?)
}

/// The claim call for an eligible review activity.
fn review_action(p: &str, row: &sqlx::sqlite::SqliteRow) -> Value {
    let id: String = row.get("id");
    let submission: String = row.get("submission_id");
    let project_policy: i64 = row.get("project_policy_revision");
    let workflow_policy: i64 = row.get("workflow_policy_revision");
    json!({
        "kind": "claim_review",
        "activity_id": id,
        "activity_kind": row.get::<String, _>("kind"),
        "subject_task_id": row.get::<String, _>("subject_task_id"),
        "submission_id": submission,
        "submission_kind": row.get::<String, _>("submission_kind"),
        "title": row.get::<String, _>("title"),
        "priority": row.get::<i64, _>("priority"),
        "call": {
            "method": "POST",
            "path": format!("/api/v1/projects/{p}/workflow-activities/{id}/claim"),
            "body": {"expected_submission_id": submission,
                     "expected_project_policy_revision": project_policy,
                     "expected_workflow_policy_revision": workflow_policy},
        },
        "cli": format!("agent-coordinator reviews claim --activity {id} --submission {submission} \
                        --project-policy-revision {project_policy} --workflow-policy-revision {workflow_policy}"),
    })
}
