//! Crash-safe native artifact upload and download helpers.
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use coordinator_client::{
    ApiResponse, ClientError, CoordinatorClient, DownloadResponse, MAX_RESPONSE_BYTES, SessionAuth,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use uuid::Uuid;

use crate::config::coordinator_home;

pub struct TransferContext<'a> {
    pub client: &'a CoordinatorClient,
    pub service_origin: &'a str,
    pub project_id: &'a str,
    pub local_session: &'a str,
    pub session: &'a SessionAuth,
}

pub fn failure_is_temporary(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<ClientError>(),
        Some(ClientError::Transport(_) | ClientError::InvalidResponse(_))
    )
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct UploadIntent {
    version: u8,
    service_origin: String,
    project_id: String,
    local_session: String,
    artifact_id: String,
    source: PathBuf,
    payload: PathBuf,
    size_bytes: u64,
    sha256: String,
    idempotency_key: String,
    completed_response: Option<ApiResponse>,
}

struct UploadPaths {
    directory: PathBuf,
    state: PathBuf,
    payload: PathBuf,
    lock: PathBuf,
}

/// Uploads an explicit reservation. The first invocation snapshots the source
/// into protected local state before any PUT. Every retry uses that exact copy.
pub async fn upload(
    context: &TransferContext<'_>,
    artifact_id: &str,
    source: &Path,
) -> Result<ApiResponse> {
    validate_artifact_id(artifact_id)?;
    let paths = upload_paths(context, artifact_id)?;
    upload_at(context, artifact_id, source, paths).await
}

async fn upload_at(
    context: &TransferContext<'_>,
    artifact_id: &str,
    source: &Path,
    paths: UploadPaths,
) -> Result<ApiResponse> {
    fs::create_dir_all(&paths.directory)
        .with_context(|| format!("create artifact journal {}", paths.directory.display()))?;
    protect_directory(&paths.directory)?;
    let _lock = lock(&paths.lock)?;

    let (mut intent, initial_detail) = match load(&paths.state)? {
        Some(intent) => {
            let source = source_reference(source)?;
            validate_saved(context, artifact_id, &source, &intent)?;
            (intent, None)
        }
        None => {
            let source = fs::canonicalize(source)
                .with_context(|| format!("resolve artifact source {}", source.display()))?;
            let (size_bytes, sha256) = snapshot(&source, &paths.payload, &paths.directory)?;
            let detail_path = format!(
                "/api/v1/projects/{}/artifacts/{artifact_id}",
                context.project_id
            );
            let detail = context
                .client
                .get(&detail_path, Some(context.session))
                .await
                .context("inspect artifact upload reservation")?;
            if !detail.is_success() {
                let _ = fs::remove_file(&paths.payload);
                return Ok(detail);
            }
            if let Err(error) = inspect_upload(&detail, size_bytes, &sha256) {
                let _ = fs::remove_file(&paths.payload);
                return Err(error);
            }
            let intent = UploadIntent {
                version: 1,
                service_origin: context.service_origin.to_owned(),
                project_id: context.project_id.to_owned(),
                local_session: context.local_session.to_owned(),
                artifact_id: artifact_id.to_owned(),
                source,
                payload: paths.payload.clone(),
                size_bytes,
                sha256,
                idempotency_key: Uuid::new_v4().to_string(),
                completed_response: None,
            };
            save(&paths.state, &intent, &paths.directory)?;
            (intent, Some(detail))
        }
    };
    if intent.completed_response.is_some() {
        return refresh_completed(context, artifact_id, &intent, &paths).await;
    }
    let detail = match initial_detail {
        Some(detail) => detail,
        None => authenticated_detail(context, artifact_id).await?,
    };
    if !detail.is_success() {
        return Ok(detail);
    }
    match inspect_upload(&detail, intent.size_bytes, &intent.sha256)? {
        UploadState::Pending => {}
        UploadState::Finalized => {
            intent.completed_response = Some(detail.clone());
            save(&paths.state, &intent, &paths.directory)?;
            cleanup_payload(&paths)?;
            return Ok(detail);
        }
    }
    validate_payload(&intent)?;
    let path = format!(
        "/api/v1/projects/{}/artifacts/{artifact_id}/content",
        context.project_id
    );
    let response = context
        .client
        .upload_file(
            &path,
            &intent.payload,
            intent.size_bytes,
            &intent.idempotency_key,
            Some(context.session),
        )
        .await
        .context("stream artifact upload")?;
    if response.is_success() {
        intent.completed_response = Some(response.clone());
        save(&paths.state, &intent, &paths.directory)?;
        cleanup_payload(&paths)?;
    }
    Ok(response)
}

