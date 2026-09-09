use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
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
            database_path: dir.path().join("test.sqlite3"),
            public_origin: "http://127.0.0.1:8080".into(),
            allow_insecure_loopback: true,
            ..Config::default()
        })
        .await
        .unwrap();
        let clock = Arc::new(TestClock(AtomicI64::new(1_800_000_000_000)));
        state.clock = clock.clone();
        let admin = seed(&state, true, "admin").await;
        let a = seed(&state, false, "agent-a").await;
        let b = seed(&state, false, "agent-b").await;
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
        c: &Caller,
        method: &str,
        path: &str,
        key: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        call(self.app.clone(), c, method, path, key, body).await
    }
    async fn project(&self, name: &str) -> String {
        let (status,v)=self.call(&self.admin,"POST","/api/v1/projects",&Uuid::new_v4().to_string(),json!({"name":name,"repository_url":"https://example.com/test/repo.git","target_branch":"main"})).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        v["data"]["id"].as_str().unwrap().into()
    }
    async fn task(&self, p: &str, title: &str, deps: Vec<String>) -> Value {
        let (status,v)=self.call(&self.a,"POST",&format!("/api/v1/projects/{p}/tasks"),&Uuid::new_v4().to_string(),json!({"title":title,"description":"Test task","acceptance_criteria":["Required behavior verified"],"kind":"code","depends_on":deps})).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        v["data"].clone()
    }
    async fn ack(&self, c: &Caller, p: &str) {
        let (status,v)=self.call(c,"POST",&format!("/api/v1/sessions/{}/instruction-acknowledgments",c.session),&Uuid::new_v4().to_string(),json!({"project_id":p,"policy_revision":1,"instruction_version":"2","sections":["coordination-v2"]})).await;
        assert_eq!(status, StatusCode::OK, "{v}");
    }
    async fn claim(
        &self,
        c: &Caller,
        p: &str,
        t: &Value,
        key: &str,
        mode: &str,
    ) -> (StatusCode, Value) {
        self.call(c,"POST",&format!("/api/v1/projects/{p}/claims"),key,json!({"task_id":t["id"],"expected_task_revision":t["revision"],"mode":mode,"policy_revision":1,"instruction_version":"2"})).await
    }
}
async fn seed(state: &AppState, human: bool, name: &str) -> Caller {
    let c = Caller {
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
    .bind(&c.principal)
    .bind(name)
    .bind(if human { "human" } else { "agent" })
    .bind(if human { "admin" } else { "agent" })
    .bind(if human {
        Some("unused-test-password-hash")
    } else {
        None
    })
    .bind(state.now())
    .execute(&state.pool)
    .await
    .unwrap();
    if human {
        sqlx::query(
            "INSERT INTO browser_sessions(id,principal_id,token_hash,expires_at) VALUES(?,?,?,?)",
        )
        .bind(&c.session)
        .bind(&c.principal)
        .bind(digest(&c.token))
        .bind(state.now() + 86_400_000)
        .execute(&state.pool)
        .await
        .unwrap();
    } else {
        sqlx::query(
            "INSERT INTO credentials(id,principal_id,token_hash,created_at) VALUES(?,?,?,?)",
        )
        .bind(&c.credential)
        .bind(&c.principal)
        .bind(digest(&c.token))
        .bind(state.now())
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO agent_sessions(id,principal_id,credential_id,workstation_id,proof_hash,created_at,capabilities,harness) VALUES(?,?,?,?,?,?,'[]','test')")
            .bind(&c.session).bind(&c.principal).bind(&c.credential).bind(name).bind(digest(&c.proof)).bind(state.now()).execute(&state.pool).await.unwrap();
    }
    c
}
async fn call(
    app: Router,
    c: &Caller,
    method: &str,
    path: &str,
    key: &str,
    body: Value,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("Content-Type", "application/json")
        .header("Idempotency-Key", key);
    if c.human {
        req = req
            .header("Cookie", format!("coordinator_local={}", c.token))
            .header("Origin", "http://127.0.0.1:8080")
            .header(
                "X-CSRF-Token",
                digest(&format!("coordinator-browser-csrf-v1:{}", c.token)),
            );
    } else {
        req = req
            .header("Authorization", format!("Bearer {}", c.token))
            .header("X-Coordinator-Session", &c.session)
            .header("X-Coordinator-Session-Proof", &c.proof);
    }
    let res = app
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| panic!("non-JSON response status {status}")),
    )
}
fn attempt(v: &Value) -> (String, i64) {
    (
        v["data"]["claim"]["attempt"]["id"].as_str().unwrap().into(),
        v["data"]["claim"]["attempt"]["generation"]
            .as_i64()
            .unwrap(),
    )
}

