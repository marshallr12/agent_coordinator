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
    fs::create_dir_all(&paths.directory)
        .with_context(|| format!("create artifact journal {}", paths.directory.display()))?;
    protect_directory(&paths.directory)?;
    let _lock = lock(&paths.lock)?;

    let mut intent = match load(&paths.state)? {
        Some(intent) => {
            let source = source_reference(source)?;
            validate_saved(context, artifact_id, &source, &intent)?;
            intent
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
            validate_reservation(&detail, size_bytes, &sha256)?;
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
            intent
        }
    };
    if let Some(response) = intent.completed_response {
        return Ok(response);
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
        match fs::remove_file(&paths.payload) {
            Ok(()) => sync_directory(&paths.directory)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("remove completed artifact snapshot"),
        }
    }
    Ok(response)
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
    let path = format!(
        "/api/v1/projects/{}/artifacts/{artifact_id}/content",
        context.project_id
    );
    context
        .client
        .download_to_path(&path, output, Some(context.session))
        .await
        .context("stream artifact download")
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

fn validate_reservation(response: &ApiResponse, size: u64, sha256: &str) -> Result<()> {
    let artifact = response
        .body
        .pointer("/data/artifact")
        .ok_or_else(|| anyhow!("artifact detail omitted data.artifact"))?;
    if artifact.get("kind").and_then(serde_json::Value::as_str) != Some("upload")
        || artifact
            .get("availability")
            .and_then(serde_json::Value::as_str)
            != Some("pending")
    {
        bail!("artifact is not a live upload reservation");
    }
    if artifact
        .get("size_bytes")
        .and_then(serde_json::Value::as_u64)
        != Some(size)
        || artifact.get("sha256").and_then(serde_json::Value::as_str) != Some(sha256)
    {
        bail!("saved file size or SHA-256 does not match the upload reservation");
    }
    Ok(())
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

fn save(path: &Path, intent: &UploadIntent, directory: &Path) -> Result<()> {
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

#[cfg(unix)]
fn protect_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("protect artifact journal {}", path.display()))
}

#[cfg(not(unix))]
fn protect_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn protect_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("protect artifact journal file {}", path.display()))
}

#[cfg(not(unix))]
fn protect_file(_path: &Path) -> Result<()> {
    Ok(())
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
}
