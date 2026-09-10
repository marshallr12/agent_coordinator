//! Cursor-bound, full-fidelity task history pages.

use crate::{auth::Auth, error::AppError, response, state::AppState};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
};
use coordinator_core::{TaskHistoryItem, TaskHistoryPage, timestamp};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection, sqlite::SqliteRow};

type Reply = Result<Json<Value>, AppError>;
const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 200;
const MAX_CURSOR_BYTES: usize = 2048;
const MAX_HISTORY_DATA_BYTES: usize = 256 * 1024 - 512;

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/v1/projects/{project}/tasks/{task}/history",
        get(history),
    )
}

#[derive(Deserialize)]
struct HistoryQuery {
    kind: String,
    limit: Option<i64>,
    cursor: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct HistoryCursor {
    version: u8,
    project_id: String,
    task_id: String,
    kind: String,
    cutoff: i64,
    last: i64,
}

fn valid_kind(kind: &str) -> bool {
    matches!(
        kind,
        "attempts"
            | "checkpoints"
            | "checkouts"
            | "jobs"
            | "job_observations"
            | "resources"
            | "artifacts"
            | "submissions"
            | "reviews"
            | "integrations"
            | "task_revisions"
            | "events"
    )
}

async fn insertion_cutoff(c: &mut SqliteConnection, kind: &str) -> Result<i64, AppError> {
    let value = match kind {
        "attempts" => {
            sqlx::query_scalar("SELECT COALESCE(max(rowid),0) FROM attempts")
                .fetch_one(c)
                .await?
        }
        "checkpoints" => {
            sqlx::query_scalar("SELECT COALESCE(max(rowid),0) FROM checkpoints")
                .fetch_one(c)
                .await?
        }
        "checkouts" => {
            sqlx::query_scalar("SELECT COALESCE(max(rowid),0) FROM checkouts")
                .fetch_one(c)
                .await?
        }
        "jobs" => {
            sqlx::query_scalar("SELECT COALESCE(max(rowid),0) FROM jobs")
                .fetch_one(c)
                .await?
        }
        "job_observations" => {
            sqlx::query_scalar("SELECT COALESCE(max(rowid),0) FROM job_observations")
                .fetch_one(c)
                .await?
        }
        "resources" => {
            sqlx::query_scalar("SELECT COALESCE(max(rowid),0) FROM reservations")
                .fetch_one(c)
                .await?
        }
        "artifacts" => {
            sqlx::query_scalar("SELECT COALESCE(max(rowid),0) FROM artifacts")
                .fetch_one(c)
                .await?
        }
        "submissions" => {
            sqlx::query_scalar("SELECT COALESCE(max(rowid),0) FROM submissions")
                .fetch_one(c)
                .await?
        }
        "reviews" | "integrations" => {
            sqlx::query_scalar("SELECT COALESCE(max(rowid),0) FROM workflow_activities")
                .fetch_one(c)
                .await?
        }
        "task_revisions" => {
            sqlx::query_scalar("SELECT COALESCE(max(rowid),0) FROM task_revisions")
                .fetch_one(c)
                .await?
        }
        "events" => {
            sqlx::query_scalar("SELECT COALESCE(max(rowid),0) FROM events")
                .fetch_one(c)
                .await?
        }
        _ => unreachable!("kind validated before cutoff"),
    };
    Ok(value)
}

fn decode_cursor(value: &str) -> Result<HistoryCursor, AppError> {
    if value.is_empty() || value.len() > MAX_CURSOR_BYTES || !value.is_ascii() {
        return Err(AppError::bad_request("The history cursor is invalid."));
    }
    let bytes =
        hex::decode(value).map_err(|_| AppError::bad_request("The history cursor is invalid."))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| AppError::bad_request("The history cursor is invalid."))
}

fn encode_cursor(cursor: &HistoryCursor) -> Result<String, AppError> {
    Ok(hex::encode(serde_json::to_vec(cursor)?))
}

fn snapshot(project: &str, task: &str, kind: &str, cutoff: i64) -> String {
    let mut digest = Sha256::new();
    for part in [project, task, kind, &cutoff.to_string()] {
        digest.update(part.as_bytes());
        digest.update([0]);
    }
    hex::encode(digest.finalize())
}

