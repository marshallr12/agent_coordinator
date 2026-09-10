//! Consistent, bounded, self-contained SQLite and artifact backups.

use crate::{artifacts, state::AppState};
use anyhow::{Context, anyhow, ensure};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{ConnectOptions, Row, SqlitePool, sqlite::SqliteConnectOptions};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use uuid::Uuid;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub const MAX_BACKUP_DATABASE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub const MAX_BACKUP_ARTIFACTS: usize = 1_000_000;
pub const MAX_BACKUP_MANIFEST_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_BACKUP_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024;
pub const BACKUP_DISK_RESERVE_BYTES: u64 = 256 * 1024 * 1024;
pub const BACKUP_OPERATION_TIMEOUT: Duration = Duration::from_secs(45 * 60);
const COPY_BUFFER_BYTES: usize = 64 * 1024;
const FORMAT_VERSION: u32 = 1;
const HOURLY_BUCKETS: usize = 24;
const DAILY_BUCKETS: usize = 30;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format_version: u32,
    snapshot_id: String,
    created_at: String,
    created_at_ms: i64,
    service_version: String,
    schema_version: i64,
    database: FileRecord,
    artifacts: Vec<ArtifactRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRecord {
    path: String,
    size_bytes: u64,
    sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactRecord {
    artifact_id: String,
    storage_key: String,
    size_bytes: u64,
    sha256: String,
    path: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Completion {
    format_version: u32,
    snapshot_id: String,
    manifest_sha256: String,
}

struct RepositoryLock {
    _file: File,
}

#[derive(Default)]
struct RetentionResult {
    kept_snapshots: usize,
    pruned_snapshots: usize,
    retained_bytes: u64,
}

struct VerifiedSnapshot {
    manifest: Manifest,
    snapshot_bytes: u64,
    artifact_bytes: u64,
}

/// Creates and atomically publishes one consistent, self-contained snapshot.
pub async fn create_backup(state: &AppState, repository: &Path) -> anyhow::Result<Value> {
    ensure!(
        state.config.database_path != Path::new(":memory:"),
        "An in-memory database cannot be backed up."
    );
    validate_regular_file(&state.config.database_path, "database")?;
    reject_repository_overlap(state, repository)?;
    ensure_repository(repository)?;
    let _repository_lock = acquire_repository_lock(repository, true)?;
    cleanup_staging(repository)?;
    let _artifact_guard = artifacts::acquire_backup_guard(state)
        .await
        .context("Could not hold finalized artifact content for backup.")?;

    let created_at_ms = state.now();
    let created_at = timestamp(created_at_ms)?;
    let snapshot_id = Uuid::new_v4().to_string();
    let directory_name = format!(
        "{}-{snapshot_id}",
        DateTime::<Utc>::from_timestamp_millis(created_at_ms)
            .ok_or_else(|| anyhow!("The service clock is outside the supported timestamp range."))?
            .format("%Y%m%dT%H%M%S%.3fZ")
    );
    let staging_root = repository.join(".staging");
    let staging = staging_root.join(format!("{directory_name}.partial"));
    create_private_directory(&staging)?;
    let started = std::time::Instant::now();
    let manifest = match assemble_snapshot(
        state,
        &staging,
        &snapshot_id,
        &created_at,
        created_at_ms,
        started,
    )
    .await
    {
        Ok(manifest) => manifest,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    let verified = match verify_staged_snapshot(&staging, &manifest, started).await {
        Ok(verified) => verified,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    let published = repository.join("snapshots").join(&directory_name);
    ensure!(
        fs::symlink_metadata(&published).is_err(),
        "Generated snapshot destination already exists."
    );
    if let Err(error) = rename_noreplace(&staging, &published)
        .context("Could not atomically publish the snapshot without overwrite.")
    {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    sync_directory(&repository.join("snapshots"))?;
    let retention = apply_retention(repository);
    let (retention_value, retention_warning) = match retention {
        Ok(retention) => (
            json!({
                "applied":true,
                "kept_snapshots":retention.kept_snapshots,
                "pruned_snapshots":retention.pruned_snapshots,
                "retained_bytes":retention.retained_bytes,
            }),
            Value::Null,
        ),
        Err(_) => (
            json!({"applied":false}),
            json!({
                "code":"retention_failed",
                "message":"The new snapshot is complete, but old snapshot retention could not be applied. Inspect the repository before the next backup."
            }),
        ),
    };
    Ok(json!({
        "snapshot_id":manifest.snapshot_id,
        "snapshot_path":absolute_path(&published)?,
        "created_at":manifest.created_at,
        "database_bytes":manifest.database.size_bytes,
        "artifact_count":manifest.artifacts.len(),
        "artifact_bytes":verified.artifact_bytes,
        "snapshot_bytes":verified.snapshot_bytes,
        "retention":retention_value,
        "warning":retention_warning,
    }))
}

async fn verify_staged_snapshot(
    staging: &Path,
    manifest: &Manifest,
    started: std::time::Instant,
) -> anyhow::Result<VerifiedSnapshot> {
    sync_directory(staging)?;
    let verified = verify_snapshot_files(staging, started)?;
    validate_snapshot_database(
        &staging.join(&manifest.database.path),
        manifest,
        manifest.schema_version,
        started,
    )
    .await?;
    Ok(verified)
}

async fn assemble_snapshot(
    state: &AppState,
    staging: &Path,
    snapshot_id: &str,
    created_at: &str,
    created_at_ms: i64,
    started: std::time::Instant,
) -> anyhow::Result<Manifest> {
    let estimated_database_bytes: i64 = sqlx::query_scalar(
        "SELECT CAST((SELECT page_count FROM pragma_page_count) AS INTEGER) * \
         CAST((SELECT page_size FROM pragma_page_size) AS INTEGER)",
    )
    .fetch_one(&state.pool)
    .await
    .context("Could not determine database size before backup.")?;
    ensure!(
        estimated_database_bytes >= 0
            && estimated_database_bytes as u64 <= MAX_BACKUP_DATABASE_BYTES,
        "Database exceeds the fixed 8 GiB backup limit."
    );
    ensure_free_space(staging, estimated_database_bytes as u64)?;

    let database_path = staging.join("database.sqlite3");
    let destination = database_path
        .to_str()
        .ok_or_else(|| anyhow!("Backup paths must be valid UTF-8."))?;
    vacuum_into(&state.pool, destination, started).await?;
    check_deadline(started, "Backup")?;
    make_private_file(&database_path)?;
    let database_metadata = validate_regular_file(&database_path, "backup database")?;
    ensure!(
        database_metadata.len() <= MAX_BACKUP_DATABASE_BYTES,
        "Backup database exceeds the fixed 8 GiB limit."
    );
    sync_file(&database_path)?;
    let (database_size, database_sha256) =
        hash_file(&database_path, MAX_BACKUP_DATABASE_BYTES, started)?;

    let snapshot_pool = open_snapshot_database(&database_path).await?;
    validate_database_pool(&snapshot_pool, started).await?;
    let schema_version = schema_version(&snapshot_pool).await?;
    let rows = sqlx::query(
        "SELECT id,storage_key,size_bytes,sha256 FROM artifacts \
         WHERE kind='upload' AND state='finalized' \
         AND (pinned=1 OR retention_until>?) ORDER BY id LIMIT ?",
    )
    .bind(created_at_ms)
    .bind((MAX_BACKUP_ARTIFACTS + 1) as i64)
    .fetch_all(&snapshot_pool)
    .await
    .context("Could not read the artifact manifest from the backup database.")?;
    ensure!(
        rows.len() <= MAX_BACKUP_ARTIFACTS,
        "Backup contains more than 1,000,000 finalized artifacts."
    );
    snapshot_pool.close().await;

    let blobs = staging.join("blobs");
    create_private_directory(&blobs)?;
    let source_root = artifacts::store_root(state);
    let mut artifact_records = Vec::with_capacity(rows.len());
    for row in rows {
        check_deadline(started, "Backup")?;
        let artifact_id: String = row.get("id");
        let storage_key: String = row
            .get::<Option<String>, _>("storage_key")
            .ok_or_else(|| anyhow!("A finalized upload has no storage key."))?;
        validate_uuid(&artifact_id, "artifact id")?;
        validate_uuid(&storage_key, "artifact storage key")?;
        let expected_size = row
            .get::<Option<i64>, _>("size_bytes")
            .ok_or_else(|| anyhow!("A finalized upload has no recorded size."))?;
        ensure!(
            (0..=MAX_BACKUP_ARTIFACT_BYTES as i64).contains(&expected_size),
            "A finalized artifact exceeds the fixed 16 MiB limit."
        );
        let expected_sha256 = row
            .get::<Option<String>, _>("sha256")
            .ok_or_else(|| anyhow!("A finalized upload has no recorded digest."))?;
        validate_sha256(&expected_sha256)?;
        ensure_free_space(staging, expected_size as u64)?;
        let source = artifacts::storage_path(&source_root, &storage_key)
            .map_err(|_| anyhow!("A finalized artifact has an invalid storage path."))?;
        let relative = format!("blobs/{storage_key}.blob");
        let destination = staging.join(&relative);
        let (size, digest) = copy_file(&source, &destination, MAX_BACKUP_ARTIFACT_BYTES, started)?;
        ensure!(
            size == expected_size as u64 && digest == expected_sha256,
            "Finalized artifact content does not match its database size and digest."
        );
        artifact_records.push(ArtifactRecord {
            artifact_id,
            storage_key,
            size_bytes: size,
            sha256: digest,
            path: relative,
        });
    }

    let manifest = Manifest {
        format_version: FORMAT_VERSION,
        snapshot_id: snapshot_id.to_owned(),
        created_at: created_at.to_owned(),
        created_at_ms,
        service_version: env!("CARGO_PKG_VERSION").to_owned(),
        schema_version,
        database: FileRecord {
            path: "database.sqlite3".to_owned(),
            size_bytes: database_size,
            sha256: database_sha256,
        },
        artifacts: artifact_records,
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    ensure!(
        manifest_bytes.len() as u64 <= MAX_BACKUP_MANIFEST_BYTES,
        "Backup manifest exceeds the fixed 64 MiB limit."
    );
    let manifest_path = staging.join("manifest.json");
    write_new_file(&manifest_path, &manifest_bytes)?;
    let completion = Completion {
        format_version: FORMAT_VERSION,
        snapshot_id: snapshot_id.to_owned(),
        manifest_sha256: hex::encode(Sha256::digest(&manifest_bytes)),
    };
    write_new_file(
        &staging.join("COMPLETE"),
        &serde_json::to_vec_pretty(&completion)?,
    )?;
    Ok(manifest)
}

/// Verifies an immutable snapshot without touching the configured live database.
pub async fn verify_backup(snapshot: &Path) -> anyhow::Result<Value> {
    let _lock = acquire_snapshot_lock(snapshot)?;
    let started = std::time::Instant::now();
    let path = snapshot.to_owned();
    let verified = tokio::task::spawn_blocking(move || verify_snapshot_files(&path, started))
        .await
        .context("Backup verification worker stopped unexpectedly.")??;
    validate_snapshot_database(
        &snapshot.join(&verified.manifest.database.path),
        &verified.manifest,
        verified.manifest.schema_version,
        started,
    )
    .await?;
    Ok(verification_value(snapshot, &verified, true)?)
}

/// Restores a verified snapshot into a newly published data directory.
pub async fn restore_backup(
    snapshot: &Path,
    destination: &Path,
    reason: &str,
) -> anyhow::Result<Value> {
    validate_reason(reason)?;
    ensure!(
        fs::symlink_metadata(destination).is_err(),
        "Restore destination must be a fresh absent data directory."
    );
    let _lock = acquire_snapshot_lock(snapshot)?;
    let started = std::time::Instant::now();
    let snapshot_owned = snapshot.to_owned();
    let verified =
        tokio::task::spawn_blocking(move || verify_snapshot_files(&snapshot_owned, started))
            .await
            .context("Backup verification worker stopped unexpectedly.")??;
    validate_snapshot_database(
        &snapshot.join(&verified.manifest.database.path),
        &verified.manifest,
        verified.manifest.schema_version,
        started,
    )
    .await?;

    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    validate_directory_without_mode(parent, "restore destination parent")?;
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("Restore destination must have a valid UTF-8 directory name."))?;
    let staging = parent.join(format!(".{name}.restore-{}.partial", Uuid::new_v4()));
    create_private_directory(&staging)?;
    let authority = match assemble_restore(snapshot, &staging, &verified, reason, started).await {
        Ok(authority) => authority,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    let publication = (|| -> anyhow::Result<()> {
        check_deadline(started, "Restore")?;
        sync_tree_directories(&staging)?;
        check_deadline(started, "Restore")?;
        rename_noreplace(&staging, destination)
            .context("Could not atomically publish restored data without overwrite.")?;
        Ok(())
    })();
    if let Err(error) = publication {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    sync_directory(parent)?;
    Ok(json!({
        "snapshot_id":verified.manifest.snapshot_id,
        "data_directory":absolute_path(destination)?,
        "database_path":absolute_path(&destination.join("coordinator.sqlite3"))?,
        "artifact_count":verified.manifest.artifacts.len(),
        "artifact_bytes":verified.artifact_bytes,
        "authority":authority,
    }))
}

async fn assemble_restore(
    snapshot: &Path,
    staging: &Path,
    verified: &VerifiedSnapshot,
    reason: &str,
    started: std::time::Instant,
) -> anyhow::Result<Value> {
    ensure_free_space(
        staging,
        verified
            .manifest
            .database
            .size_bytes
            .saturating_add(verified.artifact_bytes),
    )?;
    let database = staging.join("coordinator.sqlite3");
    let (database_size, database_digest) = copy_file(
        &snapshot.join(&verified.manifest.database.path),
        &database,
        MAX_BACKUP_DATABASE_BYTES,
        started,
    )?;
    ensure!(
        database_size == verified.manifest.database.size_bytes
            && database_digest == verified.manifest.database.sha256,
        "Restored database copy failed digest verification."
    );
    let artifact_root = staging.join("coordinator.sqlite3.artifacts");
    create_private_directory(&artifact_root)?;
    create_private_directory(&artifact_root.join("blobs"))?;
    create_private_directory(&artifact_root.join("staging"))?;
    for artifact in &verified.manifest.artifacts {
        check_deadline(started, "Restore")?;
        let destination = artifacts::storage_path(&artifact_root, &artifact.storage_key)
            .map_err(|_| anyhow!("Backup manifest contains an invalid storage key."))?;
        if let Some(parent) = destination.parent() {
            create_private_directory(parent)?;
        }
        let (size, digest) = copy_file(
            &snapshot.join(&artifact.path),
            &destination,
            MAX_BACKUP_ARTIFACT_BYTES,
            started,
        )?;
        ensure!(
            size == artifact.size_bytes && digest == artifact.sha256,
            "Restored artifact copy failed digest verification."
        );
    }
    sync_tree_directories(&artifact_root)?;
    check_deadline(started, "Restore")?;

    let config = crate::state::Config {
        database_path: database.clone(),
        public_origin: "http://127.0.0.1:8080".to_owned(),
        allow_insecure_loopback: true,
        ..crate::state::Config::default()
    };
    let staged_state = AppState::open(config)
        .await
        .context("Restored database is not compatible with this service version.")?;
    check_deadline(started, "Restore")?;
    let authority = crate::restore::invalidate_restored_state(
        &staged_state,
        &verified.manifest.snapshot_id,
        reason,
    )
    .await?;
    check_deadline(started, "Restore")?;
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(&staged_state.pool)
        .await
        .context("Could not checkpoint restored authority changes.")?;
    staged_state.pool.close().await;
    check_deadline(started, "Restore")?;
    make_private_file(&database)?;
    sync_file(&database)?;
    let restored_schema = schema_version_from_path(&database).await?;
    validate_snapshot_database(&database, &verified.manifest, restored_schema, started).await?;
    for artifact in &verified.manifest.artifacts {
        check_deadline(started, "Restore")?;
        let path = artifacts::storage_path(&artifact_root, &artifact.storage_key)
            .map_err(|_| anyhow!("Backup manifest contains an invalid storage key."))?;
        let (size, digest) = hash_file(&path, MAX_BACKUP_ARTIFACT_BYTES, started)?;
        ensure!(
            size == artifact.size_bytes && digest == artifact.sha256,
            "Restored artifact changed during authority invalidation."
        );
    }
    Ok(authority)
}

fn verify_snapshot_files(
    snapshot: &Path,
    started: std::time::Instant,
) -> anyhow::Result<VerifiedSnapshot> {
    validate_directory(snapshot, "snapshot")?;
    let completion_bytes = read_bounded(&snapshot.join("COMPLETE"), 64 * 1024)?;
    let completion: Completion = serde_json::from_slice(&completion_bytes)
        .context("Snapshot completion marker is invalid.")?;
    ensure!(
        completion.format_version == FORMAT_VERSION,
        "Snapshot completion format is unsupported."
    );
    let manifest_bytes = read_bounded(&snapshot.join("manifest.json"), MAX_BACKUP_MANIFEST_BYTES)?;
    ensure!(
        hex::encode(Sha256::digest(&manifest_bytes)) == completion.manifest_sha256,
        "Snapshot manifest digest does not match COMPLETE."
    );
    let manifest: Manifest =
        serde_json::from_slice(&manifest_bytes).context("Snapshot manifest is invalid.")?;
    ensure!(
        manifest.format_version == FORMAT_VERSION && manifest.snapshot_id == completion.snapshot_id,
        "Snapshot identity or format does not match COMPLETE."
    );
    validate_uuid(&manifest.snapshot_id, "snapshot id")?;
    ensure!(
        manifest.artifacts.len() <= MAX_BACKUP_ARTIFACTS,
        "Snapshot contains too many artifact entries."
    );
    ensure!(
        manifest.database.path == "database.sqlite3",
        "Snapshot database path is invalid."
    );
    validate_sha256(&manifest.database.sha256)?;
    ensure!(
        manifest.database.size_bytes <= MAX_BACKUP_DATABASE_BYTES,
        "Snapshot database exceeds the fixed 8 GiB limit."
    );
    let (database_size, database_digest) = hash_file(
        &snapshot.join(&manifest.database.path),
        MAX_BACKUP_DATABASE_BYTES,
        started,
    )?;
    ensure!(
        database_size == manifest.database.size_bytes
            && database_digest == manifest.database.sha256,
        "Snapshot database size or digest is invalid."
    );
    let mut ids = HashSet::with_capacity(manifest.artifacts.len());
    let mut keys = HashSet::with_capacity(manifest.artifacts.len());
    let mut artifact_bytes = 0_u64;
    for artifact in &manifest.artifacts {
        validate_uuid(&artifact.artifact_id, "artifact id")?;
        validate_uuid(&artifact.storage_key, "artifact storage key")?;
        ensure!(
            ids.insert(&artifact.artifact_id) && keys.insert(&artifact.storage_key),
            "Snapshot contains duplicate artifact identities or storage keys."
        );
        ensure!(
            artifact.path == format!("blobs/{}.blob", artifact.storage_key),
            "Snapshot artifact path is invalid."
        );
        ensure!(
            artifact.size_bytes <= MAX_BACKUP_ARTIFACT_BYTES,
            "Snapshot artifact exceeds the fixed 16 MiB limit."
        );
        validate_sha256(&artifact.sha256)?;
        let (size, digest) = hash_file(
            &snapshot.join(&artifact.path),
            MAX_BACKUP_ARTIFACT_BYTES,
            started,
        )?;
        ensure!(
            size == artifact.size_bytes && digest == artifact.sha256,
            "Snapshot artifact size or digest is invalid."
        );
        artifact_bytes = artifact_bytes.saturating_add(size);
    }
    let snapshot_bytes = directory_size(
        snapshot,
        MAX_BACKUP_DATABASE_BYTES
            .saturating_add(artifact_bytes.saturating_add(MAX_BACKUP_MANIFEST_BYTES)),
    )?;
    Ok(VerifiedSnapshot {
        manifest,
        snapshot_bytes,
        artifact_bytes,
    })
}

fn verification_value(
    snapshot: &Path,
    verified: &VerifiedSnapshot,
    valid: bool,
) -> anyhow::Result<Value> {
    Ok(json!({
        "snapshot_id":verified.manifest.snapshot_id,
        "snapshot_path":absolute_path(snapshot)?,
        "created_at":verified.manifest.created_at,
        "database_bytes":verified.manifest.database.size_bytes,
        "artifact_count":verified.manifest.artifacts.len(),
        "artifact_bytes":verified.artifact_bytes,
        "snapshot_bytes":verified.snapshot_bytes,
        "verified":valid,
    }))
}

async fn validate_snapshot_database(
    path: &Path,
    manifest: &Manifest,
    expected_schema: i64,
    started: std::time::Instant,
) -> anyhow::Result<()> {
    let pool = open_snapshot_database(path).await?;
    validate_database_pool(&pool, started).await?;
    let actual_schema = schema_version(&pool).await?;
    ensure!(
        actual_schema == expected_schema,
        "Snapshot database schema does not match its manifest."
    );
    validate_manifest_projection(&pool, manifest, started).await?;
    pool.close().await;
    Ok(())
}

async fn schema_version_from_path(path: &Path) -> anyhow::Result<i64> {
    let pool = open_snapshot_database(path).await?;
    let version = schema_version(&pool).await?;
    pool.close().await;
    Ok(version)
}

async fn open_snapshot_database(path: &Path) -> anyhow::Result<SqlitePool> {
    validate_regular_file(path, "snapshot database")?;
    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .foreign_keys(true)
        .disable_statement_logging();
    SqlitePool::connect_with(options)
        .await
        .context("Could not open snapshot database read-only.")
}

async fn validate_database_pool(
    pool: &SqlitePool,
    started: std::time::Instant,
) -> anyhow::Result<()> {
    let mut connection = pool.acquire().await?;
    connection
        .lock_handle()
        .await?
        .set_progress_handler(10_000, move || {
            started.elapsed() <= BACKUP_OPERATION_TIMEOUT
        });
    let result = async {
        let checks: Vec<String> = sqlx::query_scalar("PRAGMA integrity_check(100)")
            .fetch_all(&mut *connection)
            .await
            .context("SQLite integrity check could not run.")?;
        ensure!(
            checks.len() == 1 && checks[0] == "ok",
            "SQLite integrity check failed."
        );
        ensure!(
            sqlx::query("PRAGMA foreign_key_check")
                .fetch_optional(&mut *connection)
                .await
                .context("SQLite foreign-key check could not run.")?
                .is_none(),
            "SQLite foreign-key check failed."
        );
        let applied = sqlx::query(
            "SELECT version,description,checksum,success FROM _sqlx_migrations ORDER BY version",
        )
        .fetch_all(&mut *connection)
        .await
        .context("Snapshot migration metadata is unavailable.")?;
        let expected: Vec<_> = MIGRATOR.iter().collect();
        ensure!(
            applied.len() == expected.len(),
            "Snapshot migration set is not compatible with this service version."
        );
        for (row, migration) in applied.iter().zip(expected) {
            ensure!(
                row.get::<i64, _>("version") == migration.version
                    && row.get::<String, _>("description") == migration.description
                    && row.get::<Vec<u8>, _>("checksum") == migration.checksum.as_ref()
                    && row.get::<bool, _>("success"),
                "Snapshot migration version, checksum, or success state is incompatible."
            );
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    connection.lock_handle().await?.remove_progress_handler();
    result
}

async fn validate_manifest_projection(
    pool: &SqlitePool,
    manifest: &Manifest,
    started: std::time::Instant,
) -> anyhow::Result<()> {
    let mut connection = pool.acquire().await?;
    connection
        .lock_handle()
        .await?
        .set_progress_handler(10_000, move || {
            started.elapsed() <= BACKUP_OPERATION_TIMEOUT
        });
    let rows = sqlx::query(
        "SELECT id,storage_key,size_bytes,sha256 FROM artifacts \
         WHERE kind='upload' AND state='finalized' \
         AND (pinned=1 OR retention_until>?) ORDER BY id LIMIT ?",
    )
    .bind(manifest.created_at_ms)
    .bind((MAX_BACKUP_ARTIFACTS + 1) as i64)
    .fetch_all(&mut *connection)
    .await;
    connection.lock_handle().await?.remove_progress_handler();
    let rows = rows.context("Could not validate snapshot artifact projection.")?;
    ensure!(
        rows.len() == manifest.artifacts.len() && rows.len() <= MAX_BACKUP_ARTIFACTS,
        "Snapshot manifest does not exactly cover its eligible artifact rows."
    );
    for (row, artifact) in rows.iter().zip(&manifest.artifacts) {
        let size = row
            .get::<Option<i64>, _>("size_bytes")
            .and_then(|value| u64::try_from(value).ok());
        ensure!(
            row.get::<String, _>("id") == artifact.artifact_id
                && row.get::<Option<String>, _>("storage_key").as_deref()
                    == Some(artifact.storage_key.as_str())
                && size == Some(artifact.size_bytes)
                && row.get::<Option<String>, _>("sha256").as_deref()
                    == Some(artifact.sha256.as_str()),
            "Snapshot manifest does not exactly match its eligible artifact rows."
        );
    }
    Ok(())
}

async fn vacuum_into(
    pool: &SqlitePool,
    destination: &str,
    started: std::time::Instant,
) -> anyhow::Result<()> {
    let mut connection = pool.acquire().await?;
    connection
        .lock_handle()
        .await?
        .set_progress_handler(10_000, move || {
            started.elapsed() <= BACKUP_OPERATION_TIMEOUT
        });
    let result = sqlx::query("VACUUM main INTO ?")
        .bind(destination)
        .execute(&mut *connection)
        .await;
    connection.lock_handle().await?.remove_progress_handler();
    result
        .context("SQLite could not create the consistent backup image.")
        .map(|_| ())
}

async fn schema_version(pool: &SqlitePool) -> anyhow::Result<i64> {
    sqlx::query_scalar("SELECT COALESCE(max(version),0) FROM _sqlx_migrations WHERE success=1")
        .fetch_one(pool)
        .await
        .context("Could not read snapshot schema version.")
}

fn apply_retention(repository: &Path) -> anyhow::Result<RetentionResult> {
    let snapshots = repository.join("snapshots");
    let mut candidates = Vec::new();
    for entry in fs::read_dir(&snapshots).context("Could not list backup snapshots.")? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "Backup snapshot entries must be ordinary directories."
        );
        let manifest = read_manifest_metadata(&path)?;
        candidates.push((manifest.created_at_ms, path, manifest));
    }
    candidates.sort_by(|left, right| right.0.cmp(&left.0));
    let mut keep = HashSet::new();
    let mut hours = HashSet::new();
    let mut days = HashSet::new();
    for (created, path, _) in &candidates {
        let instant = DateTime::<Utc>::from_timestamp_millis(*created)
            .ok_or_else(|| anyhow!("Snapshot timestamp is outside the supported range."))?;
        let hour = instant.format("%Y-%m-%dT%H").to_string();
        let day = instant.format("%Y-%m-%d").to_string();
        let keep_hour = hours.len() < HOURLY_BUCKETS && hours.insert(hour);
        let keep_day = days.len() < DAILY_BUCKETS && days.insert(day);
        if keep_hour || keep_day {
            keep.insert(path.clone());
        }
    }
    let mut result = RetentionResult::default();
    for (_, path, _) in candidates {
        if keep.contains(&path) {
            result.kept_snapshots += 1;
            result.retained_bytes = result
                .retained_bytes
                .saturating_add(directory_size(&path, u64::MAX - result.retained_bytes)?);
        } else {
            fs::remove_dir_all(&path).context("Could not prune an expired backup snapshot.")?;
            result.pruned_snapshots += 1;
        }
    }
    sync_directory(&snapshots)?;
    Ok(result)
}

fn read_manifest_metadata(snapshot: &Path) -> anyhow::Result<Manifest> {
    let completion: Completion =
        serde_json::from_slice(&read_bounded(&snapshot.join("COMPLETE"), 64 * 1024)?)?;
    let bytes = read_bounded(&snapshot.join("manifest.json"), MAX_BACKUP_MANIFEST_BYTES)?;
    ensure!(
        completion.format_version == FORMAT_VERSION
            && completion.manifest_sha256 == hex::encode(Sha256::digest(&bytes)),
        "A retained snapshot has an invalid completion marker."
    );
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    ensure!(
        manifest.snapshot_id == completion.snapshot_id && manifest.format_version == FORMAT_VERSION,
        "A retained snapshot has mismatched metadata."
    );
    Ok(manifest)
}

fn cleanup_staging(repository: &Path) -> anyhow::Result<()> {
    let staging = repository.join(".staging");
    let mut removed = 0;
    for entry in fs::read_dir(&staging).context("Could not inspect backup staging.")? {
        if removed >= 100 {
            break;
        }
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(
            !metadata.file_type().is_symlink(),
            "Backup staging must not contain symlinks."
        );
        if !entry.file_name().to_string_lossy().ends_with(".partial") {
            continue;
        }
        if metadata.is_dir() {
            fs::remove_dir_all(path)?;
        } else {
            fs::remove_file(path)?;
        }
        removed += 1;
    }
    if removed != 0 {
        sync_directory(&staging)?;
    }
    Ok(())
}

fn ensure_repository(repository: &Path) -> anyhow::Result<()> {
    if fs::symlink_metadata(repository).is_err() {
        create_private_directory(repository)?;
    } else {
        validate_directory(repository, "backup repository")?;
        make_private_directory(repository)?;
    }
    for name in [".staging", "snapshots"] {
        let path = repository.join(name);
        if fs::symlink_metadata(&path).is_err() {
            create_private_directory(&path)?;
        } else {
            validate_directory(&path, "backup repository directory")?;
            make_private_directory(&path)?;
        }
    }
    Ok(())
}

fn acquire_repository_lock(repository: &Path, exclusive: bool) -> anyhow::Result<RepositoryLock> {
    let path = repository.join(".lock");
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "Backup repository lock must be a regular file."
        );
    }
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&path)?;
    make_private_open_file(&file)?;
    let result = if exclusive {
        fs2::FileExt::try_lock_exclusive(&file)
    } else {
        fs2::FileExt::try_lock_shared(&file)
    };
    result.context("Backup repository is busy; retry after the active operation finishes.")?;
    Ok(RepositoryLock { _file: file })
}

fn acquire_snapshot_lock(snapshot: &Path) -> anyhow::Result<Option<RepositoryLock>> {
    let parent = snapshot
        .parent()
        .ok_or_else(|| anyhow!("Snapshot path must have a parent directory."))?;
    if !matches!(
        parent.file_name().and_then(|name| name.to_str()),
        Some("snapshots" | ".staging")
    ) {
        return Ok(None);
    }
    let repository = parent
        .parent()
        .ok_or_else(|| anyhow!("Snapshot repository path is invalid."))?;
    validate_directory_without_mode(repository, "backup repository")?;
    let lock_path = repository.join(".lock");
    let metadata = match fs::symlink_metadata(&lock_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "Backup repository lock must be a regular file."
    );
    let file = OpenOptions::new().read(true).write(true).open(lock_path)?;
    fs2::FileExt::try_lock_shared(&file)
        .context("Backup repository is busy; retry after the active operation finishes.")?;
    Ok(Some(RepositoryLock { _file: file }))
}

fn reject_repository_overlap(state: &AppState, repository: &Path) -> anyhow::Result<()> {
    let repository = resolved_path(repository)?;
    let database = resolved_path(&state.config.database_path)?;
    let database_parent = database
        .parent()
        .ok_or_else(|| anyhow!("Database path must have a parent directory."))?;
    let artifact_root = resolved_path(&artifacts::store_root(state))?;
    ensure!(
        !repository.starts_with(database_parent)
            && !database_parent.starts_with(&repository)
            && !repository.starts_with(&artifact_root)
            && !artifact_root.starts_with(&repository),
        "Backup repository must be separate from the live database and artifact data directories."
    );
    Ok(())
}

fn resolved_path(path: &Path) -> anyhow::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut cursor = absolute.as_path();
    let mut missing = Vec::new();
    loop {
        match fs::canonicalize(cursor) {
            Ok(mut resolved) => {
                for component in missing.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = cursor
                    .file_name()
                    .ok_or_else(|| anyhow!("Path has no existing ancestor."))?;
                missing.push(name.to_os_string());
                cursor = cursor
                    .parent()
                    .ok_or_else(|| anyhow!("Path has no existing ancestor."))?;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn copy_file(
    source: &Path,
    destination: &Path,
    limit: u64,
    started: std::time::Instant,
) -> anyhow::Result<(u64, String)> {
    validate_regular_file(source, "source file")?;
    let mut source = File::open(source)?;
    let mut destination = create_new_file(destination)?;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    let mut size = 0_u64;
    let mut hasher = Sha256::new();
    loop {
        check_deadline(started, "File copy")?;
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        size = size
            .checked_add(read as u64)
            .ok_or_else(|| anyhow!("Copied file size overflowed."))?;
        ensure!(size <= limit, "Copied file exceeds its fixed size limit.");
        destination.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
    }
    destination.sync_all()?;
    Ok((size, hex::encode(hasher.finalize())))
}

fn hash_file(
    path: &Path,
    limit: u64,
    started: std::time::Instant,
) -> anyhow::Result<(u64, String)> {
    let metadata = validate_regular_file(path, "snapshot file")?;
    ensure!(
        metadata.len() <= limit,
        "Snapshot file exceeds its fixed size limit."
    );
    let mut file = File::open(path)?;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    let mut size = 0_u64;
    let mut hasher = Sha256::new();
    loop {
        check_deadline(started, "File verification")?;
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        size += read as u64;
        ensure!(size <= limit, "Snapshot file exceeds its fixed size limit.");
        hasher.update(&buffer[..read]);
    }
    Ok((size, hex::encode(hasher.finalize())))
}

fn directory_size(path: &Path, limit: u64) -> anyhow::Result<u64> {
    validate_directory(path, "snapshot directory")?;
    let mut total = 0_u64;
    let mut entries = 0_usize;
    let mut pending = vec![path.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            entries += 1;
            ensure!(
                entries <= MAX_BACKUP_ARTIFACTS + 16,
                "Snapshot contains too many filesystem entries."
            );
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            ensure!(
                !metadata.file_type().is_symlink(),
                "Snapshots must not contain symlinks."
            );
            if metadata.is_dir() {
                pending.push(entry.path());
            } else {
                ensure!(
                    metadata.is_file(),
                    "Snapshots may contain only regular files."
                );
                total = total
                    .checked_add(metadata.len())
                    .ok_or_else(|| anyhow!("Snapshot byte count overflowed."))?;
                ensure!(total <= limit, "Snapshot exceeds its fixed byte limit.");
            }
        }
    }
    Ok(total)
}

fn read_bounded(path: &Path, limit: u64) -> anyhow::Result<Vec<u8>> {
    let metadata = validate_regular_file(path, "snapshot metadata")?;
    ensure!(
        metadata.len() <= limit,
        "Snapshot metadata exceeds its fixed size limit."
    );
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "Snapshot metadata exceeds its fixed size limit."
    );
    Ok(bytes)
}

fn write_new_file(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut file = create_new_file(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn create_new_file(path: &Path) -> anyhow::Result<File> {
    ensure!(
        fs::symlink_metadata(path).is_err(),
        "Destination file already exists."
    );
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    make_private_open_file(&file)?;
    Ok(file)
}

fn create_private_directory(path: &Path) -> anyhow::Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    make_private_directory(path)
}

fn make_private_directory(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn make_private_file(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn make_private_open_file(file: &File) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn validate_regular_file(path: &Path, description: &str) -> anyhow::Result<fs::Metadata> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("Could not inspect {description}."))?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "{description} must be a regular file, not a symlink."
    );
    #[cfg(unix)]
    ensure_private_mode(&metadata, false, description)?;
    Ok(metadata)
}

fn validate_directory(path: &Path, description: &str) -> anyhow::Result<()> {
    let metadata = validate_directory_without_mode(path, description)?;
    #[cfg(unix)]
    ensure_private_mode(&metadata, true, description)?;
    Ok(())
}

fn validate_directory_without_mode(path: &Path, description: &str) -> anyhow::Result<fs::Metadata> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("Could not inspect {description}."))?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "{description} must be a directory, not a symlink."
    );
    Ok(metadata)
}

#[cfg(unix)]
fn ensure_private_mode(
    metadata: &fs::Metadata,
    directory: bool,
    description: &str,
) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = metadata.permissions().mode();
    ensure!(
        mode & 0o077 == 0 && (!directory || mode & 0o700 == 0o700),
        "{description} permissions must not grant group or other access."
    );
    Ok(())
}

fn ensure_free_space(path: &Path, incoming: u64) -> anyhow::Result<()> {
    let free = fs2::available_space(path)?;
    ensure!(
        free >= incoming.saturating_add(BACKUP_DISK_RESERVE_BYTES),
        "Backup storage lacks space while preserving the fixed 256 MiB reserve."
    );
    Ok(())
}

fn sync_file(path: &Path) -> anyhow::Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn sync_directory(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    Ok(())
}

fn sync_tree_directories(root: &Path) -> anyhow::Result<()> {
    let mut directories = vec![root.to_owned()];
    let mut cursor = 0;
    while cursor < directories.len() {
        let directory = directories[cursor].clone();
        cursor += 1;
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            ensure!(
                !metadata.file_type().is_symlink(),
                "Restore staging contains a symlink."
            );
            if metadata.is_dir() {
                directories.push(entry.path());
            }
        }
    }
    for directory in directories.into_iter().rev() {
        sync_directory(&directory)?;
    }
    Ok(())
}

