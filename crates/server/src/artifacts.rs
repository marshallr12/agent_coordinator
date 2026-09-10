//! Bounded artifact links and streamed uploads.
use crate::{auth::Auth, error::AppError, mutation::Mutation, response, state::AppState};
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, Query, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::Response,
    routing::{get, post},
};
use coordinator_core::{
    ArtifactDeleteInput, ArtifactLinkInput, ArtifactRetentionInput, ArtifactUploadInput, timestamp,
};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection};
use std::{
    collections::{HashMap, HashSet},
    io,
    path::{Path as FsPath, PathBuf},
    sync::OnceLock,
    time::{Duration, SystemTime},
};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{Mutex, Semaphore},
};
use tokio_util::io::ReaderStream;
use uuid::Uuid;

type Reply = Result<Json<Value>, AppError>;

pub const MAX_ARTIFACT_BYTES: i64 = 16 * 1024 * 1024;
pub const LIVE_ARTIFACT_QUOTA_BYTES: i64 = 10 * 1024 * 1024 * 1024;
pub const DISK_FREE_RESERVE_BYTES: u64 = 256 * 1024 * 1024;
const DEFAULT_RETENTION_DAYS: i64 = 90;
const MAX_RETENTION_DAYS: i64 = 3650;
const RESERVATION_LIFETIME_MS: i64 = 60 * 60 * 1000;
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const LOCK_WAIT: Duration = Duration::from_secs(5);
const STALE_STAGING_AGE: Duration = Duration::from_secs(5 * 60);

static UPLOAD_LIMIT: OnceLock<Semaphore> = OnceLock::new();
static DISK_WRITE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static CLEANUP_CURSORS: OnceLock<std::sync::Mutex<CleanupCursors>> = OnceLock::new();

#[derive(Default)]
struct CleanupCursors {
    expired: HashMap<PathBuf, String>,
    orphan_prefix: HashMap<PathBuf, u8>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/projects/{project}/artifacts",
            get(list_artifacts).post(create_link),
        )
        .route(
            "/api/v1/projects/{project}/artifacts/uploads",
            post(reserve_upload),
        )
        .route(
            "/api/v1/projects/{project}/artifacts/{artifact}",
            get(artifact_detail),
        )
        .route(
            "/api/v1/projects/{project}/artifacts/{artifact}/content",
            get(download).put(upload).layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/api/v1/projects/{project}/artifacts/{artifact}/retention",
            post(update_retention),
        )
        .route(
            "/api/v1/projects/{project}/artifacts/{artifact}/delete",
            post(delete_artifact),
        )
}

fn payload<T>(value: Result<Json<T>, JsonRejection>) -> Result<T, AppError> {
    value.map(|Json(value)| value).map_err(|_| {
        AppError::bad_request("The JSON body does not match this operation's request schema.")
    })
}

fn bounded(value: &str, name: &str, max: usize, required: bool) -> Result<(), AppError> {
    if value.len() > max
        || (required && value.trim().is_empty())
        || value.contains('\0')
        || value.chars().any(char::is_control)
    {
        return Err(AppError::bad_request(&format!(
            "{name} must {}contain at most {max} bytes and no control characters.",
            if required { "be nonempty and " } else { "" }
        )));
    }
    Ok(())
}

fn validate_media_type(value: &str) -> Result<(), AppError> {
    bounded(value, "media_type", 127, true)?;
    let Some((kind, subtype)) = value.split_once('/') else {
        return Err(AppError::bad_request(
            "media_type must be a MIME type such as text/plain.",
        ));
    };
    let token = |part: &str| {
        !part.is_empty()
            && part.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(
                        byte,
                        b'!' | b'#' | b'$' | b'&' | b'^' | b'_' | b'.' | b'+' | b'-'
                    )
            })
    };
    if !token(kind) || !token(subtype) {
        return Err(AppError::bad_request(
            "media_type must be a MIME type without parameters or control characters.",
        ));
    }
    Ok(())
}

fn validate_sha256(value: &str) -> Result<(), AppError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(AppError::bad_request(
            "sha256 must be 64 lowercase hexadecimal characters.",
        ));
    }
    Ok(())
}

fn validate_size(value: i64) -> Result<(), AppError> {
    if !(0..=MAX_ARTIFACT_BYTES).contains(&value) {
        return Err(AppError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "artifact_too_large",
            "Uploaded artifacts are limited to 16 MiB.",
        ));
    }
    Ok(())
}

fn retention_until(now: i64, pinned: bool, days: Option<i64>) -> Result<Option<i64>, AppError> {
    if pinned {
        if days.is_some() {
            return Err(AppError::bad_request(
                "retention_days must be omitted when pinned is true.",
            ));
        }
        return Ok(None);
    }
    let days = days.unwrap_or(DEFAULT_RETENTION_DAYS);
    if !(1..=MAX_RETENTION_DAYS).contains(&days) {
        return Err(AppError::bad_request(
            "retention_days must be between 1 and 3650.",
        ));
    }
    Ok(Some(now + days * 86_400_000))
}

