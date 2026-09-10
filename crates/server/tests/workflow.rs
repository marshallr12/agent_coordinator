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

#[tokio::test]
async fn revoked_reviewer_claim_receipt_never_grants_current_authority() {
    let f = Fixture::new().await;
    let p = f
        .project("review-revoked", "https://example.test/revoked.git")
        .await;
    let t = f.task(&p, "general", "Review revocation").await;
    let owner = f.claim(&f.a, &p, &t, 1).await;
    let submitted = f
        .submit(&f.a, &p, &t, &owner, "general", 1, None, None, None, None)
        .await;
    let review = activity(&submitted, "agent_review").clone();
    f.ack(&f.b, &p, 1).await;
    let path = format!(
        "/api/v1/projects/{p}/workflow-activities/{}/claim",
        review["id"].as_str().unwrap()
    );
    let body = json!({"expected_submission_id":review["submission_id"],"expected_project_policy_revision":1,"expected_workflow_policy_revision":0});
    let (status, first) = call(
        f.app.clone(),
        &f.b,
        "POST",
        &path,
        "stable-review-claim",
        body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    sqlx::query("UPDATE credentials SET revoked_at=? WHERE id=?")
        .bind(f.state.now())
        .bind(&f.b.credential)
        .execute(&f.state.pool)
        .await
        .unwrap();
    let (status, rejected) = call(
        f.app.clone(),
        &f.b,
        "POST",
        &path,
        "stable-review-claim",
        body,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{rejected}");
    let (status, recovered) = f.claim_activity(&f.c, &p, &review, 1, 0).await;
    assert_eq!(status, StatusCode::OK, "{recovered}");
    assert_ne!(
        recovered["data"]["attempt"]["id"],
        first["data"]["attempt"]["id"]
    );
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
    c: Caller,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::open(Config {
            database_path: dir.path().join("workflow.sqlite3"),
            public_origin: "http://127.0.0.1:8080".into(),
            allow_insecure_loopback: true,
            ..Config::default()
        })
        .await
        .unwrap();
        let clock = Arc::new(TestClock(AtomicI64::new(1_800_000_000_000)));
        state.clock = clock.clone();
        let admin = seed(&state, true, "workflow-admin").await;
        let a = seed(&state, false, "workflow-a").await;
        let b = seed(&state, false, "workflow-b").await;
        let c = seed(&state, false, "workflow-c").await;
        Self {
            app: router(state.clone()),
            state,
            clock,
            _dir: dir,
            admin,
            a,
            b,
            c,
        }
    }
    async fn call(&self, c: &Caller, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
        call(
            self.app.clone(),
            c,
            method,
            path,
            &Uuid::new_v4().to_string(),
            body,
        )
        .await
    }
    async fn project(&self, name: &str, repo: &str) -> String {
        let (s, v) = self
            .call(
                &self.admin,
                "POST",
                "/api/v1/projects",
                json!({"name":name,"repository_url":repo,"target_branch":"main"}),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["data"]["id"].as_str().unwrap().into()
    }
    async fn policy_none(&self, p: &str) {
        let (s,v)=self.call(&self.admin,"PATCH",&format!("/api/v1/projects/{p}/policy"),json!({"expected_revision":1,"review_mode":"none","recovery_mode":"agent","lease_seconds":600,"rules":"","agent_rule_editing":false,"automatic_integration":true})).await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }
    async fn workflow_policy(&self, p: &str, key: &str) {
        let (s,v)=self.call(&self.admin,"PUT",&format!("/api/v1/projects/{p}/workflow-policy"),json!({"expected_revision":0,"canonical_repository_key":key,"required_checks":[{"identity":"workspace-tests","version":"v1","environment":"linux-ci"}]})).await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }
    async fn task(&self, p: &str, kind: &str, title: &str) -> Value {
        let (s,v)=self.call(&self.a,"POST",&format!("/api/v1/projects/{p}/tasks"),json!({"title":title,"description":"workflow test","acceptance_criteria":["required behavior verified"],"kind":kind})).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["data"].clone()
    }
    async fn ack(&self, c: &Caller, p: &str, policy: i64) {
        let (s,v)=self.call(c,"POST",&format!("/api/v1/sessions/{}/instruction-acknowledgments",c.session),json!({"project_id":p,"policy_revision":policy,"instruction_version":coordinator_core::INSTRUCTION_VERSION,"sections":[coordinator_core::REQUIRED_SECTION]})).await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }
    async fn claim(&self, c: &Caller, p: &str, t: &Value, policy: i64) -> Value {
        self.ack(c, p, policy).await;
        let (s,v)=self.call(c,"POST",&format!("/api/v1/projects/{p}/claims"),json!({"task_id":t["id"],"expected_task_revision":t["revision"],"mode":"work","policy_revision":policy,"instruction_version":coordinator_core::INSTRUCTION_VERSION})).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["data"]["claim"]["attempt"].clone()
    }
    async fn checkout(&self, c: &Caller, p: &str, attempt: &Value, base: &str) {
        let (s,v)=self.call(c,"POST",&format!("/api/v1/projects/{p}/attempts/{}/checkout",attempt["id"].as_str().unwrap()),json!({"generation":attempt["generation"],"workstation_id":format!("{0}-workstation",c.principal),"identity":Uuid::new_v4().to_string(),"path":"/tmp/workflow-test","branch":"workflow-test","base_revision":base,"clean":true})).await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }
    async fn check_job(
        &self,
        p: &str,
        activity: &Value,
        attempt: &Value,
        source_revision: &str,
        source_tree: &str,
    ) -> String {
        let reservation = Uuid::new_v4().to_string();
        let job = Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO reservations(id,project_id,attempt_id,generation,state,created_by,created_at,released_at,released_by,release_reason) VALUES(?,?,?,?,'released',?,?,?,?,'workflow check complete')")
            .bind(&reservation).bind(p).bind(attempt["id"].as_str().unwrap()).bind(attempt["generation"].as_i64().unwrap()).bind(&self.c.principal).bind(self.state.now()).bind(self.state.now()).bind(&self.c.principal).execute(&self.state.pool).await.unwrap();
        sqlx::query("INSERT INTO jobs(id,producer_id,project_id,task_id,attempt_id,generation,runner_instance_id,workstation_id,label,source_revision,source_tree,reservation_id,state,last_sequence,last_observed_at,exit_code,inputs_unchanged,summary,created_at,check_identity,check_version,check_environment) VALUES(?,?,?,?,?,?,?,?,?,?,?,?, 'succeeded',1,?,0,1,'passed',?,?,?,?)")
            .bind(&job).bind(Uuid::new_v4().to_string()).bind(p).bind(activity["activity_task_id"].as_str().unwrap()).bind(attempt["id"].as_str().unwrap()).bind(attempt["generation"].as_i64().unwrap()).bind(Uuid::new_v4().to_string()).bind(format!("{}-workstation",self.c.principal)).bind("workspace tests").bind(source_revision).bind(source_tree).bind(&reservation).bind(self.state.now()).bind(self.state.now()).bind("workspace-tests").bind("v1").bind("linux-ci").execute(&self.state.pool).await.unwrap();
        job
    }
    #[allow(clippy::too_many_arguments)]
    async fn submit(
        &self,
        c: &Caller,
        p: &str,
        t: &Value,
        a: &Value,
        kind: &str,
        policy: i64,
        repo: Option<&str>,
        base: Option<&str>,
        candidate: Option<&str>,
        tree: Option<&str>,
    ) -> Value {
        let (s,v)=self.call(c,"POST",&format!("/api/v1/projects/{p}/attempts/{}/submissions",a["id"].as_str().unwrap()),json!({"generation":a["generation"],"task_revision":t["revision"],"project_policy_revision":policy,"workflow_policy_revision":if kind=="code"{1}else{0},"kind":kind,"summary":"candidate ready","acceptance_evidence":[{"criterion":"required behavior verified","evidence":"verified in workflow test"}],"handoff":"review exact evidence","repository":repo,"base_revision":base,"candidate_revision":candidate,"candidate_tree":tree})).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["data"].clone()
    }
    async fn claim_activity(
        &self,
        c: &Caller,
        p: &str,
        a: &Value,
        policy: i64,
        workflow_policy: i64,
    ) -> (StatusCode, Value) {
        self.ack(c, p, policy).await;
        self.call(c,"POST",&format!("/api/v1/projects/{p}/workflow-activities/{}/claim",a["id"].as_str().unwrap()),json!({"expected_submission_id":a["submission_id"],"expected_project_policy_revision":policy,"expected_workflow_policy_revision":workflow_policy})).await
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
    .bind(if human { Some("unused") } else { None })
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
        sqlx::query("INSERT INTO agent_sessions(id,principal_id,credential_id,workstation_id,proof_hash,created_at,capabilities,harness) VALUES(?,?,?,?,?,?,'[]','test')").bind(&c.session).bind(&c.principal).bind(&c.credential).bind(format!("{}-workstation",c.principal)).bind(digest(&c.proof)).bind(state.now()).execute(&state.pool).await.unwrap();
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
    let mut r = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("idempotency-key", key);
    if c.human {
        r = r
            .header("cookie", format!("coordinator_local={}", c.token))
            .header("origin", "http://127.0.0.1:8080")
            .header(
                "x-csrf-token",
                digest(&format!("coordinator-browser-csrf-v1:{}", c.token)),
            );
    } else {
        r = r
            .header("authorization", format!("Bearer {}", c.token))
            .header("x-coordinator-session", &c.session)
            .header("x-coordinator-session-proof", &c.proof);
    }
    let response = app
        .oneshot(r.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn activity<'a>(snapshot: &'a Value, kind: &str) -> &'a Value {
    snapshot["activities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["kind"] == kind && v["status"] != "canceled")
        .unwrap()
}

#[tokio::test]
async fn general_submission_requires_an_independent_reviewer_and_then_completes() {
    let f = Fixture::new().await;
    let p = f
        .project("general-review", "https://example.test/general.git")
        .await;
    let t = f.task(&p, "general", "Prepare release notes").await;
    let owner = f.claim(&f.a, &p, &t, 1).await;
    let submitted = f
        .submit(&f.a, &p, &t, &owner, "general", 1, None, None, None, None)
        .await;
    let review = activity(&submitted, "agent_review");
    let (status, _) = f.claim_activity(&f.a, &p, review, 1, 0).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, claimed) = f.claim_activity(&f.b, &p, review, 1, 0).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    let attempt = &claimed["data"]["attempt"];
    let (status,done)=f.call(&f.b,"POST",&format!("/api/v1/projects/{p}/workflow-activities/{}/review",review["id"].as_str().unwrap()),json!({"generation":attempt["generation"],"submission_id":review["submission_id"],"decision":"approved","summary":"acceptance evidence is sufficient","findings":[]})).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(done["data"]["work_status"], "done");
    let lifecycle: String = sqlx::query_scalar("SELECT lifecycle FROM tasks WHERE id=?")
        .bind(t["id"].as_str().unwrap())
        .fetch_one(&f.state.pool)
        .await
        .unwrap();
    assert_eq!(lifecycle, "done");
}

const BASE: &str = "1111111111111111111111111111111111111111";

#[tokio::test]
async fn submission_lessons_and_artifact_references_commit_together_or_not_at_all() {
    let f = Fixture::new().await;
    let p = f
        .project("shared-submission", "https://example.test/shared.git")
        .await;
    let t = f.task(&p, "general", "Preserve useful evidence").await;
    let owner = f.claim(&f.a, &p, &t, 1).await;
    let path = format!(
        "/api/v1/projects/{p}/attempts/{}/submissions",
        owner["id"].as_str().unwrap()
    );
    let mut body = json!({"generation":owner["generation"],"task_revision":t["revision"],
        "project_policy_revision":1,"workflow_policy_revision":0,"kind":"general",
        "summary":"candidate with durable knowledge","acceptance_evidence":[{"criterion":"required behavior verified","evidence":"observed directly"}],
        "handoff":"Continue from the attached evidence",
        "lessons":[{"kind":"lesson","title":"Keep producer identity","body":"A missing observer is not a failed producer.","status":"observed","scope":{},"provenance_summary":"Learned while validating this task"}],
        "artifact_ids":["missing-artifact"]});
    let (status, _) = f.call(&f.a, "POST", &path, body.clone()).await;
    assert!(status.is_client_error());
    for query in [
        "SELECT count(*) FROM submissions",
        "SELECT count(*) FROM knowledge_records",
    ] {
        let count: i64 = sqlx::query_scalar(query)
            .fetch_one(&f.state.pool)
            .await
            .unwrap();
        assert_eq!(count, 0, "failed submission left records: {query}");
    }
    let active: String = sqlx::query_scalar("SELECT state FROM attempts WHERE id=?")
        .bind(owner["id"].as_str().unwrap())
        .fetch_one(&f.state.pool)
        .await
        .unwrap();
    assert_eq!(active, "active");
    let (status, artifact) = f.call(&f.a, "POST", &format!("/api/v1/projects/{p}/artifacts"),
        json!({"display_name":"Producer report","media_type":"text/plain","external_url":"https://example.test/report","task_id":t["id"]})).await;
    assert_eq!(status, StatusCode::OK, "{artifact}");
    let artifact_id = artifact
        .pointer("/data/id")
        .or_else(|| artifact.pointer("/data/artifact/id"))
        .unwrap()
        .clone();
    body["artifact_ids"] = json!([artifact_id]);
    let key = "atomic-shared-submission";
    let (status, submitted) = call(f.app.clone(), &f.a, "POST", &path, key, body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{submitted}");
    let (status, replay) = call(f.app.clone(), &f.a, "POST", &path, key, body).await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(submitted["data"], replay["data"]);
    let knowledge: i64 = sqlx::query_scalar("SELECT count(*) FROM knowledge_records")
        .fetch_one(&f.state.pool)
        .await
        .unwrap();
    let links: i64 = sqlx::query_scalar("SELECT count(*) FROM submission_artifacts")
        .fetch_one(&f.state.pool)
        .await
        .unwrap();
    assert_eq!((knowledge, links), (1, 1));
    assert_eq!(
        submitted["data"]["submission"]["artifact_ids"],
        json!([artifact_id])
    );
    assert_eq!(
        submitted["data"]["submission"]["lesson_ids"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

const BASE_TREE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[tokio::test]
async fn a_pending_subject_decision_blocks_review_completion_but_allows_saving_work() {
    let f = Fixture::new().await;
    let p = f
        .project("scoped-review", "https://example.test/decision.git")
        .await;
    let t = f.task(&p, "general", "Scoped deliverable").await;
    let owner = f.claim(&f.a, &p, &t, 1).await;
    let submitted = f
        .submit(&f.a, &p, &t, &owner, "general", 1, None, None, None, None)
        .await;
    let review = activity(&submitted, "agent_review");
    let (status, claimed) = f.claim_activity(&f.b, &p, review, 1, 0).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    let attempt = &claimed["data"]["attempt"];
    let (status, decision) = f.call(&f.a, "POST", &format!("/api/v1/projects/{p}/decisions"), json!({
        "question":"May this deliverable proceed?","options":["Proceed","Wait"],"rationale":"Confirm the environment before completion",
        "required_actor":"human","affected_tasks":[{"task_id":t["id"],"task_revision":t["revision"]}],
        "policy_revision":1,"environment":"test","conditions":"Operator checked the target","expires_at":null
    })).await;
    assert_eq!(status, StatusCode::OK, "{decision}");
    let review_path = format!(
        "/api/v1/projects/{p}/workflow-activities/{}/review",
        review["id"].as_str().unwrap()
    );
    let review_body = json!({"generation":attempt["generation"],"submission_id":review["submission_id"],"decision":"approved","summary":"Evidence checked","findings":[]});
    let (status, blocked) = f
        .call(&f.b, "POST", &review_path, review_body.clone())
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{blocked}");
    assert_eq!(blocked["error"]["code"], "decision_required");
    let (status, checkpoint) = f.call(&f.b, "POST", &format!("/api/v1/projects/{p}/attempts/{}/checkpoints", attempt["id"].as_str().unwrap()), json!({"generation":attempt["generation"],"summary":"Waiting for operator decision","current_action":"Saving review findings","next_step":"Resume after scoped approval","blockers":["Pending decision"]})).await;
    assert_eq!(status, StatusCode::OK, "{checkpoint}");
    let answer_path = format!(
        "/api/v1/projects/{p}/decisions/{}/answer",
        decision["data"]["id"].as_str().unwrap()
    );
    let answer = json!({"expected_generation":1,"disposition":"allow","answer":"Proceed","rationale":"Target inspected","conditions_confirmed":true});
    let (status, _) = f.call(&f.b, "POST", &answer_path, answer.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, answered) = f.call(&f.admin, "POST", &answer_path, answer).await;
    assert_eq!(status, StatusCode::OK, "{answered}");
    let (status, done) = f.call(&f.b, "POST", &review_path, review_body).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(done["data"]["work_status"], "done");
}

const CANDIDATE: &str = "2222222222222222222222222222222222222222";
const TREE: &str = "3333333333333333333333333333333333333333";
const RESULT: &str = "4444444444444444444444444444444444444444";
const RESULT_TREE: &str = "5555555555555555555555555555555555555555";

async fn code_integration(f: &Fixture, name: &str, canonical: &str) -> (String, Value, Value) {
    let repo = format!("https://example.test/{name}.git");
    let p = f.project(name, &repo).await;
    f.policy_none(&p).await;
    f.workflow_policy(&p, canonical).await;
    let t = f.task(&p, "code", "Implement exact workflow").await;
    let owner = f.claim(&f.a, &p, &t, 2).await;
    f.checkout(&f.a, &p, &owner, BASE).await;
    let submitted = f
        .submit(
            &f.a,
            &p,
            &t,
            &owner,
            "code",
            2,
            Some(&repo),
            Some(BASE),
            Some(CANDIDATE),
            Some(TREE),
        )
        .await;
    (p, t, submitted)
}

#[tokio::test]
async fn exact_success_job_enables_publish_and_atomic_completion() {
    let f = Fixture::new().await;
    let (p, t, submitted) = code_integration(&f, "code-success", "code-success").await;
    let integration = activity(&submitted, "integration").clone();
    let (status, claimed) = f.claim_activity(&f.c, &p, &integration, 2, 1).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    let attempt = claimed["data"]["attempt"].clone();
    f.checkout(&f.c, &p, &attempt, CANDIDATE).await;
    let (status,intent)=f.call(&f.c,"POST",&format!("/api/v1/projects/{p}/workflow-activities/{}/publication-intent",integration["id"].as_str().unwrap()),json!({"generation":attempt["generation"],"submission_id":integration["submission_id"],"observed_target_revision":BASE,"observed_target_tree":BASE_TREE,"result_revision":RESULT,"result_tree":RESULT_TREE})).await;
    assert_eq!(status, StatusCode::OK, "{intent}");
    let reservation = Uuid::new_v4().to_string();
    let job = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO reservations(id,project_id,attempt_id,generation,state,created_by,created_at,released_at,released_by,release_reason) VALUES(?,?,?,?,'released',?,?,?,?,'test receipt retained')")
        .bind(&reservation).bind(&p).bind(attempt["id"].as_str().unwrap()).bind(attempt["generation"].as_i64().unwrap()).bind(&f.c.principal).bind(f.state.now()).bind(f.state.now()).bind(&f.c.principal).execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO jobs(id,producer_id,project_id,task_id,attempt_id,generation,runner_instance_id,workstation_id,label,source_revision,source_tree,reservation_id,state,last_sequence,last_observed_at,exit_code,inputs_unchanged,summary,created_at,check_identity,check_version,check_environment) VALUES(?,?,?,?,?,?,?,?,?,?,?,?, 'succeeded',1,?,0,1,'passed',?,?,?,?)")
        .bind(&job).bind(Uuid::new_v4().to_string()).bind(&p).bind(integration["activity_task_id"].as_str().unwrap()).bind(attempt["id"].as_str().unwrap()).bind(attempt["generation"].as_i64().unwrap()).bind(Uuid::new_v4().to_string()).bind(format!("{}-workstation",f.c.principal)).bind("workspace tests").bind(RESULT).bind(RESULT_TREE).bind(&reservation).bind(f.state.now()).bind(f.state.now()).bind("workspace-tests").bind("v1").bind("linux-ci").execute(&f.state.pool).await.unwrap();
    let (status, detail) = f
        .call(
            &f.c,
            "GET",
            &format!(
                "/api/v1/projects/{p}/workflow-activities/{}",
                integration["id"].as_str().unwrap()
            ),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["data"]["publication_allowed"], true);
    let (status,result)=f.call(&f.c,"POST",&format!("/api/v1/projects/{p}/workflow-activities/{}/integration-result",integration["id"].as_str().unwrap()),json!({"generation":attempt["generation"],"submission_id":integration["submission_id"],"publication_state":"published","observed_target_revision":BASE,"result_revision":RESULT,"result_tree":RESULT_TREE,"check_job_ids":[job],"summary":"published with exact checks"})).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let (_, after) = f
        .call(
            &f.c,
            "GET",
            &format!(
                "/api/v1/projects/{p}/workflow-activities/{}",
                integration["id"].as_str().unwrap()
            ),
            json!({}),
        )
        .await;
    assert_eq!(after["data"]["publication_allowed"], false);
    let (status,done)=f.call(&f.c,"POST",&format!("/api/v1/projects/{p}/workflow-activities/{}/finalize",integration["id"].as_str().unwrap()),json!({"generation":attempt["generation"],"submission_id":integration["submission_id"],"observed_target_revision":RESULT,"observed_target_tree":RESULT_TREE})).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(done["data"]["work_status"], "done");
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT lifecycle FROM tasks WHERE id=?")
            .bind(t["id"].as_str().unwrap())
            .fetch_one(&f.state.pool)
            .await
            .unwrap(),
        "done"
    );
}

#[tokio::test]
async fn intent_only_crash_retains_hold_until_human_reconciliation_creates_replacement() {
    let f = Fixture::new().await;
    let (p, _t, submitted) = code_integration(&f, "intent-crash", "intent-crash").await;
    let integration = activity(&submitted, "integration").clone();
    let (_, claimed) = f.claim_activity(&f.c, &p, &integration, 2, 1).await;
    let attempt = claimed["data"]["attempt"].clone();
    f.checkout(&f.c, &p, &attempt, CANDIDATE).await;
    let (status,v)=f.call(&f.c,"POST",&format!("/api/v1/projects/{p}/workflow-activities/{}/publication-intent",integration["id"].as_str().unwrap()),json!({"generation":attempt["generation"],"submission_id":integration["submission_id"],"observed_target_revision":BASE,"observed_target_tree":BASE_TREE,"result_revision":RESULT,"result_tree":RESULT_TREE})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    f.clock.0.fetch_add(600_000, Ordering::SeqCst);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM integration_holds WHERE activity_id=?")
            .bind(integration["id"].as_str().unwrap())
            .fetch_one(&f.state.pool)
            .await
            .unwrap(),
        "held"
    );
    let (status,reconciled)=f.call(&f.admin,"POST",&format!("/api/v1/projects/{p}/workflow-activities/{}/publication-reconciliation",integration["id"].as_str().unwrap()),json!({"submission_id":integration["submission_id"],"disposition":"published","observed_target_revision":RESULT,"observed_target_tree":RESULT_TREE,"evidence":"publisher process stopped; remote target inspected exactly"})).await;
    assert_eq!(status, StatusCode::OK, "{reconciled}");
    let activities = reconciled["data"]["activities"].as_array().unwrap();
    assert_eq!(
        activities
            .iter()
            .filter(|a| a["kind"] == "integration")
            .count(),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM integration_holds WHERE activity_id=?")
            .bind(integration["id"].as_str().unwrap())
            .fetch_one(&f.state.pool)
            .await
            .unwrap(),
        "released"
    );
    let (status,blocked)=f.call(&f.admin,"PUT",&format!("/api/v1/projects/{p}/workflow-policy"),json!({"expected_revision":1,"canonical_repository_key":"intent-crash","required_checks":[{"identity":"workspace-tests","version":"v2","environment":"linux-ci"}]})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{blocked}");
    let replacement = activity(&reconciled["data"], "integration").clone();
    let (status, fresh_claim) = f.claim_activity(&f.c, &p, &replacement, 2, 1).await;
    assert_eq!(status, StatusCode::OK, "{fresh_claim}");
    let fresh = fresh_claim["data"]["attempt"].clone();
    f.checkout(&f.c, &p, &fresh, RESULT).await;
    let (status,v)=f.call(&f.c,"POST",&format!("/api/v1/projects/{p}/workflow-activities/{}/publication-intent",replacement["id"].as_str().unwrap()),json!({"generation":fresh["generation"],"submission_id":replacement["submission_id"],"observed_target_revision":RESULT,"observed_target_tree":RESULT_TREE,"result_revision":RESULT,"result_tree":RESULT_TREE})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let job = f
        .check_job(&p, &replacement, &fresh, RESULT, RESULT_TREE)
        .await;
    let (status,v)=f.call(&f.c,"POST",&format!("/api/v1/projects/{p}/workflow-activities/{}/integration-result",replacement["id"].as_str().unwrap()),json!({"generation":fresh["generation"],"submission_id":replacement["submission_id"],"publication_state":"published","observed_target_revision":RESULT,"result_revision":RESULT,"result_tree":RESULT_TREE,"check_job_ids":[job],"summary":"replacement result revalidated"})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (status,done)=f.call(&f.c,"POST",&format!("/api/v1/projects/{p}/workflow-activities/{}/finalize",replacement["id"].as_str().unwrap()),json!({"generation":fresh["generation"],"submission_id":replacement["submission_id"],"observed_target_revision":RESULT,"observed_target_tree":RESULT_TREE})).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(done["data"]["work_status"], "done");
}

#[tokio::test]
async fn canonical_target_claim_race_grants_one_global_hold() {
    let f = Fixture::new().await;
    let (p1, _t1, s1) = code_integration(&f, "shared-one", "shared-canonical").await;
    let (p2, _t2, s2) = code_integration(&f, "shared-two", "shared-canonical").await;
    let a1 = activity(&s1, "integration").clone();
    let a2 = activity(&s2, "integration").clone();
    f.ack(&f.b, &p1, 2).await;
    f.ack(&f.c, &p2, 2).await;
    let app1 = f.app.clone();
    let app2 = f.app.clone();
    let b = f.b.clone();
    let c = f.c.clone();
    let path1 = format!(
        "/api/v1/projects/{p1}/workflow-activities/{}/claim",
        a1["id"].as_str().unwrap()
    );
    let path2 = format!(
        "/api/v1/projects/{p2}/workflow-activities/{}/claim",
        a2["id"].as_str().unwrap()
    );
    let body1 = json!({"expected_submission_id":a1["submission_id"],"expected_project_policy_revision":2,"expected_workflow_policy_revision":1});
    let body2 = json!({"expected_submission_id":a2["submission_id"],"expected_project_policy_revision":2,"expected_workflow_policy_revision":1});
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let b1 = barrier.clone();
    let b2 = barrier.clone();
    let one = tokio::spawn(async move {
        b1.wait().await;
        call(app1, &b, "POST", &path1, "race-one", body1).await.0
    });
    let two = tokio::spawn(async move {
        b2.wait().await;
        call(app2, &c, "POST", &path2, "race-two", body2).await.0
    });
    let statuses = [one.await.unwrap(), two.await.unwrap()];
    assert_eq!(statuses.iter().filter(|s| **s == StatusCode::OK).count(), 1);
    assert_eq!(
        statuses
            .iter()
            .filter(|s| **s == StatusCode::CONFLICT)
            .count(),
        1
    );
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM integration_holds WHERE canonical_repository_key='shared-canonical' AND target_branch='main' AND state='held'").fetch_one(&f.state.pool).await.unwrap(),1);
}

#[tokio::test]
async fn manual_recovery_rejects_agent_takeover_and_human_reopens_expired_review() {
    let f = Fixture::new().await;
    let p = f
        .project("manual-review", "https://example.test/manual.git")
        .await;
    let (status,v)=f.call(&f.admin,"PATCH",&format!("/api/v1/projects/{p}/policy"),json!({"expected_revision":1,"review_mode":"agent","recovery_mode":"manual","lease_seconds":600,"rules":"","agent_rule_editing":false,"automatic_integration":true})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let t = f.task(&p, "general", "Manual recovery").await;
    let owner = f.claim(&f.a, &p, &t, 2).await;
    let submitted = f
        .submit(&f.a, &p, &t, &owner, "general", 2, None, None, None, None)
        .await;
    let review = activity(&submitted, "agent_review").clone();
    let (status, claimed) = f.claim_activity(&f.b, &p, &review, 2, 0).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    f.clock.0.fetch_add(600_000, Ordering::SeqCst);
    let (status, _) = f.claim_activity(&f.c, &p, &review, 2, 0).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status,reopened)=f.call(&f.admin,"POST",&format!("/api/v1/projects/{p}/tasks/{}/workflow/reopen",t["id"].as_str().unwrap()),json!({"submission_id":review["submission_id"],"reason":"expired reviewer inspected; no jobs or holds remain"})).await;
    assert_eq!(status, StatusCode::OK, "{reopened}");
    assert_eq!(reopened["data"]["work_status"], "ready");
}

#[tokio::test]
async fn changes_requested_revokes_a_concurrent_review_without_losing_history() {
    let f = Fixture::new().await;
    let p = f
        .project("both-review", "https://example.test/both.git")
        .await;
    let (status,v)=f.call(&f.admin,"PATCH",&format!("/api/v1/projects/{p}/policy"),json!({"expected_revision":1,"review_mode":"both","recovery_mode":"agent","lease_seconds":600,"rules":"","agent_rule_editing":false,"automatic_integration":true})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let t = f.task(&p, "general", "Both reviews").await;
    let owner = f.claim(&f.a, &p, &t, 2).await;
    let submitted = f
        .submit(&f.a, &p, &t, &owner, "general", 2, None, None, None, None)
        .await;
    let agent = activity(&submitted, "agent_review").clone();
    let human_slot = activity(&submitted, "human_review").clone();
    let (_, agent_claim) = f.claim_activity(&f.b, &p, &agent, 2, 0).await;
    let (status,human_claim)=f.call(&f.admin,"POST",&format!("/api/v1/projects/{p}/workflow-activities/{}/claim",human_slot["id"].as_str().unwrap()),json!({"expected_submission_id":human_slot["submission_id"],"expected_project_policy_revision":2,"expected_workflow_policy_revision":0})).await;
    assert_eq!(status, StatusCode::OK, "{human_claim}");
    let (status,changed)=f.call(&f.b,"POST",&format!("/api/v1/projects/{p}/workflow-activities/{}/review",agent["id"].as_str().unwrap()),json!({"generation":agent_claim["data"]["attempt"]["generation"],"submission_id":agent["submission_id"],"decision":"changes_requested","summary":"revision required","findings":[{"severity":"required","remedy":"fix the identified issue","evidence":"reviewed exact submission"}]})).await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    assert_eq!(changed["data"]["work_status"], "ready");
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM attempts WHERE id=?")
            .bind(human_claim["data"]["attempt"]["id"].as_str().unwrap())
            .fetch_one(&f.state.pool)
            .await
            .unwrap(),
        "canceled"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM review_findings WHERE activity_id=?")
            .bind(agent["id"].as_str().unwrap())
            .fetch_one(&f.state.pool)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn next_selection_skips_decision_blocked_work() {
    let f = Fixture::new().await;
    let p = f
        .project("decision-selection", "https://example.test/selection.git")
        .await;
    let blocked = f.task(&p, "general", "Needs an answer").await;
    let ready = f.task(&p, "general", "May proceed").await;
    let (status, decision) = f
        .call(
            &f.a,
            "POST",
            &format!("/api/v1/projects/{p}/decisions"),
            json!({
                "question":"May the protected task proceed?", "options":["Proceed","Wait"],
                "rationale":"An operator must inspect the environment", "required_actor":"human",
                "affected_tasks":[{"task_id":blocked["id"],"task_revision":blocked["revision"]}],
                "policy_revision":1,"environment":"test","conditions":"Environment inspected"
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{decision}");
    f.ack(&f.a, &p, 1).await;
    let (status, orientation) = f
        .call(
            &f.a,
            "GET",
            &format!("/api/v1/projects/{p}/orientation"),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{orientation}");
    let candidates = orientation["data"]["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0]["id"], ready["id"]);
    let mut input = json!({"task_id":blocked["id"],"expected_task_revision":blocked["revision"],"mode":"work","policy_revision":1,"instruction_version":coordinator_core::INSTRUCTION_VERSION});
    let (status, refusal) = f
        .call(
            &f.a,
            "POST",
            &format!("/api/v1/projects/{p}/claims"),
            input.clone(),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refusal}");
    assert_eq!(refusal["error"]["code"], "decision_required");
    input.as_object_mut().unwrap().remove("task_id");
    input
        .as_object_mut()
        .unwrap()
        .remove("expected_task_revision");
    let (status, claimed) = f
        .call(&f.a, "POST", &format!("/api/v1/projects/{p}/claims"), input)
        .await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    assert_eq!(claimed["data"]["claim"]["attempt"]["task_id"], ready["id"]);
}
