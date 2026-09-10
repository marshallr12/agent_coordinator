use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use coordinator_server::{
    auth::{digest, secret},
    router,
    state::{AppState, Config},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

struct Fixture {
    state: AppState,
    app: Router,
    _dir: tempfile::TempDir,
    token: String,
    principal: String,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::open(Config {
            database_path: dir.path().join("history.sqlite3"),
            public_origin: "http://127.0.0.1:8080".into(),
            allow_insecure_loopback: true,
            ..Config::default()
        })
        .await
        .unwrap();
        let token = secret();
        let principal = Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO principals(id,name,kind,role,password_hash,created_at) VALUES(?,?,'human','admin','unused',?)")
            .bind(&principal).bind("history-admin").bind(state.now()).execute(&state.pool).await.unwrap();
        sqlx::query(
            "INSERT INTO browser_sessions(id,principal_id,token_hash,expires_at) VALUES(?,?,?,?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(&principal)
        .bind(digest(&token))
        .bind(state.now() + 86_400_000)
        .execute(&state.pool)
        .await
        .unwrap();
        Self {
            app: router(state.clone()),
            state,
            _dir: dir,
            token,
            principal,
        }
    }

    async fn project_task(&self, label: &str) -> (String, String) {
        let project = Uuid::new_v4().to_string();
        let task = Uuid::new_v4().to_string();
        let now = self.state.now();
        sqlx::query("INSERT INTO projects(id,name,repository_url,target_branch,created_at) VALUES(?,?,?,?,?)")
            .bind(&project).bind(format!("history-{label}-{project}"))
            .bind(format!("https://example.test/{project}.git")).bind("main").bind(now)
            .execute(&self.state.pool).await.unwrap();
        sqlx::query("INSERT INTO tasks(id,project_id,title,description,acceptance_json,kind,priority,lifecycle,created_at,ready_since) VALUES(?,?,?,'','[]','general',2,'planned',?,?)")
            .bind(&task).bind(&project).bind(format!("Task {label}")).bind(now).bind(now)
            .execute(&self.state.pool).await.unwrap();
        (project, task)
    }

    async fn get(&self, path: &str) -> (StatusCode, Value) {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(path)
                    .header("cookie", format!("coordinator_local={}", self.token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    async fn history(
        &self,
        project: &str,
        task: &str,
        kind: &str,
        limit: i64,
        cursor: Option<&str>,
    ) -> Value {
        let mut path =
            format!("/api/v1/projects/{project}/tasks/{task}/history?kind={kind}&limit={limit}");
        if let Some(cursor) = cursor {
            path.push_str("&cursor=");
            path.push_str(cursor);
        }
        let (status, value) = self.get(&path).await;
        assert_eq!(status, StatusCode::OK, "{value}");
        value["data"].clone()
    }

    async fn attempt(&self, project: &str, task: &str, generation: i64, label: &str) -> String {
        let id = Uuid::new_v4().to_string();
        let now = self.state.now() + generation;
        sqlx::query("INSERT INTO attempts(id,project_id,task_id,owner_id,session_id,credential_id,generation,state,mode,expires_at,last_heartbeat_at,last_progress_at,created_at,ended_at,outcome,task_revision,policy_revision) VALUES(?,?,?,?,?,NULL,?,'released','work',?,?,?,?,?,'historical outcome',1,1)")
            .bind(&id).bind(project).bind(task).bind(&self.principal).bind(format!("secret-session-{label}"))
            .bind(generation).bind(now+60_000).bind(now).bind(now).bind(now).bind(now+1)
            .execute(&self.state.pool).await.unwrap();
        id
    }
}

#[tokio::test]
async fn high_volume_pages_have_no_gaps_and_exclude_concurrent_inserts() {
    let f = Fixture::new().await;
    let (project, task) = f.project_task("volume").await;
    let mut expected_attempts = Vec::new();
    for generation in 1..=75 {
        expected_attempts.push(f.attempt(&project, &task, generation, "volume").await);
    }
    for index in 0..125 {
        let attempt = &expected_attempts[index % expected_attempts.len()];
        sqlx::query("INSERT INTO checkpoints(id,project_id,attempt_id,summary,current_action,next_step,blockers_json,created_at) VALUES(?,?,?,?,?,?,?,?)")
            .bind(Uuid::new_v4().to_string()).bind(&project).bind(attempt)
            .bind(format!("checkpoint-{index:03}")).bind("inspect").bind("continue").bind("[]")
            .bind(f.state.now()+index as i64).execute(&f.state.pool).await.unwrap();
    }

    let first = f.history(&project, &task, "attempts", 20, None).await;
    let cursor = first["next_cursor"].as_str().unwrap().to_owned();
    let inserted_late = f.attempt(&project, &task, 76, "late").await;
    let snapshot = first["snapshot"].clone();
    let mut ids: Vec<String> = first["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["record"]["id"].as_str().unwrap().to_owned())
        .collect();
    let mut next = Some(cursor);
    while let Some(cursor) = next {
        let page = f
            .history(&project, &task, "attempts", 20, Some(&cursor))
            .await;
        assert_eq!(page["snapshot"], snapshot);
        ids.extend(
            page["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["record"]["id"].as_str().unwrap().to_owned()),
        );
        next = page["next_cursor"].as_str().map(str::to_owned);
    }
    assert_eq!(ids.len(), 75);
    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        75
    );
    assert!(!ids.contains(&inserted_late));
    assert!(expected_attempts.iter().all(|id| ids.contains(id)));

    let mut summaries = Vec::new();
    let mut next = None;
    loop {
        let page = f
            .history(&project, &task, "checkpoints", 37, next.as_deref())
            .await;
        summaries.extend(
            page["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["record"]["summary"].as_str().unwrap().to_owned()),
        );
        next = page["next_cursor"].as_str().map(str::to_owned);
        if next.is_none() {
            break;
        }
    }
    assert_eq!(summaries.len(), 125);
    assert_eq!(
        summaries
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        125
    );
}

#[tokio::test]
async fn cursor_and_task_relationships_never_cross_project_or_kind() {
    let f = Fixture::new().await;
    let (project, task) = f.project_task("one").await;
    let (other_project, other_task) = f.project_task("two").await;
    for generation in 1..=3 {
        f.attempt(&project, &task, generation, "one").await;
    }
    let page = f.history(&project, &task, "attempts", 1, None).await;
    let cursor = page["next_cursor"].as_str().unwrap();
    for path in [
        format!(
            "/api/v1/projects/{other_project}/tasks/{other_task}/history?kind=attempts&limit=1&cursor={cursor}"
        ),
        format!(
            "/api/v1/projects/{project}/tasks/{task}/history?kind=checkpoints&limit=1&cursor={cursor}"
        ),
    ] {
        let (status, value) = f.get(&path).await;
        assert_eq!(status, StatusCode::CONFLICT, "{value}");
        assert_eq!(value["error"]["code"], "history_cursor_mismatch");
    }
    let (status, _) = f
        .get(&format!(
            "/api/v1/projects/{other_project}/tasks/{task}/history?kind=attempts&limit=10"
        ))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn operational_evidence_is_complete_but_authentication_material_is_redacted() {
    let f = Fixture::new().await;
    let (project, task) = f.project_task("evidence").await;
    let attempt = f.attempt(&project, &task, 1, "auth-redaction").await;
    sqlx::query("INSERT INTO checkouts(attempt_id,project_id,workstation_id,identity,path,branch,base_revision,created_at) VALUES(?,?,?,?,?,?,?,?)")
        .bind(&attempt).bind(&project).bind("workstation-a").bind("repo-identity")
        .bind("/worktree with spaces").bind("task/history").bind("abc123").bind(f.state.now())
        .execute(&f.state.pool).await.unwrap();
    let resource = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO resources(id,key,capacity,description,created_by,created_at) VALUES(?,'history/device',1,'Historical device',?,?)")
        .bind(&resource).bind(&f.principal).bind(f.state.now()).execute(&f.state.pool).await.unwrap();
    let reservation = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO reservations(id,project_id,attempt_id,generation,state,created_by,created_at,released_at,released_by,release_reason) VALUES(?,?,?,1,'released',?,?,?,?,?)")
        .bind(&reservation).bind(&project).bind(&attempt).bind(&f.principal).bind(f.state.now())
        .bind(f.state.now()+1).bind(&f.principal).bind("producer ended").execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO reservation_items(reservation_id,resource_id,units) VALUES(?,?,1)")
        .bind(&reservation)
        .bind(&resource)
        .execute(&f.state.pool)
        .await
        .unwrap();
    let job = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO jobs(id,producer_id,project_id,task_id,attempt_id,generation,runner_instance_id,workstation_id,label,source_revision,source_tree,reservation_id,state,last_sequence,last_observed_at,pid,process_started_at,exit_code,inputs_unchanged,summary,created_at,check_identity,check_version,check_environment) VALUES(?,?,?,?,?,1,'runner','workstation-a','history check','revision','tree',?,'succeeded',1,?,42,'start-token',0,1,'complete evidence',?,'acceptance','1','linux')")
        .bind(&job).bind("producer-public-id").bind(&project).bind(&task).bind(&attempt).bind(&reservation)
        .bind(f.state.now()).bind(f.state.now()).execute(&f.state.pool).await.unwrap();
    let reporter = Uuid::new_v4().to_string();
    let credential = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO credentials(id,principal_id,token_hash,created_at) VALUES(?,?,?,?)")
        .bind(&credential)
        .bind(&f.principal)
        .bind(digest("credential-secret-sentinel"))
        .bind(f.state.now())
        .execute(&f.state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO agent_sessions(id,principal_id,credential_id,workstation_id,proof_hash,created_at,capabilities,harness) VALUES(?,?,?,?,?,?,'[]','history-test')")
        .bind("reporter-session-secret")
        .bind(&f.principal)
        .bind(&credential)
        .bind("workstation-a")
        .bind("session-proof-secret")
        .bind(f.state.now())
        .execute(&f.state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO reporters(id,job_id,principal_id,credential_id,session_id,proof_hash,expires_at,renew_until,created_at) VALUES(?,?,?,?,?,'proof-secret-sentinel',?,?,?)")
        .bind(&reporter).bind(&job).bind(&f.principal).bind(&credential).bind("reporter-session-secret")
        .bind(f.state.now()+60_000).bind(f.state.now()+60_000).bind(f.state.now()).execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO job_observations(reporter_id,sequence,request_hash,producer_id,state,pid,process_started_at,exit_code,inputs_unchanged,summary,observed_at) VALUES(?,1,'request-secret-sentinel','producer-public-id','succeeded',42,'start-token',0,1,'original observation evidence',?)")
        .bind(&reporter).bind(f.state.now()).execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO artifacts(id,project_id,kind,task_id,display_name,media_type,size_bytes,sha256,storage_key,state,created_by,created_at,finalized_at,retention_until,pinned) VALUES(?,?,'upload',?,'evidence.bin','application/octet-stream',4,?,'storage-secret-sentinel','finalized',?,?,?,?,0)")
        .bind(Uuid::new_v4().to_string()).bind(&project).bind(&task).bind("a".repeat(64))
        .bind(&f.principal).bind(f.state.now()).bind(f.state.now()).bind(f.state.now()+60_000).execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO events(project_id,actor_id,kind,record_id,data_json,created_at) VALUES(?,?,'job.observed',?,?,?)")
        .bind(&project)
        .bind(&f.principal)
        .bind(&job)
        .bind(json!({
            "evidence": "retained event evidence",
            "nested": {
                "proof": "event-proof-secret",
                "token": "event-token-secret"
            }
        }).to_string())
        .bind(f.state.now())
        .execute(&f.state.pool)
        .await
        .unwrap();

    // Artifact mutations have their own record identity. They belong in the
    // task's event history through either a task or producer association.
    for (label, task_link, job_link) in [
        ("task", Some(task.as_str()), None),
        ("job", None, Some(job.as_str())),
        ("unrelated", None, None),
    ] {
        let artifact = Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO artifacts(id,project_id,kind,task_id,job_id,display_name,media_type,external_url,state,created_by,created_at,finalized_at) VALUES(?,?,'external_link',?,?,'history link','text/plain','https://example.test/log','finalized',?,?,?)")
            .bind(&artifact).bind(&project).bind(task_link).bind(job_link)
            .bind(&f.principal).bind(f.state.now()).bind(f.state.now()).execute(&f.state.pool).await.unwrap();
        sqlx::query("INSERT INTO events(project_id,actor_id,kind,record_id,data_json,created_at) VALUES(?,?,'artifact.linked',?,?,?)")
            .bind(&project).bind(&f.principal).bind(&artifact)
            .bind(json!({"evidence":format!("{label} artifact event")}).to_string())
            .bind(f.state.now()).execute(&f.state.pool).await.unwrap();
    }
    let artifact_events = f
        .history(&project, &task, "events", 100, None)
        .await
        .to_string();
    assert!(artifact_events.contains("task artifact event"));
    assert!(artifact_events.contains("job artifact event"));
    assert!(!artifact_events.contains("unrelated artifact event"));

    let mut combined = String::new();
    for kind in [
        "attempts",
        "checkouts",
        "jobs",
        "job_observations",
        "resources",
        "artifacts",
        "events",
    ] {
        let page = f.history(&project, &task, kind, 100, None).await;
        assert!(
            !page["items"].as_array().unwrap().is_empty(),
            "missing {kind}"
        );
        combined.push_str(&page.to_string());
    }
    assert!(combined.contains("original observation evidence"));
    assert!(combined.contains("Historical device"));
    assert!(combined.contains("retained event evidence"));
    for secret in [
        "secret-session-auth-redaction",
        "credential-secret-sentinel",
        "reporter-session-secret",
        "proof-secret-sentinel",
        "request-secret-sentinel",
        "storage-secret-sentinel",
        "session-proof-secret",
        "event-proof-secret",
        "event-token-secret",
    ] {
        assert!(!combined.contains(secret), "history leaked {secret}");
    }
}

#[tokio::test]
async fn subject_and_activity_routes_expose_immutable_workflow_evidence() {
    let f = Fixture::new().await;
    let (project, subject) = f.project_task("workflow").await;
    let (_, unrelated_task) = f.project_task("unrelated-project").await;
    let subject_attempt = f.attempt(&project, &subject, 1, "subject").await;
    let review_task = Uuid::new_v4().to_string();
    let integration_task = Uuid::new_v4().to_string();
    for (id, title) in [
        (&review_task, "Review activity"),
        (&integration_task, "Integration activity"),
    ] {
        sqlx::query("INSERT INTO tasks(id,project_id,title,description,acceptance_json,kind,priority,lifecycle,created_at,ready_since) VALUES(?,?,?,'','[]','general',2,'done',?,?)")
            .bind(id).bind(&project).bind(title).bind(f.state.now()).bind(f.state.now()).execute(&f.state.pool).await.unwrap();
    }
    let review_attempt = f
        .attempt(&project, &review_task, 1, "review-private-session")
        .await;
    let integration_attempt = f
        .attempt(&project, &integration_task, 1, "integration")
        .await;
    let integration_resource = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO resources(id,key,capacity,description,created_by,created_at) VALUES(?, ?,1,'Integration check resource',?,?)")
        .bind(&integration_resource)
        .bind(format!("history/integration/{integration_resource}"))
        .bind(&f.principal)
        .bind(f.state.now())
        .execute(&f.state.pool)
        .await
        .unwrap();
    let integration_reservation = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO reservations(id,project_id,attempt_id,generation,state,created_by,created_at,released_at,released_by,release_reason) VALUES(?,?,?,1,'released',?,?,?,?,?)")
        .bind(&integration_reservation)
        .bind(&project)
        .bind(&integration_attempt)
        .bind(&f.principal)
        .bind(f.state.now())
        .bind(f.state.now() + 1)
        .bind(&f.principal)
        .bind("integration check completed")
        .execute(&f.state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO reservation_items(reservation_id,resource_id,units) VALUES(?,?,1)")
        .bind(&integration_reservation)
        .bind(&integration_resource)
        .execute(&f.state.pool)
        .await
        .unwrap();
    let integration_check_job = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO jobs(id,producer_id,project_id,task_id,attempt_id,generation,runner_instance_id,workstation_id,label,source_revision,source_tree,reservation_id,state,last_sequence,last_observed_at,pid,process_started_at,exit_code,inputs_unchanged,summary,created_at,check_identity,check_version,check_environment) VALUES(?,?,?,?,?,1,'integration-runner','workstation-a','required integration check','result-revision','result-tree',?,'succeeded',1,?,42,'integration-start-token',0,1,'exact check result evidence',?,'required-check','v2','linux')")
        .bind(&integration_check_job)
        .bind("integration-check-producer")
        .bind(&project)
        .bind(&integration_task)
        .bind(&integration_attempt)
        .bind(&integration_reservation)
        .bind(f.state.now())
        .bind(f.state.now())
        .execute(&f.state.pool)
        .await
        .unwrap();
    let submission = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO submissions(id,project_id,task_id,attempt_id,kind,task_revision,project_policy_revision,workflow_policy_revision,summary,acceptance_evidence_json,handoff,created_by,contributor_session_id,created_at) VALUES(?,?,?,?,'general',1,1,0,'complete submission summary',?,'original handoff evidence',?,'contributor-session-secret',?)")
        .bind(&submission).bind(&project).bind(&subject).bind(&subject_attempt)
        .bind(json!([{"criterion":"history visible","evidence":"original acceptance evidence"}]).to_string())
        .bind(&f.principal).bind(f.state.now()).execute(&f.state.pool).await.unwrap();
    let review = Uuid::new_v4().to_string();
    let integration = Uuid::new_v4().to_string();
    for (id, task, kind) in [
        (&review, &review_task, "agent_review"),
        (&integration, &integration_task, "integration"),
    ] {
        sqlx::query("INSERT INTO workflow_activities(id,project_id,subject_task_id,submission_id,activity_task_id,kind,slot,state,created_at,completed_at) VALUES(?,?,?,?,?,?,1,'completed',?,?)")
            .bind(id).bind(&project).bind(&subject).bind(&submission).bind(task).bind(kind)
            .bind(f.state.now()).bind(f.state.now()+1).execute(&f.state.pool).await.unwrap();
    }
    sqlx::query("INSERT INTO review_decisions(activity_id,submission_id,attempt_id,reviewer_id,reviewer_session_id,decision,summary,created_at) VALUES(?,?,?,?,'reviewer-session-secret','approved','original review summary',?)")
        .bind(&review).bind(&submission).bind(&review_attempt).bind(&f.principal).bind(f.state.now()).execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO review_findings(id,activity_id,severity,remedy,evidence,created_at) VALUES(?,?,'advisory','retain history','original finding evidence',?)")
        .bind(Uuid::new_v4().to_string()).bind(&review).bind(f.state.now()).execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO integration_authorizations(activity_id,submission_id,project_policy_revision,workflow_policy_revision,actor_id,summary,created_at) VALUES(?,?,1,0,?,'authorization evidence',?)")
        .bind(&integration).bind(&submission).bind(&f.principal).bind(f.state.now()).execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO integration_holds(id,activity_id,canonical_repository_key,target_branch,state,acquired_by,acquired_at,released_by,released_at,release_reason) VALUES(?,?,'repo/key','main','released',?,?,?,?,?)")
        .bind(Uuid::new_v4().to_string()).bind(&integration).bind(&f.principal).bind(f.state.now()).bind(&f.principal).bind(f.state.now()+1).bind("published").execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO publication_intents(activity_id,submission_id,attempt_id,observed_target_revision,observed_target_tree,result_revision,result_tree,created_by,created_at) VALUES(?,?,?,'base-revision','base-tree','result-revision','result-tree',?,?)")
        .bind(&integration).bind(&submission).bind(&integration_attempt).bind(&f.principal).bind(f.state.now()).execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO integration_results(activity_id,submission_id,attempt_id,publication_state,observed_target_revision,result_revision,result_tree,check_job_ids_json,summary,reported_by,created_at) VALUES(?,?,?,'published','base-revision','result-revision','result-tree',?,'integration result evidence',?,?)")
        .bind(&integration)
        .bind(&submission)
        .bind(&integration_attempt)
        .bind(json!([integration_check_job]).to_string())
        .bind(&f.principal)
        .bind(f.state.now())
        .execute(&f.state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO publication_reconciliations(activity_id,submission_id,disposition,observed_target_revision,observed_target_tree,evidence,actor_id,created_at) VALUES(?,?,'published','result-revision','result-tree','remote observation evidence',?,?)")
        .bind(&integration).bind(&submission).bind(&f.principal).bind(f.state.now()).execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO events(project_id,actor_id,kind,record_id,data_json,created_at) VALUES(?,?,'review.decided',?,'{\"original\":true}',?)")
        .bind(&project).bind(&f.principal).bind(&review).bind(f.state.now()).execute(&f.state.pool).await.unwrap();

    let submissions = f.history(&project, &subject, "submissions", 20, None).await;
    let reviews = f.history(&project, &subject, "reviews", 20, None).await;
    let integrations = f
        .history(&project, &subject, "integrations", 20, None)
        .await;
    let activity_attempts = f
        .history(&project, &review_task, "attempts", 20, None)
        .await;
    let events = f.history(&project, &subject, "events", 20, None).await;
    assert!(
        submissions
            .to_string()
            .contains("original acceptance evidence")
    );
    assert!(
        submissions
            .to_string()
            .contains("original handoff evidence")
    );
    assert!(reviews.to_string().contains("original review summary"));
    assert!(reviews.to_string().contains("original finding evidence"));
    assert!(integrations.to_string().contains("authorization evidence"));
    assert!(
        integrations
            .to_string()
            .contains("exact check result evidence")
    );
    assert!(integrations.to_string().contains("result-revision"));
    assert!(integrations.to_string().contains("result-tree"));
    assert!(
        integrations
            .to_string()
            .contains("remote observation evidence")
    );
    assert_eq!(activity_attempts["subject_task_id"], subject);
    let relations: std::collections::HashSet<_> = activity_attempts["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["relation"].as_str().unwrap())
        .collect();
    assert!(relations.contains("subject") && relations.contains("workflow_activity"));
    assert!(events.to_string().contains("review.decided"));
    let combined = format!("{submissions}{reviews}{integrations}");
    assert!(!combined.contains("contributor-session-secret"));
    assert!(!combined.contains("reviewer-session-secret"));
    assert!(!combined.contains(&unrelated_task));

    assert_eq!(
        integrations["items"][0]["record"]["authorization"]["valid"],
        true
    );
    let incident = Uuid::new_v4().to_string();
    let now = f.state.now();
    sqlx::query("INSERT INTO clock_incidents(id,observed_wall_time_ms,high_water_time_ms,detected_at) VALUES(?,?,?,?)")
        .bind(&incident)
        .bind(now - 10_000)
        .bind(now)
        .bind(now)
        .execute(&f.state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE clock_state SET status='clock_reconciliation',incident_id=?,observed_wall_time_ms=?,detected_at=?,last_safe_time_ms=? WHERE singleton=1")
        .bind(&incident)
        .bind(now - 10_000)
        .bind(now)
        .bind(now)
        .execute(&f.state.pool)
        .await
        .unwrap();
    let paused_integrations = f
        .history(&project, &subject, "integrations", 20, None)
        .await;
    assert_eq!(
        paused_integrations["items"][0]["record"]["authorization"]["valid"],
        false
    );
    assert_eq!(
        paused_integrations["items"][0]["record"]["authorization"]["validity_reason"],
        "clock_reconciliation_required"
    );
}
