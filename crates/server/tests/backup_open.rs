use coordinator_server::state::{AppState, Config};

#[tokio::test]
async fn backup_open_requires_matching_schema_and_cannot_write() {
    let directory = tempfile::tempdir().unwrap();
    let config = Config {
        database_path: directory.path().join("coordinator.sqlite3"),
        ..Config::default()
    };
    let live = AppState::open(config.clone()).await.unwrap();
    let backup = AppState::open_existing_read_only(config.clone())
        .await
        .unwrap();
    assert!(
        sqlx::query("CREATE TABLE backup_must_not_write(value INTEGER)")
            .execute(&backup.pool)
            .await
            .is_err()
    );
    let migration_count: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(&live.pool)
        .await
        .unwrap();
    backup.pool.close().await;

    // Model a newer or damaged migration ledger without printing its contents.
    sqlx::query("UPDATE _sqlx_migrations SET checksum=X'00' WHERE version=(SELECT max(version) FROM _sqlx_migrations)")
        .execute(&live.pool).await.unwrap();
    assert!(AppState::open_existing_read_only(config).await.is_err());
    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(&live.pool)
        .await
        .unwrap();
    assert_eq!(remaining, migration_count);
    live.pool.close().await;
}

#[tokio::test]
async fn backup_open_does_not_create_missing_installations() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing").join("coordinator.sqlite3");
    assert!(
        AppState::open_existing_read_only(Config {
            database_path: missing.clone(),
            ..Config::default()
        })
        .await
        .is_err()
    );
    assert!(!missing.parent().unwrap().exists());
}