#[derive(Deserialize, Serialize)]
struct PublicationIntent {
    version: u8,
    service_origin: String,
    project_id: String,
    local_session: String,
    source: PathBuf,
    request: serde_json::Value,
    reservation_key: String,
    artifact_id: Option<String>,
}

/// The caller holds the native session lock. Publication journals live in the
/// protected native artifact directory, outside validated session storage.
/// Save exact bytes and reservation intent before dispatch. A lost reservation
/// response reuses its key; a lost upload response uses the existing transfer.
pub async fn publish(
    context: &TransferContext<'_>,
    publication_id: &str,
    source: &Path,
    request: serde_json::Value,
) -> Result<ApiResponse> {
    validate_artifact_id(publication_id)?;
    let paths = upload_paths(context, &format!("publication:{publication_id}"))?;
    publish_at(context, source, request, &paths.directory).await
}

async fn publish_at(
    context: &TransferContext<'_>,
    source: &Path,
    request: serde_json::Value,
    directory: &Path,
) -> Result<ApiResponse> {
    let input: coordinator_core::ArtifactUploadInput =
        serde_json::from_value(request.clone()).context("parse publication metadata")?;
    let task = input
        .task_id
        .as_deref()
        .ok_or_else(|| anyhow!("publication requires task_id"))?;
    let job = input
        .job_id
        .as_deref()
        .ok_or_else(|| anyhow!("publication requires producer job_id"))?;
    validate_artifact_id(task)?;
    validate_artifact_id(job)?;
    let source = source_reference(source)?;
    create_private_directory(directory)?;
    protect_directory(directory)?;
    let state_path = directory.join("publication.json");
    let payload = directory.join("report.bin");
    let mut intent: PublicationIntent = match fs::read(&state_path) {
        Ok(bytes) => serde_json::from_slice(&bytes).context("read publication intent")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let (size, digest) = snapshot(&source, &payload, directory)?;
            if input.size_bytes < 0 || size != input.size_bytes as u64 || digest != input.sha256 {
                bail!("publication source does not match reservation size and SHA-256");
            }
            let intent = PublicationIntent {
                version: 1,
                service_origin: context.service_origin.into(),
                project_id: context.project_id.into(),
                local_session: context.local_session.into(),
                source: source.clone(),
                request: request.clone(),
                reservation_key: Uuid::new_v4().to_string(),
                artifact_id: None,
            };
            save(&state_path, &intent, directory)?;
            intent
        }
        Err(error) => return Err(error).context("read publication intent"),
    };
    if intent.version != 1
        || intent.service_origin != context.service_origin
        || intent.project_id != context.project_id
        || intent.local_session != context.local_session
        || intent.source != source
        || intent.request != request
    {
        bail!(
            "publication identity already belongs to a different request; reuse the original arguments"
        );
    }
    // Retain the independent publication snapshot even after successful transfer:
    // the operator decides its retention after verifying service-hosted evidence.
    let (size, digest) = hash_file(&payload)?;
    if input.size_bytes < 0 || size != input.size_bytes as u64 || digest != input.sha256 {
        bail!("saved publication bytes do not match their reservation");
    }
    if intent.artifact_id.is_none() {
        let path = format!("/api/v1/projects/{}/artifacts/uploads", context.project_id);
        let response = context
            .client
            .mutate(
                &path,
                &intent.request,
                &intent.reservation_key,
                Some(context.session),
            )
            .await
            .context("reserve publication artifact")?;
        if !response.is_success() {
            return Ok(response);
        }
        let id = artifact_metadata(&response)?
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow!("reservation omitted artifact ID"))?;
        validate_artifact_id(id)?;
        validate_same_upload(&response, size, &digest)?;
        intent.artifact_id = Some(id.into());
        save(&state_path, &intent, directory)?;
    }
    let id = intent.artifact_id.as_deref().expect("saved reservation");
    let transfer_directory = directory.join("upload");
    let response = upload_at(
        context,
        id,
        &payload,
        UploadPaths {
            state: transfer_directory.join("intent.json"),
            payload: transfer_directory.join("payload.bin"),
            lock: transfer_directory.join("intent.lock"),
            directory: transfer_directory,
        },
    )
    .await?;
    if !response.is_success() {
        return Ok(response);
    }
    // A receipt is not evidence of present availability. Authenticate a fresh
    // metadata read, and never return publication success for pending/tombstoned bytes.
    let current = authenticated_detail(context, id).await?;
    if !current.is_success() {
        return Ok(current);
    }
    let (current_size, current_digest) = download_expectation(&current)?;
    if current_size != size || current_digest != digest {
        bail!("published artifact differs from retained report");
    }
    let artifact = artifact_metadata(&current)?;
    if artifact.get("task_id").and_then(serde_json::Value::as_str) != Some(task)
        || artifact.get("job_id").and_then(serde_json::Value::as_str) != Some(job)
    {
        bail!("published artifact has different task or producer job provenance");
    }
    Ok(current)
}