fn validate_link_url(value: &str) -> Result<(), AppError> {
    bounded(value, "external_url", 4096, true)?;
    let parsed = url::Url::parse(value)
        .map_err(|_| AppError::bad_request("external_url must be a valid HTTPS URL."))?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(AppError::bad_request(
            "external_url must be an HTTPS URL without embedded credentials.",
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
struct Page {
    cursor: Option<String>,
    limit: Option<i64>,
}

impl Page {
    fn limit(&self) -> Result<i64, AppError> {
        let limit = self.limit.unwrap_or(50);
        if !(1..=200).contains(&limit)
            || self
                .cursor
                .as_deref()
                .is_some_and(|value| Uuid::parse_str(value).is_err())
        {
            return Err(AppError::bad_request(
                "limit must be between 1 and 200 and cursor must be an artifact UUID.",
            ));
        }
        Ok(limit)
    }
}

async fn project_exists(connection: &mut SqliteConnection, project: &str) -> Result<(), AppError> {
    if sqlx::query_scalar::<_, i64>("SELECT count(*) FROM projects WHERE id=?")
        .bind(project)
        .fetch_one(connection)
        .await?
        == 0
    {
        return Err(AppError::not_found());
    }
    Ok(())
}

async fn validate_associations(
    connection: &mut SqliteConnection,
    project: &str,
    task_id: Option<&str>,
    job_id: Option<&str>,
) -> Result<(), AppError> {
    if let Some(task) = task_id
        && (Uuid::parse_str(task).is_err()
            || sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM tasks WHERE project_id=? AND id=?",
            )
            .bind(project)
            .bind(task)
            .fetch_one(&mut *connection)
            .await?
                == 0)
    {
        return Err(AppError::conflict(
            "artifact_reference_invalid",
            "The referenced task does not exist in this project.",
        ));
    }
    if let Some(job) = job_id {
        if Uuid::parse_str(job).is_err() {
            return Err(AppError::conflict(
                "artifact_reference_invalid",
                "The referenced job does not exist in this project.",
            ));
        }
        let job_task =
            sqlx::query_scalar::<_, String>("SELECT task_id FROM jobs WHERE project_id=? AND id=?")
                .bind(project)
                .bind(job)
                .fetch_optional(&mut *connection)
                .await?
                .ok_or_else(|| {
                    AppError::conflict(
                        "artifact_reference_invalid",
                        "The referenced job does not exist in this project.",
                    )
                })?;
        if task_id.is_some_and(|task| task != job_task) {
            return Err(AppError::conflict(
                "artifact_reference_invalid",
                "The referenced job belongs to a different task.",
            ));
        }
    }
    Ok(())
}

pub(crate) fn store_root(state: &AppState) -> PathBuf {
    let path = &state.config.database_path;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("coordinator.sqlite3");
    path.with_file_name(format!("{name}.artifacts"))
}

pub(crate) fn storage_path(root: &FsPath, key: &str) -> Result<PathBuf, AppError> {
    let uuid = Uuid::parse_str(key).map_err(|_| AppError::internal())?;
    let canonical = uuid.to_string();
    Ok(root
        .join("blobs")
        .join(&canonical[..2])
        .join(format!("{canonical}.blob")))
}

fn staging_path(root: &FsPath, key: &str, suffix: &str) -> Result<PathBuf, AppError> {
    let uuid = Uuid::parse_str(key).map_err(|_| AppError::internal())?;
    Ok(root.join("staging").join(format!("{}.{suffix}", uuid)))
}

async fn ensure_store(root: &FsPath) -> Result<(), AppError> {
    if let Ok(metadata) = fs::symlink_metadata(root).await
        && (!metadata.is_dir() || metadata.file_type().is_symlink())
    {
        return Err(AppError::internal());
    }
    fs::create_dir_all(root.join("blobs"))
        .await
        .map_err(|_| AppError::internal())?;
    fs::create_dir_all(root.join("staging"))
        .await
        .map_err(|_| AppError::internal())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for directory in [root.to_owned(), root.join("blobs"), root.join("staging")] {
            let metadata = fs::symlink_metadata(&directory)
                .await
                .map_err(|_| AppError::internal())?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(AppError::internal());
            }
            fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
                .await
                .map_err(|_| AppError::internal())?;
        }
    }
    Ok(())
}

/// Keeps finalized artifact bytes stable while an online database snapshot is
/// assembled. Physical cleanup takes the corresponding exclusive lock and
/// defers rather than blocking a request when a backup is active.
pub(crate) struct ArtifactBackupGuard {
    _file: std::fs::File,
}

pub(crate) async fn acquire_backup_guard(
    state: &AppState,
) -> Result<ArtifactBackupGuard, AppError> {
    let root = store_root(state);
    ensure_store(&root).await?;
    let path = root.join(".gc.lock");
    tokio::task::spawn_blocking(move || {
        let file = open_gc_lock(&path)?;
        fs2::FileExt::lock_shared(&file).map_err(|_| AppError::internal())?;
        Ok(ArtifactBackupGuard { _file: file })
    })
    .await
    .map_err(|_| AppError::internal())?
}

async fn try_acquire_cleanup_guard(root: &FsPath) -> Result<Option<std::fs::File>, AppError> {
    let path = root.join(".gc.lock");
    tokio::task::spawn_blocking(move || {
        let file = open_gc_lock(&path)?;
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => Ok(Some(file)),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(_) => Err(AppError::internal()),
        }
    })
    .await
    .map_err(|_| AppError::internal())?
}

fn open_gc_lock(path: &FsPath) -> Result<std::fs::File, AppError> {
    if let Ok(metadata) = std::fs::symlink_metadata(path)
        && (!metadata.is_file() || metadata.file_type().is_symlink())
    {
        return Err(AppError::internal());
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path).map_err(|_| AppError::internal())?;
    let metadata = file.metadata().map_err(|_| AppError::internal())?;
    if !metadata.is_file() {
        return Err(AppError::internal());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|_| AppError::internal())?;
    }
    Ok(file)
}

fn check_disk(root: &FsPath, incoming: u64, reserve: u64) -> Result<(), AppError> {
    let free = fs2::available_space(root).map_err(|_| AppError::internal())?;
    if free < incoming.saturating_add(reserve) {
        return Err(AppError::new(
            StatusCode::INSUFFICIENT_STORAGE,
            "artifact_storage_low",
            "The artifact store does not have enough free space while preserving its disk reserve.",
        ));
    }
    Ok(())
}

async fn artifact_row(
    connection: &mut SqliteConnection,
    project: &str,
    artifact: &str,
) -> Result<sqlx::sqlite::SqliteRow, AppError> {
    if Uuid::parse_str(artifact).is_err() {
        return Err(AppError::not_found());
    }
    sqlx::query("SELECT * FROM artifacts WHERE project_id=? AND id=?")
        .bind(project)
        .bind(artifact)
        .fetch_optional(connection)
        .await?
        .ok_or_else(AppError::not_found)
}

