use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use coordinator_server::{
    auth::{digest, secret},
    knowledge::ensure_decisions_resolved,
    operator_access::recover_clock,
    router,
    state::{AppState, Clock, Config},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::Row;
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};
use tower::ServiceExt;
use uuid::Uuid;

const BASE: i64 = 1_800_000_000_000;

struct TestClock(AtomicI64);
impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}
impl TestClock {
    fn set(&self, value: i64) {
        self.0.store(value, Ordering::SeqCst);
    }
}

#[derive(Clone)]
struct Caller {
    token: String,
    session: String,
    proof: String,
    principal: String,
    credential: String,
    human: bool,
}

struct Fixture {
    state: AppState,
    app: Router,
    clock: Arc<TestClock>,
    admin: Caller,
    agent: Caller,
    expiring_agent: Caller,
    directory: tempfile::TempDir,
}

impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut state = AppState::open(Config {
            database_path: directory.path().join("clock.sqlite3"),
            public_origin: "http://127.0.0.1:8080".into(),
            allow_insecure_loopback: true,
            ..Config::default()
        })
        .await
        .unwrap();
        let clock = Arc::new(TestClock(AtomicI64::new(BASE)));
        state.clock = clock.clone();
        let admin = seed(&state, true, "clock-admin", None).await;
        let agent = seed(&state, false, "clock-agent", None).await;
        let expiring_agent = seed(&state, false, "expiring-clock-agent", Some(BASE + 10_000)).await;
        let app = router(state.clone());
        Self {
            state,
            app,
            clock,
            admin,
            agent,
            expiring_agent,
            directory,
        }
    }

    async fn call(
        &self,
        caller: &Caller,
        method: &str,
        path: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        call(self.app.clone(), caller, method, path, body).await
    }

    async fn status(&self) -> Value {
        let (status, value) = self
            .call(&self.admin, "GET", "/api/v1/admin/clock", json!({}))
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        value["data"].clone()
    }
}

