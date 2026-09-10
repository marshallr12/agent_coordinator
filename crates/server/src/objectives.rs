//! Optional objective grouping backed by ordinary general tasks.

use crate::{auth::Auth, error::AppError, mutation::Mutation, response, state::AppState};
use axum::{
    Json, Router,
    extract::{Path, Query, State, rejection::JsonRejection},
    http::HeaderMap,
    routing::{get, patch},
};
use coordinator_core::{ObjectiveChildInput, ObjectiveChildrenInput, ObjectiveInput, timestamp};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection};
use uuid::Uuid;

type Reply = Result<Json<Value>, AppError>;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/projects/{project}/objectives",
            get(list_objectives).post(create_objective),
        )
        .route(
            "/api/v1/projects/{project}/objectives/{id}",
            get(get_objective),
        )
        .route(
            "/api/v1/projects/{project}/objectives/{id}/children",
            patch(update_children),
        )
}

fn payload<T>(value: Result<Json<T>, JsonRejection>) -> Result<T, AppError> {
    value.map(|Json(value)| value).map_err(|_| {
        AppError::bad_request("The JSON body does not match this operation's request schema.")
    })
}

fn bounded(value: &str, name: &str, max: usize, required: bool) -> Result<(), AppError> {
    if value.len() > max || value.contains('\0') || (required && value.trim().is_empty()) {
        return Err(AppError::bad_request(&format!(
            "{name} must {}contain at most {max} bytes and no NUL characters.",
            if required { "be nonempty and " } else { "" }
        )));
    }
    Ok(())
}

fn validate_objective(input: &ObjectiveInput) -> Result<(), AppError> {
    bounded(&input.title, "title", 300, true)?;
    bounded(&input.description, "description", 32768, false)?;
    if input.acceptance_criteria.is_empty() || input.acceptance_criteria.len() > 100 {
        return Err(AppError::bad_request("Provide 1–100 acceptance criteria."));
    }
    for criterion in &input.acceptance_criteria {
        bounded(criterion, "acceptance criterion", 2048, true)?;
    }
    if !(0..=3).contains(&input.priority) {
        return Err(AppError::bad_request("priority must be between 0 and 3."));
    }
    validate_child_input(&input.children)
}

