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
use std::time::Duration;
use tower::ServiceExt;
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

#[tokio::test]
async fn task_archive_restore_cancel_and_delete_are_separate_from_queue() {
    let f = Fixture::new().await;
    let p = f.project("task-archive").await;
    let task = f.task(&p, "Keep me", vec![]).await;
    set_origin(&f, &task, "human").await;
    let path = format!(
        "/api/v1/projects/{p}/tasks/{}",
        task["id"].as_str().unwrap()
    );
    let archive = format!("{path}/archive");
    let restore = format!("{path}/restore");
    let input = |revision| json!({"expected_revision":revision,"reason":"Operator confirmed this work is obsolete."});
    assert_eq!(
        f.call(&f.a, "POST", &archive, "agent-cannot-archive", input(1))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        f.call(&f.admin, "POST", &archive, "archive-task", input(1))
            .await
            .0,
        StatusCode::OK
    );
    f.ack(&f.a, &p).await;
    let (archived_claim_status, archived_claim) = f
        .claim(&f.a, &p, &task, "archived-task-claim", "work")
        .await;
    assert_eq!(
        archived_claim_status,
        StatusCode::CONFLICT,
        "{archived_claim}"
    );
    assert_eq!(archived_claim["error"]["code"], "task_archived");
    let (_, main) = f
        .call(
            &f.admin,
            "GET",
            &format!("/api/v1/projects/{p}/tasks"),
            "",
            json!({}),
        )
        .await;
    let (_, archived) = f
        .call(
            &f.admin,
            "GET",
            &format!("/api/v1/projects/{p}/tasks/archived"),
            "",
            json!({}),
        )
        .await;
    assert!(main["data"]["items"].as_array().unwrap().is_empty());
    assert_eq!(archived["data"]["items"].as_array().unwrap().len(), 1);
    assert!(
        f.call(&f.admin, "POST", &restore, "restore-task", input(2))
            .await
            .0
            == StatusCode::OK
    );
    let (_, main) = f
        .call(
            &f.admin,
            "GET",
            &format!("/api/v1/projects/{p}/tasks"),
            "",
            json!({}),
        )
        .await;
    assert_eq!(main["data"]["items"].as_array().unwrap().len(), 1);
    let owned = f.task(&p, "Owned work", vec![]).await;
    f.ack(&f.a, &p).await;
    let (claim_status, _) = f
        .claim(&f.a, &p, &owned, "archive-guard-claim", "work")
        .await;
    assert_eq!(claim_status, StatusCode::OK);
    let owned_archive = format!(
        "/api/v1/projects/{p}/tasks/{}/archive",
        owned["id"].as_str().unwrap()
    );
    assert_eq!(
        f.call(&f.admin, "POST", &owned_archive, "archive-owned", input(1))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let task_path = format!(
        "/api/v1/projects/{p}/tasks/{}",
        task["id"].as_str().unwrap()
    );
    assert_eq!(
        f.call(
            &f.admin,
            "POST",
            &format!("{task_path}/cancel"),
            "cancel-task",
            input(3)
        )
        .await
        .0,
        StatusCode::OK
    );
    let actual_revision: i64 = sqlx::query_scalar("SELECT revision FROM tasks WHERE id=?")
        .bind(task["id"].as_str().unwrap())
        .fetch_one(&f.state.pool)
        .await
        .unwrap();
    let actual_lifecycle: String = sqlx::query_scalar("SELECT lifecycle FROM tasks WHERE id=?")
        .bind(task["id"].as_str().unwrap())
        .fetch_one(&f.state.pool)
        .await
        .unwrap();
    assert_eq!(actual_lifecycle, "canceled");
    let (delete_status, delete_response) = f
        .call(
            &f.admin,
            "DELETE",
            &task_path,
            "delete-task",
            input(actual_revision),
        )
        .await;
    assert_eq!(delete_status, StatusCode::OK, "{delete_response}");
    let (_, main) = f
        .call(
            &f.admin,
            "GET",
            &format!("/api/v1/projects/{p}/tasks"),
            "",
            json!({}),
        )
        .await;
    assert!(
        !main["data"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == task["id"])
    );
}
async fn set_origin(f: &Fixture, task: &Value, origin: &str) {
    sqlx::query("UPDATE tasks SET origin=?,lifecycle='open',budget_held_at=NULL WHERE id=?")
        .bind(origin)
        .bind(task["id"].as_str().unwrap())
        .execute(&f.state.pool)
        .await
        .unwrap();
}
async fn task_with_origin(f: &Fixture, p: &str, title: &str, origin: &str) -> (String, Value) {
    let task = f.task(p, title, vec![]).await;
    set_origin(f, &task, origin).await;
    (
        format!(
            "/api/v1/projects/{p}/tasks/{}",
            task["id"].as_str().unwrap()
        ),
        task,
    )
}
/// Put a fresh task of `origin` into the state `action` applies to; returns its path and revision.
async fn ready_for(f: &Fixture, p: &str, action: &str, origin: &str) -> (String, i64) {
    let (path, task) = task_with_origin(f, p, &format!("{action} {origin}"), origin).await;
    let id = task["id"].as_str().unwrap();
    let sql = match action {
        "restore" => "UPDATE tasks SET archived_at=1 WHERE id=?",
        "delete" => "UPDATE tasks SET lifecycle='canceled' WHERE id=?",
        _ => "UPDATE tasks SET lifecycle='open' WHERE id=?",
    };
    sqlx::query(sql)
        .bind(id)
        .execute(&f.state.pool)
        .await
        .unwrap();
    (path, 1)
}
async fn lifecycle(
    f: &Fixture,
    c: &Caller,
    action: &str,
    (path, revision): (&str, i64),
    key: &str,
) -> (StatusCode, Value) {
    let body = json!({"expected_revision":revision,"reason":"Cleaning up."});
    if action == "delete" {
        f.call(c, "DELETE", path, key, body).await
    } else {
        f.call(c, "POST", &format!("{path}/{action}"), key, body)
            .await
    }
}

#[tokio::test]
async fn agents_manage_agent_and_service_tasks_but_not_human_ones() {
    let f = Fixture::new().await;
    let p = f.project("lifecycle-matrix").await;
    for action in ["cancel", "archive", "restore", "delete"] {
        for origin in ["human", "agent", "service"] {
            for caller in [&f.admin, &f.a] {
                let (path, revision) = ready_for(&f, &p, action, origin).await;
                let key = format!("{action}-{origin}-{}", caller.human);
                let (status, v) = lifecycle(&f, caller, action, (&path, revision), &key).await;
                if caller.human || origin != "human" {
                    assert_eq!(status, StatusCode::OK, "{action} {origin}: {v}");
                    let id = path.rsplit('/').next().unwrap();
                    let (rev, deleted, archived, state): (i64, Option<i64>, Option<i64>, String) =
                        sqlx::query_as(
                            "SELECT revision,deleted_at,archived_at,lifecycle FROM tasks WHERE id=?",
                        )
                        .bind(id)
                        .fetch_one(&f.state.pool)
                        .await
                        .unwrap();
                    assert_eq!(rev, revision + 1, "{action} {origin}");
                    match action {
                        "cancel" => assert_eq!(state, "canceled"),
                        "archive" => assert!(archived.is_some()),
                        "restore" => assert!(archived.is_none()),
                        _ => assert!(deleted.is_some()),
                    }
                    let actor: String = sqlx::query_scalar(
                        "SELECT actor_id FROM events WHERE record_id=? AND kind=? ORDER BY seq DESC LIMIT 1",
                    )
                    .bind(id)
                    .bind(match action {
                        "archive" => "task.archived",
                        "restore" => "task.restored",
                        "cancel" => "task.canceled",
                        _ => "task.deleted",
                    })
                    .fetch_one(&f.state.pool)
                    .await
                    .unwrap();
                    assert_eq!(actor, caller.principal, "{action} {origin}");
                } else {
                    assert_eq!(status, StatusCode::FORBIDDEN, "{action} {origin}: {v}");
                    assert_eq!(v["error"]["details"]["gate"], format!("task_{action}"));
                    assert_eq!(v["error"]["details"]["required_actor"], "human");
                    assert_eq!(
                        v["error"]["message"],
                        "This project has not delegated this task lifecycle action to agents."
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn agent_cancel_of_a_human_task_still_follows_agent_rule_editing() {
    let f = Fixture::new().await;
    let p = f.project("lifecycle-delegation").await;
    let (path, revision) = ready_for(&f, &p, "cancel", "human").await;
    let (status, _) = lifecycle(&f, &f.a, "cancel", (&path, revision), "refused").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let change = json!({"expected_revision":1,"review_mode":"agent","recovery_mode":"agent","lease_seconds":600,"rules":"Read all evidence","agent_rule_editing":true,"automatic_integration":true});
    let (status, v) = f
        .call(
            &f.admin,
            "PATCH",
            &format!("/api/v1/projects/{p}/policy"),
            "delegate",
            change,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (status, v) = lifecycle(&f, &f.a, "cancel", (&path, revision), "allowed").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    for action in ["archive", "restore", "delete"] {
        let (path, revision) = ready_for(&f, &p, action, "human").await;
        let (status, v) = lifecycle(&f, &f.a, action, (&path, revision), action).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{action}: {v}");
    }
}

#[tokio::test]
async fn agent_lifecycle_actions_keep_every_guard() {
    let f = Fixture::new().await;
    let p = f.project("lifecycle-guards").await;
    let code = |v: &Value| v["error"]["code"].as_str().unwrap_or_default().to_owned();
    for origin in ["agent", "service"] {
        // A stale revision.
        let (path, _) = ready_for(&f, &p, "cancel", origin).await;
        let (status, v) = lifecycle(&f, &f.a, "cancel", (&path, 9), "stale").await;
        assert_eq!(status, StatusCode::CONFLICT, "{v}");
        assert_eq!(code(&v), "task_revision_changed");
        // An archived task must be restored first.
        let (path, revision) = ready_for(&f, &p, "restore", origin).await;
        for action in ["cancel", "archive", "delete"] {
            let (status, v) = lifecycle(&f, &f.a, action, (&path, revision), action).await;
            assert_eq!(status, StatusCode::CONFLICT, "{v}");
            assert_eq!(code(&v), "task_archived");
        }
        // A task not in a state the action permits.
        let (path, revision) = ready_for(&f, &p, "cancel", origin).await;
        let (status, v) = lifecycle(&f, &f.a, "restore", (&path, revision), "restore").await;
        assert_eq!(status, StatusCode::CONFLICT, "{v}");
        assert_eq!(code(&v), "task_lifecycle_invalid");
        let (status, v) = lifecycle(&f, &f.a, "delete", (&path, revision), "delete").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
        // A live attempt.
        let (path, task) = task_with_origin(&f, &p, "Claimed", origin).await;
        f.ack(&f.b, &p).await;
        let (status, v) = f
            .claim(&f.b, &p, &task, &format!("claim-{origin}"), "work")
            .await;
        assert_eq!(status, StatusCode::OK, "{v}");
        for action in ["cancel", "archive"] {
            let (status, v) = lifecycle(&f, &f.a, action, (&path, 1), action).await;
            assert_eq!(status, StatusCode::CONFLICT, "{v}");
            assert_eq!(code(&v), "task_attempt_protected");
        }
        // Review or integration work.
        let (path, task) = task_with_origin(&f, &p, "In review", origin).await;
        let id = task["id"].as_str().unwrap();
        let mut conn = f.state.pool.acquire().await.unwrap();
        sqlx::query("PRAGMA foreign_keys=OFF")
            .execute(&mut *conn)
            .await
            .unwrap();
        sqlx::query("INSERT INTO workflow_activities(id,project_id,subject_task_id,submission_id,activity_task_id,kind,state,created_at) VALUES(?,?,?,?,?,'agent_review','queued',1)")
            .bind(Uuid::new_v4().to_string()).bind(&p).bind(id).bind(Uuid::new_v4().to_string()).bind(Uuid::new_v4().to_string())
            .execute(&mut *conn).await.unwrap();
        sqlx::query("PRAGMA foreign_keys=ON")
            .execute(&mut *conn)
            .await
            .unwrap();
        drop(conn);
        for action in ["cancel", "archive"] {
            let (status, v) = lifecycle(&f, &f.a, action, (&path, 1), action).await;
            assert_eq!(status, StatusCode::CONFLICT, "{v}");
            assert_eq!(code(&v), "task_workflow_protected");
        }
        // A task other tasks depend on keeps its history: archive instead.
        let (path, prerequisite) = task_with_origin(&f, &p, "Prerequisite", origin).await;
        let dependent = f
            .task(
                &p,
                "Dependent",
                vec![prerequisite["id"].as_str().unwrap().into()],
            )
            .await;
        assert!(dependent["id"].is_string());
        sqlx::query("UPDATE tasks SET lifecycle='canceled' WHERE id=?")
            .bind(prerequisite["id"].as_str().unwrap())
            .execute(&f.state.pool)
            .await
            .unwrap();
        let (status, v) = lifecycle(&f, &f.a, "delete", (&path, 1), "history").await;
        assert_eq!(status, StatusCode::CONFLICT, "{v}");
        assert_eq!(code(&v), "task_history_protected");
        let (status, v) = lifecycle(&f, &f.a, "archive", (&path, 1), "archive-history").await;
        assert_eq!(status, StatusCode::OK, "{v}");
    }
}

#[tokio::test]
async fn the_stored_origin_decides_not_the_caller() {
    let f = Fixture::new().await;
    let p = f.project("lifecycle-origin").await;
    // An agent-session body cannot claim another origin for a human task.
    let (path, revision) = ready_for(&f, &p, "archive", "human").await;
    let body = json!({"expected_revision":revision,"reason":"Cleaning up.","origin":"agent"});
    let (status, v) = f
        .call(&f.a, "POST", &format!("{path}/archive"), "spoof", body)
        .await;
    assert_ne!(status, StatusCode::OK, "{v}");
    let stored: String = sqlx::query_scalar("SELECT origin FROM tasks WHERE id=?")
        .bind(path.rsplit('/').next().unwrap())
        .fetch_one(&f.state.pool)
        .await
        .unwrap();
    assert_eq!(stored, "human");
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
        let (status,v)=self.call(c,"POST",&format!("/api/v1/sessions/{}/instruction-acknowledgments",c.session),&Uuid::new_v4().to_string(),json!({"project_id":p,"policy_revision":1,"instruction_version":coordinator_core::INSTRUCTION_VERSION,"sections":[coordinator_core::REQUIRED_SECTION]})).await;
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
        self.call(c,"POST",&format!("/api/v1/projects/{p}/claims"),key,json!({"task_id":t["id"],"expected_task_revision":t["revision"],"mode":mode,"policy_revision":1,"instruction_version":coordinator_core::INSTRUCTION_VERSION})).await
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

#[tokio::test]
async fn preconditions_report_service_gates_and_state_wait_detects_task_changes() {
    let f = Fixture::new().await;
    let p = f.project("preconditions and state wait").await;
    let prerequisite = f.task(&p, "Prerequisite", vec![]).await;
    let dependent = f
        .task(
            &p,
            "Dependent",
            vec![prerequisite["id"].as_str().unwrap().to_owned()],
        )
        .await;
    let dependent_id = dependent["id"].as_str().unwrap();
    let path = format!("/api/v1/projects/{p}/preconditions/{dependent_id}");
    let (status, inspected) = f.call(&f.a, "GET", &path, "", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{inspected}");
    assert_eq!(inspected["data"]["eligible_to_claim"], false);
    assert!(
        inspected["data"]["unmet_preconditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|blocker| blocker["code"] == "dependencies_incomplete")
    );
    let (status, task_detail) = f
        .call(
            &f.a,
            "GET",
            &format!("/api/v1/projects/{p}/tasks/{dependent_id}"),
            "",
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{task_detail}");
    assert_eq!(
        task_detail["data"]["state_token"],
        inspected["data"]["state_token"]
    );
    assert!(
        task_detail["data"]["preconditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|blocker| blocker["code"] == "dependencies_incomplete")
    );
    let token = inspected["data"]["state_token"]
        .as_str()
        .unwrap()
        .to_owned();

    let app = f.app.clone();
    let caller = f.a.clone();
    let wait_path = format!(
        "/api/v1/projects/{p}/state-wait?target_kind=task&target_id={}&after_state_token={token}&timeout_seconds=3",
        dependent_id
    );
    let wait =
        tokio::spawn(async move { call(app, &caller, "GET", &wait_path, "", json!({})).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    sqlx::query("UPDATE tasks SET blocked_reason='Task is intentionally held.' WHERE id=?")
        .bind(dependent["id"].as_str().unwrap())
        .execute(&f.state.pool)
        .await
        .unwrap();
    let (status, changed) = wait.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{changed}");
    assert_eq!(changed["data"]["changed"], true);
    assert_eq!(
        changed["data"]["state"]["blocked_reason"],
        "Task is intentionally held."
    );

    let (status, before_ack) = f
        .call(
            &f.a,
            "GET",
            &format!("/api/v1/projects/{p}/preconditions/{dependent_id}"),
            "",
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{before_ack}");
    let before_token = before_ack["data"]["state_token"].as_str().unwrap();
    let before_status = before_ack["data"]["work_status"].clone();
    f.ack(&f.a, &p).await;
    let (status, after_ack) = f
        .call(
            &f.a,
            "GET",
            &format!("/api/v1/projects/{p}/state-wait?target_kind=task&target_id={dependent_id}&after_state_token={before_token}&timeout_seconds=3"),
            "",
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{after_ack}");
    assert_eq!(after_ack["data"]["changed"], true);
    assert_eq!(after_ack["data"]["state"]["work_status"], before_status);
    assert_ne!(after_ack["data"]["state_token"], before_token);

    let (status, bad_timeout) = f
        .call(
            &f.a,
            "GET",
            &format!("/api/v1/projects/{p}/state-wait?target_kind=task&target_id={dependent_id}&after_state_token={token}&timeout_seconds=31"),
            "",
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{bad_timeout}");
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
        let body = json!({"task_id":t["id"],"expected_task_revision":1,"policy_revision":1,"instruction_version":coordinator_core::INSTRUCTION_VERSION});
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
    let (status, replay) = call(router(reopened), &f.a, "POST", &format!("/api/v1/projects/{p}/claims"), "persisted-claim", json!({"task_id":t["id"],"expected_task_revision":1,"policy_revision":1,"instruction_version":coordinator_core::INSTRUCTION_VERSION})).await;
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
            &f.admin,
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

#[tokio::test]
async fn task_listing_can_exclude_done_tasks_before_pagination() {
    let f = Fixture::new().await;
    let p = f.project("queue pagination").await;
    let ready = f.task(&p, "unfinished", vec![]).await;
    let done = f.task(&p, "completed", vec![]).await;
    sqlx::query("UPDATE tasks SET lifecycle='done' WHERE id=?")
        .bind(done["id"].as_str().unwrap())
        .execute(&f.state.pool)
        .await
        .unwrap();

    let (_, all_tasks) = f
        .call(
            &f.admin,
            "GET",
            &format!("/api/v1/projects/{p}/tasks?limit=1"),
            "",
            json!({}),
        )
        .await;
    let (_, queue_tasks) = f
        .call(
            &f.admin,
            "GET",
            &format!("/api/v1/projects/{p}/tasks?limit=1&exclude_done=true"),
            "",
            json!({}),
        )
        .await;
    assert!(all_tasks["data"]["next_cursor"].is_string());
    assert_eq!(queue_tasks["data"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(queue_tasks["data"]["items"][0]["id"], ready["id"]);
    assert!(queue_tasks["data"]["next_cursor"].is_null());
}

#[tokio::test]
async fn acceptance_changes_after_work_require_a_human_or_explicit_delegation() {
    let f = Fixture::new().await;
    let p = f.project("protected acceptance").await;
    let t = f.task(&p, "outcome", vec![]).await;
    f.ack(&f.a, &p).await;
    let (status, claimed) = f.claim(&f.a, &p, &t, "claim-for-criteria", "work").await;
    assert_eq!(status, StatusCode::OK);
    let attempt = &claimed["data"]["claim"]["attempt"];
    let (status, _) = f
        .call(
            &f.a,
            "POST",
            &format!(
                "/api/v1/projects/{p}/attempts/{}/release",
                attempt["id"].as_str().unwrap()
            ),
            "release-for-review",
            json!({"generation":attempt["generation"],"summary":"Work needs revision."}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let path = format!("/api/v1/projects/{p}/tasks/{}", t["id"].as_str().unwrap());
    let changed = json!({"expected_revision":1,"title":"outcome","description":"Test task",
        "acceptance_criteria":["A weaker criterion"],"priority":2,"depends_on":[],"planned":false});
    assert_eq!(
        f.call(&f.a, "PATCH", &path, "weaken-own-criteria", changed.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (_, unchanged) = f.call(&f.a, "GET", &path, "read-criteria", json!({})).await;
    assert_eq!(
        unchanged["data"]["acceptance_criteria"],
        t["acceptance_criteria"]
    );
    assert_eq!(
        f.call(&f.admin, "PATCH", &path, "human-revises-criteria", changed)
            .await
            .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn task_definition_grants_are_scoped_revocable_and_preserve_contributor_safety() {
    let f = Fixture::new().await;
    let p = f.project("definition grants").await;
    let t = f.task(&p, "editable by delegate", vec![]).await;
    let path = format!("/api/v1/projects/{p}/tasks/{}", t["id"].as_str().unwrap());
    let edit = json!({"expected_revision":1,"title":"revised","description":"Test task","acceptance_criteria":["verified"],"priority":2,"depends_on":[],"planned":false});
    assert_eq!(
        f.call(&f.b, "PATCH", &path, "default-deny", edit.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );

    let grants = format!("/api/v1/projects/{p}/task-definition-grants");
    let grant_body = json!({"target_kind":"principal","agent_principal_id":f.b.principal});
    let (status, grant) = f
        .call(&f.admin, "POST", &grants, "grant-b", grant_body.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "{grant}");
    let (_, replay) = f
        .call(&f.admin, "POST", &grants, "grant-b", grant_body)
        .await;
    assert_eq!(replay["data"]["id"], grant["data"]["id"]);
    assert_eq!(
        f.call(&f.b, "PATCH", &path, "granted-edit", edit.clone())
            .await
            .0,
        StatusCode::OK
    );

    let revoke = format!("{}/{}", grants, grant["data"]["id"].as_str().unwrap());
    assert_eq!(
        f.call(
            &f.admin,
            "POST",
            &revoke,
            "revoke-b",
            json!({"expected_revision":1})
        )
        .await
        .0,
        StatusCode::OK
    );
    let other = f.task(&p, "denied after revoke", vec![]).await;
    let other_path = format!(
        "/api/v1/projects/{p}/tasks/{}",
        other["id"].as_str().unwrap()
    );
    assert_eq!(f.call(&f.b, "PATCH", &other_path, "revoked-deny", json!({"expected_revision":1,"title":"nope","description":"Test task","acceptance_criteria":["verified"],"priority":2,"depends_on":[],"planned":false})).await.0, StatusCode::FORBIDDEN);

    let (status, role_grant) = f
        .call(
            &f.admin,
            "POST",
            &grants,
            "grant-agent-role",
            json!({"target_kind":"role","agent_role":"agent"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{role_grant}");
    let role_task = f.task(&p, "editable by delegated role", vec![]).await;
    let role_path = format!(
        "/api/v1/projects/{p}/tasks/{}",
        role_task["id"].as_str().unwrap()
    );
    assert_eq!(
        f.call(
            &f.b,
            "PATCH",
            &role_path,
            "role-edit",
            json!({"expected_revision":1,"title":"role revised","description":"Test task","acceptance_criteria":["verified"],"priority":2,"depends_on":[],"planned":false}),
        )
        .await
        .0,
        StatusCode::OK
    );

    let isolated = f.project("definition grant isolation").await;
    let isolated_task = f.task(&isolated, "not delegated here", vec![]).await;
    assert_eq!(
        f.call(
            &f.b,
            "PATCH",
            &format!("/api/v1/projects/{isolated}/tasks/{}", isolated_task["id"]),
            "cross-project-deny",
            json!({"expected_revision":1,"title":"nope","description":"Test task","acceptance_criteria":["verified"],"priority":2,"depends_on":[],"planned":false}),
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );

    f.ack(&f.b, &p).await;
    let (status, claim) = f.claim(&f.b, &p, &other, "contributor-claim", "work").await;
    assert_eq!(status, StatusCode::OK);
    let (attempt_id, generation) = attempt(&claim);
    assert_eq!(
        f.call(
            &f.b,
            "POST",
            &format!("/api/v1/projects/{p}/attempts/{attempt_id}/release"),
            "contributor-release",
            json!({"generation":generation,"summary":"saved"})
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, renewed) = f
        .call(
            &f.admin,
            "POST",
            &grants,
            "renew-b",
            json!({"target_kind":"principal","agent_principal_id":f.b.principal}),
        )
        .await;
    assert_eq!(renewed["data"]["agent_principal_id"], f.b.principal);
    assert_eq!(f.call(&f.b, "PATCH", &other_path, "contributor-deny", json!({"expected_revision":1,"title":"nope","description":"changed","acceptance_criteria":["weaker"],"priority":2,"depends_on":[],"planned":false})).await.0, StatusCode::FORBIDDEN);
}

const RECORDED: &str = "0123456789abcdef0123456789abcdef01234567";

/// Agent A claims and checkpoints (with `revision`, if any); its lease then
/// expires and agent B takes a recovery claim. Returns the project, the old
/// generation, and B's recovery attempt and generation.
async fn expired_with_checkpoint(
    f: &Fixture,
    name: &str,
    revision: Option<&str>,
) -> (String, i64, String, i64) {
    let p = f.project(name).await;
    let t = f.task(&p, "task", vec![]).await;
    f.ack(&f.a, &p).await;
    f.ack(&f.b, &p).await;
    let (_, v) = f.claim(&f.a, &p, &t, "initial", "work").await;
    let (old, generation) = attempt(&v);
    let mut body = json!({"generation":generation,"summary":"WIP pushed"});
    if let Some(r) = revision {
        body["revision"] = json!(r);
    }
    let path = format!("/api/v1/projects/{p}/attempts/{old}/checkpoints");
    let (status, v) = f.call(&f.a, "POST", &path, "wip", body).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["revision"], json!(revision));
    f.clock.0.fetch_add(600_000, Ordering::SeqCst);
    let (status, v) = f.claim(&f.b, &p, &t, "takeover", "recovery").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (new, new_generation) = attempt(&v);
    (p, generation, new, new_generation)
}

/// Posts one recovery resolution by agent B with the given extra fields.
async fn resolve(
    f: &Fixture,
    p: &str,
    id: &str,
    g: i64,
    key: &str,
    extra: Value,
) -> (StatusCode, Value) {
    let mut body = json!({"generation":g,"disposition":"resume","summary":"Fetched the WIP ref."});
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let path = format!("/api/v1/projects/{p}/attempts/{id}/recovery-resolution");
    f.call(&f.b, "POST", &path, key, body).await
}

#[tokio::test]
async fn checkpoint_revision_must_be_a_full_lowercase_sha() {
    let f = Fixture::new().await;
    let p = f.project("revision-format").await;
    let t = f.task(&p, "task", vec![]).await;
    f.ack(&f.a, &p).await;
    let (_, v) = f.claim(&f.a, &p, &t, "claim", "work").await;
    let (id, g) = attempt(&v);
    let path = format!("/api/v1/projects/{p}/attempts/{id}/checkpoints");
    for (key, bad) in [("short", "abc123"), ("upper", &RECORDED.to_uppercase()[..])] {
        let body = json!({"generation":g,"summary":"WIP","revision":bad});
        let (status, v) = f.call(&f.a, "POST", &path, key, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    }
}

#[tokio::test]
async fn recovery_with_a_recorded_revision_is_verified_by_the_service() {
    let f = Fixture::new().await;
    let (p, old_g, new, g) = expired_with_checkpoint(&f, "verified", Some(RECORDED)).await;
    assert_eq!(g, old_g + 1, "the recovery claim bumps the generation");
    let (status, v) = resolve(&f, &p, &new, g, "none", json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["error"]["code"], "recovery_revision_required");
    let wrong = json!({"fetched_revision":"f".repeat(40),"saved_work_checked":true,"running_jobs_checked":true});
    let (status, v) = resolve(&f, &p, &new, g, "wrong", wrong).await;
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["error"]["code"], "recovery_revision_mismatch");
    assert_eq!(v["error"]["details"]["recorded_revision"], RECORDED);
    let (status, v) = resolve(
        &f,
        &p,
        &new,
        g,
        "match",
        json!({"fetched_revision":RECORDED}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "booleans are not required: {v}");
    assert_eq!(v["data"]["evidence"], "service_verified");
    assert_eq!(v["data"]["attempt"]["mode"], "work");
}

#[tokio::test]
async fn legacy_null_revision_recovery_keeps_local_attestations() {
    let f = Fixture::new().await;
    let (p, _, new, g) = expired_with_checkpoint(&f, "legacy", None).await;
    let fetched = json!({"fetched_revision":RECORDED});
    let (status, v) = resolve(&f, &p, &new, g, "fetched-only", fetched).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a SHA cannot replace attestations: {v}"
    );
    let attested = json!({"saved_work_checked":true,"running_jobs_checked":true});
    let (status, v) = resolve(&f, &p, &new, g, "attested", attested).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["evidence"], "local_attestation");
}

#[tokio::test]
async fn legacy_recovery_under_manual_mode_keeps_the_human_gate() {
    let f = Fixture::new().await;
    let p = f.project("manual").await;
    let t = f.task(&p, "task", vec![]).await;
    f.ack(&f.a, &p).await;
    f.ack(&f.b, &p).await;
    let (_, v) = f.claim(&f.a, &p, &t, "initial", "work").await;
    let (old, g) = attempt(&v);
    let path = format!("/api/v1/projects/{p}/attempts/{old}/checkpoints");
    let body = json!({"generation":g,"summary":"legacy checkpoint"});
    assert_eq!(
        f.call(&f.a, "POST", &path, "cp", body).await.0,
        StatusCode::OK
    );
    sqlx::query("UPDATE projects SET recovery_mode='manual' WHERE id=?")
        .bind(&p)
        .execute(&f.state.pool)
        .await
        .unwrap();
    f.clock.0.fetch_add(600_000, Ordering::SeqCst);
    let (status, v) = f.claim(&f.b, &p, &t, "takeover", "recovery").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["error"]["details"]["required_actor"], "human");
}

#[tokio::test]
async fn checkpoint_revision_migration_applies_to_an_existing_database() {
    use sqlx::Connection;
    let dir = tempfile::tempdir().unwrap();
    let migrations = dir.path().join("schema26");
    std::fs::create_dir(&migrations).unwrap();
    for m in sqlx::migrate!("./migrations")
        .iter()
        .filter(|m| m.version <= 26)
    {
        let name = format!("{:04}_{}.sql", m.version, m.description.replace(' ', "_"));
        std::fs::write(migrations.join(name), m.sql.as_str().as_bytes()).unwrap();
    }
    let database = dir.path().join("upgrade.sqlite3");
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&database)
        .create_if_missing(true)
        .foreign_keys(false);
    let mut old = sqlx::SqliteConnection::connect_with(&options)
        .await
        .unwrap();
    sqlx::migrate::Migrator::new(migrations.as_path())
        .await
        .unwrap()
        .run(&mut old)
        .await
        .unwrap();
    sqlx::query("INSERT INTO checkpoints(id,project_id,attempt_id,summary,current_action,next_step,blockers_json,created_at) VALUES('cp1','p1','a1','old','','','[]',1)")
        .execute(&mut old).await.unwrap();
    old.close().await.unwrap();
    let state = AppState::open(Config {
        database_path: database,
        public_origin: "http://127.0.0.1:8080".into(),
        allow_insecure_loopback: true,
        ..Config::default()
    })
    .await
    .unwrap();
    let kept: Option<String> =
        sqlx::query_scalar("SELECT revision FROM checkpoints WHERE id='cp1'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(
        kept, None,
        "existing checkpoints become legacy (NULL) checkpoints"
    );
    let bad = sqlx::query("UPDATE checkpoints SET revision='NOTASHA' WHERE id='cp1'")
        .execute(&state.pool)
        .await;
    assert!(bad.is_err(), "the column only stores full lowercase SHAs");
}

#[tokio::test]
async fn renewal_past_the_maximum_attempt_duration_is_refused() {
    let f = Fixture::new().await;
    let p = f.project("max-duration").await;
    let t = f.task(&p, "task", vec![]).await;
    let policy = format!("/api/v1/projects/{p}/policy");
    let limit = json!({"expected_revision":1,"review_mode":"agent","recovery_mode":"agent","lease_seconds":600,"rules":"","agent_rule_editing":true,"automatic_integration":true,"max_attempt_seconds":900});
    let (status, v) = f
        .call(&f.a, "PATCH", &policy, "agent-limit", limit.clone())
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    let (status, v) = f.call(&f.admin, "PATCH", &policy, "limit", limit).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["max_attempt_seconds"], 900);
    let (status,v)=f.call(&f.a,"POST",&format!("/api/v1/sessions/{}/instruction-acknowledgments",f.a.session),"ack",json!({"project_id":p,"policy_revision":2,"instruction_version":coordinator_core::INSTRUCTION_VERSION,"sections":[coordinator_core::REQUIRED_SECTION]})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (status,v)=f.call(&f.a,"POST",&format!("/api/v1/projects/{p}/claims"),"claim",json!({"task_id":t["id"],"expected_task_revision":t["revision"],"mode":"work","policy_revision":2,"instruction_version":coordinator_core::INSTRUCTION_VERSION})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (id, g) = attempt(&v);
    let path = format!("/api/v1/projects/{p}/attempts/{id}/renew");
    f.clock.0.fetch_add(500_000, Ordering::SeqCst);
    let (status, capped) = f
        .call(&f.a, "POST", &path, "within", json!({"generation":g}))
        .await;
    assert_eq!(status, StatusCode::OK, "{capped}");
    assert_eq!(capped["data"]["lease_remaining_ms"], 400_000);
    f.clock.0.fetch_add(399_000, Ordering::SeqCst);
    let (status, refused) = f
        .call(&f.a, "POST", &path, "past", json!({"generation":g}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["error"]["code"], "attempt_duration_exceeded");
}

#[tokio::test]
async fn a_claim_never_outlasts_the_maximum_attempt_duration() {
    let f = Fixture::new().await;
    let p = f.project("capped-claim").await;
    let t = f.task(&p, "task", vec![]).await;
    sqlx::query("UPDATE projects SET lease_seconds=3600,max_attempt_seconds=900 WHERE id=?")
        .bind(&p)
        .execute(&f.state.pool)
        .await
        .unwrap();
    f.ack(&f.a, &p).await;
    let (status, v) = f.claim(&f.a, &p, &t, "claim", "work").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["claim"]["lease_remaining_ms"], 900_000);
    assert_eq!(v["data"]["renew_after_seconds"], 60);
    let (id, g) = attempt(&v);
    let path = format!("/api/v1/projects/{p}/attempts/{id}/renew");
    let (status, refused) = f
        .call(&f.a, "POST", &path, "renew", json!({"generation":g}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["error"]["code"], "attempt_duration_exceeded");
}

#[tokio::test]
async fn lifecycle_events_keep_the_reason_actor_and_replacement() {
    let f = Fixture::new().await;
    let p = f.project("lifecycle-reasons").await;
    let (old_path, old) = task_with_origin(&f, &p, "old", "agent").await;
    let (_, new) = task_with_origin(&f, &p, "new", "agent").await;
    let (old_id, new_id) = (old["id"].as_str().unwrap(), new["id"].as_str().unwrap());
    let body = json!({"expected_revision":1,"reason":"Superseded by a better plan.","replacement_task_id":new_id});
    let (status, v) = f
        .call(
            &f.a,
            "POST",
            &format!("{old_path}/cancel"),
            "cancel-old",
            body,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (status, v) = f.call(&f.a, "GET", &old_path, "", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let events = v["data"]["lifecycle_events"].as_array().unwrap();
    assert_eq!(events.len(), 1, "{v}");
    assert_eq!(events[0]["action"], "canceled");
    assert_eq!(events[0]["actor_id"], f.a.principal);
    assert_eq!(events[0]["reason"], "Superseded by a better plan.");
    assert_eq!(events[0]["replacement_task_id"], new_id);
    assert!(events[0]["actor_name"].is_string(), "{v}");
    assert!(v["data"]["replaces"].as_array().unwrap().is_empty());
    let (status, v) = f
        .call(
            &f.a,
            "GET",
            &format!("/api/v1/projects/{p}/tasks/{new_id}"),
            "",
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let replaces = v["data"]["replaces"].as_array().unwrap();
    assert_eq!(replaces.len(), 1, "{v}");
    assert_eq!(replaces[0]["task_id"], old_id);
    assert_eq!(replaces[0]["title"], "old");
    assert_eq!(replaces[0]["reason"], "Superseded by a better plan.");
    let (status, v) = f
        .call(
            &f.a,
            "GET",
            &format!("{old_path}/history?kind=events&limit=50"),
            "",
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let canceled = v["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["record"]["kind"] == "task.canceled")
        .unwrap_or_else(|| panic!("{v}"));
    let data = &canceled["record"]["data"];
    assert_eq!(data["reason"], "Superseded by a better plan.");
    assert_eq!(data["replacement_task_id"], new_id);
    assert_eq!(data["actor_id"], f.a.principal);
    // Archive and restore keep their reasons and carry no replacement.
    let archive = json!({"expected_revision":2,"reason":"Tidy up."});
    let (status, v) = f
        .call(
            &f.a,
            "POST",
            &format!("{old_path}/archive"),
            "arch",
            archive,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (_, v) = f.call(&f.a, "GET", &old_path, "", json!({})).await;
    let events = v["data"]["lifecycle_events"].as_array().unwrap();
    assert_eq!(events[0]["action"], "archived");
    assert_eq!(events[0]["reason"], "Tidy up.");
    assert!(events[0]["replacement_task_id"].is_null());
    assert_eq!(events[1]["action"], "canceled");
}

#[tokio::test]
async fn lifecycle_reason_backfill_copies_only_from_surviving_receipts() {
    use sqlx::Connection;
    let dir = tempfile::tempdir().unwrap();
    let migrations = dir.path().join("schema35");
    std::fs::create_dir(&migrations).unwrap();
    for m in sqlx::migrate!("./migrations")
        .iter()
        .filter(|m| m.version <= 35)
    {
        let name = format!("{:04}_{}.sql", m.version, m.description.replace(' ', "_"));
        std::fs::write(migrations.join(name), m.sql.as_str().as_bytes()).unwrap();
    }
    let database = dir.path().join("upgrade.sqlite3");
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&database)
        .create_if_missing(true)
        .foreign_keys(false);
    let mut old = sqlx::SqliteConnection::connect_with(&options)
        .await
        .unwrap();
    sqlx::migrate::Migrator::new(migrations.as_path())
        .await
        .unwrap()
        .run(&mut old)
        .await
        .unwrap();
    let op = |action: &str, task: &str| format!("POST /api/v1/projects/p1/tasks/{task}/{action}");
    // (operation, key, receipt result, receipt time, compaction time)
    let cases = [
        (
            op("cancel", "t1"),
            "k1",
            r#"{"reason":"Replaced.","replacement_task_id":"t2"}"#,
            10,
            None,
        ),
        (
            op("archive", "t1"),
            "k2",
            r#"{"reason":"Tidy.","replacement_task_id":null}"#,
            20,
            None,
        ),
        (op("cancel", "t3"), "k3", "null", 30, Some(99)),
    ];
    for (operation, key, json, at, compacted) in &cases {
        sqlx::query("INSERT INTO mutation_receipts(principal_id,operation,key,fingerprint,result_json,created_at,authority_epoch,compacted_at) VALUES('u1',?,?,'f',?,?,'initial',?)")
            .bind(operation).bind(key).bind(json).bind(at).bind(compacted).execute(&mut old).await.unwrap();
    }
    for (kind, task, at) in [
        ("task.canceled", "t1", 10),
        ("task.archived", "t1", 20),
        ("task.canceled", "t3", 30),
        ("task.deleted", "t4", 40),
    ] {
        sqlx::query("INSERT INTO events(project_id,actor_id,kind,record_id,data_json,created_at) VALUES('p1','u1',?,?,'{}',?)")
            .bind(kind).bind(task).bind(at).execute(&mut old).await.unwrap();
    }
    old.close().await.unwrap();
    let state = AppState::open(Config {
        database_path: database,
        public_origin: "http://127.0.0.1:8080".into(),
        allow_insecure_loopback: true,
        ..Config::default()
    })
    .await
    .unwrap();
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT record_id||kind,data_json FROM events WHERE project_id='p1' ORDER BY seq",
    )
    .fetch_all(&state.pool)
    .await
    .unwrap();
    let data: Vec<Value> = rows
        .iter()
        .map(|r| serde_json::from_str(&r.1).unwrap())
        .collect();
    assert_eq!(
        data[0],
        json!({"reason":"Replaced.","actor_id":"u1","replacement_task_id":"t2"})
    );
    assert_eq!(data[1], json!({"reason":"Tidy.","actor_id":"u1"}));
    assert_eq!(data[2], json!({}), "a compacted receipt is not used");
    assert_eq!(data[3], json!({}), "no receipt, no invented data");
}