fn availability(row: &sqlx::sqlite::SqliteRow, now: i64, root: &FsPath) -> &'static str {
    let state: String = row.get("state");
    if state == "deleted" {
        return "deleted";
    }
    if state == "reserved" {
        return if row
            .get::<Option<i64>, _>("reservation_expires_at")
            .is_some_and(|until| until <= now)
        {
            "expired"
        } else {
            "pending"
        };
    }
    if row.get::<i64, _>("pinned") == 0
        && row
            .get::<Option<i64>, _>("retention_until")
            .is_some_and(|until| until <= now)
    {
        return "expired";
    }
    if row.get::<String, _>("kind") == "upload" {
        let Some(key) = row.get::<Option<String>, _>("storage_key") else {
            return "unavailable";
        };
        if storage_path(root, &key).ok().is_none_or(|path| {
            std::fs::symlink_metadata(path)
                .ok()
                .is_none_or(|metadata| !metadata.is_file() || metadata.file_type().is_symlink())
        }) {
            return "unavailable";
        }
    }
    "available"
}

fn artifact_value(row: &sqlx::sqlite::SqliteRow, now: i64, root: &FsPath) -> Value {
    let id: String = row.get("id");
    let project: String = row.get("project_id");
    let kind: String = row.get("kind");
    let available = availability(row, now, root);
    json!({
        "id":id,
        "project_id":project,
        "kind":kind,
        "task_id":row.get::<Option<String>,_>("task_id"),
        "job_id":row.get::<Option<String>,_>("job_id"),
        "display_name":row.get::<String,_>("display_name"),
        "media_type":row.get::<String,_>("media_type"),
        "size_bytes":row.get::<Option<i64>,_>("size_bytes"),
        "sha256":row.get::<Option<String>,_>("sha256"),
        "external_url":row.get::<Option<String>,_>("external_url"),
        "state":row.get::<String,_>("state"),
        "availability":available,
        "created_by":row.get::<String,_>("created_by"),
        "created_at":timestamp(row.get("created_at")),
        "reservation_expires_at":row.get::<Option<i64>,_>("reservation_expires_at").map(timestamp),
        "finalized_at":row.get::<Option<i64>,_>("finalized_at").map(timestamp),
        "retention_until":row.get::<Option<i64>,_>("retention_until").map(timestamp),
        "pinned":row.get::<i64,_>("pinned") != 0,
        "deleted_at":row.get::<Option<i64>,_>("deleted_at").map(timestamp),
        "deletion_reason":row.get::<Option<String>,_>("deletion_reason"),
        "content_path":(kind == "upload" && available == "available")
            .then(|| format!("/api/v1/projects/{project}/artifacts/{id}/content")),
    })
}

async fn create_link(
    State(state): State<AppState>,
    auth: Auth,
    Path(project): Path<String>,
    headers: HeaderMap,
    body: Result<Json<ArtifactLinkInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    bounded(&input.display_name, "display_name", 255, true)?;
    validate_media_type(&input.media_type)?;
    validate_link_url(&input.external_url)?;
    if let Some(size) = input.size_bytes
        && size < 0
    {
        return Err(AppError::bad_request("size_bytes cannot be negative."));
    }
    if let Some(digest) = &input.sha256 {
        validate_sha256(digest)?;
    }
    let mut mutation = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{project}/artifacts"),
        &input,
    )
    .await?;
    if let Some(value) = mutation.replay {
        return Ok(response(value));
    }
    project_exists(&mut mutation.tx, &project).await?;
    validate_associations(
        &mut mutation.tx,
        &project,
        input.task_id.as_deref(),
        input.job_id.as_deref(),
    )
    .await?;
    let id = Uuid::new_v4().to_string();
    let retention = retention_until(mutation.now, input.pinned, input.retention_days)?;
    sqlx::query("INSERT INTO artifacts(id,project_id,kind,task_id,job_id,display_name,media_type,size_bytes,sha256,external_url,state,created_by,created_at,finalized_at,retention_until,pinned) VALUES(?,?,'external_link',?,?,?,?,?,?,?,'finalized',?,?,?,?,?)")
        .bind(&id).bind(&project).bind(&input.task_id).bind(&input.job_id)
        .bind(&input.display_name).bind(&input.media_type).bind(input.size_bytes)
        .bind(&input.sha256).bind(&input.external_url).bind(&mutation.actor.id)
        .bind(mutation.now).bind(mutation.now).bind(retention).bind(input.pinned)
        .execute(&mut *mutation.tx).await?;
    let row = artifact_row(&mut mutation.tx, &project, &id).await?;
    let value = json!({"artifact":artifact_value(&row, mutation.now, &store_root(&state))});
    Ok(response(
        mutation
            .finish(value, Some(&project), "artifact.linked", &id)
            .await?,
    ))
}

