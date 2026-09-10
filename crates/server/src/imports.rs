//! Bounded, service-authoritative Markdown import previews and generated exports.
//! Source paths are provenance strings only; this module never reads them.

use crate::{auth::Auth, error::AppError, mutation::Mutation, response, state::AppState};
use axum::{
    Json, Router,
    extract::{Path, Query, State, rejection::JsonRejection},
    http::HeaderMap,
    routing::{get, post},
};
use coordinator_core::{
    ApplyImportInput, ExportRecord, HistoricalMapping, ImportConflict, ImportItem, ImportPreview,
    ImportPreviewInput, ImportSource, MarkdownChunk, UnresolvedMarkdownLink, timestamp,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

type Reply = Result<Json<Value>, AppError>;
const MAX_CHUNKS: usize = 32;
const MAX_SOURCE_BYTES: usize = 200 * 1024;
const MAX_ITEMS: usize = 200;
// Reserve space for the response helper's request ID and server timestamp so
// the complete serialized HTTP JSON remains within 256 KiB.
const MAX_EXPORT_DATA_BYTES: usize = 256 * 1024 - 512;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/projects/{project}/imports/preview", post(preview))
        .route(
            "/api/v1/projects/{project}/imports/{preview}",
            get(get_preview),
        )
        .route(
            "/api/v1/projects/{project}/imports/{preview}/apply",
            post(apply),
        )
        .route("/api/v1/projects/{project}/exports", get(export))
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

fn human(auth: &crate::auth::Actor) -> Result<(), AppError> {
    if auth.kind != "human" {
        return Err(AppError::forbidden(
            "A human operator must apply imported records.",
        ));
    }
    Ok(())
}

fn digest<T: serde::Serialize>(value: &T) -> Result<String, AppError> {
    let bytes = serde_json::to_vec(value)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn validate_source(source: &ImportSource) -> Result<(), AppError> {
    bounded(&source.context, "source context", 255, true)?;
    bounded(&source.git_revision, "Git revision", 128, true)?;
    bounded(&source.observed_at, "observation time", 64, true)?;
    bounded(&source.branch, "source branch", 255, false)?;
    bounded(&source.environment, "source environment", 255, false)?;
    chrono::DateTime::parse_from_rfc3339(&source.observed_at)
        .map_err(|_| AppError::bad_request("observed_at must be an RFC 3339 timestamp."))?;
    if !matches!(source.git_revision.len(), 40 | 64)
        || !source
            .git_revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(AppError::bad_request(
            "git_revision must be a full hexadecimal Git object ID.",
        ));
    }
    Ok(())
}

fn normalize_path(value: &str) -> Result<String, AppError> {
    bounded(value, "source path", 512, true)?;
    let value = value.replace('\\', "/");
    if value.starts_with('/') || value.split('/').any(|part| part == "..") {
        return Err(AppError::bad_request(
            "Source paths must be relative metadata paths without parent traversal.",
        ));
    }
    let parts: Vec<_> = value
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    if parts.is_empty() || parts[0].contains(':') {
        return Err(AppError::bad_request("Source path metadata is invalid."));
    }
    Ok(parts.join("/"))
}

fn normalize_identity_text(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn stable_identity(context: &str, path: &str, section: &str, anchor: &str) -> String {
    let mut hash = Sha256::new();
    for part in [context, path, section, anchor] {
        hash.update(part.as_bytes());
        hash.update([0]);
    }
    hex::encode(hash.finalize())
}

fn explicit_anchor(line: &str) -> Option<String> {
    let marker = "<!-- coordinator-id:";
    let start = line.find(marker)? + marker.len();
    let end = line[start..].find("-->")? + start;
    let value = line[start..end].trim();
    (!value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte)))
    .then(|| value.to_owned())
}

fn checklist(line: &str) -> Option<(bool, &str)> {
    let line = line.trim_start();
    for (prefix, closed) in [
        ("- [ ] ", false),
        ("* [ ] ", false),
        ("- [x] ", true),
        ("- [X] ", true),
        ("* [x] ", true),
        ("* [X] ", true),
    ] {
        if let Some(value) = line.strip_prefix(prefix) {
            return Some((closed, value));
        }
    }
    None
}

fn clean_title(value: &str) -> String {
    let without_anchor = value.split("<!-- coordinator-id:").next().unwrap_or(value);
    without_anchor
        .trim()
        .trim_matches('~')
        .trim_matches('*')
        .trim()
        .to_owned()
}

struct ParsedImport {
    items: Vec<ImportItem>,
    conflicts: Vec<ImportConflict>,
    unresolved_links: Vec<UnresolvedMarkdownLink>,
}

