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
    fn use_monotonic_elapsed(&self) -> bool {
        false
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
    _dir: tempfile::TempDir,
    admin: Caller,
    owner: Caller,
    reviewer: Caller,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::open(Config {
            database_path: dir.path().join("objectives.sqlite3"),
            public_origin: "http://127.0.0.1:8080".into(),
            allow_insecure_loopback: true,
            ..Config::default()
        })
        .await
        .unwrap();
        state.clock = Arc::new(TestClock(AtomicI64::new(1_800_000_000_000)));
        let admin = seed(&state, true, "objective-admin").await;
        let owner = seed(&state, false, "objective-owner").await;
        let reviewer = seed(&state, false, "objective-reviewer").await;
        Self {
            app: router(state.clone()),
            state,
            _dir: dir,
            admin,
            owner,
            reviewer,
        }
    }

    async fn call(
        &self,
        caller: &Caller,
        method: &str,
        path: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        call(
            self.app.clone(),
            caller,
            method,
            path,
            &Uuid::new_v4().to_string(),
            body,
        )
        .await
    }

    async fn project(&self, name: &str) -> String {
        let (status, value) = self
            .call(
                &self.admin,
                "POST",
                "/api/v1/projects",
                json!({"name":name,"repository_url":format!("https://example.test/{name}.git"),"target_branch":"main"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        value["data"]["id"].as_str().unwrap().into()
    }

    async fn task(&self, project: &str, title: &str) -> Value {
        let (status, value) = self
            .call(
                &self.owner,
                "POST",
                &format!("/api/v1/projects/{project}/tasks"),
                json!({"title":title,"description":"objective route test",
                    "acceptance_criteria":["required behavior verified"],"kind":"general"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        value["data"].clone()
    }

    async fn objective(&self, project: &str, title: &str, children: Vec<Value>) -> Value {
        let (status, value) = self
            .call(
                &self.owner,
                "POST",
                &format!("/api/v1/projects/{project}/objectives"),
                json!({"title":title,"description":"Coordinate the child outcomes.",
                    "acceptance_criteria":["Required children and objective evidence are reviewed"],
                    "priority":2,"children":children,"planned":false}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        value["data"].clone()
    }

    async fn acknowledge(&self, caller: &Caller, project: &str) {
        let (status, value) = self
            .call(
                caller,
                "POST",
                &format!(
                    "/api/v1/sessions/{}/instruction-acknowledgments",
                    caller.session
                ),
                json!({"project_id":project,"policy_revision":1,
                    "instruction_version":coordinator_core::INSTRUCTION_VERSION,
                    "sections":[coordinator_core::REQUIRED_SECTION]}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
    }

    async fn claim(&self, project: &str, task: &Value) -> (StatusCode, Value) {
        self.acknowledge(&self.owner, project).await;
        self.call(
            &self.owner,
            "POST",
            &format!("/api/v1/projects/{project}/claims"),
            json!({"task_id":task["id"],"expected_task_revision":task["revision"],
                "mode":"work","policy_revision":1,
                "instruction_version":coordinator_core::INSTRUCTION_VERSION}),
        )
        .await
    }

    async fn complete_general(&self, project: &str, task: &Value, criterion: &str) -> Value {
        let (status, claim) = self.claim(project, task).await;
        assert_eq!(status, StatusCode::OK, "{claim}");
        let attempt = &claim["data"]["claim"]["attempt"];
        let (status, submitted) = self
            .call(
                &self.owner,
                "POST",
                &format!(
                    "/api/v1/projects/{project}/attempts/{}/submissions",
                    attempt["id"].as_str().unwrap()
                ),
                json!({"generation":attempt["generation"],"task_revision":task["revision"],
                    "project_policy_revision":1,"workflow_policy_revision":0,"kind":"general",
                    "summary":"General result ready","acceptance_evidence":[{"criterion":criterion,"evidence":"Observed in the objective route test"}],
                    "handoff":"Review the immutable result","repository":null,"base_revision":null,
                    "candidate_revision":null,"candidate_tree":null}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{submitted}");
        let review = activity(&submitted["data"], "agent_review");
        self.acknowledge(&self.reviewer, project).await;
        let (status, claimed_review) = self
            .call(
                &self.reviewer,
                "POST",
                &format!(
                    "/api/v1/projects/{project}/workflow-activities/{}/claim",
                    review["id"].as_str().unwrap()
                ),
                json!({"expected_submission_id":review["submission_id"],
                    "expected_project_policy_revision":1,"expected_workflow_policy_revision":0}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{claimed_review}");
        let review_attempt = &claimed_review["data"]["attempt"];
        let (status, completed) = self
            .call(
                &self.reviewer,
                "POST",
                &format!(
                    "/api/v1/projects/{project}/workflow-activities/{}/review",
                    review["id"].as_str().unwrap()
                ),
                json!({"generation":review_attempt["generation"],"submission_id":review["submission_id"],
                    "decision":"approved","summary":"Exact evidence approved","findings":[]}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{completed}");
        completed["data"].clone()
    }
}

#[tokio::test]
async fn required_children_block_selection_then_objective_completes_its_own_review() {
    let fixture = Fixture::new().await;
    let project = fixture.project("objective-completion").await;
    let required = fixture.task(&project, "Complete required child").await;
    let optional = fixture.task(&project, "Optional follow-up").await;
    let objective = fixture
        .objective(
            &project,
            "Release objective",
            vec![
                json!({"task_id":required["id"],"required":true}),
                json!({"task_id":optional["id"],"required":false}),
            ],
        )
        .await;
    assert_eq!(objective["kind"], "general");
    assert_eq!(objective["objective_id"], objective["id"]);
    assert_eq!(objective["required_children_ready"], false);
    assert_eq!(objective["child_count"], 2);
    assert_eq!(objective["required_child_count"], 1);
    assert_eq!(
        fixture.claim(&project, &objective).await.0,
        StatusCode::CONFLICT
    );

    fixture
        .complete_general(&project, &required, "required behavior verified")
        .await;
    let (status, detail) = fixture
        .call(
            &fixture.owner,
            "GET",
            &format!(
                "/api/v1/projects/{project}/objectives/{}",
                objective["id"].as_str().unwrap()
            ),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["data"]["required_children_ready"], true);
    assert_eq!(detail["data"]["children"][0]["title"], required["title"]);
    assert_eq!(detail["data"]["children"][0]["lifecycle"], "done");
    assert_eq!(detail["data"]["children"][1]["lifecycle"], "open");

    let current = detail["data"].clone();
    let (status, claim) = fixture.claim(&project, &current).await;
    assert_eq!(status, StatusCode::OK, "{claim}");
    let (status, frozen) = fixture
        .call(
            &fixture.owner,
            "PATCH",
            &format!(
                "/api/v1/projects/{project}/objectives/{}/children",
                objective["id"].as_str().unwrap()
            ),
            json!({"expected_revision":1,"children":[]}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{frozen}");
    assert_eq!(frozen["error"]["code"], "objective_membership_frozen");

    let parent_attempt = &claim["data"]["claim"]["attempt"];
    let submission_path = format!(
        "/api/v1/projects/{project}/attempts/{}/submissions",
        parent_attempt["id"].as_str().unwrap()
    );
    let submission_body = json!({"generation":parent_attempt["generation"],"task_revision":current["revision"],
        "project_policy_revision":1,"workflow_policy_revision":0,"kind":"general",
        "summary":"Objective evidence ready",
        "acceptance_evidence":[{"criterion":"Required children and objective evidence are reviewed","evidence":"Required child completion and objective result verified"}],
        "handoff":"Perform the objective's independent review","repository":null,
        "base_revision":null,"candidate_revision":null,"candidate_tree":null});
    sqlx::query("UPDATE tasks SET lifecycle='open' WHERE id=?")
        .bind(required["id"].as_str().unwrap())
        .execute(&fixture.state.pool)
        .await
        .unwrap();
    let (status, blocked_submission) = fixture
        .call(
            &fixture.owner,
            "POST",
            &submission_path,
            submission_body.clone(),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{blocked_submission}");
    assert_eq!(
        blocked_submission["error"]["code"],
        "objective_children_incomplete"
    );
    sqlx::query("UPDATE tasks SET lifecycle='done' WHERE id=?")
        .bind(required["id"].as_str().unwrap())
        .execute(&fixture.state.pool)
        .await
        .unwrap();
    let (status, submitted) = fixture
        .call(&fixture.owner, "POST", &submission_path, submission_body)
        .await;
    assert_eq!(status, StatusCode::OK, "{submitted}");
    let review = activity(&submitted["data"], "agent_review");
    let (status, review_claim) = fixture
        .call(
            &fixture.reviewer,
            "POST",
            &format!(
                "/api/v1/projects/{project}/workflow-activities/{}/claim",
                review["id"].as_str().unwrap()
            ),
            json!({"expected_submission_id":review["submission_id"],
                "expected_project_policy_revision":1,"expected_workflow_policy_revision":0}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{review_claim}");
    let review_path = format!(
        "/api/v1/projects/{project}/workflow-activities/{}/review",
        review["id"].as_str().unwrap()
    );
    let review_body = json!({"generation":review_claim["data"]["attempt"]["generation"],
        "submission_id":review["submission_id"],"decision":"approved",
        "summary":"Objective acceptance independently approved","findings":[]});
    sqlx::query("UPDATE tasks SET lifecycle='open' WHERE id=?")
        .bind(required["id"].as_str().unwrap())
        .execute(&fixture.state.pool)
        .await
        .unwrap();
    let (status, blocked_completion) = fixture
        .call(&fixture.reviewer, "POST", &review_path, review_body.clone())
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{blocked_completion}");
    assert_eq!(
        blocked_completion["error"]["code"],
        "objective_children_incomplete"
    );
    sqlx::query("UPDATE tasks SET lifecycle='done' WHERE id=?")
        .bind(required["id"].as_str().unwrap())
        .execute(&fixture.state.pool)
        .await
        .unwrap();
    let (status, completed) = fixture
        .call(&fixture.reviewer, "POST", &review_path, review_body)
        .await;
    assert_eq!(status, StatusCode::OK, "{completed}");
    assert_eq!(completed["data"]["work_status"], "done");
    let optional_lifecycle: String = sqlx::query_scalar("SELECT lifecycle FROM tasks WHERE id=?")
        .bind(optional["id"].as_str().unwrap())
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap();
    assert_eq!(optional_lifecycle, "open");
}

#[tokio::test]
async fn membership_updates_are_revisioned_and_reject_stale_or_duplicate_parent_scope() {
    let fixture = Fixture::new().await;
    let project = fixture.project("objective-revisions").await;
    let child = fixture.task(&project, "Revisioned child").await;
    let objective = fixture
        .objective(&project, "First parent", Vec::new())
        .await;
    let path = format!(
        "/api/v1/projects/{project}/objectives/{}/children",
        objective["id"].as_str().unwrap()
    );
    let (status, updated) = fixture
        .call(
            &fixture.owner,
            "PATCH",
            &path,
            json!({"expected_revision":1,"children":[{"task_id":child["id"],"required":true}]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["data"]["objective_revision"], 2);
    assert_eq!(updated["data"]["revision"], 2);
    assert_eq!(
        updated["data"]["membership_history"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let (status, first_history) = fixture
        .call(
            &fixture.owner,
            "GET",
            &format!(
                "/api/v1/projects/{project}/objectives/{}?limit=1",
                objective["id"].as_str().unwrap()
            ),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{first_history}");
    assert_eq!(
        first_history["data"]["membership_history"][0]["revision"],
        2
    );
    assert_eq!(first_history["data"]["membership_history_next_cursor"], 2);
    let (status, older_history) = fixture
        .call(
            &fixture.owner,
            "GET",
            &format!(
                "/api/v1/projects/{project}/objectives/{}?limit=1&cursor=2",
                objective["id"].as_str().unwrap()
            ),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{older_history}");
    assert_eq!(
        older_history["data"]["membership_history"][0]["revision"],
        1
    );
    assert!(older_history["data"]["membership_history_next_cursor"].is_null());
    let (status, stale) = fixture
        .call(
            &fixture.owner,
            "PATCH",
            &path,
            json!({"expected_revision":1,"children":[]}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    assert_eq!(stale["error"]["code"], "revision_conflict");

    let second = fixture
        .objective(&project, "Second parent", Vec::new())
        .await;
    let (status, duplicate_parent) = fixture
        .call(
            &fixture.owner,
            "PATCH",
            &format!(
                "/api/v1/projects/{project}/objectives/{}/children",
                second["id"].as_str().unwrap()
            ),
            json!({"expected_revision":1,"children":[{"task_id":child["id"],"required":false}]}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{duplicate_parent}");
    assert_eq!(
        duplicate_parent["error"]["code"],
        "objective_parent_conflict"
    );
}

#[tokio::test]
async fn objective_scope_is_project_local_and_combined_dag_cycles_are_rejected() {
    let fixture = Fixture::new().await;
    let project = fixture.project("objective-cycles").await;
    let other_project = fixture.project("objective-cross-project").await;
    let foreign = fixture.task(&other_project, "Foreign child").await;
    let (status, cross_project) = fixture
        .call(
            &fixture.owner,
            "POST",
            &format!("/api/v1/projects/{project}/objectives"),
            json!({"title":"Invalid scope","description":"","acceptance_criteria":["Never created"],
                "children":[{"task_id":foreign["id"],"required":true}]}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{cross_project}");

    let objective = fixture
        .objective(&project, "Cycle parent", Vec::new())
        .await;
    let child = fixture.task(&project, "Cycle child").await;
    let (status, edited) = fixture
        .call(
            &fixture.owner,
            "PATCH",
            &format!("/api/v1/projects/{project}/tasks/{}", child["id"].as_str().unwrap()),
            json!({"expected_revision":1,"title":child["title"],"description":"objective route test",
                "acceptance_criteria":["required behavior verified"],"priority":2,
                "depends_on":[objective["id"]],"planned":false}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{edited}");
    let (status, mixed_cycle) = fixture
        .call(
            &fixture.owner,
            "PATCH",
            &format!(
                "/api/v1/projects/{project}/objectives/{}/children",
                objective["id"].as_str().unwrap()
            ),
            json!({"expected_revision":1,"children":[{"task_id":child["id"],"required":true}]}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{mixed_cycle}");
    assert_eq!(mixed_cycle["error"]["code"], "dependency_cycle");

    let nested = fixture
        .objective(&project, "Nested objective", Vec::new())
        .await;
    let (status, first_edge) = fixture
        .call(
            &fixture.owner,
            "PATCH",
            &format!(
                "/api/v1/projects/{project}/objectives/{}/children",
                objective["id"].as_str().unwrap()
            ),
            json!({"expected_revision":1,"children":[{"task_id":nested["id"],"required":false}]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{first_edge}");
    let (status, nested_cycle) = fixture
        .call(
            &fixture.owner,
            "PATCH",
            &format!(
                "/api/v1/projects/{project}/objectives/{}/children",
                nested["id"].as_str().unwrap()
            ),
            json!({"expected_revision":1,"children":[{"task_id":objective["id"],"required":false}]}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{nested_cycle}");
    assert_eq!(nested_cycle["error"]["code"], "dependency_cycle");
}

fn activity<'a>(snapshot: &'a Value, kind: &str) -> &'a Value {
    snapshot["activities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|value| value["kind"] == kind && value["status"] != "canceled")
        .unwrap()
}

async fn seed(state: &AppState, human: bool, name: &str) -> Caller {
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
        sqlx::query("INSERT INTO agent_sessions(id,principal_id,credential_id,workstation_id,proof_hash,created_at,capabilities,harness) VALUES(?,?,?,?,?,?,'[]','test')")
            .bind(&caller.session).bind(&caller.principal).bind(&caller.credential).bind(name)
            .bind(digest(&caller.proof)).bind(state.now()).execute(&state.pool).await.unwrap();
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
        .header("content-type", "application/json")
        .header("idempotency-key", key);
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