#[tokio::test]
async fn competing_claims_have_exactly_one_owner_and_one_generation() {
    let f = Fixture::new().await;
    let p = f.project("race").await;
    let t = f.task(&p, "one task", vec![]).await;
    f.ack(&f.a, &p).await;
    f.ack(&f.b, &p).await;
    let barrier = Arc::new(tokio::sync::Barrier::new(20));
    let mut workers = Vec::new();
    for i in 0..20 {
        let app = f.app.clone();
        let c = if i % 2 == 0 { f.a.clone() } else { f.b.clone() };
        let gate = barrier.clone();
        let path = format!("/api/v1/projects/{p}/claims");
        let body = json!({"task_id":t["id"],"expected_task_revision":1,"policy_revision":1,"instruction_version":"2"});
        workers.push(tokio::spawn(async move {
            gate.wait().await;
            call(app, &c, "POST", &path, &format!("race-{i}"), body)
                .await
                .0
        }));
    }
    let mut successes = 0;
    for worker in workers {
        match worker.await.unwrap() {
            StatusCode::OK => successes += 1,
            StatusCode::CONFLICT => (),
            s => panic!("unexpected status {s}"),
        }
    }
    assert_eq!(successes, 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM attempts")
            .fetch_one(&f.state.pool)
            .await
            .unwrap(),
        1
    );
}
#[tokio::test]
async fn lost_claim_response_replays_original_and_never_revives_expired_authority() {
    let f = Fixture::new().await;
    let p = f.project("retry").await;
    let t = f.task(&p, "task", vec![]).await;
    f.ack(&f.a, &p).await;
    let (status, v) = f.claim(&f.a, &p, &t, "same-key", "work").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let original = attempt(&v);
    let (_, replay) = f.claim(&f.a, &p, &t, "same-key", "work").await;
    assert_eq!(attempt(&replay), original);
    assert_eq!(replay["data"]["current_authority"]["valid"], true);
    f.clock.0.fetch_add(600_000, Ordering::SeqCst);
    let (_, old) = f.claim(&f.a, &p, &t, "same-key", "work").await;
    assert_eq!(old["data"]["current_authority"]["valid"], false);
    let (status, _) = f
        .call(
            &f.a,
            "POST",
            &format!("/api/v1/projects/{p}/attempts/{}/renew", original.0),
            "late-renew",
            json!({"generation":original.1}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
}
#[tokio::test]
async fn replayed_renewal_does_not_restart_the_lease_countdown() {
    let f = Fixture::new().await;
    let p = f.project("renew-replay").await;
    let t = f.task(&p, "task", vec![]).await;
    f.ack(&f.a, &p).await;
    let (_, v) = f.claim(&f.a, &p, &t, "claim", "work").await;
    let (id, g) = attempt(&v);
    let path = format!("/api/v1/projects/{p}/attempts/{id}/renew");
    let (status, first) = f
        .call(&f.a, "POST", &path, "renew", json!({"generation":g}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["data"]["lease_remaining_ms"], 600_000);
    f.clock.0.fetch_add(590_000, Ordering::SeqCst);
    let (_, replay) = f
        .call(&f.a, "POST", &path, "renew", json!({"generation":g}))
        .await;
    assert_eq!(
        replay["data"]["attempt"]["expires_at"],
        first["data"]["attempt"]["expires_at"]
    );
    assert_eq!(replay["data"]["lease_remaining_ms"], 10_000);
    f.clock.0.fetch_add(10_000, Ordering::SeqCst);
    assert_eq!(
        f.call(&f.a, "POST", &path, "renew", json!({"generation":g}))
            .await
            .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn ownership_and_receipts_survive_a_service_restart() {
    let f = Fixture::new().await;
    let p = f.project("restart").await;
    let t = f.task(&p, "task", vec![]).await;
    f.ack(&f.a, &p).await;
    let (_, first) = f.claim(&f.a, &p, &t, "persisted-claim", "work").await;
    f.state.pool.close().await;
    let mut reopened = AppState::open(f.state.config.clone()).await.unwrap();
    reopened.clock = f.clock.clone();
    let (status, replay) = call(router(reopened), &f.a, "POST", &format!("/api/v1/projects/{p}/claims"), "persisted-claim", json!({"task_id":t["id"],"expected_task_revision":1,"policy_revision":1,"instruction_version":"2"})).await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(attempt(&first), attempt(&replay));
    assert_eq!(replay["data"]["current_authority"]["valid"], true);
}

#[tokio::test]
async fn stale_owned_mutations_do_not_replay_as_current_success() {
    let f = Fixture::new().await;
    let p = f.project("stale-replay").await;
    let t = f.task(&p, "task", vec![]).await;
    f.ack(&f.a, &p).await;
    let (_, v) = f.claim(&f.a, &p, &t, "claim", "work").await;
    let (id, g) = attempt(&v);
    let checkpoint = json!({"generation":g,"summary":"Saved work"});
    let checkout = json!({"generation":g,"workstation_id":"agent-a","identity":"repo/worktree-1","path":"/tmp/worktree-1","branch":"task-1","base_revision":"abc123","clean":true});
    for (op, body) in [("checkpoints", &checkpoint), ("checkout", &checkout)] {
        let (status, v) = f
            .call(
                &f.a,
                "POST",
                &format!("/api/v1/projects/{p}/attempts/{id}/{op}"),
                op,
                body.clone(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{v}");
    }
    f.clock.0.fetch_add(600_000, Ordering::SeqCst);
    for (op, body) in [("checkpoints", checkpoint), ("checkout", checkout)] {
        assert_eq!(
            f.call(
                &f.a,
                "POST",
                &format!("/api/v1/projects/{p}/attempts/{id}/{op}"),
                op,
                body
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
    }
}

#[tokio::test]
async fn recovery_fences_old_owner_and_requires_inspection_before_requeue() {
    let f = Fixture::new().await;
    let p = f.project("recover").await;
    let t = f.task(&p, "task", vec![]).await;
    f.ack(&f.a, &p).await;
    f.ack(&f.b, &p).await;
    let (_, v) = f.claim(&f.a, &p, &t, "initial", "work").await;
    let (old, generation) = attempt(&v);
    f.clock.0.fetch_add(600_000, Ordering::SeqCst);
    assert_eq!(
        f.claim(&f.b, &p, &t, "wrong-mode", "work").await.0,
        StatusCode::CONFLICT
    );
    let (status, v) = f.claim(&f.b, &p, &t, "takeover", "recovery").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (new, new_generation) = attempt(&v);
    assert_eq!(new_generation, generation + 1);
    assert_eq!(
        f.call(
            &f.a,
            "POST",
            &format!("/api/v1/projects/{p}/attempts/{old}/checkpoints"),
            "old-write",
            json!({"generation":generation,"summary":"late update"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        f.call(
            &f.b,
            "POST",
            &format!("/api/v1/projects/{p}/attempts/{new}/release"),
            "unresolved",
            json!({"generation":new_generation,"summary":"skip inspection"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (status,v)=f.call(&f.b,"POST",&format!("/api/v1/projects/{p}/attempts/{new}/recovery-resolution"),"inspect",json!({"generation":new_generation,"disposition":"resume","summary":"Checked saved branch and producer; no running jobs.","saved_work_checked":true,"running_jobs_checked":true})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        f.call(
            &f.b,
            "POST",
            &format!("/api/v1/projects/{p}/attempts/{new}/release"),
            "release",
            json!({"generation":new_generation,"summary":"Saved inspected work for next attempt."})
        )
        .await
        .0,
        StatusCode::OK
    );
}
#[tokio::test]
async fn renewal_waiting_for_writer_lock_samples_time_after_lock() {
    let f = Fixture::new().await;
    let p = f.project("lock").await;
    let t = f.task(&p, "task", vec![]).await;
    f.ack(&f.a, &p).await;
    let (_, v) = f.claim(&f.a, &p, &t, "claim", "work").await;
    let (id, g) = attempt(&v);
    let held = f.state.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let app = f.app.clone();
    let c = f.a.clone();
    let path = format!("/api/v1/projects/{p}/attempts/{id}/renew");
    let started = tokio::sync::oneshot::channel();
    let tx = started.0;
    let rx = started.1;
    let renewal = tokio::spawn(async move {
        tx.send(()).unwrap();
        call(
            app,
            &c,
            "POST",
            &path,
            "waiting-renew",
            json!({"generation":g}),
        )
        .await
    });
    rx.await.unwrap();
    tokio::task::yield_now().await;
    f.clock.0.fetch_add(600_000, Ordering::SeqCst);
    held.commit().await.unwrap();
    let (status, v) = renewal.await.unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
}
#[tokio::test]
async fn sessions_cannot_borrow_one_anothers_attempts_even_with_same_agent_token() {
    let f = Fixture::new().await;
    let p = f.project("isolation").await;
    let t = f.task(&p, "task", vec![]).await;
    f.ack(&f.a, &p).await;
    let (_, v) = f.claim(&f.a, &p, &t, "claim", "work").await;
    let (id, g) = attempt(&v);
    let mut second = f.a.clone();
    second.session = Uuid::new_v4().to_string();
    second.proof = secret();
    sqlx::query("INSERT INTO agent_sessions(id,principal_id,credential_id,workstation_id,proof_hash,created_at,capabilities,harness) VALUES(?,?,?,'agent-a',?,?,'[]','second')")
        .bind(&second.session).bind(&second.principal).bind(&second.credential).bind(digest(&second.proof)).bind(f.state.now()).execute(&f.state.pool).await.unwrap();
    assert_eq!(
        f.call(
            &second,
            "POST",
            &format!("/api/v1/projects/{p}/attempts/{id}/renew"),
            "other-session",
            json!({"generation":g})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    second.session = f.a.session.clone();
    assert_eq!(
        f.call(&second, "GET", "/api/v1/projects", "none", json!({}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}
#[tokio::test]
async fn dependency_cycles_rollback_edits_and_blocked_tasks_are_not_claimed() {
    let f = Fixture::new().await;
    let p = f.project("deps").await;
    let t1 = f.task(&p, "first", vec![]).await;
    let t2 = f
        .task(&p, "second", vec![t1["id"].as_str().unwrap().into()])
        .await;
    f.ack(&f.a, &p).await;
    assert_eq!(t2["work_status"], "blocked");
    assert_eq!(
        f.claim(&f.a, &p, &t2, "blocked", "work").await.0,
        StatusCode::CONFLICT
    );
    let edit = json!({"expected_revision":1,"title":"changed","description":"","acceptance_criteria":["done"],"priority":2,"depends_on":[t2["id"]],"planned":false});
    let (status, _) = f
        .call(
            &f.a,
            "PATCH",
            &format!("/api/v1/projects/{p}/tasks/{}", t1["id"].as_str().unwrap()),
            "cycle",
            edit,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (_, v) = f
        .call(
            &f.a,
            "GET",
            &format!("/api/v1/projects/{p}/tasks/{}", t1["id"].as_str().unwrap()),
            "read",
            json!({}),
        )
        .await;
    assert_eq!(v["data"]["revision"], 1);
    assert_eq!(v["data"]["title"], "first");
}
#[tokio::test]
async fn all_project_access_does_not_allow_cross_project_relationships() {
    let f = Fixture::new().await;
    let p = f.project("one").await;
    let q = f.project("two").await;
    let t = f.task(&p, "a", vec![]).await;
    let (status, v) = f
        .call(&f.b, "GET", "/api/v1/projects", "read", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["data"]["items"].as_array().unwrap().len(), 2);
    assert_eq!(
        f.call(
            &f.b,
            "POST",
            &format!("/api/v1/projects/{q}/tasks"),
            "cross",
            json!({"title":"bad","acceptance_criteria":["done"],"depends_on":[t["id"]]})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tasks WHERE project_id=?")
            .bind(q)
            .fetch_one(&f.state.pool)
            .await
            .unwrap(),
        0
    );
}
#[tokio::test]
async fn revocation_invalidates_replays_and_makes_ownership_recoverable() {
    let f = Fixture::new().await;
    let p = f.project("revoked").await;
    let t = f.task(&p, "task", vec![]).await;
    f.ack(&f.a, &p).await;
    f.ack(&f.b, &p).await;
    assert_eq!(
        f.claim(&f.a, &p, &t, "original", "work").await.0,
        StatusCode::OK
    );
    assert_eq!(
        f.call(
            &f.admin,
            "POST",
            &format!("/api/v1/admin/credentials/{}/revoke", f.a.credential),
            "revoke",
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        f.claim(&f.a, &p, &t, "original", "work").await.0,
        StatusCode::UNAUTHORIZED
    );
    let (status, v) = f.claim(&f.b, &p, &t, "recover", "recovery").await;
    assert_eq!(status, StatusCode::OK, "{v}");
}
#[tokio::test]
async fn checkpoints_preserve_deadline_and_release_does_not_complete_code() {
    let f = Fixture::new().await;
    let p = f.project("progress").await;
    let t = f.task(&p, "task", vec![]).await;
    f.ack(&f.a, &p).await;
    let (_, v) = f.claim(&f.a, &p, &t, "claim", "work").await;
    let (id, g) = attempt(&v);
    let expires = v["data"]["claim"]["attempt"]["expires_at"].clone();
    f.clock.0.fetch_add(120_000, Ordering::SeqCst);
    assert_eq!(f.call(&f.a,"POST",&format!("/api/v1/projects/{p}/attempts/{id}/checkpoints"),"progress",json!({"generation":g,"summary":"Tests are running","current_action":"Observe existing test process"})).await.0,StatusCode::OK);
    let (_, status) = f
        .call(
            &f.a,
            "GET",
            &format!("/api/v1/projects/{p}/attempts/{id}"),
            "read",
            json!({}),
        )
        .await;
    assert_eq!(status["data"]["attempt"]["expires_at"], expires);
    let (_, result) = f
        .call(
            &f.a,
            "POST",
            &format!("/api/v1/projects/{p}/attempts/{id}/release"),
            "release",
            json!({"generation":g,"summary":"Paused with a checkpoint"}),
        )
        .await;
    assert_eq!(result["data"]["task"]["lifecycle"], "open");
}
#[tokio::test]
async fn claims_require_current_instructions_and_policy_delegation_is_separate() {
    let f = Fixture::new().await;
    let p = f.project("policy").await;
    let t = f.task(&p, "task", vec![]).await;
    assert_eq!(
        f.claim(&f.a, &p, &t, "unread", "work").await.0,
        StatusCode::CONFLICT
    );
    let change = json!({"expected_revision":1,"review_mode":"agent","recovery_mode":"agent","lease_seconds":600,"rules":"Read all evidence","agent_rule_editing":true,"automatic_integration":true});
    assert_eq!(
        f.call(
            &f.a,
            "PATCH",
            &format!("/api/v1/projects/{p}/policy"),
            "self-grant",
            change.clone()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        f.call(
            &f.admin,
            "PATCH",
            &format!("/api/v1/projects/{p}/policy"),
            "delegate",
            change.clone()
        )
        .await
        .0,
        StatusCode::OK
    );
    let mut allowed = change;
    allowed["expected_revision"] = json!(2);
    allowed["rules"] = json!("Use exact commit evidence");
    assert_eq!(
        f.call(
            &f.a,
            "PATCH",
            &format!("/api/v1/projects/{p}/policy"),
            "rules",
            allowed
        )
        .await
        .0,
        StatusCode::OK
    );
}
#[tokio::test]
async fn reusing_mutation_key_with_changed_input_is_a_conflict() {
    let f = Fixture::new().await;
    let p = f.project("idempotency").await;
    let path = format!("/api/v1/projects/{p}/tasks");
    let body = json!({"title":"original","acceptance_criteria":["done"]});
    assert_eq!(
        f.call(&f.a, "POST", &path, "same", body).await.0,
        StatusCode::OK
    );
    assert_eq!(
        f.call(
            &f.a,
            "POST",
            &path,
            "same",
            json!({"title":"different","acceptance_criteria":["done"]})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}