fn validate_uuid(value: &str, description: &str) -> anyhow::Result<()> {
    let uuid = Uuid::parse_str(value).with_context(|| format!("Invalid {description}."))?;
    ensure!(
        uuid.to_string() == value,
        "{description} must use canonical UUID form."
    );
    Ok(())
}

fn validate_sha256(value: &str) -> anyhow::Result<()> {
    ensure!(
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "Snapshot contains an invalid SHA-256 digest."
    );
    Ok(())
}

fn validate_reason(reason: &str) -> anyhow::Result<()> {
    ensure!(
        !reason.is_empty()
            && reason.len() <= 2000
            && reason.trim() == reason
            && !reason.contains('\0')
            && !reason.chars().any(char::is_control),
        "Restore reason must be 1-2000 bytes, have no surrounding whitespace, and contain no control characters."
    );
    Ok(())
}

fn timestamp(value: i64) -> anyhow::Result<String> {
    DateTime::<Utc>::from_timestamp_millis(value)
        .map(|value| value.to_rfc3339())
        .ok_or_else(|| anyhow!("Timestamp is outside the supported range."))
}

fn absolute_path(path: &Path) -> anyhow::Result<String> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    Ok(path.to_string_lossy().into_owned())
}

fn check_deadline(started: std::time::Instant, operation: &str) -> anyhow::Result<()> {
    ensure!(
        started.elapsed() <= BACKUP_OPERATION_TIMEOUT,
        "{operation} exceeded the fixed 45-minute operation deadline."
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn rename_noreplace(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    const AT_FDCWD: i32 = -100;
    const RENAME_NOREPLACE: u32 = 1;
    unsafe extern "C" {
        fn renameat2(
            olddirfd: i32,
            oldpath: *const std::os::raw::c_char,
            newdirfd: i32,
            newpath: *const std::os::raw::c_char,
            flags: u32,
        ) -> i32;
    }
    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL in source path"))?;
    let destination = CString::new(destination.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL in destination path")
    })?;
    // SAFETY: both C strings remain alive for the call and renameat2 does not
    // retain their pointers. RENAME_NOREPLACE gives the required atomic guard.
    let result = unsafe {
        renameat2(
            AT_FDCWD,
            source.as_ptr(),
            AT_FDCWD,
            destination.as_ptr(),
            RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(target_os = "windows")]
fn rename_noreplace(source: &Path, destination: &Path) -> std::io::Result<()> {
    // Windows rename fails when the destination already exists.
    fs::rename(source, destination)
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn rename_noreplace(_source: &Path, _destination: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic no-replace directory publication is unsupported on this host",
    ))
}

#[cfg(all(test, any(target_os = "linux", target_os = "windows")))]
mod tests {
    use super::rename_noreplace;
    use std::fs;

    #[test]
    fn atomic_publication_never_replaces_an_existing_empty_directory() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("payload"), b"snapshot").unwrap();
        fs::create_dir(&destination).unwrap();

        assert!(rename_noreplace(&source, &destination).is_err());
        assert!(source.join("payload").is_file());
        assert!(destination.read_dir().unwrap().next().is_none());
    }
}