fn create_private_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                bail!("publication storage must be a directory, not a link");
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .ok_or_else(|| anyhow!("publication directory has no parent"))?;
            create_private_directory(parent)?;
            fs::create_dir(path).context("create publication directory")?;
            protect_directory(path)?;
            sync_directory(parent)?;
        }
        Err(error) => return Err(error).context("inspect publication directory"),
    }
    Ok(())
}

fn source_reference(source: &Path) -> Result<PathBuf> {
    if let Ok(canonical) = fs::canonicalize(source) {
        return Ok(canonical);
    }
    let absolute = if source.is_absolute() {
        source.to_owned()
    } else {
        std::env::current_dir()
            .context("read current directory")?
            .join(source)
    };
    let name = absolute
        .file_name()
        .ok_or_else(|| anyhow!("artifact source has no filename"))?;
    if let Some(parent) = absolute.parent()
        && let Ok(parent) = fs::canonicalize(parent)
    {
        return Ok(parent.join(name));
    }
    Ok(absolute)
}

/// Downloads an explicit artifact to an explicit new file. Existing directory
/// entries, including symlinks, are never replaced.
pub async fn download(
    context: &TransferContext<'_>,
    artifact_id: &str,
    output: &Path,
) -> Result<DownloadResponse> {
    validate_artifact_id(artifact_id)?;
    if fs::symlink_metadata(output).is_ok() {
        bail!("download output already exists: {}", output.display());
    }
    let detail = authenticated_detail(context, artifact_id).await?;
    if !detail.is_success() {
        return Ok(DownloadResponse::Api(detail));
    }
    let (expected_size, expected_sha256) = download_expectation(&detail)?;
    let path = format!(
        "/api/v1/projects/{}/artifacts/{artifact_id}/content",
        context.project_id
    );
    context
        .client
        .download_to_path(
            &path,
            output,
            expected_size,
            &expected_sha256,
            Some(context.session),
        )
        .await
        .context("stream artifact download")
}

async fn authenticated_detail(
    context: &TransferContext<'_>,
    artifact_id: &str,
) -> Result<ApiResponse> {
    let path = format!(
        "/api/v1/projects/{}/artifacts/{artifact_id}",
        context.project_id
    );
    context
        .client
        .get(&path, Some(context.session))
        .await
        .context("inspect current artifact metadata")
}

async fn refresh_completed(
    context: &TransferContext<'_>,
    artifact_id: &str,
    intent: &UploadIntent,
    paths: &UploadPaths,
) -> Result<ApiResponse> {
    let detail = authenticated_detail(context, artifact_id).await?;
    if detail.is_success() {
        validate_same_upload(&detail, intent.size_bytes, &intent.sha256)?;
    }
    cleanup_payload(paths)?;
    Ok(detail)
}

fn validate_artifact_id(value: &str) -> Result<()> {
    if Uuid::parse_str(value).is_err() {
        bail!("artifact ID must be a UUID");
    }
    Ok(())
}