fn parse_chunks(
    source: &ImportSource,
    chunks: &[MarkdownChunk],
    mappings: &[HistoricalMapping],
) -> Result<ParsedImport, AppError> {
    if chunks.is_empty() || chunks.len() > MAX_CHUNKS {
        return Err(AppError::bad_request(
            "Provide 1–32 Markdown source chunks.",
        ));
    }
    let total: usize = chunks.iter().map(|chunk| chunk.markdown.len()).sum();
    if total > MAX_SOURCE_BYTES || mappings.len() > MAX_ITEMS {
        return Err(AppError::bad_request(
            "Import source is limited to 200 KiB and 200 mappings or parsed items.",
        ));
    }
    let mut normalized = Vec::with_capacity(chunks.len());
    let mut paths = HashSet::new();
    for chunk in chunks {
        let path = normalize_path(&chunk.path)?;
        bounded(&chunk.markdown, "Markdown chunk", MAX_SOURCE_BYTES, false)?;
        if !paths.insert(path.clone()) {
            return Err(AppError::bad_request("Source chunk paths must be unique."));
        }
        normalized.push((path, &chunk.markdown));
    }

    let mut items = Vec::new();
    let mut conflicts = Vec::new();
    let mut unresolved = Vec::new();
    let mut identities: HashMap<String, (String, String)> = HashMap::new();
    for (path, markdown) in &normalized {
        if markdown.contains("<!-- agent-coordinator-generated-export") {
            conflicts.push(ImportConflict {
                code: "generated_projection".into(),
                source_path: path.clone(),
                section_identity: "(document)".into(),
                message: "Generated exports are projections and cannot be imported as authority."
                    .into(),
                blocking: true,
            });
        }
        let mut headings: Vec<String> = Vec::new();
        let mut pending_anchor = None;
        for (index, line) in markdown.lines().enumerate() {
            let trimmed = line.trim();
            if let Some(anchor) = explicit_anchor(trimmed) {
                pending_anchor = Some(anchor);
            }
            if trimmed.starts_with('#') {
                let level = trimmed.chars().take_while(|ch| *ch == '#').count();
                if (1..=6).contains(&level) && trimmed.as_bytes().get(level) == Some(&b' ') {
                    headings.truncate(level - 1);
                    headings.push(trimmed[level + 1..].trim().to_owned());
                    pending_anchor = None;
                }
            }
            scan_links(path, index + 1, line, &paths, &mut unresolved);
            let Some((closed, raw_title)) = checklist(line) else {
                continue;
            };
            let title = clean_title(raw_title);
            if title.is_empty() || title.len() > 300 {
                conflicts.push(ImportConflict {
                    code: "invalid_checklist_item".into(),
                    source_path: path.clone(),
                    section_identity: headings.join(" / "),
                    message: "Checklist titles must contain 1–300 bytes.".into(),
                    blocking: true,
                });
                continue;
            }
            let section = if headings.is_empty() {
                "(root)".to_owned()
            } else {
                headings.join(" / ")
            };
            let anchor = explicit_anchor(raw_title)
                .or_else(|| pending_anchor.take())
                .unwrap_or_else(|| normalize_identity_text(&title));
            let identity = stable_identity(&source.context, path, &section, &anchor);
            if let Some((first_path, first_section)) = identities.get(&identity) {
                conflicts.push(ImportConflict {
                    code: "ambiguous_source_identity".into(),
                    source_path: path.clone(),
                    section_identity: section.clone(),
                    message: format!(
                        "This item repeats source identity already used at {first_path} in {first_section}; add distinct coordinator-id markers."
                    ),
                    blocking: true,
                });
                continue;
            }
            identities.insert(identity.clone(), (path.clone(), section.clone()));
            items.push(ImportItem {
                stable_identity: identity,
                source_path: path.clone(),
                section_identity: section,
                title,
                record_kind: "task".into(),
                disposition: if closed { "closed" } else { "planned" }.into(),
                source_line: index + 1,
                evidence: if closed {
                    "Explicit checked Markdown checklist item at the recorded source revision."
                        .into()
                } else {
                    String::new()
                },
                prior_record_revision: None,
            });
        }
    }
    for mapping in mappings {
        let path = normalize_path(&mapping.path)?;
        if !paths.contains(&path) {
            return Err(AppError::bad_request(
                "Every historical mapping path must name a supplied source chunk.",
            ));
        }
        bounded(&mapping.section_identity, "section identity", 512, true)?;
        bounded(&mapping.title, "historical title", 255, true)?;
        bounded(&mapping.evidence, "historical evidence", 8192, true)?;
        if !["closed", "rejected", "superseded"].contains(&mapping.disposition.as_str()) {
            return Err(AppError::bad_request(
                "Historical disposition must be closed, rejected, or superseded.",
            ));
        }
        let identity = stable_identity(
            &source.context,
            &path,
            &mapping.section_identity,
            &normalize_identity_text(&mapping.title),
        );
        if identities.contains_key(&identity) {
            conflicts.push(ImportConflict {
                code: "ambiguous_source_identity".into(),
                source_path: path.clone(),
                section_identity: mapping.section_identity.clone(),
                message: "Historical mapping duplicates another source identity.".into(),
                blocking: true,
            });
            continue;
        }
        identities.insert(
            identity.clone(),
            (path.clone(), mapping.section_identity.clone()),
        );
        items.push(ImportItem {
            stable_identity: identity,
            source_path: path,
            section_identity: mapping.section_identity.clone(),
            title: mapping.title.clone(),
            record_kind: "historical".into(),
            disposition: mapping.disposition.clone(),
            source_line: 0,
            evidence: mapping.evidence.clone(),
            prior_record_revision: None,
        });
    }
    if items.len() > MAX_ITEMS {
        return Err(AppError::bad_request(
            "A preview may contain at most 200 items.",
        ));
    }
    Ok(ParsedImport {
        items,
        conflicts,
        unresolved_links: unresolved,
    })
}

