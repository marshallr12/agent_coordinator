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

    // Retention boundary tests need an exact, explicitly advanced service
    // clock. Monotonic elapsed behavior is covered by clock_safety.rs.
    fn use_monotonic_elapsed(&self) -> bool {
        false
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
            ..MaintenanceOptions::default()
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
            ..MaintenanceOptions::default()
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
            ..MaintenanceOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(result["observation_payloads_compacted"], 2);
    assert_eq!(result["observation_rows_inspected"], 6);
    let rows = sqlx::query(
        "SELECT sequence,request_hash,summary,payload_compacted_at,retention_checked_at \
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
        assert_eq!(row.get::<Option<i64>, _>("retention_checked_at"), Some(NOW));
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
async fn observation_inspection_is_bounded_even_when_nothing_can_be_compacted() {
    let fixture = Fixture::new().await;
    let reporter = seed_job(&fixture).await;
    for sequence in 1..=12_i64 {
        sqlx::query(
            "INSERT INTO job_observations( \
               reporter_id,sequence,request_hash,producer_id,state,summary,observed_at \
             ) VALUES(?,?,?,'00000000-0000-0000-0000-000000000111','running',?,?)",
        )
        .bind(&reporter)
        .bind(sequence)
        .bind(format!("request-{sequence}"))
        .bind(format!("distinct progress {sequence}"))
        .bind(NOW - 31 * DAY_MS + sequence)
        .execute(&fixture.state.pool)
        .await
        .unwrap();
    }

    let first = run_maintenance(
        &fixture.state,
        MaintenanceOptions {
            batch_size: 3,
            max_batches: 1,
            ..MaintenanceOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(first["observation_rows_inspected"], 3);
    assert_eq!(first["observation_payloads_compacted"], 0);
    assert_eq!(first["remaining"]["observation_rows_to_inspect"], true);
    let checked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM job_observations WHERE retention_checked_at IS NOT NULL",
    )
    .fetch_one(&fixture.state.pool)
    .await
    .unwrap();
    assert_eq!(checked, 3);

    let second = run_maintenance(
        &fixture.state,
        MaintenanceOptions {
            batch_size: 3,
            max_batches: 3,
            ..MaintenanceOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(second["observation_rows_inspected"], 9);
    assert_eq!(second["observation_payloads_compacted"], 0);
    assert_eq!(second["remaining"]["observation_rows_to_inspect"], false);
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
                ..MaintenanceOptions::default()
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
                ..MaintenanceOptions::default()
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

/// Agents, a project and tasks for the idle-session tests, all old enough
/// that only the rows a test adds decide idleness.
struct IdleWorld<'a> {
    fixture: &'a Fixture,
    credential: String,
    tasks: std::cell::Cell<u32>,
}

impl<'a> IdleWorld<'a> {
    /// Seeds a credential and a project for `fixture`'s principal.
    async fn new(fixture: &'a Fixture) -> Self {
        let credential = Uuid::new_v4().to_string();
        idle_exec(fixture, "INSERT INTO credentials(id,principal_id,token_hash,created_at) VALUES(?2,?3,'token-'||?2,?1)", &[&credential, &fixture.principal_id], NOW - 100 * DAY_MS).await;
        idle_exec(fixture, "INSERT INTO projects(id,name,repository_url,target_branch,created_at) VALUES('idle-project','Idle','https://example.test/idle.git','main',?1)", &[], NOW - 100 * DAY_MS).await;
        Self {
            fixture,
            credential,
            tasks: std::cell::Cell::new(0),
        }
    }

    /// Seeds an open session registered `days` ago, optionally a subagent of
    /// `parent`, and returns its id.
    async fn session(&self, days: i64, parent: Option<&str>) -> String {
        let id = Uuid::new_v4().to_string();
        idle_exec(self.fixture, "INSERT INTO agent_sessions(id,principal_id,credential_id,workstation_id,proof_hash,created_at,capabilities,harness,parent_session_id) VALUES(?2,?3,?4,'host','proof',?1,'[]','test',NULLIF(?5,''))", &[&id, &self.fixture.principal_id, &self.credential, parent.unwrap_or("")], NOW - days * DAY_MS).await;
        id
    }

    /// Seeds an attempt by `session` in `state`, last touched `days` ago, on
    /// a fresh task, and returns its id.
    async fn attempt(&self, session: &str, state: &str, days: i64) -> String {
        let n = self.tasks.get() + 1;
        self.tasks.set(n);
        let (task, id) = (format!("idle-task-{n}"), Uuid::new_v4().to_string());
        idle_exec(self.fixture, "INSERT INTO tasks(id,project_id,title,description,acceptance_json,kind,priority,lifecycle,created_at,ready_since) VALUES(?2,'idle-project','Task','','[]','code',1,'open',?1,?1)", &[&task], NOW - 100 * DAY_MS).await;
        idle_exec(self.fixture, "INSERT INTO attempts(id,project_id,task_id,owner_id,session_id,credential_id,generation,state,mode,expires_at,last_heartbeat_at,last_progress_at,created_at) VALUES(?2,'idle-project',?3,?4,?5,?6,1,?7,'work',?1,?1,?1,?1)", &[&id, &task, &self.fixture.principal_id, session, &self.credential, state], NOW - days * DAY_MS).await;
        id
    }

    /// Seeds a reservation in `state` and a job in `job_state` on `attempt`,
    /// returning the job id.
    async fn job(&self, attempt: &str, reservation_state: &str, job_state: &str) -> String {
        let (reservation, job) = (Uuid::new_v4().to_string(), Uuid::new_v4().to_string());
        idle_exec(self.fixture, "INSERT INTO reservations(id,project_id,attempt_id,generation,state,created_by,created_at) VALUES(?2,'idle-project',?3,1,?4,?5,?1)", &[&reservation, attempt, reservation_state, &self.fixture.principal_id], NOW - 30 * DAY_MS).await;
        idle_exec(self.fixture, "INSERT INTO jobs(id,producer_id,project_id,task_id,attempt_id,generation,runner_instance_id,workstation_id,label,source_revision,source_tree,reservation_id,state,created_at) SELECT ?2,?2,'idle-project',task_id,id,1,'runner','host','test','rev','tree',?3,?4,?1 FROM attempts WHERE id=?5", &[&job, &reservation, job_state, attempt], NOW - 30 * DAY_MS).await;
        job
    }

    /// Seeds a reporter for `job` and `session` that expires in `days`.
    async fn reporter(&self, job: &str, session: &str, days: i64) {
        let id = Uuid::new_v4().to_string();
        idle_exec(self.fixture, "INSERT INTO reporters(id,job_id,principal_id,credential_id,session_id,proof_hash,expires_at,renew_until,created_at) VALUES(?2,?3,?4,?5,?6,'proof',?1,?1,?1)", &[&id, job, &self.fixture.principal_id, &self.credential, session], NOW + days * DAY_MS).await;
    }
}

/// Runs one statement binding the integer `at` as `?1` and `text` as `?2…`.
async fn idle_exec(fixture: &Fixture, sql: &'static str, text: &[&str], at: i64) {
    let mut query = sqlx::query(sql).bind(at);
    for value in text {
        query = query.bind(value.to_string());
    }
    query.execute(&fixture.state.pool).await.unwrap();
}

/// Whether session `id` is open.
async fn session_open(fixture: &Fixture, id: &str) -> bool {
    sqlx::query_scalar::<_, Option<i64>>("SELECT closed_at FROM agent_sessions WHERE id=?")
        .bind(id)
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap()
        .is_none()
}

/// Maintenance with the default seven-day idle period.
async fn idle_maintenance(fixture: &Fixture) -> serde_json::Value {
    run_maintenance(&fixture.state, MaintenanceOptions::default())
        .await
        .unwrap()
}

/// Sessions idle past seven days close with one event each; any activity
/// in the period (registration, acknowledgment, attempt, checkpoint) keeps
/// a session open.
#[tokio::test]
async fn idle_sessions_close_with_an_event_and_recent_activity_keeps_them_open() {
    let fixture = Fixture::new().await;
    let world = IdleWorld::new(&fixture).await;
    let idle = world.session(8, None).await;
    let finished = world.session(30, None).await;
    world.attempt(&finished, "submitted", 20).await;
    let kept = keep_open_by_activity(&world).await;
    let report = idle_maintenance(&fixture).await;
    assert_eq!(report["idle_sessions_closed"], 2);
    assert_eq!(report["remaining"]["idle_sessions"], false);
    assert_eq!(report["limits"]["session_idle_days"], 7);
    assert!(!session_open(&fixture, &idle).await && !session_open(&fixture, &finished).await);
    for id in &kept {
        assert!(session_open(&fixture, id).await, "{id} was closed");
    }
    assert_idle_event(&fixture, &idle, NOW - 8 * DAY_MS).await;
    let counted: i64 = sqlx::query_scalar("SELECT sessions_closed FROM maintenance_runs")
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap();
    assert_eq!(counted, 2);
    assert_eq!(idle_maintenance(&fixture).await["idle_sessions_closed"], 0);
}

/// Seeds four old sessions, each with one kind of activity inside the idle
/// period, and returns their ids.
async fn keep_open_by_activity(world: &IdleWorld<'_>) -> Vec<String> {
    let fresh = world.session(6, None).await;
    let acknowledged = world.session(30, None).await;
    idle_exec(world.fixture, "INSERT INTO instruction_acknowledgments(session_id,project_id,policy_revision,instruction_version,created_at) VALUES(?2,'idle-project',1,'9',?1)", &[&acknowledged], NOW - 2 * DAY_MS).await;
    let progressed = world.session(30, None).await;
    world.attempt(&progressed, "released", 3).await;
    let checkpointed = world.session(30, None).await;
    let attempt = world.attempt(&checkpointed, "submitted", 20).await;
    idle_exec(world.fixture, "INSERT INTO checkpoints(id,project_id,attempt_id,summary,current_action,next_step,blockers_json,created_at) VALUES(?2,'idle-project',?3,'working','testing','more','[]',?1)", &[&Uuid::new_v4().to_string(), &attempt], NOW - DAY_MS).await;
    vec![fresh, acknowledged, progressed, checkpointed]
}

/// The close event names the session, its principal and the idle reason.
async fn assert_idle_event(fixture: &Fixture, session: &str, last_activity: i64) {
    let row = sqlx::query("SELECT project_id,actor_id,data_json,created_at FROM events WHERE kind='agent_session_closed' AND record_id=?")
        .bind(session)
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap();
    assert_eq!(row.get::<Option<String>, _>("project_id"), None);
    assert_eq!(row.get::<String, _>("actor_id"), fixture.principal_id);
    assert_eq!(row.get::<i64, _>("created_at"), NOW);
    let data: serde_json::Value = serde_json::from_str(row.get("data_json")).unwrap();
    assert_eq!(data["reason"], "idle");
    assert_eq!(data["idle_days"], 7);
    assert_eq!(
        data["last_activity_at"],
        coordinator_core::timestamp(last_activity)
    );
}

/// Old sessions holding an active attempt (even a lapsed one), a live job
/// reporter, a held reservation, an unreconciled job or an open subagent
/// session stay open; an expired reporter does not protect its session.
#[tokio::test]
async fn sessions_holding_live_work_are_never_closed() {
    let fixture = Fixture::new().await;
    let world = IdleWorld::new(&fixture).await;
    let lapsed = world.session(30, None).await;
    world.attempt(&lapsed, "active", 20).await;
    let reporting = world.session(30, None).await;
    let attempt = world.attempt(&reporting, "submitted", 20).await;
    let job = world.job(&attempt, "released", "succeeded").await;
    world.reporter(&job, &reporting, 1).await;
    let holding = world.session(30, None).await;
    let attempt = world.attempt(&holding, "released", 20).await;
    world.job(&attempt, "held", "succeeded").await;
    let running = world.session(30, None).await;
    let attempt = world.attempt(&running, "released", 20).await;
    world.job(&attempt, "released", "running").await;
    let parent = world.session(30, None).await;
    world.session(1, Some(&parent)).await;
    let expired = world.session(30, None).await;
    let attempt = world.attempt(&expired, "submitted", 20).await;
    let job = world.job(&attempt, "released", "succeeded").await;
    world.reporter(&job, &expired, -1).await;
    assert_eq!(idle_maintenance(&fixture).await["idle_sessions_closed"], 1);
    for id in [&lapsed, &reporting, &holding, &running, &parent] {
        assert!(session_open(&fixture, id).await, "{id} was closed");
    }
    assert!(!session_open(&fixture, &expired).await);
}

/// Closing runs in bounded batches, 0 disables it, and out-of-range idle
/// periods are refused before a run starts.
#[tokio::test]
async fn idle_closing_is_batched_disableable_and_validated() {
    let fixture = Fixture::new().await;
    let world = IdleWorld::new(&fixture).await;
    for _ in 0..3 {
        world.session(10, None).await;
    }
    let options = |batch_size, session_idle_days| MaintenanceOptions {
        batch_size,
        max_batches: 1,
        session_idle_days,
    };
    let disabled = run_maintenance(&fixture.state, options(10, 0))
        .await
        .unwrap();
    assert_eq!(
        (
            disabled["idle_sessions_closed"].clone(),
            disabled["remaining"]["idle_sessions"].clone()
        ),
        (0.into(), false.into())
    );
    let first = run_maintenance(&fixture.state, options(2, 7))
        .await
        .unwrap();
    assert_eq!(
        (
            first["idle_sessions_closed"].clone(),
            first["remaining"]["idle_sessions"].clone()
        ),
        (2.into(), true.into())
    );
    let second = run_maintenance(&fixture.state, options(2, 7))
        .await
        .unwrap();
    assert_eq!(
        (
            second["idle_sessions_closed"].clone(),
            second["remaining"]["idle_sessions"].clone()
        ),
        (1.into(), false.into())
    );
    for days in [-1, 367] {
        assert!(
            run_maintenance(&fixture.state, options(2, days))
                .await
                .is_err()
        );
    }
}