async fn reserve_upload(
    State(state): State<AppState>,
    auth: Auth,
    Path(project): Path<String>,
    headers: HeaderMap,
    body: Result<Json<ArtifactUploadInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    bounded(&input.filename, "filename", 255, true)?;
    validate_media_type(&input.media_type)?;
    validate_size(input.size_bytes)?;
    validate_sha256(&input.sha256)?;
    let root = store_root(&state);
    ensure_store(&root).await?;
    reconcile_store(&state).await?;
    check_disk(
        &root,
        input.size_bytes as u64,
        state.config.artifact_disk_reserve_bytes,
    )?;
    let mut mutation = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{project}/artifacts/uploads"),
        &input,
    )
    .await?;
    if let Some(value) = mutation.replay {
        return Ok(response(value));
    }
    project_exists(&mut mutation.tx, &project).await?;
    validate_associations(
        &mut mutation.tx,
        &project,
        input.task_id.as_deref(),
        input.job_id.as_deref(),
    )
    .await?;
    let used = sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(sum(size_bytes),0) FROM artifacts WHERE kind='upload' AND \
         ((state='reserved' AND reservation_expires_at>?) OR \
          (state='finalized' AND (pinned=1 OR retention_until>?)))",
    )
    .bind(mutation.now)
    .bind(mutation.now)
    .fetch_one(&mut *mutation.tx)
    .await?;
    if used.saturating_add(input.size_bytes) > state.config.artifact_quota_bytes {
        return Err(AppError::new(
            StatusCode::INSUFFICIENT_STORAGE,
            "artifact_quota_exceeded",
            "The configured live artifact quota would be exceeded. Delete or expire retained uploads before retrying.",
        ));
    }
    let id = Uuid::new_v4().to_string();
    let storage_key = Uuid::new_v4().to_string();
    let retention = retention_until(mutation.now, input.pinned, input.retention_days)?;
    sqlx::query("INSERT INTO artifacts(id,project_id,kind,task_id,job_id,display_name,media_type,size_bytes,sha256,storage_key,state,created_by,created_at,reservation_expires_at,retention_until,pinned) VALUES(?,?,'upload',?,?,?,?,?,?,?,'reserved',?,?,?,?,?)")
        .bind(&id).bind(&project).bind(&input.task_id).bind(&input.job_id)
        .bind(&input.filename).bind(&input.media_type).bind(input.size_bytes)
        .bind(&input.sha256).bind(&storage_key).bind(&mutation.actor.id)
        .bind(mutation.now).bind(mutation.now + RESERVATION_LIFETIME_MS)
        .bind(retention).bind(input.pinned).execute(&mut *mutation.tx).await?;
    let row = artifact_row(&mut mutation.tx, &project, &id).await?;
    let value = json!({
        "artifact":artifact_value(&row, mutation.now, &root),
        "upload_path":format!("/api/v1/projects/{project}/artifacts/{id}/content"),
    });
    Ok(response(
        mutation
            .finish(value, Some(&project), "artifact.upload_reserved", &id)
            .await?,
    ))
}

async fn list_artifacts(
    State(state): State<AppState>,
    _auth: Auth,
    Path(project): Path<String>,
    Query(page): Query<Page>,
) -> Reply {
    let limit = page.limit()?;
    let root = store_root(&state);
    ensure_store(&root).await?;
    let mut connection = state.pool.acquire().await?;
    project_exists(&mut connection, &project).await?;
    let usage = sqlx::query(
        "SELECT \
         COALESCE(sum(CASE WHEN state='reserved' AND reservation_expires_at>? THEN size_bytes ELSE 0 END),0) AS reserved_bytes, \
         COALESCE(sum(CASE WHEN state='finalized' AND (pinned=1 OR retention_until>?) THEN size_bytes ELSE 0 END),0) AS live_bytes \
         FROM artifacts WHERE kind='upload'",
    )
    .bind(state.now())
    .bind(state.now())
    .fetch_one(&mut *connection)
    .await?;
    let rows = sqlx::query(
        "SELECT * FROM artifacts WHERE project_id=? AND (? IS NULL OR id>?) ORDER BY id LIMIT ?",
    )
    .bind(&project)
    .bind(&page.cursor)
    .bind(&page.cursor)
    .bind(limit + 1)
    .fetch_all(&mut *connection)
    .await?;
    let items = rows
        .iter()
        .take(limit as usize)
        .map(|row| artifact_value(row, state.now(), &root))
        .collect::<Vec<_>>();
    let next = (rows.len() > limit as usize)
        .then(|| {
            items
                .last()
                .and_then(|item| item["id"].as_str())
                .map(str::to_owned)
        })
        .flatten();
    let reserved_bytes = usage.get::<i64, _>("reserved_bytes");
    let live_bytes = usage.get::<i64, _>("live_bytes");
    let disk_available_bytes = fs2::available_space(&root).map_err(|_| AppError::internal())?;
    Ok(response(json!({
        "items":items,
        "next_cursor":next,
        "storage":{
            "live_bytes":live_bytes,
            "reserved_bytes":reserved_bytes,
            "quota_used_bytes":live_bytes.saturating_add(reserved_bytes),
            "quota_bytes":state.config.artifact_quota_bytes,
            "max_artifact_bytes":MAX_ARTIFACT_BYTES,
            "disk_available_bytes":disk_available_bytes,
            "disk_reserve_bytes":state.config.artifact_disk_reserve_bytes,
            "default_retention_days":DEFAULT_RETENTION_DAYS,
        }
    })))
}

async fn artifact_detail(
    State(state): State<AppState>,
    _auth: Auth,
    Path((project, artifact)): Path<(String, String)>,
) -> Reply {
    let mut connection = state.pool.acquire().await?;
    project_exists(&mut connection, &project).await?;
    let row = artifact_row(&mut connection, &project, &artifact).await?;
    Ok(response(json!({
        "artifact":artifact_value(&row, state.now(), &store_root(&state))
    })))
}

fn require_upload_key(headers: &HeaderMap) -> Result<(), AppError> {
    let mut values = headers.get_all("Idempotency-Key").iter();
    let valid = values
        .next()
        .filter(|_| values.next().is_none())
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            !value.is_empty()
                && value.len() <= 128
                && value.bytes().all(|byte| byte.is_ascii_graphic())
        });
    if !valid {
        return Err(AppError::bad_request(
            "Provide exactly one Idempotency-Key of 1–128 visible ASCII characters.",
        ));
    }
    Ok(())
}

struct UploadLock(PathBuf);
impl Drop for UploadLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

