use coordinator_server::{
    artifacts::reconcile_store,
    backup::{create_backup, restore_backup, verify_backup},
    state::{AppState, Clock, Config},
};
use fs2::FileExt;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
};
use uuid::Uuid;

struct TestClock(AtomicI64);
impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    fn use_monotonic_elapsed(&self) -> bool {
        false
    }
}

struct Fixture {
    state: AppState,
    clock: Arc<TestClock>,
    directory: tempfile::TempDir,
}

impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        private_directory(&directory.path().join("live-data"));
        let mut state = AppState::open(Config {
            database_path: directory.path().join("live-data/live.sqlite3"),
            public_origin: "http://127.0.0.1:8080".to_owned(),
            allow_insecure_loopback: true,
            ..Config::default()
        })
        .await
        .unwrap();
        let clock = Arc::new(TestClock(AtomicI64::new(1_800_000_000_000)));
        state.clock = clock.clone();
        let principal = Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO principals(id,name,kind,role,password_hash,created_at) VALUES(?,'backup-admin','human','admin','test-only',?)")
            .bind(&principal)
            .bind(state.now())
            .execute(&state.pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO projects(id,name,repository_url,target_branch,created_at) VALUES('backup-project','Backup project','https://example.test/repository.git','main',?)")
            .bind(state.now())
            .execute(&state.pool)
            .await
            .unwrap();
        Self {
            state,
            clock,
            directory,
        }
    }

    fn repository(&self) -> PathBuf {
        self.directory.path().join("backups")
    }

    async fn finalized_artifact(&self, content: &[u8], write_content: bool) -> (String, String) {
        let artifact = Uuid::new_v4().to_string();
        let storage = Uuid::new_v4().to_string();
        let digest = hex::encode(Sha256::digest(content));
        let principal: String =
            sqlx::query_scalar("SELECT id FROM principals WHERE name='backup-admin'")
                .fetch_one(&self.state.pool)
                .await
                .unwrap();
        sqlx::query("INSERT INTO artifacts(id,project_id,kind,display_name,media_type,size_bytes,sha256,storage_key,state,created_by,created_at,finalized_at,retention_until,pinned) VALUES(?,'backup-project','upload','evidence.bin','application/octet-stream',?,?,?,'finalized',?,?,?,?,1)")
            .bind(&artifact)
            .bind(content.len() as i64)
            .bind(&digest)
            .bind(&storage)
            .bind(&principal)
            .bind(self.state.now())
            .bind(self.state.now())
            .bind(Option::<i64>::None)
            .execute(&self.state.pool)
            .await
            .unwrap();
        if write_content {
            write_runtime_blob(&self.state.config.database_path, &storage, content);
        }
        (artifact, storage)
    }
}

#[tokio::test]
async fn creates_verifies_and_restores_a_self_contained_snapshot() {
    let fixture = Fixture::new().await;
    let content = b"bounded artifact evidence";
    let (artifact, storage) = fixture.finalized_artifact(content, true).await;
    let created = create_backup(&fixture.state, &fixture.repository())
        .await
        .unwrap();
    assert_eq!(created["artifact_count"], 1);
    assert_eq!(created["artifact_bytes"], content.len());
    assert_eq!(created["retention"]["applied"], true);
    let snapshot = PathBuf::from(created["snapshot_path"].as_str().unwrap());
    assert!(snapshot.join("database.sqlite3").is_file());
    assert!(snapshot.join(format!("blobs/{storage}.blob")).is_file());
    assert!(snapshot.join("manifest.json").is_file());
    assert!(snapshot.join("COMPLETE").is_file());

    let verified = verify_backup(&snapshot).await.unwrap();
    assert_eq!(verified["snapshot_id"], created["snapshot_id"]);
    assert_eq!(verified["verified"], true);

    let destination = fixture.directory.path().join("restored-data");
    let restored = restore_backup(&snapshot, &destination, "test recovery exercise")
        .await
        .unwrap();
    assert_eq!(restored["snapshot_id"], created["snapshot_id"]);
    assert_eq!(
        restored["database_path"],
        absolute(&destination.join("coordinator.sqlite3"))
    );
    let restored_blob = runtime_blob(&destination.join("coordinator.sqlite3"), &storage);
    assert_eq!(fs::read(restored_blob).unwrap(), content);
    let restored_pool =
        sqlx::SqlitePool::connect(destination.join("coordinator.sqlite3").to_str().unwrap())
            .await
            .unwrap();
    let found: i64 = sqlx::query_scalar("SELECT count(*) FROM artifacts WHERE id=?")
        .bind(artifact)
        .fetch_one(&restored_pool)
        .await
        .unwrap();
    assert_eq!(found, 1);
    restored_pool.close().await;
    assert!(
        restore_backup(&snapshot, &destination, "must not overwrite")
            .await
            .is_err()
    );

    #[cfg(unix)]
    assert_private_tree(&destination);
}

