//! Atomic ownership and project-scoped coordination. No external process runs in a transaction.
use crate::{auth::Auth, error::AppError, mutation::Mutation, response, state::AppState};
use axum::{
    Json, Router,
    extract::{Path, Query, State, rejection::JsonRejection},
    http::HeaderMap,
    routing::{get, patch, post},
};
use coordinator_core::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, Row, SqliteConnection};
use uuid::Uuid;

type Reply = Result<Json<Value>, AppError>;
fn payload<T>(v: Result<Json<T>, JsonRejection>) -> Result<T, AppError> {
    v.map(|Json(v)| v).map_err(|_| {
        AppError::bad_request("The JSON body does not match this operation's request schema.")
    })
}
fn bounded(value: &str, name: &str, max: usize, required: bool) -> Result<(), AppError> {
    if value.len() > max || (required && value.trim().is_empty()) || value.contains('\0') {
        return Err(AppError::bad_request(&format!(
            "{name} must {}contain at most {max} bytes and no NUL characters.",
            if required { "be nonempty and " } else { "" }
        )));
    }
    Ok(())
}
fn criteria(values: &[String]) -> Result<(), AppError> {
    if values.is_empty() || values.len() > 100 {
        return Err(AppError::bad_request("Provide 1–100 acceptance criteria."));
    }
    for v in values {
        bounded(v, "acceptance criterion", 2048, true)?;
    }
    Ok(())
}
fn admin_or_operator(actor: &crate::auth::Actor) -> Result<(), AppError> {
    if actor.kind != "human" {
        return Err(AppError::forbidden(
            "A human operator manages project setup and delegation.",
        ));
    }
    Ok(())
}
fn session(actor: &crate::auth::Actor) -> Result<&str, AppError> {
    actor.session_id.as_deref().ok_or_else(|| {
        AppError::forbidden("Connect a harness session before claiming or changing owned work.")
    })
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/projects", get(projects).post(create_project))
        .route("/api/v1/projects/{project}/policy", patch(update_policy))
        .route("/api/v1/projects/{project}/orientation", get(orientation))
        .route(
            "/api/v1/projects/{project}/tasks",
            get(tasks).post(create_task),
        )
        .route(
            "/api/v1/projects/{project}/tasks/{task}",
            get(task_detail).patch(edit_task),
        )
        .route(
            "/api/v1/projects/{project}/tasks/{task}/unblock",
            post(unblock_task),
        )
        .route("/api/v1/projects/{project}/claims", post(claim))
        .route(
            "/api/v1/projects/{project}/attempts/{attempt}",
            get(attempt_detail),
        )
        .route(
            "/api/v1/projects/{project}/attempts/{attempt}/renew",
            post(renew),
        )
        .route(
            "/api/v1/projects/{project}/attempts/{attempt}/checkpoints",
            post(checkpoint),
        )
        .route(
            "/api/v1/projects/{project}/attempts/{attempt}/release",
            post(release),
        )
        .route(
            "/api/v1/projects/{project}/attempts/{attempt}/checkout",
            post(register_checkout),
        )
        .route(
            "/api/v1/projects/{project}/attempts/{attempt}/recovery-resolution",
            post(recovery_resolution),
        )
        .route("/api/v1/projects/{project}/events", get(events))
        .route(
            "/api/v1/sessions/{id}/instruction-acknowledgments",
            post(acknowledge),
        )
}

