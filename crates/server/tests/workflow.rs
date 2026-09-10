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
        let (s,v)=self.call(c,"POST",&format!("/api/v1/sessions/{}/instruction-acknowledgments",c.session),json!({"project_id":p,"policy_revision":policy,"instruction_version":"3","sections":["coordination-v3"]})).await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }
    async fn claim(&self, c: &Caller, p: &str, t: &Value, policy: i64) -> Value {
        self.ack(c, p, policy).await;
        let (s,v)=self.call(c,"POST",&format!("/api/v1/projects/{p}/claims"),json!({"task_id":t["id"],"expected_task_revision":t["revision"],"mode":"work","policy_revision":policy,"instruction_version":"3"})).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["data"]["claim"]["attempt"].clone()
    }
    async fn checkout(&self, c: &Caller, p: &str, attempt: &Value, base: &str) {
        let (s,v)=self.call(c,"POST",&format!("/api/v1/projects/{p}/attempts/{}/checkout",attempt["id"].as_str().unwrap()),json!({"generation":attempt["generation"],"workstation_id":format!("{0}-workstation",c.principal),"identity":Uuid::new_v4().to_string(),"path":"/tmp/workflow-test","branch":"workflow-test","base_revision":base,"clean":true})).await;
        assert_eq!(s, StatusCode::OK, "{v}");
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
const BASE_TREE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
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
}
