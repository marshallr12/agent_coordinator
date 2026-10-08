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

const BASE: &str = "1111111111111111111111111111111111111111";
const CANDIDATE: &str = "2222222222222222222222222222222222222222";
const CANDIDATE_TREE: &str = "3333333333333333333333333333333333333333";
const RESULT: &str = "4444444444444444444444444444444444444444";
const RESULT_TREE: &str = "5555555555555555555555555555555555555555";

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
    _credential: String,
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
            database_path: dir.path().join("decision-authority.sqlite3"),
            public_origin: "http://127.0.0.1:8080".into(),
            allow_insecure_loopback: true,
            ..Config::default()
        })
        .await
        .unwrap();
        let clock = Arc::new(TestClock(AtomicI64::new(1_800_000_000_000)));
        state.clock = clock.clone();
        let admin = seed(&state, true, "decision-authority-admin").await;
        let a = seed(&state, false, "decision-authority-a").await;
        let b = seed(&state, false, "decision-authority-b").await;
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

    async fn project(&self, name: &str) -> String {
        let (status, value) = self
            .call(
                &self.admin,
                "POST",
                "/api/v1/projects",
                &Uuid::new_v4().to_string(),
                json!({"name":name,"repository_url":format!("https://example.test/{name}.git"),"target_branch":"main"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        value["data"]["id"].as_str().unwrap().into()
    }

    async fn task(&self, project: &str, kind: &str, title: &str) -> Value {
        let (status, value) = self
            .call(
                &self.a,
                "POST",
                &format!("/api/v1/projects/{project}/tasks"),
                &Uuid::new_v4().to_string(),
                json!({"title":title,"description":"decision authority regression","acceptance_criteria":["required behavior verified"],"kind":kind}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        value["data"].clone()
    }

    async fn acknowledge(&self, caller: &Caller, project: &str, policy_revision: i64) {
        let (status, value) = self
            .call(
                caller,
                "POST",
                &format!(
                    "/api/v1/sessions/{}/instruction-acknowledgments",
                    caller.session
                ),
                &Uuid::new_v4().to_string(),
                json!({"project_id":project,"policy_revision":policy_revision,"instruction_version":coordinator_core::INSTRUCTION_VERSION,"sections":[coordinator_core::REQUIRED_SECTION]}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
    }

    async fn claim(
        &self,
        caller: &Caller,
        project: &str,
        task: &Value,
        mode: &str,
        policy_revision: i64,
        key: &str,
    ) -> (StatusCode, Value, Value) {
        self.acknowledge(caller, project, policy_revision).await;
        let body = json!({"task_id":task["id"],"expected_task_revision":task["revision"],"mode":mode,
            "policy_revision":policy_revision,"instruction_version":coordinator_core::INSTRUCTION_VERSION});
        let (status, value) = self
            .call(
                caller,
                "POST",
                &format!("/api/v1/projects/{project}/claims"),
                key,
                body.clone(),
            )
            .await;
        (status, value, body)
    }

    async fn decision(
        &self,
        project: &str,
        tasks: Vec<Value>,
        policy_revision: i64,
        expires_at: Option<i64>,
    ) -> Value {
        let affected_tasks = tasks
            .into_iter()
            .map(|task| json!({"task_id":task["id"],"task_revision":task["revision"]}))
            .collect::<Vec<_>>();
        let (status, value) = self
            .call(
                &self.a,
                "POST",
                &format!("/api/v1/projects/{project}/decisions"),
                &Uuid::new_v4().to_string(),
                json!({"question":"May this work proceed?","options":["Proceed","Wait"],
                    "rationale":"Verify current authority projection","required_actor":"human",
                    "affected_tasks":affected_tasks,"policy_revision":policy_revision,
                    "environment":"test","conditions":"Operator verified the exact scope",
                    "expires_at":expires_at}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        value["data"].clone()
    }

    async fn allow(&self, project: &str, decision: &Value) {
        let (status, value) = self
            .call(
                &self.admin,
                "POST",
                &format!(
                    "/api/v1/projects/{project}/decisions/{}/answer",
                    decision["id"].as_str().unwrap()
                ),
                &Uuid::new_v4().to_string(),
                json!({"expected_generation":decision["generation"],"disposition":"allow",
                    "answer":"Proceed","rationale":"Exact scope verified","conditions_confirmed":true}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
    }

    async fn checkout(&self, caller: &Caller, project: &str, attempt: &Value, base: &str) {
        let (status, value) = self
            .call(
                caller,
                "POST",
                &format!(
                    "/api/v1/projects/{project}/attempts/{}/checkout",
                    attempt["id"].as_str().unwrap()
                ),
                &Uuid::new_v4().to_string(),
                json!({"generation":attempt["generation"],"workstation_id":format!("{}-workstation",caller.principal),
                    "identity":Uuid::new_v4().to_string(),"path":"/tmp/decision-authority-test",
                    "branch":"decision-authority-test","base_revision":base,"clean":true}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
    }
}

#[tokio::test]
async fn resumed_recovery_replays_and_attempt_detail_respect_a_new_decision() {
    let fixture = Fixture::new().await;
    let project = fixture.project("recovery-decision-authority").await;
    let task = fixture
        .task(&project, "general", "Recover exact saved work")
        .await;
    let (status, initial, _) = fixture
        .claim(&fixture.a, &project, &task, "work", 1, "initial-work")
        .await;
    assert_eq!(status, StatusCode::OK, "{initial}");
    fixture.clock.0.fetch_add(600_000, Ordering::SeqCst);

    let (status, recovery, recovery_body) = fixture
        .claim(
            &fixture.b,
            &project,
            &task,
            "recovery",
            1,
            "stable-recovery-claim",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{recovery}");
    let attempt = recovery["data"]["claim"]["attempt"].clone();
    let resolution_body = json!({"generation":attempt["generation"],"disposition":"resume",
        "summary":"Saved work and running producers were inspected.",
        "saved_work_checked":true,"running_jobs_checked":true});
    let resolution_path = format!(
        "/api/v1/projects/{project}/attempts/{}/recovery-resolution",
        attempt["id"].as_str().unwrap()
    );
    let (status, resolved) = fixture
        .call(
            &fixture.b,
            "POST",
            &resolution_path,
            "stable-recovery-resolution",
            resolution_body.clone(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{resolved}");
    assert_eq!(resolved["data"]["attempt"]["mode"], "work");

    fixture
        .decision(&project, vec![task.clone()], 1, None)
        .await;

    let (status, detail) = fixture
        .call(
            &fixture.b,
            "GET",
            &format!(
                "/api/v1/projects/{project}/attempts/{}",
                attempt["id"].as_str().unwrap()
            ),
            &Uuid::new_v4().to_string(),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["data"]["authority_valid"], false, "{detail}");

    let (status, replayed_claim) = fixture
        .call(
            &fixture.b,
            "POST",
            &format!("/api/v1/projects/{project}/claims"),
            "stable-recovery-claim",
            recovery_body,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{replayed_claim}");
    assert_eq!(
        replayed_claim["data"]["current_authority"]["valid"], false,
        "{replayed_claim}"
    );

    let (status, replayed_resolution) = fixture
        .call(
            &fixture.b,
            "POST",
            &resolution_path,
            "stable-recovery-resolution",
            resolution_body,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{replayed_resolution}");
    assert_eq!(
        replayed_resolution["error"]["code"], "decision_required",
        "{replayed_resolution}"
    );
}

#[tokio::test]
async fn workflow_detail_blocks_authority_for_subject_and_activity_decisions() {
    let fixture = Fixture::new().await;
    let project = fixture.project("review-decision-authority").await;
    let task = fixture
        .task(&project, "general", "Review exact evidence")
        .await;
    let (status, claimed, _) = fixture
        .claim(
            &fixture.a,
            &project,
            &task,
            "work",
            1,
            "review-subject-claim",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    let owner = claimed["data"]["claim"]["attempt"].clone();
    let (status, submitted) = fixture
        .call(
            &fixture.a,
            "POST",
            &format!(
                "/api/v1/projects/{project}/attempts/{}/submissions",
                owner["id"].as_str().unwrap()
            ),
            &Uuid::new_v4().to_string(),
            json!({"generation":owner["generation"],"task_revision":task["revision"],
                "project_policy_revision":1,"workflow_policy_revision":0,"kind":"general",
                "summary":"Candidate ready","acceptance_evidence":[{"criterion":"required behavior verified","evidence":"Observed directly"}],
                "handoff":"Review the exact candidate","repository":null,"base_revision":null,
                "candidate_revision":null,"candidate_tree":null}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{submitted}");
    let review = activity(&submitted["data"], "agent_review").clone();
    fixture.acknowledge(&fixture.b, &project, 1).await;
    let (status, review_claim) = fixture
        .call(
            &fixture.b,
            "POST",
            &format!(
                "/api/v1/projects/{project}/workflow-activities/{}/claim",
                review["id"].as_str().unwrap()
            ),
            &Uuid::new_v4().to_string(),
            json!({"expected_submission_id":review["submission_id"],
                "expected_project_policy_revision":1,"expected_workflow_policy_revision":0}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{review_claim}");

    let subject_decision = fixture
        .decision(&project, vec![task.clone()], 1, None)
        .await;
    assert_workflow_authority_blocked(&fixture, &project, &review).await;
    fixture.allow(&project, &subject_decision).await;

    let activity_task = get_task(
        &fixture,
        &project,
        review["activity_task_id"].as_str().unwrap(),
    )
    .await;
    fixture
        .decision(&project, vec![activity_task], 1, None)
        .await;
    assert_workflow_authority_blocked(&fixture, &project, &review).await;
}

#[tokio::test]
async fn expired_subject_decision_removes_integration_publication_authority() {
    let fixture = Fixture::new().await;
    let project = fixture.project("integration-decision-authority").await;
    let (status, policy) = fixture
        .call(
            &fixture.admin,
            "PATCH",
            &format!("/api/v1/projects/{project}/policy"),
            &Uuid::new_v4().to_string(),
            json!({"expected_revision":1,"review_mode":"none","recovery_mode":"agent",
                "lease_seconds":600,"rules":"","agent_rule_editing":false,
                "automatic_integration":true}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{policy}");
    let (status, workflow_policy) = fixture
        .call(
            &fixture.admin,
            "PUT",
            &format!("/api/v1/projects/{project}/workflow-policy"),
            &Uuid::new_v4().to_string(),
            json!({"expected_revision":0,"canonical_repository_key":"decision-authority",
                "required_checks":[{"identity":"workspace-tests","version":"v1","environment":"linux-ci"}]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{workflow_policy}");
    let task = fixture
        .task(&project, "code", "Publish the exact candidate")
        .await;
    let decision = fixture
        .decision(
            &project,
            vec![task.clone()],
            2,
            Some(fixture.state.now() + 1_000),
        )
        .await;
    fixture.allow(&project, &decision).await;
    let (status, claimed, _) = fixture
        .claim(
            &fixture.a,
            &project,
            &task,
            "work",
            2,
            "integration-subject-claim",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    let owner = claimed["data"]["claim"]["attempt"].clone();
    fixture.checkout(&fixture.a, &project, &owner, BASE).await;
    let repository = String::from("https://example.test/integration-decision-authority.git");
    let (status, submitted) = fixture
        .call(
            &fixture.a,
            "POST",
            &format!(
                "/api/v1/projects/{project}/attempts/{}/submissions",
                owner["id"].as_str().unwrap()
            ),
            &Uuid::new_v4().to_string(),
            json!({"generation":owner["generation"],"task_revision":task["revision"],
                "project_policy_revision":2,"workflow_policy_revision":1,"kind":"code",
                "summary":"Candidate ready","acceptance_evidence":[{"criterion":"required behavior verified","evidence":"Observed directly"}],
                "handoff":"Integrate the exact candidate","repository":repository,"base_revision":BASE,
                "candidate_revision":CANDIDATE,"candidate_tree":CANDIDATE_TREE,
                "candidate_remote":"decision-authority","candidate_ref":"refs/agent-coordinator/candidates/decision-authority"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{submitted}");
    let integration = activity(&submitted["data"], "integration").clone();
    fixture.acknowledge(&fixture.b, &project, 2).await;
    let (status, integration_claim) = fixture
        .call(
            &fixture.b,
            "POST",
            &format!(
                "/api/v1/projects/{project}/workflow-activities/{}/claim",
                integration["id"].as_str().unwrap()
            ),
            &Uuid::new_v4().to_string(),
            json!({"expected_submission_id":integration["submission_id"],
                "expected_project_policy_revision":2,"expected_workflow_policy_revision":1}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{integration_claim}");
    let integration_attempt = integration_claim["data"]["attempt"].clone();
    fixture
        .checkout(&fixture.b, &project, &integration_attempt, CANDIDATE)
        .await;
    let (status, intent) = fixture
        .call(
            &fixture.b,
            "POST",
            &format!(
                "/api/v1/projects/{project}/workflow-activities/{}/publication-intent",
                integration["id"].as_str().unwrap()
            ),
            &Uuid::new_v4().to_string(),
            json!({"generation":integration_attempt["generation"],"submission_id":integration["submission_id"],
                "observed_target_revision":BASE,"observed_target_tree":BASE,
                "result_revision":RESULT,"result_tree":RESULT_TREE}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{intent}");
    insert_successful_check(&fixture, &project, &integration, &integration_attempt).await;

    let before = workflow_detail(&fixture, &project, &integration).await;
    assert_eq!(
        before["data"]["current_authority"]["valid"], true,
        "{before}"
    );
    assert_eq!(before["data"]["publication_allowed"], true, "{before}");

    fixture.clock.0.fetch_add(1_001, Ordering::SeqCst);
    let after = workflow_detail(&fixture, &project, &integration).await;
    assert_eq!(
        after["data"]["current_authority"]["valid"], false,
        "{after}"
    );
    assert_eq!(after["data"]["publication_allowed"], false, "{after}");
}

async fn assert_workflow_authority_blocked(fixture: &Fixture, project: &str, activity: &Value) {
    let detail = workflow_detail(fixture, project, activity).await;
    assert_eq!(
        detail["data"]["current_authority"]["valid"], false,
        "{detail}"
    );
    assert_eq!(detail["data"]["publication_allowed"], false, "{detail}");
}

async fn workflow_detail(fixture: &Fixture, project: &str, activity: &Value) -> Value {
    let (status, value) = fixture
        .call(
            &fixture.b,
            "GET",
            &format!(
                "/api/v1/projects/{project}/workflow-activities/{}",
                activity["id"].as_str().unwrap()
            ),
            &Uuid::new_v4().to_string(),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value
}

async fn get_task(fixture: &Fixture, project: &str, task: &str) -> Value {
    let (status, value) = fixture
        .call(
            &fixture.b,
            "GET",
            &format!("/api/v1/projects/{project}/tasks/{task}"),
            &Uuid::new_v4().to_string(),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value["data"].clone()
}

async fn insert_successful_check(
    fixture: &Fixture,
    project: &str,
    activity: &Value,
    attempt: &Value,
) {
    let reservation = Uuid::new_v4().to_string();
    let job = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO reservations(id,project_id,attempt_id,generation,state,created_by,created_at,released_at,released_by,release_reason) VALUES(?,?,?,?,'released',?,?,?,?,'test check complete')")
        .bind(&reservation).bind(project).bind(attempt["id"].as_str().unwrap())
        .bind(attempt["generation"].as_i64().unwrap()).bind(&fixture.b.principal)
        .bind(fixture.state.now()).bind(fixture.state.now()).bind(&fixture.b.principal)
        .execute(&fixture.state.pool).await.unwrap();
    sqlx::query("INSERT INTO jobs(id,producer_id,project_id,task_id,attempt_id,generation,runner_instance_id,workstation_id,label,source_revision,source_tree,reservation_id,state,last_sequence,last_observed_at,exit_code,inputs_unchanged,summary,created_at,check_identity,check_version,check_environment) VALUES(?,?,?,?,?,?,?,?,?,?,?,?, 'succeeded',1,?,0,1,'passed',?,?,?,?)")
        .bind(&job).bind(Uuid::new_v4().to_string()).bind(project)
        .bind(activity["activity_task_id"].as_str().unwrap()).bind(attempt["id"].as_str().unwrap())
        .bind(attempt["generation"].as_i64().unwrap()).bind(Uuid::new_v4().to_string())
        .bind(format!("{}-workstation",fixture.b.principal)).bind("workspace tests")
        .bind(RESULT).bind(RESULT_TREE).bind(&reservation).bind(fixture.state.now())
        .bind(fixture.state.now()).bind("workspace-tests").bind("v1").bind("linux-ci")
        .execute(&fixture.state.pool).await.unwrap();
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
        _credential: Uuid::new_v4().to_string(),
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
        .bind(&caller._credential)
        .bind(&caller.principal)
        .bind(digest(&caller.token))
        .bind(state.now())
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO agent_sessions(id,principal_id,credential_id,workstation_id,proof_hash,created_at,capabilities,harness) VALUES(?,?,?,?,?,?,'[]','test')")
            .bind(&caller.session).bind(&caller.principal).bind(&caller._credential)
            .bind(format!("{}-workstation",caller.principal)).bind(digest(&caller.proof))
            .bind(state.now()).execute(&state.pool).await.unwrap();
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

fn policy_body(revision: i64, relay: Option<bool>, agent_rule_editing: bool) -> Value {
    let mut body = json!({"expected_revision":revision,"review_mode":"none","recovery_mode":"agent",
        "lease_seconds":600,"rules":"","agent_rule_editing":agent_rule_editing,
        "automatic_integration":true});
    if let Some(relay) = relay {
        body["allow_relayed_human_answers"] = json!(relay);
    }
    body
}

impl Fixture {
    async fn set_policy(&self, caller: &Caller, project: &str, body: Value) -> (StatusCode, Value) {
        self.call(
            caller,
            "PATCH",
            &format!("/api/v1/projects/{project}/policy"),
            &Uuid::new_v4().to_string(),
            body,
        )
        .await
    }

    async fn answer(
        &self,
        caller: &Caller,
        project: &str,
        decision: &Value,
        generation: Value,
        relay: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut body = json!({"expected_generation":generation,"disposition":"allow",
            "answer":"Proceed","rationale":"The user chose Proceed.","conditions_confirmed":true});
        if let Some(relay) = relay {
            body["relay"] = relay;
        }
        self.call(
            caller,
            "POST",
            &format!(
                "/api/v1/projects/{project}/decisions/{}/answer",
                decision["id"].as_str().unwrap()
            ),
            &Uuid::new_v4().to_string(),
            body,
        )
        .await
    }
}

fn relay_evidence() -> Value {
    json!({"prompt":"May this work proceed? Options: Proceed, Wait","response":"Proceed"})
}

#[tokio::test]
async fn relayed_human_answer_records_the_relay_and_its_authority() {
    let fixture = Fixture::new().await;
    let project = fixture.project("relayed-answer").await;
    let task = fixture.task(&project, "general", "Needs a human").await;
    let (status, policy) = fixture
        .set_policy(&fixture.admin, &project, policy_body(1, Some(true), false))
        .await;
    assert_eq!(status, StatusCode::OK, "{policy}");
    assert_eq!(policy["data"]["allow_relayed_human_answers"], true);
    let decision = fixture.decision(&project, vec![task], 2, None).await;

    let (status, answered) = fixture
        .answer(
            &fixture.b,
            &project,
            &decision,
            decision["generation"].clone(),
            Some(relay_evidence()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{answered}");
    let answer = &answered["data"]["answer"];
    assert_eq!(answered["data"]["status"], "allowed");
    assert_eq!(answer["relayed"], true);
    assert_eq!(answer["actor_id"], fixture.b.principal);
    assert_eq!(answer["actor_session_id"], fixture.b.session);
    assert_eq!(answer["relay"]["prompt"], relay_evidence()["prompt"]);
    assert_eq!(answer["relay"]["response"], "Proceed");
    assert_eq!(answer["relay"]["relayed_by_session"], fixture.b.session);
    assert_eq!(answer["relay"]["authorized_by"], fixture.admin.principal);
    assert_eq!(answer["relay"]["policy_revision"], 2);

    let (status, shown) = fixture
        .call(
            &fixture.a,
            "GET",
            &format!(
                "/api/v1/projects/{project}/decisions/{}",
                decision["id"].as_str().unwrap()
            ),
            &Uuid::new_v4().to_string(),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{shown}");
    assert_eq!(shown["data"]["answer"]["relayed"], true);
    let entry = &shown["data"]["history"][0];
    assert_eq!(entry["answered_by_session"], fixture.b.session);
    assert_eq!(entry["relay"]["response"], "Proceed");
    assert_eq!(entry["relay"]["authorized_by"], fixture.admin.principal);

    // A human's direct answer is plainly not relayed.
    let direct_task = fixture.task(&project, "general", "Direct answer").await;
    let direct = fixture.decision(&project, vec![direct_task], 2, None).await;
    let (status, answered) = fixture
        .answer(
            &fixture.admin,
            &project,
            &direct,
            direct["generation"].clone(),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{answered}");
    assert_eq!(answered["data"]["answer"]["relayed"], false);
    assert!(answered["data"]["answer"]["relay"].is_null());
    assert!(answered["data"]["history"][0]["relay"].is_null());
}

#[tokio::test]
async fn unrelayed_or_unauthorized_agent_answers_to_human_decisions_are_refused() {
    let fixture = Fixture::new().await;
    let project = fixture.project("unrelayed-answer").await;
    let task = fixture.task(&project, "general", "Needs a human").await;

    // Relaying is off by default, so even a relay object is refused.
    let decision = fixture
        .decision(&project, vec![task.clone()], 1, None)
        .await;
    let generation = decision["generation"].clone();
    let (status, refused) = fixture
        .answer(&fixture.b, &project, &decision, generation.clone(), None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(refused["error"]["code"], "operation_not_permitted");
    let (status, refused) = fixture
        .answer(
            &fixture.b,
            &project,
            &decision,
            generation,
            Some(relay_evidence()),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(refused["error"]["code"], "operation_not_permitted");

    // Agents cannot switch relaying on, even with delegated rule editing.
    let (status, policy) = fixture
        .set_policy(&fixture.admin, &project, policy_body(1, None, true))
        .await;
    assert_eq!(status, StatusCode::OK, "{policy}");
    let (status, refused) = fixture
        .set_policy(&fixture.a, &project, policy_body(2, Some(true), true))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(refused["error"]["code"], "operation_not_permitted");

    // With relaying enabled, an answer without the relay object stays refused,
    // and a relay is rejected where no human is required or by a human.
    let (status, policy) = fixture
        .set_policy(&fixture.admin, &project, policy_body(2, Some(true), true))
        .await;
    assert_eq!(status, StatusCode::OK, "{policy}");
    let decision = fixture
        .decision(&project, vec![task.clone()], 3, None)
        .await;
    let generation = decision["generation"].clone();
    let (status, refused) = fixture
        .answer(&fixture.b, &project, &decision, generation.clone(), None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(refused["error"]["code"], "operation_not_permitted");
    let (status, rejected) = fixture
        .answer(
            &fixture.admin,
            &project,
            &decision,
            generation,
            Some(relay_evidence()),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected}");
    let (status, agent_decision) = fixture
        .call(
            &fixture.a,
            "POST",
            &format!("/api/v1/projects/{project}/decisions"),
            &Uuid::new_v4().to_string(),
            json!({"question":"Which option?","options":["Proceed","Wait"],"rationale":"Agent call",
                "required_actor":"either","affected_tasks":[{"task_id":task["id"],"task_revision":task["revision"]}],
                "policy_revision":3,"environment":"test","conditions":"none"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{agent_decision}");
    let agent_decision = agent_decision["data"].clone();
    let (status, rejected) = fixture
        .answer(
            &fixture.b,
            &project,
            &agent_decision,
            agent_decision["generation"].clone(),
            Some(relay_evidence()),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected}");
}

#[tokio::test]
async fn relayed_answer_for_a_stale_generation_is_refused() {
    let fixture = Fixture::new().await;
    let project = fixture.project("stale-relay").await;
    let task = fixture.task(&project, "general", "Needs a human").await;
    let (status, policy) = fixture
        .set_policy(&fixture.admin, &project, policy_body(1, Some(true), false))
        .await;
    assert_eq!(status, StatusCode::OK, "{policy}");
    let decision = fixture
        .decision(&project, vec![task.clone()], 2, None)
        .await;
    let (status, reopened) = fixture
        .call(
            &fixture.admin,
            "POST",
            &format!(
                "/api/v1/projects/{project}/decisions/{}/reopen",
                decision["id"].as_str().unwrap()
            ),
            &Uuid::new_v4().to_string(),
            json!({"expected_generation":1,"rationale":"Scope changed","policy_revision":2,
                "affected_tasks":[{"task_id":task["id"],"task_revision":task["revision"]}],
                "environment":"test","conditions":"Operator verified the exact scope"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{reopened}");
    assert_eq!(reopened["data"]["generation"], 2);

    let (status, refused) = fixture
        .answer(
            &fixture.b,
            &project,
            &decision,
            json!(1),
            Some(relay_evidence()),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["error"]["code"], "generation_conflict");
    let (status, accepted) = fixture
        .answer(
            &fixture.b,
            &project,
            &decision,
            json!(2),
            Some(relay_evidence()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    assert_eq!(accepted["data"]["answer"]["relayed"], true);
}