#[derive(FromRow, Serialize)]
struct Project {
    id: String,
    name: String,
    repository_url: String,
    target_branch: String,
    policy_revision: i64,
    review_mode: String,
    recovery_mode: String,
    lease_seconds: i64,
    rules: String,
    agent_rule_editing: bool,
    automatic_integration: bool,
    #[serde(serialize_with = "serialize_timestamp")]
    created_at: i64,
}
fn serialize_timestamp<S: serde::Serializer>(
    value: &i64,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&timestamp(*value))
}
async fn project(c: &mut SqliteConnection, id: &str) -> Result<Project, AppError> {
    sqlx::query_as("SELECT * FROM projects WHERE id=?")
        .bind(id)
        .fetch_optional(c)
        .await?
        .ok_or_else(AppError::not_found)
}
#[derive(Deserialize)]
struct Page {
    cursor: Option<String>,
    limit: Option<i64>,
}
impl Page {
    fn limit(&self) -> Result<i64, AppError> {
        let n = self.limit.unwrap_or(50);
        if !(1..=200).contains(&n) {
            return Err(AppError::bad_request("limit must be between 1 and 200."));
        }
        if self.cursor.as_ref().is_some_and(|v| v.len() > 128) {
            return Err(AppError::bad_request("Invalid cursor."));
        }
        Ok(n)
    }
}
async fn projects(State(s): State<AppState>, _auth: Auth, Query(page): Query<Page>) -> Reply {
    let limit = page.limit()?;
    let mut items: Vec<Project> =
        sqlx::query_as("SELECT * FROM projects WHERE (? IS NULL OR id>?) ORDER BY id LIMIT ?")
            .bind(&page.cursor)
            .bind(&page.cursor)
            .bind(limit + 1)
            .fetch_all(&s.pool)
            .await?;
    let more = items.len() > limit as usize;
    items.truncate(limit as usize);
    let cursor = if more {
        items.last().map(|v| v.id.clone())
    } else {
        None
    };
    Ok(response(json!({"items":items,"next_cursor":cursor})))
}
async fn create_project(
    State(s): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    body: Result<Json<ProjectInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    bounded(&input.name, "name", 120, true)?;
    bounded(&input.repository_url, "repository_url", 2048, true)?;
    bounded(&input.target_branch, "target_branch", 255, true)?;
    if input.target_branch.starts_with('-')
        || input
            .target_branch
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
    {
        return Err(AppError::bad_request(
            "Use a Git branch name without whitespace or a leading dash.",
        ));
    }
    let mut m = Mutation::begin(&s, &auth, &headers, "POST /api/v1/projects", &input).await?;
    admin_or_operator(&m.actor)?;
    if let Some(v) = m.replay {
        return Ok(response(v));
    }
    if sqlx::query_scalar::<_, i64>("SELECT count(*) FROM projects WHERE name=?")
        .bind(&input.name)
        .fetch_one(&mut *m.tx)
        .await?
        > 0
    {
        return Err(AppError::conflict(
            "project_exists",
            "A project already uses this name.",
        ));
    }
    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO projects(id,name,repository_url,target_branch,created_at) VALUES(?,?,?,?,?)",
    )
    .bind(&id)
    .bind(&input.name)
    .bind(&input.repository_url)
    .bind(&input.target_branch)
    .bind(m.now)
    .execute(&mut *m.tx)
    .await?;
    let value = serde_json::to_value(project(&mut m.tx, &id).await?)?;
    sqlx::query("INSERT INTO policy_revisions(project_id,revision,data_json,actor_id,created_at) VALUES(?,1,?,?,?)")
        .bind(&id).bind(value.to_string()).bind(&m.actor.id).bind(m.now).execute(&mut *m.tx).await?;
    Ok(response(
        m.finish(value, Some(&id), "project.created", &id).await?,
    ))
}
async fn update_policy(
    State(s): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<PolicyInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    if !["none", "agent", "human", "both"].contains(&input.review_mode.as_str())
        || !["agent", "manual"].contains(&input.recovery_mode.as_str())
        || !(30..=3600).contains(&input.lease_seconds)
    {
        return Err(AppError::bad_request(
            "Invalid review/recovery mode or lease_seconds (30–3600).",
        ));
    }
    bounded(&input.rules, "rules", 32768, false)?;
    let mut m = Mutation::begin(
        &s,
        &auth,
        &headers,
        &format!("PATCH /api/v1/projects/{id}/policy"),
        &input,
    )
    .await?;
    let current = project(&mut m.tx, &id).await?;
    if m.actor.kind == "agent"
        && (!current.agent_rule_editing
            || current.agent_rule_editing != input.agent_rule_editing
            || current.automatic_integration != input.automatic_integration)
    {
        return Err(AppError::forbidden(
            "This project has not delegated this rule change. Agents cannot alter permission grants.",
        ));
    }
    if let Some(v) = m.replay {
        return Ok(response(v));
    }
    if current.policy_revision != input.expected_revision {
        return Err(AppError::conflict(
            "revision_conflict",
            "Read the current project policy before editing it.",
        ));
    }
    sqlx::query("UPDATE projects SET policy_revision=policy_revision+1,review_mode=?,recovery_mode=?,lease_seconds=?,rules=?,agent_rule_editing=?,automatic_integration=? WHERE id=?")
        .bind(&input.review_mode).bind(&input.recovery_mode).bind(input.lease_seconds).bind(&input.rules).bind(input.agent_rule_editing).bind(input.automatic_integration).bind(&id).execute(&mut *m.tx).await?;
    let value = serde_json::to_value(project(&mut m.tx, &id).await?)?;
    sqlx::query("INSERT INTO policy_revisions(project_id,revision,data_json,actor_id,created_at) VALUES(?,?,?,?,?)")
        .bind(&id).bind(current.policy_revision+1).bind(value.to_string()).bind(&m.actor.id).bind(m.now).execute(&mut *m.tx).await?;
    Ok(response(
        m.finish(value, Some(&id), "policy.updated", &id).await?,
    ))
}