fn upload_paths(context: &TransferContext<'_>, artifact_id: &str) -> Result<UploadPaths> {
    let mut hasher = Sha256::new();
    for value in [
        context.service_origin,
        context.project_id,
        context.local_session,
        artifact_id,
    ] {
        hasher.update(value.as_bytes());
        hasher.update([0]);
    }
    let directory = coordinator_home()?
        .join("artifact-uploads")
        .join(hex::encode(hasher.finalize()));
    Ok(UploadPaths {
        state: directory.join("intent.json"),
        payload: directory.join("payload.bin"),
        lock: directory.join("intent.lock"),
        directory,
    })
}

fn lock(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("open artifact journal lock {}", path.display()))?;
    protect_file(path)?;
    file.try_lock().with_context(|| {
        format!(
            "artifact transfer is busy in another process (lock {})",
            path.display()
        )
    })?;
    Ok(file)
}

fn snapshot(source: &Path, payload: &Path, directory: &Path) -> Result<(u64, String)> {
    let metadata = fs::symlink_metadata(source)
        .with_context(|| format!("inspect artifact source {}", source.display()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("artifact source must be a regular file, not a symlink");
    }
    if metadata.len() > MAX_RESPONSE_BYTES as u64 {
        bail!("artifact source exceeds 16 MiB");
    }
    let mut input =
        File::open(source).with_context(|| format!("open artifact source {}", source.display()))?;
    let mut temporary = NamedTempFile::new_in(directory)
        .with_context(|| format!("create artifact snapshot in {}", directory.display()))?;
    protect_file(temporary.path())?;
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut size = 0_u64;
    let mut hasher = Sha256::new();
    loop {
        let read = input.read(&mut buffer).context("read artifact source")?;
        if read == 0 {
            break;
        }
        size = size.saturating_add(read as u64);
        if size > MAX_RESPONSE_BYTES as u64 {
            bail!("artifact source exceeds 16 MiB while copying");
        }
        hasher.update(&buffer[..read]);
        temporary
            .write_all(&buffer[..read])
            .context("write protected artifact snapshot")?;
    }
    temporary
        .as_file()
        .sync_all()
        .context("sync protected artifact snapshot")?;
    temporary
        .persist(payload)
        .map_err(|error| error.error)
        .with_context(|| format!("publish artifact snapshot {}", payload.display()))?;
    protect_file(payload)?;
    sync_directory(directory)?;
    Ok((size, hex::encode(hasher.finalize())))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UploadState {
    Pending,
    Finalized,
}

fn artifact_metadata(response: &ApiResponse) -> Result<&serde_json::Value> {
    response
        .body
        .pointer("/data/artifact")
        .ok_or_else(|| anyhow!("artifact detail omitted data.artifact"))
}

fn validate_same_upload(response: &ApiResponse, size: u64, sha256: &str) -> Result<()> {
    let artifact = artifact_metadata(response)?;
    if artifact.get("kind").and_then(serde_json::Value::as_str) != Some("upload")
        || artifact
            .get("size_bytes")
            .and_then(serde_json::Value::as_u64)
            != Some(size)
        || artifact.get("sha256").and_then(serde_json::Value::as_str) != Some(sha256)
    {
        bail!("current artifact metadata does not match the saved upload");
    }
    Ok(())
}

fn inspect_upload(response: &ApiResponse, size: u64, sha256: &str) -> Result<UploadState> {
    validate_same_upload(response, size, sha256)?;
    let artifact = artifact_metadata(response)?;
    match (
        artifact.get("state").and_then(serde_json::Value::as_str),
        artifact
            .get("availability")
            .and_then(serde_json::Value::as_str),
    ) {
        (Some("reserved"), Some("pending")) => Ok(UploadState::Pending),
        (Some("finalized"), _) => Ok(UploadState::Finalized),
        _ => bail!("artifact is not a live upload reservation or finalized upload"),
    }
}

fn download_expectation(response: &ApiResponse) -> Result<(u64, String)> {
    let artifact = artifact_metadata(response)?;
    if artifact.get("kind").and_then(serde_json::Value::as_str) != Some("upload")
        || artifact
            .get("availability")
            .and_then(serde_json::Value::as_str)
            != Some("available")
        || artifact.get("state").and_then(serde_json::Value::as_str) != Some("finalized")
    {
        bail!("artifact is not an available finalized upload");
    }
    let size = artifact
        .get("size_bytes")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| anyhow!("artifact detail omitted a valid size_bytes"))?;
    let sha256 = artifact
        .get("sha256")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("artifact detail omitted a valid sha256"))?;
    if size > MAX_RESPONSE_BYTES as u64
        || sha256.len() != 64
        || !sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("artifact detail contains an invalid size or SHA-256 digest");
    }
    Ok((size, sha256.to_owned()))
}

