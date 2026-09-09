use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use coordinator_core::{INSTRUCTION_VERSION, REQUIRED_SECTION};
use coordinator_server::{
    auth::{digest, secret},
    router,
    state::{AppState, Clock, Config},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};
use tower::ServiceExt;
use uuid::Uuid;

struct TestClock(AtomicI64);
impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Clone)]
struct Caller {
    token: String,
    session: String,
    proof: String,
    principal: String,
    credential: String,
    workstation: String,
    human: bool,
}

struct Fixture {
    state: AppState,
    app: Router,
    clock: Arc<TestClock>,
    _dir: tempfile::TempDir,
    admin: Caller,
    a: Caller,
    b: Caller,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::open(Config {
            database_path: dir.path().join("jobs.sqlite3"),
            public_origin: "http://127.0.0.1:8080".into(),
            allow_insecure_loopback: true,
            ..Config::default()
        })
        .await
        .unwrap();
        let clock = Arc::new(TestClock(AtomicI64::new(1_800_000_000_000)));
        state.clock = clock.clone();
        let admin = seed(&state, true, "jobs-admin").await;
        let a = seed(&state, false, "jobs-agent-a").await;
        let b = seed(&state, false, "jobs-agent-b").await;
        Self {
            app: router(state.clone()),
            state,
            clock,
            _dir: dir,
            admin,
            a,
            b,
        }
    }

    async fn call(
        &self,
        caller: &Caller,
        method: &str,
        path: &str,
        key: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        call(self.app.clone(), caller, method, path, key, body).await
    }

    async fn reporter(
        &self,
        token: &str,
        method: &str,
        path: &str,
        key: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("Authorization", format!("Bearer {token}"));
        if method != "GET" {
            request = request
                .header("Content-Type", "application/json")
                .header("Idempotency-Key", key);
        }
        response(
            self.app
                .clone()
                .oneshot(request.body(Body::from(body.to_string())).unwrap())
                .await
                .unwrap(),
        )
        .await
    }

    async fn project(&self, name: &str) -> String {
        let (status, value) = self
            .call(
                &self.admin,
                "POST",
                "/api/v1/projects",
                &Uuid::new_v4().to_string(),
                json!({"name":name,"repository_url":"https://example.test/repository.git","target_branch":"main"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        value["data"]["id"].as_str().unwrap().into()
    }

    async fn claimed(&self, caller: &Caller, project: &str, title: &str) -> (String, String, i64) {
        let (status, task) = self
            .call(
                caller,
                "POST",
                &format!("/api/v1/projects/{project}/tasks"),
                &Uuid::new_v4().to_string(),
                json!({"title":title,"acceptance_criteria":["job evidence recorded"]}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{task}");
        let (status, ack) = self
            .call(
                caller,
                "POST",
                &format!(
                    "/api/v1/sessions/{}/instruction-acknowledgments",
                    caller.session
                ),
                &Uuid::new_v4().to_string(),
                json!({"project_id":project,"policy_revision":1,"instruction_version":INSTRUCTION_VERSION,"sections":[REQUIRED_SECTION]}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{ack}");
        let (status, claim) = self
            .call(
                caller,
                "POST",
                &format!("/api/v1/projects/{project}/claims"),
                &Uuid::new_v4().to_string(),
                json!({"task_id":task["data"]["id"],"expected_task_revision":1,"policy_revision":1,"instruction_version":INSTRUCTION_VERSION}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{claim}");
        (
            task["data"]["id"].as_str().unwrap().into(),
            claim["data"]["claim"]["attempt"]["id"]
                .as_str()
                .unwrap()
                .into(),
            claim["data"]["claim"]["attempt"]["generation"]
                .as_i64()
                .unwrap(),
        )
    }

    async fn resource(&self, key: &str, capacity: i64) -> String {
        let (status, value) = self
            .call(
                &self.admin,
                "POST",
                "/api/v1/resources",
                &Uuid::new_v4().to_string(),
                json!({"key":key,"capacity":capacity,"description":"test resource"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert_eq!(value["data"]["held_units"], 0);
        value["data"]["id"].as_str().unwrap().into()
    }

    async fn reserve(
        &self,
        caller: &Caller,
        project: &str,
        attempt: &str,
        generation: i64,
        items: Value,
    ) -> (StatusCode, Value) {
        self.call(
            caller,
            "POST",
            &format!("/api/v1/projects/{project}/attempts/{attempt}/reservations"),
            &Uuid::new_v4().to_string(),
            json!({"generation":generation,"items":items}),
        )
        .await
    }

    async fn checkout(&self, caller: &Caller, project: &str, attempt: &str, generation: i64) {
        let (status, value) = self
            .call(
                caller,
                "POST",
                &format!("/api/v1/projects/{project}/attempts/{attempt}/checkout"),
                &Uuid::new_v4().to_string(),
                json!({"generation":generation,"workstation_id":caller.workstation,"identity":Uuid::new_v4().to_string(),"path":"/tmp/job-worktree","branch":"job-test","base_revision":"0123456789abcdef","clean":true}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
    }

    async fn job(
        &self,
        caller: &Caller,
        project: &str,
        attempt: &str,
        generation: i64,
        reservation: &str,
        renew_for_seconds: i64,
    ) -> JobRegistration {
        let registration = JobRegistration {
            job: Uuid::new_v4().to_string(),
            producer: Uuid::new_v4().to_string(),
            runner: Uuid::new_v4().to_string(),
            reporter: Uuid::new_v4().to_string(),
            proof: secret(),
        };
        let (status, value) = self
            .call(
                caller,
                "POST",
                &format!("/api/v1/projects/{project}/attempts/{attempt}/jobs"),
                &Uuid::new_v4().to_string(),
                json!({"generation":generation,"job_id":registration.job,"producer_id":registration.producer,
                    "runner_instance_id":registration.runner,"workstation_id":caller.workstation,"label":"cargo test",
                    "source_revision":"0123456789abcdef","source_tree":"fedcba9876543210","reservation_id":reservation,
                    "reporter_id":registration.reporter,"reporter_proof":registration.proof,"renew_for_seconds":renew_for_seconds}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert_eq!(value["data"]["job"]["state"], "registered");
        assert!(!value.to_string().contains(&registration.proof));
        registration
    }
}

struct JobRegistration {
    job: String,
    producer: String,
    runner: String,
    reporter: String,
    proof: String,
}

impl JobRegistration {
    fn token(&self) -> String {
        format!("acr_{}.{}", self.reporter, self.proof)
    }
}

async fn seed(state: &AppState, human: bool, name: &str) -> Caller {
    let caller = Caller {
        token: secret(),
        session: Uuid::new_v4().to_string(),
        proof: secret(),
        principal: Uuid::new_v4().to_string(),
        credential: Uuid::new_v4().to_string(),
        workstation: Uuid::new_v4().to_string(),
        human,
    };
    sqlx::query(
        "INSERT INTO principals(id,name,kind,role,password_hash,created_at) VALUES(?,?,?,?,?,?)",
    )
    .bind(&caller.principal)
    .bind(name)
    .bind(if human { "human" } else { "agent" })
    .bind(if human { "admin" } else { "agent" })
    .bind(human.then_some("unused-test-password-hash"))
    .bind(state.now())
    .execute(&state.pool)
    .await
    .unwrap();
    if human {
        sqlx::query(
            "INSERT INTO browser_sessions(id,principal_id,token_hash,expires_at) VALUES(?,?,?,?)",
        )
        .bind(&caller.session)
        .bind(&caller.principal)
        .bind(digest(&caller.token))
        .bind(state.now() + 86_400_000)
        .execute(&state.pool)
        .await
        .unwrap();
    } else {
        sqlx::query(
            "INSERT INTO credentials(id,principal_id,token_hash,created_at) VALUES(?,?,?,?)",
        )
        .bind(&caller.credential)
        .bind(&caller.principal)
        .bind(digest(&caller.token))
        .bind(state.now())
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO agent_sessions(id,principal_id,credential_id,workstation_id,proof_hash,created_at,capabilities,harness) VALUES(?,?,?,?,?,?,'[]','test')",
        )
        .bind(&caller.session)
        .bind(&caller.principal)
        .bind(&caller.credential)
        .bind(&caller.workstation)
        .bind(digest(&caller.proof))
        .bind(state.now())
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
    key: &str,
    body: Value,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("Content-Type", "application/json");
    if method != "GET" {
        request = request.header("Idempotency-Key", key);
    }
    if caller.human {
        request = request
            .header("Cookie", format!("coordinator_local={}", caller.token))
            .header("Origin", "http://127.0.0.1:8080")
            .header(
                "X-CSRF-Token",
                digest(&format!("coordinator-browser-csrf-v1:{}", caller.token)),
            );
    } else {
        request = request
            .header("Authorization", format!("Bearer {}", caller.token))
            .header("X-Coordinator-Session", &caller.session)
            .header("X-Coordinator-Session-Proof", &caller.proof);
    }
    response(
        app.oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap(),
    )
    .await
}

async fn response(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| panic!("non-JSON response with status {status}"));
    (status, value)
}

#[tokio::test]
async fn reservations_are_atomic_and_capacity_is_global_across_projects() {
    let fixture = Fixture::new().await;
    let project_a = fixture.project("capacity-a").await;
    let project_b = fixture.project("capacity-b").await;
    let (_, attempt_a, generation_a) = fixture.claimed(&fixture.a, &project_a, "task a").await;
    let (_, attempt_b, generation_b) = fixture.claimed(&fixture.b, &project_b, "task b").await;
    let scarce = fixture.resource("device/gpu-0", 1).await;
    let free = fixture.resource("network/port-1", 1).await;

    let gate = Arc::new(tokio::sync::Barrier::new(2));
    let left = {
        let app = fixture.app.clone();
        let caller = fixture.a.clone();
        let project = project_a.clone();
        let attempt = attempt_a.clone();
        let resource = scarce.clone();
        let gate = gate.clone();
        tokio::spawn(async move {
            gate.wait().await;
            call(
                app,
                &caller,
                "POST",
                &format!("/api/v1/projects/{project}/attempts/{attempt}/reservations"),
                "race-a",
                json!({"generation":generation_a,"items":[{"resource_id":resource,"units":1}]}),
            )
            .await
            .0
        })
    };
    let right = {
        let app = fixture.app.clone();
        let caller = fixture.b.clone();
        let project = project_b.clone();
        let attempt = attempt_b.clone();
        let resource = scarce.clone();
        let gate = gate.clone();
        tokio::spawn(async move {
            gate.wait().await;
            call(
                app,
                &caller,
                "POST",
                &format!("/api/v1/projects/{project}/attempts/{attempt}/reservations"),
                "race-b",
                json!({"generation":generation_b,"items":[{"resource_id":resource,"units":1}]}),
            )
            .await
            .0
        })
    };
    let statuses = [left.await.unwrap(), right.await.unwrap()];
    assert_eq!(statuses.iter().filter(|&&s| s == StatusCode::OK).count(), 1);
    assert_eq!(
        statuses
            .iter()
            .filter(|&&s| s == StatusCode::CONFLICT)
            .count(),
        1
    );

    let (loser, project, attempt, generation) = if statuses[0] == StatusCode::OK {
        (&fixture.b, &project_b, &attempt_b, generation_b)
    } else {
        (&fixture.a, &project_a, &attempt_a, generation_a)
    };
    let (status, error) = fixture
        .reserve(
            loser,
            project,
            attempt,
            generation,
            json!([{"resource_id":free,"units":1},{"resource_id":scarce,"units":1}]),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], "resource_unavailable");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM reservation_items WHERE resource_id=?")
            .bind(&free)
            .fetch_one(&fixture.state.pool)
            .await
            .unwrap(),
        0
    );

    fixture.clock.0.fetch_add(600_000, Ordering::SeqCst);
    let (_, resources) = fixture
        .call(&fixture.admin, "GET", "/api/v1/resources", "", json!({}))
        .await;
    let held = resources["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == scarce)
        .unwrap();
    assert_eq!(held["held_units"], 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM reservations WHERE state='held'")
            .fetch_one(&fixture.state.pool)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn scoped_reporter_survives_lease_and_session_loss_but_parent_revocation_stops_it() {
    let fixture = Fixture::new().await;
    let project = fixture.project("reporter-auth").await;
    let (_, attempt, generation) = fixture.claimed(&fixture.a, &project, "producer task").await;
    fixture
        .checkout(&fixture.a, &project, &attempt, generation)
        .await;
    let resource = fixture.resource("device/test-runner", 1).await;
    let (_, reservation) = fixture
        .reserve(
            &fixture.a,
            &project,
            &attempt,
            generation,
            json!([{"resource_id":resource,"units":1}]),
        )
        .await;
    let reservation_id = reservation["data"]["id"].as_str().unwrap();
    let registered = fixture
        .job(
            &fixture.a,
            &project,
            &attempt,
            generation,
            reservation_id,
            3600,
        )
        .await;
    let token = registered.token();

    let (status, detail) = fixture
        .reporter(
            &token,
            "GET",
            &format!("/api/v1/reporters/{}", registered.reporter),
            "",
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["data"]["reporter"]["launch_allowed"], true);
    assert!(
        detail["data"]["reporter"]["lease_remaining_ms"]
            .as_i64()
            .unwrap()
            > 0
    );

    let (status, _) = fixture
        .reporter(
            &format!("acr_{}.{}", registered.reporter, secret()),
            "GET",
            &format!("/api/v1/reporters/{}", registered.reporter),
            "",
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = fixture
        .reporter(&token, "GET", "/api/v1/resources", "", json!({}))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    fixture.clock.0.fetch_add(600_000, Ordering::SeqCst);
    sqlx::query("UPDATE agent_sessions SET closed_at=? WHERE id=?")
        .bind(fixture.state.now())
        .bind(&fixture.a.session)
        .execute(&fixture.state.pool)
        .await
        .unwrap();
    let (_, expired) = fixture
        .reporter(
            &token,
            "GET",
            &format!("/api/v1/reporters/{}", registered.reporter),
            "",
            json!({}),
        )
        .await;
    assert_eq!(expired["data"]["reporter"]["launch_allowed"], false);
    assert_eq!(expired["data"]["reporter"]["lease_remaining_ms"], 0);
    assert_eq!(expired["data"]["reporter"]["observation_authorized"], true);

    let observation = json!({"sequence":1,"producer_id":registered.producer,"state":"running","pid":4123,
        "process_started_at":"linux-proc-start:9911","exit_code":null,"inputs_unchanged":null,"summary":"still running"});
    let path = format!("/api/v1/reporters/{}/observations", registered.reporter);
    let (status, first) = fixture
        .reporter(&token, "POST", &path, "observe-one", observation.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let (status, replay) = fixture
        .reporter(&token, "POST", &path, "observe-replay", observation)
        .await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay["data"]["replayed"], true);
    assert_eq!(replay["data"]["job"]["observation_freshness"], "fresh");

    let (status, regression) = fixture
        .reporter(
            &token,
            "POST",
            &path,
            "regression",
            json!({"sequence":2,"producer_id":registered.producer,"state":"not_started","pid":null,
                "process_started_at":null,"exit_code":null,"inputs_unchanged":true,"summary":"launch failed"}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{regression}");
    assert_eq!(regression["error"]["code"], "job_already_launched");

    let stored_proof: String = sqlx::query_scalar("SELECT proof_hash FROM reporters WHERE id=?")
        .bind(&registered.reporter)
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap();
    assert_eq!(stored_proof, digest(&registered.proof));
    assert_ne!(stored_proof, registered.proof);
    for value in sqlx::query_scalar::<_, String>(
        "SELECT result_json FROM mutation_receipts UNION ALL SELECT data_json FROM events",
    )
    .fetch_all(&fixture.state.pool)
    .await
    .unwrap()
    {
        assert!(!value.contains(&registered.proof));
    }

    sqlx::query("UPDATE credentials SET revoked_at=? WHERE id=?")
        .bind(fixture.state.now())
        .bind(&fixture.a.credential)
        .execute(&fixture.state.pool)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .reporter(
                &token,
                "POST",
                &path,
                "after-revoke",
                json!({"sequence":2,"producer_id":registered.producer,"state":"unknown","pid":4123,
                    "process_started_at":"linux-proc-start:9911","exit_code":null,"inputs_unchanged":true,"summary":"observer lost"}),
            )
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn reporter_renewal_extends_only_live_parent_ownership() {
    let fixture = Fixture::new().await;
    let project = fixture.project("reporter-renewal").await;
    let (_, attempt, generation) = fixture.claimed(&fixture.a, &project, "renewed job").await;
    fixture
        .checkout(&fixture.a, &project, &attempt, generation)
        .await;
    let resource = fixture.resource("device/renewal", 1).await;
    let (_, reservation) = fixture
        .reserve(
            &fixture.a,
            &project,
            &attempt,
            generation,
            json!([{"resource_id":resource,"units":1}]),
        )
        .await;
    let registered = fixture
        .job(
            &fixture.a,
            &project,
            &attempt,
            generation,
            reservation["data"]["id"].as_str().unwrap(),
            3600,
        )
        .await;
    fixture.clock.0.fetch_add(500_000, Ordering::SeqCst);
    let path = format!("/api/v1/reporters/{}/renew", registered.reporter);
    let (status, renewed) = fixture
        .reporter(
            &registered.token(),
            "POST",
            &path,
            "reporter-renew",
            json!({"generation":generation}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{renewed}");
    assert_eq!(renewed["data"]["lease_remaining_ms"], 600_000);
    fixture.clock.0.fetch_add(10_000, Ordering::SeqCst);
    let (status, replay) = fixture
        .reporter(
            &registered.token(),
            "POST",
            &path,
            "reporter-renew",
            json!({"generation":generation}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay["data"]["lease_remaining_ms"], 590_000);
    assert_eq!(replay["data"]["replayed"], true);
}

#[tokio::test]
async fn terminal_observation_allows_release_and_never_releases_automatically() {
    let fixture = Fixture::new().await;
    let project = fixture.project("terminal-release").await;
    let (_, attempt, generation) = fixture.claimed(&fixture.a, &project, "terminal job").await;
    fixture
        .checkout(&fixture.a, &project, &attempt, generation)
        .await;
    let resource = fixture.resource("device/terminal", 1).await;
    let (_, reservation) = fixture
        .reserve(
            &fixture.a,
            &project,
            &attempt,
            generation,
            json!([{"resource_id":resource,"units":1}]),
        )
        .await;
    let reservation_id = reservation["data"]["id"].as_str().unwrap().to_owned();
    let registered = fixture
        .job(
            &fixture.a,
            &project,
            &attempt,
            generation,
            &reservation_id,
            3600,
        )
        .await;
    let path = format!("/api/v1/reporters/{}/observations", registered.reporter);
    let (status, result) = fixture
        .reporter(
            &registered.token(),
            "POST",
            &path,
            "terminal",
            json!({"sequence":1,"producer_id":registered.producer,"state":"succeeded","pid":333,
                "process_started_at":"win-create-time:123","exit_code":0,"inputs_unchanged":true,"summary":"passed"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM reservations WHERE id=?")
            .bind(&reservation_id)
            .fetch_one(&fixture.state.pool)
            .await
            .unwrap(),
        "held"
    );
    assert_eq!(
        fixture
            .reporter(
                &registered.token(),
                "POST",
                &format!("/api/v1/reporters/{}/renew", registered.reporter),
                "renew-terminal",
                json!({"generation":generation}),
            )
            .await
            .0,
        StatusCode::CONFLICT
    );
    let (status, released) = fixture
        .call(
            &fixture.a,
            "POST",
            &format!("/api/v1/projects/{project}/reservations/{reservation_id}/release"),
            "release-reservation",
            json!({"generation":generation,"reason":"terminal result inspected"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{released}");
    assert_eq!(released["data"]["state"], "released");
}

#[tokio::test]
async fn human_resolution_reconciles_unknown_job_without_rewriting_producer_state() {
    let fixture = Fixture::new().await;
    let project = fixture.project("human-resolution").await;
    let (task, attempt, generation) = fixture.claimed(&fixture.a, &project, "uncertain job").await;
    fixture
        .checkout(&fixture.a, &project, &attempt, generation)
        .await;
    let resource = fixture.resource("device/recovery", 1).await;
    let (_, reservation) = fixture
        .reserve(
            &fixture.a,
            &project,
            &attempt,
            generation,
            json!([{"resource_id":resource,"units":1}]),
        )
        .await;
    let reservation_id = reservation["data"]["id"].as_str().unwrap().to_owned();
    let registered = fixture
        .job(
            &fixture.a,
            &project,
            &attempt,
            generation,
            &reservation_id,
            0,
        )
        .await;
    let (status, denied) = fixture
        .call(
            &fixture.a,
            "POST",
            &format!("/api/v1/projects/{project}/attempts/{attempt}/release"),
            "release-with-hold",
            json!({"generation":generation,"summary":"pause","blocked":false}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{denied}");
    assert_eq!(denied["error"]["code"], "attempt_evidence_unresolved");

    let (status, blocked) = fixture
        .call(
            &fixture.a,
            "POST",
            &format!("/api/v1/projects/{project}/attempts/{attempt}/release"),
            "blocked-with-hold",
            json!({"generation":generation,"summary":"producer state uncertain","blocked":true}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{blocked}");
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM reservations WHERE id=?")
            .bind(&reservation_id)
            .fetch_one(&fixture.state.pool)
            .await
            .unwrap(),
        "held"
    );

    let (status, resolved) = fixture
        .call(
            &fixture.admin,
            "POST",
            &format!("/api/v1/projects/{project}/reservations/{reservation_id}/resolve"),
            "human-resolution",
            json!({"reason":"physical inspection complete","evidence":"operator verified the isolated runner is powered off"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{resolved}");
    assert_eq!(resolved["data"]["state"], "resolved");
    let (_, detail) = fixture
        .call(
            &fixture.admin,
            "GET",
            &format!("/api/v1/projects/{project}/jobs/{}", registered.job),
            "",
            json!({}),
        )
        .await;
    assert_eq!(detail["data"]["state"], "registered");
    assert!(detail["data"]["reconciled_at"].is_string());
    assert_eq!(
        detail["data"]["reconciliation_evidence"],
        "operator verified the isolated runner is powered off"
    );
    coordinator_server::jobs::ensure_attempt_quiescent(
        &mut fixture.state.pool.acquire().await.unwrap(),
        &project,
        &task,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn recovery_owner_can_release_an_old_terminal_reservation() {
    let fixture = Fixture::new().await;
    let project = fixture.project("recovery-release").await;
    let (task, attempt, generation) = fixture
        .claimed(&fixture.a, &project, "recover terminal")
        .await;
    fixture
        .checkout(&fixture.a, &project, &attempt, generation)
        .await;
    let resource = fixture.resource("device/recover-terminal", 1).await;
    let (_, reservation) = fixture
        .reserve(
            &fixture.a,
            &project,
            &attempt,
            generation,
            json!([{"resource_id":resource,"units":1}]),
        )
        .await;
    let reservation_id = reservation["data"]["id"].as_str().unwrap().to_owned();
    let registered = fixture
        .job(
            &fixture.a,
            &project,
            &attempt,
            generation,
            &reservation_id,
            0,
        )
        .await;
    fixture
        .reporter(
            &registered.token(),
            "POST",
            &format!(
                "/api/v1/reporters/{}/observations",
                registered.reporter
            ),
            "not-started",
            json!({"sequence":1,"producer_id":registered.producer,"state":"not_started","pid":null,
                "process_started_at":null,"exit_code":null,"inputs_unchanged":true,"summary":"executable missing before launch"}),
        )
        .await;
    fixture.clock.0.fetch_add(600_000, Ordering::SeqCst);
    let (status, ack) = fixture
        .call(
            &fixture.b,
            "POST",
            &format!(
                "/api/v1/sessions/{}/instruction-acknowledgments",
                fixture.b.session
            ),
            "recovery-ack",
            json!({"project_id":project,"policy_revision":1,"instruction_version":INSTRUCTION_VERSION,"sections":[REQUIRED_SECTION]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{ack}");
    let (status, recovery) = fixture
        .call(
            &fixture.b,
            "POST",
            &format!("/api/v1/projects/{project}/claims"),
            "recovery-claim",
            json!({"task_id":task,"expected_task_revision":1,"mode":"recovery","policy_revision":1,"instruction_version":INSTRUCTION_VERSION}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{recovery}");
    let recovery_attempt = recovery["data"]["claim"]["attempt"]["id"].as_str().unwrap();
    let recovery_generation = recovery["data"]["claim"]["attempt"]["generation"]
        .as_i64()
        .unwrap();
    let (status, released) = fixture
        .call(
            &fixture.b,
            "POST",
            &format!("/api/v1/projects/{project}/reservations/{reservation_id}/release"),
            "recovery-release",
            json!({"generation":recovery_generation,"reason":"prelaunch failure verified"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{released}");
    assert_eq!(released["data"]["state"], "released");
    let (status, resolution) = fixture
        .call(
            &fixture.b,
            "POST",
            &format!("/api/v1/projects/{project}/attempts/{recovery_attempt}/recovery-resolution"),
            "recovery-resolution",
            json!({"generation":recovery_generation,"disposition":"restart","summary":"old producer never launched",
                "saved_work_checked":true,"running_jobs_checked":true}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{resolution}");
}