async fn acquire_upload_lock(path: PathBuf) -> Result<UploadLock, AppError> {
    for attempt in 0..2 {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
        {
            Ok(mut file) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    file.set_permissions(std::fs::Permissions::from_mode(0o600))
                        .await
                        .map_err(|_| AppError::internal())?;
                }
                file.write_all(b"artifact upload lock\n")
                    .await
                    .map_err(|_| AppError::internal())?;
                file.sync_all().await.map_err(|_| AppError::internal())?;
                return Ok(UploadLock(path));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists && attempt == 0 => {
                let stale = fs::metadata(&path)
                    .await
                    .ok()
                    .and_then(|metadata| metadata.modified().ok())
                    .and_then(|modified| SystemTime::now().duration_since(modified).ok())
                    .is_some_and(|age| age > STALE_STAGING_AGE);
                if stale {
                    let _ = fs::remove_file(&path).await;
                    continue;
                }
                return Err(AppError::conflict(
                    "artifact_upload_in_progress",
                    "This artifact already has an upload in progress. Retry with the same saved request and key.",
                ));
            }
            Err(_) => return Err(AppError::internal()),
        }
    }
    Err(AppError::conflict(
        "artifact_upload_in_progress",
        "This artifact already has an upload in progress. Retry with the same saved request and key.",
    ))
}

#[derive(Serialize)]
struct UploadReceipt<'a> {
    artifact_id: &'a str,
    size_bytes: i64,
    sha256: &'a str,
}

async fn upload(
    State(state): State<AppState>,
    auth: Auth,
    Path((project, artifact)): Path<(String, String)>,
    headers: HeaderMap,
    body: Body,
) -> Reply {
    require_upload_key(&headers)?;
    let permit = tokio::time::timeout(
        LOCK_WAIT,
        UPLOAD_LIMIT.get_or_init(|| Semaphore::new(4)).acquire(),
    )
    .await
    .map_err(|_| {
        AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "artifact_upload_busy",
            "All upload slots are busy. Retry with the same saved request and key.",
        )
    })?
    .map_err(|_| AppError::internal())?;
    let root = store_root(&state);
    ensure_store(&root).await?;
    let mut connection = state.pool.acquire().await?;
    let row = artifact_row(&mut connection, &project, &artifact).await?;
    let kind: String = row.get("kind");
    let expected_size: i64 = row.get::<Option<i64>, _>("size_bytes").unwrap_or(-1);
    let expected_digest: String = row.get::<Option<String>, _>("sha256").unwrap_or_default();
    let storage_key: String = row
        .get::<Option<String>, _>("storage_key")
        .unwrap_or_default();
    let creator: String = row.get("created_by");
    let state_before: String = row.get("state");
    let availability_before = availability(&row, state.now(), &root);
    let reservation_expires: Option<i64> = row.get("reservation_expires_at");
    drop(connection);
    if kind != "upload" {
        return Err(AppError::conflict(
            "artifact_has_no_upload",
            "External-link artifacts do not accept uploaded content.",
        ));
    }
    if creator != auth.actor.id {
        return Err(AppError::forbidden(
            "Only the principal that reserved this upload may provide its content.",
        ));
    }
    if matches!(availability_before, "expired" | "deleted" | "unavailable")
        || state_before == "deleted"
        || (state_before == "reserved"
            && reservation_expires.is_none_or(|until| until <= state.now()))
    {
        return Err(AppError::new(
            StatusCode::GONE,
            "artifact_unavailable",
            "The upload reservation was deleted or expired.",
        ));
    }
    if let Some(length) = headers.get(header::CONTENT_LENGTH) {
        let length = length
            .to_str()
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
            .ok_or_else(|| AppError::bad_request("Content-Length must be a byte count."))?;
        if length != expected_size {
            return Err(AppError::conflict(
                "artifact_size_mismatch",
                "Content-Length does not match the reserved artifact size.",
            ));
        }
    }
    check_disk(
        &root,
        expected_size as u64,
        state.config.artifact_disk_reserve_bytes,
    )?;
    let lock_path = staging_path(&root, &storage_key, "lock")?;
    let _upload_lock = acquire_upload_lock(lock_path).await?;
    let part_path = staging_path(&root, &storage_key, "part")?;
    let final_path = storage_path(&root, &storage_key)?;
    if let Some(parent) = final_path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|_| AppError::internal())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = fs::symlink_metadata(parent)
                .await
                .map_err(|_| AppError::internal())?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(AppError::internal());
            }
            fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                .await
                .map_err(|_| AppError::internal())?;
        }
    }
    let stream_result = tokio::time::timeout(UPLOAD_TIMEOUT, async {
        match fs::symlink_metadata(&part_path).await {
            Ok(_) => fs::remove_file(&part_path)
                .await
                .map_err(|_| AppError::internal())?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(AppError::internal()),
        }
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&part_path)
            .await
            .map_err(|_| AppError::internal())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .await
                .map_err(|_| AppError::internal())?;
        }
        let mut received = 0_i64;
        let mut hasher = Sha256::new();
        let mut body = body;
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|_| {
                AppError::bad_request("The artifact request body could not be read completely.")
            })?;
            if let Ok(data) = frame.into_data() {
                received = received.saturating_add(data.len() as i64);
                if received > expected_size || received > MAX_ARTIFACT_BYTES {
                    return Err(AppError::new(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "artifact_too_large",
                        "The uploaded bytes exceed the reserved artifact size or 16 MiB limit.",
                    ));
                }
                hasher.update(&data);
                {
                    // Serialize the free-space check with artifact writes so
                    // concurrent uploads cannot each consume the disk reserve.
                    let _disk = DISK_WRITE_LOCK.get_or_init(|| Mutex::new(())).lock().await;
                    check_disk(
                        &root,
                        data.len() as u64,
                        state.config.artifact_disk_reserve_bytes,
                    )?;
                    file.write_all(&data)
                        .await
                        .map_err(|_| AppError::internal())?;
                }
            }
        }
        if received != expected_size {
            return Err(AppError::conflict(
                "artifact_size_mismatch",
                "The uploaded bytes do not match the reserved artifact size.",
            ));
        }
        let digest = hex::encode(hasher.finalize());
        if digest != expected_digest {
            return Err(AppError::conflict(
                "artifact_digest_mismatch",
                "The uploaded bytes do not match the reserved SHA-256 digest.",
            ));
        }
        file.sync_all().await.map_err(|_| AppError::internal())?;
        Ok::<_, AppError>((received, digest))
    })
    .await;
    let (received, digest) = match stream_result {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => {
            let _ = fs::remove_file(&part_path).await;
            return Err(error);
        }
        Err(_) => {
            let _ = fs::remove_file(&part_path).await;
            return Err(AppError::new(
                StatusCode::REQUEST_TIMEOUT,
                "artifact_upload_timeout",
                "The artifact upload did not finish within two minutes. Retry with the same saved bytes, digest, and key.",
            ));
        }
    };
    match fs::symlink_metadata(&final_path).await {
        Ok(metadata) => {
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                let _ = fs::remove_file(&part_path).await;
                return Err(AppError::internal());
            }
            let (existing_size, existing_digest) = hash_file(&final_path).await?;
            if existing_size != received || existing_digest != digest {
                let _ = fs::remove_file(&part_path).await;
                return Err(AppError::conflict(
                    "artifact_content_conflict",
                    "Durable content for this reservation differs from the saved upload. Do not replace the reservation.",
                ));
            }
            fs::remove_file(&part_path)
                .await
                .map_err(|_| AppError::internal())?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::rename(&part_path, &final_path)
                .await
                .map_err(|_| AppError::internal())?;
            sync_directory(final_path.parent().ok_or_else(AppError::internal)?).await?;
        }
        Err(_) => {
            let _ = fs::remove_file(&part_path).await;
            return Err(AppError::internal());
        }
    }
    let receipt = UploadReceipt {
        artifact_id: &artifact,
        size_bytes: received,
        sha256: &digest,
    };
    let mut mutation = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("PUT /api/v1/projects/{project}/artifacts/{artifact}/content"),
        &receipt,
    )
    .await?;
    if let Some(value) = mutation.replay {
        drop(permit);
        return Ok(response(value));
    }
    let current = artifact_row(&mut mutation.tx, &project, &artifact).await?;
    if current.get::<String, _>("created_by") != mutation.actor.id {
        return Err(AppError::forbidden(
            "Only the principal that reserved this upload may finalize it.",
        ));
    }
    if current.get::<String, _>("state") != "reserved"
        || current
            .get::<Option<i64>, _>("reservation_expires_at")
            .is_none_or(|until| until <= mutation.now)
        || current.get::<Option<i64>, _>("size_bytes") != Some(received)
        || current.get::<Option<String>, _>("sha256").as_deref() != Some(&digest)
    {
        return Err(AppError::conflict(
            "artifact_reservation_changed",
            "The upload reservation is no longer live or no longer matches these bytes.",
        ));
    }
    sqlx::query("UPDATE artifacts SET state='finalized',finalized_at=?,reservation_expires_at=NULL WHERE id=? AND state='reserved'")
        .bind(mutation.now).bind(&artifact).execute(&mut *mutation.tx).await?;
    let finalized = artifact_row(&mut mutation.tx, &project, &artifact).await?;
    let value = json!({"artifact":artifact_value(&finalized, mutation.now, &root)});
    let value = mutation
        .finish(
            value,
            Some(&project),
            "artifact.upload_finalized",
            &artifact,
        )
        .await?;
    drop(permit);
    Ok(response(value))
}