async fn seed(state: &AppState, human: bool, name: &str, credential_expiry: Option<i64>) -> Caller {
    let caller = Caller {
        token: secret(),
        session: Uuid::new_v4().to_string(),
        proof: secret(),
        principal: Uuid::new_v4().to_string(),
        credential: Uuid::new_v4().to_string(),
        human,
    };
    sqlx::query(
        "INSERT INTO principals(id,name,kind,role,password_hash,created_at) VALUES(?,?,?,?,?,?)",
    )
    .bind(&caller.principal)
    .bind(name)
    .bind(if human { "human" } else { "agent" })
    .bind(if human { "admin" } else { "agent" })
    .bind(if human { Some("unused") } else { None })
    .bind(BASE)
    .execute(&state.pool)
    .await
    .unwrap();
    if human {
        sqlx::query("INSERT INTO browser_sessions(id,principal_id,token_hash,expires_at,created_at) VALUES(?,?,?,?,?)")
            .bind(&caller.session)
            .bind(&caller.principal)
            .bind(digest(&caller.token))
            .bind(BASE + 86_400_000)
            .bind(BASE)
            .execute(&state.pool)
            .await
            .unwrap();
    } else {
        sqlx::query("INSERT INTO credentials(id,principal_id,token_hash,created_at,expires_at) VALUES(?,?,?,?,?)")
            .bind(&caller.credential)
            .bind(&caller.principal)
            .bind(digest(&caller.token))
            .bind(BASE)
            .bind(credential_expiry)
            .execute(&state.pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO agent_sessions(id,principal_id,credential_id,workstation_id,proof_hash,created_at,capabilities,harness) VALUES(?,?,?,?,?,?,'[]','clock-test')")
            .bind(&caller.session)
            .bind(&caller.principal)
            .bind(&caller.credential)
            .bind(name)
            .bind(digest(&caller.proof))
            .bind(BASE)
            .execute(&state.pool)
            .await
            .unwrap();
    }
    caller
}

async fn call(
    app: Router,
    caller: &Caller,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("idempotency-key", Uuid::new_v4().to_string());
    if caller.human {
        request = request
            .header("cookie", format!("coordinator_local={}", caller.token))
            .header("origin", "http://127.0.0.1:8080")
            .header(
                "x-csrf-token",
                digest(&format!("coordinator-browser-csrf-v1:{}", caller.token)),
            );
    } else {
        request = request
            .header("authorization", format!("Bearer {}", caller.token))
            .header("x-coordinator-session", &caller.session)
            .header("x-coordinator-session-proof", &caller.proof);
    }
    let response = app
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn seed_authority_state(fixture: &Fixture) -> (String, String, String, String) {
    let project = Uuid::new_v4().to_string();
    let held_task = Uuid::new_v4().to_string();
    let decision_task = Uuid::new_v4().to_string();
    let attempt = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO projects(id,name,repository_url,target_branch,created_at) VALUES(?,'clock-project','https://example.test/clock.git','main',?)")
        .bind(&project).bind(BASE).execute(&fixture.state.pool).await.unwrap();
    sqlx::query("INSERT INTO policy_revisions(project_id,revision,data_json,actor_id,created_at) VALUES(?,1,'{}',?,?)")
        .bind(&project).bind(&fixture.admin.principal).bind(BASE).execute(&fixture.state.pool).await.unwrap();
    for (id, title) in [
        (&held_task, "held clock task"),
        (&decision_task, "decision clock task"),
    ] {
        sqlx::query("INSERT INTO tasks(id,project_id,title,description,acceptance_json,kind,priority,lifecycle,created_at,ready_since) VALUES(?,?,?,'clock safety','[\"safe\"]','general',1,'open',?,?)")
            .bind(id).bind(&project).bind(title).bind(BASE).bind(BASE).execute(&fixture.state.pool).await.unwrap();
        sqlx::query("INSERT INTO task_revisions(project_id,task_id,revision,data_json,actor_id,created_at) VALUES(?,?,1,'{}',?,?)")
            .bind(&project).bind(id).bind(&fixture.admin.principal).bind(BASE).execute(&fixture.state.pool).await.unwrap();
    }
    sqlx::query("INSERT INTO attempts(id,project_id,task_id,owner_id,session_id,credential_id,generation,state,mode,expires_at,last_heartbeat_at,last_progress_at,created_at,task_revision,policy_revision) VALUES(?,?,?,?,?,?,1,'active','work',?,?,?,?,1,1)")
        .bind(&attempt).bind(&project).bind(&held_task).bind(&fixture.agent.principal).bind(&fixture.agent.session).bind(&fixture.agent.credential)
        .bind(BASE+10_000).bind(BASE).bind(BASE).bind(BASE).execute(&fixture.state.pool).await.unwrap();
    sqlx::query("UPDATE tasks SET current_attempt_id=?,generation=1 WHERE id=?")
        .bind(&attempt)
        .bind(&held_task)
        .execute(&fixture.state.pool)
        .await
        .unwrap();
    let reservation = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO reservations(id,project_id,attempt_id,generation,state,created_by,created_at) VALUES(?,?,?,1,'held',?,?)")
        .bind(&reservation).bind(&project).bind(&attempt).bind(&fixture.agent.principal).bind(BASE).execute(&fixture.state.pool).await.unwrap();
    let job = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO jobs(id,producer_id,project_id,task_id,attempt_id,generation,runner_instance_id,workstation_id,label,source_revision,source_tree,reservation_id,created_at) VALUES(?,?,?,?,?,1,'runner','workstation','clock job','revision','tree',?,?)")
        .bind(&job).bind(Uuid::new_v4().to_string()).bind(&project).bind(&held_task).bind(&attempt).bind(&reservation).bind(BASE).execute(&fixture.state.pool).await.unwrap();
    let reporter = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO reporters(id,job_id,principal_id,credential_id,session_id,proof_hash,expires_at,renew_until,created_at) VALUES(?,?,?,?,?,?,?,?,?)")
        .bind(&reporter).bind(&job).bind(&fixture.agent.principal).bind(&fixture.agent.credential).bind(&fixture.agent.session).bind(digest(&secret()))
        .bind(BASE+10_000).bind(BASE+10_000).bind(BASE).execute(&fixture.state.pool).await.unwrap();
    let decision = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO decisions(id,project_id,question,options_json,rationale,required_actor,created_by,created_at) VALUES(?,?,'Proceed?','[\"Yes\",\"No\"]','Clock decision','either',?,?)")
        .bind(&decision).bind(&project).bind(&fixture.admin.principal).bind(BASE).execute(&fixture.state.pool).await.unwrap();
    sqlx::query("INSERT INTO decision_cycles(decision_id,generation,policy_revision,environment,conditions,expires_at,rationale,opened_by,created_at) VALUES(?,1,1,'test','Clock remains trusted',?,'Clock decision',?,?)")
        .bind(&decision).bind(BASE+10_000).bind(&fixture.admin.principal).bind(BASE).execute(&fixture.state.pool).await.unwrap();
    sqlx::query("INSERT INTO decision_affected_tasks(decision_id,generation,project_id,task_id,task_revision) VALUES(?,1,?,?,1)")
        .bind(&decision).bind(&project).bind(&decision_task).execute(&fixture.state.pool).await.unwrap();
    sqlx::query("INSERT INTO decision_answers(decision_id,generation,disposition,answer,rationale,actor_id,actor_session_id,conditions_confirmed,created_at) VALUES(?,1,'allow','Yes','Allowed before expiry',?,?,1,?)")
        .bind(&decision).bind(&fixture.admin.principal).bind(&fixture.admin.session).bind(BASE).execute(&fixture.state.pool).await.unwrap();
    (project, decision_task, attempt, reporter)
}

#[tokio::test]
async fn rollback_expires_authority_preserves_holds_and_never_revives_deadlines() {
    let fixture = Fixture::new().await;
    let (project, decision_task, attempt, reporter) = seed_authority_state(&fixture).await;
    let mut connection = fixture.state.pool.acquire().await.unwrap();
    ensure_decisions_resolved(&mut connection, &project, &decision_task, BASE)
        .await
        .unwrap();
    drop(connection);

    fixture.clock.set(BASE + 20_000);
    fixture.status().await;
    let (expired_status, _) = fixture
        .call(&fixture.expiring_agent, "GET", "/api/v1/me", json!({}))
        .await;
    assert_eq!(expired_status, StatusCode::UNAUTHORIZED);

    fixture.clock.set(BASE + 1_000);
    let (status, blocked) = fixture
        .call(
            &fixture.admin,
            "POST",
            "/api/v1/projects",
            json!({"name":"must-not-exist","repository_url":"https://example.test/no.git","target_branch":"main"}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{blocked}");
    assert_eq!(blocked["error"]["code"], "clock_reconciliation_required");
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM projects WHERE name='must-not-exist'")
            .fetch_one(&fixture.state.pool)
            .await
            .unwrap();
    assert_eq!(count, 0);

    let clock = fixture.status().await;
    assert_eq!(clock["clock_state"]["status"], "clock_reconciliation");
    assert_eq!(clock["material_rollback_threshold_ms"], 5_000);
    let attempt_row = sqlx::query("SELECT state FROM attempts WHERE id=?")
        .bind(&attempt)
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap();
    assert_eq!(attempt_row.get::<String, _>("state"), "expired");
    let pointer: String = sqlx::query_scalar(
        "SELECT current_attempt_id FROM tasks WHERE id=(SELECT task_id FROM attempts WHERE id=?)",
    )
    .bind(&attempt)
    .fetch_one(&fixture.state.pool)
    .await
    .unwrap();
    assert_eq!(pointer, attempt);
    let held: i64 =
        sqlx::query_scalar("SELECT count(*) FROM reservations WHERE attempt_id=? AND state='held'")
            .bind(&attempt)
            .fetch_one(&fixture.state.pool)
            .await
            .unwrap();
    assert_eq!(held, 1);
    let reporter_expiry: i64 = sqlx::query_scalar("SELECT expires_at FROM reporters WHERE id=?")
        .bind(&reporter)
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap();
    assert!(reporter_expiry >= BASE + 20_000);
    assert!(reporter_expiry <= clock["clock_state"]["last_safe_time_ms"].as_i64().unwrap());

    let first_safe = clock["clock_state"]["last_safe_time_ms"].as_i64().unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let later = fixture.status().await;
    assert!(later["clock_state"]["last_safe_time_ms"].as_i64().unwrap() > first_safe);

    let incident = later["clock_state"]["incident_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, too_early) = fixture
        .call(
            &fixture.admin,
            "POST",
            "/api/v1/admin/clock/reconcile",
            json!({"incident_id":incident,"reason":"The host clock source is being repaired."}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{too_early}");
    assert_eq!(too_early["error"]["code"], "clock_still_untrusted");

    let required = later["clock_state"]["last_safe_time_ms"].as_i64().unwrap();
    fixture.clock.set(required + 10);
    let (status, recovered) = fixture
        .call(
            &fixture.admin,
            "POST",
            "/api/v1/admin/clock/reconcile",
            json!({"incident_id":incident,"reason":"The host clock now agrees with an independent trusted source."}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{recovered}");
    assert_eq!(recovered["data"]["clock_state"]["status"], "ready");

    let mut connection = fixture.state.pool.acquire().await.unwrap();
    let decision = ensure_decisions_resolved(
        &mut connection,
        &project,
        &decision_task,
        fixture.state.now(),
    )
    .await
    .unwrap_err();
    assert_eq!(decision.code, "decision_required");
    let (expired_status, _) = fixture
        .call(&fixture.expiring_agent, "GET", "/api/v1/me", json!({}))
        .await;
    assert_eq!(expired_status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn restart_detects_persisted_rollback_and_host_recovery_is_attributed() {
    let fixture = Fixture::new().await;
    fixture.clock.set(BASE + 30_000);
    fixture.status().await;

    let mut reopened = AppState::open(fixture.state.config.clone()).await.unwrap();
    let restarted_clock = Arc::new(TestClock(AtomicI64::new(BASE)));
    reopened.clock = restarted_clock.clone();
    let app = router(reopened.clone());
    let (status, _) = call(
        app.clone(),
        &fixture.admin,
        "GET",
        "/api/v1/admin/clock",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let row = sqlx::query(
        "SELECT status,incident_id,last_safe_time_ms FROM clock_state WHERE singleton=1",
    )
    .fetch_one(&reopened.pool)
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("status"), "clock_reconciliation");
    let incident: String = row.get("incident_id");
    let high_water: i64 = row.get("last_safe_time_ms");
    let too_early = recover_clock(&reopened, "The host clock has not been corrected yet.")
        .await
        .unwrap_err();
    assert_eq!(too_early.code, "clock_still_untrusted");

    restarted_clock.set(high_water + 10);
    let recovered = recover_clock(
        &reopened,
        "The host clock now agrees with the trusted workstation clock.",
    )
    .await
    .unwrap();
    assert_eq!(recovered["clock_state"]["status"], "ready");
    let event = sqlx::query("SELECT initiator_kind,actor_id,reason FROM clock_reconciliation_events WHERE incident_id=? AND kind='clock_reconciled'")
        .bind(&incident).fetch_one(&reopened.pool).await.unwrap();
    assert_eq!(event.get::<String, _>("initiator_kind"), "host_operator");
    assert!(event.get::<Option<String>, _>("actor_id").is_none());
    assert_eq!(
        event.get::<String, _>("reason"),
        "The host clock now agrees with the trusted workstation clock."
    );
    drop(fixture.directory);
}

#[tokio::test]
async fn small_backward_adjustment_is_clamped_without_an_incident() {
    let fixture = Fixture::new().await;
    fixture.clock.set(BASE + 10_000);
    let before = fixture.status().await;
    fixture.clock.set(BASE + 7_000);
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    let after = fixture.status().await;
    assert_eq!(after["clock_state"]["status"], "ready");
    assert!(after["incident"].is_null());
    assert!(
        after["clock_state"]["last_safe_time_ms"].as_i64().unwrap()
            > before["clock_state"]["last_safe_time_ms"].as_i64().unwrap()
    );
}