#[tokio::test]
async fn missing_or_corrupt_content_never_verifies_as_complete() {
    let missing = Fixture::new().await;
    missing.finalized_artifact(b"missing", false).await;
    assert!(
        create_backup(&missing.state, &missing.repository())
            .await
            .is_err()
    );
    assert_eq!(
        fs::read_dir(missing.repository().join("snapshots"))
            .unwrap()
            .count(),
        0
    );

    let corrupt = Fixture::new().await;
    let (_, storage) = corrupt.finalized_artifact(b"correct", true).await;
    let created = create_backup(&corrupt.state, &corrupt.repository())
        .await
        .unwrap();
    let snapshot = PathBuf::from(created["snapshot_path"].as_str().unwrap());
    let path = snapshot.join(format!("blobs/{storage}.blob"));
    let mut file = OpenOptions::new().write(true).open(path).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(b"X").unwrap();
    file.sync_all().unwrap();
    assert!(verify_backup(&snapshot).await.is_err());
}

#[tokio::test]
async fn verifier_requires_manifest_to_cover_every_eligible_database_artifact() {
    let fixture = Fixture::new().await;
    fixture
        .finalized_artifact(b"must remain covered", true)
        .await;
    let created = create_backup(&fixture.state, &fixture.repository())
        .await
        .unwrap();
    let snapshot = PathBuf::from(created["snapshot_path"].as_str().unwrap());
    let manifest_path = snapshot.join("manifest.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["artifacts"].as_array_mut().unwrap().clear();
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    fs::write(&manifest_path, &manifest_bytes).unwrap();
    let completion_path = snapshot.join("COMPLETE");
    let mut completion: Value =
        serde_json::from_slice(&fs::read(&completion_path).unwrap()).unwrap();
    completion["manifest_sha256"] = Value::String(hex::encode(Sha256::digest(&manifest_bytes)));
    fs::write(
        completion_path,
        serde_json::to_vec_pretty(&completion).unwrap(),
    )
    .unwrap();
    assert!(verify_backup(&snapshot).await.is_err());
}

#[tokio::test]
async fn artifact_cleanup_defers_while_a_backup_shared_lock_is_held() {
    let fixture = Fixture::new().await;
    let (artifact, storage) = fixture.finalized_artifact(b"held", true).await;
    sqlx::query("UPDATE artifacts SET state='deleted',deleted_at=? WHERE id=?")
        .bind(fixture.state.now())
        .bind(artifact)
        .execute(&fixture.state.pool)
        .await
        .unwrap();
    let root = artifact_root(&fixture.state.config.database_path);
    fs::create_dir_all(root.join("staging")).unwrap();
    let lock_path = root.join(".gc.lock");
    let lock = secure_file(&lock_path);
    FileExt::lock_shared(&lock).unwrap();
    reconcile_store(&fixture.state).await.unwrap();
    let blob = runtime_blob(&fixture.state.config.database_path, &storage);
    assert!(blob.is_file());
    FileExt::unlock(&lock).unwrap();
    reconcile_store(&fixture.state).await.unwrap();
    reconcile_store(&fixture.state).await.unwrap();
    assert!(!blob.exists());
}

#[tokio::test]
async fn retention_keeps_24_hourly_and_30_daily_buckets() {
    let fixture = Fixture::new().await;
    let repository = fixture.repository();
    let day = 86_400_000_i64;
    let hour = 3_600_000_i64;
    let base = chrono::DateTime::parse_from_rfc3339("2027-03-01T00:00:00Z")
        .unwrap()
        .timestamp_millis();
    // Keep every fixture snapshot after initialization: protected service time
    // correctly clamps a rollback, which would collapse historical buckets.
    assert!(base - 30 * day > fixture.state.now());
    for days_ago in (0..=30).rev() {
        fixture
            .clock
            .0
            .store(base - days_ago * day, Ordering::SeqCst);
        create_backup(&fixture.state, &repository).await.unwrap();
    }
    for current_hour in 1..24 {
        fixture
            .clock
            .0
            .store(base + current_hour * hour, Ordering::SeqCst);
        create_backup(&fixture.state, &repository).await.unwrap();
    }
    assert_eq!(
        fs::read_dir(repository.join("snapshots")).unwrap().count(),
        53
    );
}

#[cfg(unix)]
#[tokio::test]
async fn verifier_rejects_symlinked_snapshot_content() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new().await;
    let (_, storage) = fixture.finalized_artifact(b"link target", true).await;
    let created = create_backup(&fixture.state, &fixture.repository())
        .await
        .unwrap();
    let snapshot = PathBuf::from(created["snapshot_path"].as_str().unwrap());
    let blob = snapshot.join(format!("blobs/{storage}.blob"));
    let target = fixture.directory.path().join("outside");
    fs::write(&target, b"link target").unwrap();
    fs::remove_file(&blob).unwrap();
    symlink(&target, &blob).unwrap();
    assert!(verify_backup(&snapshot).await.is_err());
}