#[derive(FromRow)]
struct Task {
    id: String,
    project_id: String,
    title: String,
    description: String,
    acceptance_json: String,
    kind: String,
    priority: i64,
    lifecycle: String,
    revision: i64,
    generation: i64,
    current_attempt_id: Option<String>,
    blocked_reason: Option<String>,
    created_at: i64,
    ready_since: i64,
    attempt_state: Option<String>,
    attempt_expires: Option<i64>,
    owner_authorized: bool,
    dependencies_ready: bool,
}
macro_rules! task_sql {($suffix:literal)=>{concat!(
    "WITH visible AS (SELECT t.*, a.state AS attempt_state,a.expires_at AS attempt_expires, ",
    "COALESCE(p.disabled_at IS NULL AND p.id IS NOT NULL AND CASE WHEN a.credential_id IS NULL THEN b.id IS NOT NULL AND b.revoked_at IS NULL AND b.expires_at>? ELSE c.id IS NOT NULL AND c.revoked_at IS NULL AND (c.expires_at IS NULL OR c.expires_at>?) AND ag.id IS NOT NULL AND ag.closed_at IS NULL END,0) AS owner_authorized, ",
    "NOT EXISTS(SELECT 1 FROM task_dependencies d JOIN tasks prerequisite ON prerequisite.id=d.prerequisite_id WHERE d.task_id=t.id AND prerequisite.lifecycle!='done') AS dependencies_ready ",
    "FROM tasks t LEFT JOIN attempts a ON a.id=t.current_attempt_id LEFT JOIN principals p ON p.id=a.owner_id LEFT JOIN credentials c ON c.id=a.credential_id LEFT JOIN agent_sessions ag ON ag.id=a.session_id AND ag.credential_id=a.credential_id LEFT JOIN browser_sessions b ON b.id=a.session_id WHERE t.project_id=?) ",$suffix
)}}
impl Task {
    fn status(&self, now: i64) -> &str {
        if self.lifecycle != "open" {
            return &self.lifecycle;
        }
        if self.current_attempt_id.is_some() {
            return if self.attempt_state.as_deref() == Some("active")
                && self.attempt_expires.is_some_and(|v| v > now)
                && self.owner_authorized
            {
                "in_progress"
            } else {
                "recovery_required"
            };
        }
        if self.blocked_reason.is_some() || !self.dependencies_ready {
            "blocked"
        } else {
            "ready"
        }
    }
    fn value(&self, now: i64) -> Value {
        json!({"id":self.id,"project_id":self.project_id,"title":self.title,"description":self.description,
        "acceptance_criteria":serde_json::from_str::<Value>(&self.acceptance_json).unwrap_or(Value::Null),"kind":self.kind,"priority":self.priority,
        "lifecycle":self.lifecycle,"revision":self.revision,"generation":self.generation,"current_attempt_id":self.current_attempt_id,
        "work_status":self.status(now),"blocked_reason":self.blocked_reason,"dependencies_ready":self.dependencies_ready,
        "created_at":timestamp(self.created_at),"ready_since":timestamp(self.ready_since)})
    }
}
async fn task(c: &mut SqliteConnection, p: &str, id: &str, now: i64) -> Result<Task, AppError> {
    sqlx::query_as(task_sql!("SELECT * FROM visible WHERE id=?"))
        .bind(now)
        .bind(now)
        .bind(p)
        .bind(id)
        .fetch_optional(c)
        .await?
        .ok_or_else(AppError::not_found)
}
async fn task_list(
    c: &mut SqliteConnection,
    p: &str,
    page: &Page,
    now: i64,
) -> Result<Value, AppError> {
    let limit = page.limit()?;
    let mut items: Vec<Task> = sqlx::query_as(task_sql!(
        "SELECT * FROM visible WHERE (? IS NULL OR id>?) ORDER BY id LIMIT ?"
    ))
    .bind(now)
    .bind(now)
    .bind(p)
    .bind(&page.cursor)
    .bind(&page.cursor)
    .bind(limit + 1)
    .fetch_all(c)
    .await?;
    let more = items.len() > limit as usize;
    items.truncate(limit as usize);
    let cursor = if more {
        items.last().map(|v| v.id.clone())
    } else {
        None
    };
    Ok(json!({"items":items.iter().map(|t|t.value(now)).collect::<Vec<_>>(),"next_cursor":cursor}))
}
async fn tasks(
    State(s): State<AppState>,
    _auth: Auth,
    Path(p): Path<String>,
    Query(page): Query<Page>,
) -> Reply {
    let mut c = s.pool.acquire().await?;
    project(&mut c, &p).await?;
    Ok(response(task_list(&mut c, &p, &page, s.now()).await?))
}
async fn set_dependencies(
    c: &mut SqliteConnection,
    p: &str,
    id: &str,
    deps: &[String],
) -> Result<(), AppError> {
    if deps.len() > 100 {
        return Err(AppError::bad_request(
            "A task may have at most 100 direct prerequisites.",
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for d in deps {
        if d == id || !seen.insert(d) {
            return Err(AppError::bad_request(
                "Dependencies must be distinct and cannot reference this task.",
            ));
        }
        let exists: i64 =
            sqlx::query_scalar("SELECT count(*) FROM tasks WHERE project_id=? AND id=?")
                .bind(p)
                .bind(d)
                .fetch_one(&mut *c)
                .await?;
        if exists == 0 {
            return Err(AppError::bad_request(
                "Every dependency must identify a task in this project.",
            ));
        }
        let cycle:i64=sqlx::query_scalar("WITH RECURSIVE ancestors(id) AS (SELECT ? UNION SELECT d.prerequisite_id FROM task_dependencies d JOIN ancestors a ON a.id=d.task_id) SELECT count(*) FROM ancestors WHERE id=?")
            .bind(d).bind(id).fetch_one(&mut *c).await?;
        if cycle > 0 {
            return Err(AppError::conflict(
                "dependency_cycle",
                "This dependency would create a cycle.",
            ));
        }
    }
    sqlx::query("DELETE FROM task_dependencies WHERE task_id=?")
        .bind(id)
        .execute(&mut *c)
        .await?;
    for d in deps {
        sqlx::query(
            "INSERT INTO task_dependencies(project_id,task_id,prerequisite_id) VALUES(?,?,?)",
        )
        .bind(p)
        .bind(id)
        .bind(d)
        .execute(&mut *c)
        .await?;
    }
    Ok(())
}
async fn save_revision(m: &mut Mutation, p: &str, id: &str) -> Result<Value, AppError> {
    let t = task(&mut m.tx, p, id, m.now).await?;
    let mut value = t.value(m.now);
    value["depends_on"] =
        json!(sqlx::query_scalar::<_, String>(
        "SELECT prerequisite_id FROM task_dependencies WHERE task_id=? ORDER BY prerequisite_id"
    ).bind(id).fetch_all(&mut *m.tx).await?);
    sqlx::query("INSERT INTO task_revisions(project_id,task_id,revision,data_json,actor_id,created_at) VALUES(?,?,?,?,?,?)")
        .bind(p).bind(id).bind(t.revision).bind(value.to_string()).bind(&m.actor.id).bind(m.now).execute(&mut *m.tx).await?;
    Ok(value)
}
async fn create_task(
    State(s): State<AppState>,
    auth: Auth,
    Path(p): Path<String>,
    headers: HeaderMap,
    body: Result<Json<TaskInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    bounded(&input.title, "title", 300, true)?;
    bounded(&input.description, "description", 32768, false)?;
    criteria(&input.acceptance_criteria)?;
    if !["code", "general"].contains(&input.kind.as_str()) || !(0..=3).contains(&input.priority) {
        return Err(AppError::bad_request(
            "kind must be code/general and priority 0 (urgent) through 3 (low).",
        ));
    }
    let mut m = Mutation::begin(
        &s,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{p}/tasks"),
        &input,
    )
    .await?;
    project(&mut m.tx, &p).await?;
    if let Some(v) = m.replay {
        return Ok(response(v));
    }
    let id = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO tasks(id,project_id,title,description,acceptance_json,kind,priority,lifecycle,created_at,ready_since) VALUES(?,?,?,?,?,?,?,?,?,?)")
        .bind(&id).bind(&p).bind(&input.title).bind(&input.description).bind(serde_json::to_string(&input.acceptance_criteria)?).bind(&input.kind).bind(input.priority)
        .bind(if input.planned{"planned"}else{"open"}).bind(m.now).bind(m.now).execute(&mut *m.tx).await?;
    set_dependencies(&mut m.tx, &p, &id, &input.depends_on).await?;
    let value = save_revision(&mut m, &p, &id).await?;
    Ok(response(
        m.finish(value, Some(&p), "task.created", &id).await?,
    ))
}
async fn edit_task(
    State(s): State<AppState>,
    auth: Auth,
    Path((p, id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<TaskEdit>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    bounded(&input.title, "title", 300, true)?;
    bounded(&input.description, "description", 32768, false)?;
    criteria(&input.acceptance_criteria)?;
    if !(0..=3).contains(&input.priority) {
        return Err(AppError::bad_request("priority must be between 0 and 3."));
    }
    let mut m = Mutation::begin(
        &s,
        &auth,
        &headers,
        &format!("PATCH /api/v1/projects/{p}/tasks/{id}"),
        &input,
    )
    .await?;
    if let Some(v) = m.replay {
        return Ok(response(v));
    }
    let current = task(&mut m.tx, &p, &id, m.now).await?;
    if current.revision != input.expected_revision {
        return Err(AppError::conflict(
            "revision_conflict",
            "Read the latest task revision before editing.",
        ));
    }
    if current.current_attempt_id.is_some()
        || !["open", "planned"].contains(&current.lifecycle.as_str())
    {
        return Err(AppError::conflict(
            "task_not_editable",
            "Only unowned open or planned tasks can be edited. Preserve active work and reconcile it first.",
        ));
    }
    set_dependencies(&mut m.tx, &p, &id, &input.depends_on).await?;
    sqlx::query("UPDATE tasks SET title=?,description=?,acceptance_json=?,priority=?,lifecycle=?,revision=revision+1 WHERE id=?")
        .bind(&input.title).bind(&input.description).bind(serde_json::to_string(&input.acceptance_criteria)?).bind(input.priority).bind(if input.planned{"planned"}else{"open"}).bind(&id).execute(&mut *m.tx).await?;
    let value = save_revision(&mut m, &p, &id).await?;
    Ok(response(
        m.finish(value, Some(&p), "task.edited", &id).await?,
    ))
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Unblock {
    expected_revision: i64,
    reason: String,
}
async fn unblock_task(
    State(s): State<AppState>,
    auth: Auth,
    Path((p, id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<Unblock>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    bounded(&input.reason, "reason", 4096, true)?;
    let mut m = Mutation::begin(
        &s,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{p}/tasks/{id}/unblock"),
        &input,
    )
    .await?;
    admin_or_operator(&m.actor)?;
    if let Some(v) = m.replay {
        return Ok(response(v));
    }
    let t = task(&mut m.tx, &p, &id, m.now).await?;
    if t.revision != input.expected_revision {
        return Err(AppError::conflict(
            "revision_conflict",
            "Refresh this task first.",
        ));
    }
    if t.current_attempt_id.is_some() || t.lifecycle != "open" {
        return Err(AppError::conflict(
            "task_not_editable",
            "An active or expired attempt requires its recovery workflow.",
        ));
    }
    sqlx::query(
        "UPDATE tasks SET blocked_reason=NULL,revision=revision+1,ready_since=? WHERE id=?",
    )
    .bind(m.now)
    .bind(&id)
    .execute(&mut *m.tx)
    .await?;
    let value = save_revision(&mut m, &p, &id).await?;
    Ok(response(
        m.finish(
            json!({"task":value,"resolution":input.reason}),
            Some(&p),
            "task.unblocked",
            &id,
        )
        .await?,
    ))
}

#[derive(FromRow)]
struct Attempt {
    id: String,
    project_id: String,
    task_id: String,
    owner_id: String,
    session_id: String,
    credential_id: Option<String>,
    generation: i64,
    state: String,
    mode: String,
    expires_at: i64,
    last_heartbeat_at: i64,
    last_progress_at: i64,
    created_at: i64,
    ended_at: Option<i64>,
    outcome: Option<String>,
}
impl Attempt {
    fn value(&self) -> Value {
        json!({"id":self.id,"project_id":self.project_id,"task_id":self.task_id,"owner_id":self.owner_id,"session_id":self.session_id,
    "generation":self.generation,"state":self.state,"mode":self.mode,"expires_at":timestamp(self.expires_at),"last_heartbeat_at":timestamp(self.last_heartbeat_at),
    "last_progress_at":timestamp(self.last_progress_at),"created_at":timestamp(self.created_at),"ended_at":self.ended_at.map(timestamp),"outcome":self.outcome})
    }
}
async fn attempt(c: &mut SqliteConnection, p: &str, id: &str) -> Result<Attempt, AppError> {
    sqlx::query_as("SELECT * FROM attempts WHERE project_id=? AND id=?")
        .bind(p)
        .bind(id)
        .fetch_optional(c)
        .await?
        .ok_or_else(AppError::not_found)
}
async fn owned(m: &mut Mutation, p: &str, id: &str, generation: i64) -> Result<Attempt, AppError> {
    let a = attempt(&mut m.tx, p, id).await?;
    if a.owner_id != m.actor.id
        || Some(a.session_id.as_str()) != m.actor.session_id.as_deref()
        || a.credential_id != m.actor.credential_id
    {
        return Err(AppError::forbidden(
            "This attempt belongs to another session. Use a recovery claim after ownership expires.",
        ));
    }
    if a.generation != generation || a.state != "active" || a.expires_at <= m.now {
        return Err(AppError::conflict(
            "lease_expired",
            "This ownership grant is no longer valid. Stop changing its work and inspect recovery instructions.",
        ));
    }
    let t = task(&mut m.tx, p, &a.task_id, m.now).await?;
    if t.current_attempt_id.as_deref() != Some(id)
        || t.generation != generation
        || !t.owner_authorized
    {
        return Err(AppError::conflict(
            "lease_expired",
            "This attempt is no longer the current owner.",
        ));
    }
    Ok(a)
}
async fn task_detail(
    State(s): State<AppState>,
    _auth: Auth,
    Path((p, id)): Path<(String, String)>,
) -> Reply {
    let mut c = s.pool.begin().await?;
    let t = task(&mut c, &p, &id, s.now()).await?;
    let mut value = t.value(s.now());
    let attempts: Vec<Attempt> = sqlx::query_as(
        "SELECT * FROM attempts WHERE project_id=? AND task_id=? ORDER BY generation DESC LIMIT 50",
    )
    .bind(&p)
    .bind(&id)
    .fetch_all(&mut *c)
    .await?;
    let rows=sqlx::query("SELECT cp.* FROM checkpoints cp JOIN attempts a ON a.id=cp.attempt_id WHERE a.project_id=? AND a.task_id=? ORDER BY cp.created_at DESC,cp.id DESC LIMIT 100").bind(&p).bind(&id).fetch_all(&mut *c).await?;
    value["attempts"] = json!(attempts.iter().map(Attempt::value).collect::<Vec<_>>());
    value["checkpoints"] = json!(rows.iter().map(checkpoint_value).collect::<Vec<_>>());
    value["depends_on"] = json!(
        sqlx::query_scalar::<_, String>(
            "SELECT prerequisite_id FROM task_dependencies WHERE task_id=? ORDER BY prerequisite_id"
        )
        .bind(&id)
        .fetch_all(&mut *c)
        .await?
    );
    let checkouts = sqlx::query("SELECT ch.* FROM checkouts ch JOIN attempts a ON a.id=ch.attempt_id WHERE a.project_id=? AND a.task_id=? ORDER BY a.generation DESC LIMIT 50")
        .bind(&p).bind(&id).fetch_all(&mut *c).await?;
    value["checkouts"] = json!(checkouts.iter().map(checkout_value).collect::<Vec<_>>());
    value["history_limits"] = json!({"attempts":50,"checkpoints":100,"checkouts":50});
    Ok(response(value))
}
fn checkout_value(r: &sqlx::sqlite::SqliteRow) -> Value {
    json!({"attempt_id":r.get::<String,_>("attempt_id"),"workstation_id":r.get::<String,_>("workstation_id"),"identity":r.get::<String,_>("identity"),"path":r.get::<String,_>("path"),"branch":r.get::<String,_>("branch"),"base_revision":r.get::<String,_>("base_revision"),"created_at":timestamp(r.get("created_at"))})
}
fn checkpoint_value(r: &sqlx::sqlite::SqliteRow) -> Value {
    json!({"id":r.get::<String,_>("id"),"attempt_id":r.get::<String,_>("attempt_id"),"summary":r.get::<String,_>("summary"),"current_action":r.get::<String,_>("current_action"),"next_step":r.get::<String,_>("next_step"),"blockers":serde_json::from_str::<Value>(&r.get::<String,_>("blockers_json")).unwrap_or(Value::Null),"created_at":timestamp(r.get("created_at"))})
}
async fn attempt_detail(
    State(s): State<AppState>,
    _auth: Auth,
    Path((p, id)): Path<(String, String)>,
) -> Reply {
    let mut c = s.pool.begin().await?;
    let now = s.now();
    let a = attempt(&mut c, &p, &id).await?;
    let t = task(&mut c, &p, &a.task_id, now).await?;
    let checkout = sqlx::query("SELECT * FROM checkouts WHERE project_id=? AND attempt_id=?")
        .bind(&p)
        .bind(&id)
        .fetch_optional(&mut *c)
        .await?;
    let valid = a.state == "active"
        && a.expires_at > now
        && t.owner_authorized
        && t.current_attempt_id.as_deref() == Some(&id);
    Ok(response(
        json!({"attempt":a.value(),"task":t.value(now),"authority_valid":valid,"lease_remaining_ms":if valid{a.expires_at-now}else{0},"checkout":checkout.as_ref().map(checkout_value)}),
    ))
}

const INSTRUCTIONS: &str = "Connect or resume your own harness session; never reuse another harness's session proof. Read this project's current rules and acknowledge coordination-v1 before claiming. A task listing reserves nothing. Claim a ready task atomically, or inspect an expired task with a recovery claim. Before editing code, register a separate clean worktree and check the task is still undone. Record checkpoints and renew at the returned renew_after_seconds cadence, before the server's deadline. Checkpoints do not renew ownership. Use the same persisted Idempotency-Key when retrying a lost response. On lease loss stop ownership-dependent edits. Recovery must inspect saved work and still-running jobs before resuming. Never restart an unknown job merely because its observer is missing. Release with a handoff if paused; release is not completion. This foundation implements project/task admission, leases, checkpoints, checkout registration and recovery. Submission, review, integration, job reporters and shared lessons are not yet implemented: do not claim work complete through a generic status edit.";
async fn orientation(State(s): State<AppState>, auth: Auth, Path(p): Path<String>) -> Reply {
    let mut c = s.pool.acquire().await?;
    let proj = project(&mut c, &p).await?;
    let now = s.now();
    let candidates:Vec<Task>=sqlx::query_as(task_sql!("SELECT * FROM visible WHERE lifecycle='open' AND blocked_reason IS NULL AND dependencies_ready AND current_attempt_id IS NULL ORDER BY priority,ready_since,id LIMIT 20"))
        .bind(now).bind(now).bind(&p).fetch_all(&mut *c).await?;
    let active:Vec<Attempt>=sqlx::query_as("SELECT * FROM attempts WHERE project_id=? AND owner_id=? AND session_id=? AND state='active' ORDER BY created_at LIMIT 50")
        .bind(&p).bind(&auth.actor.id).bind(&auth.actor.session_id).fetch_all(&mut *c).await?;
    let recovery:Vec<Task>=sqlx::query_as(task_sql!("SELECT * FROM visible WHERE lifecycle='open' AND current_attempt_id IS NOT NULL AND (attempt_state!='active' OR attempt_expires<=? OR NOT owner_authorized) ORDER BY priority,ready_since,id LIMIT 20"))
        .bind(now).bind(now).bind(&p).bind(now).fetch_all(&mut *c).await?;
    Ok(response(
        json!({"project":proj,"policy_revision":proj.policy_revision,"instruction_version":INSTRUCTION_VERSION,"required_sections":[REQUIRED_SECTION],"instructions":INSTRUCTIONS,"instructions_complete":true,
        "candidates":candidates.iter().map(|t|t.value(now)).collect::<Vec<_>>(),"active_attempts":active.iter().map(Attempt::value).collect::<Vec<_>>(),"recovery_candidates":recovery.iter().map(|t|t.value(now)).collect::<Vec<_>>(),"implemented_stage":"foundation"}),
    ))
}
async fn acknowledge(
    State(s): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<Acknowledgment>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    let mut m = Mutation::begin(
        &s,
        &auth,
        &headers,
        &format!("POST /api/v1/sessions/{id}/instruction-acknowledgments"),
        &input,
    )
    .await?;
    if m.actor.kind != "agent" || m.actor.session_id.as_deref() != Some(&id) {
        return Err(AppError::forbidden(
            "Acknowledge instructions using the connected agent session and its proof.",
        ));
    }
    if let Some(v) = m.replay {
        return Ok(response(v));
    }
    let proj = project(&mut m.tx, &input.project_id).await?;
    if proj.policy_revision != input.policy_revision
        || input.instruction_version != INSTRUCTION_VERSION
        || input.sections != [REQUIRED_SECTION]
    {
        return Err(AppError::conflict(
            "policy_changed",
            "Read the complete current orientation before acknowledging instructions.",
        ));
    }
    sqlx::query("INSERT INTO instruction_acknowledgments(session_id,project_id,policy_revision,instruction_version,created_at) VALUES(?,?,?,?,?) ON CONFLICT(session_id,project_id) DO UPDATE SET policy_revision=excluded.policy_revision,instruction_version=excluded.instruction_version,created_at=excluded.created_at")
        .bind(&id).bind(&input.project_id).bind(input.policy_revision).bind(&input.instruction_version).bind(m.now).execute(&mut *m.tx).await?;
    Ok(response(
        m.finish(
            json!({"acknowledged":true,"policy_revision":input.policy_revision}),
            Some(&input.project_id),
            "instructions.acknowledged",
            &id,
        )
        .await?,
    ))
}
async fn claim(
    State(s): State<AppState>,
    auth: Auth,
    Path(p): Path<String>,
    headers: HeaderMap,
    body: Result<Json<ClaimInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    if !["work", "recovery"].contains(&input.mode.as_str()) {
        return Err(AppError::bad_request("mode must be work or recovery."));
    }
    if input.task_id.is_some() != input.expected_task_revision.is_some() {
        return Err(AppError::bad_request(
            "An explicit task claim requires its expected_task_revision; next-eligible claims omit both.",
        ));
    }
    let mut m = Mutation::begin(
        &s,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{p}/claims"),
        &input,
    )
    .await?;
    let owner_session = session(&m.actor)?.to_owned();
    let proj = project(&mut m.tx, &p).await?;
    if let Some(mut v) = m.replay {
        if let Some(id) = v
            .pointer("/claim/attempt/id")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            let a = attempt(&mut m.tx, &p, &id).await?;
            let t = task(&mut m.tx, &p, &a.task_id, m.now).await?;
            v["current_authority"] = json!({"valid":a.state=="active"&&a.expires_at>m.now&&t.owner_authorized&&t.current_attempt_id.as_deref()==Some(&id),"attempt":a.value(),"task_status":t.status(m.now),"lease_remaining_ms":(a.expires_at-m.now).max(0)});
        }
        v["replayed"] = json!(true);
        return Ok(response(v));
    }
    if proj.policy_revision != input.policy_revision
        || input.instruction_version != INSTRUCTION_VERSION
    {
        return Err(AppError::conflict(
            "policy_changed",
            "Read and acknowledge the current orientation before claiming.",
        ));
    }
    if m.actor.kind == "agent" {
        let acknowledged:i64=sqlx::query_scalar("SELECT count(*) FROM instruction_acknowledgments WHERE session_id=? AND project_id=? AND policy_revision=? AND instruction_version=?")
            .bind(&owner_session).bind(&p).bind(proj.policy_revision).bind(INSTRUCTION_VERSION).fetch_one(&mut *m.tx).await?;
        if acknowledged == 0 {
            return Err(AppError::conflict(
                "instructions_required",
                "Read orientation and acknowledge its required sections before claiming.",
            ));
        }
    }
    if input.mode == "recovery" && proj.recovery_mode == "manual" && m.actor.kind != "human" {
        return Err(AppError::forbidden(
            "This project requires a human operator to inspect and recover expired work.",
        ));
    }
    let chosen = if let Some(id) = &input.task_id {
        Some(task(&mut m.tx, &p, id, m.now).await?)
    } else {
        sqlx::query_as::<_,Task>(task_sql!("SELECT * FROM visible WHERE lifecycle='open' AND blocked_reason IS NULL AND dependencies_ready AND ((?='work' AND current_attempt_id IS NULL) OR (?='recovery' AND current_attempt_id IS NOT NULL AND (attempt_state!='active' OR attempt_expires<=? OR NOT owner_authorized))) ORDER BY priority,ready_since,id LIMIT 1"))
            .bind(m.now).bind(m.now).bind(&p).bind(&input.mode).bind(&input.mode).bind(m.now).fetch_optional(&mut *m.tx).await?
    };
    let Some(t) = chosen else {
        return Ok(response(m.finish(json!({"claim":null,"reasons":["No eligible task in this project and mode. Inspect task blockers, active owners, or recovery candidates."],"retry_after_seconds":30}),Some(&p),"claim.empty",&p).await?));
    };
    if input
        .expected_task_revision
        .is_some_and(|v| v != t.revision)
    {
        return Err(AppError::conflict(
            "revision_conflict",
            "The task changed after selection. Read its current revision.",
        ));
    }
    let required = if input.mode == "work" {
        "ready"
    } else {
        "recovery_required"
    };
    if t.status(m.now) != required {
        return Err(AppError::conflict(
            "claim_conflict",
            "The task is not eligible for this claim mode.",
        )
        .with_details(json!({"task":t.value(m.now)})));
    }
    if let Some(previous) = &t.current_attempt_id {
        sqlx::query("UPDATE attempts SET state='expired',ended_at=?,outcome='Ownership expired or was revoked; recovery began.' WHERE id=? AND state='active'").bind(m.now).bind(previous).execute(&mut *m.tx).await?;
    }
    let id = Uuid::new_v4().to_string();
    let generation = t.generation + 1;
    let expires = m.now + proj.lease_seconds * 1000;
    sqlx::query("INSERT INTO attempts(id,project_id,task_id,owner_id,session_id,credential_id,generation,state,mode,expires_at,last_heartbeat_at,last_progress_at,created_at) VALUES(?,?,?,?,?,?,?,'active',?,?,?,?,?)")
        .bind(&id).bind(&p).bind(&t.id).bind(&m.actor.id).bind(&owner_session).bind(&m.actor.credential_id).bind(generation).bind(&input.mode).bind(expires).bind(m.now).bind(m.now).bind(m.now).execute(&mut *m.tx).await?;
    sqlx::query("UPDATE tasks SET current_attempt_id=?,generation=? WHERE id=?")
        .bind(&id)
        .bind(generation)
        .bind(&t.id)
        .execute(&mut *m.tx)
        .await?;
    let a = attempt(&mut m.tx, &p, &id).await?;
    let updated = task(&mut m.tx, &p, &t.id, m.now).await?;
    let value = json!({"claim":{"task":updated.value(m.now),"attempt":a.value(),"lease_remaining_ms":proj.lease_seconds*1000},"renew_after_seconds":(proj.lease_seconds / 3).min(60),"next_actions":if input.mode=="recovery"{vec!["Inspect saved work and running jobs; record a recovery resolution before editing."]}else{vec!["Prepare/register a separate worktree before code changes; checkpoint and renew ownership."]}});
    Ok(response(
        m.finish(value, Some(&p), "attempt.claimed", &id).await?,
    ))
}
async fn renew(
    State(s): State<AppState>,
    auth: Auth,
    Path((p, id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<RenewInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    let mut m = Mutation::begin(
        &s,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{p}/attempts/{id}/renew"),
        &input,
    )
    .await?;
    // A renewal receipt never grants current authority; verify even before replay.
    let current = owned(&mut m, &p, &id, input.generation).await?;
    if let Some(mut v) = m.replay {
        // Replaying the recorded renewal must not restart its countdown.
        v["attempt"] = current.value();
        v["lease_remaining_ms"] = json!((current.expires_at - m.now).max(0));
        v["replayed"] = json!(true);
        return Ok(response(v));
    }
    let proj = project(&mut m.tx, &p).await?;
    sqlx::query("UPDATE attempts SET expires_at=?,last_heartbeat_at=? WHERE id=?")
        .bind(m.now + proj.lease_seconds * 1000)
        .bind(m.now)
        .bind(&id)
        .execute(&mut *m.tx)
        .await?;
    let a = attempt(&mut m.tx, &p, &id).await?;
    Ok(response(
        m.finish(
            json!({"attempt":a.value(),"lease_remaining_ms":proj.lease_seconds*1000}),
            Some(&p),
            "attempt.renewed",
            &id,
        )
        .await?,
    ))
}
async fn add_checkpoint(
    m: &mut Mutation,
    p: &str,
    id: &str,
    input: &CheckpointInput,
) -> Result<Value, AppError> {
    let checkpoint_id = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO checkpoints(id,project_id,attempt_id,summary,current_action,next_step,blockers_json,created_at) VALUES(?,?,?,?,?,?,?,?)")
        .bind(&checkpoint_id).bind(p).bind(id).bind(&input.summary).bind(&input.current_action).bind(&input.next_step).bind(serde_json::to_string(&input.blockers)?).bind(m.now).execute(&mut *m.tx).await?;
    sqlx::query("UPDATE attempts SET last_progress_at=? WHERE id=?")
        .bind(m.now)
        .bind(id)
        .execute(&mut *m.tx)
        .await?;
    Ok(
        json!({"id":checkpoint_id,"attempt_id":id,"summary":input.summary,"current_action":input.current_action,"next_step":input.next_step,"blockers":input.blockers,"created_at":timestamp(m.now)}),
    )
}
async fn checkpoint(
    State(s): State<AppState>,
    auth: Auth,
    Path((p, id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<CheckpointInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    bounded(&input.summary, "summary", 8192, true)?;
    bounded(&input.current_action, "current_action", 4096, false)?;
    bounded(&input.next_step, "next_step", 4096, false)?;
    if input.blockers.len() > 50 {
        return Err(AppError::bad_request("Use at most 50 blockers."));
    }
    for b in &input.blockers {
        bounded(b, "blocker", 2048, true)?;
    }
    let mut m = Mutation::begin(
        &s,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{p}/attempts/{id}/checkpoints"),
        &input,
    )
    .await?;
    owned(&mut m, &p, &id, input.generation).await?;
    if let Some(v) = m.replay {
        return Ok(response(v));
    }
    let value = add_checkpoint(&mut m, &p, &id, &input).await?;
    Ok(response(
        m.finish(value, Some(&p), "attempt.checkpointed", &id)
            .await?,
    ))
}
async fn release(
    State(s): State<AppState>,
    auth: Auth,
    Path((p, id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<ReleaseInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    bounded(&input.summary, "summary", 8192, true)?;
    let mut m = Mutation::begin(
        &s,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{p}/attempts/{id}/release"),
        &input,
    )
    .await?;
    if let Some(v) = m.replay {
        return Ok(response(v));
    }
    let a = owned(&mut m, &p, &id, input.generation).await?;
    if a.mode == "recovery" && !input.blocked {
        return Err(AppError::conflict(
            "recovery_unresolved",
            "Resolve the recovery inspection before requeuing this task, or release it as blocked.",
        ));
    }
    add_checkpoint(
        &mut m,
        &p,
        &id,
        &CheckpointInput {
            generation: input.generation,
            summary: input.summary.clone(),
            current_action: String::new(),
            next_step: String::new(),
            blockers: if input.blocked {
                vec![input.summary.clone()]
            } else {
                vec![]
            },
        },
    )
    .await?;
    sqlx::query("UPDATE attempts SET state=?,ended_at=?,outcome=? WHERE id=?")
        .bind(if input.blocked { "blocked" } else { "released" })
        .bind(m.now)
        .bind(&input.summary)
        .bind(&id)
        .execute(&mut *m.tx)
        .await?;
    sqlx::query(
        "UPDATE tasks SET current_attempt_id=NULL,blocked_reason=?,ready_since=? WHERE id=?",
    )
    .bind(if input.blocked {
        Some(&input.summary)
    } else {
        None
    })
    .bind(m.now)
    .bind(&a.task_id)
    .execute(&mut *m.tx)
    .await?;
    let t = task(&mut m.tx, &p, &a.task_id, m.now).await?;
    let value = json!({"task":t.value(m.now),"released":true});
    Ok(response(
        m.finish(value, Some(&p), "attempt.released", &id).await?,
    ))
}
async fn recovery_resolution(
    State(s): State<AppState>,
    auth: Auth,
    Path((p, id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<RecoveryInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    bounded(&input.summary, "summary", 8192, true)?;
    if !["resume", "restart"].contains(&input.disposition.as_str())
        || !input.saved_work_checked
        || !input.running_jobs_checked
    {
        return Err(AppError::bad_request(
            "Recovery must inspect saved work and running jobs before choosing resume or restart. Release as blocked if inspection is incomplete.",
        ));
    }
    let mut m = Mutation::begin(
        &s,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{p}/attempts/{id}/recovery-resolution"),
        &input,
    )
    .await?;
    let a = owned(&mut m, &p, &id, input.generation).await?;
    if let Some(v) = m.replay {
        return Ok(response(v));
    }
    if a.mode != "recovery" {
        return Err(AppError::conflict(
            "not_recovering",
            "This attempt is not inspecting a recovery.",
        ));
    }
    add_checkpoint(
        &mut m,
        &p,
        &id,
        &CheckpointInput {
            generation: input.generation,
            summary: input.summary.clone(),
            current_action: format!("Recovery disposition: {}", input.disposition),
            next_step: "Prepare an isolated checkout before continuing.".into(),
            blockers: vec![],
        },
    )
    .await?;
    sqlx::query("UPDATE attempts SET mode='work' WHERE id=?")
        .bind(&id)
        .execute(&mut *m.tx)
        .await?;
    let updated = attempt(&mut m.tx, &p, &id).await?;
    Ok(response(
        m.finish(
            json!({"attempt":updated.value(),"disposition":input.disposition}),
            Some(&p),
            "recovery.resolved",
            &id,
        )
        .await?,
    ))
}
async fn register_checkout(
    State(s): State<AppState>,
    auth: Auth,
    Path((p, id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<CheckoutInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    for (name, v) in [
        ("workstation_id", &input.workstation_id),
        ("identity", &input.identity),
        ("path", &input.path),
        ("branch", &input.branch),
        ("base_revision", &input.base_revision),
    ] {
        bounded(v, name, 4096, true)?;
    }
    if !input.clean {
        return Err(AppError::conflict(
            "dirty_checkout",
            "Use a separate clean worktree. Preserve existing edits without resetting or stashing them automatically.",
        ));
    }
    let mut m = Mutation::begin(
        &s,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{p}/attempts/{id}/checkout"),
        &input,
    )
    .await?;
    let a = owned(&mut m, &p, &id, input.generation).await?;
    if let Some(v) = m.replay {
        return Ok(response(v));
    }
    if a.mode == "recovery" {
        return Err(AppError::conflict(
            "recovery_unresolved",
            "Inspect prior work and jobs before registering editable work.",
        ));
    }
    if m.actor.kind == "agent" {
        let workstation: String =
            sqlx::query_scalar("SELECT workstation_id FROM agent_sessions WHERE id=?")
                .bind(&a.session_id)
                .fetch_one(&mut *m.tx)
                .await?;
        if workstation != input.workstation_id {
            return Err(AppError::forbidden(
                "Register a checkout on this session's declared workstation.",
            ));
        }
    }
    let conflict:i64=sqlx::query_scalar("SELECT count(*) FROM checkouts ch JOIN attempts a ON a.id=ch.attempt_id WHERE ch.workstation_id=? AND ch.identity=? AND ch.attempt_id!=? AND a.state IN ('active','expired')")
        .bind(&input.workstation_id).bind(&input.identity).bind(&id).fetch_one(&mut *m.tx).await?;
    if conflict > 0 {
        return Err(AppError::conflict(
            "checkout_in_use",
            "This checkout belongs to active or unresolved earlier work. Prepare a different worktree.",
        ));
    }
    let existing: i64 = sqlx::query_scalar("SELECT count(*) FROM checkouts WHERE attempt_id=?")
        .bind(&id)
        .fetch_one(&mut *m.tx)
        .await?;
    if existing > 0 {
        return Err(AppError::conflict(
            "checkout_registered",
            "This attempt already has a registered checkout. Retry its original mutation key to inspect the result.",
        ));
    }
    sqlx::query("INSERT INTO checkouts(attempt_id,project_id,workstation_id,identity,path,branch,base_revision,created_at) VALUES(?,?,?,?,?,?,?,?)")
        .bind(&id).bind(&p).bind(&input.workstation_id).bind(&input.identity).bind(&input.path).bind(&input.branch).bind(&input.base_revision).bind(m.now).execute(&mut *m.tx).await?;
    Ok(response(
        m.finish(
            json!({"attempt_id":id,"checkout":input}),
            Some(&p),
            "checkout.registered",
            &id,
        )
        .await?,
    ))
}
async fn events(
    State(s): State<AppState>,
    _auth: Auth,
    Path(p): Path<String>,
    Query(page): Query<Page>,
) -> Reply {
    let limit = page.limit()?;
    let cursor = page
        .cursor
        .as_deref()
        .unwrap_or("0")
        .parse::<i64>()
        .map_err(|_| AppError::bad_request("Invalid event cursor."))?;
    let mut c = s.pool.acquire().await?;
    project(&mut c, &p).await?;
    let rows=sqlx::query("SELECT seq,actor_id,kind,record_id,created_at FROM events WHERE project_id=? AND seq>? ORDER BY seq LIMIT ?").bind(&p).bind(cursor).bind(limit+1).fetch_all(&mut *c).await?;
    let more = rows.len() > limit as usize;
    let items=rows.iter().take(limit as usize).map(|r|json!({"seq":r.get::<i64,_>("seq"),"actor_id":r.get::<String,_>("actor_id"),"kind":r.get::<String,_>("kind"),"record_id":r.get::<String,_>("record_id"),"created_at":timestamp(r.get("created_at"))})).collect::<Vec<_>>();
    let next = if more {
        items.last().map(|v| v["seq"].to_string())
    } else {
        None
    };
    Ok(response(json!({"items":items,"next_cursor":next})))
}