async fn hash_file(path: &FsPath) -> Result<(i64, String), AppError> {
    let mut file = fs::File::open(path)
        .await
        .map_err(|_| AppError::internal())?;
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut size = 0_i64;
    let mut hasher = Sha256::new();
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|_| AppError::internal())?;
        if read == 0 {
            break;
        }
        size = size.saturating_add(read as i64);
        if size > MAX_ARTIFACT_BYTES {
            return Err(AppError::conflict(
                "artifact_content_conflict",
                "Durable content for this reservation exceeds the artifact limit.",
            ));
        }
        hasher.update(&buffer[..read]);
    }
    Ok((size, hex::encode(hasher.finalize())))
}

async fn sync_directory(path: &FsPath) -> Result<(), AppError> {
    #[cfg(unix)]
    {
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || std::fs::File::open(path)?.sync_all())
            .await
            .map_err(|_| AppError::internal())?
            .map_err(|_| AppError::internal())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

async fn download(
    State(state): State<AppState>,
    _auth: Auth,
    Path((project, artifact)): Path<(String, String)>,
) -> Result<Response, AppError> {
    let mut connection = state.pool.acquire().await?;
    project_exists(&mut connection, &project).await?;
    let row = artifact_row(&mut connection, &project, &artifact).await?;
    let root = store_root(&state);
    match availability(&row, state.now(), &root) {
        "pending" => {
            return Err(AppError::conflict(
                "artifact_not_finalized",
                "Upload content and finalize this reservation before downloading it.",
            ));
        }
        "available" => {}
        _ => {
            return Err(AppError::new(
                StatusCode::GONE,
                "artifact_unavailable",
                "The artifact content is expired, deleted, or unavailable.",
            ));
        }
    }
    if row.get::<String, _>("kind") != "upload" {
        return Err(AppError::conflict(
            "artifact_is_external_link",
            "This artifact is an external link; use its authenticated metadata URL.",
        ));
    }
    let storage_key: String = row.get("storage_key");
    let path = storage_path(&root, &storage_key)?;
    let metadata = fs::symlink_metadata(&path).await.map_err(|_| {
        AppError::new(
            StatusCode::GONE,
            "artifact_unavailable",
            "The artifact content is unavailable from this service.",
        )
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(AppError::new(
            StatusCode::GONE,
            "artifact_unavailable",
            "The artifact content is unavailable from this service.",
        ));
    }
    let file = fs::File::open(path).await.map_err(|_| {
        AppError::new(
            StatusCode::GONE,
            "artifact_unavailable",
            "The artifact content is unavailable from this service.",
        )
    })?;
    let name = download_name(&artifact, &row.get::<String, _>("display_name"));
    let media_type: String = row.get("media_type");
    let size: i64 = row.get("size_bytes");
    let body = Body::from_stream(ReaderStream::new(file));
    let mut result = Response::new(body);
    result.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&media_type).map_err(|_| AppError::internal())?,
    );
    result.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{name}\""))
            .map_err(|_| AppError::internal())?,
    );
    result.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&size.to_string()).map_err(|_| AppError::internal())?,
    );
    result.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    Ok(result)
}