#[tokio::test]
async fn overlap_is_rejected_before_creation_and_standalone_verify_is_read_only() {
    let fixture = Fixture::new().await;
    let overlapping = fixture.directory.path().join("live-data/backups");
    assert!(create_backup(&fixture.state, &overlapping).await.is_err());
    assert!(!overlapping.exists());

    fixture.finalized_artifact(b"standalone", true).await;
    let created = create_backup(&fixture.state, &fixture.repository())
        .await
        .unwrap();
    let snapshot = PathBuf::from(created["snapshot_path"].as_str().unwrap());
    let standalone_parent = fixture.directory.path().join("copied-offsite");
    private_directory(&standalone_parent);
    let standalone = standalone_parent.join("snapshot-copy");
    fs::rename(snapshot, &standalone).unwrap();
    #[cfg(unix)]
    let original_mode = {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(&standalone_parent)
            .unwrap()
            .permissions()
            .mode()
    };

    assert_eq!(verify_backup(&standalone).await.unwrap()["verified"], true);
    assert!(!standalone_parent.join(".lock").exists());
    assert!(!standalone_parent.join(".staging").exists());
    assert!(!standalone_parent.join("snapshots").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&standalone_parent)
                .unwrap()
                .permissions()
                .mode(),
            original_mode
        );
    }
}

fn artifact_root(database: &Path) -> PathBuf {
    let name = database.file_name().unwrap().to_str().unwrap();
    database.with_file_name(format!("{name}.artifacts"))
}

fn runtime_blob(database: &Path, storage: &str) -> PathBuf {
    artifact_root(database)
        .join("blobs")
        .join(&storage[..2])
        .join(format!("{storage}.blob"))
}

fn write_runtime_blob(database: &Path, storage: &str, content: &[u8]) {
    let path = runtime_blob(database, storage);
    private_directory(path.parent().unwrap().parent().unwrap());
    private_directory(path.parent().unwrap());
    let mut file = secure_file(&path);
    file.write_all(content).unwrap();
    file.sync_all().unwrap();
}

fn private_directory(path: &Path) {
    if !path.exists() {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(path).unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
}

fn secure_file(path: &Path) -> File {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .unwrap();
    }
    file
}

fn absolute(path: &Path) -> Value {
    Value::String(path.to_string_lossy().into_owned())
}

#[cfg(unix)]
fn assert_private_tree(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut pending = vec![root.to_owned()];
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path).unwrap();
        assert_eq!(
            metadata.permissions().mode() & 0o077,
            0,
            "{}",
            path.display()
        );
        if metadata.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                pending.push(entry.unwrap().path());
            }
        }
    }
}

// Construct an actual schema-12 database from the original migration bytes,
// then put it in a version-1 snapshot envelope. The separate old-binary upgrade
// exercise also verifies a snapshot produced by the previous executable.
async fn old_schema_snapshot(fixture: &Fixture) -> PathBuf {
    let migrations = fixture.directory.path().join("schema12-migrations");
    fs::create_dir(&migrations).unwrap();
    for migration in sqlx::migrate!("./migrations")
        .iter()
        .filter(|m| m.version <= 12)
    {
        let name = format!(
            "{:04}_{}.sql",
            migration.version,
            migration.description.replace(' ', "_")
        );
        fs::write(migrations.join(name), migration.sql.as_str().as_bytes()).unwrap();
    }
    let old_database = fixture.directory.path().join("schema12.sqlite3");
    let pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&old_database)
            .create_if_missing(true)
            .foreign_keys(true),
    )
    .await
    .unwrap();
    sqlx::migrate::Migrator::new(migrations.as_path())
        .await
        .unwrap()
        .run(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO principals(id,name,kind,role,password_hash,created_at) VALUES('old-admin','old-admin','human','admin','test-password-hash',1)")
        .execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO projects(id,name,repository_url,target_branch,created_at) VALUES('old-project','Old project','https://example.test/old.git','main',1)")
        .execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO tasks(id,project_id,title,description,acceptance_json,kind,priority,lifecycle,created_at,ready_since) VALUES('old-task','old-project','Preserved old search evidence','','[]','general',2,'canceled',1,1)")
        .execute(&pool).await.unwrap();
    pool.close().await;
    let created = create_backup(&fixture.state, &fixture.repository())
        .await
        .unwrap();
    let snapshot = PathBuf::from(created["snapshot_path"].as_str().unwrap());
    drop(secure_file(&old_database));
    fs::copy(&old_database, snapshot.join("database.sqlite3")).unwrap();
    reseal_snapshot_database(&snapshot, 12);
    snapshot
}