async fn history(
    State(state): State<AppState>,
    _auth: Auth,
    Path((project, task)): Path<(String, String)>,
    Query(query): Query<HistoryQuery>,
) -> Reply {
    if !valid_kind(&query.kind) {
        return Err(AppError::bad_request(
            "The requested task history kind is not supported.",
        ));
    }
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(AppError::bad_request("limit must be between 1 and 200."));
    }
    let mut tx = state.pool.begin().await?;
    let exists: i64 = sqlx::query_scalar("SELECT count(*) FROM tasks WHERE project_id=? AND id=?")
        .bind(&project)
        .bind(&task)
        .fetch_one(&mut *tx)
        .await?;
    if exists == 0 {
        return Err(AppError::not_found());
    }
    let subject: String = sqlx::query_scalar(
        "SELECT subject_task_id FROM workflow_activities WHERE project_id=? AND activity_task_id=?",
    )
    .bind(&project)
    .bind(&task)
    .fetch_optional(&mut *tx)
    .await?
    .unwrap_or_else(|| task.clone());

    let (cutoff, last) = if let Some(value) = query.cursor.as_deref() {
        let cursor = decode_cursor(value)?;
        if cursor.version != 1
            || cursor.project_id != project
            || cursor.task_id != task
            || cursor.kind != query.kind
            || cursor.cutoff < 0
            || cursor.last < 0
            || cursor.last > cursor.cutoff
        {
            return Err(AppError::conflict(
                "history_cursor_mismatch",
                "The history cursor belongs to another project, task, kind, or snapshot.",
            ));
        }
        (cursor.cutoff, cursor.last)
    } else {
        let cutoff = insertion_cutoff(&mut tx, &query.kind).await?;
        (cutoff, 0)
    };
    let rows = fetch_rows(
        &mut tx,
        &project,
        &subject,
        &query.kind,
        last,
        cutoff,
        limit + 1,
    )
    .await?;
    let had_more = rows.len() > limit as usize;
    let rows = &rows[..rows.len().min(limit as usize)];
    let mut items = materialize(&mut tx, &query.kind, rows).await?;
    let page_snapshot = snapshot(&project, &task, &query.kind, cutoff);
    let mut more = had_more;
    loop {
        let next_cursor = if more {
            items
                .last()
                .map(|(cursor_id, _)| {
                    encode_cursor(&HistoryCursor {
                        version: 1,
                        project_id: project.clone(),
                        task_id: task.clone(),
                        kind: query.kind.clone(),
                        cutoff,
                        last: *cursor_id,
                    })
                })
                .transpose()?
        } else {
            None
        };
        let page = TaskHistoryPage {
            project_id: project.clone(),
            task_id: task.clone(),
            subject_task_id: subject.clone(),
            kind: query.kind.clone(),
            snapshot: page_snapshot.clone(),
            items: items.iter().map(|(_, item)| item.clone()).collect(),
            next_cursor,
        };
        let value = serde_json::to_value(page)?;
        if serde_json::to_vec(&json!({"data":&value}))?.len() <= MAX_HISTORY_DATA_BYTES {
            return Ok(response(value));
        }
        if items.len() <= 1 {
            return Err(AppError::conflict(
                "history_record_too_large",
                "One complete history record exceeds the 256 KiB page budget; no evidence was truncated.",
            ));
        }
        items.pop();
        more = true;
    }
}