fn scan_links(
    source_path: &str,
    line_number: usize,
    line: &str,
    known_paths: &HashSet<String>,
    unresolved: &mut Vec<UnresolvedMarkdownLink>,
) {
    let mut rest = line;
    while let Some(close_label) = rest.find("](") {
        let target_start = close_label + 2;
        let Some(close_target) = rest[target_start..].find(')') else {
            break;
        };
        let raw = rest[target_start..target_start + close_target].trim();
        let target = raw.split_whitespace().next().unwrap_or("");
        if !target.is_empty()
            && !target.starts_with('#')
            && !target.contains("://")
            && !target.starts_with("mailto:")
        {
            let target = target.split(['#', '?']).next().unwrap_or(target);
            let resolved = resolve_link(source_path, target);
            if !known_paths.contains(&resolved) {
                unresolved.push(UnresolvedMarkdownLink {
                    source_path: source_path.to_owned(),
                    source_line: line_number,
                    target: target.to_owned(),
                });
            }
        }
        rest = &rest[target_start + close_target + 1..];
    }
}

fn resolve_link(source: &str, target: &str) -> String {
    if target.starts_with('/') {
        return target.trim_start_matches('/').replace('\\', "/");
    }
    let mut parts: Vec<String> = source.split('/').map(str::to_owned).collect();
    parts.pop();
    let normalized_target = target.replace('\\', "/");
    for part in normalized_target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            value => parts.push(value.to_owned()),
        }
    }
    parts.join("/")
}

async fn project_event_revision(c: &mut SqliteConnection, project: &str) -> Result<i64, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT COALESCE(max(seq),0) FROM events WHERE project_id=? AND kind!='import.preview_created'",
    )
    .bind(project)
    .fetch_one(c)
    .await?)
}

async fn preview(
    State(state): State<AppState>,
    auth: Auth,
    Path(project): Path<String>,
    headers: HeaderMap,
    body: Result<Json<ImportPreviewInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    validate_source(&input.source)?;
    let ParsedImport {
        mut items,
        mut conflicts,
        unresolved_links,
    } = parse_chunks(&input.source, &input.chunks, &input.historical_mappings)?;
    let preview_digest = digest(&input)?;
    let mut mutation = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{project}/imports/preview"),
        &input,
    )
    .await?;
    let exists: i64 = sqlx::query_scalar("SELECT count(*) FROM projects WHERE id=?")
        .bind(&project)
        .fetch_one(&mut *mutation.tx)
        .await?;
    if exists == 0 {
        return Err(AppError::not_found());
    }
    if let Some(value) = mutation.replay {
        return Ok(response(value));
    }
    for item in &mut items {
        item.prior_record_revision = sqlx::query_scalar(
            "SELECT record_revision FROM import_records WHERE project_id=? AND stable_identity=?",
        )
        .bind(&project)
        .bind(&item.stable_identity)
        .fetch_optional(&mut *mutation.tx)
        .await?;
        if let Some(row)=sqlx::query("SELECT ir.task_revision_at_apply,t.revision,t.lifecycle,t.current_attempt_id,EXISTS(SELECT 1 FROM attempts a WHERE a.task_id=t.id) AS has_attempt_history,EXISTS(SELECT 1 FROM workflow_subjects ws WHERE ws.task_id=t.id) AS has_workflow FROM import_records ir JOIN tasks t ON t.id=ir.task_id WHERE ir.project_id=? AND ir.stable_identity=?")
            .bind(&project).bind(&item.stable_identity).fetch_optional(&mut *mutation.tx).await?
            && (row.get::<i64,_>("revision") > row.get::<i64,_>("task_revision_at_apply")
                || !matches!(row.get::<String,_>("lifecycle").as_str(),"planned"|"done")
                || row.get::<Option<String>,_>("current_attempt_id").is_some()
                || row.get::<bool,_>("has_attempt_history")
                || row.get::<bool,_>("has_workflow")) {
            conflicts.push(ImportConflict { code:"newer_service_state".into(), source_path:item.source_path.clone(), section_identity:item.section_identity.clone(), message:"The linked service task has newer edits and will be preserved.".into(), blocking:false });
        }
    }
    let event_revision = project_event_revision(&mut mutation.tx, &project).await?;
    let id = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO import_previews(id,project_id,digest,project_event_revision,source_json,items_json,conflicts_json,unresolved_links_json,created_by,created_at) VALUES(?,?,?,?,?,?,?,?,?,?)")
        .bind(&id).bind(&project).bind(&preview_digest).bind(event_revision)
        .bind(serde_json::to_string(&input.source)?).bind(serde_json::to_string(&items)?)
        .bind(serde_json::to_string(&conflicts)?).bind(serde_json::to_string(&unresolved_links)?)
        .bind(&mutation.actor.id).bind(mutation.now).execute(&mut *mutation.tx).await?;
    let value = serde_json::to_value(ImportPreview {
        id: id.clone(),
        digest: preview_digest,
        project_id: project.clone(),
        project_event_revision: event_revision,
        source: input.source,
        items,
        conflicts,
        unresolved_links,
        created_at: timestamp(mutation.now),
        applied_at: None,
    })?;
    Ok(response(
        mutation
            .finish(value, Some(&project), "import.preview_created", &id)
            .await?,
    ))
}

