//! Immutable shared knowledge, bounded context search, and revision-scoped decisions.

use crate::{
    auth::{Actor, Auth},
    error::AppError,
    mutation::Mutation,
    response,
    state::AppState,
};
use axum::{
    Json, Router,
    extract::{Path, Query, State, rejection::JsonRejection},
    http::HeaderMap,
    routing::{get, post},
};
use coordinator_core::{
    DecisionAnswerInput, DecisionInput, DecisionReopenInput, DecisionTaskInput, KnowledgeEditInput,
    KnowledgeFeedbackInput, KnowledgeInput, KnowledgeProvenance, KnowledgeScope,
    SubmissionLessonInput, timestamp,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection};
use uuid::Uuid;

type Reply = Result<Json<Value>, AppError>;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/projects/{project}/knowledge",
            get(list_knowledge).post(create_knowledge),
        )
        .route(
            "/api/v1/projects/{project}/knowledge/{id}",
            get(get_knowledge).patch(edit_knowledge),
        )
        .route(
            "/api/v1/projects/{project}/knowledge/{id}/feedback",
            post(add_feedback),
        )
        .route("/api/v1/projects/{project}/context", get(context))
        .route(
            "/api/v1/projects/{project}/policy/history",
            get(policy_history),
        )
        .route(
            "/api/v1/projects/{project}/decisions",
            get(list_decisions).post(create_decision),
        )
        .route(
            "/api/v1/projects/{project}/decisions/{id}",
            get(get_decision),
        )
        .route(
            "/api/v1/projects/{project}/decisions/{id}/answer",
            post(answer_decision),
        )
        .route(
            "/api/v1/projects/{project}/decisions/{id}/reopen",
            post(reopen_decision),
        )
}