fn download_name(id: &str, display_name: &str) -> String {
    let safe = display_name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .take(100)
        .collect::<String>();
    let safe = if safe.trim_matches('_').is_empty() {
        "artifact".to_owned()
    } else {
        safe
    };
    format!("{}-{safe}", &id[..8])
}

async fn update_retention(
    State(state): State<AppState>,
    auth: Auth,
    Path((project, artifact)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<ArtifactRetentionInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    ensure_store(&store_root(&state)).await?;
    let storage_key = artifact_storage_key(&state, &project, &artifact).await?;
    let _artifact_lock = match storage_key {
        Some(ref key) => {
            Some(acquire_upload_lock(staging_path(&store_root(&state), key, "lock")?).await?)
        }
        None => None,
    };
    let mut mutation = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{project}/artifacts/{artifact}/retention"),
        &input,
    )
    .await?;
    if let Some(value) = mutation.replay {
        return Ok(response(value));
    }
    let row = artifact_row(&mut mutation.tx, &project, &artifact).await?;
    authorize_change(&mutation, &row)?;
    if row.get::<String, _>("state") == "deleted" {
        return Err(AppError::new(
            StatusCode::GONE,
            "artifact_deleted",
            "Deleted artifact metadata cannot be retained again.",
        ));
    }
    let until = retention_until(mutation.now, input.pinned, input.retention_days)?;
    sqlx::query("UPDATE artifacts SET pinned=?,retention_until=? WHERE id=?")
        .bind(input.pinned)
        .bind(until)
        .bind(&artifact)
        .execute(&mut *mutation.tx)
        .await?;
    let updated = artifact_row(&mut mutation.tx, &project, &artifact).await?;
    let value = json!({"artifact":artifact_value(&updated, mutation.now, &store_root(&state))});
    Ok(response(
        mutation
            .finish(
                value,
                Some(&project),
                "artifact.retention_updated",
                &artifact,
            )
            .await?,
    ))
}

fn authorize_change(mutation: &Mutation, row: &sqlx::sqlite::SqliteRow) -> Result<(), AppError> {
    if mutation.actor.kind != "human" && row.get::<String, _>("created_by") != mutation.actor.id {
        return Err(AppError::forbidden(
            "Only the artifact author or a human operator may change retention or delete it.",
        ));
    }
    Ok(())
}

async fn delete_artifact(
    State(state): State<AppState>,
    auth: Auth,
    Path((project, artifact)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<ArtifactDeleteInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    bounded(&input.reason, "reason", 4096, true)?;
    ensure_store(&store_root(&state)).await?;
    let storage_key = artifact_storage_key(&state, &project, &artifact).await?;
    let _artifact_lock = match storage_key {
        Some(ref key) => {
            Some(acquire_upload_lock(staging_path(&store_root(&state), key, "lock")?).await?)
        }
        None => None,
    };
    let mut mutation = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{project}/artifacts/{artifact}/delete"),
        &input,
    )
    .await?;
    if let Some(value) = mutation.replay.clone() {
        drop(mutation);
        if let Some(key) = storage_key.as_deref() {
            cleanup_artifact_files(&state, key).await?;
        }
        return Ok(response(value));
    }
    let row = artifact_row(&mut mutation.tx, &project, &artifact).await?;
    authorize_change(&mutation, &row)?;
    if row.get::<String, _>("state") == "deleted" {
        return Err(AppError::conflict(
            "artifact_already_deleted",
            "This artifact already has a retained deletion tombstone.",
        ));
    }
    sqlx::query("UPDATE artifacts SET state='deleted',deleted_at=?,deleted_by=?,deletion_reason=? WHERE id=?")
        .bind(mutation.now).bind(&mutation.actor.id).bind(&input.reason).bind(&artifact)
        .execute(&mut *mutation.tx).await?;
    let updated = artifact_row(&mut mutation.tx, &project, &artifact).await?;
    let value = json!({"artifact":artifact_value(&updated, mutation.now, &store_root(&state))});
    let value = mutation
        .finish(value, Some(&project), "artifact.deleted", &artifact)
        .await?;
    if let Some(key) = storage_key.as_deref() {
        cleanup_artifact_files(&state, key).await?;
    }
    Ok(response(value))
}

async fn artifact_storage_key(
    state: &AppState,
    project: &str,
    artifact: &str,
) -> Result<Option<String>, AppError> {
    let mut connection = state.pool.acquire().await?;
    let row = artifact_row(&mut connection, project, artifact).await?;
    Ok(row.get("storage_key"))
}

async fn cleanup_artifact_files(state: &AppState, key: &str) -> Result<(), AppError> {
    let root = store_root(state);
    let Some(_cleanup_guard) = try_acquire_cleanup_guard(&root).await? else {
        return Ok(());
    };
    for path in [storage_path(&root, key)?, staging_path(&root, key, "part")?] {
        match fs::remove_file(path).await {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(AppError::internal()),
        }
    }
    Ok(())
}