async fn load_preview(
    c: &mut SqliteConnection,
    project: &str,
    id: &str,
) -> Result<ImportPreview, AppError> {
    let row = sqlx::query("SELECT * FROM import_previews WHERE project_id=? AND id=?")
        .bind(project)
        .bind(id)
        .fetch_optional(&mut *c)
        .await?
        .ok_or_else(AppError::not_found)?;
    Ok(ImportPreview {
        id: row.get("id"),
        digest: row.get("digest"),
        project_id: row.get("project_id"),
        project_event_revision: row.get("project_event_revision"),
        source: serde_json::from_str(&row.get::<String, _>("source_json"))?,
        items: serde_json::from_str(&row.get::<String, _>("items_json"))?,
        conflicts: serde_json::from_str(&row.get::<String, _>("conflicts_json"))?,
        unresolved_links: serde_json::from_str(&row.get::<String, _>("unresolved_links_json"))?,
        created_at: timestamp(row.get("created_at")),
        applied_at: row.get::<Option<i64>, _>("applied_at").map(timestamp),
    })
}

async fn get_preview(
    State(state): State<AppState>,
    _auth: Auth,
    Path((project, preview)): Path<(String, String)>,
) -> Reply {
    let mut c = state.pool.acquire().await?;
    Ok(response(serde_json::to_value(
        load_preview(&mut c, &project, &preview).await?,
    )?))
}

