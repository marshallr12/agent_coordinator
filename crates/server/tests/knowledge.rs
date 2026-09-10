use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use coordinator_core::{KnowledgeScope, SubmissionLessonInput};
use coordinator_server::{
    auth::{Actor, digest, secret},
    knowledge::{ensure_decisions_resolved, insert_submission_lessons},
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
    admin: Caller,
    agent: Caller,
    other: Caller,
    _dir: tempfile::TempDir,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::open(Config {
            database_path: dir.path().join("knowledge.sqlite3"),
            public_origin: "http://127.0.0.1:8080".into(),
            allow_insecure_loopback: true,
            ..Config::default()
        })
        .await
        .unwrap();
        state.clock = Arc::new(TestClock(AtomicI64::new(1_800_000_000_000)));
        let admin = seed(&state, true, "knowledge-admin").await;
        let agent = seed(&state, false, "knowledge-agent").await;
        let other = seed(&state, false, "knowledge-other").await;
        Self {
            app: router(state.clone()),
            state,
            admin,
            agent,
            other,
            _dir: dir,
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
                &self.agent,
                "POST",
                &format!("/api/v1/projects/{project}/tasks"),
                json!({"title":title,"description":"knowledge route test","acceptance_criteria":["evidence recorded"],"kind":"general"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        value["data"].clone()
    }

    async fn acknowledge(&self, project: &str, policy_revision: i64) {
        let (status, value) = self
            .call(
                &self.agent,
                "POST",
                &format!(
                    "/api/v1/sessions/{}/instruction-acknowledgments",
                    self.agent.session
                ),
                json!({"project_id":project,"policy_revision":policy_revision,"instruction_version":"5","sections":["coordination-v5"]}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
    }
}

#[tokio::test]
async fn revisioned_shared_knowledge_search_feedback_and_history_are_bounded() {
    let fixture = Fixture::new().await;
    let source = fixture.project("knowledge-source").await;
    let consumer = fixture.project("knowledge-consumer").await;
    let task = fixture.task(&source, "Index quartzneedle behavior").await;
    let body = json!({
        "kind":"lesson","title":"Quartzneedle cache behavior","body":"Use the quartzneedle cache only after validation.",
        "status":"observed","scope":{"task_ids":[task["id"]],"components":["cache"],"environments":["linux"],"versions":["v2"]},
        "tags":["search","cache"],"applicability":"The v2 Linux cache path.",
        "provenance":{"summary":"Observed in a bounded route test.","source_uri":"https://example.test/evidence","source_task_id":task["id"],"source_submission_id":null},
        "collection":"shared","share_across_projects":true
    });
    let (status, created) = fixture
        .call(
            &fixture.agent,
            "POST",
            &format!("/api/v1/projects/{source}/knowledge"),
            body,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let id = created["data"]["id"].as_str().unwrap();

    let (status, private_list) = fixture
        .call(
            &fixture.other,
            "GET",
            &format!("/api/v1/projects/{consumer}/knowledge"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{private_list}");
    assert!(private_list["data"]["items"].as_array().unwrap().is_empty());
    let (status, shared_list) = fixture
        .call(
            &fixture.other,
            "GET",
            &format!("/api/v1/projects/{consumer}/knowledge?include_shared=true&limit=1"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{shared_list}");
    assert_eq!(shared_list["data"]["items"][0]["id"], id);

    let (status, context) = fixture
        .call(
            &fixture.other,
            "GET",
            &format!("/api/v1/projects/{consumer}/context?q=quartzneedle&include_shared=true&component=cache&environment=linux&version=v2&limit=10&budget=8192"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{context}");
    assert!(
        context["data"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["record"]["id"] == id)
    );

    let edit = json!({
        "expected_revision":1,"title":"Quartzneedle cache behavior","body":"Use the quartzneedle cache only after validation and record the digest.",
        "status":"validated","scope":{"task_ids":[task["id"]],"components":["cache"],"environments":["linux"],"versions":["v2"]},
        "tags":["search","cache"],"applicability":"The v2 Linux cache path.",
        "provenance":{"summary":"Validated by the route test.","source_uri":"https://example.test/evidence","source_task_id":task["id"],"source_submission_id":null},
        "superseded_by_id":null
    });
    let (status, revised) = fixture
        .call(
            &fixture.agent,
            "PATCH",
            &format!("/api/v1/projects/{source}/knowledge/{id}"),
            edit.clone(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{revised}");
    assert_eq!(revised["data"]["revision"], 2);
    let (status, stale_feedback) = fixture
        .call(
            &fixture.other,
            "POST",
            &format!("/api/v1/projects/{source}/knowledge/{id}/feedback"),
            json!({"expected_revision":1,"useful":true,"comment":"Based on the revision I read."}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale_feedback}");
    assert_eq!(stale_feedback["error"]["code"], "revision_conflict");
    let (status, stale) = fixture
        .call(
            &fixture.agent,
            "PATCH",
            &format!("/api/v1/projects/{source}/knowledge/{id}"),
            edit,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    assert_eq!(stale["error"]["code"], "revision_conflict");

    let (status, feedback) = fixture
        .call(
            &fixture.other,
            "POST",
            &format!("/api/v1/projects/{source}/knowledge/{id}/feedback"),
            json!({"expected_revision":2,"useful":true,"comment":"Applied to the cache diagnosis."}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{feedback}");
    assert_eq!(feedback["data"]["revision"], 2);
    let (status, detail) = fixture
        .call(
            &fixture.other,
            "GET",
            &format!("/api/v1/projects/{source}/knowledge/{id}?limit=1"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["data"]["revisions"].as_array().unwrap().len(), 1);
    assert_eq!(detail["data"]["revisions_next_cursor"], 2);
    assert_eq!(detail["data"]["feedback_summary"]["useful"], 1);

    let rules = "mandatory-rule ".repeat(120);
    let (status, policy) = fixture
        .call(
            &fixture.admin,
            "PATCH",
            &format!("/api/v1/projects/{consumer}/policy"),
            json!({"expected_revision":1,"review_mode":"agent","recovery_mode":"agent","lease_seconds":600,"rules":rules,"agent_rule_editing":false,"automatic_integration":true}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{policy}");
    let (status, small_context) = fixture
        .call(
            &fixture.other,
            "GET",
            &format!("/api/v1/projects/{consumer}/context?q=quartzneedle&budget=1024"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{small_context}");
    assert_eq!(small_context["data"]["policy"]["rules"], rules);
    assert_eq!(small_context["data"]["instructions_complete"], false);
    assert_eq!(small_context["data"]["truncated"], true);
    let (status, history) = fixture
        .call(
            &fixture.other,
            "GET",
            &format!("/api/v1/projects/{consumer}/policy/history?limit=1"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{history}");
    assert_eq!(history["data"]["items"][0]["revision"], 2);
    let immutable = sqlx::query(
        "UPDATE policy_revisions SET provenance='rewritten' WHERE project_id=? AND revision=1",
    )
    .bind(&consumer)
    .execute(&fixture.state.pool)
    .await;
    assert!(immutable.is_err());
}

#[tokio::test]
async fn decisions_require_typed_current_allow_and_preserve_reopen_history() {
    let fixture = Fixture::new().await;
    let project = fixture.project("decision-scope").await;
    let task = fixture
        .task(&project, "Wait for the production choice")
        .await;
    let companion = fixture
        .task(&project, "Keep the rollback owner current")
        .await;
    let path = format!("/api/v1/projects/{project}/decisions");
    let (status, created) = fixture
        .call(
            &fixture.agent,
            "POST",
            &path,
            json!({"question":"May these exact tasks proceed?","options":["Proceed","Stop"],"rationale":"Production ownership requires an explicit choice.","required_actor":"human","affected_tasks":[{"task_id":task["id"],"task_revision":1},{"task_id":companion["id"],"task_revision":1}],"policy_revision":1,"environment":"production","conditions":"The rollback owner is present.","expires_at":1800000060000_i64}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let decision = created["data"]["id"].as_str().unwrap();
    let mut connection = fixture.state.pool.acquire().await.unwrap();
    let error = ensure_decisions_resolved(
        &mut connection,
        &project,
        task["id"].as_str().unwrap(),
        fixture.state.now(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "decision_required");
    drop(connection);

    let answer_path = format!("/api/v1/projects/{project}/decisions/{decision}/answer");
    let (status, rejected) = fixture
        .call(
            &fixture.agent,
            "POST",
            &answer_path,
            json!({"expected_generation":1,"disposition":"allow","answer":"Proceed","rationale":"Agent assertion.","conditions_confirmed":true}),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{rejected}");
    let (status, denied) = fixture
        .call(
            &fixture.admin,
            "POST",
            &answer_path,
            json!({"expected_generation":1,"disposition":"deny","answer":"Stop","rationale":"Rollback owner is absent.","conditions_confirmed":false}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{denied}");
    assert_eq!(denied["data"]["status"], "denied");
    let mut connection = fixture.state.pool.acquire().await.unwrap();
    assert!(
        ensure_decisions_resolved(
            &mut connection,
            &project,
            task["id"].as_str().unwrap(),
            fixture.state.now()
        )
        .await
        .is_err()
    );
    drop(connection);

    let reopen_path = format!("/api/v1/projects/{project}/decisions/{decision}/reopen");
    let (status, reopened) = fixture
        .call(
            &fixture.agent,
            "POST",
            &reopen_path,
            json!({"expected_generation":1,"rationale":"Rollback owner is now assigned.","affected_tasks":[{"task_id":task["id"],"task_revision":1},{"task_id":companion["id"],"task_revision":1}],"policy_revision":1,"environment":"production","conditions":"The rollback owner is present.","expires_at":1800000120000_i64}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{reopened}");
    assert_eq!(reopened["data"]["generation"], 2);
    assert_eq!(reopened["data"]["required_actor"], "human");
    let (status, missing_confirmation) = fixture
        .call(
            &fixture.admin,
            "POST",
            &answer_path,
            json!({"expected_generation":2,"disposition":"allow","answer":"Proceed","rationale":"Ready.","conditions_confirmed":false}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{missing_confirmation}");
    let (status, allowed) = fixture
        .call(
            &fixture.admin,
            "POST",
            &answer_path,
            json!({"expected_generation":2,"disposition":"allow","answer":"Proceed","rationale":"Rollback owner confirmed.","conditions_confirmed":true}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{allowed}");
    assert_eq!(allowed["data"]["status"], "allowed");
    assert_eq!(allowed["data"]["history"].as_array().unwrap().len(), 2);
    let mut connection = fixture.state.pool.acquire().await.unwrap();
    ensure_decisions_resolved(
        &mut connection,
        &project,
        task["id"].as_str().unwrap(),
        fixture.state.now(),
    )
    .await
    .unwrap();
    drop(connection);

    let (status, edited) = fixture
        .call(
            &fixture.agent,
            "PATCH",
            &format!("/api/v1/projects/{project}/tasks/{}",companion["id"].as_str().unwrap()),
            json!({"expected_revision":1,"title":"Keep the revised rollback owner current","description":"knowledge route test","acceptance_criteria":["evidence recorded"],"priority":2,"depends_on":[],"planned":false}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{edited}");
    let mut connection = fixture.state.pool.acquire().await.unwrap();
    let stale = ensure_decisions_resolved(
        &mut connection,
        &project,
        task["id"].as_str().unwrap(),
        fixture.state.now(),
    )
    .await
    .unwrap_err();
    assert_eq!(stale.code, "decision_required");
}

#[tokio::test]
async fn context_marks_pending_decisions_truncated_at_the_item_limit() {
    let fixture = Fixture::new().await;
    let project = fixture.project("decision-context-limit").await;
    let task = fixture
        .task(&project, "Bound decisionneedle pending context")
        .await;
    let mut decisions = Vec::new();
    for question in ["First unresolved choice?", "Second unresolved choice?"] {
        let (status, decision) = fixture
            .call(
                &fixture.agent,
                "POST",
                &format!("/api/v1/projects/{project}/decisions"),
                json!({"question":question,"options":["Proceed","Wait"],
                    "rationale":"The bounded context must disclose truncation.",
                    "required_actor":"human","affected_tasks":[{"task_id":task["id"],"task_revision":1}],
                    "policy_revision":1,"environment":"test","conditions":"Operator confirmation required.",
                    "expires_at":null}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{decision}");
        decisions.push(decision["data"].clone());
    }

    let (status, context) = fixture
        .call(
            &fixture.agent,
            "GET",
            &format!(
                "/api/v1/projects/{project}/context?q=decisionneedle&task_id={}&limit=1&budget=8192",
                task["id"].as_str().unwrap()
            ),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{context}");
    assert_eq!(context["data"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(context["data"]["items"][0]["type"], "decision");
    assert_eq!(context["data"]["truncated"], true, "{context}");

    let (status, allowed) = fixture
        .call(
            &fixture.admin,
            "POST",
            &format!(
                "/api/v1/projects/{project}/decisions/{}/answer",
                decisions[0]["id"].as_str().unwrap()
            ),
            json!({"expected_generation":1,"disposition":"allow","answer":"Proceed",
                "rationale":"Exact scope checked.","conditions_confirmed":true}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{allowed}");
    let (status, exact_capacity) = fixture
        .call(
            &fixture.agent,
            "GET",
            &format!(
                "/api/v1/projects/{project}/context?q=decisionneedle&task_id={}&limit=1&budget=8192",
                task["id"].as_str().unwrap()
            ),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{exact_capacity}");
    assert_eq!(
        exact_capacity["data"]["items"][0]["type"], "decision",
        "{exact_capacity}"
    );
    assert_eq!(
        exact_capacity["data"]["truncated"], true,
        "{exact_capacity}"
    );
}

#[tokio::test]
async fn submission_lesson_helper_validates_the_batch_before_inserting() {
    let fixture = Fixture::new().await;
    let project = fixture.project("submission-lessons").await;
    let task = fixture.task(&project, "Submit atomic lessons").await;
    fixture.acknowledge(&project, 1).await;
    let (status, claim) = fixture
        .call(
            &fixture.agent,
            "POST",
            &format!("/api/v1/projects/{project}/claims"),
            json!({"task_id":task["id"],"expected_task_revision":1,"mode":"work","policy_revision":1,"instruction_version":"5"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{claim}");
    let attempt = &claim["data"]["claim"]["attempt"];
    let (status, submitted) = fixture
        .call(
            &fixture.agent,
            "POST",
            &format!("/api/v1/projects/{project}/attempts/{}/submissions",attempt["id"].as_str().unwrap()),
            json!({"generation":attempt["generation"],"task_revision":1,"project_policy_revision":1,"workflow_policy_revision":0,"kind":"general","summary":"Lessons ready","acceptance_evidence":[{"criterion":"evidence recorded","evidence":"helper test"}],"handoff":"Review the exact lesson records.","repository":null,"base_revision":null,"candidate_revision":null,"candidate_tree":null}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{submitted}");
    let submission = submitted["data"]["submission"]["id"].as_str().unwrap();
    let actor = Actor {
        id: fixture.agent.principal.clone(),
        name: "knowledge-agent".into(),
        kind: "agent".into(),
        role: "agent".into(),
        credential_id: Some(fixture.agent.credential.clone()),
        session_id: Some(fixture.agent.session.clone()),
    };
    let lesson = |title: &str, task_ids: Vec<String>| SubmissionLessonInput {
        kind: "lesson".into(),
        title: title.into(),
        body: "A complete immutable submission lesson.".into(),
        status: "observed".into(),
        scope: KnowledgeScope {
            task_ids,
            components: vec!["submission".into()],
            environments: vec![],
            versions: vec![],
        },
        tags: vec!["atomic".into()],
        applicability: "Submission processing.".into(),
        collection: "project".into(),
        share_across_projects: false,
        provenance_summary: "Observed while submitting.".into(),
        source_uri: None,
    };
    let valid = lesson(
        "First valid lesson",
        vec![task["id"].as_str().unwrap().into()],
    );
    let invalid = lesson("Invalid lesson", vec![Uuid::new_v4().to_string()]);
    let mut tx = fixture
        .state
        .pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .unwrap();
    let result = insert_submission_lessons(
        &mut tx,
        &actor,
        fixture.state.now(),
        &project,
        task["id"].as_str().unwrap(),
        submission,
        &[valid.clone(), invalid],
    )
    .await;
    assert!(result.is_err());
    tx.rollback().await.unwrap();
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM knowledge_records WHERE source_project_id=?")
            .bind(&project)
            .fetch_one(&fixture.state.pool)
            .await
            .unwrap();
    assert_eq!(count, 0);

    let mut tx = fixture
        .state
        .pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .unwrap();
    let inserted = insert_submission_lessons(
        &mut tx,
        &actor,
        fixture.state.now(),
        &project,
        task["id"].as_str().unwrap(),
        submission,
        &[valid],
    )
    .await
    .unwrap();
    assert_eq!(inserted.len(), 1);
    tx.commit().await.unwrap();
    let provenance = &inserted[0]["provenance"];
    assert_eq!(provenance["source_submission_id"], submission);
    assert_eq!(provenance["source_task_id"], task["id"]);
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
            .bind(&caller.session).bind(&caller.principal).bind(&caller.credential).bind(format!("{}-workstation",caller.principal)).bind(digest(&caller.proof)).bind(state.now()).execute(&state.pool).await.unwrap();
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
