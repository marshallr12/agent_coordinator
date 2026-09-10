use coordinator_server::{
    maintenance::{MaintenanceOptions, run_maintenance},
    state::{AppState, Clock, Config},
};
use sqlx::Row;
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};
use uuid::Uuid;

const NOW: i64 = 1_800_000_000_000;
const DAY_MS: i64 = 86_400_000;

struct TestClock(AtomicI64);
impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct Fixture {
    state: AppState,
    clock: Arc<TestClock>,
    _directory: tempfile::TempDir,
    principal_id: String,
}

impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut state = AppState::open(Config {
            database_path: directory.path().join("maintenance.sqlite3"),
            public_origin: "http://127.0.0.1:8080".to_owned(),
            allow_insecure_loopback: true,
            ..Config::default()
        })
        .await
        .unwrap();
        let clock = Arc::new(TestClock(AtomicI64::new(NOW)));
        state.clock = clock.clone();
        let principal_id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO principals(id,name,kind,role,password_hash,created_at) \
             VALUES(?,'maintenance-agent','agent','agent',NULL,?)",
        )
        .bind(&principal_id)
        .bind(NOW - 100 * DAY_MS)
        .execute(&state.pool)
        .await
        .unwrap();
        Self {
            state,
            clock,
            _directory: directory,
            principal_id,
        }
    }

    async fn receipt(&self, suffix: &str, created_at: i64) {
        sqlx::query(
            "INSERT INTO mutation_receipts( \
               principal_id,operation,key,fingerprint,result_json,created_at,authority_epoch \
             ) VALUES(?,'POST /test',?,?,?,?,'initial')",
        )
        .bind(&self.principal_id)
        .bind(format!("key-{suffix}"))
        .bind(format!("fingerprint-{suffix}"))
        .bind(format!(r#"{{"large_result":"{}"}}"#, "x".repeat(4096)))
        .bind(created_at)
        .execute(&self.state.pool)
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn receipts_become_permanent_tombstones_only_after_thirty_days() {
    let fixture = Fixture::new().await;
    let cutoff = NOW - 30 * DAY_MS;
    fixture.receipt("old-a", cutoff - 2).await;
    fixture.receipt("old-b", cutoff - 1).await;
    fixture.receipt("boundary", cutoff).await;
    fixture.receipt("recent", cutoff + 1).await;

    let first = run_maintenance(
        &fixture.state,
        MaintenanceOptions {
            batch_size: 1,
            max_batches: 1,
        },
    )
    .await
    .unwrap();
    assert_eq!(first["receipt_results_compacted"], 1);
    assert_eq!(first["remaining"]["receipt_results"], true);

    let second = run_maintenance(
        &fixture.state,
        MaintenanceOptions {
            batch_size: 100,
            max_batches: 2,
        },
    )
    .await
    .unwrap();
    assert_eq!(second["receipt_results_compacted"], 1);
    assert_eq!(second["remaining"]["receipt_results"], false);

    let rows = sqlx::query(
        "SELECT key,fingerprint,result_json,created_at,authority_epoch,compacted_at \
         FROM mutation_receipts ORDER BY created_at,key",
    )
    .fetch_all(&fixture.state.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 4);
    for row in &rows[..2] {
        assert_eq!(row.get::<String, _>("result_json"), "null");
        assert_eq!(row.get::<Option<i64>, _>("compacted_at"), Some(NOW));
        assert!(row.get::<String, _>("key").starts_with("key-old-"));
        assert!(
            row.get::<String, _>("fingerprint")
                .starts_with("fingerprint-old-")
        );
        assert_eq!(row.get::<String, _>("authority_epoch"), "initial");
    }
    for row in &rows[2..] {
        assert_ne!(row.get::<String, _>("result_json"), "null");
        assert_eq!(row.get::<Option<i64>, _>("compacted_at"), None);
    }
    let runs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM maintenance_runs WHERE state='complete' AND completed_at IS NOT NULL",
    )
    .fetch_one(&fixture.state.pool)
    .await
    .unwrap();
    assert_eq!(runs, 2);
}

#[tokio::test]
async fn only_exact_duplicate_middle_running_summaries_are_elided() {
    let fixture = Fixture::new().await;
    let reporter = seed_job(&fixture).await;
    let observed_at = NOW - 31 * DAY_MS;
    for sequence in 1..=6_i64 {
        let (state, summary, exit_code, inputs_unchanged) = match sequence {
            1..=4 => ("running", "unchanged heartbeat", None, None),
            5 => ("running", "new progress", None, None),
            _ => ("succeeded", "terminal result", Some(0), Some(true)),
        };
        sqlx::query(
            "INSERT INTO job_observations( \
               reporter_id,sequence,request_hash,producer_id,state,pid,process_started_at, \
               exit_code,inputs_unchanged,summary,observed_at \
             ) VALUES(?,?,?,?,?,42,'2026-01-01T00:00:00Z',?,?,?,?)",
        )
        .bind(&reporter)
        .bind(sequence)
        .bind(format!("request-{sequence}"))
        .bind("00000000-0000-0000-0000-000000000111")
        .bind(state)
        .bind(exit_code)
        .bind(inputs_unchanged)
        .bind(summary)
        .bind(observed_at + sequence)
        .execute(&fixture.state.pool)
        .await
        .unwrap();
    }

    let result = run_maintenance(
        &fixture.state,
        MaintenanceOptions {
            batch_size: 100,
            max_batches: 2,
        },
    )
    .await
    .unwrap();
    assert_eq!(result["observation_payloads_compacted"], 2);
    let rows = sqlx::query(
        "SELECT sequence,request_hash,summary,payload_compacted_at \
         FROM job_observations WHERE reporter_id=? ORDER BY sequence",
    )
    .bind(&reporter)
    .fetch_all(&fixture.state.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 6);
    for (index, row) in rows.iter().enumerate() {
        let sequence = i64::try_from(index + 1).unwrap();
        assert_eq!(row.get::<i64, _>("sequence"), sequence);
        assert_eq!(
            row.get::<String, _>("request_hash"),
            format!("request-{sequence}")
        );
        if matches!(sequence, 2 | 3) {
            assert_eq!(row.get::<String, _>("summary"), "");
            assert_eq!(row.get::<Option<i64>, _>("payload_compacted_at"), Some(NOW));
        } else {
            assert_ne!(row.get::<String, _>("summary"), "");
            assert_eq!(row.get::<Option<i64>, _>("payload_compacted_at"), None);
        }
    }
    let projected: String = sqlx::query_scalar("SELECT summary FROM jobs WHERE id='job-one'")
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap();
    assert_eq!(projected, "terminal result");
}

#[tokio::test]
async fn a_failed_batch_rolls_back_payloads_and_effect_count() {
    let fixture = Fixture::new().await;
    fixture.receipt("will-fail", NOW - 31 * DAY_MS).await;
    sqlx::query(
        "CREATE TRIGGER reject_receipt_compaction BEFORE UPDATE OF compacted_at ON mutation_receipts \
         BEGIN SELECT RAISE(ABORT,'simulated maintenance failure'); END",
    )
    .execute(&fixture.state.pool)
    .await
    .unwrap();
    assert!(
        run_maintenance(&fixture.state, MaintenanceOptions::default())
            .await
            .is_err()
    );
    let receipt = sqlx::query(
        "SELECT result_json,compacted_at FROM mutation_receipts WHERE key='key-will-fail'",
    )
    .fetch_one(&fixture.state.pool)
    .await
    .unwrap();
    assert_ne!(receipt.get::<String, _>("result_json"), "null");
    assert_eq!(receipt.get::<Option<i64>, _>("compacted_at"), None);
    let run = sqlx::query(
        "SELECT state,batches,receipt_results_compacted FROM maintenance_runs ORDER BY started_at DESC LIMIT 1",
    )
    .fetch_one(&fixture.state.pool)
    .await
    .unwrap();
    assert_eq!(run.get::<String, _>("state"), "running");
    assert_eq!(run.get::<i64, _>("batches"), 0);
    assert_eq!(run.get::<i64, _>("receipt_results_compacted"), 0);
}

#[tokio::test]
async fn invalid_limits_do_not_create_a_maintenance_run() {
    let fixture = Fixture::new().await;
    assert!(
        run_maintenance(
            &fixture.state,
            MaintenanceOptions {
                batch_size: 0,
                max_batches: 1,
            },
        )
        .await
        .is_err()
    );
    assert!(
        run_maintenance(
            &fixture.state,
            MaintenanceOptions {
                batch_size: 1,
                max_batches: 101,
            },
        )
        .await
        .is_err()
    );
    let runs: i64 = sqlx::query_scalar("SELECT count(*) FROM maintenance_runs")
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap();
    assert_eq!(runs, 0);
}

#[tokio::test]
async fn detected_clock_rollback_is_persisted_without_compaction() {
    let fixture = Fixture::new().await;
    run_maintenance(&fixture.state, MaintenanceOptions::default())
        .await
        .unwrap();
    fixture.receipt("clock-held", NOW - 31 * DAY_MS).await;
    fixture.clock.0.store(NOW - 6_000, Ordering::SeqCst);

    let error = run_maintenance(&fixture.state, MaintenanceOptions::default())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("clock_reconciliation_required"));
    let compacted: Option<i64> =
        sqlx::query_scalar("SELECT compacted_at FROM mutation_receipts WHERE key='key-clock-held'")
            .fetch_one(&fixture.state.pool)
            .await
            .unwrap();
    assert_eq!(compacted, None);
    let status: String = sqlx::query_scalar("SELECT status FROM clock_state WHERE singleton=1")
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap();
    assert_eq!(status, "clock_reconciliation");
}

async fn seed_job(fixture: &Fixture) -> String {
    let credential = Uuid::new_v4().to_string();
    let session = Uuid::new_v4().to_string();
    let attempt = Uuid::new_v4().to_string();
    let reservation = Uuid::new_v4().to_string();
    let reporter = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO credentials(id,principal_id,token_hash,created_at) VALUES(?,?,?,?)")
        .bind(&credential)
        .bind(&fixture.principal_id)
        .bind(format!("token-{credential}"))
        .bind(NOW - 100 * DAY_MS)
        .execute(&fixture.state.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO agent_sessions(id,principal_id,credential_id,workstation_id,proof_hash,created_at,capabilities,harness) \
         VALUES(?,?,?,'host','proof',?,'[]','test')",
    )
    .bind(&session)
    .bind(&fixture.principal_id)
    .bind(&credential)
    .bind(NOW - 100 * DAY_MS)
    .execute(&fixture.state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO projects(id,name,repository_url,target_branch,created_at) \
         VALUES('project-one','Maintenance','https://example.test/repo.git','main',?)",
    )
    .bind(NOW - 100 * DAY_MS)
    .execute(&fixture.state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO tasks(id,project_id,title,description,acceptance_json,kind,priority,lifecycle,created_at,ready_since) \
         VALUES('task-one','project-one','Task','','[]','code',1,'done',?,?)",
    )
    .bind(NOW - 100 * DAY_MS)
    .bind(NOW - 100 * DAY_MS)
    .execute(&fixture.state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO attempts(id,project_id,task_id,owner_id,session_id,credential_id,generation,state,mode,expires_at,last_heartbeat_at,last_progress_at,created_at,ended_at) \
         VALUES(?,'project-one','task-one',?,?,?,1,'submitted','work',?,?,?,?,?)",
    )
    .bind(&attempt)
    .bind(&fixture.principal_id)
    .bind(&session)
    .bind(&credential)
    .bind(NOW - 99 * DAY_MS)
    .bind(NOW - 100 * DAY_MS)
    .bind(NOW - 100 * DAY_MS)
    .bind(NOW - 100 * DAY_MS)
    .bind(NOW - 99 * DAY_MS)
    .execute(&fixture.state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO reservations(id,project_id,attempt_id,generation,state,created_by,created_at,released_at,released_by,release_reason) \
         VALUES(?,'project-one',?,1,'released',?,?,?,?,'test complete')",
    )
    .bind(&reservation)
    .bind(&attempt)
    .bind(&fixture.principal_id)
    .bind(NOW - 100 * DAY_MS)
    .bind(NOW - 99 * DAY_MS)
    .bind(&fixture.principal_id)
    .execute(&fixture.state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO jobs(id,producer_id,project_id,task_id,attempt_id,generation,runner_instance_id,workstation_id,label,source_revision,source_tree,reservation_id,state,last_sequence,last_observed_at,pid,process_started_at,exit_code,inputs_unchanged,summary,created_at) \
         VALUES('job-one','00000000-0000-0000-0000-000000000111','project-one','task-one',?,1,'runner','host','test','revision','tree',?,'succeeded',6,?,42,'2026-01-01T00:00:00Z',0,1,'terminal result',?)",
    )
    .bind(&attempt)
    .bind(&reservation)
    .bind(NOW - 31 * DAY_MS + 6)
    .bind(NOW - 100 * DAY_MS)
    .execute(&fixture.state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO reporters(id,job_id,principal_id,credential_id,session_id,proof_hash,expires_at,renew_until,created_at) \
         VALUES(?,'job-one',?,?,?,'proof',?,?,?)",
    )
    .bind(&reporter)
    .bind(&fixture.principal_id)
    .bind(&credential)
    .bind(&session)
    .bind(NOW - 90 * DAY_MS)
    .bind(NOW - 90 * DAY_MS)
    .bind(NOW - 100 * DAY_MS)
    .execute(&fixture.state.pool)
    .await
    .unwrap();
    reporter
}