async fn fetch_rows(
    c: &mut SqliteConnection,
    project: &str,
    subject: &str,
    kind: &str,
    last: i64,
    cutoff: i64,
    take: i64,
) -> Result<Vec<SqliteRow>, AppError> {
    let rows = match kind {
        "attempts" => sqlx::query("SELECT a.rowid cursor_id,CASE WHEN a.task_id=? THEN 'subject' ELSE 'workflow_activity' END relation,a.* FROM attempts a WHERE a.project_id=? AND (a.task_id=? OR EXISTS(SELECT 1 FROM workflow_activities scope WHERE scope.project_id=? AND scope.subject_task_id=? AND scope.activity_task_id=a.task_id)) AND a.rowid>? AND a.rowid<=? ORDER BY a.rowid LIMIT ?")
            .bind(subject).bind(project).bind(subject).bind(project).bind(subject).bind(last).bind(cutoff).bind(take).fetch_all(c).await?,
        "checkpoints" => sqlx::query("SELECT cp.rowid cursor_id,CASE WHEN a.task_id=? THEN 'subject' ELSE 'workflow_activity' END relation,a.task_id related_task_id,cp.* FROM checkpoints cp JOIN attempts a ON a.project_id=cp.project_id AND a.id=cp.attempt_id WHERE cp.project_id=? AND (a.task_id=? OR EXISTS(SELECT 1 FROM workflow_activities scope WHERE scope.project_id=? AND scope.subject_task_id=? AND scope.activity_task_id=a.task_id)) AND cp.rowid>? AND cp.rowid<=? ORDER BY cp.rowid LIMIT ?")
            .bind(subject).bind(project).bind(subject).bind(project).bind(subject).bind(last).bind(cutoff).bind(take).fetch_all(c).await?,
        "checkouts" => sqlx::query("SELECT ch.rowid cursor_id,CASE WHEN a.task_id=? THEN 'subject' ELSE 'workflow_activity' END relation,a.task_id related_task_id,ch.* FROM checkouts ch JOIN attempts a ON a.project_id=ch.project_id AND a.id=ch.attempt_id WHERE ch.project_id=? AND (a.task_id=? OR EXISTS(SELECT 1 FROM workflow_activities scope WHERE scope.project_id=? AND scope.subject_task_id=? AND scope.activity_task_id=a.task_id)) AND ch.rowid>? AND ch.rowid<=? ORDER BY ch.rowid LIMIT ?")
            .bind(subject).bind(project).bind(subject).bind(project).bind(subject).bind(last).bind(cutoff).bind(take).fetch_all(c).await?,
        "jobs" => sqlx::query("SELECT j.rowid cursor_id,CASE WHEN j.task_id=? THEN 'subject' ELSE 'workflow_activity' END relation,j.task_id related_task_id,j.* FROM jobs j WHERE j.project_id=? AND (j.task_id=? OR EXISTS(SELECT 1 FROM workflow_activities scope WHERE scope.project_id=? AND scope.subject_task_id=? AND scope.activity_task_id=j.task_id)) AND j.rowid>? AND j.rowid<=? ORDER BY j.rowid LIMIT ?")
            .bind(subject).bind(project).bind(subject).bind(project).bind(subject).bind(last).bind(cutoff).bind(take).fetch_all(c).await?,
        "job_observations" => sqlx::query("SELECT o.rowid cursor_id,CASE WHEN j.task_id=? THEN 'subject' ELSE 'workflow_activity' END relation,j.task_id related_task_id,j.id job_id,o.* FROM job_observations o JOIN reporters reporter ON reporter.id=o.reporter_id JOIN jobs j ON j.id=reporter.job_id WHERE j.project_id=? AND (j.task_id=? OR EXISTS(SELECT 1 FROM workflow_activities scope WHERE scope.project_id=? AND scope.subject_task_id=? AND scope.activity_task_id=j.task_id)) AND o.rowid>? AND o.rowid<=? ORDER BY o.rowid LIMIT ?")
            .bind(subject).bind(project).bind(subject).bind(project).bind(subject).bind(last).bind(cutoff).bind(take).fetch_all(c).await?,
        "resources" => sqlx::query("SELECT r.rowid cursor_id,CASE WHEN a.task_id=? THEN 'subject' ELSE 'workflow_activity' END relation,a.task_id related_task_id,r.* FROM reservations r JOIN attempts a ON a.project_id=r.project_id AND a.id=r.attempt_id WHERE r.project_id=? AND (a.task_id=? OR EXISTS(SELECT 1 FROM workflow_activities scope WHERE scope.project_id=? AND scope.subject_task_id=? AND scope.activity_task_id=a.task_id)) AND r.rowid>? AND r.rowid<=? ORDER BY r.rowid LIMIT ?")
            .bind(subject).bind(project).bind(subject).bind(project).bind(subject).bind(last).bind(cutoff).bind(take).fetch_all(c).await?,
        "artifacts" => sqlx::query("SELECT ar.rowid cursor_id,CASE WHEN ar.task_id=? THEN 'subject' WHEN ar.task_id IS NOT NULL THEN 'workflow_activity' ELSE 'subject_evidence' END relation,COALESCE(ar.task_id,(SELECT j.task_id FROM jobs j WHERE j.id=ar.job_id),?) related_task_id,ar.* FROM artifacts ar WHERE ar.project_id=? AND (ar.task_id=? OR EXISTS(SELECT 1 FROM workflow_activities scope WHERE scope.project_id=? AND scope.subject_task_id=? AND scope.activity_task_id=ar.task_id) OR EXISTS(SELECT 1 FROM jobs j WHERE j.project_id=? AND j.id=ar.job_id AND (j.task_id=? OR EXISTS(SELECT 1 FROM workflow_activities scope WHERE scope.project_id=? AND scope.subject_task_id=? AND scope.activity_task_id=j.task_id))) OR EXISTS(SELECT 1 FROM submission_artifacts sa JOIN submissions s ON s.project_id=sa.project_id AND s.id=sa.submission_id WHERE sa.project_id=? AND sa.artifact_id=ar.id AND s.task_id=?)) AND ar.rowid>? AND ar.rowid<=? ORDER BY ar.rowid LIMIT ?")
            .bind(subject).bind(subject).bind(project).bind(subject).bind(project).bind(subject).bind(project).bind(subject).bind(project).bind(subject).bind(project).bind(subject).bind(last).bind(cutoff).bind(take).fetch_all(c).await?,
        "submissions" => sqlx::query("SELECT s.rowid cursor_id,'subject' relation,s.task_id related_task_id,s.* FROM submissions s WHERE s.project_id=? AND s.task_id=? AND s.rowid>? AND s.rowid<=? ORDER BY s.rowid LIMIT ?")
            .bind(project).bind(subject).bind(last).bind(cutoff).bind(take).fetch_all(c).await?,
        "reviews" => sqlx::query("SELECT wa.rowid cursor_id,'workflow_activity' relation,wa.activity_task_id related_task_id,wa.* FROM workflow_activities wa WHERE wa.project_id=? AND wa.subject_task_id=? AND wa.kind IN ('agent_review','human_review') AND wa.rowid>? AND wa.rowid<=? ORDER BY wa.rowid LIMIT ?")
            .bind(project).bind(subject).bind(last).bind(cutoff).bind(take).fetch_all(c).await?,
        "integrations" => sqlx::query("SELECT wa.rowid cursor_id,'workflow_activity' relation,wa.activity_task_id related_task_id,wa.* FROM workflow_activities wa WHERE wa.project_id=? AND wa.subject_task_id=? AND wa.kind='integration' AND wa.rowid>? AND wa.rowid<=? ORDER BY wa.rowid LIMIT ?")
            .bind(project).bind(subject).bind(last).bind(cutoff).bind(take).fetch_all(c).await?,
        "task_revisions" => sqlx::query("SELECT tr.rowid cursor_id,CASE WHEN tr.task_id=? THEN 'subject' ELSE 'workflow_activity' END relation,tr.task_id related_task_id,tr.* FROM task_revisions tr WHERE tr.project_id=? AND (tr.task_id=? OR EXISTS(SELECT 1 FROM workflow_activities scope WHERE scope.project_id=? AND scope.subject_task_id=? AND scope.activity_task_id=tr.task_id)) AND tr.rowid>? AND tr.rowid<=? ORDER BY tr.rowid LIMIT ?")
            .bind(subject).bind(project).bind(subject).bind(project).bind(subject).bind(last).bind(cutoff).bind(take).fetch_all(c).await?,
        "events" => sqlx::query("SELECT e.rowid cursor_id,'task_graph_event' relation,? related_task_id,e.* FROM events e WHERE e.project_id=? AND (e.record_id=? OR EXISTS(SELECT 1 FROM workflow_activities wa WHERE wa.project_id=? AND wa.subject_task_id=? AND (wa.id=e.record_id OR wa.activity_task_id=e.record_id OR wa.submission_id=e.record_id)) OR EXISTS(SELECT 1 FROM attempts a WHERE a.project_id=? AND a.id=e.record_id AND (a.task_id=? OR EXISTS(SELECT 1 FROM workflow_activities wa WHERE wa.project_id=? AND wa.subject_task_id=? AND wa.activity_task_id=a.task_id))) OR EXISTS(SELECT 1 FROM checkpoints cp JOIN attempts a ON a.id=cp.attempt_id WHERE cp.project_id=? AND cp.id=e.record_id AND (a.task_id=? OR EXISTS(SELECT 1 FROM workflow_activities wa WHERE wa.project_id=? AND wa.subject_task_id=? AND wa.activity_task_id=a.task_id))) OR EXISTS(SELECT 1 FROM jobs j WHERE j.project_id=? AND j.id=e.record_id AND (j.task_id=? OR EXISTS(SELECT 1 FROM workflow_activities wa WHERE wa.project_id=? AND wa.subject_task_id=? AND wa.activity_task_id=j.task_id))) OR EXISTS(SELECT 1 FROM reservations r JOIN attempts a ON a.id=r.attempt_id WHERE r.project_id=? AND r.id=e.record_id AND (a.task_id=? OR EXISTS(SELECT 1 FROM workflow_activities wa WHERE wa.project_id=? AND wa.subject_task_id=? AND wa.activity_task_id=a.task_id))) OR EXISTS(SELECT 1 FROM submissions s WHERE s.project_id=? AND s.id=e.record_id AND s.task_id=?)) AND e.rowid>? AND e.rowid<=? ORDER BY e.rowid LIMIT ?")
            .bind(subject).bind(project).bind(subject)
            .bind(project).bind(subject)
            .bind(project).bind(subject).bind(project).bind(subject)
            .bind(project).bind(subject).bind(project).bind(subject)
            .bind(project).bind(subject).bind(project).bind(subject)
            .bind(project).bind(subject).bind(project).bind(subject)
            .bind(project).bind(subject)
            .bind(last).bind(cutoff).bind(take).fetch_all(c).await?,
        _ => unreachable!("kind validated before query"),
    };
    Ok(rows)
}

