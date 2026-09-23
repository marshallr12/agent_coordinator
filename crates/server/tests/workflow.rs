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
use sha2::{Digest, Sha256};
use sqlx::{Connection, Row};
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
        let candidate_remote: Option<String> = if kind == "code" {
            sqlx::query_scalar(
                "SELECT canonical_repository_key FROM workflow_policies WHERE project_id=?",
            )
            .bind(p)
            .fetch_one(&self.state.pool)
            .await
            .unwrap()
        } else {
            None
        };
        let candidate_ref = (kind == "code")
            .then(|| format!("refs/agent-coordinator/candidates/{}", Uuid::new_v4()));
        let (s,v)=self.call(c,"POST",&format!("/api/v1/projects/{p}/attempts/{}/submissions",a["id"].as_str().unwrap()),json!({"generation":a["generation"],"task_revision":t["revision"],"project_policy_revision":policy,"workflow_policy_revision":if kind=="code"{1}else{0},"kind":kind,"summary":"candidate ready","acceptance_evidence":[{"criterion":"required behavior verified","evidence":"verified in workflow test"}],"handoff":"review exact evidence","repository":repo,"base_revision":base,"candidate_revision":candidate,"candidate_tree":tree,"candidate_remote":candidate_remote,"candidate_ref":candidate_ref})).await;
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
        if !c.human {
            self.ack(c, p, policy).await;
        }
        self.call(c,"POST",&format!("/api/v1/projects/{p}/workflow-activities/{}/claim",a["id"].as_str().unwrap()),json!({"expected_submission_id":a["submission_id"],"expected_project_policy_revision":policy,"expected_workflow_policy_revision":workflow_policy})).await
    }

    async fn review_policy(&self, p: &str, mode: &str) {
        let (status, result) = self.call(&self.admin, "PATCH", &format!("/api/v1/projects/{p}/policy"), json!({"expected_revision":1,"review_mode":mode,"recovery_mode":"agent","lease_seconds":600,"rules":"","agent_rule_editing":false,"automatic_integration":true})).await;
        assert_eq!(status, StatusCode::OK, "{result}");
        assert_eq!(result["data"]["review_mode"], mode);
    }
}