async fn apply(
    State(state): State<AppState>,
    auth: Auth,
    Path((project, preview_id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<ApplyImportInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    bounded(&input.preview_digest, "preview digest", 64, true)?;
    let mut mutation = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{project}/imports/{preview_id}/apply"),
        &input,
    )
    .await?;
    human(&mutation.actor)?;
    if let Some(value) = mutation.replay {
        return Ok(response(value));
    }
    let preview = load_preview(&mut mutation.tx, &project, &preview_id).await?;
    if preview.applied_at.is_some() {
        return Err(AppError::conflict(
            "import_already_applied",
            "This preview was already applied; retry with its original idempotency key.",
        ));
    }
    if preview.digest != input.preview_digest
        || preview.project_event_revision != input.expected_project_event_revision
    {
        return Err(AppError::conflict(
            "preview_mismatch",
            "Apply must name the exact saved preview digest and project event revision.",
        ));
    }
    let current_event = project_event_revision(&mut mutation.tx, &project).await?;
    if current_event != preview.project_event_revision {
        return Err(AppError::conflict(
            "stale_import_preview",
            "Project state changed after preview; create and inspect a new preview.",
        ));
    }
    if preview.conflicts.iter().any(|conflict| conflict.blocking) {
        return Err(AppError::conflict(
            "import_conflicts",
            "Resolve blocking preview conflicts before applying.",
        ));
    }
    for item in &preview.items {
        let current: Option<i64> = sqlx::query_scalar(
            "SELECT record_revision FROM import_records WHERE project_id=? AND stable_identity=?",
        )
        .bind(&project)
        .bind(&item.stable_identity)
        .fetch_optional(&mut *mutation.tx)
        .await?;
        if current != item.prior_record_revision {
            return Err(AppError::conflict(
                "stale_import_record",
                "An imported record changed after preview; create and inspect a new preview.",
            ));
        }
    }
    let source_digest = preview.digest.clone();
    let mut created = 0;
    let mut updated = 0;
    let mut preserved = 0;
    let mut closed = 0;
    for item in &preview.items {
        let existing =
            sqlx::query("SELECT * FROM import_records WHERE project_id=? AND stable_identity=?")
                .bind(&project)
                .bind(&item.stable_identity)
                .fetch_optional(&mut *mutation.tx)
                .await?;
        if let Some(record) = existing {
            let mut disposition: String = record.get("disposition");
            let task_id: Option<String> = record.get("task_id");
            let mut task_revision_at_apply: Option<i64> = record.get("task_revision_at_apply");
            let knowledge_id: Option<String> = record.get("knowledge_id");
            let mut knowledge_revision_at_apply: Option<i64> =
                record.get("knowledge_revision_at_apply");
            if disposition != "closed" && item.disposition == "closed" {
                disposition = "closed".into();
            }
            if let Some(task_id) = task_id.as_deref() {
                let task = sqlx::query(
                    "SELECT t.revision,t.lifecycle,t.title,t.current_attempt_id, \
                     NOT EXISTS(SELECT 1 FROM attempts a WHERE a.task_id=t.id) AS no_attempt_history, \
                     NOT EXISTS(SELECT 1 FROM workflow_subjects ws WHERE ws.task_id=t.id) AS no_workflow \
                     FROM tasks t WHERE t.project_id=? AND t.id=?",
                )
                .bind(&project)
                .bind(task_id)
                .fetch_one(&mut *mutation.tx)
                .await?;
                let current_revision: i64 = task.get("revision");
                let import_managed = matches!(
                    task.get::<String, _>("lifecycle").as_str(),
                    "planned" | "done"
                ) && task
                    .get::<Option<String>, _>("current_attempt_id")
                    .is_none()
                    && task.get::<bool, _>("no_attempt_history")
                    && task.get::<bool, _>("no_workflow");
                if Some(current_revision) == task_revision_at_apply && import_managed {
                    if task.get::<String, _>("lifecycle") == "done" || disposition == "closed" {
                        disposition = "closed".into();
                    }
                    let lifecycle = if disposition == "closed" {
                        "done"
                    } else {
                        "planned"
                    };
                    if task.get::<String, _>("title") != item.title
                        || task.get::<String, _>("lifecycle") != lifecycle
                    {
                        sqlx::query(
                            "UPDATE tasks SET title=?,lifecycle=?,revision=revision+1 WHERE id=?",
                        )
                        .bind(&item.title)
                        .bind(lifecycle)
                        .bind(task_id)
                        .execute(&mut *mutation.tx)
                        .await?;
                        task_revision_at_apply = Some(current_revision + 1);
                        save_imported_task_revision(&mut mutation, &project, task_id).await?;
                    }
                } else {
                    preserved += 1;
                    if task.get::<String, _>("lifecycle") == "done" {
                        disposition = "closed".into();
                    }
                }
            }
            if let Some(knowledge_id) = knowledge_id.as_deref() {
                let current_revision: i64 = sqlx::query_scalar(
                    "SELECT current_revision FROM knowledge_records WHERE source_project_id=? AND id=?",
                )
                .bind(&project)
                .bind(knowledge_id)
                .fetch_one(&mut *mutation.tx)
                .await?;
                if Some(current_revision) == knowledge_revision_at_apply {
                    let next_revision = current_revision + 1;
                    save_imported_knowledge(
                        &mut mutation,
                        &project,
                        knowledge_id,
                        next_revision,
                        item,
                        &preview.source,
                    )
                    .await?;
                    knowledge_revision_at_apply = Some(next_revision);
                } else {
                    preserved += 1;
                }
            }
            let closure = closure_provenance(item, &preview.source, &preview.id, &disposition)?;
            sqlx::query("UPDATE import_records SET record_revision=record_revision+1,disposition=?,title=?,source_git_revision=?,source_observed_at=?,source_branch=?,source_environment=?,source_digest=?,evidence=?,task_revision_at_apply=?,knowledge_revision_at_apply=?,closure_provenance_json=COALESCE(closure_provenance_json,?),preview_id=?,updated_by=?,updated_at=? WHERE id=?")
                .bind(&disposition).bind(&item.title).bind(&preview.source.git_revision).bind(&preview.source.observed_at)
                .bind(&preview.source.branch).bind(&preview.source.environment).bind(&source_digest).bind(&item.evidence)
                .bind(task_revision_at_apply).bind(knowledge_revision_at_apply).bind(closure).bind(&preview.id).bind(&mutation.actor.id).bind(mutation.now)
                .bind(record.get::<String,_>("id")).execute(&mut *mutation.tx).await?;
            updated += 1;
            if disposition == "closed" {
                closed += 1;
            }
        } else {
            let (task_id, task_revision, knowledge_id, knowledge_revision) = if item.record_kind
                == "task"
            {
                let task_id = Uuid::new_v4().to_string();
                let lifecycle = if item.disposition == "closed" {
                    "done"
                } else {
                    "planned"
                };
                let description = format!(
                    "Imported from {} at {} ({}; branch {}; environment {}).\n\n{}",
                    item.source_path,
                    preview.source.git_revision,
                    preview.source.observed_at,
                    preview.source.branch,
                    preview.source.environment,
                    item.evidence
                );
                let acceptance = serde_json::to_string(&vec![format!(
                    "Imported checklist item remains traceable to {} / {}.",
                    item.source_path, item.section_identity
                )])?;
                sqlx::query("INSERT INTO tasks(id,project_id,title,description,acceptance_json,kind,priority,lifecycle,created_at,ready_since) VALUES(?,?,?,?,?,'general',2,?,?,?)")
                    .bind(&task_id).bind(&project).bind(&item.title).bind(description).bind(acceptance).bind(lifecycle).bind(mutation.now).bind(mutation.now).execute(&mut *mutation.tx).await?;
                save_imported_task_revision(&mut mutation, &project, &task_id).await?;
                (Some(task_id), Some(1_i64), None, None)
            } else {
                let knowledge_id = Uuid::new_v4().to_string();
                save_imported_knowledge(
                    &mut mutation,
                    &project,
                    &knowledge_id,
                    1,
                    item,
                    &preview.source,
                )
                .await?;
                (None, None, Some(knowledge_id), Some(1_i64))
            };
            let closure =
                closure_provenance(item, &preview.source, &preview.id, &item.disposition)?;
            sqlx::query("INSERT INTO import_records(id,project_id,stable_identity,record_revision,record_kind,disposition,title,source_context,source_path,section_identity,source_git_revision,source_observed_at,source_branch,source_environment,source_digest,evidence,task_id,task_revision_at_apply,knowledge_id,knowledge_revision_at_apply,closure_provenance_json,preview_id,created_by,created_at,updated_by,updated_at) VALUES(?,?,?,1,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
                .bind(Uuid::new_v4().to_string()).bind(&project).bind(&item.stable_identity).bind(&item.record_kind).bind(&item.disposition).bind(&item.title)
                .bind(&preview.source.context).bind(&item.source_path).bind(&item.section_identity).bind(&preview.source.git_revision).bind(&preview.source.observed_at)
                .bind(&preview.source.branch).bind(&preview.source.environment).bind(&source_digest).bind(&item.evidence).bind(task_id).bind(task_revision).bind(knowledge_id).bind(knowledge_revision).bind(closure)
                .bind(&preview.id).bind(&mutation.actor.id).bind(mutation.now).bind(&mutation.actor.id).bind(mutation.now).execute(&mut *mutation.tx).await?;
            created += 1;
            if item.disposition == "closed" {
                closed += 1;
            }
        }
    }
    let value = json!({"preview_id":preview.id,"preview_digest":preview.digest,"project_event_revision":preview.project_event_revision,"created":created,"updated":updated,"preserved_service_tasks":preserved,"closed":closed});
    sqlx::query("INSERT INTO import_applications(preview_id,project_id,preview_digest,project_event_revision,result_json,applied_by,applied_at) VALUES(?,?,?,?,?,?,?)")
        .bind(&preview.id).bind(&project).bind(&preview.digest).bind(preview.project_event_revision).bind(value.to_string()).bind(&mutation.actor.id).bind(mutation.now).execute(&mut *mutation.tx).await?;
    sqlx::query("UPDATE import_previews SET applied_by=?,applied_at=? WHERE id=?")
        .bind(&mutation.actor.id)
        .bind(mutation.now)
        .bind(&preview.id)
        .execute(&mut *mutation.tx)
        .await?;
    Ok(response(
        mutation
            .finish(value, Some(&project), "import.applied", &preview.id)
            .await?,
    ))
}

fn closure_provenance(
    item: &ImportItem,
    source: &ImportSource,
    preview: &str,
    disposition: &str,
) -> Result<Option<String>, AppError> {
    if disposition != "closed" {
        return Ok(None);
    }
    Ok(Some(json!({"kind":"imported_checklist_closure","preview_id":preview,"source_context":source.context,"source_path":item.source_path,"section_identity":item.section_identity,"git_revision":source.git_revision,"observed_at":source.observed_at,"branch":source.branch,"environment":source.environment,"evidence":item.evidence}).to_string()))
}

async fn save_imported_task_revision(
    m: &mut Mutation,
    project: &str,
    task_id: &str,
) -> Result<(), AppError> {
    let row=sqlx::query("SELECT title,description,acceptance_json,kind,priority,lifecycle,revision FROM tasks WHERE project_id=? AND id=?")
        .bind(project).bind(task_id).fetch_one(&mut *m.tx).await?;
    let value = json!({"id":task_id,"project_id":project,"title":row.get::<String,_>("title"),"description":row.get::<String,_>("description"),"acceptance_criteria":serde_json::from_str::<Value>(&row.get::<String,_>("acceptance_json"))?,"kind":row.get::<String,_>("kind"),"priority":row.get::<i64,_>("priority"),"lifecycle":row.get::<String,_>("lifecycle"),"revision":row.get::<i64,_>("revision"),"depends_on":[]});
    sqlx::query("INSERT INTO task_revisions(project_id,task_id,revision,data_json,actor_id,created_at) VALUES(?,?,?,?,?,?)")
        .bind(project).bind(task_id).bind(row.get::<i64,_>("revision")).bind(value.to_string()).bind(&m.actor.id).bind(m.now).execute(&mut *m.tx).await?;
    Ok(())
}

async fn save_imported_knowledge(
    mutation: &mut Mutation,
    project: &str,
    knowledge_id: &str,
    revision: i64,
    item: &ImportItem,
    source: &ImportSource,
) -> Result<(), AppError> {
    let (kind, status) = match item.disposition.as_str() {
        "rejected" => ("rejected_approach", "validated"),
        "superseded" => ("checkpoint", "superseded"),
        _ => ("fact", "validated"),
    };
    let scope = json!({
        "source_context":source.context,
        "source_path":item.source_path,
        "section_identity":item.section_identity,
        "branch":source.branch,
        "environment":source.environment
    });
    let tags = json!(["imported", "historical"]);
    let applicability =
        "Historical evidence only; this record is not task, policy, hook, or execution authority.";
    let provenance = json!({
        "imported":true,
        "git_revision":source.git_revision,
        "observed_at":source.observed_at,
        "stable_identity":item.stable_identity
    });
    if revision == 1 {
        sqlx::query("INSERT INTO knowledge_records(id,source_project_id,collection,current_revision,kind,status,title,body,scope_json,tags_json,applicability,provenance_json,created_by,created_at,updated_at) VALUES(?,?,'project',1,?,?,?,?,?,?,?,?,?,?,?)")
            .bind(knowledge_id).bind(project).bind(kind).bind(status).bind(&item.title).bind(&item.evidence)
            .bind(scope.to_string()).bind(tags.to_string()).bind(applicability).bind(provenance.to_string())
            .bind(&mutation.actor.id).bind(mutation.now).bind(mutation.now).execute(&mut *mutation.tx).await?;
    } else {
        sqlx::query("UPDATE knowledge_records SET current_revision=?,kind=?,status=?,title=?,body=?,scope_json=?,tags_json=?,applicability=?,provenance_json=?,superseded_by_id=NULL,updated_at=? WHERE source_project_id=? AND id=? AND current_revision=?")
            .bind(revision).bind(kind).bind(status).bind(&item.title).bind(&item.evidence).bind(scope.to_string()).bind(tags.to_string())
            .bind(applicability).bind(provenance.to_string()).bind(mutation.now).bind(project).bind(knowledge_id).bind(revision-1)
            .execute(&mut *mutation.tx).await?;
    }
    sqlx::query("INSERT INTO knowledge_revisions(knowledge_id,revision,kind,status,title,body,scope_json,tags_json,applicability,provenance_json,superseded_by_id,actor_id,created_at) VALUES(?,?,?,?,?,?,?,?,?,?,NULL,?,?)")
        .bind(knowledge_id).bind(revision).bind(kind).bind(status).bind(&item.title).bind(&item.evidence)
        .bind(scope.to_string()).bind(tags.to_string()).bind(applicability).bind(provenance.to_string()).bind(&mutation.actor.id).bind(mutation.now)
        .execute(&mut *mutation.tx).await?;
    Ok(())
}

#[derive(Deserialize)]
struct ExportPage {
    cursor: Option<String>,
    limit: Option<i64>,
}

async fn export(
    State(state): State<AppState>,
    _auth: Auth,
    Path(project): Path<String>,
    Query(page): Query<ExportPage>,
) -> Reply {
    let limit = page.limit.unwrap_or(50);
    if !(1..=200).contains(&limit) {
        return Err(AppError::bad_request("limit must be between 1 and 200."));
    }
    let mut c = state.pool.acquire().await?;
    let exists: i64 = sqlx::query_scalar("SELECT count(*) FROM projects WHERE id=?")
        .bind(&project)
        .fetch_one(&mut *c)
        .await?;
    if exists == 0 {
        return Err(AppError::not_found());
    }
    let snapshot = project_event_revision(&mut c, &project).await?;
    let after = match page.cursor {
        None => String::new(),
        Some(cursor) => {
            bounded(&cursor, "export cursor", 512, true)?;
            let (revision, key) = cursor
                .split_once('~')
                .ok_or_else(|| AppError::bad_request("Invalid export cursor."))?;
            if revision.parse::<i64>().ok() != Some(snapshot) {
                return Err(AppError::conflict(
                    "export_snapshot_changed",
                    "Project records changed between export pages; restart the export.",
                ));
            }
            key.to_owned()
        }
    };
    let rows=sqlx::query("SELECT * FROM (\
        SELECT 'task:'||t.id sort_key,'task' kind,t.id,t.title,t.lifecycle status,t.description body,json_object('revision',t.revision,'created_at',t.created_at,'import_identity',ir.stable_identity) provenance FROM tasks t LEFT JOIN import_records ir ON ir.task_id=t.id WHERE t.project_id=? AND NOT EXISTS(SELECT 1 FROM workflow_activities wa WHERE wa.activity_task_id=t.id) \
        UNION ALL SELECT 'submission:'||s.id,'submission',s.id,s.summary,CASE WHEN s.superseded_at IS NULL THEN 'current' ELSE 'superseded' END,s.handoff,json_object('task_id',s.task_id,'kind',s.kind,'created_at',s.created_at) FROM submissions s WHERE s.project_id=? \
        UNION ALL SELECT printf('project-policy:%020d',pr.revision),'project_policy',CAST(pr.revision AS TEXT),'Project policy revision '||pr.revision,'historical',pr.data_json,json_object('revision',pr.revision,'created_at',pr.created_at,'provenance',pr.provenance) FROM policy_revisions pr WHERE pr.project_id=? \
        UNION ALL SELECT printf('workflow-policy:%020d',wr.revision),'workflow_policy',CAST(wr.revision AS TEXT),'Workflow policy revision '||wr.revision,'historical',wr.required_checks_json,json_object('revision',wr.revision,'canonical_repository_key',wr.canonical_repository_key,'created_at',wr.created_at) FROM workflow_policy_revisions wr WHERE wr.project_id=? \
        UNION ALL SELECT 'knowledge:'||kr.id,'knowledge',kr.id,kr.title,kr.status,kr.body,json_object('revision',kr.current_revision,'collection',kr.collection,'kind',kr.kind,'scope',json(kr.scope_json),'tags',json(kr.tags_json),'applicability',kr.applicability,'provenance',json(kr.provenance_json),'created_at',kr.created_at,'updated_at',kr.updated_at) FROM knowledge_records kr WHERE kr.source_project_id=? \
        UNION ALL SELECT 'decision:'||d.id,'decision',d.id,d.question,COALESCE((SELECT da.disposition FROM decision_answers da WHERE da.decision_id=d.id AND da.generation=d.current_generation),'pending'),d.rationale,json_object('generation',d.current_generation,'required_actor',d.required_actor,'options',json(d.options_json),'cycle_environment',(SELECT dc.environment FROM decision_cycles dc WHERE dc.decision_id=d.id AND dc.generation=d.current_generation),'cycle_conditions',(SELECT dc.conditions FROM decision_cycles dc WHERE dc.decision_id=d.id AND dc.generation=d.current_generation),'answer',(SELECT da.answer FROM decision_answers da WHERE da.decision_id=d.id AND da.generation=d.current_generation),'created_at',d.created_at) FROM decisions d WHERE d.project_id=? \
        UNION ALL SELECT 'historical:'||ir.id,'historical',ir.id,ir.title,ir.disposition,ir.evidence,json_object('stable_identity',ir.stable_identity,'source_context',ir.source_context,'source_path',ir.source_path,'section_identity',ir.section_identity,'git_revision',ir.source_git_revision,'observed_at',ir.source_observed_at,'branch',ir.source_branch,'environment',ir.source_environment,'record_revision',ir.record_revision) FROM import_records ir WHERE ir.project_id=? AND ir.record_kind='historical'\
        ) WHERE sort_key>? ORDER BY sort_key LIMIT ?")
        .bind(&project).bind(&project).bind(&project).bind(&project).bind(&project).bind(&project).bind(&project).bind(&after).bind(limit+1).fetch_all(&mut *c).await?;
    let mut more = rows.len() > limit as usize;
    let rows = &rows[..rows.len().min(limit as usize)];
    let mut records: Vec<ExportRecord> = rows
        .iter()
        .map(|row| ExportRecord {
            sort_key: row.get("sort_key"),
            kind: row.get("kind"),
            id: row.get("id"),
            title: row.get("title"),
            status: row.get("status"),
            body: row.get("body"),
            provenance: serde_json::from_str(&row.get::<String, _>("provenance"))
                .unwrap_or_else(|_| json!({})),
        })
        .collect();
    let generated_at = timestamp(state.now());
    loop {
        let next_cursor = if more {
            records
                .last()
                .map(|record| format!("{snapshot}~{}", record.sort_key))
        } else {
            None
        };
        let mut markdown = format!(
            "<!-- agent-coordinator-generated-export project={} event_revision={} generated_at={} -->\n\n# Agent Coordinator export\n\n",
            project, snapshot, generated_at
        );
        for record in &records {
            markdown.push_str(&format!(
                "## {}: {}\n\nStatus: `{}`\n\n{}\n\n",
                record.kind, record.title, record.status, record.body
            ));
        }
        let value = json!({"project_id":project,"snapshot_event_revision":snapshot,"generated_at":generated_at,"generated":true,"records":records,"markdown":markdown,"next_cursor":next_cursor,"page_complete":!more,"omissions":[]});
        // Measure the complete envelope shape as returned by `response`, because
        // record content is represented in both structured JSON and Markdown.
        if serde_json::to_vec(&json!({"data":&value}))?.len() <= MAX_EXPORT_DATA_BYTES {
            return Ok(response(value));
        }
        if records.len() <= 1 {
            return Err(AppError::conflict(
                "export_record_too_large",
                "One authoritative export record exceeds the 256 KiB page budget; no content was truncated.",
            ));
        }
        records.pop();
        more = true;
    }
}