fn relation(row: &SqliteRow) -> String {
    row.get("relation")
}

fn related_task(row: &SqliteRow) -> String {
    row.try_get("related_task_id")
        .unwrap_or_else(|_| row.get("task_id"))
}

fn time(value: Option<i64>) -> Option<String> {
    value.map(timestamp)
}

async fn materialize(
    c: &mut SqliteConnection,
    kind: &str,
    rows: &[SqliteRow],
) -> Result<Vec<(i64, TaskHistoryItem)>, AppError> {
    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        let cursor_id = row.get("cursor_id");
        let (task_id, occurred_at, record) = match kind {
            "attempts" => (
                row.get("task_id"),
                time(Some(row.get("created_at"))),
                json!({"id":row.get::<String,_>("id"),"owner_id":row.get::<String,_>("owner_id"),"generation":row.get::<i64,_>("generation"),"state":row.get::<String,_>("state"),"mode":row.get::<String,_>("mode"),"expires_at":timestamp(row.get("expires_at")),"last_heartbeat_at":timestamp(row.get("last_heartbeat_at")),"last_progress_at":timestamp(row.get("last_progress_at")),"created_at":timestamp(row.get("created_at")),"ended_at":time(row.get("ended_at")),"outcome":row.get::<Option<String>,_>("outcome"),"task_revision":row.get::<Option<i64>,_>("task_revision"),"policy_revision":row.get::<Option<i64>,_>("policy_revision")}),
            ),
            "checkpoints" => (
                related_task(row),
                time(Some(row.get("created_at"))),
                json!({"id":row.get::<String,_>("id"),"attempt_id":row.get::<String,_>("attempt_id"),"summary":row.get::<String,_>("summary"),"current_action":row.get::<String,_>("current_action"),"next_step":row.get::<String,_>("next_step"),"blockers":serde_json::from_str::<Value>(&row.get::<String,_>("blockers_json"))?,"created_at":timestamp(row.get("created_at"))}),
            ),
            "checkouts" => (
                related_task(row),
                time(Some(row.get("created_at"))),
                json!({"attempt_id":row.get::<String,_>("attempt_id"),"workstation_id":row.get::<String,_>("workstation_id"),"identity":row.get::<String,_>("identity"),"path":row.get::<String,_>("path"),"branch":row.get::<String,_>("branch"),"base_revision":row.get::<String,_>("base_revision"),"created_at":timestamp(row.get("created_at"))}),
            ),
            "jobs" => (
                related_task(row),
                time(Some(row.get("created_at"))),
                job_value(row),
            ),
            "job_observations" => (
                related_task(row),
                time(Some(row.get("observed_at"))),
                json!({"job_id":row.get::<String,_>("job_id"),"sequence":row.get::<i64,_>("sequence"),"producer_id":row.get::<String,_>("producer_id"),"state":row.get::<String,_>("state"),"pid":row.get::<Option<i64>,_>("pid"),"process_started_at":row.get::<Option<String>,_>("process_started_at"),"exit_code":row.get::<Option<i64>,_>("exit_code"),"inputs_unchanged":row.get::<Option<bool>,_>("inputs_unchanged"),"summary":row.get::<String,_>("summary"),"observed_at":timestamp(row.get("observed_at"))}),
            ),
            "resources" => {
                let id: String = row.get("id");
                let items = sqlx::query("SELECT ri.resource_id,ri.units,r.key,r.capacity,r.description FROM reservation_items ri JOIN resources r ON r.id=ri.resource_id WHERE ri.reservation_id=? ORDER BY r.key,r.id")
                    .bind(&id).fetch_all(&mut *c).await?;
                let items: Vec<Value> = items.iter().map(|item|json!({"resource_id":item.get::<String,_>("resource_id"),"key":item.get::<String,_>("key"),"units":item.get::<i64,_>("units"),"capacity":item.get::<i64,_>("capacity"),"description":item.get::<String,_>("description")})).collect();
                (
                    related_task(row),
                    time(Some(row.get("created_at"))),
                    json!({"id":id,"attempt_id":row.get::<String,_>("attempt_id"),"generation":row.get::<i64,_>("generation"),"state":row.get::<String,_>("state"),"created_by":row.get::<String,_>("created_by"),"created_at":timestamp(row.get("created_at")),"released_at":time(row.get("released_at")),"released_by":row.get::<Option<String>,_>("released_by"),"release_reason":row.get::<Option<String>,_>("release_reason"),"resolved_at":time(row.get("resolved_at")),"resolved_by":row.get::<Option<String>,_>("resolved_by"),"resolution_reason":row.get::<Option<String>,_>("resolution_reason"),"resolution_evidence":row.get::<Option<String>,_>("resolution_evidence"),"items":items}),
                )
            }
            "artifacts" => (
                related_task(row),
                time(Some(row.get("created_at"))),
                artifact_value(row),
            ),
            "submissions" => {
                let id: String = row.get("id");
                let lessons=sqlx::query("SELECT kr.knowledge_id,kr.revision,kr.kind,kr.status,kr.title,kr.body,kr.scope_json,kr.tags_json,kr.applicability,kr.provenance_json,kr.created_at FROM submission_knowledge sk JOIN knowledge_revisions kr ON kr.knowledge_id=sk.knowledge_id AND kr.revision=sk.knowledge_revision WHERE sk.project_id=? AND sk.submission_id=? ORDER BY kr.knowledge_id")
                    .bind(row.get::<String,_>("project_id")).bind(&id).fetch_all(&mut *c).await?;
                let lessons:Vec<Value>=lessons.iter().map(|lesson| -> Result<Value,serde_json::Error>{Ok(json!({"id":lesson.get::<String,_>("knowledge_id"),"revision":lesson.get::<i64,_>("revision"),"kind":lesson.get::<String,_>("kind"),"status":lesson.get::<String,_>("status"),"title":lesson.get::<String,_>("title"),"body":lesson.get::<String,_>("body"),"scope":serde_json::from_str::<Value>(&lesson.get::<String,_>("scope_json"))?,"tags":serde_json::from_str::<Value>(&lesson.get::<String,_>("tags_json"))?,"applicability":lesson.get::<String,_>("applicability"),"provenance":serde_json::from_str::<Value>(&lesson.get::<String,_>("provenance_json"))?,"created_at":timestamp(lesson.get("created_at"))}))}).collect::<Result<_,_>>()?;
                let artifact_ids=sqlx::query_scalar::<_,String>("SELECT artifact_id FROM submission_artifacts WHERE project_id=? AND submission_id=? ORDER BY artifact_id").bind(row.get::<String,_>("project_id")).bind(&id).fetch_all(&mut *c).await?;
                (
                    related_task(row),
                    time(Some(row.get("created_at"))),
                    submission_value(row, lessons, artifact_ids)?,
                )
            }
            "reviews" => {
                let activity: String = row.get("id");
                let decision=sqlx::query("SELECT attempt_id,reviewer_id,decision,summary,created_at FROM review_decisions WHERE activity_id=?").bind(&activity).fetch_optional(&mut *c).await?;
                let decision=decision.map(|value|json!({"attempt_id":value.get::<String,_>("attempt_id"),"reviewer_id":value.get::<String,_>("reviewer_id"),"decision":value.get::<String,_>("decision"),"summary":value.get::<String,_>("summary"),"created_at":timestamp(value.get("created_at"))}));
                let findings=sqlx::query("SELECT id,severity,remedy,evidence,created_at FROM review_findings WHERE activity_id=? ORDER BY created_at,id").bind(&activity).fetch_all(&mut *c).await?;
                let findings:Vec<Value>=findings.iter().map(|value|json!({"id":value.get::<String,_>("id"),"severity":value.get::<String,_>("severity"),"remedy":value.get::<String,_>("remedy"),"evidence":value.get::<String,_>("evidence"),"created_at":timestamp(value.get("created_at"))})).collect();
                (
                    related_task(row),
                    time(Some(row.get("created_at"))),
                    json!({"activity":activity_value(row),"decision":decision,"findings":findings}),
                )
            }
            "integrations" => {
                let activity: String = row.get("id");
                let authorization=sqlx::query("SELECT submission_id,project_policy_revision,workflow_policy_revision,actor_id,summary,created_at FROM integration_authorizations WHERE activity_id=?").bind(&activity).fetch_optional(&mut *c).await?.map(|v|json!({"submission_id":v.get::<String,_>("submission_id"),"project_policy_revision":v.get::<i64,_>("project_policy_revision"),"workflow_policy_revision":v.get::<i64,_>("workflow_policy_revision"),"actor_id":v.get::<String,_>("actor_id"),"summary":v.get::<String,_>("summary"),"created_at":timestamp(v.get("created_at"))}));
                let hold=sqlx::query("SELECT id,canonical_repository_key,target_branch,state,acquired_by,acquired_at,released_by,released_at,release_reason FROM integration_holds WHERE activity_id=?").bind(&activity).fetch_optional(&mut *c).await?.map(|v|json!({"id":v.get::<String,_>("id"),"canonical_repository_key":v.get::<String,_>("canonical_repository_key"),"target_branch":v.get::<String,_>("target_branch"),"state":v.get::<String,_>("state"),"acquired_by":v.get::<String,_>("acquired_by"),"acquired_at":timestamp(v.get("acquired_at")),"released_by":v.get::<Option<String>,_>("released_by"),"released_at":time(v.get("released_at")),"release_reason":v.get::<Option<String>,_>("release_reason")}));
                let intent=sqlx::query("SELECT submission_id,attempt_id,observed_target_revision,observed_target_tree,result_revision,result_tree,created_by,created_at FROM publication_intents WHERE activity_id=?").bind(&activity).fetch_optional(&mut *c).await?.map(|v|json!({"submission_id":v.get::<String,_>("submission_id"),"attempt_id":v.get::<String,_>("attempt_id"),"observed_target_revision":v.get::<String,_>("observed_target_revision"),"observed_target_tree":v.get::<String,_>("observed_target_tree"),"result_revision":v.get::<String,_>("result_revision"),"result_tree":v.get::<String,_>("result_tree"),"created_by":v.get::<String,_>("created_by"),"created_at":timestamp(v.get("created_at"))}));
                let integration_result=sqlx::query("SELECT submission_id,attempt_id,publication_state,observed_target_revision,result_revision,result_tree,check_job_ids_json,summary,reported_by,created_at FROM integration_results WHERE activity_id=?").bind(&activity).fetch_optional(&mut *c).await?;
                let check_job_ids = integration_result
                    .as_ref()
                    .map(|v| {
                        serde_json::from_str::<Vec<String>>(
                            &v.get::<String, _>("check_job_ids_json"),
                        )
                    })
                    .transpose()?
                    .unwrap_or_default();
                let mut check_jobs = Vec::with_capacity(check_job_ids.len());
                for job_id in &check_job_ids {
                    if let Some(job) =
                        sqlx::query("SELECT * FROM jobs WHERE project_id=? AND task_id=? AND id=?")
                            .bind(row.get::<String, _>("project_id"))
                            .bind(row.get::<String, _>("activity_task_id"))
                            .bind(job_id)
                            .fetch_optional(&mut *c)
                            .await?
                    {
                        check_jobs.push(job_value(&job));
                    }
                }
                let integration_result=integration_result.map(|v|json!({"submission_id":v.get::<String,_>("submission_id"),"attempt_id":v.get::<String,_>("attempt_id"),"publication_state":v.get::<String,_>("publication_state"),"observed_target_revision":v.get::<String,_>("observed_target_revision"),"result_revision":v.get::<String,_>("result_revision"),"result_tree":v.get::<String,_>("result_tree"),"check_job_ids":check_job_ids,"summary":v.get::<String,_>("summary"),"reported_by":v.get::<String,_>("reported_by"),"created_at":timestamp(v.get("created_at"))}));
                let reconciliation=sqlx::query("SELECT submission_id,disposition,observed_target_revision,observed_target_tree,evidence,actor_id,created_at FROM publication_reconciliations WHERE activity_id=?").bind(&activity).fetch_optional(&mut *c).await?.map(|v|json!({"submission_id":v.get::<String,_>("submission_id"),"disposition":v.get::<String,_>("disposition"),"observed_target_revision":v.get::<String,_>("observed_target_revision"),"observed_target_tree":v.get::<String,_>("observed_target_tree"),"evidence":v.get::<String,_>("evidence"),"actor_id":v.get::<String,_>("actor_id"),"created_at":timestamp(v.get("created_at"))}));
                (
                    related_task(row),
                    time(Some(row.get("created_at"))),
                    json!({"activity":activity_value(row),"authorization":authorization,"hold":hold,"publication_intent":intent,"integration_result":integration_result,"check_jobs":check_jobs,"publication_reconciliation":reconciliation}),
                )
            }
            "task_revisions" => (
                related_task(row),
                time(Some(row.get("created_at"))),
                json!({"revision":row.get::<i64,_>("revision"),"definition":serde_json::from_str::<Value>(&row.get::<String,_>("data_json"))?,"actor_id":row.get::<String,_>("actor_id"),"created_at":timestamp(row.get("created_at"))}),
            ),
            "events" => {
                let mut data = serde_json::from_str::<Value>(&row.get::<String, _>("data_json"))?;
                redact_sensitive(&mut data);
                (
                    related_task(row),
                    time(Some(row.get("created_at"))),
                    json!({"seq":row.get::<i64,_>("seq"),"actor_id":row.get::<String,_>("actor_id"),"kind":row.get::<String,_>("kind"),"record_id":row.get::<String,_>("record_id"),"data":data,"created_at":timestamp(row.get("created_at"))}),
                )
            }
            _ => unreachable!("kind validated before materialization"),
        };
        result.push((
            cursor_id,
            TaskHistoryItem {
                kind: kind.to_owned(),
                relation: relation(row),
                task_id,
                occurred_at,
                record,
            },
        ));
    }
    Ok(result)
}