fn cleanup_payload(paths: &UploadPaths) -> Result<()> {
    match fs::remove_file(&paths.payload) {
        Ok(()) => sync_directory(&paths.directory),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("remove completed artifact snapshot"),
    }
}

fn validate_saved(
    context: &TransferContext<'_>,
    artifact_id: &str,
    source: &Path,
    intent: &UploadIntent,
) -> Result<()> {
    if intent.version != 1
        || intent.service_origin != context.service_origin
        || intent.project_id != context.project_id
        || intent.local_session != context.local_session
        || intent.artifact_id != artifact_id
        || intent.source != source
    {
        bail!("saved artifact upload intent does not match this request");
    }
    Ok(())
}

fn validate_payload(intent: &UploadIntent) -> Result<()> {
    if intent.completed_response.is_some() {
        return Ok(());
    }
    let (size, digest) = hash_file(&intent.payload)?;
    if size != intent.size_bytes || digest != intent.sha256 {
        bail!("saved artifact upload bytes do not match the persisted size and digest");
    }
    Ok(())
}

fn hash_file(path: &Path) -> Result<(u64, String)> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect saved artifact snapshot {}", path.display()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("saved artifact snapshot is not a regular file");
    }
    let mut file = File::open(path)
        .with_context(|| format!("open saved artifact snapshot {}", path.display()))?;
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut size = 0_u64;
    let mut hasher = Sha256::new();
    loop {
        let read = file.read(&mut buffer).context("read artifact snapshot")?;
        if read == 0 {
            break;
        }
        size = size.saturating_add(read as u64);
        if size > MAX_RESPONSE_BYTES as u64 {
            bail!("saved artifact snapshot exceeds 16 MiB");
        }
        hasher.update(&buffer[..read]);
    }
    Ok((size, hex::encode(hasher.finalize())))
}

fn load(path: &Path) -> Result<Option<UploadIntent>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("parse artifact upload intent {}", path.display()))
            .map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("read upload intent {}", path.display())),
    }
}

fn save(path: &Path, intent: &impl Serialize, directory: &Path) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(intent).context("serialize artifact upload intent")?;
    let mut temporary = NamedTempFile::new_in(directory)
        .with_context(|| format!("create temporary upload intent in {}", directory.display()))?;
    protect_file(temporary.path())?;
    temporary
        .write_all(&bytes)
        .context("write artifact upload intent")?;
    temporary
        .as_file()
        .sync_all()
        .context("sync artifact upload intent")?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("replace artifact upload intent {}", path.display()))?;
    protect_file(path)?;
    sync_directory(directory)
}

fn protect_directory(path: &Path) -> Result<()> {
    crate::state::protect_directory(path).context("protect artifact directory")
}