fn payload<T>(value: Result<Json<T>, JsonRejection>) -> Result<T, AppError> {
    value.map(|Json(v)| v).map_err(|_| {
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

fn valid_knowledge_kind(value: &str) -> bool {
    matches!(
        value,
        "lesson" | "fact" | "rejected_approach" | "checkpoint"
    )
}

fn valid_knowledge_status(value: &str) -> bool {
    matches!(
        value,
        "observed" | "validated" | "deprecated" | "superseded"
    )
}

fn validate_tags(tags: &[String]) -> Result<(), AppError> {
    if tags.len() > 50 {
        return Err(AppError::bad_request("Provide no more than 50 tags."));
    }
    let mut normalized = std::collections::BTreeSet::new();
    for tag in tags {
        bounded(tag, "tag", 64, true)?;
        if !normalized.insert(tag.to_lowercase()) {
            return Err(AppError::bad_request(
                "Knowledge tags must be unique ignoring case.",
            ));
        }
    }
    Ok(())
}

fn validate_scope(scope: &KnowledgeScope) -> Result<(), AppError> {
    if scope.task_ids.len() > 100
        || scope.components.len() > 50
        || scope.environments.len() > 50
        || scope.versions.len() > 50
    {
        return Err(AppError::bad_request(
            "Knowledge scope contains too many values.",
        ));
    }
    let mut tasks = std::collections::BTreeSet::new();
    for id in &scope.task_ids {
        bounded(id, "scope task id", 128, true)?;
        if !tasks.insert(id) {
            return Err(AppError::bad_request(
                "Knowledge scope task IDs must be unique.",
            ));
        }
    }
    for (name, values) in [
        ("component", &scope.components),
        ("environment", &scope.environments),
        ("version", &scope.versions),
    ] {
        let mut unique = std::collections::BTreeSet::new();
        for value in values {
            bounded(value, name, 255, true)?;
            if !unique.insert(value.to_lowercase()) {
                return Err(AppError::bad_request(&format!(
                    "Knowledge scope {name} values must be unique ignoring case."
                )));
            }
        }
    }
    Ok(())
}

fn validate_provenance(value: &KnowledgeProvenance) -> Result<(), AppError> {
    bounded(&value.summary, "provenance summary", 4096, true)?;
    if let Some(uri) = &value.source_uri {
        bounded(uri, "provenance source_uri", 4096, true)?;
    }
    if let Some(id) = &value.source_task_id {
        bounded(id, "provenance source_task_id", 128, true)?;
    }
    if let Some(id) = &value.source_submission_id {
        bounded(id, "provenance source_submission_id", 128, true)?;
    }
    Ok(())
}

fn validate_knowledge(input: &KnowledgeInput) -> Result<(), AppError> {
    if !valid_knowledge_kind(&input.kind) || !valid_knowledge_status(&input.status) {
        return Err(AppError::bad_request(
            "Knowledge kind or status is not supported.",
        ));
    }
    bounded(&input.title, "title", 255, true)?;
    bounded(&input.body, "body", 32768, true)?;
    bounded(&input.applicability, "applicability", 4096, false)?;
    validate_scope(&input.scope)?;
    validate_tags(&input.tags)?;
    validate_provenance(&input.provenance)?;
    if !matches!(input.collection.as_str(), "project" | "shared")
        || (input.collection == "shared") != input.share_across_projects
    {
        return Err(AppError::bad_request(
            "Shared knowledge requires collection=shared and share_across_projects=true; project knowledge requires both to be project-scoped.",
        ));
    }
    Ok(())
}

async fn require_project(c: &mut SqliteConnection, project: &str) -> Result<i64, AppError> {
    sqlx::query_scalar("SELECT policy_revision FROM projects WHERE id=?")
        .bind(project)
        .fetch_optional(c)
        .await?
        .ok_or_else(AppError::not_found)
}

async fn validate_knowledge_references(
    c: &mut SqliteConnection,
    project: &str,
    scope: &KnowledgeScope,
    provenance: &KnowledgeProvenance,
) -> Result<(), AppError> {
    for task in &scope.task_ids {
        if sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tasks WHERE project_id=? AND id=?")
            .bind(project)
            .bind(task)
            .fetch_one(&mut *c)
            .await?
            == 0
        {
            return Err(AppError::bad_request(
                "Every scoped task must belong to the source project.",
            ));
        }
    }
    if let Some(task) = &provenance.source_task_id
        && sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tasks WHERE project_id=? AND id=?")
            .bind(project)
            .bind(task)
            .fetch_one(&mut *c)
            .await?
            == 0
    {
        return Err(AppError::bad_request(
            "The provenance task must belong to the source project.",
        ));
    }
    if let Some(submission) = &provenance.source_submission_id {
        let expected_task = provenance.source_task_id.as_deref();
        let row = sqlx::query("SELECT task_id FROM submissions WHERE project_id=? AND id=?")
            .bind(project)
            .bind(submission)
            .fetch_optional(&mut *c)
            .await?;
        if row.as_ref().map(|r| r.get::<String, _>("task_id")) != expected_task.map(str::to_owned) {
            return Err(AppError::bad_request(
                "The provenance submission must belong to the source project and provenance task.",
            ));
        }
    }
    Ok(())
}

async fn insert_knowledge_record(
    c: &mut SqliteConnection,
    actor: &Actor,
    now: i64,
    project: &str,
    input: &KnowledgeInput,
) -> Result<Value, AppError> {
    let id = Uuid::new_v4().to_string();
    let scope = serde_json::to_string(&input.scope)?;
    let tags = serde_json::to_string(&input.tags)?;
    let provenance = serde_json::to_string(&input.provenance)?;
    sqlx::query("INSERT INTO knowledge_records(id,source_project_id,collection,current_revision,kind,status,title,body,scope_json,tags_json,applicability,provenance_json,created_by,created_at,updated_at) VALUES(?,?,?,1,?,?,?,?,?,?,?,?,?,?,?)")
        .bind(&id).bind(project).bind(&input.collection).bind(&input.kind).bind(&input.status)
        .bind(&input.title).bind(&input.body).bind(&scope).bind(&tags).bind(&input.applicability)
        .bind(&provenance).bind(&actor.id).bind(now).bind(now).execute(&mut *c).await?;
    sqlx::query("INSERT INTO knowledge_revisions(knowledge_id,revision,kind,status,title,body,scope_json,tags_json,applicability,provenance_json,actor_id,created_at) VALUES(?,1,?,?,?,?,?,?,?,?,?,?)")
        .bind(&id).bind(&input.kind).bind(&input.status).bind(&input.title).bind(&input.body)
        .bind(&scope).bind(&tags).bind(&input.applicability).bind(&provenance).bind(&actor.id)
        .bind(now).execute(&mut *c).await?;
    knowledge_value(c, project, &id).await
}

fn knowledge_row_value(row: &sqlx::sqlite::SqliteRow) -> Result<Value, AppError> {
    Ok(json!({
        "id": row.get::<String,_>("id"),
        "source_project_id": row.get::<String,_>("source_project_id"),
        "collection": row.get::<String,_>("collection"),
        "revision": row.get::<i64,_>("current_revision"),
        "kind": row.get::<String,_>("kind"),
        "status": row.get::<String,_>("status"),
        "title": row.get::<String,_>("title"),
        "body": row.get::<String,_>("body"),
        "scope": serde_json::from_str::<Value>(&row.get::<String,_>("scope_json"))?,
        "tags": serde_json::from_str::<Value>(&row.get::<String,_>("tags_json"))?,
        "applicability": row.get::<String,_>("applicability"),
        "provenance": serde_json::from_str::<Value>(&row.get::<String,_>("provenance_json"))?,
        "superseded_by_id": row.get::<Option<String>,_>("superseded_by_id"),
        "created_by": row.get::<String,_>("created_by"),
        "created_at": timestamp(row.get("created_at")),
        "updated_at": timestamp(row.get("updated_at")),
    }))
}

async fn knowledge_value(
    c: &mut SqliteConnection,
    project: &str,
    id: &str,
) -> Result<Value, AppError> {
    let row = sqlx::query("SELECT * FROM knowledge_records WHERE source_project_id=? AND id=?")
        .bind(project)
        .bind(id)
        .fetch_optional(c)
        .await?
        .ok_or_else(AppError::not_found)?;
    knowledge_row_value(&row)
}

#[derive(Deserialize)]
struct KnowledgePage {
    cursor: Option<String>,
    limit: Option<i64>,
    kind: Option<String>,
    status: Option<String>,
    tag: Option<String>,
    #[serde(default)]
    include_shared: bool,
}

impl KnowledgePage {
    fn limit(&self) -> Result<i64, AppError> {
        let limit = self.limit.unwrap_or(50);
        if !(1..=200).contains(&limit) {
            return Err(AppError::bad_request("limit must be between 1 and 200."));
        }
        Ok(limit)
    }
    fn cursor(&self) -> Result<(Option<i64>, Option<String>), AppError> {
        let Some(cursor) = &self.cursor else {
            return Ok((None, None));
        };
        let (time, id) = cursor
            .split_once(':')
            .ok_or_else(|| AppError::bad_request("Invalid knowledge cursor."))?;
        let time = time
            .parse::<i64>()
            .map_err(|_| AppError::bad_request("Invalid knowledge cursor."))?;
        bounded(id, "cursor", 128, true)?;
        Ok((Some(time), Some(id.to_owned())))
    }
}

async fn list_knowledge(
    State(state): State<AppState>,
    _auth: Auth,
    Path(project): Path<String>,
    Query(page): Query<KnowledgePage>,
) -> Reply {
    let limit = page.limit()?;
    if page
        .kind
        .as_deref()
        .is_some_and(|v| !valid_knowledge_kind(v))
        || page
            .status
            .as_deref()
            .is_some_and(|v| !valid_knowledge_status(v))
    {
        return Err(AppError::bad_request(
            "Invalid knowledge kind or status filter.",
        ));
    }
    if let Some(tag) = &page.tag {
        bounded(tag, "tag", 64, true)?;
    }
    let (cursor_time, cursor_id) = page.cursor()?;
    let mut c = state.pool.acquire().await?;
    require_project(&mut c, &project).await?;
    let rows = sqlx::query(
        "SELECT * FROM knowledge_records k WHERE (k.source_project_id=? OR (? AND k.collection='shared')) AND (? IS NULL OR k.kind=?) AND (? IS NULL OR k.status=?) AND (? IS NULL OR EXISTS(SELECT 1 FROM json_each(k.tags_json) WHERE lower(value)=lower(?))) AND (? IS NULL OR k.updated_at<? OR (k.updated_at=? AND k.id<?)) ORDER BY k.updated_at DESC,k.id DESC LIMIT ?",
    )
    .bind(&project).bind(page.include_shared).bind(&page.kind).bind(&page.kind)
    .bind(&page.status).bind(&page.status).bind(&page.tag).bind(&page.tag)
    .bind(cursor_time).bind(cursor_time).bind(cursor_time).bind(cursor_id)
    .bind(limit + 1).fetch_all(&mut *c).await?;
    let more = rows.len() > limit as usize;
    let rows = &rows[..rows.len().min(limit as usize)];
    let items = rows
        .iter()
        .map(knowledge_row_value)
        .collect::<Result<Vec<_>, _>>()?;
    let next_cursor = if more {
        rows.last().map(|row| {
            format!(
                "{}:{}",
                row.get::<i64, _>("updated_at"),
                row.get::<String, _>("id")
            )
        })
    } else {
        None
    };
    Ok(response(json!({"items":items,"next_cursor":next_cursor})))
}

async fn create_knowledge(
    State(state): State<AppState>,
    auth: Auth,
    Path(project): Path<String>,
    headers: HeaderMap,
    body: Result<Json<KnowledgeInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    validate_knowledge(&input)?;
    let mut mutation = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{project}/knowledge"),
        &input,
    )
    .await?;
    require_project(&mut mutation.tx, &project).await?;
    if let Some(value) = mutation.replay {
        return Ok(response(value));
    }
    validate_knowledge_references(&mut mutation.tx, &project, &input.scope, &input.provenance)
        .await?;
    let value = insert_knowledge_record(
        &mut mutation.tx,
        &mutation.actor,
        mutation.now,
        &project,
        &input,
    )
    .await?;
    let id = value["id"].as_str().unwrap_or_default().to_owned();
    Ok(response(
        mutation
            .finish(value, Some(&project), "knowledge.created", &id)
            .await?,
    ))
}

#[derive(Deserialize)]
struct HistoryPage {
    cursor: Option<i64>,
    limit: Option<i64>,
}

async fn policy_history(
    State(state): State<AppState>,
    _auth: Auth,
    Path(project): Path<String>,
    Query(page): Query<HistoryPage>,
) -> Reply {
    let limit = page.limit.unwrap_or(50);
    if !(1..=200).contains(&limit) || page.cursor.is_some_and(|v| v <= 0) {
        return Err(AppError::bad_request(
            "History limit must be 1–200 and cursor must be a positive revision.",
        ));
    }
    let mut c = state.pool.acquire().await?;
    require_project(&mut c, &project).await?;
    let rows = sqlx::query("SELECT revision,data_json,actor_id,created_at,provenance FROM policy_revisions WHERE project_id=? AND (? IS NULL OR revision<?) ORDER BY revision DESC LIMIT ?")
        .bind(&project).bind(page.cursor).bind(page.cursor).bind(limit+1).fetch_all(&mut *c).await?;
    let more = rows.len() > limit as usize;
    let rows = &rows[..rows.len().min(limit as usize)];
    let items = rows
        .iter()
        .map(|row| -> Result<Value, AppError> {
            Ok(json!({
                "revision":row.get::<i64,_>("revision"),
                "policy":serde_json::from_str::<Value>(&row.get::<String,_>("data_json"))?,
                "actor_id":row.get::<String,_>("actor_id"),
                "created_at":timestamp(row.get("created_at")),
                "provenance":row.get::<String,_>("provenance")
            }))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let next_cursor = if more {
        rows.last().map(|row| row.get::<i64, _>("revision"))
    } else {
        None
    };
    Ok(response(json!({"items":items,"next_cursor":next_cursor})))
}

async fn get_knowledge(
    State(state): State<AppState>,
    _auth: Auth,
    Path((project, id)): Path<(String, String)>,
    Query(page): Query<HistoryPage>,
) -> Reply {
    let limit = page.limit.unwrap_or(50);
    if !(1..=200).contains(&limit) || page.cursor.is_some_and(|v| v <= 0) {
        return Err(AppError::bad_request(
            "History limit must be 1–200 and cursor must be a positive revision.",
        ));
    }
    let mut c = state.pool.acquire().await?;
    let mut value = knowledge_value(&mut c, &project, &id).await?;
    let rows = sqlx::query("SELECT * FROM knowledge_revisions WHERE knowledge_id=? AND (? IS NULL OR revision<?) ORDER BY revision DESC LIMIT ?")
        .bind(&id).bind(page.cursor).bind(page.cursor).bind(limit+1).fetch_all(&mut *c).await?;
    let more = rows.len() > limit as usize;
    let rows = &rows[..rows.len().min(limit as usize)];
    let revisions = rows.iter().map(|row| -> Result<Value,AppError> { Ok(json!({
        "revision":row.get::<i64,_>("revision"),"kind":row.get::<String,_>("kind"),"status":row.get::<String,_>("status"),
        "title":row.get::<String,_>("title"),"body":row.get::<String,_>("body"),
        "scope":serde_json::from_str::<Value>(&row.get::<String,_>("scope_json"))?,"tags":serde_json::from_str::<Value>(&row.get::<String,_>("tags_json"))?,
        "applicability":row.get::<String,_>("applicability"),"provenance":serde_json::from_str::<Value>(&row.get::<String,_>("provenance_json"))?,
        "superseded_by_id":row.get::<Option<String>,_>("superseded_by_id"),"actor_id":row.get::<String,_>("actor_id"),"created_at":timestamp(row.get("created_at"))
    }))}).collect::<Result<Vec<_>,_>>()?;
    let feedback = sqlx::query("SELECT useful,count(*) AS count FROM knowledge_feedback WHERE knowledge_id=? GROUP BY useful")
        .bind(&id).fetch_all(&mut *c).await?;
    let useful = feedback
        .iter()
        .find(|r| r.get::<i64, _>("useful") == 1)
        .map(|r| r.get::<i64, _>("count"))
        .unwrap_or(0);
    let not_useful = feedback
        .iter()
        .find(|r| r.get::<i64, _>("useful") == 0)
        .map(|r| r.get::<i64, _>("count"))
        .unwrap_or(0);
    value["revisions"] = json!(revisions);
    value["revisions_next_cursor"] = if more {
        rows.last().map(|r| r.get::<i64, _>("revision")).into()
    } else {
        Value::Null
    };
    value["feedback_summary"] = json!({"useful":useful,"not_useful":not_useful});
    Ok(response(value))
}

async fn edit_knowledge(
    State(state): State<AppState>,
    auth: Auth,
    Path((project, id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<KnowledgeEditInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    let for_validation = KnowledgeInput {
        kind: "lesson".into(),
        title: input.title.clone(),
        body: input.body.clone(),
        status: input.status.clone(),
        scope: input.scope.clone(),
        tags: input.tags.clone(),
        applicability: input.applicability.clone(),
        provenance: input.provenance.clone(),
        collection: "project".into(),
        share_across_projects: false,
    };
    validate_knowledge(&for_validation)?;
    if input.expected_revision <= 0 {
        return Err(AppError::bad_request("expected_revision must be positive."));
    }
    let mut mutation = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("PATCH /api/v1/projects/{project}/knowledge/{id}"),
        &input,
    )
    .await?;
    let current = sqlx::query("SELECT * FROM knowledge_records WHERE source_project_id=? AND id=?")
        .bind(&project)
        .bind(&id)
        .fetch_optional(&mut *mutation.tx)
        .await?
        .ok_or_else(AppError::not_found)?;
    if let Some(value) = mutation.replay {
        return Ok(response(value));
    }
    let revision = current.get::<i64, _>("current_revision");
    if revision != input.expected_revision {
        return Err(AppError::conflict(
            "revision_conflict",
            "Read the current knowledge revision before correcting it.",
        ));
    }
    validate_knowledge_references(&mut mutation.tx, &project, &input.scope, &input.provenance)
        .await?;
    if let Some(other) = &input.superseded_by_id {
        if input.status != "superseded" {
            return Err(AppError::bad_request(
                "superseded_by_id requires status=superseded.",
            ));
        }
        if sqlx::query_scalar::<_, i64>("SELECT count(*) FROM knowledge_records WHERE id=?")
            .bind(other)
            .fetch_one(&mut *mutation.tx)
            .await?
            == 0
        {
            return Err(AppError::bad_request(
                "The superseding knowledge record does not exist.",
            ));
        }
    } else if input.status == "superseded" {
        return Err(AppError::bad_request(
            "A superseded revision requires superseded_by_id.",
        ));
    }
    let next = revision + 1;
    let scope = serde_json::to_string(&input.scope)?;
    let tags = serde_json::to_string(&input.tags)?;
    let provenance = serde_json::to_string(&input.provenance)?;
    let kind = current.get::<String, _>("kind");
    sqlx::query("INSERT INTO knowledge_revisions(knowledge_id,revision,kind,status,title,body,scope_json,tags_json,applicability,provenance_json,superseded_by_id,actor_id,created_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)")
        .bind(&id).bind(next).bind(&kind).bind(&input.status).bind(&input.title).bind(&input.body).bind(&scope).bind(&tags)
        .bind(&input.applicability).bind(&provenance).bind(&input.superseded_by_id).bind(&mutation.actor.id).bind(mutation.now)
        .execute(&mut *mutation.tx).await?;
    sqlx::query("UPDATE knowledge_records SET current_revision=?,status=?,title=?,body=?,scope_json=?,tags_json=?,applicability=?,provenance_json=?,superseded_by_id=?,updated_at=? WHERE id=?")
        .bind(next).bind(&input.status).bind(&input.title).bind(&input.body).bind(&scope).bind(&tags).bind(&input.applicability)
        .bind(&provenance).bind(&input.superseded_by_id).bind(mutation.now).bind(&id).execute(&mut *mutation.tx).await?;
    let value = knowledge_value(&mut mutation.tx, &project, &id).await?;
    Ok(response(
        mutation
            .finish(value, Some(&project), "knowledge.revised", &id)
            .await?,
    ))
}

async fn add_feedback(
    State(state): State<AppState>,
    auth: Auth,
    Path((project, id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<KnowledgeFeedbackInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    bounded(&input.comment, "comment", 2048, false)?;
    if input.expected_revision <= 0 {
        return Err(AppError::bad_request("expected_revision must be positive."));
    }
    let mut mutation = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{project}/knowledge/{id}/feedback"),
        &input,
    )
    .await?;
    let revision: i64 = sqlx::query_scalar(
        "SELECT current_revision FROM knowledge_records WHERE source_project_id=? AND id=?",
    )
    .bind(&project)
    .bind(&id)
    .fetch_optional(&mut *mutation.tx)
    .await?
    .ok_or_else(AppError::not_found)?;
    if revision != input.expected_revision {
        return Err(AppError::conflict(
            "revision_conflict",
            "Read the current knowledge revision before recording feedback.",
        ));
    }
    if let Some(value) = mutation.replay {
        return Ok(response(value));
    }
    let feedback_id = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO knowledge_feedback(id,knowledge_id,revision,actor_id,useful,comment,created_at) VALUES(?,?,?,?,?,?,?)")
        .bind(&feedback_id).bind(&id).bind(revision).bind(&mutation.actor.id).bind(input.useful).bind(&input.comment).bind(mutation.now)
        .execute(&mut *mutation.tx).await?;
    let value = json!({"id":feedback_id,"knowledge_id":id,"revision":revision,"useful":input.useful,"comment":input.comment,"actor_id":mutation.actor.id,"created_at":timestamp(mutation.now)});
    Ok(response(
        mutation
            .finish(value, Some(&project), "knowledge.feedback_added", &id)
            .await?,
    ))
}

#[derive(Deserialize)]
struct ContextQuery {
    q: String,
    limit: Option<i64>,
    budget: Option<usize>,
    #[serde(default)]
    include_shared: bool,
    task_id: Option<String>,
    component: Option<String>,
    environment: Option<String>,
    version: Option<String>,
}

fn fts_query(value: &str) -> Result<String, AppError> {
    bounded(value, "q", 1024, true)?;
    let tokens = value
        .split_whitespace()
        .map(|token| token.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-'))
        .filter(|token| !token.is_empty())
        .take(32)
        .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
        .collect::<Vec<_>>();
    if tokens.is_empty() {
        return Err(AppError::bad_request("q must contain searchable words."));
    }
    Ok(tokens.join(" AND "))
}

async fn context(
    State(state): State<AppState>,
    _auth: Auth,
    Path(project): Path<String>,
    Query(query): Query<ContextQuery>,
) -> Reply {
    let limit = query.limit.unwrap_or(20);
    let budget = query.budget.unwrap_or(32768);
    if !(1..=100).contains(&limit) || !(1024..=131072).contains(&budget) {
        return Err(AppError::bad_request(
            "Context limit must be 1–100 and budget must be 1024–131072 bytes.",
        ));
    }
    let fts = fts_query(&query.q)?;
    for (name, value) in [
        ("task_id", query.task_id.as_deref()),
        ("component", query.component.as_deref()),
        ("environment", query.environment.as_deref()),
        ("version", query.version.as_deref()),
    ] {
        if let Some(value) = value {
            bounded(value, name, 255, true)?;
        }
    }
    let mut c = state.pool.begin().await?;
    let policy = sqlx::query("SELECT policy_revision,rules FROM projects WHERE id=?")
        .bind(&project)
        .fetch_optional(&mut *c)
        .await?
        .ok_or_else(AppError::not_found)?;
    let rules = policy.get::<String, _>("rules");
    let mandatory = json!({"revision":policy.get::<i64,_>("policy_revision"),"rules":rules});
    let mandatory_size = serde_json::to_vec(&mandatory)?.len();
    let instructions_complete = mandatory_size <= budget;
    let mut items = Vec::new();
    let mut used = mandatory_size;
    let mut truncated = !instructions_complete;
    let decisions = pending_decision_ids(
        &mut c,
        &project,
        query.task_id.as_deref(),
        state.now(),
        (limit + 1) as usize,
    )
    .await?;
    for id in decisions {
        let item = json!({"type":"decision","record":decision_value(&mut c,&project,&id,state.now(),false).await?});
        let size = serde_json::to_vec(&item)?.len();
        if !instructions_complete || items.len() >= limit as usize || used + size > budget {
            truncated = true;
            break;
        }
        used += size;
        items.push(item);
    }
    if instructions_complete && items.len() < limit as usize {
        // An indexed project filter prevents ranking every project's matching
        // history. User words remain limited to task content, not index metadata.
        let scoped_fts = format!(
            "project_id:\"{}\" AND {{title description acceptance}} : ({fts})",
            project.replace('"', "\"\"")
        );
        // Let FTS5's rank cursor stop after the bounded candidate set. Joining
        // and sorting every matching task first defeats this early termination.
        let task_rows=sqlx::query("WITH hits AS MATERIALIZED (SELECT task_id,rank AS search_rank FROM task_search WHERE task_search MATCH ? AND project_id=? AND rank MATCH 'bm25(0.0,0.0,1.0,1.0,1.0)' ORDER BY rank LIMIT ?) SELECT t.id,t.title,t.description,t.acceptance_json,t.revision,t.lifecycle FROM hits h JOIN tasks t ON t.id=h.task_id AND t.project_id=? ORDER BY h.search_rank,t.id")
            .bind(&scoped_fts).bind(&project).bind(limit+1).bind(&project).fetch_all(&mut *c).await?;
        for row in task_rows {
            let item = json!({"type":"task","record":{"id":row.get::<String,_>("id"),"title":row.get::<String,_>("title"),"description":row.get::<String,_>("description"),"acceptance_criteria":serde_json::from_str::<Value>(&row.get::<String,_>("acceptance_json"))?,"revision":row.get::<i64,_>("revision"),"lifecycle":row.get::<String,_>("lifecycle")}});
            let size = serde_json::to_vec(&item)?.len();
            if items.len() >= limit as usize || used + size > budget {
                truncated = true;
                break;
            }
            used += size;
            items.push(item);
        }
    }
    if instructions_complete && items.len() < limit as usize {
        let rows=sqlx::query("SELECT k.*,bm25(knowledge_search) AS rank FROM knowledge_search JOIN knowledge_records k ON k.id=knowledge_search.record_id WHERE knowledge_search MATCH ? AND (knowledge_search.source_project_id=? OR (? AND knowledge_search.collection='shared')) AND (? IS NULL OR json_array_length(k.scope_json,'$.task_ids')=0 OR EXISTS(SELECT 1 FROM json_each(k.scope_json,'$.task_ids') WHERE value=?)) AND (? IS NULL OR json_array_length(k.scope_json,'$.components')=0 OR EXISTS(SELECT 1 FROM json_each(k.scope_json,'$.components') WHERE lower(value)=lower(?))) AND (? IS NULL OR json_array_length(k.scope_json,'$.environments')=0 OR EXISTS(SELECT 1 FROM json_each(k.scope_json,'$.environments') WHERE lower(value)=lower(?))) AND (? IS NULL OR json_array_length(k.scope_json,'$.versions')=0 OR EXISTS(SELECT 1 FROM json_each(k.scope_json,'$.versions') WHERE lower(value)=lower(?))) ORDER BY rank,k.id LIMIT ?")
            .bind(&fts).bind(&project).bind(query.include_shared)
            .bind(&query.task_id).bind(&query.task_id).bind(&query.component).bind(&query.component)
            .bind(&query.environment).bind(&query.environment).bind(&query.version).bind(&query.version)
            .bind(limit+1).fetch_all(&mut *c).await?;
        for row in rows {
            let item = json!({"type":"knowledge","record":knowledge_row_value(&row)?});
            let size = serde_json::to_vec(&item)?.len();
            if items.len() >= limit as usize || used + size > budget {
                truncated = true;
                break;
            }
            used += size;
            items.push(item);
        }
    }
    // Reaching capacity leaves later record categories unexamined.
    truncated |= items.len() >= limit as usize;
    let next_actions = if !instructions_complete {
        json!([{"action":"increase_context_budget","minimum_bytes":mandatory_size}])
    } else if truncated {
        json!([{"action":"refine_query_or_increase_budget"}])
    } else if items.is_empty() {
        json!([{"action":"broaden_query_or_enable_shared_knowledge"}])
    } else {
        json!([])
    };
    Ok(response(
        json!({"query":query.q,"policy":mandatory,"instructions_complete":instructions_complete,"items":items,"include_shared":query.include_shared,"budget_bytes":budget,"used_bytes":used,"truncated":truncated,"next_actions":next_actions}),
    ))
}

fn validate_decision_tasks(tasks: &[DecisionTaskInput]) -> Result<(), AppError> {
    if tasks.is_empty() || tasks.len() > 100 {
        return Err(AppError::bad_request(
            "A decision must affect 1–100 exact task revisions.",
        ));
    }
    let mut ids = std::collections::BTreeSet::new();
    for task in tasks {
        bounded(&task.task_id, "affected task id", 128, true)?;
        if task.task_revision <= 0 || !ids.insert(&task.task_id) {
            return Err(AppError::bad_request(
                "Affected task IDs must be unique and revisions positive.",
            ));
        }
    }
    Ok(())
}

fn validate_decision(input: &DecisionInput) -> Result<(), AppError> {
    bounded(&input.question, "question", 4096, true)?;
    bounded(&input.rationale, "rationale", 8192, true)?;
    bounded(&input.environment, "environment", 2048, false)?;
    bounded(&input.conditions, "conditions", 4096, false)?;
    if !matches!(input.required_actor.as_str(), "human" | "agent" | "either") {
        return Err(AppError::bad_request(
            "required_actor must be human, agent, or either.",
        ));
    }
    if input.options.len() < 2 || input.options.len() > 20 {
        return Err(AppError::bad_request("Provide 2–20 decision options."));
    }
    let mut options = std::collections::BTreeSet::new();
    for option in &input.options {
        bounded(option, "decision option", 2048, true)?;
        if !options.insert(option) {
            return Err(AppError::bad_request("Decision options must be unique."));
        }
    }
    validate_decision_tasks(&input.affected_tasks)?;
    if input.policy_revision <= 0 {
        return Err(AppError::bad_request(
            "Use a positive current policy_revision.",
        ));
    }
    Ok(())
}

async fn validate_decision_scope(
    c: &mut SqliteConnection,
    project: &str,
    policy_revision: i64,
    tasks: &[DecisionTaskInput],
) -> Result<(), AppError> {
    let current = require_project(c, project).await?;
    if current != policy_revision {
        return Err(AppError::conflict(
            "revision_conflict",
            "Read the current project policy before recording this decision scope.",
        ));
    }
    for task in tasks {
        let revision =
            sqlx::query_scalar::<_, i64>("SELECT revision FROM tasks WHERE project_id=? AND id=?")
                .bind(project)
                .bind(&task.task_id)
                .fetch_optional(&mut *c)
                .await?
                .ok_or_else(|| {
                    AppError::bad_request("Every affected task must belong to the project.")
                })?;
        if revision != task.task_revision {
            return Err(AppError::conflict(
                "revision_conflict",
                "Read each affected task's current revision before recording this decision scope.",
            ));
        }
    }
    let held:i64=sqlx::query_scalar("SELECT count(*) FROM integration_holds h JOIN workflow_activities wa ON wa.id=h.activity_id WHERE h.state='held' AND wa.project_id=? AND EXISTS(SELECT 1 FROM json_each(?) requested JOIN workflow_activities linked ON linked.id=h.activity_id WHERE linked.subject_task_id=json_extract(requested.value,'$.task_id') OR linked.activity_task_id=json_extract(requested.value,'$.task_id'))")
        .bind(project).bind(serde_json::to_string(tasks)?).fetch_one(&mut *c).await?;
    if held > 0 {
        return Err(AppError::conflict(
            "policy_hold_conflict",
            "A decision cannot be opened or rebound after integration publication authority is held. Finish or reconcile that integration first.",
        ));
    }
    Ok(())
}

async fn create_decision(
    State(state): State<AppState>,
    auth: Auth,
    Path(project): Path<String>,
    headers: HeaderMap,
    body: Result<Json<DecisionInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    validate_decision(&input)?;
    let mut m = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{project}/decisions"),
        &input,
    )
    .await?;
    if input.expires_at.is_some_and(|value| value <= m.now) {
        return Err(AppError::bad_request("expires_at must be in the future."));
    }
    validate_decision_scope(
        &mut m.tx,
        &project,
        input.policy_revision,
        &input.affected_tasks,
    )
    .await?;
    if let Some(value) = m.replay {
        return Ok(response(value));
    }
    let id = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO decisions(id,project_id,question,options_json,rationale,required_actor,created_by,created_at) VALUES(?,?,?,?,?,?,?,?)")
        .bind(&id).bind(&project).bind(&input.question).bind(serde_json::to_string(&input.options)?).bind(&input.rationale).bind(&input.required_actor).bind(&m.actor.id).bind(m.now).execute(&mut *m.tx).await?;
    sqlx::query("INSERT INTO decision_cycles(decision_id,generation,policy_revision,environment,conditions,expires_at,rationale,opened_by,created_at) VALUES(?,1,?,?,?,?,?,?,?)")
        .bind(&id).bind(input.policy_revision).bind(&input.environment).bind(&input.conditions).bind(input.expires_at).bind(&input.rationale).bind(&m.actor.id).bind(m.now).execute(&mut *m.tx).await?;
    insert_decision_tasks(&mut m.tx, &id, 1, &project, &input.affected_tasks).await?;
    let value = decision_value(&mut m.tx, &project, &id, m.now, true).await?;
    Ok(response(
        m.finish(value, Some(&project), "decision.created", &id)
            .await?,
    ))
}

async fn insert_decision_tasks(
    c: &mut SqliteConnection,
    id: &str,
    generation: i64,
    project: &str,
    tasks: &[DecisionTaskInput],
) -> Result<(), AppError> {
    for task in tasks {
        sqlx::query("INSERT INTO decision_affected_tasks(decision_id,generation,project_id,task_id,task_revision) VALUES(?,?,?,?,?)")
            .bind(id).bind(generation).bind(project).bind(&task.task_id).bind(task.task_revision).execute(&mut *c).await?;
    }
    Ok(())
}

#[derive(Deserialize)]
struct DecisionPage {
    cursor: Option<String>,
    limit: Option<i64>,
    status: Option<String>,
}

async fn list_decisions(
    State(state): State<AppState>,
    _auth: Auth,
    Path(project): Path<String>,
    Query(page): Query<DecisionPage>,
) -> Reply {
    let limit = page.limit.unwrap_or(50);
    if !(1..=200).contains(&limit) {
        return Err(AppError::bad_request("limit must be between 1 and 200."));
    }
    if page.status.as_deref().is_some_and(|v| {
        !matches!(
            v,
            "pending" | "allowed" | "denied" | "deferred" | "expired" | "stale"
        )
    }) {
        return Err(AppError::bad_request("Invalid decision status filter."));
    }
    let (cursor_time, cursor_id) = if let Some(cursor) = &page.cursor {
        let (a, b) = cursor
            .split_once(':')
            .ok_or_else(|| AppError::bad_request("Invalid decision cursor."))?;
        (
            Some(
                a.parse::<i64>()
                    .map_err(|_| AppError::bad_request("Invalid decision cursor."))?,
            ),
            Some(b.to_owned()),
        )
    } else {
        (None, None)
    };
    let mut c = state.pool.acquire().await?;
    require_project(&mut c, &project).await?;
    let rows=sqlx::query("SELECT id,created_at FROM decisions WHERE project_id=? AND (? IS NULL OR created_at<? OR (created_at=? AND id<?)) ORDER BY created_at DESC,id DESC LIMIT ?")
        .bind(&project).bind(cursor_time).bind(cursor_time).bind(cursor_time).bind(cursor_id).bind(limit+1).fetch_all(&mut *c).await?;
    let more = rows.len() > limit as usize;
    let rows = &rows[..rows.len().min(limit as usize)];
    let mut items = Vec::new();
    for row in rows {
        let id = row.get::<String, _>("id");
        let value = decision_value(&mut c, &project, &id, state.now(), false).await?;
        if page.status.as_deref().is_none_or(|v| value["status"] == v) {
            items.push(value);
        }
    }
    let next = if more {
        rows.last().map(|row| {
            format!(
                "{}:{}",
                row.get::<i64, _>("created_at"),
                row.get::<String, _>("id")
            )
        })
    } else {
        None
    };
    Ok(response(json!({"items":items,"next_cursor":next})))
}

async fn get_decision(
    State(state): State<AppState>,
    _auth: Auth,
    Path((project, id)): Path<(String, String)>,
    Query(page): Query<HistoryPage>,
) -> Reply {
    let limit = page.limit.unwrap_or(50);
    if !(1..=200).contains(&limit) || page.cursor.is_some_and(|value| value <= 0) {
        return Err(AppError::bad_request(
            "History limit must be 1–200 and cursor must be a positive generation.",
        ));
    }
    let mut c = state.pool.acquire().await?;
    let mut value = decision_value(&mut c, &project, &id, state.now(), false).await?;
    let rows=sqlx::query("SELECT c.*,a.disposition,a.answer,a.rationale AS answer_rationale,a.conditions_confirmed,a.actor_id,a.actor_session_id,a.created_at AS answered_at FROM decision_cycles c LEFT JOIN decision_answers a ON a.decision_id=c.decision_id AND a.generation=c.generation WHERE c.decision_id=? AND (? IS NULL OR c.generation<?) ORDER BY c.generation DESC LIMIT ?")
        .bind(&id).bind(page.cursor).bind(page.cursor).bind(limit+1).fetch_all(&mut *c).await?;
    let more = rows.len() > limit as usize;
    let rows = &rows[..rows.len().min(limit as usize)];
    value["history"] = json!(rows.iter().map(decision_history_value).collect::<Vec<_>>());
    value["history_next_cursor"] = if more {
        rows.last()
            .map(|row| row.get::<i64, _>("generation"))
            .into()
    } else {
        Value::Null
    };
    Ok(response(value))
}

fn decision_history_value(row: &sqlx::sqlite::SqliteRow) -> Value {
    json!({"generation":row.get::<i64,_>("generation"),"policy_revision":row.get::<i64,_>("policy_revision"),"environment":row.get::<String,_>("environment"),"conditions":row.get::<String,_>("conditions"),"expires_at":row.get::<Option<i64>,_>("expires_at").map(timestamp),"reopen_rationale":row.get::<String,_>("rationale"),"opened_by":row.get::<String,_>("opened_by"),"created_at":timestamp(row.get("created_at")),"disposition":row.get::<Option<String>,_>("disposition"),"answer":row.get::<Option<String>,_>("answer"),"answer_rationale":row.get::<Option<String>,_>("answer_rationale"),"conditions_confirmed":row.get::<Option<bool>,_>("conditions_confirmed"),"answered_by":row.get::<Option<String>,_>("actor_id"),"answered_at":row.get::<Option<i64>,_>("answered_at").map(timestamp)})
}

async fn decision_value(
    c: &mut SqliteConnection,
    project: &str,
    id: &str,
    now: i64,
    history: bool,
) -> Result<Value, AppError> {
    let row = sqlx::query("SELECT * FROM decisions WHERE project_id=? AND id=?")
        .bind(project)
        .bind(id)
        .fetch_optional(&mut *c)
        .await?
        .ok_or_else(AppError::not_found)?;
    let generation = row.get::<i64, _>("current_generation");
    let cycle = sqlx::query("SELECT * FROM decision_cycles WHERE decision_id=? AND generation=?")
        .bind(id)
        .bind(generation)
        .fetch_one(&mut *c)
        .await?;
    let answer = sqlx::query("SELECT * FROM decision_answers WHERE decision_id=? AND generation=?")
        .bind(id)
        .bind(generation)
        .fetch_optional(&mut *c)
        .await?;
    let current_policy: i64 = sqlx::query_scalar("SELECT policy_revision FROM projects WHERE id=?")
        .bind(project)
        .fetch_one(&mut *c)
        .await?;
    let tasks=sqlx::query("SELECT dt.task_id,dt.task_revision,t.revision AS current_revision FROM decision_affected_tasks dt JOIN tasks t ON t.project_id=dt.project_id AND t.id=dt.task_id WHERE dt.decision_id=? AND dt.generation=? ORDER BY dt.task_id")
        .bind(id).bind(generation).fetch_all(&mut *c).await?;
    let stale = current_policy != cycle.get::<i64, _>("policy_revision")
        || tasks
            .iter()
            .any(|r| r.get::<i64, _>("task_revision") != r.get::<i64, _>("current_revision"));
    let expires = cycle.get::<Option<i64>, _>("expires_at");
    let status = if stale {
        "stale"
    } else if expires.is_some_and(|v| v <= now) {
        "expired"
    } else if let Some(a) = &answer {
        match a.get::<String, _>("disposition").as_str() {
            "allow" if a.get::<bool, _>("conditions_confirmed") => "allowed",
            "deny" => "denied",
            _ => "deferred",
        }
    } else {
        "pending"
    };
    let answer_value=answer.map(|a|json!({"disposition":a.get::<String,_>("disposition"),"answer":a.get::<String,_>("answer"),"rationale":a.get::<String,_>("rationale"),"conditions_confirmed":a.get::<bool,_>("conditions_confirmed"),"actor_id":a.get::<String,_>("actor_id"),"actor_session_id":a.get::<Option<String>,_>("actor_session_id"),"created_at":timestamp(a.get("created_at"))}));
    let mut value = json!({"id":id,"project_id":project,"question":row.get::<String,_>("question"),"options":serde_json::from_str::<Value>(&row.get::<String,_>("options_json"))?,"rationale":row.get::<String,_>("rationale"),"required_actor":row.get::<String,_>("required_actor"),"generation":generation,"status":status,"work_allowed":status=="allowed","policy_revision":cycle.get::<i64,_>("policy_revision"),"environment":cycle.get::<String,_>("environment"),"conditions":cycle.get::<String,_>("conditions"),"expires_at":expires.map(timestamp),"affected_tasks":tasks.iter().map(|r|json!({"task_id":r.get::<String,_>("task_id"),"task_revision":r.get::<i64,_>("task_revision"),"current_revision":r.get::<i64,_>("current_revision")})).collect::<Vec<_>>(),"answer":answer_value,"created_by":row.get::<String,_>("created_by"),"created_at":timestamp(row.get("created_at"))});
    if history {
        let cycles=sqlx::query("SELECT c.*,a.disposition,a.answer,a.rationale AS answer_rationale,a.conditions_confirmed,a.actor_id,a.actor_session_id,a.created_at AS answered_at FROM decision_cycles c LEFT JOIN decision_answers a ON a.decision_id=c.decision_id AND a.generation=c.generation WHERE c.decision_id=? ORDER BY c.generation DESC LIMIT 200").bind(id).fetch_all(&mut *c).await?;
        value["history"] = json!(
            cycles
                .iter()
                .map(decision_history_value)
                .collect::<Vec<_>>()
        );
        value["history_truncated"] = json!(generation > 200);
    }
    Ok(value)
}

async fn answer_decision(
    State(state): State<AppState>,
    auth: Auth,
    Path((project, id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<DecisionAnswerInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    bounded(&input.answer, "answer", 2048, true)?;
    bounded(&input.rationale, "rationale", 8192, true)?;
    if !matches!(input.disposition.as_str(), "allow" | "deny" | "defer") {
        return Err(AppError::bad_request(
            "disposition must be allow, deny, or defer.",
        ));
    }
    if input.disposition == "allow" && !input.conditions_confirmed {
        return Err(AppError::bad_request(
            "An allow answer must explicitly confirm the recorded conditions and environment.",
        ));
    }
    let mut m = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{project}/decisions/{id}/answer"),
        &input,
    )
    .await?;
    let row=sqlx::query("SELECT d.*,c.policy_revision,c.expires_at FROM decisions d JOIN decision_cycles c ON c.decision_id=d.id AND c.generation=d.current_generation WHERE d.project_id=? AND d.id=?")
        .bind(&project).bind(&id).fetch_optional(&mut *m.tx).await?.ok_or_else(AppError::not_found)?;
    let generation = row.get::<i64, _>("current_generation");
    if generation != input.expected_generation {
        return Err(AppError::conflict(
            "generation_conflict",
            "Read the current decision generation before answering.",
        ));
    }
    let required = row.get::<String, _>("required_actor");
    if (required == "human" && m.actor.kind != "human")
        || (required == "agent" && m.actor.kind != "agent")
    {
        return Err(AppError::forbidden(
            "This decision requires the configured actor type.",
        ));
    }
    if m.actor.kind == "agent" && m.actor.session_id.is_none() {
        return Err(AppError::forbidden(
            "Connect an agent session before answering a decision.",
        ));
    }
    ensure_decision_scope_current(
        &mut m.tx,
        &project,
        &id,
        generation,
        row.get("policy_revision"),
        m.now,
        row.get("expires_at"),
    )
    .await?;
    let options: Vec<String> = serde_json::from_str(&row.get::<String, _>("options_json"))?;
    if !options.contains(&input.answer) {
        return Err(AppError::bad_request(
            "The answer must exactly match one of the decision options.",
        ));
    }
    if let Some(value) = m.replay {
        return Ok(response(value));
    }
    sqlx::query("INSERT INTO decision_answers(decision_id,generation,disposition,answer,rationale,actor_id,actor_session_id,conditions_confirmed,created_at) VALUES(?,?,?,?,?,?,?,?,?)")
        .bind(&id).bind(generation).bind(&input.disposition).bind(&input.answer).bind(&input.rationale).bind(&m.actor.id).bind(&m.actor.session_id).bind(input.conditions_confirmed).bind(m.now).execute(&mut *m.tx).await?;
    let value = decision_value(&mut m.tx, &project, &id, m.now, true).await?;
    Ok(response(
        m.finish(value, Some(&project), "decision.answered", &id)
            .await?,
    ))
}

async fn ensure_decision_scope_current(
    c: &mut SqliteConnection,
    project: &str,
    id: &str,
    generation: i64,
    policy_revision: i64,
    now: i64,
    expires_at: Option<i64>,
) -> Result<(), AppError> {
    if expires_at.is_some_and(|v| v <= now) {
        return Err(AppError::conflict(
            "decision_expired",
            "Reopen the expired decision against current task and policy revisions.",
        ));
    }
    if require_project(c, project).await? != policy_revision {
        return Err(AppError::conflict(
            "decision_scope_stale",
            "Project policy changed; reopen the decision with the current scope.",
        ));
    }
    let stale:i64=sqlx::query_scalar("SELECT count(*) FROM decision_affected_tasks dt JOIN tasks t ON t.project_id=dt.project_id AND t.id=dt.task_id WHERE dt.decision_id=? AND dt.generation=? AND (dt.project_id!=? OR dt.task_revision!=t.revision)")
        .bind(id).bind(generation).bind(project).fetch_one(&mut *c).await?;
    if stale > 0 {
        return Err(AppError::conflict(
            "decision_scope_stale",
            "An affected task changed; reopen the decision with exact current revisions.",
        ));
    }
    Ok(())
}

async fn reopen_decision(
    State(state): State<AppState>,
    auth: Auth,
    Path((project, id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<DecisionReopenInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    bounded(&input.rationale, "rationale", 8192, true)?;
    bounded(&input.environment, "environment", 2048, false)?;
    bounded(&input.conditions, "conditions", 4096, false)?;
    validate_decision_tasks(&input.affected_tasks)?;
    if input.policy_revision <= 0 {
        return Err(AppError::bad_request(
            "Use a positive current policy_revision.",
        ));
    }
    let mut m = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{project}/decisions/{id}/reopen"),
        &input,
    )
    .await?;
    if input.expires_at.is_some_and(|value| value <= m.now) {
        return Err(AppError::bad_request("expires_at must be in the future."));
    }
    let generation = sqlx::query_scalar::<_, i64>(
        "SELECT current_generation FROM decisions WHERE project_id=? AND id=?",
    )
    .bind(&project)
    .bind(&id)
    .fetch_optional(&mut *m.tx)
    .await?
    .ok_or_else(AppError::not_found)?;
    if generation != input.expected_generation {
        return Err(AppError::conflict(
            "generation_conflict",
            "Read the current decision generation before reopening.",
        ));
    }
    validate_decision_scope(
        &mut m.tx,
        &project,
        input.policy_revision,
        &input.affected_tasks,
    )
    .await?;
    if let Some(value) = m.replay {
        return Ok(response(value));
    }
    let next = generation + 1;
    sqlx::query("INSERT INTO decision_cycles(decision_id,generation,policy_revision,environment,conditions,expires_at,rationale,opened_by,created_at) VALUES(?,?,?,?,?,?,?,?,?)")
        .bind(&id).bind(next).bind(input.policy_revision).bind(&input.environment).bind(&input.conditions).bind(input.expires_at).bind(&input.rationale).bind(&m.actor.id).bind(m.now).execute(&mut *m.tx).await?;
    insert_decision_tasks(&mut m.tx, &id, next, &project, &input.affected_tasks).await?;
    sqlx::query("UPDATE decisions SET current_generation=? WHERE id=?")
        .bind(next)
        .bind(&id)
        .execute(&mut *m.tx)
        .await?;
    let value = decision_value(&mut m.tx, &project, &id, m.now, true).await?;
    Ok(response(
        m.finish(value, Some(&project), "decision.reopened", &id)
            .await?,
    ))
}

/// Return blocking decision IDs in stable order. A prior allow stops blocking
/// only while its complete task/policy scope and expiry remain current.
pub async fn pending_decision_ids(
    c: &mut SqliteConnection,
    project: &str,
    task: Option<&str>,
    now: i64,
    limit: usize,
) -> Result<Vec<String>, AppError> {
    let bounded_limit = limit.clamp(1, 200) as i64;
    let rows=sqlx::query_scalar::<_,String>("SELECT d.id FROM decisions d JOIN decision_cycles c ON c.decision_id=d.id AND c.generation=d.current_generation JOIN projects p ON p.id=d.project_id LEFT JOIN decision_answers a ON a.decision_id=d.id AND a.generation=d.current_generation WHERE d.project_id=? AND (? IS NULL OR EXISTS(SELECT 1 FROM decision_affected_tasks target WHERE target.decision_id=d.id AND target.generation=d.current_generation AND target.task_id=?)) AND (c.policy_revision!=p.policy_revision OR EXISTS(SELECT 1 FROM decision_affected_tasks scoped JOIN tasks current ON current.project_id=scoped.project_id AND current.id=scoped.task_id WHERE scoped.decision_id=d.id AND scoped.generation=d.current_generation AND scoped.task_revision!=current.revision) OR a.decision_id IS NULL OR a.disposition!='allow' OR a.conditions_confirmed=0 OR (c.expires_at IS NOT NULL AND c.expires_at<=?)) ORDER BY d.created_at,d.id LIMIT ?")
        .bind(project).bind(task).bind(task).bind(now).bind(bounded_limit).fetch_all(c).await?;
    Ok(rows)
}

/// Guard ownership-dependent work after the caller has acquired the SQLite
/// writer lock and revalidated its authority.
pub async fn ensure_decisions_resolved(
    c: &mut SqliteConnection,
    project: &str,
    task: &str,
    now: i64,
) -> Result<(), AppError> {
    let ids = pending_decision_ids(c, project, Some(task), now, 21).await?;
    if ids.is_empty() {
        return Ok(());
    }
    let truncated = ids.len() > 20;
    let shown = ids.into_iter().take(20).collect::<Vec<_>>();
    Err(AppError::conflict(
        "decision_required",
        "Resolve or reopen the current scoped decision before continuing ownership-dependent work.",
    )
    .with_details(json!({"decision_ids":shown,"truncated":truncated})))
}

/// Insert submission lessons inside the submission's already-open mutation.
/// Validation completes before inserts, and every record is tied to the exact
/// source task/submission in its immutable provenance.
pub async fn insert_submission_lessons(
    c: &mut SqliteConnection,
    actor: &Actor,
    now: i64,
    project: &str,
    task: &str,
    submission: &str,
    lessons: &[SubmissionLessonInput],
) -> Result<Vec<Value>, AppError> {
    if lessons.len() > 50 {
        return Err(AppError::bad_request(
            "A submission may include at most 50 lessons.",
        ));
    }
    let exists: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM submissions WHERE project_id=? AND task_id=? AND id=?",
    )
    .bind(project)
    .bind(task)
    .bind(submission)
    .fetch_one(&mut *c)
    .await?;
    if exists != 1 {
        return Err(AppError::bad_request(
            "The lesson source submission must match the exact project and task.",
        ));
    }
    let mut inputs = Vec::with_capacity(lessons.len());
    for lesson in lessons {
        let provenance = KnowledgeProvenance {
            summary: if lesson.provenance_summary.trim().is_empty() {
                "Created with the immutable task submission.".into()
            } else {
                lesson.provenance_summary.clone()
            },
            source_uri: lesson.source_uri.clone(),
            source_task_id: Some(task.into()),
            source_submission_id: Some(submission.into()),
        };
        let input = KnowledgeInput {
            kind: lesson.kind.clone(),
            title: lesson.title.clone(),
            body: lesson.body.clone(),
            status: lesson.status.clone(),
            scope: lesson.scope.clone(),
            tags: lesson.tags.clone(),
            applicability: lesson.applicability.clone(),
            provenance,
            collection: lesson.collection.clone(),
            share_across_projects: lesson.share_across_projects,
        };
        validate_knowledge(&input)?;
        validate_knowledge_references(c, project, &input.scope, &input.provenance).await?;
        inputs.push(input);
    }
    let mut values = Vec::with_capacity(inputs.len());
    for input in &inputs {
        let value = insert_knowledge_record(c, actor, now, project, input).await?;
        let id = value["id"].as_str().unwrap_or_default();
        sqlx::query("INSERT INTO events(project_id,actor_id,kind,record_id,data_json,created_at) VALUES(?,?,'knowledge.created_from_submission',?,'{}',?)")
            .bind(project).bind(&actor.id).bind(id).bind(now).execute(&mut *c).await?;
        values.push(value);
    }
    Ok(values)
}