fn redact_sensitive(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            fields.retain(|key, value| {
                let key = key.to_ascii_lowercase();
                let sensitive = [
                    "authorization",
                    "cookie",
                    "credential",
                    "password",
                    "proof",
                    "request_hash",
                    "secret",
                    "session_id",
                    "token",
                ]
                .iter()
                .any(|part| key.contains(part));
                if !sensitive {
                    redact_sensitive(value);
                }
                !sensitive
            });
        }
        Value::Array(values) => values.iter_mut().for_each(redact_sensitive),
        _ => {}
    }
}

fn activity_value(row: &SqliteRow) -> Value {
    json!({"id":row.get::<String,_>("id"),"subject_task_id":row.get::<String,_>("subject_task_id"),"submission_id":row.get::<String,_>("submission_id"),"activity_task_id":row.get::<String,_>("activity_task_id"),"kind":row.get::<String,_>("kind"),"slot":row.get::<i64,_>("slot"),"state":row.get::<String,_>("state"),"created_at":timestamp(row.get("created_at")),"completed_at":time(row.get("completed_at")),"canceled_at":time(row.get("canceled_at"))})
}

fn job_value(row: &SqliteRow) -> Value {
    json!({"id":row.get::<String,_>("id"),"producer_id":row.get::<String,_>("producer_id"),"attempt_id":row.get::<String,_>("attempt_id"),"generation":row.get::<i64,_>("generation"),"runner_instance_id":row.get::<String,_>("runner_instance_id"),"workstation_id":row.get::<String,_>("workstation_id"),"label":row.get::<String,_>("label"),"source_revision":row.get::<String,_>("source_revision"),"source_tree":row.get::<String,_>("source_tree"),"reservation_id":row.get::<String,_>("reservation_id"),"state":row.get::<String,_>("state"),"last_sequence":row.get::<i64,_>("last_sequence"),"last_observed_at":time(row.get("last_observed_at")),"pid":row.get::<Option<i64>,_>("pid"),"process_started_at":row.get::<Option<String>,_>("process_started_at"),"exit_code":row.get::<Option<i64>,_>("exit_code"),"inputs_unchanged":row.get::<Option<bool>,_>("inputs_unchanged"),"summary":row.get::<String,_>("summary"),"reconciled_at":time(row.get("reconciled_at")),"reconciled_by":row.get::<Option<String>,_>("reconciled_by"),"reconciliation_reason":row.get::<Option<String>,_>("reconciliation_reason"),"reconciliation_evidence":row.get::<Option<String>,_>("reconciliation_evidence"),"check_identity":row.get::<Option<String>,_>("check_identity"),"check_version":row.get::<Option<String>,_>("check_version"),"check_environment":row.get::<Option<String>,_>("check_environment"),"created_at":timestamp(row.get("created_at"))})
}