fn protect_file(path: &Path) -> Result<()> {
    crate::state::protect_file(path).context("protect artifact file")
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("open artifact journal directory {}", path.display()))?
        .sync_all()
        .with_context(|| format!("sync artifact journal directory {}", path.display()))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    #[tokio::test]
    async fn publication_replays_lost_reservation_and_retains_exact_report() {
        use std::sync::atomic::AtomicUsize;
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("report.json");
        fs::write(&source, b"synthetic report").unwrap();
        let journal_dir = directory.path().join("journals").join("publication");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let posts = Arc::new(AtomicUsize::new(0));
        let puts = Arc::new(AtomicUsize::new(0));
        let available = Arc::new(AtomicBool::new(true));
        let artifact = Uuid::new_v4().to_string();
        let task = Uuid::new_v4().to_string();
        let job = Uuid::new_v4().to_string();
        let digest = hex::encode(Sha256::digest(b"synthetic report"));
        let request = serde_json::json!({"filename":"report.json","media_type":"application/json",
            "task_id":task,"job_id":job,"size_bytes":16,"sha256":digest});
        let server_posts = posts.clone();
        let server_puts = puts.clone();
        let server_available = available.clone();
        let server_request = request.clone();
        let server_artifact = artifact.clone();
        let server = tokio::spawn(async move {
            let mut reservation_key = None;
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let header_end = loop {
                    let mut buffer = [0; 4096];
                    let n = stream.read(&mut buffer).await.unwrap();
                    assert_ne!(n, 0);
                    bytes.extend_from_slice(&buffer[..n]);
                    if let Some(index) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                        break index + 4;
                    }
                };
                let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .unwrap_or(0);
                while bytes.len() < header_end + length {
                    let mut buffer = [0; 4096];
                    let n = stream.read(&mut buffer).await.unwrap();
                    assert_ne!(n, 0);
                    bytes.extend_from_slice(&buffer[..n]);
                }
                if headers.starts_with("POST ") {
                    let body: serde_json::Value =
                        serde_json::from_slice(&bytes[header_end..]).unwrap();
                    assert_eq!(body, server_request);
                    let key = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("idempotency-key")
                                .then(|| value.trim().to_owned())
                        })
                        .unwrap();
                    if let Some(saved) = &reservation_key {
                        assert_eq!(&key, saved);
                    } else {
                        reservation_key = Some(key);
                    }
                    if server_posts.fetch_add(1, Ordering::SeqCst) == 0 {
                        // Commit happened, but the response disappeared.
                        drop(stream);
                        continue;
                    }
                } else if headers.starts_with("PUT ") {
                    assert_eq!(&bytes[header_end..], b"synthetic report");
                    server_puts.fetch_add(1, Ordering::SeqCst);
                } else {
                    assert!(headers.starts_with("GET "));
                }
                let finalized = server_puts.load(Ordering::SeqCst) > 0;
                let body = serde_json::json!({"data":{"artifact":{
                    "id":server_artifact,"kind":"upload","task_id":task,"job_id":job,
                    "size_bytes":16,"sha256":digest,
                    "state":if finalized { "finalized" } else { "reserved" },
                    "availability":if !server_available.load(Ordering::SeqCst) { "deleted" }
                        else if finalized { "available" } else { "pending" }
                }}})
                .to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let client =
            CoordinatorClient::new(&format!("http://{address}"), "synthetic", true).unwrap();
        let session = SessionAuth {
            id: "session".into(),
            proof: "synthetic".into(),
        };
        let context = TransferContext {
            client: &client,
            service_origin: client.origin(),
            project_id: "project",
            local_session: "local",
            session: &session,
        };
        assert!(
            publish_at(&context, &source, request.clone(), &journal_dir)
                .await
                .is_err()
        );
        assert_eq!(posts.load(Ordering::SeqCst), 1);
        fs::write(&source, b"changed source").unwrap();
        let response = publish_at(&context, &source, request.clone(), &journal_dir)
            .await
            .unwrap();
        assert_eq!(artifact_metadata(&response).unwrap()["id"], artifact);
        assert_eq!(posts.load(Ordering::SeqCst), 2);
        assert_eq!(puts.load(Ordering::SeqCst), 1);
        assert_eq!(
            fs::read(journal_dir.join("report.bin")).unwrap(),
            b"synthetic report"
        );
        publish_at(&context, &source, request.clone(), &journal_dir)
            .await
            .unwrap();
        assert_eq!(posts.load(Ordering::SeqCst), 2);
        assert_eq!(puts.load(Ordering::SeqCst), 1);
        let mut changed = request.clone();
        changed["filename"] = "other.json".into();
        assert!(
            publish_at(&context, &source, changed, &journal_dir)
                .await
                .is_err()
        );
        available.store(false, Ordering::SeqCst);
        assert!(
            publish_at(&context, &source, request, &journal_dir)
                .await
                .is_err()
        );
        assert_eq!(posts.load(Ordering::SeqCst), 2);
        assert_eq!(puts.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[test]
    fn snapshot_is_bounded_and_digest_bound() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.bin");
        let payload = directory.path().join("payload.bin");
        fs::write(&source, b"artifact bytes").unwrap();
        let canonical_source = fs::canonicalize(&source).unwrap();
        let (size, digest) = snapshot(&source, &payload, directory.path()).unwrap();
        assert_eq!(size, 14);
        assert_eq!(digest, hex::encode(Sha256::digest(b"artifact bytes")));
        fs::write(&payload, b"altered").unwrap();
        let intent = UploadIntent {
            version: 1,
            service_origin: "https://example.test".into(),
            project_id: "project".into(),
            local_session: "session".into(),
            artifact_id: Uuid::new_v4().to_string(),
            source,
            payload,
            size_bytes: size,
            sha256: digest,
            idempotency_key: Uuid::new_v4().to_string(),
            completed_response: None,
        };
        assert!(validate_payload(&intent).is_err());
        fs::remove_file(&canonical_source).unwrap();
        assert_eq!(
            source_reference(&canonical_source).unwrap(),
            canonical_source
        );
    }

    #[tokio::test]
    async fn completed_upload_rechecks_current_auth_and_cleans_payload() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let saw_request = Arc::new(AtomicBool::new(false));
        let observed = saw_request.clone();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let read = stream.read(&mut request).await.unwrap();
            assert!(request[..read].starts_with(b"GET /api/v1/projects/project/artifacts/"));
            assert!(
                request[..read]
                    .windows(b"\r\nx-coordinator-session:".len())
                    .any(|window| window.eq_ignore_ascii_case(b"\r\nx-coordinator-session:"))
            );
            observed.store(true, Ordering::SeqCst);
            let body = serde_json::json!({
                "error": {"code":"session_revoked","message":"revoked"},
                "request_id":"request",
                "server_time":"2026-09-09T00:00:00Z"
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });

        let directory = tempfile::tempdir().unwrap();
        let payload = directory.path().join("payload.bin");
        fs::write(&payload, b"artifact bytes").unwrap();
        let artifact_id = Uuid::new_v4().to_string();
        let client =
            CoordinatorClient::new(&format!("http://{address}"), "test-secret", true).unwrap();
        let session = SessionAuth {
            id: "session".into(),
            proof: "proof".into(),
        };
        let context = TransferContext {
            client: &client,
            service_origin: client.origin(),
            project_id: "project",
            local_session: "local",
            session: &session,
        };
        let intent = UploadIntent {
            version: 1,
            service_origin: client.origin().into(),
            project_id: "project".into(),
            local_session: "local".into(),
            artifact_id: artifact_id.clone(),
            source: directory.path().join("source.bin"),
            payload: payload.clone(),
            size_bytes: 14,
            sha256: hex::encode(Sha256::digest(b"artifact bytes")),
            idempotency_key: Uuid::new_v4().to_string(),
            completed_response: Some(ApiResponse {
                status: 200,
                body: serde_json::json!({"data":{"artifact":{}}}),
            }),
        };
        let paths = UploadPaths {
            directory: directory.path().to_owned(),
            state: directory.path().join("intent.json"),
            payload: payload.clone(),
            lock: directory.path().join("intent.lock"),
        };
        let response = refresh_completed(&context, &artifact_id, &intent, &paths)
            .await
            .unwrap();
        assert_eq!(response.status, 403);
        assert!(saw_request.load(Ordering::SeqCst));
        assert!(!payload.exists());
        server.await.unwrap();
    }

    #[test]
    fn matching_finalized_upload_is_reconciled_without_another_put() {
        let digest = hex::encode(Sha256::digest(b"artifact bytes"));
        let detail = ApiResponse {
            status: 200,
            body: serde_json::json!({
                "data":{"artifact":{
                    "kind":"upload",
                    "state":"finalized",
                    "availability":"available",
                    "size_bytes":14,
                    "sha256":digest
                }},
                "request_id":"request",
                "server_time":"2026-09-09T00:00:00Z"
            }),
        };
        assert_eq!(
            inspect_upload(&detail, 14, &digest).unwrap(),
            UploadState::Finalized
        );
    }
}