/// Verifies that submission references are same-project, finalized, and live.
pub async fn validate_submission_artifacts(
    connection: &mut SqliteConnection,
    project: &str,
    ids: &[String],
    now: i64,
) -> Result<(), AppError> {
    if ids.len() > 100 {
        return Err(AppError::bad_request(
            "A submission may reference at most 100 artifacts.",
        ));
    }
    let mut unique = HashSet::new();
    for id in ids {
        if Uuid::parse_str(id).is_err() || !unique.insert(id) {
            return Err(AppError::bad_request(
                "Artifact references must be unique artifact UUIDs.",
            ));
        }
        let valid = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM artifacts WHERE project_id=? AND id=? AND state='finalized' \
             AND (pinned=1 OR retention_until>?)",
        )
        .bind(project)
        .bind(id)
        .bind(now)
        .fetch_one(&mut *connection)
        .await?;
        if valid == 0 {
            return Err(AppError::conflict(
                "submission_artifact_unavailable",
                "Every submission artifact must be finalized, live, and belong to this project.",
            ));
        }
    }
    Ok(())
}

/// Adds immutable submission references after validation in the caller's mutation transaction.
pub async fn link_submission_artifacts(
    connection: &mut SqliteConnection,
    project: &str,
    submission: &str,
    ids: &[String],
    now: i64,
) -> Result<(), AppError> {
    for id in ids {
        sqlx::query("INSERT INTO submission_artifacts(project_id,submission_id,artifact_id,linked_at) VALUES(?,?,?,?)")
            .bind(project).bind(submission).bind(id).bind(now)
            .execute(&mut *connection).await?;
    }
    Ok(())
}

/// Performs a bounded cleanup pass while keeping artifact tombstones.
pub async fn reconcile_store(state: &AppState) -> Result<(), AppError> {
    let root = store_root(state);
    ensure_store(&root).await?;
    let now = state.now();
    let cursor = {
        let cursors = CLEANUP_CURSORS
            .get_or_init(|| std::sync::Mutex::new(CleanupCursors::default()))
            .lock()
            .map_err(|_| AppError::internal())?;
        cursors.expired.get(&root).cloned().unwrap_or_default()
    };
    let rows = sqlx::query(
        "SELECT id,storage_key FROM artifacts WHERE kind='upload' AND id>? AND \
         (state='deleted' OR (state='reserved' AND reservation_expires_at<=?) OR \
          (state='finalized' AND pinned=0 AND retention_until<=?)) ORDER BY id LIMIT 100",
    )
    .bind(&cursor)
    .bind(now)
    .bind(now)
    .fetch_all(&state.pool)
    .await?;
    let next = rows.last().map(|row| row.get::<String, _>("id"));
    for row in rows {
        cleanup_if_still_unavailable(state, &row.get::<String, _>("storage_key"), now).await?;
    }
    {
        let mut cursors = CLEANUP_CURSORS
            .get_or_init(|| std::sync::Mutex::new(CleanupCursors::default()))
            .lock()
            .map_err(|_| AppError::internal())?;
        if let Some(next) = next {
            cursors.expired.insert(root.clone(), next);
        } else {
            cursors.expired.remove(&root);
        }
    }
    cleanup_orphan_prefix(state, &root).await?;
    let staging = root.join("staging");
    if let Ok(mut files) = fs::read_dir(staging).await {
        let mut inspected = 0;
        while inspected < 100 {
            let Some(file) = files.next_entry().await.map_err(|_| AppError::internal())? else {
                break;
            };
            inspected += 1;
            let old = file
                .metadata()
                .await
                .ok()
                .and_then(|metadata| metadata.modified().ok())
                .and_then(|modified| SystemTime::now().duration_since(modified).ok())
                .is_some_and(|age| age > STALE_STAGING_AGE);
            if old {
                let _ = fs::remove_file(file.path()).await;
            }
        }
    }
    Ok(())
}

async fn cleanup_if_still_unavailable(
    state: &AppState,
    key: &str,
    now: i64,
) -> Result<(), AppError> {
    let lock = match acquire_upload_lock(staging_path(&store_root(state), key, "lock")?).await {
        Ok(lock) => lock,
        Err(error) if error.code == "artifact_upload_in_progress" => return Ok(()),
        Err(error) => return Err(error),
    };
    let unavailable = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM artifacts WHERE storage_key=? AND \
         (state='deleted' OR (state='reserved' AND reservation_expires_at<=?) OR \
          (state='finalized' AND pinned=0 AND retention_until<=?))",
    )
    .bind(key)
    .bind(now)
    .bind(now)
    .fetch_one(&state.pool)
    .await?
        != 0;
    if unavailable {
        cleanup_artifact_files(state, key).await?;
    }
    drop(lock);
    Ok(())
}

async fn cleanup_orphan_prefix(state: &AppState, root: &FsPath) -> Result<(), AppError> {
    let prefix = {
        let mut cursors = CLEANUP_CURSORS
            .get_or_init(|| std::sync::Mutex::new(CleanupCursors::default()))
            .lock()
            .map_err(|_| AppError::internal())?;
        let next = cursors.orphan_prefix.entry(root.to_owned()).or_insert(0);
        let current = *next;
        *next = next.wrapping_add(1);
        current
    };
    let directory = root.join("blobs").join(format!("{prefix:02x}"));
    let Ok(mut files) = fs::read_dir(directory).await else {
        return Ok(());
    };
    let mut inspected = 0;
    while inspected < 100 {
        let Some(file) = files.next_entry().await.map_err(|_| AppError::internal())? else {
            break;
        };
        inspected += 1;
        let old = file
            .metadata()
            .await
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age > STALE_STAGING_AGE);
        if !old {
            continue;
        }
        let name = file.file_name().to_string_lossy().to_string();
        let Some(key) = name.strip_suffix(".blob") else {
            continue;
        };
        if Uuid::parse_str(key).is_err() {
            continue;
        }
        let lock = match acquire_upload_lock(staging_path(root, key, "lock")?).await {
            Ok(lock) => lock,
            Err(error) if error.code == "artifact_upload_in_progress" => continue,
            Err(error) => return Err(error),
        };
        let referenced =
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM artifacts WHERE storage_key=?")
                .bind(key)
                .fetch_one(&state.pool)
                .await?
                != 0;
        if !referenced {
            cleanup_artifact_files(state, key).await?;
        }
        drop(lock);
    }
    Ok(())
}
