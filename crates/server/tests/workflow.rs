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
        } else if table == "credentials" {
            // Later migrations add credential attributes the old schema lacks.
            "INSERT INTO main.credentials SELECT id,principal_id,token_hash,created_at,
                revoked_at,expires_at,name,issued_by FROM original.credentials"
                .to_owned()
        } else if table == "projects" {
            // Later migrations add the integration owner the old schema lacks.
            "INSERT INTO main.projects(id,name,repository_url,target_branch,policy_revision,
                review_mode,recovery_mode,lease_seconds,rules,agent_rule_editing,
                automatic_integration,created_at,allow_subagent_reviews)
                SELECT id,name,repository_url,target_branch,policy_revision,review_mode,
                recovery_mode,lease_seconds,rules,agent_rule_editing,automatic_integration,
                created_at,allow_subagent_reviews FROM original.projects"
                .to_owned()
        } else if table == "review_decisions" {
            "INSERT INTO main.review_decisions SELECT activity_id,submission_id,attempt_id,
                reviewer_id,reviewer_session_id,decision,summary,created_at
                FROM original.review_decisions"
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
    let lifecycle_revision: i64 = sqlx::query_scalar("SELECT revision FROM tasks WHERE id=?")
        .bind(t["id"].as_str().unwrap())
        .fetch_one(&f.state.pool)
        .await
        .unwrap();
    let (status, protected) = f
        .call(
            &f.admin,
            "POST",
            &format!(
                "/api/v1/projects/{p}/tasks/{}/archive",
                t["id"].as_str().unwrap()
            ),
            json!({"expected_revision":lifecycle_revision,"reason":"Must wait for review."}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{protected}");
    assert_eq!(protected["error"]["code"], "task_workflow_protected");
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

// P1 autonomy (B1): a review_mode change reconciles the required reviews instead of
// stranding the candidate behind a human reopen.
#[tokio::test]
async fn review_mode_change_reconciles_reviews_instead_of_stranding_candidate() {
    let f = Fixture::new().await;
    let p = f
        .project(
            "precondition review",
            "https://example.test/preconditions.git",
        )
        .await;
    f.review_policy(&p, "agent").await;
    let task = f.task(&p, "general", "Reconciled review candidate").await;
    f.ack(&f.a, &p, 2).await;
    let owner = f.claim(&f.a, &p, &task, 2).await;
    let submitted = f
        .submit(
            &f.a, &p, &task, &owner, "general", 2, None, None, None, None,
        )
        .await;
    let review = activity(&submitted, "agent_review").clone();
    let review_path = format!(
        "/api/v1/projects/{p}/preconditions/{}",
        review["id"].as_str().unwrap()
    );
    let (status, inspected) = f.call(&f.a, "GET", &review_path, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{inspected}");
    let codes: Vec<_> = inspected["data"]["unmet_preconditions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["code"].clone())
        .collect();
    assert!(
        codes.contains(&json!("reviewer_not_independent")),
        "{inspected}"
    );
    assert!(
        !codes.contains(&json!("operator_reopen_required")),
        "{inspected}"
    );

    let (status, updated_policy) = f
        .call(
            &f.admin,
            "PATCH",
            &format!("/api/v1/projects/{p}/policy"),
            json!({"expected_revision":2,"review_mode":"none","recovery_mode":"agent","lease_seconds":600,"rules":"","agent_rule_editing":false,"automatic_integration":true}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{updated_policy}");
    let (status, detail) = f
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
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["data"]["lifecycle"], "done", "{detail}");
    let state: String = sqlx::query_scalar("SELECT state FROM workflow_activities WHERE id=?")
        .bind(review["id"].as_str().unwrap())
        .fetch_one(&f.state.pool)
        .await
        .unwrap();
    assert_eq!(state, "canceled");
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

// P1 autonomy: every remaining human-only refusal is labelled, so agents can route
// it to the human queue instead of retrying.
#[tokio::test]
async fn human_only_refusals_and_preconditions_are_labelled() {
    let f = Fixture::new().await;
    let p = f
        .project("labelled-gates", "https://example.test/labelled.git")
        .await;
    let (status, refused) = f
        .call(&f.a, "PUT", &format!("/api/v1/projects/{p}/workflow-policy"),
            json!({"expected_revision":0,"canonical_repository_key":"labelled","required_checks":[{"identity":"t","version":"v1","environment":"any"}]}))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(refused["error"]["code"], "operation_not_permitted");
    assert_eq!(refused["error"]["details"]["required_actor"], "human");
    assert_eq!(refused["error"]["details"]["gate"], "workflow_policy_edit");

    f.review_policy(&p, "human").await;
    let t = f.task(&p, "general", "Human-reviewed work").await;
    let owner = f.claim(&f.a, &p, &t, 2).await;
    let submitted = f
        .submit(&f.a, &p, &t, &owner, "general", 2, None, None, None, None)
        .await;
    let review = activity(&submitted, "human_review").clone();
    f.ack(&f.b, &p, 2).await;
    let (status, inspected) = f
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
    assert_eq!(status, StatusCode::OK, "{inspected}");
    let gate = inspected["data"]["unmet_preconditions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["code"] == "human_reviewer_required")
        .expect("human reviewer gate");
    assert_eq!(gate["required_actor"], "human");
}

// P1 autonomy (B1): a lease-only policy change between claim and submit no longer
// strands the attempt or its candidate; the reviewer claims under the new policy.
#[tokio::test]
async fn lease_only_policy_change_keeps_attempt_and_candidate_current() {
    let f = Fixture::new().await;
    let p = f
        .project("lease-change", "https://example.test/lease.git")
        .await;
    f.review_policy(&p, "agent").await;
    let task = f.task(&p, "general", "Lease change survivor").await;
    let owner = f.claim(&f.a, &p, &task, 2).await;
    let (status, changed) = f
        .call(&f.admin, "PATCH", &format!("/api/v1/projects/{p}/policy"),
            json!({"expected_revision":2,"review_mode":"agent","recovery_mode":"agent","lease_seconds":3600,"rules":"","agent_rule_editing":false,"automatic_integration":true}))
        .await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    let submitted = f
        .submit(
            &f.a, &p, &task, &owner, "general", 2, None, None, None, None,
        )
        .await;
    let review = activity(&submitted, "agent_review").clone();
    f.ack(&f.b, &p, 3).await;
    let (status, claimed) = f
        .call(&f.b, "POST", &format!("/api/v1/projects/{p}/workflow-activities/{}/claim", review["id"].as_str().unwrap()),
            json!({"expected_submission_id":review["submission_id"],"expected_project_policy_revision":2,"expected_workflow_policy_revision":0}))
        .await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
}

// P1 autonomy: tightening review_mode during review adds the newly required review
// and cancels the queued one that is no longer required (never grandfathered).
#[tokio::test]
async fn tightened_review_mode_adds_required_review() {
    let f = Fixture::new().await;
    let p = f
        .project("tighten", "https://example.test/tighten.git")
        .await;
    f.review_policy(&p, "agent").await;
    let task = f.task(&p, "general", "Tightened review").await;
    let owner = f.claim(&f.a, &p, &task, 2).await;
    let submitted = f
        .submit(
            &f.a, &p, &task, &owner, "general", 2, None, None, None, None,
        )
        .await;
    let submission = activity(&submitted, "agent_review")["submission_id"].clone();
    let (status, changed) = f
        .call(&f.admin, "PATCH", &format!("/api/v1/projects/{p}/policy"),
            json!({"expected_revision":2,"review_mode":"human","recovery_mode":"agent","lease_seconds":600,"rules":"","agent_rule_editing":false,"automatic_integration":true}))
        .await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT kind,state FROM workflow_activities WHERE submission_id=? ORDER BY kind",
    )
    .bind(submission.as_str().unwrap())
    .fetch_all(&f.state.pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            ("agent_review".to_owned(), "canceled".to_owned()),
            ("human_review".to_owned(), "queued".to_owned())
        ]
    );
}

// P1 autonomy: agents unblock under recovery_mode=agent and cancel (with a
// replacement link) under agent_rule_editing; otherwise the gate stays human.
#[tokio::test]
async fn agents_unblock_and_cancel_only_when_delegated() {
    let f = Fixture::new().await;
    let p = f
        .project("agent-lifecycle", "https://example.test/lifecycle.git")
        .await;
    f.policy_none(&p).await;
    let t = f.task(&p, "general", "Wrongly shaped work").await;
    let owner = f.claim(&f.a, &p, &t, 2).await;
    let (status, released) = f
        .call(&f.a, "POST", &format!("/api/v1/projects/{p}/attempts/{}/release", owner["id"].as_str().unwrap()),
            json!({"generation":owner["generation"],"summary":"Needs a resource that now exists.","blocked":true}))
        .await;
    assert_eq!(status, StatusCode::OK, "{released}");
    let task_path = format!("/api/v1/projects/{p}/tasks/{}", t["id"].as_str().unwrap());
    let revision = |v: &Value| v["data"]["revision"].as_i64().unwrap();
    let (_, current) = f.call(&f.b, "GET", &task_path, Value::Null).await;
    let (status, unblocked) = f
        .call(
            &f.b,
            "POST",
            &format!("{task_path}/unblock"),
            json!({"expected_revision":revision(&current),"reason":"The resource was created."}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{unblocked}");

    let (_, current) = f.call(&f.b, "GET", &task_path, Value::Null).await;
    let cancel = json!({"expected_revision":revision(&current),"reason":"Wrong kind; replaced."});
    let (status, refused) = f
        .call(&f.b, "POST", &format!("{task_path}/cancel"), cancel.clone())
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(refused["error"]["details"]["gate"], "task_cancel");

    let (status, policy) = f
        .call(&f.admin, "PATCH", &format!("/api/v1/projects/{p}/policy"),
            json!({"expected_revision":2,"review_mode":"none","recovery_mode":"agent","lease_seconds":600,"rules":"","agent_rule_editing":true,"automatic_integration":true}))
        .await;
    assert_eq!(status, StatusCode::OK, "{policy}");
    let replacement = f.task(&p, "code", "Correctly shaped work").await;
    let mut cancel = cancel;
    cancel["replacement_task_id"] = replacement["id"].clone();
    let (status, canceled) = f
        .call(&f.b, "POST", &format!("{task_path}/cancel"), cancel)
        .await;
    assert_eq!(status, StatusCode::OK, "{canceled}");
    assert_eq!(canceled["data"]["lifecycle"], "canceled");
    assert_eq!(canceled["data"]["replacement_task_id"], replacement["id"]);
}

/// Submit an approved-by-policy (review_mode none) code candidate and return the
/// task and its integration activity.
async fn integrating_code_task(f: &Fixture, name: &str) -> (String, Value, Value) {
    let repo = format!("https://example.test/{name}.git");
    let p = f.project(name, &repo).await;
    f.policy_none(&p).await;
    f.workflow_policy(&p, &repo).await;
    let t = f.task(&p, "code", "Conflicting candidate").await;
    let owner = f.claim(&f.a, &p, &t, 2).await;
    let base = "1111111111111111111111111111111111111111";
    f.checkout(&f.a, &p, &owner, base).await;
    let submitted = f
        .submit(
            &f.a,
            &p,
            &t,
            &owner,
            "code",
            2,
            Some(&repo),
            Some(base),
            Some("2222222222222222222222222222222222222222"),
            Some("3333333333333333333333333333333333333333"),
        )
        .await;
    let integration = activity(&submitted, "integration").clone();
    (p, t, integration)
}

fn revise_body(submission: &Value, code: &str, evidence: Option<&str>) -> Value {
    json!({"submission_id":submission,"reason":"Agent revise","reason_code":code,"evidence":evidence})
}

// P1 autonomy (B2): the integration owner revises a conflicting candidate with
// evidence; other agents and reason-less agent calls are refused.
#[tokio::test]
async fn integration_owner_revises_conflicting_candidate() {
    let f = Fixture::new().await;
    let (p, t, integration) = integrating_code_task(&f, "revise-conflict").await;
    let (status, claimed) = f.claim_activity(&f.c, &p, &integration, 2, 1).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    let path = format!(
        "/api/v1/projects/{p}/tasks/{}/workflow/reopen",
        t["id"].as_str().unwrap()
    );
    let submission = &integration["submission_id"];
    let (status, missing) = f
        .call(
            &f.c,
            "POST",
            &path,
            json!({"submission_id":submission,"reason":"conflict"}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{missing}");
    f.ack(&f.b, &p, 2).await;
    let (status, other) = f
        .call(
            &f.b,
            "POST",
            &path,
            revise_body(submission, "conflict", Some("scripts/x.mjs")),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{other}");
    assert_eq!(other["error"]["code"], "revise_not_permitted");
    let (status, revised) = f
        .call(
            &f.c,
            "POST",
            &path,
            revise_body(submission, "conflict", Some("CONFLICT in scripts/x.mjs")),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{revised}");
    assert_eq!(revised["data"]["phase"], "revision_needed");
    assert_eq!(revised["data"]["revise"]["reason_code"], "conflict");
}

// P1 autonomy (B8): a legacy code submission without a durable candidate ref is
// revised by any agent, because the service can verify the missing ref itself.
#[tokio::test]
async fn legacy_null_candidate_ref_is_revised_by_any_agent() {
    let f = Fixture::new().await;
    let (p, t, integration) = integrating_code_task(&f, "revise-legacy").await;
    let submission = &integration["submission_id"];
    let path = format!(
        "/api/v1/projects/{p}/tasks/{}/workflow/reopen",
        t["id"].as_str().unwrap()
    );
    f.ack(&f.b, &p, 2).await;
    let (status, refused) = f
        .call(
            &f.b,
            "POST",
            &path,
            revise_body(submission, "candidate_missing", None),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    sqlx::query("UPDATE submissions SET candidate_ref=NULL WHERE id=?")
        .bind(submission.as_str().unwrap())
        .execute(&f.state.pool)
        .await
        .unwrap();
    let (status, revised) = f
        .call(
            &f.b,
            "POST",
            &path,
            revise_body(submission, "candidate_missing", None),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{revised}");
    assert_eq!(revised["data"]["phase"], "revision_needed");
}

// P1 autonomy: the fourth agent revise of a subject within 24 hours parks it in
// the human queue; manual recovery reserves revise to humans.
#[tokio::test]
async fn agent_revise_is_rate_limited_and_needs_agent_recovery() {
    let f = Fixture::new().await;
    let p = f
        .project("revise-limit", "https://example.test/limit.git")
        .await;
    f.review_policy(&p, "agent").await;
    let t = f.task(&p, "general", "Withdrawn repeatedly").await;
    let path = format!(
        "/api/v1/projects/{p}/tasks/{}/workflow/reopen",
        t["id"].as_str().unwrap()
    );
    let mut first = Value::Null;
    for round in 0..3 {
        let owner = f.claim(&f.a, &p, &t, 2).await;
        let submitted = f
            .submit(&f.a, &p, &t, &owner, "general", 2, None, None, None, None)
            .await;
        let submission = activity(&submitted, "agent_review")["submission_id"].clone();
        if round == 0 {
            first = submission.clone();
        } else if round == 2 {
            // Agents cannot claim a parked task, so the limit is reached by
            // recording a third agent revise directly, against the first
            // submission, before this round's revise.
            sqlx::query("INSERT INTO events(actor_id,kind,record_id,data_json,created_at) VALUES(?,'submission.reopened',?,'{}',?)")
                .bind(&f.a.principal).bind(first.as_str().unwrap()).bind(f.state.now())
                .execute(&f.state.pool).await.unwrap();
        }
        let (status, result) = f
            .call(
                &f.a,
                "POST",
                &path,
                revise_body(&submission, "author_withdraw", None),
            )
            .await;
        if round < 2 {
            assert_eq!(status, StatusCode::OK, "{result}");
        } else {
            assert_eq!(status, StatusCode::FORBIDDEN, "{result}");
            assert_eq!(result["error"]["code"], "revise_limit_reached");
            assert_eq!(result["error"]["details"]["required_actor"], "human");
        }
    }
    let (status, policy) = f
        .call(&f.admin, "PATCH", &format!("/api/v1/projects/{p}/policy"),
            json!({"expected_revision":2,"review_mode":"agent","recovery_mode":"manual","lease_seconds":600,"rules":"","agent_rule_editing":false,"automatic_integration":true}))
        .await;
    assert_eq!(status, StatusCode::OK, "{policy}");
    let submission: String =
        sqlx::query_scalar("SELECT current_submission_id FROM workflow_subjects WHERE task_id=?")
            .bind(t["id"].as_str().unwrap())
            .fetch_one(&f.state.pool)
            .await
            .unwrap();
    let (status, manual) = f
        .call(
            &f.a,
            "POST",
            &path,
            revise_body(&json!(submission), "author_withdraw", None),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{manual}");
    assert_eq!(manual["error"]["code"], "human_reopen_required");
    assert_eq!(manual["error"]["details"]["required_actor"], "human");
}

/// A general task in `p` at `priority` (lower is more urgent).
async fn task_at(f: &Fixture, p: &str, title: &str, priority: i64) -> Value {
    let body = json!({"title":title,"description":"workflow test","acceptance_criteria":["required behavior verified"],"kind":"general","priority":priority});
    let (status, v) = f
        .call(&f.a, "POST", &format!("/api/v1/projects/{p}/tasks"), body)
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    v["data"].clone()
}

/// Claims and submits general task `t` as `f.a`, then withdraws the
/// submission (an agent revise); returns the withdrawn submission id.
async fn withdrawn_round(f: &Fixture, p: &str, t: &Value) -> Value {
    let path = format!("/api/v1/projects/{p}/tasks/{}", t["id"].as_str().unwrap());
    let (_, current) = f.call(&f.a, "GET", &path, Value::Null).await;
    let t = &current["data"];
    let owner = f.claim(&f.a, p, t, 2).await;
    let submitted = f
        .submit(&f.a, p, t, &owner, "general", 2, None, None, None, None)
        .await;
    let submission = activity(&submitted, "agent_review")["submission_id"].clone();
    let body = revise_body(&submission, "author_withdraw", None);
    let (status, v) = f
        .call(&f.a, "POST", &format!("{path}/workflow/reopen"), body)
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    submission
}

/// Posts an agent work claim as `f.a`: of `t` when given, otherwise of the
/// next eligible task.
async fn work_claim(f: &Fixture, p: &str, t: Option<&Value>) -> (StatusCode, Value) {
    let mut body = json!({"mode":"work","policy_revision":2,"instruction_version":coordinator_core::INSTRUCTION_VERSION});
    if let Some(t) = t {
        body["task_id"] = t["id"].clone();
        body["expected_task_revision"] = t["revision"].clone();
    }
    f.call(&f.a, "POST", &format!("/api/v1/projects/{p}/claims"), body)
        .await
}

// P4 S4d: on a project without the integrator, three agent revises in 24
// hours park the task, and an agent's explicit work claim is refused.
#[tokio::test]
async fn an_agent_cannot_claim_a_task_parked_by_revises() {
    let f = Fixture::new().await;
    let p = f
        .project("revise-claim", "https://example.test/revise-claim.git")
        .await;
    f.review_policy(&p, "agent").await;
    let t = f.task(&p, "general", "Withdrawn three times").await;
    for _ in 0..3 {
        withdrawn_round(&f, &p, &t).await;
    }
    let path = format!("/api/v1/projects/{p}/tasks/{}", t["id"].as_str().unwrap());
    let (_, current) = f.call(&f.a, "GET", &path, Value::Null).await;
    let (status, v) = work_claim(&f, &p, Some(&current["data"])).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["error"]["code"], "revise_limit_reached", "{v}");
    assert_eq!(v["error"]["details"]["required_actor"], "human", "{v}");
}

// P4 S4d: a next-eligible claim pages past any number of parked
// higher-priority tasks to the first eligible one.
#[tokio::test]
async fn a_next_eligible_claim_pages_past_parked_tasks() {
    let f = Fixture::new().await;
    let p = f
        .project("revise-paging", "https://example.test/revise-paging.git")
        .await;
    f.review_policy(&p, "agent").await;
    for n in 0..21 {
        let parked = task_at(&f, &p, &format!("Parked {n}"), 0).await;
        let submission = withdrawn_round(&f, &p, &parked).await;
        for _ in 0..2 {
            sqlx::query("INSERT INTO events(actor_id,kind,record_id,data_json,created_at) VALUES(?,'submission.reopened',?,'{}',?)")
                .bind(&f.a.principal).bind(submission.as_str().unwrap()).bind(f.state.now())
                .execute(&f.state.pool).await.unwrap();
        }
    }
    let eligible = task_at(&f, &p, "Eligible", 3).await;
    let (status, v) = work_claim(&f, &p, None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["claim"]["task"]["id"], eligible["id"], "{v}");
}

/// Change only the project's lease (rules untouched) or only its rules text.
async fn patch_policy(f: &Fixture, p: &str, expected: i64, lease: i64, rules: &str) {
    let (status, v) = f
        .call(&f.admin, "PATCH", &format!("/api/v1/projects/{p}/policy"),
            json!({"expected_revision":expected,"review_mode":"none","recovery_mode":"agent","lease_seconds":lease,"rules":rules,"agent_rule_editing":false,"automatic_integration":true}))
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
}

// P1 autonomy: instruction acks and decisions are keyed on the rules text and the
// judged task fields, so a lease-only change keeps them current and a rules change
// re-pends them.
#[tokio::test]
async fn acks_and_decisions_repend_only_on_rules_changes() {
    let f = Fixture::new().await;
    let p = f.project("ack-scope", "https://example.test/ack.git").await;
    f.policy_none(&p).await;
    let t = f.task(&p, "general", "Scoped decision").await;
    f.ack(&f.a, &p, 2).await;
    let decision = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO decisions(id,project_id,question,options_json,rationale,required_actor,created_by,created_at) VALUES(?,?,'Proceed?','[\"Yes\",\"No\"]','scope','either',?,?)")
        .bind(&decision).bind(&p).bind(&f.admin.principal).bind(f.state.now()).execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO decision_cycles(decision_id,generation,policy_revision,environment,conditions,expires_at,rationale,opened_by,created_at) VALUES(?,1,2,'test','none',NULL,'scope',?,?)")
        .bind(&decision).bind(&f.admin.principal).bind(f.state.now()).execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO decision_affected_tasks(decision_id,generation,project_id,task_id,task_revision) VALUES(?,1,?,?,?)")
        .bind(&decision).bind(&p).bind(t["id"].as_str().unwrap()).bind(t["revision"].as_i64().unwrap()).execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO decision_answers(decision_id,generation,disposition,answer,rationale,actor_id,actor_session_id,conditions_confirmed,created_at) VALUES(?,1,'allow','Yes','ok',?,?,1,?)")
        .bind(&decision).bind(&f.admin.principal).bind(&f.admin.session).bind(f.state.now()).execute(&f.state.pool).await.unwrap();
    let task_path = format!("/api/v1/projects/{p}/tasks/{}", t["id"].as_str().unwrap());
    let decision_path = format!("/api/v1/projects/{p}/decisions/{decision}");
    let needs_ack = |v: &Value| {
        v["data"]["preconditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["code"] == "instructions_required")
    };

    patch_policy(&f, &p, 2, 3600, "").await;
    let (_, detail) = f.call(&f.a, "GET", &task_path, Value::Null).await;
    assert!(!needs_ack(&detail), "{detail}");
    let (_, current) = f.call(&f.a, "GET", &decision_path, Value::Null).await;
    assert_eq!(current["data"]["status"], "allowed", "{current}");

    patch_policy(&f, &p, 3, 3600, "Always run the browser fixture.").await;
    let (_, detail) = f.call(&f.a, "GET", &task_path, Value::Null).await;
    assert!(needs_ack(&detail), "{detail}");
    let (_, stale) = f.call(&f.a, "GET", &decision_path, Value::Null).await;
    assert_eq!(stale["data"]["status"], "stale", "{stale}");
}

/// Submit a general task carrying an acceptance-criteria amendment and let `b`
/// claim its agent review. Returns the task path, review and review claim.
async fn amended_submission(f: &Fixture, name: &str) -> (String, Value, Value) {
    let p = f
        .project(name, &format!("https://example.test/{name}.git"))
        .await;
    f.review_policy(&p, "agent").await;
    let t = f.task(&p, "general", "Amended criteria").await;
    let owner = f.claim(&f.a, &p, &t, 2).await;
    let new = ["required behavior verified", "operator docs updated"];
    let (status, submitted) = f
        .call(&f.a, "POST", &format!("/api/v1/projects/{p}/attempts/{}/submissions", owner["id"].as_str().unwrap()),
            json!({"generation":owner["generation"],"task_revision":t["revision"],"project_policy_revision":2,"workflow_policy_revision":0,"kind":"general",
                "summary":"candidate ready","handoff":"review the amendment",
                "acceptance_evidence":new.iter().map(|c| json!({"criterion":c,"evidence":"verified"})).collect::<Vec<_>>(),
                "ac_amendment":{"old":["required behavior verified"],"new":new,"rationale":"Docs are part of the behaviour."}}))
        .await;
    assert_eq!(status, StatusCode::OK, "{submitted}");
    let review = activity(&submitted["data"], "agent_review").clone();
    let (status, claimed) = f.claim_activity(&f.b, &p, &review, 2, 0).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    (format!("/api/v1/projects/{p}"), review, claimed)
}

fn review_body(claimed: &Value, review: &Value, amendment: Option<&str>) -> Value {
    json!({"generation":claimed["data"]["attempt"]["generation"],"submission_id":review["submission_id"],"decision":"approved","summary":"Reviewed","findings":[],"amendment_decision":amendment})
}

// P1 autonomy: an accepted AC amendment updates the task's criteria; approving
// without deciding the amendment is refused.
#[tokio::test]
async fn accepted_ac_amendment_updates_criteria() {
    let f = Fixture::new().await;
    let (base, review, claimed) = amended_submission(&f, "amend-accept").await;
    let path = format!(
        "{base}/workflow-activities/{}/review",
        review["id"].as_str().unwrap()
    );
    let (status, missing) = f
        .call(&f.b, "POST", &path, review_body(&claimed, &review, None))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{missing}");
    let (status, done) = f
        .call(
            &f.b,
            "POST",
            &path,
            review_body(&claimed, &review, Some("accepted")),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(done["data"]["phase"], "done", "{done}");
    let criteria: String = sqlx::query_scalar(
        "SELECT t.acceptance_json FROM tasks t JOIN submissions s ON s.task_id=t.id WHERE s.id=?",
    )
    .bind(review["submission_id"].as_str().unwrap())
    .fetch_one(&f.state.pool)
    .await
    .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&criteria).unwrap(),
        json!(["required behavior verified", "operator docs updated"])
    );
}

// P1 autonomy: a rejected AC amendment makes the whole submission changes_requested
// and leaves the criteria untouched.
#[tokio::test]
async fn rejected_ac_amendment_requests_changes() {
    let f = Fixture::new().await;
    let (base, review, claimed) = amended_submission(&f, "amend-reject").await;
    let path = format!(
        "{base}/workflow-activities/{}/review",
        review["id"].as_str().unwrap()
    );
    let (status, result) = f
        .call(
            &f.b,
            "POST",
            &path,
            review_body(&claimed, &review, Some("rejected")),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["data"]["phase"], "revision_needed", "{result}");
    let criteria: String = sqlx::query_scalar(
        "SELECT t.acceptance_json FROM tasks t JOIN submissions s ON s.task_id=t.id WHERE s.id=?",
    )
    .bind(review["submission_id"].as_str().unwrap())
    .fetch_one(&f.state.pool)
    .await
    .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&criteria).unwrap(),
        json!(["required behavior verified"])
    );
}

// P3b verdict pipeline (6cf630c0): the reviewer sees the amendment on the
// submission, and the supervisor's approval round-trips with its
// amendment_decision and review_independence recorded on the decision.
#[tokio::test]
async fn supervisor_amendment_approval_round_trips_with_independence() {
    let f = Fixture::new().await;
    let (base, review, claimed) = amended_submission(&f, "amend-verdict").await;
    let subject = review["subject_task_id"].as_str().unwrap();
    let (_, workflow) = f
        .call(
            &f.b,
            "GET",
            &format!("{base}/tasks/{subject}/workflow"),
            Value::Null,
        )
        .await;
    let amendment = &workflow["data"]["submission"]["ac_amendment"];
    assert_eq!(amendment["new"][1], "operator docs updated", "{workflow}");
    let path = format!(
        "{base}/workflow-activities/{}/review",
        review["id"].as_str().unwrap()
    );
    let mut body = review_body(&claimed, &review, Some("accepted"));
    body["review_independence"] = json!("same_launch");
    let (status, refused) = f.call(&f.b, "POST", &path, body.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
    body["review_independence"] = json!("distinct_launch");
    let (status, done) = f.call(&f.b, "POST", &path, body).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    let decided = activity(&done["data"], "agent_review");
    assert_eq!(
        decided["review"]["amendment_decision"], "accepted",
        "{done}"
    );
    assert_eq!(decided["review"]["review_independence"], "distinct_launch");
}

// Review fix: agents with agent_rule_editing may edit rules text but never review or
// recovery mode, so an author cannot loosen review to land its own candidate.
#[tokio::test]
async fn agents_cannot_change_review_or_recovery_mode() {
    let f = Fixture::new().await;
    let p = f
        .project("mode-gate", "https://example.test/mode.git")
        .await;
    let base = json!({"review_mode":"agent","recovery_mode":"agent","lease_seconds":600,"rules":"","agent_rule_editing":true,"automatic_integration":true});
    let mut first = base.clone();
    first["expected_revision"] = json!(1);
    let (status, v) = f
        .call(
            &f.admin,
            "PATCH",
            &format!("/api/v1/projects/{p}/policy"),
            first,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    for (field, value) in [("review_mode", "none"), ("recovery_mode", "manual")] {
        let mut change = base.clone();
        change["expected_revision"] = json!(2);
        change[field] = json!(value);
        let (status, refused) = f
            .call(
                &f.a,
                "PATCH",
                &format!("/api/v1/projects/{p}/policy"),
                change,
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
        assert_eq!(
            refused["error"]["details"]["gate"],
            "policy_permission_change"
        );
    }
    let mut rules = base.clone();
    rules["expected_revision"] = json!(2);
    rules["rules"] = json!("Run the browser fixture.");
    let (status, v) = f
        .call(
            &f.a,
            "PATCH",
            &format!("/api/v1/projects/{p}/policy"),
            rules,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
}

// Review fix: an amendment needs a reviewer; review_mode none refuses it.
#[tokio::test]
async fn ac_amendment_without_reviewers_is_refused() {
    let f = Fixture::new().await;
    let p = f
        .project("amend-none", "https://example.test/amend-none.git")
        .await;
    f.policy_none(&p).await;
    let t = f.task(&p, "general", "Unreviewed amendment").await;
    let owner = f.claim(&f.a, &p, &t, 2).await;
    let (status, refused) = f
        .call(&f.a, "POST", &format!("/api/v1/projects/{p}/attempts/{}/submissions", owner["id"].as_str().unwrap()),
            json!({"generation":owner["generation"],"task_revision":t["revision"],"project_policy_revision":2,"workflow_policy_revision":0,"kind":"general",
                "summary":"s","handoff":"h","acceptance_evidence":[{"criterion":"easier","evidence":"e"}],
                "ac_amendment":{"old":["required behavior verified"],"new":["easier"],"rationale":"r"}}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["error"]["code"], "amendment_review_required");
}

// Review fix: a review no longer required after a mode change, released after the
// subject advanced, never gates integration.
#[tokio::test]
async fn leftover_review_does_not_gate_integration() {
    let f = Fixture::new().await;
    let repo = "https://example.test/leftover.git";
    let p = f.project("leftover", repo).await;
    f.review_policy(&p, "human").await;
    f.workflow_policy(&p, repo).await;
    let t = f.task(&p, "code", "Leftover review").await;
    let owner = f.claim(&f.a, &p, &t, 2).await;
    let base = "1111111111111111111111111111111111111111";
    f.checkout(&f.a, &p, &owner, base).await;
    let submitted = f
        .submit(
            &f.a,
            &p,
            &t,
            &owner,
            "code",
            2,
            Some(repo),
            Some(base),
            Some("2222222222222222222222222222222222222222"),
            Some("3333333333333333333333333333333333333333"),
        )
        .await;
    let human_review = activity(&submitted, "human_review").clone();
    let integration = activity(&submitted, "integration").clone();
    let (status, held) = f.claim_activity(&f.admin, &p, &human_review, 2, 1).await;
    assert_eq!(status, StatusCode::OK, "{held}");
    let (status, v) = f
        .call(&f.admin, "PATCH", &format!("/api/v1/projects/{p}/policy"),
            json!({"expected_revision":2,"review_mode":"agent","recovery_mode":"agent","lease_seconds":600,"rules":"","agent_rule_editing":false,"automatic_integration":true}))
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (_, wf) = f
        .call(
            &f.b,
            "GET",
            &format!(
                "/api/v1/projects/{p}/tasks/{}/workflow",
                t["id"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
    let agent_review = activity(&wf["data"], "agent_review").clone();
    f.ack(&f.b, &p, 3).await;
    f.ack(&f.c, &p, 3).await;
    let claim = |a: &Value| json!({"expected_submission_id":a["submission_id"],"expected_project_policy_revision":2,"expected_workflow_policy_revision":1});
    let claim_path = |a: &Value| {
        format!(
            "/api/v1/projects/{p}/workflow-activities/{}/claim",
            a["id"].as_str().unwrap()
        )
    };
    let (status, claimed) = f
        .call(
            &f.b,
            "POST",
            &claim_path(&agent_review),
            claim(&agent_review),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    let (status, decided) = f
        .call(&f.b, "POST", &format!("/api/v1/projects/{p}/workflow-activities/{}/review", agent_review["id"].as_str().unwrap()),
            json!({"generation":claimed["data"]["attempt"]["generation"],"submission_id":agent_review["submission_id"],"decision":"approved","summary":"ok","findings":[]}))
        .await;
    assert_eq!(status, StatusCode::OK, "{decided}");
    assert_eq!(decided["data"]["phase"], "integration");
    let attempt = &held["data"]["attempt"];
    let (status, released) = f
        .call(&f.admin, "POST", &format!("/api/v1/projects/{p}/workflow-activities/{}/release", human_review["id"].as_str().unwrap()),
            json!({"generation":attempt["generation"],"summary":"No longer required","blocked":false}))
        .await;
    assert_eq!(status, StatusCode::OK, "{released}");
    let (status, integrating) = f
        .call(&f.c, "POST", &claim_path(&integration), claim(&integration))
        .await;
    assert_eq!(status, StatusCode::OK, "{integrating}");
}

/// Reads `next` for `role` as `c`.
async fn next_action(f: &Fixture, c: &Caller, p: &str, role: &str) -> Value {
    let path = format!("/api/v1/projects/{p}/next?role={role}");
    let (status, value) = f.call(c, "GET", &path, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value["data"].clone()
}

/// Follows a `next` call template exactly, after acknowledging instructions.
async fn follow(f: &Fixture, c: &Caller, p: &str, offered: &Value) -> Value {
    f.ack(c, p, 1).await;
    let call = &offered["action"]["call"];
    let path = call["path"].as_str().unwrap();
    let (status, value) = f.call(c, "POST", path, call["body"].clone()).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value["data"].clone()
}

#[tokio::test]
async fn next_offers_a_claim_template_and_an_independent_review() {
    let f = Fixture::new().await;
    let p = f
        .project("next-action", "https://example.test/next.git")
        .await;
    let t = f.task(&p, "general", "Write the guide").await;
    let offered = next_action(&f, &f.a, &p, "implementer").await;
    assert_eq!(offered["action"]["kind"], "claim_task", "{offered}");
    assert_eq!(offered["action"]["task_id"], t["id"]);
    assert_eq!(offered["caller_steps"][0]["code"], "instructions_required");
    let owner = follow(&f, &f.a, &p, &offered).await["claim"]["attempt"].clone();
    let idle = next_action(&f, &f.b, &p, "implementer").await;
    assert!(idle["action"].is_null(), "{idle}");
    assert_eq!(idle["retry_after_seconds"], 30);
    f.submit(&f.a, &p, &t, &owner, "general", 1, None, None, None, None)
        .await;
    let author = next_action(&f, &f.a, &p, "reviewer").await;
    assert!(author["action"].is_null(), "{author}");
    assert_eq!(author["inspected"], 1);
    let review = next_action(&f, &f.b, &p, "reviewer").await;
    assert_eq!(review["action"]["kind"], "claim_review", "{review}");
    assert_eq!(review["action"]["subject_task_id"], t["id"]);
    let claimed = follow(&f, &f.b, &p, &review).await;
    assert_eq!(claimed["attempt"]["state"], "active", "{claimed}");
}

#[tokio::test]
async fn next_serves_read_only_credentials_and_routes_human_gates() {
    let f = Fixture::new().await;
    let p = f
        .project("next-recovery", "https://example.test/next.git")
        .await;
    let t = f.task(&p, "general", "Stranded work").await;
    f.claim(&f.a, &p, &t, 1).await;
    f.clock.0.fetch_add(3_600_000, Ordering::SeqCst);
    sqlx::query("UPDATE credentials SET access='read' WHERE id=?")
        .bind(&f.b.credential)
        .execute(&f.state.pool)
        .await
        .unwrap();
    let recover = next_action(&f, &f.b, &p, "implementer").await;
    assert_eq!(recover["action"]["kind"], "recover_task", "{recover}");
    assert_eq!(recover["action"]["call"]["body"]["mode"], "recovery");
    let (status, v) = f.call(&f.admin, "PATCH", &format!("/api/v1/projects/{p}/policy"), json!({"expected_revision":1,"review_mode":"agent","recovery_mode":"manual","lease_seconds":600,"rules":"","agent_rule_editing":false,"automatic_integration":true})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let gated = next_action(&f, &f.b, &p, "implementer").await;
    assert!(gated["action"].is_null(), "{gated}");
    assert_eq!(gated["human_queue"], 1);
    assert_eq!(gated["skipped"]["human_recovery_required"], 1);
    let path = format!("/api/v1/projects/{p}/next?role=integrator");
    let (status, _) = f.call(&f.b, "GET", &path, Value::Null).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "integrator role needs the class"
    );
    let path = format!("/api/v1/projects/{p}/next?role=bogus");
    let (status, _) = f.call(&f.b, "GET", &path, Value::Null).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn next_is_not_starved_by_blocked_higher_priority_tasks() {
    let f = Fixture::new().await;
    let p = f
        .project("next-starve", "https://example.test/next.git")
        .await;
    let prerequisite = f.task(&p, "general", "Prerequisite").await;
    let ready = f.task(&p, "general", "Ready work").await;
    for (task, priority) in [(&prerequisite, 3), (&ready, 2)] {
        sqlx::query("UPDATE tasks SET priority=? WHERE id=?")
            .bind(priority)
            .bind(task["id"].as_str().unwrap())
            .execute(&f.state.pool)
            .await
            .unwrap();
    }
    for n in 0..51 {
        let blocked = f.task(&p, "general", &format!("Blocked {n}")).await;
        sqlx::query("UPDATE tasks SET priority=0 WHERE id=?")
            .bind(blocked["id"].as_str().unwrap())
            .execute(&f.state.pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO task_dependencies(project_id,task_id,prerequisite_id) VALUES(?,?,?)",
        )
        .bind(&p)
        .bind(blocked["id"].as_str().unwrap())
        .bind(prerequisite["id"].as_str().unwrap())
        .execute(&f.state.pool)
        .await
        .unwrap();
    }
    let offered = next_action(&f, &f.b, &p, "implementer").await;
    assert_eq!(offered["action"]["task_id"], ready["id"], "{offered}");
}

/// An agent caller whose credential is switched to the integrator class.
async fn integrator_caller(f: &Fixture) -> Caller {
    let c = seed(&f.state, false, "integrator").await;
    sqlx::query("UPDATE credentials SET class='integrator' WHERE id=?")
        .bind(&c.credential)
        .execute(&f.state.pool)
        .await
        .unwrap();
    c
}

/// Patches the owner at `expected` with agent rule editing on and every other
/// field unchanged from `policy_none`.
async fn patch_owner(
    f: &Fixture,
    c: &Caller,
    p: &str,
    expected: i64,
    owner: &str,
) -> (StatusCode, Value) {
    f.call(c, "PATCH", &format!("/api/v1/projects/{p}/policy"), json!({"expected_revision":expected,"review_mode":"none","recovery_mode":"agent","lease_seconds":600,"rules":"","agent_rule_editing":true,"automatic_integration":true,"integration_owner":owner})).await
}

/// Hands a project's integration to the integrator (policy revision 2 → 3).
async fn set_integration_owner(
    f: &Fixture,
    c: &Caller,
    p: &str,
    owner: &str,
) -> (StatusCode, Value) {
    patch_owner(f, c, p, 2, owner).await
}

/// The fixture roster check's GitHub check-run name and workflow blob.
const CHECK: &str = "Linux format, Clippy, and workspace tests";
const BLOB: &str = "abababababababababababababababababababab";

/// A result body for the fixture candidate at target tip `t0`.
fn result_body(submission: &str, t0: &str, r: &str) -> Value {
    json!({"submission_id":submission,"t0":t0,"t0_tree":"4444444444444444444444444444444444444444","c":"2222222222222222222222222222222222222222","r":r,"r_tree":"6666666666666666666666666666666666666666","landing_range":["2222222222222222222222222222222222222222"],"roster":roster()})
}

/// The roster the fixture integrator reads from T0: the protected check.
fn roster() -> Value {
    json!({"revision":1,"required_checks":[{"identity":"workspace-tests","check_name":CHECK,"workflow_path":".github/workflows/checks.yml","workflow_blob":BLOB}]})
}

#[tokio::test]
async fn integrator_queue_is_owner_switched_and_class_scoped() {
    let f = Fixture::new().await;
    let (p, t, _) = integrating_code_task(&f, "integrator-queue").await;
    let i = integrator_caller(&f).await;
    let queue = format!("/api/v1/projects/{p}/integrator/queue");
    let (status, refused) = f.call(&i, "GET", &queue, json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["error"]["code"], "integration_owned_by_agents");
    sqlx::query("UPDATE credentials SET access='read' WHERE id=?")
        .bind(&i.credential)
        .execute(&f.state.pool)
        .await
        .unwrap();
    let (status, _) = f
        .call(
            &i,
            "GET",
            "/api/v1/projects/missing/integrator/queue?shadow=true",
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let shadow = format!("{queue}?shadow=true");
    let (status, v) = f.call(&i, "GET", &shadow, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["items"][0]["subject_task_id"], t["id"]);
    let seen: Option<i64> =
        sqlx::query_scalar("SELECT integrator_last_seen FROM projects WHERE id=?")
            .bind(&p)
            .fetch_one(&f.state.pool)
            .await
            .unwrap();
    assert_eq!(seen, None, "shadow must not advertise a live integrator");
    let (status, _) = f.call(&f.a, "GET", &shadow, json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    sqlx::query("UPDATE credentials SET access='write' WHERE id=?")
        .bind(&i.credential)
        .execute(&f.state.pool)
        .await
        .unwrap();
    let (status, v) = set_integration_owner(&f, &f.admin, &p, "agent").await;
    assert_eq!(status, StatusCode::OK, "delegate rule editing only: {v}");
    let (status, v) = patch_owner(&f, &f.a, &p, 3, "integrator").await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "agents cannot switch the owner: {v}"
    );
    assert_eq!(v["error"]["details"]["gate"], "policy_permission_change");
    let (status, v) = patch_owner(&f, &f.admin, &p, 3, "integrator").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["integration_owner"], "integrator");
    let (status, v) = f.call(&f.a, "GET", &queue, json!({})).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "agents cannot read the queue: {v}"
    );
    let (status, v) = f.call(&i, "GET", &queue, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let items = v["data"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["subject_task_id"], t["id"]);
    let target = json!({"repository_url": items[0]["repository_url"], "target_branch": "main"});
    assert_eq!(v["data"]["targets"], json!([target]), "one target per pair");
    assert_eq!(
        items[0]["reviewed_base"],
        "1111111111111111111111111111111111111111"
    );
    assert_eq!(
        v["data"]["roster"]["required_checks"][0]["identity"],
        "workspace-tests"
    );
    let seen: Option<i64> =
        sqlx::query_scalar("SELECT integrator_last_seen FROM projects WHERE id=?")
            .bind(&p)
            .fetch_one(&f.state.pool)
            .await
            .unwrap();
    assert_eq!(seen, Some(f.state.now()));
    let (status, v) = f
        .call(
            &i,
            "POST",
            &format!("/api/v1/projects/{p}/tasks"),
            json!({"title":"x","description":"x","acceptance_criteria":["x"],"kind":"general"}),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "integrator is scoped to its API: {v}"
    );
}

#[tokio::test]
async fn integrator_queue_lists_targets_with_no_items() {
    let f = Fixture::new().await;
    let repo = "https://example.test/idle-target.git";
    let p = f.project("idle-target", repo).await;
    f.policy_none(&p).await;
    let (status, v) = set_integration_owner(&f, &f.admin, &p, "integrator").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let i = integrator_caller(&f).await;
    let queue = format!("/api/v1/projects/{p}/integrator/queue");
    let (status, v) = f.call(&i, "GET", &queue, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["items"], json!([]));
    assert_eq!(
        v["data"]["targets"],
        json!([{"repository_url": repo, "target_branch": "main"}])
    );
}

#[tokio::test]
async fn integrator_results_pin_one_r_per_submission_and_tip() {
    let f = Fixture::new().await;
    let (p, _, integration) = integrating_code_task(&f, "integrator-results").await;
    set_integration_owner(&f, &f.admin, &p, "integrator").await;
    let i = integrator_caller(&f).await;
    let submission = integration["submission_id"].as_str().unwrap();
    let path = format!("/api/v1/projects/{p}/integrator/results");
    let t0 = "7777777777777777777777777777777777777777";
    let r = "5555555555555555555555555555555555555555";
    let (status, first) = f
        .call(&i, "POST", &path, result_body(submission, t0, r))
        .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let (status, again) = f
        .call(&i, "POST", &path, result_body(submission, t0, r))
        .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(
        again["data"]["id"], first["data"]["id"],
        "same key replays the pinned result"
    );
    let (status, v) = f
        .call(
            &i,
            "POST",
            &path,
            result_body(submission, t0, "8888888888888888888888888888888888888888"),
        )
        .await;
    assert_eq!(
        (status, v["error"]["code"].clone()),
        (StatusCode::CONFLICT, json!("result_conflict"))
    );
    let mut stale = result_body(submission, t0, r);
    stale["c"] = json!("9999999999999999999999999999999999999999");
    let (status, v) = f.call(&i, "POST", &path, stale).await;
    assert_eq!(
        (status, v["error"]["code"].clone()),
        (StatusCode::CONFLICT, json!("candidate_changed"))
    );
    let (status, v) = f
        .call(&f.a, "POST", &path, result_body(submission, t0, r))
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "only the integrator records results: {v}"
    );
    let class: String = sqlx::query_scalar(
        "SELECT credential_class FROM events WHERE kind='integrator.result_recorded' LIMIT 1",
    )
    .fetch_one(&f.state.pool)
    .await
    .unwrap();
    assert_eq!(class, "integrator");
    let (_, queue) = f
        .call(
            &i,
            "GET",
            &format!("/api/v1/projects/{p}/integrator/queue"),
            json!({}),
        )
        .await;
    assert_eq!(queue["data"]["items"][0]["results"][0]["r"], r);
}

#[tokio::test]
async fn integrator_receipts_bind_to_r_and_the_latest_attempt_decides() {
    let f = Fixture::new().await;
    let (p, _, integration) = integrating_code_task(&f, "integrator-receipts").await;
    set_integration_owner(&f, &f.admin, &p, "integrator").await;
    let i = integrator_caller(&f).await;
    let r = "5555555555555555555555555555555555555555";
    let body = result_body(
        integration["submission_id"].as_str().unwrap(),
        "7777777777777777777777777777777777777777",
        r,
    );
    let (_, result) = f
        .call(
            &i,
            "POST",
            &format!("/api/v1/projects/{p}/integrator/results"),
            body,
        )
        .await;
    let receipt = |attempt: i64, head: &str, conclusion: &str| json!({"result_id":result["data"]["id"],"check_name":"Linux format, Clippy, and workspace tests","run_id":900,"run_attempt":attempt,"head_sha":head,"app_id":15368,"workflow_path":".github/workflows/checks.yml","workflow_blob":"abababababababababababababababababababab","conclusion":conclusion});
    let path = format!("/api/v1/projects/{p}/integrator/receipts");
    let (status, v) = f
        .call(
            &i,
            "POST",
            &path,
            receipt(1, "7777777777777777777777777777777777777777", "success"),
        )
        .await;
    assert_eq!(
        (status, v["error"]["code"].clone()),
        (StatusCode::CONFLICT, json!("receipt_head_mismatch"))
    );
    let (status, v) = f.call(&i, "POST", &path, receipt(1, r, "failure")).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (status, v) = f.call(&i, "POST", &path, receipt(2, r, "success")).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        v["data"]["deciding_run"],
        json!({"run_id":900,"run_attempt":2,"conclusion":"success"})
    );
    let (status, v) = f.call(&i, "POST", &path, receipt(2, r, "failure")).await;
    assert_eq!(
        (status, v["error"]["code"].clone()),
        (StatusCode::CONFLICT, json!("receipt_conflict"))
    );
    let (status, v) = f.call(&i, "POST", &path, receipt(2, r, "success")).await;
    assert_eq!(status, StatusCode::OK, "an identical attempt replays: {v}");
}

#[tokio::test]
async fn integrator_migration_keeps_credentials_events_and_sequence() {
    let dir = tempfile::tempdir().unwrap();
    let migrations = dir.path().join("schema23");
    std::fs::create_dir(&migrations).unwrap();
    for m in sqlx::migrate!("./migrations")
        .iter()
        .filter(|m| m.version <= 23)
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
    sqlx::query("INSERT INTO principals(id,name,kind,role,created_at) VALUES('p1','old-agent','agent','agent',1)").execute(&mut old).await.unwrap();
    sqlx::query("INSERT INTO credentials(rowid,id,principal_id,token_hash,created_at,name,class,access) VALUES(42,'c1','p1','h1',1,'shadow','supervised','read')").execute(&mut old).await.unwrap();
    for seq in 1..=3 {
        sqlx::query("INSERT INTO events(project_id,actor_id,kind,record_id,data_json,created_at,credential_class) VALUES(NULL,'p1','old.event',?,'{}',1,'supervised')").bind(seq.to_string()).execute(&mut old).await.unwrap();
    }
    sqlx::query("DELETE FROM events WHERE seq=3")
        .execute(&mut old)
        .await
        .unwrap();
    old.close().await.unwrap();
    let state = AppState::open(Config {
        database_path: database,
        public_origin: "http://127.0.0.1:8080".into(),
        allow_insecure_loopback: true,
        ..Config::default()
    })
    .await
    .unwrap();
    let row = sqlx::query("SELECT rowid,class,access,name FROM credentials WHERE id='c1'")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        (
            row.get::<i64, _>("rowid"),
            row.get::<String, _>("class"),
            row.get::<String, _>("access")
        ),
        (42, "supervised".into(), "read".into())
    );
    assert_eq!(row.get::<String, _>("name"), "shadow");
    sqlx::query("INSERT INTO events(project_id,actor_id,kind,record_id,data_json,created_at,credential_class) VALUES(NULL,'p1','new.event','n','{}',2,'integrator')").execute(&state.pool).await.unwrap();
    let seqs: Vec<i64> = sqlx::query_scalar("SELECT seq FROM events ORDER BY seq")
        .fetch_all(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        seqs,
        vec![1, 2, 4],
        "a deleted sequence number is never reused"
    );
    let owner: String = sqlx::query_scalar("SELECT integration_owner FROM projects LIMIT 1")
        .fetch_optional(&state.pool)
        .await
        .unwrap()
        .unwrap_or_else(|| "agent".into());
    assert_eq!(owner, "agent");
}

const T0: &str = "7777777777777777777777777777777777777777";
const R: &str = "5555555555555555555555555555555555555555";

/// An integrating code task on a project handed to the integrator, with the
/// integrator's caller.
async fn integrator_task(f: &Fixture, name: &str) -> (String, Value, Value, Caller) {
    let (p, t, integration) = integrating_code_task(f, name).await;
    let (status, v) = set_integration_owner(f, &f.admin, &p, "integrator").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    (p, t, integration, integrator_caller(f).await)
}

/// The path of one workflow activity route.
fn activity_path(p: &str, a: &Value, route: &str) -> String {
    format!(
        "/api/v1/projects/{p}/workflow-activities/{}/{route}",
        a["id"].as_str().unwrap()
    )
}

/// A claim body pinned to the fixture submission's revisions (2 and 1).
fn activity_claim(a: &Value) -> Value {
    json!({"expected_submission_id":a["submission_id"],"expected_project_policy_revision":2,"expected_workflow_policy_revision":1})
}

/// Posts as the integrator to `/integrator/{route}`.
async fn integrator_post(
    f: &Fixture,
    i: &Caller,
    p: &str,
    route: &str,
    body: Value,
) -> (StatusCode, Value) {
    let path = format!("/api/v1/projects/{p}/integrator/{route}");
    f.call(i, "POST", &path, body).await
}

/// Records the result for (submission, t0) and returns its id.
async fn pinned_result(f: &Fixture, i: &Caller, p: &str, submission: &Value, t0: &str) -> String {
    let body = result_body(submission.as_str().unwrap(), t0, R);
    let (status, v) = integrator_post(f, i, p, "results", body).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    v["data"]["id"].as_str().unwrap().to_owned()
}

/// Posts one receipt for the roster check on R.
async fn receipt(f: &Fixture, i: &Caller, p: &str, result: &str, run: i64, conclusion: &str) {
    let body = json!({"result_id":result,"check_name":CHECK,"run_id":run,"run_attempt":1,"head_sha":R,"app_id":15368,"workflow_path":".github/workflows/checks.yml","workflow_blob":BLOB,"conclusion":conclusion});
    let (status, v) = integrator_post(f, i, p, "receipts", body).await;
    assert_eq!(status, StatusCode::OK, "{v}");
}

/// Asks for push authority on `result`.
async fn authority(f: &Fixture, i: &Caller, p: &str, result: &str) -> (StatusCode, Value) {
    integrator_post(f, i, p, "push-authority", json!({"result_id":result})).await
}

/// Pins R at `t0`, passes its check and takes push authority.
async fn authorized(f: &Fixture, i: &Caller, p: &str, submission: &Value, t0: &str) -> String {
    let result = pinned_result(f, i, p, submission, t0).await;
    receipt(f, i, p, &result, 900, "success").await;
    let (status, v) = authority(f, i, p, &result).await;
    assert_eq!(
        (status, v["data"]["granted"].clone()),
        (StatusCode::OK, json!(true)),
        "{v}"
    );
    result
}

/// Posts an observation of `tip` for `result`.
async fn observe(
    f: &Fixture,
    i: &Caller,
    p: &str,
    result: &str,
    tip: &str,
    ancestry: &str,
) -> (StatusCode, Value) {
    let body = json!({"result_id":result,"tip":tip,"ancestry":ancestry,"evidence":"git ls-remote"});
    integrator_post(f, i, p, "observations", body).await
}

/// `authority_expires_at` of the queue head's first result.
async fn queued_authority(f: &Fixture, i: &Caller, p: &str) -> Value {
    let (status, v) = f
        .call(
            i,
            "GET",
            &format!("/api/v1/projects/{p}/integrator/queue"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    v["data"]["items"][0]["results"][0]["authority_expires_at"].clone()
}

/// The number of held integration holds in the project.
async fn held_holds(f: &Fixture, p: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM integration_holds h JOIN workflow_activities a ON a.id=h.activity_id WHERE a.project_id=? AND h.state='held'")
        .bind(p).fetch_one(&f.state.pool).await.unwrap()
}

/// The subject's workflow phase and task lifecycle.
async fn subject_state(f: &Fixture, t: &Value) -> (String, String) {
    sqlx::query_as("SELECT ws.phase,t.lifecycle FROM workflow_subjects ws JOIN tasks t ON t.id=ws.task_id WHERE ws.task_id=?")
        .bind(t["id"].as_str().unwrap()).fetch_one(&f.state.pool).await.unwrap()
}

// P4 S2: on an integrator-owned project the LLM integration path is refused
// with a labelled gate, and `next` serves the integrator its queue head.
#[tokio::test]
async fn integrator_projects_refuse_the_agent_integration_path() {
    let f = Fixture::new().await;
    let (p, t, integration, i) = integrator_task(&f, "integrator-refusal").await;
    let (status, v) = f
        .call(
            &f.c,
            "POST",
            &activity_path(&p, &integration, "claim"),
            activity_claim(&integration),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["error"]["code"], "integration_owned_by_integrator");
    assert_eq!(v["error"]["details"]["required_actor"], "integrator");
    let path = format!(
        "/api/v1/projects/{p}/workflow-activities/{}/authorization",
        integration["id"].as_str().unwrap()
    );
    let (status, v) = f
        .call(&f.admin, "POST", &path, json!({"submission_id":integration["submission_id"],"expected_project_policy_revision":2,"expected_workflow_policy_revision":1,"summary":"ship it"}))
        .await;
    assert_eq!(
        (status, v["error"]["code"].clone()),
        (
            StatusCode::CONFLICT,
            json!("integration_owned_by_integrator")
        )
    );
    let next = next_action(&f, &i, &p, "integrator").await;
    assert_eq!(next["action"]["kind"], "integrate", "{next}");
    assert_eq!(next["action"]["subject_task_id"], t["id"]);
}

// P4 S2: push authority needs a deciding success receipt for every roster
// check, and a roster that keeps every protected check.
#[tokio::test]
async fn push_authority_needs_passing_checks_and_the_protected_roster() {
    let f = Fixture::new().await;
    let (p, _, integration, i) = integrator_task(&f, "integrator-authority").await;
    let submission = &integration["submission_id"];
    let result = pinned_result(&f, &i, &p, submission, T0).await;
    let (status, v) = authority(&f, &i, &p, &result).await;
    assert_eq!(
        (status, v["error"]["code"].clone()),
        (StatusCode::CONFLICT, json!("checks_not_passed"))
    );
    assert_eq!(v["error"]["details"]["pending"], json!(["workspace-tests"]));
    receipt(&f, &i, &p, &result, 900, "failure").await;
    let (_, v) = authority(&f, &i, &p, &result).await;
    assert_eq!(
        v["error"]["details"]["failed"],
        json!(["workspace-tests"]),
        "{v}"
    );
    receipt(&f, &i, &p, &result, 901, "success").await;
    let (status, v) = authority(&f, &i, &p, &result).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["deciding_runs"][0]["run_id"], 901);
    assert_eq!(v["data"]["protected_ids"], json!(["workspace-tests"]));
    assert_eq!(held_holds(&f, &p).await, 1);
    observe(&f, &i, &p, &result, T0, "equal_t0").await;
    let mut dropped = result_body(
        submission.as_str().unwrap(),
        "8888888888888888888888888888888888888888",
        R,
    );
    dropped["roster"]["required_checks"][0]["identity"] = json!("renamed");
    let (status, v) = integrator_post(&f, &i, &p, "results", dropped).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let other = v["data"]["id"].as_str().unwrap().to_owned();
    let (_, v) = authority(&f, &i, &p, &other).await;
    assert_eq!(v["error"]["code"], "protected_check_missing", "{v}");
}

// P4 S2: roll-forward. Not published and target moved release the hold; a
// contained R completes the subject.
#[tokio::test]
async fn observations_roll_forward_until_r_is_contained() {
    let f = Fixture::new().await;
    let (p, t, integration, i) = integrator_task(&f, "integrator-observe").await;
    let submission = &integration["submission_id"];
    let result = authorized(&f, &i, &p, submission, T0).await;
    assert!(
        queued_authority(&f, &i, &p).await.is_string(),
        "held authority is visible"
    );
    let (status, v) = observe(&f, &i, &p, &result, R, "equal_t0").await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "tip contradicts ancestry: {v}"
    );
    let (status, v) = observe(&f, &i, &p, &result, T0, "equal_t0").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["disposition"], "not_published");
    assert_eq!(held_holds(&f, &p).await, 0);
    assert!(
        queued_authority(&f, &i, &p).await.is_null(),
        "observation ends authority"
    );
    let (_, v) = authority(&f, &i, &p, &result).await;
    assert_eq!(v["data"]["granted"], true, "authority is re-issued: {v}");
    let moved = "9999999999999999999999999999999999999999";
    let (_, v) = observe(&f, &i, &p, &result, moved, "moved").await;
    assert_eq!(v["data"]["disposition"], "target_moved", "{v}");
    let next = authorized(&f, &i, &p, submission, moved).await;
    let (_, v) = observe(
        &f,
        &i,
        &p,
        &next,
        "abcdefabcdefabcdefabcdefabcdefabcdefabcd",
        "contained",
    )
    .await;
    assert_eq!(v["data"]["disposition"], "published", "{v}");
    assert_eq!(subject_state(&f, &t).await, ("done".into(), "done".into()));
    assert_eq!(held_holds(&f, &p).await, 0);
    let (status, v) = observe(&f, &i, &p, &next, moved, "moved").await;
    assert_eq!(
        (status, v["error"]["code"].clone()),
        (StatusCode::CONFLICT, json!("result_already_published"))
    );
}

// P4 S2: a revise during an outstanding push is deferred; it becomes a
// follow-up (a revert for author_withdraw) when the push lands, and applies
// when it does not.
#[tokio::test]
async fn revise_loses_to_a_landed_push() {
    let f = Fixture::new().await;
    let (p, t, integration, i) = integrator_task(&f, "integrator-revise-loses").await;
    let submission = &integration["submission_id"];
    let result = authorized(&f, &i, &p, submission, T0).await;
    f.ack(&f.a, &p, 3).await;
    let path = format!(
        "/api/v1/projects/{p}/tasks/{}/workflow/reopen",
        t["id"].as_str().unwrap()
    );
    let (status, v) = f
        .call(
            &f.a,
            "POST",
            &path,
            revise_body(submission, "author_withdraw", None),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["revise_deferred"], true);
    let (_, v) = observe(&f, &i, &p, &result, R, "contained").await;
    let follow_up = v["data"]["revise"]["follow_up_task_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (title, priority): (String, i64) =
        sqlx::query_as("SELECT title,priority FROM tasks WHERE id=?")
            .bind(&follow_up)
            .fetch_one(&f.state.pool)
            .await
            .unwrap();
    assert_eq!(
        (title.as_str(), priority),
        ("Revert: Conflicting candidate", 0)
    );
    assert_eq!(subject_state(&f, &t).await.0, "done");

    let f = Fixture::new().await;
    let (p, t, integration, i) = integrator_task(&f, "integrator-revise-applies").await;
    let submission = &integration["submission_id"];
    let result = authorized(&f, &i, &p, submission, T0).await;
    f.ack(&f.a, &p, 3).await;
    let path = format!(
        "/api/v1/projects/{p}/tasks/{}/workflow/reopen",
        t["id"].as_str().unwrap()
    );
    let (_, v) = f
        .call(
            &f.a,
            "POST",
            &path,
            revise_body(submission, "author_withdraw", None),
        )
        .await;
    assert_eq!(v["data"]["revise_deferred"], true, "{v}");
    let (_, v) = observe(&f, &i, &p, &result, T0, "equal_t0").await;
    assert_eq!(v["data"]["revise"]["resolution"], "applied", "{v}");
    assert_eq!(subject_state(&f, &t).await.0, "revision_needed");
}

// P4 S2: the integrator revises a conflicting candidate itself, but only
// after observing any push it was authorized to make.
#[tokio::test]
async fn integrator_revise_waits_for_its_own_observation() {
    let f = Fixture::new().await;
    let (p, t, integration, i) = integrator_task(&f, "integrator-revise").await;
    let submission = &integration["submission_id"];
    let body = json!({"submission_id":submission,"reason_code":"conflict","evidence":"CONFLICT (content): src/lib.rs"});
    let (status, v) = integrator_post(
        &f,
        &i,
        &p,
        "revise",
        json!({"submission_id":submission,"reason_code":"author_withdraw","evidence":"x"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    let result = authorized(&f, &i, &p, submission, T0).await;
    let (status, v) = integrator_post(&f, &i, &p, "revise", body.clone()).await;
    assert_eq!(
        (status, v["error"]["code"].clone()),
        (StatusCode::CONFLICT, json!("observation_required"))
    );
    observe(&f, &i, &p, &result, T0, "equal_t0").await;
    let (status, v) = integrator_post(&f, &i, &p, "revise", body).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(subject_state(&f, &t).await.0, "revision_needed");
}

/// Submits a code task by `author` in an agent-review project and has
/// `reviewer` approve it; returns the task and its submission id.
async fn approved_code_task(
    f: &Fixture,
    p: &str,
    repo: &str,
    (author, reviewer): (&Caller, &Caller),
    (title, candidate): (&str, &str),
) -> (Value, Value) {
    let t = f.task(p, "code", title).await;
    let owner = f.claim(author, p, &t, 2).await;
    let base = "1111111111111111111111111111111111111111";
    f.checkout(author, p, &owner, base).await;
    let submitted = f
        .submit(
            author,
            p,
            &t,
            &owner,
            "code",
            2,
            Some(repo),
            Some(base),
            Some(candidate),
            Some("3333333333333333333333333333333333333333"),
        )
        .await;
    let review = activity(&submitted, "agent_review").clone();
    let (status, claimed) = f.claim_activity(reviewer, p, &review, 2, 1).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    let path = format!(
        "/api/v1/projects/{p}/workflow-activities/{}/review",
        review["id"].as_str().unwrap()
    );
    let (status, v) = f.call(reviewer, "POST", &path, json!({"generation":claimed["data"]["attempt"]["generation"],"submission_id":review["submission_id"],"decision":"approved","summary":"ok","findings":[]})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    (t, review["submission_id"].clone())
}

// P4 S2 (plan-final §2.2 5a): an approver who contributed to another task
// whose commits the landing range carries sends the subject back to review,
// and cannot claim the replacement review; unapproved stacked work is refused.
#[tokio::test]
async fn contributor_approvals_and_unapproved_stacks_block_authority() {
    let f = Fixture::new().await;
    let repo = "https://example.test/integrator-stack.git";
    let p = f.project("integrator-stack", repo).await;
    f.review_policy(&p, "agent").await;
    f.workflow_policy(&p, repo).await;
    let below = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
    approved_code_task(&f, &p, repo, (&f.b, &f.c), ("Stacked below", below)).await;
    let (t, submission) = approved_code_task(
        &f,
        &p,
        repo,
        (&f.a, &f.b),
        ("Stacked above", "2222222222222222222222222222222222222222"),
    )
    .await;
    let (status, v) = f.call(&f.admin, "PATCH", &format!("/api/v1/projects/{p}/policy"), json!({"expected_revision":2,"review_mode":"agent","recovery_mode":"agent","lease_seconds":600,"rules":"","agent_rule_editing":true,"automatic_integration":true,"integration_owner":"integrator"})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let i = integrator_caller(&f).await;
    let mut body = result_body(submission.as_str().unwrap(), T0, R);
    body["landing_range"] = json!([below, "2222222222222222222222222222222222222222"]);
    let (_, v) = integrator_post(&f, &i, &p, "results", body).await;
    let result = v["data"]["id"].as_str().unwrap().to_owned();
    receipt(&f, &i, &p, &result, 900, "success").await;
    let (status, v) = authority(&f, &i, &p, &result).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["refusal"], "approver_is_contributor");
    assert_eq!(subject_state(&f, &t).await.0, "review");
    assert_eq!(held_holds(&f, &p).await, 0);
    let (_, wf) = f
        .call(
            &f.b,
            "GET",
            &format!(
                "/api/v1/projects/{p}/tasks/{}/workflow",
                t["id"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
    let replacement = wf["data"]["activities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["kind"] == "agent_review" && a["status"] == "queued")
        .unwrap()
        .clone();
    let (status, v) = {
        f.ack(&f.b, &p, 3).await;
        f.call(
            &f.b,
            "POST",
            &activity_path(&p, &replacement, "claim"),
            activity_claim(&replacement),
        )
        .await
    };
    assert_eq!(
        (status, v["error"]["code"].clone()),
        (StatusCode::CONFLICT, json!("reviewer_not_independent"))
    );
}

#[tokio::test]
async fn unapproved_stacked_work_blocks_authority() {
    let f = Fixture::new().await;
    let repo = "https://example.test/integrator-unapproved.git";
    let p = f.project("integrator-unapproved", repo).await;
    f.review_policy(&p, "agent").await;
    f.workflow_policy(&p, repo).await;
    let below = "b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2";
    let base = "1111111111111111111111111111111111111111";
    let pending = f.task(&p, "code", "Unreviewed below").await;
    let owner = f.claim(&f.b, &p, &pending, 2).await;
    f.checkout(&f.b, &p, &owner, base).await;
    f.submit(
        &f.b,
        &p,
        &pending,
        &owner,
        "code",
        2,
        Some(repo),
        Some(base),
        Some(below),
        Some("3333333333333333333333333333333333333333"),
    )
    .await;
    let (_, submission) = approved_code_task(
        &f,
        &p,
        repo,
        (&f.a, &f.c),
        ("Above", "2222222222222222222222222222222222222222"),
    )
    .await;
    let (status, v) = f.call(&f.admin, "PATCH", &format!("/api/v1/projects/{p}/policy"), json!({"expected_revision":2,"review_mode":"agent","recovery_mode":"agent","lease_seconds":600,"rules":"","agent_rule_editing":true,"automatic_integration":true,"integration_owner":"integrator"})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let i = integrator_caller(&f).await;
    let mut body = result_body(submission.as_str().unwrap(), T0, R);
    body["landing_range"] = json!([below, "2222222222222222222222222222222222222222"]);
    let (_, v) = integrator_post(&f, &i, &p, "results", body).await;
    let result = v["data"]["id"].as_str().unwrap().to_owned();
    receipt(&f, &i, &p, &result, 900, "success").await;
    let (status, v) = authority(&f, &i, &p, &result).await;
    assert_eq!(
        (status, v["error"]["code"].clone()),
        (StatusCode::CONFLICT, json!("stacked_on_unapproved"))
    );
}

/// Posts one integrator report of `kind` under `key`.
async fn report(
    f: &Fixture,
    i: &Caller,
    p: &str,
    kind: &str,
    key: &str,
    result: Option<&str>,
    details: Value,
) -> (StatusCode, Value) {
    let body = json!({"kind":kind,"dedupe_key":key,"result_id":result,"details":details});
    integrator_post(f, i, p, "reports", body).await
}

/// Resolves report `id` as `c` with `body`.
async fn resolve(f: &Fixture, c: &Caller, p: &str, id: &Value, body: Value) -> (StatusCode, Value) {
    let path = format!(
        "/api/v1/projects/{p}/integrator/reports/{}/resolve",
        id.as_str().unwrap()
    );
    f.call(c, "POST", &path, body).await
}

// P4 S4: reports are first-write-wins attestations, written only by the
// integrator on its own projects; the service derives requires_human.
#[tokio::test]
async fn integrator_reports_are_idempotent_and_integrator_scoped() {
    let f = Fixture::new().await;
    let (p, _, integration, i) = integrator_task(&f, "integrator-reports").await;
    let result = pinned_result(&f, &i, &p, &integration["submission_id"], T0).await;
    let gate = json!({"workflows":[{"path":".github/workflows/ci.yml","reasons":["secrets"]}]});
    let (status, first) = report(
        &f,
        &i,
        &p,
        "privilege_gate",
        "k1",
        Some(&result),
        gate.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["data"]["requires_human"], true);
    assert_eq!(first["data"]["allowed"], false);
    let (status, again) = report(
        &f,
        &i,
        &p,
        "privilege_gate",
        "k1",
        Some(&result),
        json!({"other":1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["data"]["id"], first["data"]["id"]);
    assert_eq!(again["data"]["details"], gate, "first write wins");
    let recorded: i64 =
        sqlx::query_scalar("SELECT count(*) FROM events WHERE kind='integrator.report_recorded'")
            .fetch_one(&f.state.pool)
            .await
            .unwrap();
    assert_eq!(recorded, 1, "a replay emits no event");
    let (status, flaky) = report(&f, &i, &p, "flaky", "k1", None, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{flaky}");
    assert_eq!(flaky["data"]["requires_human"], false);
    let blocking = json!({"verdict":"rerun_refused","blocks_subject":true});
    let (status, stuck) = report(&f, &i, &p, "flaky", "k6", None, blocking).await;
    assert_eq!(status, StatusCode::OK, "{stuck}");
    assert_eq!(
        stuck["data"]["requires_human"], true,
        "a report that blocks its subject needs a human"
    );
    let (status, v) = report(&f, &i, &p, "privilege_gate", "k2", None, json!({})).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "privilege_gate names its result: {v}"
    );
    let (status, v) = report(&f, &i, &p, "bogus", "k3", None, json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    let (status, v) = report(&f, &f.a, &p, "flaky", "k4", None, json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "agents cannot report: {v}");
    let (agent_project, _, _) = integrating_code_task(&f, "agent-owned-reports").await;
    let (status, v) = report(&f, &i, &agent_project, "flaky", "k5", None, json!({})).await;
    assert_eq!(
        (status, v["error"]["code"].clone()),
        (StatusCode::CONFLICT, json!("integration_owned_by_agents"))
    );
}

// P4 S4: only a human resolves a report; a privilege_gate resolution carries
// the allow/deny decision, and open human-required reports are listed by
// `next` as human-queue items until resolved.
#[tokio::test]
async fn humans_resolve_integrator_reports_listed_by_next() {
    let f = Fixture::new().await;
    let (p, _, integration, i) = integrator_task(&f, "integrator-resolve").await;
    let result = pinned_result(&f, &i, &p, &integration["submission_id"], T0).await;
    let (_, allow) = report(
        &f,
        &i,
        &p,
        "privilege_gate",
        "allow",
        Some(&result),
        json!({}),
    )
    .await;
    let (_, deny) = report(
        &f,
        &i,
        &p,
        "privilege_gate",
        "deny",
        Some(&result),
        json!({}),
    )
    .await;
    let (_, flaky) = report(&f, &i, &p, "flaky", "f", None, json!({})).await;
    let listed = next_action(&f, &f.a, &p, "implementer").await["human_queue_items"].clone();
    assert_eq!(listed.as_array().unwrap().len(), 2, "{listed}");
    assert_eq!(listed[0]["required_actor"], "human");
    assert_eq!(listed[0]["gate"], "privilege_gate");
    assert_eq!(listed[0]["report"]["id"], allow["data"]["id"]);
    let path = format!("/api/v1/projects/{p}/integrator/reports?open=true");
    let (status, open) = f.call(&f.a, "GET", &path, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{open}");
    assert_eq!(open["data"]["items"].as_array().unwrap().len(), 3);
    let (status, v) = resolve(
        &f,
        &f.a,
        &p,
        &allow["data"]["id"],
        json!({"note":"ok","decision":"allow"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(
        v["error"]["details"]["gate"],
        "integrator_report_resolution"
    );
    let (status, v) = resolve(
        &f,
        &i,
        &p,
        &allow["data"]["id"],
        json!({"note":"ok","decision":"allow"}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "the integrator cannot resolve: {v}"
    );
    let (status, v) = resolve(&f, &f.admin, &p, &allow["data"]["id"], json!({"note":"ok"})).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a gate needs a decision: {v}"
    );
    let (status, v) = resolve(
        &f,
        &f.admin,
        &p,
        &allow["data"]["id"],
        json!({"note":"ok","decision":"allow"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["allowed"], true);
    assert_eq!(v["data"]["resolved_by"], json!(f.admin.principal));
    let (_, replay) = report(
        &f,
        &i,
        &p,
        "privilege_gate",
        "allow",
        Some(&result),
        json!({}),
    )
    .await;
    assert_eq!(
        replay["data"]["allowed"], true,
        "the integrator reads the decision back"
    );
    let (status, v) = resolve(
        &f,
        &f.admin,
        &p,
        &allow["data"]["id"],
        json!({"note":"again","decision":"deny"}),
    )
    .await;
    assert_eq!(
        (status, v["error"]["code"].clone()),
        (StatusCode::CONFLICT, json!("report_already_resolved"))
    );
    let (status, v) = resolve(
        &f,
        &f.admin,
        &p,
        &deny["data"]["id"],
        json!({"note":"no","decision":"deny"}),
    )
    .await;
    assert_eq!(
        (status, v["data"]["allowed"].clone()),
        (StatusCode::OK, json!(false)),
        "{v}"
    );
    let (status, v) = resolve(
        &f,
        &f.admin,
        &p,
        &flaky["data"]["id"],
        json!({"note":"x","decision":"allow"}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "only gates take a decision: {v}"
    );
    let listed = next_action(&f, &f.a, &p, "implementer").await["human_queue_items"].clone();
    assert_eq!(listed, json!([]), "resolved reports leave the human queue");
    let (_, open) = f.call(&f.a, "GET", &path, Value::Null).await;
    assert_eq!(
        open["data"]["items"].as_array().unwrap().len(),
        1,
        "the flaky report stays open"
    );
}

// P4 S4: report listings page newest first with a `before` cursor.
#[tokio::test]
async fn integrator_reports_page_newest_first() {
    let f = Fixture::new().await;
    let (p, _, _, i) = integrator_task(&f, "integrator-report-pages").await;
    for key in ["k1", "k2", "k3"] {
        let (status, v) = report(&f, &i, &p, "flaky", key, None, json!({})).await;
        assert_eq!(status, StatusCode::OK, "{v}");
    }
    let list = |query: &str| format!("/api/v1/projects/{p}/integrator/reports?{query}");
    let keys = |v: &Value| -> Vec<Value> {
        let items = v["data"]["items"].as_array().unwrap();
        items.iter().map(|r| r["dedupe_key"].clone()).collect()
    };
    let (status, first) = f.call(&f.a, "GET", &list("limit=2"), Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(keys(&first), [json!("k3"), json!("k2")]);
    let cursor = first["data"]["next_before"].as_str().unwrap().to_owned();
    let (_, rest) = f
        .call(
            &f.a,
            "GET",
            &list(&format!("limit=2&before={cursor}")),
            Value::Null,
        )
        .await;
    assert_eq!(keys(&rest), [json!("k1")]);
    assert!(rest["data"]["next_before"].is_null(), "{rest}");
    for bad in ["limit=0", "limit=1001"] {
        let (status, v) = f.call(&f.a, "GET", &list(bad), Value::Null).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {v}");
    }
    let (status, _) = f
        .call(&f.a, "GET", &list("before=missing"), Value::Null)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Posts attempt `attempt` of run 900 of the roster check on R under `blob`.
async fn receipt_attempt(
    f: &Fixture,
    i: &Caller,
    p: &str,
    result: &str,
    (attempt, blob, conclusion): (i64, &str, &str),
) {
    let body = json!({"result_id":result,"check_name":CHECK,"run_id":900,"run_attempt":attempt,"head_sha":R,"app_id":15368,"workflow_path":".github/workflows/checks.yml","workflow_blob":blob,"conclusion":conclusion});
    let (status, v) = integrator_post(f, i, p, "receipts", body).await;
    assert_eq!(status, StatusCode::OK, "{v}");
}

/// An integrator `check_failed` revise citing `cited`.
fn check_failed_body(submission: &Value, cited: Value) -> Value {
    json!({"submission_id":submission,"reason_code":"check_failed","evidence":"workspace tests failed twice","result_id":cited})
}

/// Asserts a `check_failure_not_reproduced` refusal.
fn assert_not_reproduced(status: StatusCode, v: &Value) {
    assert_eq!(
        (status, v["error"]["code"].clone()),
        (StatusCode::CONFLICT, json!("check_failure_not_reproduced")),
        "{v}"
    );
}

// P4 S4b: the integrator's check_failed revise must cite a result on which
// one roster check's deciding attempt failed after an earlier failure.
#[tokio::test]
async fn integrator_check_failed_needs_a_reproduced_deciding_failure() {
    let f = Fixture::new().await;
    let (p, t, integration, i) = integrator_task(&f, "integrator-check-failed").await;
    let submission = &integration["submission_id"];
    let result = pinned_result(&f, &i, &p, submission, T0).await;
    let (status, v) = integrator_post(
        &f,
        &i,
        &p,
        "revise",
        check_failed_body(submission, Value::Null),
    )
    .await;
    assert_not_reproduced(status, &v);
    receipt_attempt(&f, &i, &p, &result, (1, BLOB, "failure")).await;
    receipt_attempt(&f, &i, &p, &result, (2, &"cd".repeat(20), "failure")).await;
    let revise = check_failed_body(submission, json!(result));
    let (status, v) = integrator_post(&f, &i, &p, "revise", revise.clone()).await;
    assert_not_reproduced(status, &v);
    assert_eq!(
        v["error"]["details"]["checks"]["workspace-tests"]["failures"],
        1
    );
    let (status, _) = integrator_post(
        &f,
        &i,
        &p,
        "revise",
        check_failed_body(submission, json!("nope")),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    receipt_attempt(&f, &i, &p, &result, (3, BLOB, "timed_out")).await;
    receipt_attempt(&f, &i, &p, &result, (4, BLOB, "success")).await;
    let (status, v) = integrator_post(&f, &i, &p, "revise", revise.clone()).await;
    assert_not_reproduced(status, &v);
    receipt_attempt(&f, &i, &p, &result, (5, BLOB, "failure")).await;
    let (status, v) = integrator_post(&f, &i, &p, "revise", revise).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(subject_state(&f, &t).await.0, "revision_needed");
}

// P4 S4b: cancelled and skipped attempts are not failures; a skipped
// deciding attempt passes the check for push authority, as on GitHub.
#[tokio::test]
async fn cancelled_and_skipped_receipts_are_not_failures() {
    let f = Fixture::new().await;
    let (p, _, integration, i) = integrator_task(&f, "integrator-skipped").await;
    let submission = &integration["submission_id"];
    let result = pinned_result(&f, &i, &p, submission, T0).await;
    receipt_attempt(&f, &i, &p, &result, (1, BLOB, "cancelled")).await;
    receipt_attempt(&f, &i, &p, &result, (2, BLOB, "skipped")).await;
    let revise = check_failed_body(submission, json!(result));
    let (status, v) = integrator_post(&f, &i, &p, "revise", revise).await;
    assert_not_reproduced(status, &v);
    let (status, v) = authority(&f, &i, &p, &result).await;
    assert_eq!(
        (status, v["data"]["granted"].clone()),
        (StatusCode::OK, json!(true)),
        "{v}"
    );
}

/// The task as the service reports it now, as seen by `f.a`: its current
/// revision, `eligible_to_claim` and unmet preconditions.
async fn fresh_task(f: &Fixture, p: &str, t: &Value) -> Value {
    let path = format!("/api/v1/projects/{p}/tasks/{}", t["id"].as_str().unwrap());
    let (status, v) = f.call(&f.a, "GET", &path, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    v["data"].clone()
}

/// A 40-hex-digit fixture candidate revision numbered `n`.
fn candidate(n: u32) -> String {
    format!("{n:040x}")
}

/// Claims, checks out and submits code task `t` with candidate `c` on an
/// integrator project (policy revision 3); returns the submission id.
async fn submit_code(f: &Fixture, p: &str, t: &Value, c: &str) -> Value {
    let t = fresh_task(f, p, t).await;
    let owner = f.claim(&f.a, p, &t, 3).await;
    f.checkout(&f.a, p, &owner, BASE).await;
    let repo: String = sqlx::query_scalar("SELECT repository_url FROM projects WHERE id=?")
        .bind(p)
        .fetch_one(&f.state.pool)
        .await
        .unwrap();
    let submitted = f
        .submit(
            &f.a,
            p,
            &t,
            &owner,
            "code",
            3,
            Some(&repo),
            Some(BASE),
            Some(c),
            Some(R),
        )
        .await;
    activity(&submitted, "integration")["submission_id"].clone()
}

/// A new code task in `p` submitted with candidate `c`: the task and its
/// submission id.
async fn new_subject(f: &Fixture, p: &str, c: &str) -> (Value, Value) {
    let t = f.task(p, "code", "Revised subject").await;
    let submission = submit_code(f, p, &t, c).await;
    (t, submission)
}

/// Pins a result of `submission` (candidate `c`) at T0 and returns its id.
async fn result_of(f: &Fixture, i: &Caller, p: &str, submission: &Value, c: &str) -> String {
    let mut body = result_body(submission.as_str().unwrap(), T0, R);
    body["c"] = json!(c);
    body["landing_range"] = json!([c]);
    let (status, v) = integrator_post(f, i, p, "results", body).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    v["data"]["id"].as_str().unwrap().to_owned()
}

/// A new code task in `p` with candidate `c` that the integrator
/// published; returns its task id and published result id.
async fn published_landing(f: &Fixture, i: &Caller, p: &str, c: &str) -> (String, String) {
    let (t, submission) = new_subject(f, p, c).await;
    let result = result_of(f, i, p, &submission, c).await;
    receipt(f, i, p, &result, 900, "success").await;
    let (status, v) = authority(f, i, p, &result).await;
    assert_eq!(v["data"]["granted"], true, "{status} {v}");
    let (status, v) = observe(f, i, p, &result, R, "contained").await;
    assert_eq!(v["data"]["disposition"], "published", "{status} {v}");
    (t["id"].as_str().unwrap().to_owned(), result)
}

/// Posts a `conflict` revise of `submission` citing `moved_by`.
async fn conflict_revise(
    f: &Fixture,
    i: &Caller,
    p: &str,
    submission: &Value,
    moved_by: Option<&str>,
) -> (StatusCode, Value) {
    let body = json!({"submission_id":submission,"reason_code":"conflict","evidence":"CONFLICT (content): src/lib.rs","moved_by_result_id":moved_by});
    integrator_post(f, i, p, "revise", body).await
}

/// Revises `submission` of `t` and resubmits candidate `c`; returns the new
/// submission id.
async fn revise_and_resubmit(
    f: &Fixture,
    i: &Caller,
    p: &str,
    (t, submission): (&Value, &Value),
    moved_by: Option<&str>,
    c: &str,
) -> Value {
    let (status, v) = conflict_revise(f, i, p, submission, moved_by).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["park_reason"], Value::Null, "{v}");
    submit_code(f, p, t, c).await
}

/// Brings subject `t` to the edge of the revise limit (one revise short of
/// `REVISE_LIMIT`) with revises that are not serialized, each followed by a
/// resubmission of `c`. Returns the current submission id.
async fn at_the_edge(
    f: &Fixture,
    i: &Caller,
    p: &str,
    t: &Value,
    submission: Value,
    c: &str,
) -> Value {
    let mut submission = submission;
    for _ in 0..2 {
        submission = revise_and_resubmit(f, i, p, (t, &submission), None, c).await;
    }
    submission
}

/// Asserts the revise applied without serializing and parked the subject
/// for `reason`.
fn assert_parking(status: StatusCode, v: &Value, reason: &str) {
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["serialized_after"], Value::Null, "{v}");
    assert_eq!(v["data"]["park_reason"], reason, "{v}");
}

/// Asserts task `t` is parked for agents: GET shows the unmet
/// `revise_limit_reached` precondition and an explicit claim is refused
/// with the same code.
async fn assert_parked(f: &Fixture, p: &str, t: &Value) {
    let task = fresh_task(f, p, t).await;
    assert_eq!(task["eligible_to_claim"], false, "{task}");
    let codes: Vec<&Value> = task["unmet_preconditions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| &u["code"])
        .collect();
    assert!(codes.contains(&&json!("revise_limit_reached")), "{task}");
    let (status, v) = f.call(&f.a, "POST", &format!("/api/v1/projects/{p}/claims"), json!({"task_id":task["id"],"expected_task_revision":task["revision"],"mode":"work","policy_revision":3,"instruction_version":coordinator_core::INSTRUCTION_VERSION})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["error"]["code"], "revise_limit_reached", "{v}");
    assert_eq!(v["error"]["details"]["required_actor"], "human", "{v}");
}

/// Asserts task `t` is claimable and is what `next` offers an implementer.
async fn assert_claimable(f: &Fixture, p: &str, t: &Value) {
    let task = fresh_task(f, p, t).await;
    assert_eq!(task["eligible_to_claim"], true, "{task}");
    let offered = next_action(f, &f.a, p, "implementer").await;
    assert_eq!(offered["action"]["task_id"], t["id"], "{offered}");
}

/// The prerequisites recorded for task `t`.
async fn prerequisites(f: &Fixture, t: &Value) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT prerequisite_id FROM task_dependencies WHERE task_id=? ORDER BY prerequisite_id",
    )
    .bind(t["id"].as_str().unwrap())
    .fetch_all(&f.state.pool)
    .await
    .unwrap()
}

// P4 S4d: a revise below the edge of the limit only records the landing.
#[tokio::test]
async fn integrator_revise_below_the_limit_only_records_the_landing() {
    let f = Fixture::new().await;
    let (p, t, integration, i) = integrator_task(&f, "serialize-below").await;
    let (landing, result) = published_landing(&f, &i, &p, &candidate(0xa1)).await;
    let submission = &integration["submission_id"];
    let (status, v) = conflict_revise(&f, &i, &p, submission, Some(&result)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["serialized_after"], Value::Null);
    assert_eq!(v["data"]["park_reason"], Value::Null);
    assert_eq!(v["data"]["revise"]["landing_task_id"], json!(landing));
    assert!(prerequisites(&f, &t).await.is_empty());
    let row: (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT landing_task_id,serialized_after FROM integrator_revises WHERE submission_id=?",
    )
    .bind(submission.as_str().unwrap())
    .fetch_one(&f.state.pool)
    .await
    .unwrap();
    assert_eq!(row, (Some(landing), None));
}

// P4 S4d: the revise that would reach the limit is serialized after a new
// landing, does not count toward the limit, and leaves the subject
// claimable and offered by `next`.
#[tokio::test]
async fn a_serialized_subject_stays_claimable() {
    let f = Fixture::new().await;
    let (p, t, integration, i) = integrator_task(&f, "serialize-claimable").await;
    let (landing, result) = published_landing(&f, &i, &p, &candidate(0xa1)).await;
    let c = "2222222222222222222222222222222222222222";
    let submission = at_the_edge(&f, &i, &p, &t, integration["submission_id"].clone(), c).await;
    let (status, v) = conflict_revise(&f, &i, &p, &submission, Some(&result)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["serialized_after"], json!(landing));
    assert_eq!(v["data"]["park_reason"], Value::Null);
    assert_eq!(prerequisites(&f, &t).await, std::slice::from_ref(&landing));
    assert_eq!(subject_state(&f, &t).await.0, "revision_needed");
    assert_claimable(&f, &p, &t).await;
}

// P4 S4d: landings A, B, then A again: the repeat is a persistent cycle, so
// that revise applies and parks the subject, and agents cannot claim it.
#[tokio::test]
async fn a_repeated_landing_parks_the_subject() {
    let f = Fixture::new().await;
    let (p, t, integration, i) = integrator_task(&f, "serialize-repeat").await;
    let (a, result_a) = published_landing(&f, &i, &p, &candidate(0xa1)).await;
    let (b, result_b) = published_landing(&f, &i, &p, &candidate(0xb1)).await;
    let c = "2222222222222222222222222222222222222222";
    let mut submission = at_the_edge(&f, &i, &p, &t, integration["submission_id"].clone(), c).await;
    for (landing, result) in [(&a, &result_a), (&b, &result_b)] {
        let (status, v) = conflict_revise(&f, &i, &p, &submission, Some(result)).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        assert_eq!(v["data"]["serialized_after"], json!(landing), "{v}");
        assert_claimable(&f, &p, &t).await;
        submission = submit_code(&f, &p, &t, c).await;
    }
    let (status, v) = conflict_revise(&f, &i, &p, &submission, Some(&result_a)).await;
    assert_parking(status, &v, "landing_repeated");
    let mut both = vec![a, b];
    both.sort();
    assert_eq!(prerequisites(&f, &t).await, both);
    assert_parked(&f, &p, &t).await;
    let offered = next_action(&f, &f.a, &p, "implementer").await;
    assert_ne!(offered["action"]["task_id"], t["id"], "{offered}");
    let next_claim = json!({"mode":"work","policy_revision":3,"instruction_version":coordinator_core::INSTRUCTION_VERSION});
    let path = format!("/api/v1/projects/{p}/claims");
    let (status, v) = f.call(&f.a, "POST", &path, next_claim).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        v["data"]["claim"],
        Value::Null,
        "a parked task is skipped: {v}"
    );
}

// P4 S4d: at the edge, a revise that cannot be serialized applies and parks
// the subject, naming why; a parked subject refuses further revises.
#[tokio::test]
async fn an_unserializable_revise_at_the_edge_parks_the_subject() {
    let f = Fixture::new().await;
    let (p, _, _, i) = integrator_task(&f, "serialize-park").await;
    let (landing, result) = published_landing(&f, &i, &p, &candidate(0xa1)).await;
    for (n, moved_by) in [(1, None), (2, Some("nope"))] {
        let (t, s) = edge_subject(&f, &i, &p, n).await;
        let (status, v) = conflict_revise(&f, &i, &p, &s, moved_by).await;
        assert_parking(status, &v, "landing_unknown");
        assert_parked(&f, &p, &t).await;
    }
    let (t, s) = edge_subject(&f, &i, &p, 3).await;
    let own = result_of(&f, &i, &p, &s, &candidate(3)).await;
    let (status, v) = conflict_revise(&f, &i, &p, &s, Some(&own)).await;
    assert_parking(status, &v, "landing_unknown");
    assert_parked(&f, &p, &t).await;
    let (t, s) = edge_subject(&f, &i, &p, 4).await;
    let own = result_of(&f, &i, &p, &s, &candidate(4)).await;
    sqlx::query("INSERT INTO integrator_observations(id,result_id,tip,ancestry,disposition,evidence,observed_by,observed_at) VALUES('own',?,?,'contained','published','x',?,0)")
        .bind(&own).bind(R).bind(&i.principal).execute(&f.state.pool).await.unwrap();
    let (status, v) = conflict_revise(&f, &i, &p, &s, Some(&own)).await;
    assert_parking(status, &v, "landing_is_subject");
    assert_parked(&f, &p, &t).await;
    let (t, s) = edge_subject(&f, &i, &p, 5).await;
    let failed = result_of(&f, &i, &p, &s, &candidate(5)).await;
    receipt_attempt(&f, &i, &p, &failed, (1, BLOB, "failure")).await;
    receipt_attempt(&f, &i, &p, &failed, (2, BLOB, "failure")).await;
    let body = json!({"submission_id":s,"reason_code":"check_failed","evidence":"x","result_id":failed,"moved_by_result_id":result});
    let (status, v) = integrator_post(&f, &i, &p, "revise", body).await;
    assert_parking(status, &v, "not_a_conflict");
    assert_parked(&f, &p, &t).await;
    let (t, s) = edge_subject(&f, &i, &p, 6).await;
    sqlx::query("INSERT INTO task_dependencies(project_id,task_id,prerequisite_id) VALUES(?,?,?)")
        .bind(&p)
        .bind(&landing)
        .bind(t["id"].as_str().unwrap())
        .execute(&f.state.pool)
        .await
        .unwrap();
    let (status, v) = conflict_revise(&f, &i, &p, &s, Some(&result)).await;
    assert_parking(status, &v, "dependency_cycle");
    assert!(prerequisites(&f, &t).await.is_empty());
    let (t, s) = new_subject(&f, &p, &candidate(7)).await;
    seed_revises(&f, &i, &t, 3).await;
    let (status, v) = conflict_revise(&f, &i, &p, &s, Some(&result)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["error"]["details"]["gate"], "revise_limit_reached", "{v}");
    assert_eq!(
        v["error"]["details"]["park_reason"], "subject_parked",
        "{v}"
    );
}

/// A new subject in `p` with candidate number `n`, brought to the edge of
/// the revise limit; returns the task and its current submission id.
async fn edge_subject(f: &Fixture, i: &Caller, p: &str, n: u32) -> (Value, Value) {
    let c = candidate(n);
    let (t, submission) = new_subject(f, p, &c).await;
    let submission = at_the_edge(f, i, p, &t, submission, &c).await;
    (t, submission)
}

/// Seeds `n` agent revises of task `t`'s current submission in the last
/// 24 hours (the limits count `submission.reopened` events).
async fn seed_revises(f: &Fixture, i: &Caller, t: &Value, n: usize) {
    for _ in 0..n {
        sqlx::query("INSERT INTO events(actor_id,kind,record_id,data_json,created_at) SELECT ?,'submission.reopened',current_submission_id,'{}',? FROM workflow_subjects WHERE task_id=?")
            .bind(&i.principal).bind(f.state.now()).bind(t["id"].as_str().unwrap())
            .execute(&f.state.pool).await.unwrap();
    }
}

// P4 S4d: SERIALIZED_REVISE_CAP revises in 24 hours park the subject even
// when every revise at the edge is serialized after a new landing.
#[tokio::test]
async fn the_cap_parks_a_subject_whose_revises_were_serialized() {
    let f = Fixture::new().await;
    let (p, t, integration, i) = integrator_task(&f, "serialize-cap").await;
    let c = "2222222222222222222222222222222222222222";
    let mut submission = at_the_edge(&f, &i, &p, &t, integration["submission_id"].clone(), c).await;
    for n in 1..=4 {
        let (landing, result) = published_landing(&f, &i, &p, &candidate(0xa0 + n)).await;
        let (status, v) = conflict_revise(&f, &i, &p, &submission, Some(&result)).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        assert_eq!(v["data"]["serialized_after"], json!(landing), "{v}");
        if n < 4 {
            assert_eq!(v["data"]["park_reason"], Value::Null, "{v}");
            submission = submit_code(&f, &p, &t, c).await;
        } else {
            assert_eq!(v["data"]["park_reason"], "serialized_cap", "{v}");
        }
    }
    assert_parked(&f, &p, &t).await;
}

/// A subject of an integrator project that the integrator published at T0
/// (candidate `2222…`, landing range `[2222…]`): the project, the task, the
/// published result id and the integrator's caller.
async fn published_subject(f: &Fixture, name: &str) -> (String, Value, String, Caller) {
    let (p, t, integration, i) = integrator_task(f, name).await;
    let result = authorized(f, &i, &p, &integration["submission_id"], T0).await;
    let (status, v) = observe(f, &i, &p, &result, R, "contained").await;
    assert_eq!(v["data"]["disposition"], "published", "{status} {v}");
    (p, t, result, i)
}

/// Posts `POST …/reverts` as `c`.
async fn post_revert(f: &Fixture, c: &Caller, p: &str, body: Value) -> (StatusCode, Value) {
    f.call(c, "POST", &format!("/api/v1/projects/{p}/reverts"), body)
        .await
}

/// Reverts `result` as `c` with `reason` and `evidence`; returns the task.
async fn reverted(
    f: &Fixture,
    c: &Caller,
    p: &str,
    result: &str,
    (reason, evidence): (&str, Value),
) -> Value {
    let body = json!({"result_id":result,"reason":reason,"evidence":evidence});
    let (status, v) = post_revert(f, c, p, body).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    v["data"].clone()
}

/// The integrator queue of `p` as `i` sees it.
async fn integrator_queue(f: &Fixture, i: &Caller, p: &str) -> Value {
    let path = format!("/api/v1/projects/{p}/integrator/queue");
    let (status, v) = f.call(i, "GET", &path, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    v["data"].clone()
}

/// A mechanical candidate body computed on `t0` with commit `commit`.
fn candidate_body(t0: &str, commit: &str) -> Value {
    json!({"t0":t0,"candidate_commit":commit,"candidate_tree":"8888888888888888888888888888888888888888","mechanical":true,"candidate_ref":format!("refs/agent-coordinator/candidates/revert-{commit}")})
}

/// Posts the mechanical candidate of revert task `revert` as `c`.
async fn revert_candidate(
    f: &Fixture,
    c: &Caller,
    p: &str,
    revert: &Value,
    body: Value,
) -> (StatusCode, Value) {
    let route = format!("reverts/{}/candidate", revert["id"].as_str().unwrap());
    integrator_post(f, c, p, &route, body).await
}

/// The number of `kind` events recorded for `record`.
async fn events_of(f: &Fixture, kind: &str, record: &Value) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM events WHERE kind=? AND record_id=?")
        .bind(kind)
        .bind(record.as_str().unwrap())
        .fetch_one(&f.state.pool)
        .await
        .unwrap()
}

/// The status and error code of a refused call.
fn refusal(status: StatusCode, v: &Value) -> (StatusCode, Value) {
    (status, v["error"]["code"].clone())
}

// P4 S4e (M6): a human's one-click revert needs no review, is recorded as an
// escaped-defect canary, is listed for the integrator, and its mechanical
// candidate goes straight to integration; the original shows reverted_by.
#[tokio::test]
async fn a_human_revert_needs_no_review_and_counts_as_a_canary() {
    let f = Fixture::new().await;
    let (p, t, result, i) = published_subject(&f, "revert-human").await;
    let revert = reverted(&f, &f.admin, &p, &result, ("human", Value::Null)).await;
    assert_eq!(revert["revert"]["review_required"], false, "{revert}");
    assert_eq!(revert["work_status"], "blocked", "{revert}");
    let canaries = events_of(&f, "revert.escaped_defect_canary", &revert["id"]).await;
    assert_eq!(canaries, 1);
    let queue = integrator_queue(&f, &i, &p).await;
    assert_eq!(queue["reverts"][0]["task_id"], revert["id"], "{queue}");
    assert_eq!(queue["reverts"][0]["target"]["r"], R, "{queue}");
    let body = candidate_body(R, &candidate(0xfeed));
    let (status, v) = revert_candidate(&f, &i, &p, &revert, body).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["phase"], "integration", "{v}");
    assert_eq!(fresh_task(&f, &p, &t).await["reverted_by"], revert["id"]);
}

// P4 S4e (M6): an agent reverts only with a reason other than human and
// non-empty evidence, and its candidate needs a review even though the
// project's review mode is none.
#[tokio::test]
async fn an_agent_revert_needs_evidence_and_review() {
    let f = Fixture::new().await;
    let (p, _, result, i) = published_subject(&f, "revert-agent").await;
    let body = json!({"result_id":result,"reason":"human"});
    let (status, v) = post_revert(&f, &f.b, &p, body).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(
        v["error"]["details"]["gate"], "revert_without_evidence",
        "{v}"
    );
    let body = json!({"result_id":result,"reason":"defect","evidence":""});
    let (status, v) = post_revert(&f, &f.b, &p, body).await;
    let expected = (StatusCode::BAD_REQUEST, json!("revert_evidence_required"));
    assert_eq!(refusal(status, &v), expected, "{v}");
    let evidence = json!({"check":"workspace-tests","first_parent":"pass","tip":"fail"});
    let revert = reverted(&f, &f.b, &p, &result, ("defect", evidence)).await;
    assert_eq!(revert["revert"]["review_required"], true, "{revert}");
    let body = candidate_body(R, &candidate(0xfeed));
    let (status, v) = revert_candidate(&f, &i, &p, &revert, body).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["phase"], "review", "{v}");
    assert_eq!(
        activity(&v["data"], "agent_review")["status"],
        "queued",
        "{v}"
    );
}

// P4 S4e (M6): one open revert per result; a result with no published
// observation, or of another project, cannot be reverted.
#[tokio::test]
async fn a_second_revert_and_unpublished_or_foreign_results_are_refused() {
    let f = Fixture::new().await;
    let (p, _, result, i) = published_subject(&f, "revert-duplicate").await;
    reverted(&f, &f.admin, &p, &result, ("human", Value::Null)).await;
    let body = json!({"result_id":result,"reason":"human"});
    let (status, v) = post_revert(&f, &f.admin, &p, body.clone()).await;
    let expected = (StatusCode::CONFLICT, json!("revert_exists"));
    assert_eq!(refusal(status, &v), expected, "{v}");
    let (q, _, integration) = integrating_code_task(&f, "revert-unpublished").await;
    let (status, v) = set_integration_owner(&f, &f.admin, &q, "integrator").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (status, v) = post_revert(&f, &f.admin, &q, body).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{v}");
    let pinned = pinned_result(&f, &i, &q, &integration["submission_id"], T0).await;
    let body = json!({"result_id":pinned,"reason":"human"});
    let (status, v) = post_revert(&f, &f.admin, &q, body).await;
    let expected = (StatusCode::CONFLICT, json!("result_not_published"));
    assert_eq!(refusal(status, &v), expected, "{v}");
}

// P4 S4e (M6): only the integrator records a revert's candidate; the same
// candidate for the same tip replays, a different one conflicts, and a
// revert with a candidate leaves the queue's reverts.
#[tokio::test]
async fn revert_candidates_are_integrator_only_and_idempotent_per_tip() {
    let f = Fixture::new().await;
    let (p, _, result, i) = published_subject(&f, "revert-candidate").await;
    let revert = reverted(&f, &f.admin, &p, &result, ("human", Value::Null)).await;
    let body = candidate_body(R, &candidate(0xfeed));
    let (status, v) = revert_candidate(&f, &f.a, &p, &revert, body.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    let (_, first) = revert_candidate(&f, &i, &p, &revert, body.clone()).await;
    let (status, again) = revert_candidate(&f, &i, &p, &revert, body).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    let id = &first["data"]["candidate_submission_id"];
    assert!(id.is_string(), "{first}");
    assert_eq!(id, &again["data"]["candidate_submission_id"]);
    let other = candidate_body(R, &candidate(0xbeef));
    let (status, v) = revert_candidate(&f, &i, &p, &revert, other).await;
    let expected = (StatusCode::CONFLICT, json!("revert_candidate_conflict"));
    assert_eq!(refusal(status, &v), expected, "{v}");
    assert_eq!(integrator_queue(&f, &i, &p).await["reverts"], json!([]));
}

// P4 S4f (M6): a mechanical revert's candidate in integration is marked in
// the queue with its revert task id, so the integrator reports a reproduced
// check failure not mechanical; an ordinary subject carries null.
#[tokio::test]
async fn a_mechanical_revert_candidate_is_marked_in_the_queue() {
    let f = Fixture::new().await;
    let (p, _, result, i) = published_subject(&f, "revert-marker").await;
    let revert = reverted(&f, &f.admin, &p, &result, ("human", Value::Null)).await;
    let body = candidate_body(R, &candidate(0xfeed));
    let (status, v) = revert_candidate(&f, &i, &p, &revert, body).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let queue = integrator_queue(&f, &i, &p).await;
    assert_eq!(
        queue["items"][0]["subject_task_id"], revert["id"],
        "{queue}"
    );
    assert_eq!(queue["items"][0]["revert_task_id"], revert["id"], "{queue}");
    let (q, t, _) = integrating_code_task(&f, "revert-marker-ordinary").await;
    let (status, v) = set_integration_owner(&f, &f.admin, &q, "integrator").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let queue = integrator_queue(&f, &i, &q).await;
    let ordinary = queue["items"][0].as_object().unwrap();
    assert_eq!(ordinary["subject_task_id"], t["id"], "{queue}");
    assert_eq!(
        ordinary.get("revert_task_id"),
        Some(&Value::Null),
        "{queue}"
    );
}

/// Claims, checks out and submits converted revert `t` of the fixture
/// project `revert-not-mechanical` as `f.a`, with one evidence entry per
/// current acceptance criterion; returns the workflow snapshot.
async fn submit_revert_work(f: &Fixture, p: &str, t: &Value) -> Value {
    let t = fresh_task(f, p, t).await;
    let owner = f.claim(&f.a, p, &t, 3).await;
    f.checkout(&f.a, p, &owner, BASE).await;
    let repo = "https://example.test/revert-not-mechanical.git";
    let evidence: Vec<Value> = t["acceptance_criteria"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| json!({"criterion":c,"evidence":"verified in workflow test"}))
        .collect();
    let path = format!(
        "/api/v1/projects/{p}/attempts/{}/submissions",
        owner["id"].as_str().unwrap()
    );
    let (status, v) = f.call(&f.a, "POST", &path, json!({"generation":owner["generation"],"task_revision":t["revision"],"project_policy_revision":3,"workflow_policy_revision":1,"kind":"code","summary":"undone by hand","acceptance_evidence":evidence,"handoff":"","repository":repo,"base_revision":BASE,"candidate_revision":candidate(0xcafe),"candidate_tree":R,"candidate_remote":repo,"candidate_ref":"refs/agent-coordinator/candidates/undo"})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    v["data"].clone()
}

// P4 S4e (M6): a revert the integrator cannot compute mechanically becomes
// claimable implementation work that needs review, and leaves the queue.
#[tokio::test]
async fn a_revert_that_is_not_mechanical_becomes_reviewed_implementation_work() {
    let f = Fixture::new().await;
    let (p, _, result, i) = published_subject(&f, "revert-not-mechanical").await;
    let revert = reverted(&f, &f.admin, &p, &result, ("human", Value::Null)).await;
    let route = format!("reverts/{}/not-mechanical", revert["id"].as_str().unwrap());
    let body = json!({"t0":R,"reason":"conflict","evidence":"CONFLICT (content): src/lib.rs"});
    let (status, v) = integrator_post(&f, &i, &p, &route, body).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["revert"]["mode"], "not_mechanical", "{v}");
    assert_eq!(v["data"]["revert"]["review_required"], true, "{v}");
    assert_eq!(fresh_task(&f, &p, &revert).await["eligible_to_claim"], true);
    assert_eq!(integrator_queue(&f, &i, &p).await["reverts"], json!([]));
    let body = candidate_body(R, &candidate(0xfeed));
    let (status, v) = revert_candidate(&f, &i, &p, &revert, body).await;
    let expected = (StatusCode::CONFLICT, json!("revert_not_mechanical"));
    assert_eq!(refusal(status, &v), expected, "{v}");
    let submitted = submit_revert_work(&f, &p, &revert).await;
    assert_eq!(submitted["phase"], "review", "{submitted}");
    assert_eq!(activity(&submitted, "agent_review")["status"], "queued");
}

// P4 S4e (M6, plan-final §2.4): an author_withdraw revise that loses to a
// landed push yields an urgent, reviewed revert of the landed result.
#[tokio::test]
async fn an_author_withdraw_that_loses_to_a_push_yields_an_urgent_revert() {
    let f = Fixture::new().await;
    let (p, t, integration, i) = integrator_task(&f, "revert-withdraw").await;
    let submission = &integration["submission_id"];
    let result = authorized(&f, &i, &p, submission, T0).await;
    f.ack(&f.a, &p, 3).await;
    let path = format!(
        "/api/v1/projects/{p}/tasks/{}/workflow/reopen",
        t["id"].as_str().unwrap()
    );
    let body = revise_body(submission, "author_withdraw", Some("wrong approach"));
    let (status, v) = f.call(&f.a, "POST", &path, body).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["revise_deferred"], true, "{v}");
    let (_, v) = observe(&f, &i, &p, &result, R, "contained").await;
    let revert = json!({"id": v["data"]["revise"]["follow_up_task_id"]});
    let view = fresh_task(&f, &p, &revert).await;
    assert_eq!(view["priority"], 0, "{view}");
    assert_eq!(view["revert"]["reason"], "author_withdraw", "{view}");
    assert_eq!(view["revert"]["review_required"], true, "{view}");
    assert_eq!(
        view["revert"]["evidence"]["evidence"], "wrong approach",
        "{view}"
    );
    assert_eq!(fresh_task(&f, &p, &t).await["reverted_by"], view["id"]);
}

/// Posts a no-op result (`r == t0`) of `submission` with candidate `c`.
async fn no_op_result(
    f: &Fixture,
    i: &Caller,
    p: &str,
    submission: &Value,
    c: &str,
) -> (StatusCode, Value) {
    let mut body = result_body(submission.as_str().unwrap(), T0, T0);
    (body["c"], body["landing_range"]) = (json!(c), json!([c]));
    integrator_post(f, i, p, "results", body).await
}

/// The candidate commit of the fixture subject `published_subject` lands.
const LANDED: &str = "2222222222222222222222222222222222222222";

// P4 S4e (plan-final §2.4 no-op): the queue lists reverted landing ranges,
// and pinning a no-op result whose candidate a recorded revert undid is
// refused with the shared commits; a no-op of new commits and a result
// that is not a no-op are still pinned.
#[tokio::test]
async fn a_no_op_over_a_reverted_landing_is_refused() {
    let f = Fixture::new().await;
    let (p, _, result, i) = published_subject(&f, "revert-history").await;
    reverted(&f, &f.admin, &p, &result, ("human", Value::Null)).await;
    let (_, submission) = new_subject(&f, &p, LANDED).await;
    let queue = integrator_queue(&f, &i, &p).await;
    let items = queue["items"].as_array().unwrap();
    let item = items
        .iter()
        .find(|v| v["submission_id"] == submission)
        .unwrap();
    assert_eq!(item["reverted"][0]["result_id"], json!(result), "{item}");
    let (status, v) = no_op_result(&f, &i, &p, &submission, LANDED).await;
    let expected = (StatusCode::CONFLICT, json!("candidate_reverted_in_history"));
    assert_eq!(refusal(status, &v), expected, "{v}");
    assert_eq!(v["error"]["details"]["commits"], json!([LANDED]), "{v}");
    result_of(&f, &i, &p, &submission, LANDED).await;
    let fresh = candidate(0x77);
    let (_, submission) = new_subject(&f, &p, &fresh).await;
    let (status, v) = no_op_result(&f, &i, &p, &submission, &fresh).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let noop = v["data"]["id"].as_str().unwrap();
    let (status, v) = observe(&f, &i, &p, noop, R, "contained").await;
    assert_eq!(
        v["data"]["disposition"], "already_contained",
        "{status} {v}"
    );
}

// P4 S4e: reverted history is scoped to the repository and target branch
// the reverted result landed on.
#[tokio::test]
async fn reverted_history_is_scoped_to_its_target_branch() {
    let f = Fixture::new().await;
    let (p, _, result, i) = published_subject(&f, "revert-history-scope").await;
    reverted(&f, &f.admin, &p, &result, ("human", Value::Null)).await;
    sqlx::query("UPDATE submissions SET target_branch='release' WHERE id=(SELECT submission_id FROM integrator_results WHERE id=?)")
        .bind(&result).execute(&f.state.pool).await.unwrap();
    let (_, submission) = new_subject(&f, &p, LANDED).await;
    let queue = integrator_queue(&f, &i, &p).await;
    let items = queue["items"].as_array().unwrap();
    let item = items
        .iter()
        .find(|v| v["submission_id"] == submission)
        .unwrap();
    assert_eq!(item["reverted"], json!([]), "{item}");
    let (status, v) = no_op_result(&f, &i, &p, &submission, LANDED).await;
    assert_eq!(status, StatusCode::OK, "{v}");
}

// P4 S4e: the integrator sends a candidate reverted in history back to its
// author with the reverted_in_history revise, which needs evidence and is
// refused for a candidate that no revert touched.
#[tokio::test]
async fn a_candidate_reverted_in_history_is_revised_back_to_its_author() {
    let f = Fixture::new().await;
    let (p, _, result, i) = published_subject(&f, "revert-history-revise").await;
    reverted(&f, &f.admin, &p, &result, ("human", Value::Null)).await;
    let (t, submission) = new_subject(&f, &p, LANDED).await;
    let body =
        json!({"submission_id":submission,"reason_code":"reverted_in_history","evidence":""});
    let (status, v) = integrator_post(&f, &i, &p, "revise", body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    let fresh = candidate(0x99);
    let (_, other) = new_subject(&f, &p, &fresh).await;
    let body = json!({"submission_id":other,"reason_code":"reverted_in_history","evidence":fresh});
    let (status, v) = integrator_post(&f, &i, &p, "revise", body).await;
    let expected = (StatusCode::CONFLICT, json!("not_reverted_in_history"));
    assert_eq!(refusal(status, &v), expected, "{v}");
    let body =
        json!({"submission_id":submission,"reason_code":"reverted_in_history","evidence":LANDED});
    let (status, v) = integrator_post(&f, &i, &p, "revise", body).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["serialized_after"], Value::Null, "{v}");
    assert_eq!(subject_state(&f, &t).await.0, "revision_needed");
}

/// Approves the queued agent review of snapshot `v` as `f.c`.
async fn approve_review(f: &Fixture, p: &str, v: &Value) {
    let review = activity(v, "agent_review").clone();
    let (status, claimed) = f.claim_activity(&f.c, p, &review, 3, 1).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    let path = activity_path(p, &review, "review");
    let (status, v) = f.call(&f.c, "POST", &path, json!({"generation":claimed["data"]["attempt"]["generation"],"submission_id":review["submission_id"],"decision":"approved","summary":"the evidence shows the defect","findings":[]})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
}

/// Pins, checks, authorizes and observes as published the revert's
/// candidate submission (commit `commit`).
async fn publish_revert(f: &Fixture, i: &Caller, p: &str, submission: &Value, commit: &str) {
    let mut body = result_body(submission.as_str().unwrap(), T0, R);
    (body["c"], body["landing_range"]) = (json!(commit), json!([commit]));
    let (status, v) = integrator_post(f, i, p, "results", body).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let result = v["data"]["id"].as_str().unwrap().to_owned();
    receipt(f, i, p, &result, 901, "success").await;
    let (status, v) = authority(f, i, p, &result).await;
    assert_eq!(v["data"]["granted"], true, "{status} {v}");
    let (status, v) = observe(f, i, p, &result, R, "contained").await;
    assert_eq!(v["data"]["disposition"], "published", "{status} {v}");
}

// P4 S4e (M6): a published defect revert proposes a planned re-land task
// seeded with the original and the revert evidence; the original stays done
// and names its revert.
#[tokio::test]
async fn a_published_defect_revert_proposes_a_reland() {
    let f = Fixture::new().await;
    let (p, t, result, i) = published_subject(&f, "revert-reland").await;
    let evidence = json!("workspace-tests fails on the tip and passes on the first parent");
    let revert = reverted(&f, &f.b, &p, &result, ("defect", evidence)).await;
    let commit = candidate(0xfeed);
    let body = candidate_body(R, &commit);
    let (status, v) = revert_candidate(&f, &i, &p, &revert, body).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    approve_review(&f, &p, &v["data"]).await;
    let submission = &v["data"]["candidate_submission_id"];
    publish_revert(&f, &i, &p, submission, &commit).await;
    let view = fresh_task(&f, &p, &revert).await;
    assert_eq!(view["lifecycle"], "done", "{view}");
    let reland = json!({"id": view["revert"]["reland_task_id"]});
    let reland = fresh_task(&f, &p, &reland).await;
    assert_eq!(reland["lifecycle"], "planned", "{reland}");
    let criteria = reland["acceptance_criteria"].as_array().unwrap();
    let last = criteria.last().unwrap().as_str().unwrap();
    assert!(last.contains("passes on the first parent"), "{reland}");
    let original = fresh_task(&f, &p, &t).await;
    assert_eq!(original["lifecycle"], "done", "{original}");
    assert_eq!(original["reverted_by"], revert["id"], "{original}");
}

/// Claims the queued agent review of snapshot `v` as `c`.
async fn claim_revert_review(f: &Fixture, c: &Caller, p: &str, v: &Value) -> (StatusCode, Value) {
    let review = activity(v, "agent_review").clone();
    f.claim_activity(c, p, &review, 3, 1).await
}

/// The author's `author_withdraw` that lost to the push of the fixture
/// subject: the project, the automatic revert id and the integrator.
async fn withdrawn_landing(f: &Fixture, name: &str) -> (String, Value, Caller) {
    let (p, t, integration, i) = integrator_task(f, name).await;
    let submission = &integration["submission_id"];
    let result = authorized(f, &i, &p, submission, T0).await;
    f.ack(&f.a, &p, 3).await;
    let path = format!(
        "/api/v1/projects/{p}/tasks/{}/workflow/reopen",
        t["id"].as_str().unwrap()
    );
    let body = revise_body(submission, "author_withdraw", Some("wrong approach"));
    let (status, v) = f.call(&f.a, "POST", &path, body).await;
    assert_eq!(v["data"]["revise_deferred"], true, "{status} {v}");
    let (_, v) = observe(f, &i, &p, &result, R, "contained").await;
    (
        p,
        json!({"id": v["data"]["revise"]["follow_up_task_id"]}),
        i,
    )
}

// P4 S4e (red team B1): the agent that decided a revert, the reverted
// task's author, and the author whose withdraw lost to the push are
// contributors and cannot review it.
#[tokio::test]
async fn a_revert_creator_cannot_review_it() {
    let f = Fixture::new().await;
    let (p, _, result, i) = published_subject(&f, "revert-creator").await;
    let revert = reverted(&f, &f.b, &p, &result, ("defect", json!("tip fails"))).await;
    let body = candidate_body(R, &candidate(0xfeed));
    let (_, v) = revert_candidate(&f, &i, &p, &revert, body).await;
    let (status, claimed) = claim_revert_review(&f, &f.b, &p, &v["data"]).await;
    let expected = (StatusCode::CONFLICT, json!("reviewer_not_independent"));
    assert_eq!(refusal(status, &claimed), expected, "{claimed}");
    let (status, claimed) = claim_revert_review(&f, &f.a, &p, &v["data"]).await;
    assert_eq!(refusal(status, &claimed), expected, "{claimed}");

    let f = Fixture::new().await;
    let (p, revert, i) = withdrawn_landing(&f, "revert-requester").await;
    let body = candidate_body(R, &candidate(0xfeed));
    let (_, v) = revert_candidate(&f, &i, &p, &revert, body).await;
    let (status, claimed) = claim_revert_review(&f, &f.a, &p, &v["data"]).await;
    assert_eq!(refusal(status, &claimed), expected, "{claimed}");
}

// P4 S4e (red team B2): a deferred author_withdraw whose no-op result is
// observed contained completes as already_contained with an ordinary
// follow-up; nothing new landed, so nothing is reverted.
#[tokio::test]
async fn a_withdraw_before_a_no_op_keeps_an_ordinary_follow_up() {
    let f = Fixture::new().await;
    let (p, t, integration, i) = integrator_task(&f, "revert-withdraw-noop").await;
    let submission = &integration["submission_id"];
    let (status, v) = integrator_post(
        &f,
        &i,
        &p,
        "results",
        result_body(submission.as_str().unwrap(), R, R),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let result = v["data"]["id"].as_str().unwrap().to_owned();
    receipt(&f, &i, &p, &result, 900, "success").await;
    let (status, v) = authority(&f, &i, &p, &result).await;
    assert_eq!(v["data"]["granted"], true, "{status} {v}");
    f.ack(&f.a, &p, 3).await;
    let path = format!(
        "/api/v1/projects/{p}/tasks/{}/workflow/reopen",
        t["id"].as_str().unwrap()
    );
    let body = revise_body(submission, "author_withdraw", Some("wrong approach"));
    let (_, v) = f.call(&f.a, "POST", &path, body).await;
    assert_eq!(v["data"]["revise_deferred"], true, "{v}");
    let (status, v) = observe(&f, &i, &p, &result, R, "contained").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["disposition"], "already_contained", "{v}");
    let follow_up = json!({"id": v["data"]["revise"]["follow_up_task_id"]});
    let view = fresh_task(&f, &p, &follow_up).await;
    assert_eq!(view["revert"], Value::Null, "{view}");
}

// P4 S4e (red team S2, A2): the reviewer sees the integrator's attestation;
// a review that requests changes rejects the decision to revert, which
// cancels the revert, clears reverted_by and leaves the queue.
#[tokio::test]
async fn a_rejected_revert_review_cancels_the_revert() {
    let f = Fixture::new().await;
    let (p, t, result, i) = published_subject(&f, "revert-rejected").await;
    let revert = reverted(&f, &f.b, &p, &result, ("defect", json!("tip fails"))).await;
    let commit = candidate(0xfeed);
    let (_, v) = revert_candidate(&f, &i, &p, &revert, candidate_body(R, &commit)).await;
    let view = fresh_task(&f, &p, &revert).await;
    assert_eq!(
        view["revert"]["candidates"][0]["candidate_commit"],
        json!(commit),
        "{view}"
    );
    assert_eq!(
        view["revert"]["candidates"][0]["attestation"], "mechanical",
        "{view}"
    );
    let review = activity(&v["data"], "agent_review").clone();
    let (status, claimed) = claim_revert_review(&f, &f.c, &p, &v["data"]).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    let path = activity_path(&p, &review, "review");
    let (status, decided) = f.call(&f.c, "POST", &path, json!({"generation":claimed["data"]["attempt"]["generation"],"submission_id":review["submission_id"],"decision":"changes_requested","summary":"the evidence does not show a defect","findings":[]})).await;
    assert_eq!(status, StatusCode::OK, "{decided}");
    let view = fresh_task(&f, &p, &revert).await;
    assert_eq!(view["lifecycle"], "canceled", "{view}");
    assert_eq!(
        view["revert"]["rejection"]["summary"],
        "the evidence does not show a defect"
    );
    assert_eq!(fresh_task(&f, &p, &t).await["reverted_by"], Value::Null);
    assert_eq!(integrator_queue(&f, &i, &p).await["reverts"], json!([]));
    let (status, v) = revert_candidate(&f, &i, &p, &revert, candidate_body(R, &commit)).await;
    let expected = (StatusCode::CONFLICT, json!("revert_not_open"));
    assert_eq!(refusal(status, &v), expected, "{v}");
}

// P4 S4e (red team A1): nobody claims or unblocks a mechanical revert, and
// a revise of its candidate blocks it again for the integrator.
#[tokio::test]
async fn a_mechanical_revert_cannot_be_claimed_or_unblocked() {
    let f = Fixture::new().await;
    let (p, _, result, i) = published_subject(&f, "revert-mechanical").await;
    let revert = reverted(&f, &f.admin, &p, &result, ("human", Value::Null)).await;
    let path = format!(
        "/api/v1/projects/{p}/tasks/{}/unblock",
        revert["id"].as_str().unwrap()
    );
    let body = json!({"expected_revision":revert["revision"],"reason":"claim it by hand"});
    let (status, v) = f.call(&f.b, "POST", &path, body).await;
    let expected = (StatusCode::CONFLICT, json!("revert_awaits_integrator"));
    assert_eq!(refusal(status, &v), expected, "{v}");
    f.ack(&f.b, &p, 3).await;
    let claim = json!({"task_id":revert["id"],"expected_task_revision":revert["revision"],"mode":"work","policy_revision":3,"instruction_version":coordinator_core::INSTRUCTION_VERSION});
    let (status, v) = f
        .call(&f.b, "POST", &format!("/api/v1/projects/{p}/claims"), claim)
        .await;
    assert_eq!(refusal(status, &v), expected, "{v}");
    let (_, v) = revert_candidate(&f, &i, &p, &revert, candidate_body(R, &candidate(0xfeed))).await;
    let submission = &v["data"]["candidate_submission_id"];
    let (status, v) = conflict_revise(&f, &i, &p, submission, None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(fresh_task(&f, &p, &revert).await["work_status"], "blocked");
    assert_eq!(
        integrator_queue(&f, &i, &p).await["reverts"][0]["id"],
        revert["id"]
    );
}

// P4 S4e (red-team probe D): a deferred author_withdraw on a no-op result
// (r == t0) that holds push authority is observed contained at T0 as
// already_contained.
#[tokio::test]
async fn a_deferred_withdraw_on_a_no_op_result_is_observed_as_already_contained() {
    let f = Fixture::new().await;
    let (p, t, integration, i) = integrator_task(&f, "revert-probe-noop-withdraw").await;
    let submission = &integration["submission_id"];
    let body = result_body(submission.as_str().unwrap(), T0, T0);
    let (status, v) = integrator_post(&f, &i, &p, "results", body).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let noop = v["data"]["id"].as_str().unwrap().to_owned();
    let body = json!({"result_id":noop,"check_name":CHECK,"run_id":960,"run_attempt":1,"head_sha":T0,"app_id":15368,"workflow_path":".github/workflows/checks.yml","workflow_blob":BLOB,"conclusion":"success"});
    let (status, v) = integrator_post(&f, &i, &p, "receipts", body).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (status, v) = authority(&f, &i, &p, &noop).await;
    assert_eq!(v["data"]["granted"], true, "{status} {v}");
    f.ack(&f.a, &p, 3).await;
    let path = format!(
        "/api/v1/projects/{p}/tasks/{}/workflow/reopen",
        t["id"].as_str().unwrap()
    );
    let body = revise_body(submission, "author_withdraw", Some("wrong"));
    let (status, v) = f.call(&f.a, "POST", &path, body).await;
    assert_eq!(v["data"]["revise_deferred"], true, "{status} {v}");
    let (status, v) = observe(&f, &i, &p, &noop, T0, "contained").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["disposition"], "already_contained", "{v}");
}

/// The reopen path of task `t` in `p`.
fn reopen_path(p: &str, t: &Value) -> String {
    format!(
        "/api/v1/projects/{p}/tasks/{}/workflow/reopen",
        t["id"].as_str().unwrap()
    )
}

/// An integrating subject whose result R1 holds push authority at T0, with
/// an agent revise deferred on it and then a human reopen of its
/// submission: the project, the task, R1 and the integrator.
async fn reopened_under_authority(f: &Fixture, name: &str) -> (String, Value, String, Caller) {
    let (p, t, integration, i) = integrator_task(f, name).await;
    let submission = &integration["submission_id"];
    let result = authorized(f, &i, &p, submission, T0).await;
    f.ack(&f.a, &p, 3).await;
    let body = revise_body(submission, "author_withdraw", None);
    let (status, v) = f.call(&f.a, "POST", &reopen_path(&p, &t), body).await;
    assert_eq!(v["data"]["revise_deferred"], true, "{status} {v}");
    let body = json!({"submission_id":submission,"reason":"a human takes it back"});
    let (status, v) = f.call(&f.admin, "POST", &reopen_path(&p, &t), body).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    (p, t, result, i)
}

/// `resolved_at` and `resolution` of the revise request deferred on the
/// submission of `result`.
async fn revise_request(f: &Fixture, result: &str) -> (Option<i64>, Option<String>) {
    sqlx::query_as(
        "SELECT resolved_at,resolution FROM integrator_revise_requests WHERE result_id=?",
    )
    .bind(result)
    .fetch_one(&f.state.pool)
    .await
    .unwrap()
}

// P4 S5 review: a revise deferred on a submission a human reopen then
// superseded is moot. Observing its result neither applies it to the
// author's newer submission nor leaves it pending, whether or not R landed.
#[tokio::test]
async fn s5_review_a_revise_on_a_reopened_submission_is_moot() {
    let f = Fixture::new().await;
    let (p, t, r1, i) = reopened_under_authority(&f, "s5-moot-not-published").await;
    submit_code(&f, &p, &t, &candidate(0x51)).await;
    assert_eq!(subject_state(&f, &t).await.0, "integration");
    let (status, v) = observe(&f, &i, &p, &r1, T0, "equal_t0").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["revise"]["resolution"], "moot", "{v}");
    assert_eq!(subject_state(&f, &t).await.0, "integration");
    let (resolved, resolution) = revise_request(&f, &r1).await;
    assert!(resolved.is_some() && resolution.is_none());

    let f = Fixture::new().await;
    let (p, _, r1, i) = reopened_under_authority(&f, "s5-moot-published").await;
    let (_, v) = observe(&f, &i, &p, &r1, R, "contained").await;
    assert_eq!(v["data"]["disposition"], "published_after_reopen", "{v}");
    assert_eq!(v["data"]["revise"]["resolution"], "moot", "{v}");
    assert!(revise_request(&f, &r1).await.0.is_some());
}

// P4 S5 review: push authority waits for a privilege_gate report on the
// result to be resolved allow; an open or denied gate refuses it.
#[tokio::test]
async fn s5_review_push_authority_waits_for_the_privilege_gate() {
    for (name, decision) in [("s5-gate-allow", "allow"), ("s5-gate-deny", "deny")] {
        let f = Fixture::new().await;
        let (p, _, integration, i) = integrator_task(&f, name).await;
        let result = pinned_result(&f, &i, &p, &integration["submission_id"], T0).await;
        receipt(&f, &i, &p, &result, 900, "success").await;
        let (_, gate) = report(&f, &i, &p, "privilege_gate", name, Some(&result), json!({})).await;
        let (status, v) = authority(&f, &i, &p, &result).await;
        let expected = (StatusCode::CONFLICT, json!("privilege_gate_unresolved"));
        assert_eq!(refusal(status, &v), expected, "{v}");
        assert_eq!(v["error"]["details"]["report_id"], gate["data"]["id"]);
        let body = json!({"note":"checked","decision":decision});
        let (status, v) = resolve(&f, &f.admin, &p, &gate["data"]["id"], body).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        let (status, v) = authority(&f, &i, &p, &result).await;
        if decision == "allow" {
            assert_eq!(v["data"]["granted"], true, "{status} {v}");
        } else {
            let expected = (StatusCode::CONFLICT, json!("privilege_gate_denied"));
            assert_eq!(refusal(status, &v), expected, "{v}");
        }
    }
}

// P4 S5 review: only a human cancels a revert task, even on a project that
// delegates canceling to agents and for the reverted task's author.
#[tokio::test]
async fn s5_review_only_a_human_cancels_a_revert() {
    let f = Fixture::new().await;
    let (p, _, result, _) = published_subject(&f, "s5-revert-cancel").await;
    let revert = reverted(&f, &f.admin, &p, &result, ("human", Value::Null)).await;
    let path = format!(
        "/api/v1/projects/{p}/tasks/{}/cancel",
        revert["id"].as_str().unwrap()
    );
    let body = json!({"expected_revision":revert["revision"],"reason":"not needed"});
    for agent in [&f.a, &f.b] {
        let (status, v) = f.call(agent, "POST", &path, body.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
        assert_eq!(v["error"]["details"]["gate"], "revert_cancel", "{v}");
        assert_eq!(v["error"]["details"]["required_actor"], "human", "{v}");
    }
    let (status, v) = f.call(&f.admin, "POST", &path, body).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["lifecycle"], "canceled", "{v}");
}

// P4 S5 review: once a revise sends a mechanical revert's candidate back
// while the tip is unchanged, the integrator's identical candidate for that
// tip is recorded as a new submission; a different one still conflicts.
#[tokio::test]
async fn s5_review_a_sent_back_revert_candidate_is_recorded_again() {
    let f = Fixture::new().await;
    let (p, _, result, i) = published_subject(&f, "s5-revert-resend").await;
    let revert = reverted(&f, &f.admin, &p, &result, ("human", Value::Null)).await;
    let body = candidate_body(R, &candidate(0xfeed));
    let (_, v) = revert_candidate(&f, &i, &p, &revert, body.clone()).await;
    let first = v["data"]["candidate_submission_id"].clone();
    let (status, v) = conflict_revise(&f, &i, &p, &first, None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let other = candidate_body(R, &candidate(0xbeef));
    let (status, v) = revert_candidate(&f, &i, &p, &revert, other).await;
    let expected = (StatusCode::CONFLICT, json!("revert_candidate_conflict"));
    assert_eq!(refusal(status, &v), expected, "{v}");
    let (status, v) = revert_candidate(&f, &i, &p, &revert, body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let second = v["data"]["candidate_submission_id"].clone();
    assert!(second.is_string() && second != first, "{v}");
    assert_eq!(v["data"]["phase"], "integration", "{v}");
    let (_, again) = revert_candidate(&f, &i, &p, &revert, body).await;
    assert_eq!(again["data"]["candidate_submission_id"], second, "{again}");
}

// P4 S5 review: an observation of a result other than the one holding push
// authority is refused and leaves that authority outstanding, and R is
// observed published only while its result holds authority.
#[tokio::test]
async fn s5_review_observations_bind_to_the_result_holding_authority() {
    let f = Fixture::new().await;
    let (p, t, integration, i) = integrator_task(&f, "s5-observe-bound").await;
    let submission = &integration["submission_id"];
    let other_tip = "8888888888888888888888888888888888888888";
    let other = pinned_result(&f, &i, &p, submission, other_tip).await;
    let held = authorized(&f, &i, &p, submission, T0).await;
    let (status, v) = observe(&f, &i, &p, &other, other_tip, "equal_t0").await;
    let expected = (StatusCode::CONFLICT, json!("observation_required"));
    assert_eq!(refusal(status, &v), expected, "{v}");
    assert_eq!(held_holds(&f, &p).await, 1);
    let (status, v) = observe(&f, &i, &p, &held, T0, "equal_t0").await;
    assert_eq!(v["data"]["disposition"], "not_published", "{status} {v}");
    let (status, v) = observe(&f, &i, &p, &held, R, "contained").await;
    let expected = (StatusCode::CONFLICT, json!("authority_not_issued"));
    assert_eq!(refusal(status, &v), expected, "{v}");
    assert_eq!(subject_state(&f, &t).await.0, "integration");
}

// P4 S5 review: a review that rejects a mechanical revert cancels it as a
// new task revision, recorded in the task's revision history.
#[tokio::test]
async fn s5_review_a_rejected_revert_is_a_new_task_revision() {
    let f = Fixture::new().await;
    let (p, _, result, i) = published_subject(&f, "s5-revert-revision").await;
    let revert = reverted(&f, &f.b, &p, &result, ("defect", json!("tip fails"))).await;
    let body = candidate_body(R, &candidate(0xfeed));
    let (_, v) = revert_candidate(&f, &i, &p, &revert, body).await;
    let before = fresh_task(&f, &p, &revert).await["revision"]
        .as_i64()
        .unwrap();
    let review = activity(&v["data"], "agent_review").clone();
    let (_, claimed) = claim_revert_review(&f, &f.c, &p, &v["data"]).await;
    let path = activity_path(&p, &review, "review");
    let (status, decided) = f.call(&f.c, "POST", &path, json!({"generation":claimed["data"]["attempt"]["generation"],"submission_id":review["submission_id"],"decision":"changes_requested","summary":"no defect shown","findings":[]})).await;
    assert_eq!(status, StatusCode::OK, "{decided}");
    let view = fresh_task(&f, &p, &revert).await;
    assert_eq!(view["lifecycle"], "canceled", "{view}");
    assert_eq!(view["revision"], before + 1, "{view}");
    let saved: String =
        sqlx::query_scalar("SELECT data_json FROM task_revisions WHERE task_id=? AND revision=?")
            .bind(revert["id"].as_str().unwrap())
            .bind(before + 1)
            .fetch_one(&f.state.pool)
            .await
            .unwrap();
    assert!(saved.contains(r#""lifecycle":"canceled""#), "{saved}");
}