#[tokio::test]
async fn code_submission_requires_a_candidate_remote_and_durable_ref() {
    let f = Fixture::new().await;
    let repository = "https://example.test/checkpoint-required.git";
    let project = f.project("checkpoint-required", repository).await;
    f.policy_none(&project).await;
    f.workflow_policy(&project, "checkpoint-required").await;
    let task = f.task(&project, "code", "Require checkpoint").await;
    let attempt = f.claim(&f.a, &project, &task, 2).await;
    f.checkout(
        &f.a,
        &project,
        &attempt,
        "1111111111111111111111111111111111111111",
    )
    .await;
    let (status, rejected) = f
        .call(
            &f.a,
            "POST",
            &format!(
                "/api/v1/projects/{project}/attempts/{}/submissions",
                attempt["id"].as_str().unwrap()
            ),
            json!({
                "generation":attempt["generation"],"task_revision":task["revision"],
                "project_policy_revision":2,"workflow_policy_revision":1,"kind":"code",
                "summary":"Uncheckpointed candidate","acceptance_evidence":[{"criterion":"required behavior verified","evidence":"local only"}],
                "handoff":"Must remain unsubmitted","repository":repository,
                "base_revision":"1111111111111111111111111111111111111111",
                "candidate_revision":"2222222222222222222222222222222222222222",
                "candidate_tree":"3333333333333333333333333333333333333333"
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
    assert_eq!(rejected["error"]["code"], "candidate_remote_mismatch");
    let (status, workflow) = f
        .call(
            &f.a,
            "GET",
            &format!("/api/v1/projects/{project}/tasks/{}/workflow", task["id"]),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(workflow["data"]["submission"].is_null());
}

#[tokio::test]
async fn either_review_accepts_each_actor_for_general_and_code_with_exact_receipts() {
    let f = Fixture::new().await;
    for human in [false, true] {
        for kind in ["general", "code"] {
            let repo = format!("https://example.test/either-{human}-{kind}.git");
            let p = f.project(&format!("either-{human}-{kind}"), &repo).await;
            f.review_policy(&p, "either").await;
            if kind == "code" {
                f.workflow_policy(&p, &repo).await;
            }
            let t = f.task(&p, kind, "Either reviewer").await;
            let owner = f.claim(&f.a, &p, &t, 2).await;
            let base = "1111111111111111111111111111111111111111";
            let candidate = "2222222222222222222222222222222222222222";
            let tree = "3333333333333333333333333333333333333333";
            if kind == "code" {
                f.checkout(&f.a, &p, &owner, base).await;
            }
            let submitted = f
                .submit(
                    &f.a,
                    &p,
                    &t,
                    &owner,
                    kind,
                    2,
                    (kind == "code").then_some(repo.as_str()),
                    (kind == "code").then_some(base),
                    (kind == "code").then_some(candidate),
                    (kind == "code").then_some(tree),
                )
                .await;
            assert_eq!(submitted["work_status"], "waiting_review");
            assert_eq!(
                submitted["activities"].as_array().unwrap().len(),
                if kind == "code" { 2 } else { 1 }
            );
            let review = activity(&submitted, "either_review");
            let workflow_revision = i64::from(kind == "code");
            if kind == "code" {
                let (status, _) = f
                    .claim_activity(&f.c, &p, activity(&submitted, "integration"), 2, 1)
                    .await;
                assert_eq!(status, StatusCode::CONFLICT);
            }
            let (status, denied) = f
                .claim_activity(&f.a, &p, review, 2, workflow_revision)
                .await;
            assert_eq!(status, StatusCode::CONFLICT, "{denied}");
            assert_eq!(denied["error"]["code"], "reviewer_not_independent");
            let reviewer = if human { &f.admin } else { &f.b };
            let (status, claimed) = f
                .claim_activity(reviewer, &p, review, 2, workflow_revision)
                .await;
            assert_eq!(status, StatusCode::OK, "{claimed}");
            let path = format!(
                "/api/v1/projects/{p}/workflow-activities/{}/review",
                review["id"].as_str().unwrap()
            );
            let body = json!({"generation":claimed["data"]["attempt"]["generation"],"submission_id":review["submission_id"],"decision":"approved","summary":"Reviewed exact evidence","findings":[]});
            let mut invalid = body.clone();
            invalid["findings"] = json!([{"severity":"required","remedy":"Still needs work","evidence":"Unresolved"}]);
            let (status, _) = f.call(reviewer, "POST", &path, invalid).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            let key = Uuid::new_v4().to_string();
            let (status, approved) =
                call(f.app.clone(), reviewer, "POST", &path, &key, body.clone()).await;
            assert_eq!(status, StatusCode::OK, "{approved}");
            assert_eq!(
                approved["data"]["work_status"],
                if kind == "code" {
                    "waiting_integration"
                } else {
                    "done"
                }
            );
            let (status, replay) = call(f.app.clone(), reviewer, "POST", &path, &key, body).await;
            assert_eq!(status, StatusCode::OK, "{replay}");
            assert_eq!(approved["data"], replay["data"]);
            let (status, _) = f
                .claim_activity(
                    if human { &f.b } else { &f.admin },
                    &p,
                    review,
                    2,
                    workflow_revision,
                )
                .await;
            assert_eq!(status, StatusCode::CONFLICT);
            let (status, history) = f
                .call(
                    &f.a,
                    "GET",
                    &format!(
                        "/api/v1/projects/{p}/tasks/{}/history?kind=reviews",
                        t["id"].as_str().unwrap()
                    ),
                    Value::Null,
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{history}");
            assert!(history.to_string().contains("either_review"));
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT count(*) FROM review_decisions WHERE submission_id=?"
                )
                .bind(review["submission_id"].as_str().unwrap())
                .fetch_one(&f.state.pool)
                .await
                .unwrap(),
                1
            );
            if kind == "code" {
                let (status, result) = f
                    .claim_activity(&f.c, &p, activity(&submitted, "integration"), 2, 1)
                    .await;
                assert_eq!(status, StatusCode::OK, "{result}");
            }
        }
    }
}

#[tokio::test]
async fn either_review_claim_race_has_one_owner_and_changes_cannot_be_overruled() {
    let f = Fixture::new().await;
    let p = f
        .project("either-race", "https://example.test/race.git")
        .await;
    f.review_policy(&p, "either").await;
    let t = f.task(&p, "general", "Concurrent reviewers").await;
    let owner = f.claim(&f.a, &p, &t, 2).await;
    let submitted = f
        .submit(&f.a, &p, &t, &owner, "general", 2, None, None, None, None)
        .await;
    let review = activity(&submitted, "either_review");
    f.ack(&f.b, &p, 2).await;
    let path = format!(
        "/api/v1/projects/{p}/workflow-activities/{}/claim",
        review["id"].as_str().unwrap()
    );
    let body = json!({"expected_submission_id":review["submission_id"],"expected_project_policy_revision":2,"expected_workflow_policy_revision":0});
    let (agent, human) = tokio::join!(
        f.call(&f.b, "POST", &path, body.clone()),
        f.call(&f.admin, "POST", &path, body)
    );
    assert_eq!(
        [agent.0, human.0]
            .iter()
            .filter(|s| **s == StatusCode::OK)
            .count(),
        1
    );
    assert_eq!(
        [agent.0, human.0]
            .iter()
            .filter(|s| **s == StatusCode::CONFLICT)
            .count(),
        1
    );
    let (winner, loser, claimed) = if agent.0 == StatusCode::OK {
        (&f.b, &f.admin, agent.1)
    } else {
        (&f.admin, &f.b, human.1)
    };
    let path = format!(
        "/api/v1/projects/{p}/workflow-activities/{}/review",
        review["id"].as_str().unwrap()
    );
    let body = json!({"generation":claimed["data"]["attempt"]["generation"],"submission_id":review["submission_id"],"decision":"changes_requested","summary":"Fix the issue","findings":[{"severity":"required","remedy":"Correct the behavior","evidence":"Review evidence"}]});
    let (status, _) = f.call(loser, "POST", &path, body.clone()).await;
    assert!(status.is_client_error());
    let (status, result) = f.call(winner, "POST", &path, body).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["data"]["phase"], "revision_needed");
    let (status, _) = f.claim_activity(loser, &p, review, 2, 0).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = f.call(winner, "POST", &path, json!({"generation":claimed["data"]["attempt"]["generation"],"submission_id":review["submission_id"],"decision":"approved","summary":"Cannot replace decision","findings":[]})).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn both_review_still_requires_two_approvals() {
    let f = Fixture::new().await;
    let p = f
        .project("both-approvals", "https://example.test/and.git")
        .await;
    f.review_policy(&p, "both").await;
    let t = f.task(&p, "general", "Two approvals").await;
    let owner = f.claim(&f.a, &p, &t, 2).await;
    let submitted = f
        .submit(&f.a, &p, &t, &owner, "general", 2, None, None, None, None)
        .await;
    for (reviewer, kind, expected) in [
        (&f.b, "agent_review", "waiting_review"),
        (&f.admin, "human_review", "done"),
    ] {
        let review = activity(&submitted, kind);
        let (status, claimed) = f.claim_activity(reviewer, &p, review, 2, 0).await;
        assert_eq!(status, StatusCode::OK, "{claimed}");
        let (status, result) = f.call(reviewer, "POST", &format!("/api/v1/projects/{p}/workflow-activities/{}/review", review["id"].as_str().unwrap()), json!({"generation":claimed["data"]["attempt"]["generation"],"submission_id":review["submission_id"],"decision":"approved","summary":"Reviewed","findings":[]})).await;
        assert_eq!(status, StatusCode::OK, "{result}");
        assert_eq!(result["data"]["work_status"], expected);
    }
}

#[tokio::test]
async fn either_review_rechecks_late_contributions_and_policy_changes() {
    let f = Fixture::new().await;
    let p = f
        .project("either-guards", "https://example.test/guards.git")
        .await;
    f.review_policy(&p, "either").await;
    let t = f.task(&p, "general", "Review guards").await;
    let owner = f.claim(&f.a, &p, &t, 2).await;
    let submitted = f
        .submit(&f.a, &p, &t, &owner, "general", 2, None, None, None, None)
        .await;
    let review = activity(&submitted, "either_review");
    let (status, claimed) = f.claim_activity(&f.b, &p, review, 2, 0).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    sqlx::query("INSERT INTO task_contributors(task_id,principal_id,session_id,first_contributed_at) VALUES(?,?,?,?)").bind(t["id"].as_str().unwrap()).bind(&f.b.principal).bind(&f.b.session).bind(f.state.now()).execute(&f.state.pool).await.unwrap();
    let path = format!(
        "/api/v1/projects/{p}/workflow-activities/{}/review",
        review["id"].as_str().unwrap()
    );
    let body = json!({"generation":claimed["data"]["attempt"]["generation"],"submission_id":review["submission_id"],"decision":"approved","summary":"Must reject","findings":[]});
    let (status, rejected) = f.call(&f.b, "POST", &path, body.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
    assert_eq!(rejected["error"]["code"], "reviewer_not_independent");
    let (status, result) = f.call(&f.admin, "PATCH", &format!("/api/v1/projects/{p}/policy"), json!({"expected_revision":2,"review_mode":"both","recovery_mode":"agent","lease_seconds":600,"rules":"","agent_rule_editing":false,"automatic_integration":true})).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let (status, _) = f.call(&f.b, "POST", &path, body).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = f.claim_activity(&f.admin, &p, review, 3, 0).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM review_decisions")
            .fetch_one(&f.state.pool)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn either_review_migration_preserves_old_reviews_and_foreign_key_enforcement() {
    let f = Fixture::new().await;
    let p = f
        .project("migration-review", "https://example.test/migration.git")
        .await;
    let t = f.task(&p, "general", "Saved review").await;
    let owner = f.claim(&f.a, &p, &t, 1).await;
    let submitted = f
        .submit(&f.a, &p, &t, &owner, "general", 1, None, None, None, None)
        .await;
    let review = activity(&submitted, "agent_review");
    let (status, claimed) = f.claim_activity(&f.b, &p, review, 1, 0).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    let (status, result) = f.call(&f.b, "POST", &format!("/api/v1/projects/{p}/workflow-activities/{}/review", review["id"].as_str().unwrap()), json!({"generation":claimed["data"]["attempt"]["generation"],"submission_id":review["submission_id"],"decision":"approved","summary":"Preserve this decision","findings":[]})).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    sqlx::query("UPDATE projects SET rowid=99 WHERE id=?")
        .bind(&p)
        .execute(&f.state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE workflow_activities SET rowid=77 WHERE id=?")
        .bind(review["id"].as_str().unwrap())
        .execute(&f.state.pool)
        .await
        .unwrap();

    // Build the previous schema from its original, checksummed migrations and
    // populate its unchanged columns with a real saved workflow and decision.
    let migrations = f._dir.path().join("schema17");
    std::fs::create_dir(&migrations).unwrap();
    for migration in sqlx::migrate!("./migrations")
        .iter()
        .filter(|m| m.version <= 17)
    {
        std::fs::write(
            migrations.join(format!(
                "{:04}_{}.sql",
                migration.version,
                migration.description.replace(' ', "_")
            )),
            migration.sql.as_str().as_bytes(),
        )
        .unwrap();
    }
    let database = f._dir.path().join("upgrade.sqlite3");
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
    sqlx::query("ATTACH DATABASE ? AS original")
        .bind(f.state.config.database_path.to_str().unwrap())
        .execute(&mut old)
        .await
        .unwrap();
    for table in [
        "principals",
        "credentials",
        "agent_sessions",
        "browser_sessions",
        "projects",
        "policy_revisions",
        "task_revisions",
        "attempts",
        "task_contributors",
        "submissions",
        "workflow_subjects",
        "workflow_activities",
        "review_decisions",
        "review_findings",
        "instruction_acknowledgments",
    ] {
        // Identifiers come only from the fixed table list above.
        let statement = if table == "submissions" {
            "INSERT INTO main.submissions (
                id,project_id,task_id,attempt_id,kind,task_revision,project_policy_revision,
                workflow_policy_revision,summary,acceptance_evidence_json,handoff,
                canonical_repository_key,repository_url,target_branch,base_revision,
                candidate_revision,candidate_tree,created_by,contributor_session_id,created_at,
                superseded_at
            ) SELECT id,project_id,task_id,attempt_id,kind,task_revision,project_policy_revision,
                workflow_policy_revision,summary,acceptance_evidence_json,handoff,
                canonical_repository_key,repository_url,target_branch,base_revision,
                candidate_revision,candidate_tree,created_by,contributor_session_id,created_at,
                superseded_at FROM original.submissions"
                .to_owned()
        } else {
            format!("INSERT INTO main.{table} SELECT * FROM original.{table}")
        };
        sqlx::query(sqlx::AssertSqlSafe(statement))
            .execute(&mut old)
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO main.tasks(id,project_id,title,description,acceptance_json,kind,priority,lifecycle,revision,generation,current_attempt_id,blocked_reason,created_at,ready_since) SELECT id,project_id,title,description,acceptance_json,kind,priority,lifecycle,revision,generation,current_attempt_id,blocked_reason,created_at,ready_since FROM original.tasks")
        .execute(&mut old).await.unwrap();
    sqlx::query("UPDATE projects SET rowid=99")
        .execute(&mut old)
        .await
        .unwrap();
    sqlx::query("UPDATE workflow_activities SET rowid=77")
        .execute(&mut old)
        .await
        .unwrap();
    assert!(
        sqlx::query("UPDATE projects SET review_mode='either'")
            .execute(&mut old)
            .await
            .is_err()
    );
    old.close().await.unwrap();
    let upgraded = AppState::open(Config {
        database_path: database,
        ..f.state.config.clone()
    })
    .await
    .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT review_mode FROM projects")
            .fetch_one(&upgraded.pool)
            .await
            .unwrap(),
        "agent"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT rowid FROM projects")
            .fetch_one(&upgraded.pool)
            .await
            .unwrap(),
        99
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT rowid FROM workflow_activities")
            .fetch_one(&upgraded.pool)
            .await
            .unwrap(),
        77
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT summary FROM review_decisions")
            .fetch_one(&upgraded.pool)
            .await
            .unwrap(),
        "Preserve this decision"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM pragma_foreign_key_check")
            .fetch_one(&upgraded.pool)
            .await
            .unwrap(),
        0
    );
    sqlx::query("UPDATE projects SET review_mode='either'")
        .execute(&upgraded.pool)
        .await
        .unwrap();
    assert!(
        sqlx::query("UPDATE projects SET review_mode='invalid'")
            .execute(&upgraded.pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE workflow_activities SET subject_task_id='missing'")
            .execute(&upgraded.pool)
            .await
            .is_err()
    );
    // All pooled request connections, including the migrated one, enforce FKs.
    let mut connections = Vec::new();
    for _ in 0..8 {
        let mut connection = upgraded.pool.acquire().await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("PRAGMA foreign_keys")
                .fetch_one(&mut *connection)
                .await
                .unwrap(),
            1
        );
        connections.push(connection);
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

async fn register_child(f: &Fixture, parent: &Caller, p: &str, name: &str) -> (Caller, Value) {
    let child = Caller {
        session: Uuid::new_v4().to_string(),
        proof: secret(),
        ..parent.clone()
    };
    let body = json!({"session_id":child.session,"workstation_id":"child-workstation","harness":"test-child","capabilities":["code"],"subagent":{"project_id":p,"name":name,"parent_session_id":parent.session}});
    let (status, response) = register_session(f, &child, body).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    (child, response["data"].clone())
}

async fn register_session(f: &Fixture, c: &Caller, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/sessions")
        .header("content-type", "application/json")
        .header("idempotency-key", Uuid::new_v4().to_string())
        .header("authorization", format!("Bearer {}", c.token))
        .header("x-coordinator-session-proof", &c.proof)
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = f.app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn subagent_review_opt_in_preserves_identity_contributions_and_default_guards() {
    for enabled in [false, true] {
        let f = Fixture::new().await;
        let p = f
            .project("subagent-review", "https://example.test/children.git")
            .await;
        let (status, policy) = f.call(&f.admin, "PATCH", &format!("/api/v1/projects/{p}/policy"), json!({"expected_revision":1,"review_mode":"agent","recovery_mode":"agent","lease_seconds":600,"rules":"","agent_rule_editing":true,"automatic_integration":true,"allow_subagent_reviews":enabled})).await;
        assert_eq!(status, StatusCode::OK, "{policy}");
        let (status, _) = f.call(&f.a, "PATCH", &format!("/api/v1/projects/{p}/policy"), json!({"expected_revision":2,"review_mode":"agent","recovery_mode":"agent","lease_seconds":600,"rules":"","agent_rule_editing":true,"automatic_integration":true,"allow_subagent_reviews":!enabled})).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (helper, original) = register_child(&f, &f.a, &p, "implementation-helper").await;
        let (reviewer, _) = register_child(&f, &f.a, &p, "reviewer").await;
        let t = f.task(&p, "general", "Child reviewer").await;
        let owner = f.claim(&f.a, &p, &t, 2).await;
        let checkpoint_path = format!(
            "/api/v1/projects/{p}/attempts/{}/checkpoints",
            owner["id"].as_str().unwrap()
        );
        let checkpoint = json!({"generation":owner["generation"],"summary":"Register helper before delegation","contributor_session_ids":[helper.session]});
        let mut invalid_checkpoint = checkpoint.clone();
        invalid_checkpoint["contributor_session_ids"] = json!([helper.session, "missing-session"]);
        let (status, _) = f
            .call(&f.a, "POST", &checkpoint_path, invalid_checkpoint)
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let helpers: i64 =
            sqlx::query_scalar("SELECT count(*) FROM task_contributors WHERE session_id=?")
                .bind(&helper.session)
                .fetch_one(&f.state.pool)
                .await
                .unwrap();
        assert_eq!(
            helpers, 0,
            "invalid checkpoint must roll back every contribution"
        );
        let (status, saved) = f.call(&f.a, "POST", &checkpoint_path, checkpoint).await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        let (helper_resumed, resumed) = register_child(&f, &f.a, &p, "implementation-helper").await;
        assert_eq!(
            original["subagent_identity_id"],
            resumed["subagent_identity_id"]
        );
        let ordinary = Caller {
            session: Uuid::new_v4().to_string(),
            proof: secret(),
            ..f.a.clone()
        };
        let (status, registered) = register_session(&f, &ordinary, json!({"session_id":ordinary.session,"workstation_id":"ordinary","harness":"ordinary","capabilities":[]})).await;
        assert_eq!(status, StatusCode::OK, "{registered}");
        let other_project = f
            .project("other-project", "https://example.test/other.git")
            .await;
        let (other_child, _) = register_child(&f, &f.a, &other_project, "reviewer").await;
        let submitted = f
            .submit(&f.a, &p, &t, &owner, "general", 2, None, None, None, None)
            .await;
        let review = activity(&submitted, "agent_review");
        for contributor in [&f.a, &helper, &helper_resumed, &ordinary, &other_child] {
            let (status, rejected) = f.claim_activity(contributor, &p, review, 2, 0).await;
            assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
            assert_eq!(rejected["error"]["code"], "reviewer_not_independent");
        }
        let (status, claimed) = f.claim_activity(&reviewer, &p, review, 2, 0).await;
        if enabled {
            assert_eq!(status, StatusCode::OK, "{claimed}");
            let attempt = &claimed["data"]["attempt"];
            let (status, done) = f.call(&reviewer, "POST", &format!("/api/v1/projects/{p}/workflow-activities/{}/review", review["id"].as_str().unwrap()), json!({"generation":attempt["generation"],"submission_id":review["submission_id"],"decision":"approved","summary":"Independent child inspected evidence","findings":[]})).await;
            assert_eq!(status, StatusCode::OK, "{done}");
            assert_eq!(done["data"]["work_status"], "done");
            let next = f
                .task(&p, "general", "Parent continues after child review")
                .await;
            let continued = f.claim(&f.a, &p, &next, 2).await;
            assert_eq!(continued["task_id"], next["id"]);
        } else {
            assert_eq!(status, StatusCode::CONFLICT, "{claimed}");
            assert_eq!(claimed["error"]["code"], "reviewer_not_independent");
        }
    }
}

#[tokio::test]
async fn omitted_subagent_policy_preserves_existing_permission() {
    let f = Fixture::new().await;
    let p = f
        .project("policy-compat", "https://example.test/policy.git")
        .await;
    let mut body = json!({"expected_revision":1,"review_mode":"agent","recovery_mode":"agent","lease_seconds":600,"rules":"","agent_rule_editing":true,"automatic_integration":true,"allow_subagent_reviews":true});
    let (status, _) = f
        .call(
            &f.admin,
            "PATCH",
            &format!("/api/v1/projects/{p}/policy"),
            body.clone(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    body["expected_revision"] = json!(2);
    body.as_object_mut()
        .unwrap()
        .remove("allow_subagent_reviews");
    let (status, policy) = f
        .call(&f.a, "PATCH", &format!("/api/v1/projects/{p}/policy"), body)
        .await;
    assert_eq!(status, StatusCode::OK, "{policy}");
    assert_eq!(policy["data"]["allow_subagent_reviews"], true);
}

#[tokio::test]
async fn subagent_registration_rejects_reparenting_and_foreign_parent() {
    let f = Fixture::new().await;
    let p = f
        .project("child-identity", "https://example.test/identity.git")
        .await;
    let (child, identity) = register_child(&f, &f.a, &p, "child").await;
    let (sibling, _) = register_child(&f, &f.a, &p, "sibling").await;
    let body = json!({"session_id":child.session,"workstation_id":"child-workstation","harness":"test-child","capabilities":["code"],"subagent":{"project_id":p,"name":"child","parent_session_id":sibling.session}});
    let (status, _) = register_session(&f, &child, body.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let mut foreign = body;
    foreign["subagent"]["parent_session_id"] = json!(f.b.session);
    let (status, _) = register_session(&f, &child, foreign).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, got) = f
        .call(
            &child,
            "GET",
            &format!("/api/v1/sessions/{}", child.session),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        got["data"]["subagent_identity_id"],
        identity["subagent_identity_id"]
    );
    assert_eq!(got["data"]["subagent"]["parent_session_id"], f.a.session);
}

#[tokio::test]
async fn review_decision_rechecks_contribution_after_claim() {
    let f = Fixture::new().await;
    let p = f
        .project("late-contributor", "https://example.test/late.git")
        .await;
    let t = f.task(&p, "general", "Late contribution").await;
    let owner = f.claim(&f.a, &p, &t, 1).await;
    let submitted = f
        .submit(&f.a, &p, &t, &owner, "general", 1, None, None, None, None)
        .await;
    let review = activity(&submitted, "agent_review");
    let (status, claimed) = f.claim_activity(&f.b, &p, review, 1, 0).await;
    assert_eq!(status, StatusCode::OK);
    sqlx::query("INSERT INTO task_contributors(task_id,principal_id,session_id,first_contributed_at) VALUES(?,?,?,?)")
        .bind(t["id"].as_str().unwrap()).bind(&f.b.principal).bind(&f.b.session).bind(f.state.now()).execute(&f.state.pool).await.unwrap();
    let (status, rejected) = f.call(&f.b, "POST", &format!("/api/v1/projects/{p}/workflow-activities/{}/review", review["id"].as_str().unwrap()), json!({"generation":claimed["data"]["attempt"]["generation"],"submission_id":review["submission_id"],"decision":"approved","summary":"Must reject contributor","findings":[]})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
    assert_eq!(rejected["error"]["code"], "reviewer_not_independent");
}

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
async fn agent_reconciliation_requires_stopped_publisher_and_exact_fresh_remote_evidence() {
    let f = Fixture::new().await;
    let (p, _t, submitted) = code_integration(&f, "agent-reconcile", "agent-reconcile").await;
    let integration = activity(&submitted, "integration").clone();
    let (_, claimed) = f.claim_activity(&f.c, &p, &integration, 2, 1).await;
    let attempt = claimed["data"]["attempt"].clone();
    f.checkout(&f.c, &p, &attempt, CANDIDATE).await;
    let path = format!(
        "/api/v1/projects/{p}/workflow-activities/{}/publication-intent",
        integration["id"].as_str().unwrap()
    );
    let (status, intent) = f.call(&f.c, "POST", &path, json!({"generation":attempt["generation"],"submission_id":integration["submission_id"],"observed_target_revision":BASE,"observed_target_tree":BASE_TREE,"result_revision":RESULT,"result_tree":RESULT_TREE})).await;
    assert_eq!(status, StatusCode::OK, "{intent}");

    let reconcile_path = format!(
        "/api/v1/projects/{p}/workflow-activities/{}/agent-publication-reconciliation",
        integration["id"].as_str().unwrap()
    );
    let evidence = json!({
        "attempt_id": attempt["id"],
        "generation": attempt["generation"],
        "submission_id": integration["submission_id"],
        "disposition": "published",
        "canonical_repository_key": "agent-reconcile",
        "target_branch": "main",
        "observed_target_revision": RESULT,
        "observed_target_tree": RESULT_TREE,
        "observed_at": f.clock.now_ms(),
        "local_journal_verified": true,
        "publisher_stopped": true,
        "evidence": "publisher process exited; fresh ls-remote observation resolved to the intended commit and tree"
    });
    let mut no_journal = evidence.clone();
    no_journal["local_journal_verified"] = json!(false);
    let (status, journal_rejected) = f.call(&f.c, "POST", &reconcile_path, no_journal).await;
    assert_eq!(status, StatusCode::CONFLICT, "{journal_rejected}");
    assert_eq!(
        journal_rejected["error"]["code"],
        "publication_evidence_incomplete"
    );
    let (status, live_rejected) = f
        .call(&f.c, "POST", &reconcile_path, evidence.clone())
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{live_rejected}");
    assert_eq!(
        live_rejected["error"]["code"],
        "publication_producer_uncertain"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM integration_holds WHERE activity_id=?")
            .bind(integration["id"].as_str().unwrap())
            .fetch_one(&f.state.pool)
            .await
            .unwrap(),
        "held"
    );

    f.clock.0.fetch_add(600_001, Ordering::SeqCst);
    let mut fresh_evidence = evidence.clone();
    fresh_evidence["observed_at"] = json!(f.clock.now_ms());
    let reconciliation_key = "agent-publication-reconciliation-retry";
    let (status, reconciled) = call(
        f.app.clone(),
        &f.c,
        "POST",
        &reconcile_path,
        reconciliation_key,
        fresh_evidence.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reconciled}");
    let (replay_status, replayed) = call(
        f.app.clone(),
        &f.c,
        "POST",
        &reconcile_path,
        reconciliation_key,
        fresh_evidence,
    )
    .await;
    assert_eq!(replay_status, StatusCode::OK, "{replayed}");
    assert_eq!(
        replayed["data"]["activities"],
        reconciled["data"]["activities"]
    );
    assert_eq!(
        reconciled["data"]["activities"]
            .as_array()
            .unwrap()
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
    let row = sqlx::query(
        "SELECT disposition,evidence,actor_id FROM publication_reconciliations WHERE activity_id=?",
    )
    .bind(integration["id"].as_str().unwrap())
    .fetch_one(&f.state.pool)
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("disposition"), "published");
    assert!(
        row.get::<String, _>("evidence")
            .contains("local_journal_verified=true; publisher_stopped=true")
    );
    assert_eq!(row.get::<String, _>("actor_id"), f.c.principal);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM publication_reconciliations WHERE activity_id=?"
        )
        .bind(integration["id"].as_str().unwrap())
        .fetch_one(&f.state.pool)
        .await
        .unwrap(),
        1
    );
}

#[tokio::test]
async fn agent_reconciliation_keeps_changed_target_human_gated() {
    let f = Fixture::new().await;
    let (p, _t, submitted) =
        code_integration(&f, "agent-reconcile-moved", "agent-reconcile-moved").await;
    let integration = activity(&submitted, "integration").clone();
    let (_, claimed) = f.claim_activity(&f.c, &p, &integration, 2, 1).await;
    let attempt = claimed["data"]["attempt"].clone();
    f.checkout(&f.c, &p, &attempt, CANDIDATE).await;
    let (status, intent) = f.call(&f.c, "POST", &format!("/api/v1/projects/{p}/workflow-activities/{}/publication-intent", integration["id"].as_str().unwrap()), json!({"generation":attempt["generation"],"submission_id":integration["submission_id"],"observed_target_revision":BASE,"observed_target_tree":BASE_TREE,"result_revision":RESULT,"result_tree":RESULT_TREE})).await;
    assert_eq!(status, StatusCode::OK, "{intent}");
    f.clock.0.fetch_add(600_001, Ordering::SeqCst);
    let (status, rejected) = f.call(&f.c, "POST", &format!("/api/v1/projects/{p}/workflow-activities/{}/agent-publication-reconciliation", integration["id"].as_str().unwrap()), json!({"attempt_id":attempt["id"],"generation":attempt["generation"],"submission_id":integration["submission_id"],"disposition":"target_moved","canonical_repository_key":"agent-reconcile-moved","target_branch":"main","observed_target_revision":RESULT,"observed_target_tree":RESULT_TREE,"observed_at":f.clock.now_ms(),"local_journal_verified":true,"publisher_stopped":true,"evidence":"target changed during uncertain publication"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected}");
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM integration_holds WHERE activity_id=?")
            .bind(integration["id"].as_str().unwrap())
            .fetch_one(&f.state.pool)
            .await
            .unwrap(),
        "held"
    );
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
    let (status, preconditions) = f
        .call(
            &f.b,
            "GET",
            &format!(
                "/api/v1/projects/{p}/preconditions/{}",
                review["id"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{preconditions}");
    assert!(
        preconditions["data"]["unmet_preconditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["code"] == "human_recovery_required")
    );
    let (status, _) = f.claim_activity(&f.c, &p, &review, 2, 0).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status,reopened)=f.call(&f.admin,"POST",&format!("/api/v1/projects/{p}/tasks/{}/workflow/reopen",t["id"].as_str().unwrap()),json!({"submission_id":review["submission_id"],"reason":"expired reviewer inspected; no jobs or holds remain"})).await;
    assert_eq!(status, StatusCode::OK, "{reopened}");
    assert_eq!(reopened["data"]["work_status"], "ready");
}

#[tokio::test]
async fn agent_mode_quiescent_expired_activity_is_claimable() {
    let f = Fixture::new().await;
    let p = f
        .project("agent recovery", "https://example.test/agent-recovery.git")
        .await;
    f.review_policy(&p, "agent").await;
    let task = f
        .task(&p, "general", "Expired review can be reclaimed")
        .await;
    let owner = f.claim(&f.a, &p, &task, 2).await;
    let submitted = f
        .submit(
            &f.a, &p, &task, &owner, "general", 2, None, None, None, None,
        )
        .await;
    let review = activity(&submitted, "agent_review").clone();
    let (status, first) = f.claim_activity(&f.b, &p, &review, 2, 0).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    f.clock.0.fetch_add(600_000, Ordering::SeqCst);
    let attempt = &first["data"]["attempt"];
    let reservation_id = Uuid::new_v4().to_string();
    let job_id = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO reservations(id,project_id,attempt_id,generation,state,created_by,created_at) VALUES(?,?,?,?,'held',?,?)")
        .bind(&reservation_id).bind(&p).bind(attempt["id"].as_str().unwrap()).bind(attempt["generation"].as_i64().unwrap()).bind(&f.b.principal).bind(f.state.now()).execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO jobs(id,producer_id,project_id,task_id,attempt_id,generation,runner_instance_id,workstation_id,label,source_revision,source_tree,reservation_id,state,created_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,'registered',?)")
        .bind(&job_id).bind(Uuid::new_v4().to_string()).bind(&p).bind(review["activity_task_id"].as_str().unwrap()).bind(attempt["id"].as_str().unwrap()).bind(attempt["generation"].as_i64().unwrap()).bind(Uuid::new_v4().to_string()).bind("review-host").bind("unresolved review producer").bind("candidate").bind("tree").bind(&reservation_id).bind(f.state.now()).execute(&f.state.pool).await.unwrap();
    let (status, preconditions) = f
        .call(
            &f.b,
            "GET",
            &format!(
                "/api/v1/projects/{p}/preconditions/{}",
                review["id"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{preconditions}");
    assert_eq!(preconditions["data"]["eligible_to_claim"], false);
    assert!(
        !preconditions["data"]["unmet_preconditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["code"] == "recovery_inspection_required")
    );
    assert!(
        preconditions["data"]["unmet_preconditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["code"] == "attempt_evidence_unresolved")
    );
    let (status, blocked_claim) = f.claim_activity(&f.b, &p, &review, 2, 0).await;
    assert_eq!(status, StatusCode::CONFLICT, "{blocked_claim}");
    assert_eq!(
        blocked_claim["error"]["code"],
        "attempt_evidence_unresolved"
    );
    sqlx::query("UPDATE jobs SET state='succeeded',exit_code=0,inputs_unchanged=1 WHERE id=?")
        .bind(&job_id)
        .execute(&f.state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE reservations SET state='released',released_at=?,released_by=?,release_reason='test terminal producer' WHERE id=?")
        .bind(f.state.now()).bind(&f.b.principal).bind(&reservation_id).execute(&f.state.pool).await.unwrap();
    let (status, preconditions) = f
        .call(
            &f.b,
            "GET",
            &format!(
                "/api/v1/projects/{p}/preconditions/{}",
                review["id"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{preconditions}");
    assert_eq!(preconditions["data"]["eligible_to_claim"], true);
    let (status, reclaimed) = f.claim_activity(&f.b, &p, &review, 2, 0).await;
    assert_eq!(status, StatusCode::OK, "{reclaimed}");
}

#[tokio::test]
async fn preconditions_surface_operator_reopen_and_reviewer_independence() {
    let f = Fixture::new().await;
    let p = f
        .project(
            "precondition review",
            "https://example.test/preconditions.git",
        )
        .await;
    let (status, policy) = f
        .call(
            &f.admin,
            "PATCH",
            &format!("/api/v1/projects/{p}/policy"),
            json!({"expected_revision":1,"review_mode":"agent","recovery_mode":"agent","lease_seconds":600,"rules":"","agent_rule_editing":false,"automatic_integration":true}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{policy}");
    let task = f.task(&p, "general", "Stale review candidate").await;
    f.ack(&f.a, &p, 2).await;
    let owner = f.claim(&f.a, &p, &task, 2).await;
    let submitted = f
        .submit(
            &f.a, &p, &task, &owner, "general", 2, None, None, None, None,
        )
        .await;
    let review = activity(&submitted, "agent_review").clone();

    let (status, updated_policy) = f
        .call(
            &f.admin,
            "PATCH",
            &format!("/api/v1/projects/{p}/policy"),
            json!({"expected_revision":2,"review_mode":"none","recovery_mode":"agent","lease_seconds":600,"rules":"","agent_rule_editing":false,"automatic_integration":true}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{updated_policy}");

    let (status, task_detail) = f
        .call(
            &f.a,
            "GET",
            &format!(
                "/api/v1/projects/{p}/tasks/{}",
                task["id"].as_str().unwrap()
            ),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{task_detail}");
    assert!(
        task_detail["data"]["preconditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|blocker| blocker["code"] == "operator_reopen_required")
    );

    let (status, inspected) = f
        .call(
            &f.a,
            "GET",
            &format!(
                "/api/v1/projects/{p}/preconditions/{}",
                review["id"].as_str().unwrap()
            ),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{inspected}");
    assert!(
        inspected["data"]["unmet_preconditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|blocker| blocker["code"] == "operator_reopen_required")
    );
    assert!(
        inspected["data"]["unmet_preconditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|blocker| blocker["code"] == "reviewer_not_independent")
    );

    let (status, reopened) = f
        .call(
            &f.admin,
            "POST",
            &format!("/api/v1/projects/{p}/tasks/{}/workflow/reopen", task["id"].as_str().unwrap()),
            json!({"submission_id":review["submission_id"],"reason":"Policy changed and the obsolete candidate was intentionally reopened."}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{reopened}");
    let (status, old_activity) = f
        .call(
            &f.a,
            "GET",
            &format!(
                "/api/v1/projects/{p}/preconditions/{}",
                review["id"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{old_activity}");
    assert!(
        !old_activity["data"]["unmet_preconditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|blocker| blocker["code"] == "operator_reopen_required")
    );
    f.ack(&f.a, &p, 3).await;
    let (status, revision_ready) = f
        .call(
            &f.a,
            "GET",
            &format!(
                "/api/v1/projects/{p}/tasks/{}",
                task["id"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{revision_ready}");
    assert_eq!(revision_ready["data"]["work_status"], "ready");
    assert_eq!(revision_ready["data"]["eligible_to_claim"], true);
    assert!(
        !revision_ready["data"]["preconditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|blocker| blocker["code"] == "operator_reopen_required")
    );
}

#[tokio::test]
async fn orientation_and_inspection_flag_merge_state_for_local_preflight() {
    let f = Fixture::new().await;
    let repo = "https://example.test/preflight.git";
    let canonical = format!(
        "url-sha256:{}",
        hex::encode(Sha256::digest(repo.as_bytes()))
    );
    let (p, task, submitted) = code_integration(&f, "preflight", &canonical).await;
    let integration = activity(&submitted, "integration").clone();

    let (status, detail) = f
        .call(
            &f.a,
            "GET",
            &format!(
                "/api/v1/projects/{p}/tasks/{}",
                task["id"].as_str().unwrap()
            ),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(
        detail["data"]["precondition_hints"][0]["code"],
        "candidate_stale_merge_conflict_requires_preflight"
    );
    assert_eq!(
        detail["data"]["precondition_hints"][0]["state"],
        "requires_local_observation"
    );

    let (status, inspected) = f
        .call(
            &f.a,
            "GET",
            &format!(
                "/api/v1/projects/{p}/preconditions/{}",
                integration["id"].as_str().unwrap()
            ),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{inspected}");
    assert_eq!(
        inspected["data"]["precondition_hints"][0]["code"],
        "candidate_stale_merge_conflict_requires_preflight"
    );

    let (status, orientation) = f
        .call(
            &f.a,
            "GET",
            &format!("/api/v1/projects/{p}/orientation"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{orientation}");
    assert!(
        orientation["data"]["workflow_subjects"]
            .as_array()
            .unwrap()
            .iter()
            .any(|subject| subject["task"]["id"] == task["id"]
                && subject["task"]["precondition_hints"][0]["code"]
                    == "candidate_stale_merge_conflict_requires_preflight")
    );
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

fn derived_roster(revision: i64) -> Value {
    json!({"expected_revision":revision,"required_checks":[{"identity":"workspace-tests","version":"v1","environment":"linux-ci"}]})
}

#[tokio::test]
async fn repository_url_aliases_derive_and_reuse_legacy_bindings() {
    let f = Fixture::new().await;
    let legacy = f
        .project("legacy", "https://github.com/Example/Repo.git")
        .await;
    f.workflow_policy(&legacy, "saved-before-url-derivation")
        .await;
    for (index, url) in [
        "git@github.com:example/repo",
        "git@GitHub.COM:Example/Repo.GIT",
        "ssh://git@GitHub.COM/Example/Repo.GIT/",
        "ssh://git@github.com/EXAMPLE/REPO.git/",
        "https://github.com/example/repo/",
    ]
    .iter()
    .enumerate()
    {
        let p = f.project(&format!("alias-{index}"), url).await;
        let (status, result) = f
            .call(
                &f.admin,
                "PUT",
                &format!("/api/v1/projects/{p}/workflow-policy"),
                derived_roster(0),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{result}");
        assert_eq!(
            result["data"]["canonical_repository_key"],
            "saved-before-url-derivation"
        );
    }
    let p = f
        .project("new repository", "https://github.com/example/other.git/")
        .await;
    let (_, result) = f
        .call(
            &f.admin,
            "PUT",
            &format!("/api/v1/projects/{p}/workflow-policy"),
            derived_roster(0),
        )
        .await;
    assert_eq!(
        result["data"]["canonical_repository_key"],
        "github.com/example/other"
    );
    let other = f
        .project("other host", "https://elsewhere.example/example/other.git")
        .await;
    let (_, different) = f
        .call(
            &f.admin,
            "PUT",
            &format!("/api/v1/projects/{other}/workflow-policy"),
            derived_roster(0),
        )
        .await;
    assert_ne!(
        different["data"]["canonical_repository_key"],
        result["data"]["canonical_repository_key"]
    );
    let (status, changed) = f.call(&f.admin, "PUT", &format!("/api/v1/projects/{legacy}/workflow-policy"), json!({"canonical_repository_key":"split", "expected_revision":1, "required_checks":derived_roster(0)["required_checks"]})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{changed}");
}

#[tokio::test]
async fn repository_aliases_require_admin_and_preserve_existing_evidence() {
    let f = Fixture::new().await;
    let p = f
        .project("custom ssh", "git@work-github:example/repo.git")
        .await;
    sqlx::query("UPDATE principals SET role='operator' WHERE id=?")
        .bind(&f.admin.principal)
        .execute(&f.state.pool)
        .await
        .unwrap();
    let path = format!("/api/v1/projects/{p}/workflow-policy");
    let (status, initial) = f.call(&f.admin, "PUT", &path, derived_roster(0)).await;
    assert_eq!(status, StatusCode::OK, "{initial}");
    assert!(
        initial["data"]["canonical_repository_key"]
            .as_str()
            .unwrap()
            .starts_with("url-sha256:")
    );
    let alias = json!({"canonical_repository_key":"github.com/example/repo", "expected_revision":1, "required_checks":derived_roster(0)["required_checks"]});
    let (status, denied) = f.call(&f.admin, "PUT", &path, alias.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{denied}");
    sqlx::query("UPDATE principals SET role='admin' WHERE id=?")
        .bind(&f.admin.principal)
        .execute(&f.state.pool)
        .await
        .unwrap();
    let (status, saved) = f.call(&f.admin, "PUT", &path, alias).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let task = f.task(&p, "general", "historical evidence").await;
    let attempt = f.claim(&f.a, &p, &task, 1).await;
    f.submit(
        &f.a, &p, &task, &attempt, "general", 1, None, None, None, None,
    )
    .await;
    let (status, rejected) = f.call(&f.admin, "PUT", &path, json!({"canonical_repository_key":"another-identity", "expected_revision":2, "required_checks":derived_roster(0)["required_checks"]})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
    assert_eq!(rejected["error"]["code"], "canonical_binding_frozen");
    // Even an external URL edit cannot silently replace this saved identity.
    sqlx::query(
        "UPDATE projects SET repository_url='https://github.com/different/repository' WHERE id=?",
    )
    .bind(&p)
    .execute(&f.state.pool)
    .await
    .unwrap();
    let (status, preserved) = f.call(&f.admin, "PUT", &path, derived_roster(2)).await;
    assert_eq!(status, StatusCode::OK, "{preserved}");
    assert_eq!(
        preserved["data"]["canonical_repository_key"],
        "github.com/example/repo"
    );
}

#[tokio::test]
async fn repository_derivation_bounds_and_conflicting_legacy_bindings() {
    let f = Fixture::new().await;
    let long_url = format!("https://github.com/owner/{}", "x".repeat(300));
    let p = f.project("long URL", &long_url).await;
    let (status, saved) = f
        .call(
            &f.admin,
            "PUT",
            &format!("/api/v1/projects/{p}/workflow-policy"),
            derived_roster(0),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let key = saved["data"]["canonical_repository_key"].as_str().unwrap();
    assert!(key.starts_with("url-sha256:") && key.len() <= 255);
    sqlx::query("UPDATE principals SET role='operator' WHERE id=?")
        .bind(&f.admin.principal)
        .execute(&f.state.pool)
        .await
        .unwrap();
    let (status, unchanged) = f.call(&f.admin, "PUT", &format!("/api/v1/projects/{p}/workflow-policy"), json!({"expected_revision":1,"canonical_repository_key":key,"required_checks":derived_roster(0)["required_checks"]})).await;
    assert_eq!(status, StatusCode::OK, "{unchanged}");
    sqlx::query("UPDATE principals SET role='admin' WHERE id=?")
        .bind(&f.admin.principal)
        .execute(&f.state.pool)
        .await
        .unwrap();
    let first = f
        .project("legacy first", "https://github.com/legacy/repo")
        .await;
    let second = f
        .project("legacy second", "git@unresolved:legacy/repo")
        .await;
    f.workflow_policy(&first, "legacy-first").await;
    f.workflow_policy(&second, "legacy-second").await;
    // Represent two pre-upgrade identities that the old exact-URL check allowed.
    sqlx::query("UPDATE projects SET repository_url='git@github.com:legacy/repo.git' WHERE id=?")
        .bind(&second)
        .execute(&f.state.pool)
        .await
        .unwrap();
    let third = f
        .project("new alias", "ssh://git@github.com/legacy/repo")
        .await;
    let (status, conflict) = f
        .call(
            &f.admin,
            "PUT",
            &format!("/api/v1/projects/{third}/workflow-policy"),
            derived_roster(0),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
    assert_eq!(conflict["error"]["code"], "canonical_repository_conflict");
    for (project, key) in [(&first, "legacy-first"), (&second, "legacy-second")] {
        let (_, value) = f
            .call(
                &f.admin,
                "GET",
                &format!("/api/v1/projects/{project}/workflow-policy"),
                Value::Null,
            )
            .await;
        assert_eq!(value["data"]["canonical_repository_key"], key);
    }
}