fn validate_child_input(children: &[ObjectiveChildInput]) -> Result<(), AppError> {
    if children.len() > 100 {
        return Err(AppError::bad_request(
            "An objective may contain at most 100 direct children.",
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for child in children {
        bounded(&child.task_id, "child task ID", 128, true)?;
        if !seen.insert(&child.task_id) {
            return Err(AppError::bad_request(
                "Objective child task IDs must be distinct.",
            ));
        }
    }
    Ok(())
}

async fn require_project(c: &mut SqliteConnection, project: &str) -> Result<(), AppError> {
    if sqlx::query_scalar::<_, i64>("SELECT count(*) FROM projects WHERE id=?")
        .bind(project)
        .fetch_one(c)
        .await?
        == 0
    {
        return Err(AppError::not_found());
    }
    Ok(())
}

async fn validate_children(
    c: &mut SqliteConnection,
    project: &str,
    objective: &str,
    children: &[ObjectiveChildInput],
) -> Result<(), AppError> {
    for child in children {
        if child.task_id == objective {
            return Err(AppError::bad_request("An objective cannot contain itself."));
        }
        let row = sqlx::query(
            "SELECT t.kind,EXISTS(SELECT 1 FROM workflow_activities wa WHERE wa.activity_task_id=t.id) AS workflow_activity \
             FROM tasks t WHERE t.project_id=? AND t.id=?",
        )
        .bind(project)
        .bind(&child.task_id)
        .fetch_optional(&mut *c)
        .await?
        .ok_or_else(|| {
            AppError::bad_request("Every objective child must be a task in the same project.")
        })?;
        if row.get::<bool, _>("workflow_activity") {
            return Err(AppError::bad_request(
                "Internal review and integration activities cannot be objective children.",
            ));
        }
        let other_parent = sqlx::query_scalar::<_, String>(
            "SELECT objective_task_id FROM objective_children WHERE child_task_id=? AND objective_task_id!=?",
        )
        .bind(&child.task_id)
        .bind(objective)
        .fetch_optional(&mut *c)
        .await?;
        if other_parent.is_some() {
            return Err(AppError::conflict(
                "objective_parent_conflict",
                "A task can belong to only one parent objective.",
            ));
        }
        let cycle: i64 = sqlx::query_scalar(
            "WITH RECURSIVE reachable(id) AS ( \
                SELECT ? \
                UNION \
                SELECT d.prerequisite_id FROM task_dependencies d JOIN reachable r ON d.task_id=r.id \
                UNION \
                SELECT oc.child_task_id FROM objective_children oc JOIN reachable r ON oc.objective_task_id=r.id \
             ) SELECT count(*) FROM reachable WHERE id=?",
        )
        .bind(&child.task_id)
        .bind(objective)
        .fetch_one(&mut *c)
        .await?;
        if cycle > 0 {
            return Err(AppError::conflict(
                "dependency_cycle",
                "This objective membership would create a cycle through task prerequisites or objectives.",
            ));
        }
    }
    Ok(())
}

async fn replace_children(
    c: &mut SqliteConnection,
    project: &str,
    objective: &str,
    children: &[ObjectiveChildInput],
) -> Result<(), AppError> {
    validate_children(c, project, objective, children).await?;
    sqlx::query("DELETE FROM objective_children WHERE objective_task_id=?")
        .bind(objective)
        .execute(&mut *c)
        .await?;
    for (position, child) in children.iter().enumerate() {
        sqlx::query("INSERT INTO objective_children(project_id,objective_task_id,child_task_id,required,position) VALUES(?,?,?,?,?)")
            .bind(project).bind(objective).bind(&child.task_id).bind(child.required).bind(position as i64)
            .execute(&mut *c).await?;
    }
    Ok(())
}

async fn objective_summary(
    c: &mut SqliteConnection,
    project: &str,
    id: &str,
    now: i64,
) -> Result<Value, AppError> {
    let row = sqlx::query(
        "SELECT o.revision AS objective_revision,o.created_by,o.created_at,o.updated_at, \
         (t.lifecycle NOT IN ('open','planned') OR EXISTS(SELECT 1 FROM attempts a WHERE a.task_id=o.task_id) OR EXISTS(SELECT 1 FROM workflow_subjects ws WHERE ws.task_id=o.task_id)) AS membership_frozen, \
         count(oc.child_task_id) AS child_count, \
         COALESCE(sum(CASE WHEN oc.required=1 THEN 1 ELSE 0 END),0) AS required_child_count, \
         COALESCE(sum(CASE WHEN oc.required=1 AND child.lifecycle='done' THEN 1 ELSE 0 END),0) AS completed_required_child_count \
         FROM objectives o JOIN tasks t ON t.id=o.task_id AND t.project_id=o.project_id \
         LEFT JOIN objective_children oc ON oc.objective_task_id=o.task_id \
         LEFT JOIN tasks child ON child.id=oc.child_task_id \
         WHERE o.project_id=? AND o.task_id=? GROUP BY o.task_id",
    )
    .bind(project)
    .bind(id)
    .fetch_optional(&mut *c)
    .await?
    .ok_or_else(AppError::not_found)?;
    let required_count = row.get::<i64, _>("required_child_count");
    let completed_required = row.get::<i64, _>("completed_required_child_count");
    let mut value = crate::coordination::task_record_value(c, project, id, now).await?;
    value["objective_revision"] = json!(row.get::<i64, _>("objective_revision"));
    value["child_count"] = json!(row.get::<i64, _>("child_count"));
    value["required_child_count"] = json!(required_count);
    value["completed_required_child_count"] = json!(completed_required);
    value["required_children_ready"] = json!(required_count == completed_required);
    value["membership_frozen"] = json!(row.get::<bool, _>("membership_frozen"));
    value["created_by"] = json!(row.get::<String, _>("created_by"));
    value["objective_created_at"] = json!(timestamp(row.get("created_at")));
    value["objective_updated_at"] = json!(timestamp(row.get("updated_at")));
    Ok(value)
}

async fn objective_detail(
    c: &mut SqliteConnection,
    project: &str,
    id: &str,
    now: i64,
    history_cursor: Option<i64>,
    history_limit: i64,
) -> Result<Value, AppError> {
    let mut value = objective_summary(c, project, id, now).await?;
    let rows = sqlx::query(
        "SELECT oc.child_task_id,oc.required,oc.position,t.title,t.lifecycle,t.revision \
         FROM objective_children oc JOIN tasks t ON t.id=oc.child_task_id AND t.project_id=oc.project_id \
         WHERE oc.project_id=? AND oc.objective_task_id=? ORDER BY oc.position,oc.child_task_id",
    )
    .bind(project)
    .bind(id)
    .fetch_all(&mut *c)
    .await?;
    let mut children = Vec::with_capacity(rows.len());
    for row in rows {
        let child_id = row.get::<String, _>("child_task_id");
        let task = crate::coordination::task_record_value(c, project, &child_id, now).await?;
        children.push(json!({
            "task_id":child_id,
            "title":row.get::<String,_>("title"),
            "lifecycle":row.get::<String,_>("lifecycle"),
            "revision":row.get::<i64,_>("revision"),
            "work_status":task["work_status"],
            "required":row.get::<bool,_>("required"),
            "position":row.get::<i64,_>("position")
        }));
    }
    let history = sqlx::query(
        "SELECT revision,children_json,actor_id,created_at FROM objective_revisions \
         WHERE objective_task_id=? AND (? IS NULL OR revision<?) ORDER BY revision DESC LIMIT ?",
    )
    .bind(id)
    .bind(history_cursor)
    .bind(history_cursor)
    .bind(history_limit + 1)
    .fetch_all(&mut *c)
    .await?;
    let more = history.len() > history_limit as usize;
    let history = &history[..history.len().min(history_limit as usize)];
    value["children"] = json!(children);
    value["membership_history"] = json!(history
        .iter()
        .map(|row| -> Result<Value,AppError> { Ok(json!({
            "revision":row.get::<i64,_>("revision"),
            "children":serde_json::from_str::<Value>(&row.get::<String,_>("children_json"))?,
            "actor_id":row.get::<String,_>("actor_id"),
            "created_at":timestamp(row.get("created_at"))
        })) })
        .collect::<Result<Vec<_>,_>>()?);
    value["membership_history_next_cursor"] = if more {
        history
            .last()
            .map(|row| json!(row.get::<i64, _>("revision")))
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    value["membership_history_limit"] = json!(history_limit);
    Ok(value)
}

#[derive(Deserialize)]
struct ListQuery {
    cursor: Option<String>,
    limit: Option<i64>,
}

async fn list_objectives(
    State(state): State<AppState>,
    _auth: Auth,
    Path(project): Path<String>,
    Query(query): Query<ListQuery>,
) -> Reply {
    let limit = query.limit.unwrap_or(50);
    if !(1..=200).contains(&limit)
        || query
            .cursor
            .as_ref()
            .is_some_and(|value| value.is_empty() || value.len() > 128)
    {
        return Err(AppError::bad_request(
            "limit must be 1–200 and cursor must be a bounded objective ID.",
        ));
    }
    let mut c = state.pool.begin().await?;
    require_project(&mut c, &project).await?;
    let rows = sqlx::query(
        "SELECT task_id FROM objectives WHERE project_id=? AND (? IS NULL OR task_id>?) ORDER BY task_id LIMIT ?",
    )
    .bind(&project)
    .bind(&query.cursor)
    .bind(&query.cursor)
    .bind(limit + 1)
    .fetch_all(&mut *c)
    .await?;
    let more = rows.len() > limit as usize;
    let rows = &rows[..rows.len().min(limit as usize)];
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        items.push(
            objective_summary(
                &mut c,
                &project,
                &row.get::<String, _>("task_id"),
                state.now(),
            )
            .await?,
        );
    }
    let next_cursor = if more {
        rows.last().map(|row| row.get::<String, _>("task_id"))
    } else {
        None
    };
    Ok(response(json!({"items":items,"next_cursor":next_cursor})))
}

#[derive(Deserialize)]
struct DetailQuery {
    cursor: Option<i64>,
    limit: Option<i64>,
}

async fn get_objective(
    State(state): State<AppState>,
    _auth: Auth,
    Path((project, id)): Path<(String, String)>,
    Query(query): Query<DetailQuery>,
) -> Reply {
    let limit = query.limit.unwrap_or(50);
    if !(1..=200).contains(&limit) || query.cursor.is_some_and(|value| value <= 0) {
        return Err(AppError::bad_request(
            "History limit must be 1–200 and cursor must be a positive revision.",
        ));
    }
    let mut c = state.pool.begin().await?;
    Ok(response(
        objective_detail(&mut c, &project, &id, state.now(), query.cursor, limit).await?,
    ))
}

async fn create_objective(
    State(state): State<AppState>,
    auth: Auth,
    Path(project): Path<String>,
    headers: HeaderMap,
    body: Result<Json<ObjectiveInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    validate_objective(&input)?;
    let mut mutation = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{project}/objectives"),
        &input,
    )
    .await?;
    require_project(&mut mutation.tx, &project).await?;
    if let Some(value) = mutation.replay {
        return Ok(response(value));
    }
    let id = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO tasks(id,project_id,title,description,acceptance_json,kind,priority,lifecycle,created_at,ready_since) VALUES(?,?,?,?,?,'general',?,?,?,?)")
        .bind(&id).bind(&project).bind(&input.title).bind(&input.description)
        .bind(serde_json::to_string(&input.acceptance_criteria)?).bind(input.priority)
        .bind(if input.planned { "planned" } else { "open" }).bind(mutation.now).bind(mutation.now)
        .execute(&mut *mutation.tx).await?;
    sqlx::query("INSERT INTO objectives(project_id,task_id,revision,created_by,created_at,updated_at) VALUES(?,?,1,?,?,?)")
        .bind(&project).bind(&id).bind(&mutation.actor.id).bind(mutation.now).bind(mutation.now)
        .execute(&mut *mutation.tx).await?;
    replace_children(&mut mutation.tx, &project, &id, &input.children).await?;
    sqlx::query("INSERT INTO objective_revisions(objective_task_id,revision,children_json,actor_id,created_at) VALUES(?,1,?,?,?)")
        .bind(&id).bind(serde_json::to_string(&input.children)?).bind(&mutation.actor.id).bind(mutation.now)
        .execute(&mut *mutation.tx).await?;
    crate::coordination::save_task_revision(&mut mutation, &project, &id).await?;
    let value = objective_detail(&mut mutation.tx, &project, &id, mutation.now, None, 50).await?;
    Ok(response(
        mutation
            .finish(value, Some(&project), "objective.created", &id)
            .await?,
    ))
}

async fn update_children(
    State(state): State<AppState>,
    auth: Auth,
    Path((project, id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<ObjectiveChildrenInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    validate_child_input(&input.children)?;
    if input.expected_revision <= 0 {
        return Err(AppError::bad_request("expected_revision must be positive."));
    }
    let mut mutation = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("PATCH /api/v1/projects/{project}/objectives/{id}/children"),
        &input,
    )
    .await?;
    let row = sqlx::query(
        "SELECT o.revision,t.lifecycle,EXISTS(SELECT 1 FROM attempts a WHERE a.task_id=o.task_id) AS attempted, \
         EXISTS(SELECT 1 FROM workflow_subjects ws WHERE ws.task_id=o.task_id) AS submitted \
         FROM objectives o JOIN tasks t ON t.id=o.task_id AND t.project_id=o.project_id \
         WHERE o.project_id=? AND o.task_id=?",
    )
    .bind(&project)
    .bind(&id)
    .fetch_optional(&mut *mutation.tx)
    .await?
    .ok_or_else(AppError::not_found)?;
    if let Some(value) = mutation.replay {
        return Ok(response(value));
    }
    if row.get::<i64, _>("revision") != input.expected_revision {
        return Err(AppError::conflict(
            "revision_conflict",
            "Read the current objective revision before changing its children.",
        ));
    }
    if row.get::<bool, _>("attempted")
        || row.get::<bool, _>("submitted")
        || !matches!(
            row.get::<String, _>("lifecycle").as_str(),
            "open" | "planned"
        )
    {
        return Err(AppError::conflict(
            "objective_membership_frozen",
            "Objective membership is frozen after its work begins. Preserve the reviewed completion scope.",
        ));
    }
    replace_children(&mut mutation.tx, &project, &id, &input.children).await?;
    let next = input.expected_revision + 1;
    sqlx::query("INSERT INTO objective_revisions(objective_task_id,revision,children_json,actor_id,created_at) VALUES(?,?,?,?,?)")
        .bind(&id).bind(next).bind(serde_json::to_string(&input.children)?).bind(&mutation.actor.id).bind(mutation.now)
        .execute(&mut *mutation.tx).await?;
    sqlx::query("UPDATE objectives SET revision=?,updated_at=? WHERE project_id=? AND task_id=? AND revision=?")
        .bind(next).bind(mutation.now).bind(&project).bind(&id).bind(input.expected_revision)
        .execute(&mut *mutation.tx).await?;
    sqlx::query("UPDATE tasks SET revision=revision+1 WHERE project_id=? AND id=?")
        .bind(&project)
        .bind(&id)
        .execute(&mut *mutation.tx)
        .await?;
    crate::coordination::save_task_revision(&mut mutation, &project, &id).await?;
    let value = objective_detail(&mut mutation.tx, &project, &id, mutation.now, None, 50).await?;
    Ok(response(
        mutation
            .finish(value, Some(&project), "objective.children_updated", &id)
            .await?,
    ))
}

/// Required child completion is checked inside the caller's writer transaction.
pub async fn ensure_required_children_done(
    c: &mut SqliteConnection,
    project: &str,
    objective: &str,
) -> Result<(), AppError> {
    let incomplete = sqlx::query_scalar::<_, String>(
        "SELECT child.id FROM objectives o JOIN objective_children oc ON oc.objective_task_id=o.task_id \
         JOIN tasks child ON child.project_id=oc.project_id AND child.id=oc.child_task_id \
         WHERE o.project_id=? AND o.task_id=? AND oc.required=1 AND child.lifecycle!='done' \
         ORDER BY oc.position,child.id LIMIT 21",
    )
    .bind(project)
    .bind(objective)
    .fetch_all(&mut *c)
    .await?;
    if incomplete.is_empty() {
        return Ok(());
    }
    let truncated = incomplete.len() > 20;
    Err(AppError::conflict(
        "objective_children_incomplete",
        "Complete every required child before starting or completing this objective's own work.",
    )
    .with_details(json!({
        "task_ids":incomplete.into_iter().take(20).collect::<Vec<_>>(),
        "truncated":truncated
    })))
}