fn reseal_snapshot_database(snapshot: &Path, schema: i64) {
    let database = fs::read(snapshot.join("database.sqlite3")).unwrap();
    let manifest_path = snapshot.join("manifest.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["schema_version"] = Value::from(schema);
    manifest["database"]["size_bytes"] = Value::from(database.len());
    manifest["database"]["sha256"] = Value::from(hex::encode(Sha256::digest(&database)));
    let encoded = serde_json::to_vec_pretty(&manifest).unwrap();
    fs::write(manifest_path, &encoded).unwrap();
    let path = snapshot.join("COMPLETE");
    let mut completion: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    completion["manifest_sha256"] = Value::from(hex::encode(Sha256::digest(encoded)));
    fs::write(path, serde_json::to_vec_pretty(&completion).unwrap()).unwrap();
}

#[tokio::test]
async fn old_snapshot_is_verified_then_migrated_and_paused_without_changing_source() {
    let fixture = Fixture::new().await;
    let snapshot = old_schema_snapshot(&fixture).await;
    let source = fs::read(snapshot.join("database.sqlite3")).unwrap();
    assert_eq!(verify_backup(&snapshot).await.unwrap()["verified"], true);
    let destination = fixture.directory.path().join("upgraded-restore");
    restore_backup(
        &snapshot,
        &destination,
        "Verify upgrade of an old snapshot.",
    )
    .await
    .unwrap();
    assert_eq!(fs::read(snapshot.join("database.sqlite3")).unwrap(), source);
    let pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new()
            .filename(destination.join("coordinator.sqlite3"))
            .read_only(true),
    )
    .await
    .unwrap();
    let newest = sqlx::migrate!("./migrations")
        .iter()
        .map(|m| m.version)
        .max()
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT max(version) FROM _sqlx_migrations")
            .fetch_one(&pool)
            .await
            .unwrap(),
        newest
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT coordination_state FROM service_state")
            .fetch_one(&pool)
            .await
            .unwrap(),
        "restore_reconciliation"
    );
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM principals WHERE id='old-admin' AND disabled_at IS NOT NULL AND password_hash<>'test-password-hash'").fetch_one(&pool).await.unwrap(), 1);
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT task_id FROM task_search WHERE task_search MATCH 'Preserved'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        "old-task"
    );
    pool.close().await;
}

#[tokio::test]
async fn snapshot_prefix_rejects_missing_altered_future_and_preformat_migrations() {
    for (change, schema) in [
        ("DELETE FROM _sqlx_migrations WHERE version=6", 12),
        (
            "UPDATE _sqlx_migrations SET checksum=x'00' WHERE version=12",
            12,
        ),
        (
            "UPDATE _sqlx_migrations SET version=999 WHERE version=12",
            999,
        ),
        ("DELETE FROM _sqlx_migrations WHERE version=12", 11),
    ] {
        let fixture = Fixture::new().await;
        let snapshot = old_schema_snapshot(&fixture).await;
        let pool = sqlx::SqlitePool::connect_with(
            sqlx::sqlite::SqliteConnectOptions::new().filename(snapshot.join("database.sqlite3")),
        )
        .await
        .unwrap();
        sqlx::query(change).execute(&pool).await.unwrap();
        pool.close().await;
        reseal_snapshot_database(&snapshot, schema);
        let error = verify_backup(&snapshot).await.unwrap_err().to_string();
        assert!(error.contains("migration"), "{error}");
        let destination = fixture.directory.path().join("must-not-publish");
        assert!(
            restore_backup(&snapshot, &destination, "Reject incompatible snapshot.")
                .await
                .is_err()
        );
        assert!(!destination.exists());
    }
}