fn artifact_value(row: &SqliteRow) -> Value {
    json!({"id":row.get::<String,_>("id"),"kind":row.get::<String,_>("kind"),"task_id":row.get::<Option<String>,_>("task_id"),"job_id":row.get::<Option<String>,_>("job_id"),"display_name":row.get::<String,_>("display_name"),"media_type":row.get::<String,_>("media_type"),"size_bytes":row.get::<Option<i64>,_>("size_bytes"),"sha256":row.get::<Option<String>,_>("sha256"),"external_url":row.get::<Option<String>,_>("external_url"),"state":row.get::<String,_>("state"),"created_by":row.get::<String,_>("created_by"),"created_at":timestamp(row.get("created_at")),"reservation_expires_at":time(row.get("reservation_expires_at")),"finalized_at":time(row.get("finalized_at")),"retention_until":time(row.get("retention_until")),"pinned":row.get::<bool,_>("pinned"),"deleted_at":time(row.get("deleted_at")),"deleted_by":row.get::<Option<String>,_>("deleted_by"),"deletion_reason":row.get::<Option<String>,_>("deletion_reason")})
}

fn submission_value(
    row: &SqliteRow,
    lessons: Vec<Value>,
    artifact_ids: Vec<String>,
) -> Result<Value, AppError> {
    Ok(
        json!({"id":row.get::<String,_>("id"),"attempt_id":row.get::<String,_>("attempt_id"),"kind":row.get::<String,_>("kind"),"task_revision":row.get::<i64,_>("task_revision"),"project_policy_revision":row.get::<i64,_>("project_policy_revision"),"workflow_policy_revision":row.get::<i64,_>("workflow_policy_revision"),"summary":row.get::<String,_>("summary"),"acceptance_evidence":serde_json::from_str::<Value>(&row.get::<String,_>("acceptance_evidence_json"))?,"handoff":row.get::<String,_>("handoff"),"lessons":lessons,"artifact_ids":artifact_ids,"canonical_repository_key":row.get::<Option<String>,_>("canonical_repository_key"),"repository":row.get::<Option<String>,_>("repository_url"),"target_branch":row.get::<Option<String>,_>("target_branch"),"base_revision":row.get::<Option<String>,_>("base_revision"),"candidate_revision":row.get::<Option<String>,_>("candidate_revision"),"candidate_tree":row.get::<Option<String>,_>("candidate_tree"),"created_by":row.get::<String,_>("created_by"),"created_at":timestamp(row.get("created_at")),"superseded_at":time(row.get("superseded_at"))}),
    )
}
