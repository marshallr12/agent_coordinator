use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode},
};
use coordinator_core::{INSTRUCTION_VERSION, REQUIRED_SECTION};
use coordinator_server::{
    auth::{digest, init_admin, secret},
    operator_access::recover_operator_password,
    restore::invalidate_restored_state,
    router,
    state::{AppState, Config},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::Row;
use tower::ServiceExt;
use uuid::Uuid;

const ORIGIN: &str = "http://127.0.0.1:8080";
const OLD_PASSWORD: &str = "restore test old password";
const NEW_PASSWORD: &str = "restore test replacement password";

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}

struct Fixture {
    state: AppState,
    app: Router,
    _dir: tempfile::TempDir,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::open(Config {
            database_path: dir.path().join("restore.sqlite3"),
            public_origin: ORIGIN.into(),
            allow_insecure_loopback: true,
            ..Config::default()
        })
        .await
        .unwrap();
        init_admin(&state, "admin", OLD_PASSWORD.into())
            .await
            .unwrap();
        Self {
            app: router(state.clone()),
            state,
            _dir: dir,
        }
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<Value>,
    ) -> Reply {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = self
            .app
            .clone()
            .oneshot(
                request
                    .body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        Reply {
            status,
            headers,
            body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        }
    }

    async fn login(&self, password: &str) -> Browser {
        let reply = self
            .call(
                "POST",
                "/api/v1/auth/login",
                &[("origin", ORIGIN)],
                Some(json!({"username":"admin","password":password})),
            )
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
        Browser {
            cookie: reply.headers["set-cookie"]
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_owned(),
            csrf: reply.body["data"]["csrf_token"]
                .as_str()
                .unwrap()
                .to_owned(),
        }
    }
}

struct Browser {
    cookie: String,
    csrf: String,
}

impl Browser {
    fn headers<'a>(&'a self, key: &'a str) -> Vec<(&'a str, &'a str)> {
        vec![
            ("cookie", &self.cookie),
            ("origin", ORIGIN),
            ("x-csrf-token", &self.csrf),
            ("idempotency-key", key),
        ]
    }
}

struct Authority {
    principal: String,
    credential: String,
    token: String,
    session: String,
    proof: String,
}

impl Authority {
    fn headers<'a>(&'a self, key: &'a str) -> Vec<(&'a str, &'a str)> {
        vec![
            ("authorization", &self.token),
            ("x-coordinator-session", &self.session),
            ("x-coordinator-session-proof", &self.proof),
            ("idempotency-key", key),
        ]
    }
}

async fn issue_agent(f: &Fixture, browser: &Browser) -> Authority {
    let issued = f
        .call(
            "POST",
            "/api/v1/admin/agents",
            &browser.headers("issue-restore-agent"),
            Some(json!({"name":"restore-agent"})),
        )
        .await;
    assert_eq!(issued.status, StatusCode::OK, "{}", issued.body);
    let token = format!("Bearer {}", issued.body["data"]["token"].as_str().unwrap());
    let session = Uuid::new_v4().to_string();
    let proof = secret();
    let connected = f
        .call(
            "POST",
            "/api/v1/sessions",
            &[
                ("authorization", &token),
                ("x-coordinator-session-proof", &proof),
                ("idempotency-key", "connect-old-agent"),
            ],
            Some(json!({"session_id":session,"workstation_id":"restore-host","harness":"restore-test","capabilities":[]})),
        )
        .await;
    assert_eq!(connected.status, StatusCode::OK, "{}", connected.body);
    Authority {
        principal: issued.body["data"]["principal_id"]
            .as_str()
            .unwrap()
            .to_owned(),
        credential: issued.body["data"]["credential_id"]
            .as_str()
            .unwrap()
            .to_owned(),
        token,
        session,
        proof,
    }
}

struct RestoredRecords {
    project: String,
    task: String,
    attempt: String,
    reservation: String,
    job: String,
    reporter: String,
    reporter_proof: String,
    integration_hold: String,
    queued_activity: String,
    queued_submission: String,
    history_cursor: String,
}

async fn seed_restored_records(
    f: &Fixture,
    browser: &Browser,
    authority: &Authority,
) -> RestoredRecords {
    let now = f.state.now();
    let project = Uuid::new_v4().to_string();
    let task = Uuid::new_v4().to_string();
    let activity_task = Uuid::new_v4().to_string();
    let attempt = Uuid::new_v4().to_string();
    let source_attempt = Uuid::new_v4().to_string();
    let reservation = Uuid::new_v4().to_string();
    let resource = Uuid::new_v4().to_string();
    let job = Uuid::new_v4().to_string();
    let reporter = Uuid::new_v4().to_string();
    let reporter_proof = secret();
    let submission = Uuid::new_v4().to_string();
    let activity = Uuid::new_v4().to_string();
    let integration_hold = Uuid::new_v4().to_string();
    let queued_subject = Uuid::new_v4().to_string();
    let queued_activity_task = Uuid::new_v4().to_string();
    let queued_source_attempt = Uuid::new_v4().to_string();
    let queued_submission = Uuid::new_v4().to_string();
    let queued_activity = Uuid::new_v4().to_string();
    let artifact = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO projects(id,name,repository_url,target_branch,automatic_integration,created_at) VALUES(?,?,'https://example.test/restore.git','main',0,?)")
        .bind(&project).bind(format!("restore-{project}")).bind(now).execute(&f.state.pool).await.unwrap();
    for (id, title) in [
        (&task, "Restored task"),
        (&activity_task, "Integration activity"),
        (&queued_subject, "Queued integration subject"),
        (&queued_activity_task, "Queued integration activity"),
    ] {
        sqlx::query("INSERT INTO tasks(id,project_id,title,description,acceptance_json,kind,priority,lifecycle,generation,created_at,ready_since) VALUES(?,?,?,'','[]','general',2,'open',1,?,?)")
            .bind(id).bind(&project).bind(title).bind(now).bind(now).execute(&f.state.pool).await.unwrap();
    }
    sqlx::query("INSERT INTO attempts(id,project_id,task_id,owner_id,session_id,credential_id,generation,state,mode,expires_at,last_heartbeat_at,last_progress_at,created_at,task_revision,policy_revision) VALUES(?,?,?,?,?,?,1,'active','work',?,?,?,?,1,1)")
        .bind(&attempt).bind(&project).bind(&task).bind(&authority.principal).bind(&authority.session).bind(&authority.credential)
        .bind(now+60_000).bind(now).bind(now).bind(now).execute(&f.state.pool).await.unwrap();
    sqlx::query("UPDATE tasks SET current_attempt_id=? WHERE id=?")
        .bind(&attempt)
        .bind(&task)
        .execute(&f.state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO attempts(id,project_id,task_id,owner_id,session_id,credential_id,generation,state,mode,expires_at,last_heartbeat_at,last_progress_at,created_at,ended_at,outcome,task_revision,policy_revision) VALUES(?,?,?,?,?,?,1,'submitted','work',?,?,?,?,?,'submitted',1,1)")
        .bind(&source_attempt).bind(&project).bind(&activity_task).bind(&authority.principal).bind(&authority.session).bind(&authority.credential)
        .bind(now).bind(now).bind(now).bind(now-2).bind(now-1).execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO resources(id,key,capacity,description,created_by,created_at) VALUES(?,'restore/device',1,'device',?,?)")
        .bind(&resource).bind(&authority.principal).bind(now).execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO reservations(id,project_id,attempt_id,generation,state,created_by,created_at) VALUES(?,?,?,1,'held',?,?)")
        .bind(&reservation).bind(&project).bind(&attempt).bind(&authority.principal).bind(now).execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO reservation_items(reservation_id,resource_id,units) VALUES(?,?,1)")
        .bind(&reservation)
        .bind(&resource)
        .execute(&f.state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO jobs(id,producer_id,project_id,task_id,attempt_id,generation,runner_instance_id,workstation_id,label,source_revision,source_tree,reservation_id,state,created_at) VALUES(?,?,?,?,?,1,'runner','restore-host','restored producer','source','tree',?,'running',?)")
        .bind(&job).bind(Uuid::new_v4().to_string()).bind(&project).bind(&task).bind(&attempt).bind(&reservation).bind(now).execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO reporters(id,job_id,principal_id,credential_id,session_id,proof_hash,expires_at,renew_until,created_at) VALUES(?,?,?,?,?,?,?,?,?)")
        .bind(&reporter).bind(&job).bind(&authority.principal).bind(&authority.credential).bind(&authority.session).bind(digest(&reporter_proof)).bind(now+60_000).bind(now+60_000).bind(now).execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO submissions(id,project_id,task_id,attempt_id,kind,task_revision,project_policy_revision,workflow_policy_revision,summary,acceptance_evidence_json,handoff,created_by,contributor_session_id,created_at) VALUES(?,?,?,?,'general',1,1,0,'done','[]','handoff',?,?,?)")
        .bind(&submission).bind(&project).bind(&activity_task).bind(&source_attempt).bind(&authority.principal).bind(&authority.session).bind(now).execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO workflow_activities(id,project_id,subject_task_id,submission_id,activity_task_id,kind,state,created_at) VALUES(?,?,?,?,?,'integration','active',?)")
        .bind(&activity).bind(&project).bind(&activity_task).bind(&submission).bind(&activity_task).bind(now).execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO integration_holds(id,activity_id,canonical_repository_key,target_branch,state,acquired_by,acquired_at) VALUES(?,?,'restore-repository','main','held',?,?)")
        .bind(&integration_hold).bind(&activity).bind(&authority.principal).bind(now).execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO integration_authorizations(activity_id,submission_id,project_policy_revision,workflow_policy_revision,actor_id,summary,created_at) VALUES(?,?,1,1,?,'approved before backup',?)")
        .bind(&activity).bind(&submission).bind(&authority.principal).bind(now).execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO attempts(id,project_id,task_id,owner_id,session_id,credential_id,generation,state,mode,expires_at,last_heartbeat_at,last_progress_at,created_at,ended_at,outcome,task_revision,policy_revision) VALUES(?,?,?,?,?,?,1,'submitted','work',?,?,?,?,?,'submitted',1,1)")
        .bind(&queued_source_attempt).bind(&project).bind(&queued_subject).bind(&authority.principal).bind(&authority.session).bind(&authority.credential)
        .bind(now).bind(now).bind(now).bind(now-2).bind(now-1).execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO submissions(id,project_id,task_id,attempt_id,kind,task_revision,project_policy_revision,workflow_policy_revision,summary,acceptance_evidence_json,handoff,created_by,contributor_session_id,created_at) VALUES(?,?,?,?,'general',1,1,0,'done','[]','handoff',?,?,?)")
        .bind(&queued_submission).bind(&project).bind(&queued_subject).bind(&queued_source_attempt).bind(&authority.principal).bind(&authority.session).bind(now).execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO workflow_subjects(project_id,task_id,current_submission_id,phase,updated_at) VALUES(?,?,?,'integration',?)")
        .bind(&project).bind(&queued_subject).bind(&queued_submission).bind(now).execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO workflow_activities(id,project_id,subject_task_id,submission_id,activity_task_id,kind,state,created_at) VALUES(?,?,?,?,?,'integration','queued',?)")
        .bind(&queued_activity).bind(&project).bind(&queued_subject).bind(&queued_submission).bind(&queued_activity_task).bind(now).execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO integration_authorizations(activity_id,submission_id,project_policy_revision,workflow_policy_revision,actor_id,summary,created_at) VALUES(?,?,1,0,?,'queued approval before backup',?)")
        .bind(&queued_activity).bind(&queued_submission).bind(&authority.principal).bind(now).execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO artifacts(id,project_id,kind,task_id,display_name,media_type,size_bytes,sha256,storage_key,state,created_by,created_at,reservation_expires_at) VALUES(?,?,'upload',?,'pending.bin','application/octet-stream',1,?,'pending-key','reserved',?,?,?)")
        .bind(&artifact).bind(&project).bind(&task).bind("a".repeat(64)).bind(&authority.principal).bind(now).bind(now+60_000).execute(&f.state.pool).await.unwrap();
    let second = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO attempts(id,project_id,task_id,owner_id,session_id,credential_id,generation,state,mode,expires_at,last_heartbeat_at,last_progress_at,created_at,ended_at,outcome,task_revision,policy_revision) VALUES(?,?,?,?,?,?,2,'released','work',?,?,?,?,?,'history',1,1)")
        .bind(&second).bind(&project).bind(&task).bind(&authority.principal).bind(&authority.session).bind(&authority.credential)
        .bind(now).bind(now).bind(now).bind(now+1).bind(now+2).execute(&f.state.pool).await.unwrap();
    let history = f
        .call(
            "GET",
            &format!("/api/v1/projects/{project}/tasks/{task}/history?kind=attempts&limit=1"),
            &[("cookie", &browser.cookie)],
            None,
        )
        .await;
    assert_eq!(history.status, StatusCode::OK, "{}", history.body);
    RestoredRecords {
        project,
        task,
        attempt,
        reservation,
        job,
        reporter,
        reporter_proof,
        integration_hold,
        queued_activity,
        queued_submission,
        history_cursor: history.body["data"]["next_cursor"]
            .as_str()
            .unwrap()
            .to_owned(),
    }
}

#[tokio::test]
async fn restore_revokes_authority_preserves_holds_and_requires_complete_reconciliation() {
    let f = Fixture::new().await;
    let old_browser = f.login(OLD_PASSWORD).await;
    let authority = issue_agent(&f, &old_browser).await;
    let pre_restore_credential = f
        .call(
            "POST",
            &format!("/api/v1/admin/agents/{}/credentials", authority.principal),
            &old_browser.headers("credential-key-before-restore"),
            Some(json!({"name":"before-restore"})),
        )
        .await;
    assert_eq!(
        pre_restore_credential.status,
        StatusCode::OK,
        "{}",
        pre_restore_credential.body
    );
    let pre_restore_account = f
        .call(
            "POST",
            "/api/v1/admin/operators",
            &old_browser.headers("account-key-before-restore"),
            Some(json!({
                "name":"restored-operator",
                "role":"operator",
                "password":"restored operator original password"
            })),
        )
        .await;
    assert_eq!(
        pre_restore_account.status,
        StatusCode::OK,
        "{}",
        pre_restore_account.body
    );
    let records = seed_restored_records(&f, &old_browser, &authority).await;
    let first_invalidation =
        invalidate_restored_state(&f.state, "snapshot-2026-09-10", "restore rehearsal")
            .await
            .unwrap();
    let first_restore_id = first_invalidation["restore_id"].as_str().unwrap();
    let invalidated = invalidate_restored_state(
        &f.state,
        "snapshot-paused-2026-09-10",
        "restore a backup captured during reconciliation",
    )
    .await
    .unwrap();
    let restore_id = invalidated["restore_id"].as_str().unwrap();
    assert_ne!(restore_id, first_restore_id);
    assert_eq!(invalidated["required_inspections"], 2);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM restore_runs")
            .fetch_one(&f.state.pool)
            .await
            .unwrap(),
        2
    );

    let task_pointer: String =
        sqlx::query_scalar("SELECT current_attempt_id FROM tasks WHERE id=?")
            .bind(&records.task)
            .fetch_one(&f.state.pool)
            .await
            .unwrap();
    assert_eq!(task_pointer, records.attempt);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM attempts WHERE id=?")
            .bind(&records.attempt)
            .fetch_one(&f.state.pool)
            .await
            .unwrap(),
        "expired"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM jobs WHERE id=?")
            .bind(&records.job)
            .fetch_one(&f.state.pool)
            .await
            .unwrap(),
        "unknown"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM reservations WHERE id=?")
            .bind(&records.reservation)
            .fetch_one(&f.state.pool)
            .await
            .unwrap(),
        "held"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM integration_holds WHERE id=?")
            .bind(&records.integration_hold)
            .fetch_one(&f.state.pool)
            .await
            .unwrap(),
        "held"
    );
    assert!(
        sqlx::query_scalar::<_, Option<i64>>(
            "SELECT invalidated_at FROM integration_authorizations"
        )
        .fetch_one(&f.state.pool)
        .await
        .unwrap()
        .is_some()
    );

    for (path, headers) in [
        (
            "/api/v1/auth/account".to_owned(),
            vec![("cookie", old_browser.cookie.as_str())],
        ),
        (
            format!("/api/v1/sessions/{}", authority.session),
            authority.headers("old-session"),
        ),
    ] {
        let reply = f.call("GET", &path, &headers, None).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{}", reply.body);
    }
    let reporter = f.call("POST", &format!("/api/v1/reporters/{}/observations", records.reporter), &[("authorization",&format!("Bearer acr_{}.{}",records.reporter,records.reporter_proof)),("idempotency-key","old-reporter")], Some(json!({"sequence":1,"producer_id":"ignored","state":"unknown","pid":null,"process_started_at":null,"exit_code":null,"inputs_unchanged":null,"summary":"stale"}))).await;
    assert_eq!(
        reporter.status,
        StatusCode::UNAUTHORIZED,
        "{}",
        reporter.body
    );

    recover_operator_password(
        &f.state,
        "admin",
        NEW_PASSWORD.into(),
        "restore administrator recovery",
    )
    .await
    .unwrap();
    let admin = f.login(NEW_PASSWORD).await;
    let blocked = f
        .call(
            "POST",
            "/api/v1/resources",
            &admin.headers("paused-resource"),
            Some(json!({"key":"restore/blocked","capacity":1,"description":"must wait"})),
        )
        .await;
    assert_eq!(blocked.status, StatusCode::CONFLICT, "{}", blocked.body);
    assert_eq!(
        blocked.body["error"]["code"],
        "restore_reconciliation_required"
    );
    let old_request = f
        .call(
            "POST",
            &format!("/api/v1/admin/agents/{}/credentials", authority.principal),
            &admin.headers("credential-key-before-restore"),
            Some(json!({"name":"before-restore"})),
        )
        .await;
    assert_eq!(
        old_request.status,
        StatusCode::CONFLICT,
        "{}",
        old_request.body
    );
    assert_eq!(
        old_request.body["error"]["code"],
        "request_from_previous_restore"
    );
    let old_account_request = f
        .call(
            "POST",
            "/api/v1/admin/operators",
            &admin.headers("account-key-before-restore"),
            Some(json!({
                "name":"restored-operator",
                "role":"operator",
                "password":"restored operator original password"
            })),
        )
        .await;
    assert_eq!(
        old_account_request.status,
        StatusCode::CONFLICT,
        "{}",
        old_account_request.body
    );
    assert_eq!(
        old_account_request.body["error"]["code"],
        "request_from_previous_restore"
    );

    let stale_cursor = f
        .call(
            "GET",
            &format!(
                "/api/v1/projects/{}/tasks/{}/history?kind=attempts&limit=1&cursor={}",
                records.project, records.task, records.history_cursor
            ),
            &[("cookie", &admin.cookie)],
            None,
        )
        .await;
    assert_eq!(
        stale_cursor.status,
        StatusCode::CONFLICT,
        "{}",
        stale_cursor.body
    );
    assert_eq!(
        stale_cursor.body["error"]["code"],
        "history_cursor_mismatch"
    );

    let credential = f
        .call(
            "POST",
            &format!("/api/v1/admin/agents/{}/credentials", authority.principal),
            &admin.headers("restore-agent-credential"),
            Some(json!({"name":"after-restore"})),
        )
        .await;
    assert_eq!(credential.status, StatusCode::OK, "{}", credential.body);
    assert!(credential.body["data"]["token"].is_string());
    assert_eq!(credential.body["data"]["principal_id"], authority.principal);
    let replay = f
        .call(
            "POST",
            &format!("/api/v1/admin/agents/{}/credentials", authority.principal),
            &admin.headers("restore-agent-credential"),
            Some(json!({"name":"after-restore"})),
        )
        .await;
    assert_eq!(replay.status, StatusCode::OK, "{}", replay.body);
    assert!(replay.body["data"]["token"].is_null());
    assert_eq!(replay.body["data"]["secret_unavailable"], true);
    let replacement_token = format!(
        "Bearer {}",
        credential.body["data"]["token"].as_str().unwrap()
    );
    let replacement_session = Uuid::new_v4().to_string();
    let replacement_proof = secret();
    let connected = f
        .call(
            "POST",
            "/api/v1/sessions",
            &[
                ("authorization", &replacement_token),
                ("x-coordinator-session-proof", &replacement_proof),
                ("idempotency-key", "connect-after-restore"),
            ],
            Some(json!({"session_id":replacement_session,"workstation_id":"restore-host-new","harness":"restore-test","capabilities":[]})),
        )
        .await;
    assert_eq!(connected.status, StatusCode::OK, "{}", connected.body);
    let connected_headers = [
        ("authorization", replacement_token.as_str()),
        ("x-coordinator-session", replacement_session.as_str()),
        ("x-coordinator-session-proof", replacement_proof.as_str()),
        ("idempotency-key", "ack-after-restore"),
    ];
    let acknowledged = f
        .call(
            "POST",
            &format!(
                "/api/v1/sessions/{replacement_session}/instruction-acknowledgments"
            ),
            &connected_headers,
            Some(json!({"project_id":records.project,"policy_revision":1,"instruction_version":INSTRUCTION_VERSION,"sections":[REQUIRED_SECTION]})),
        )
        .await;
    assert_eq!(acknowledged.status, StatusCode::OK, "{}", acknowledged.body);
    let claim_headers = [
        ("authorization", replacement_token.as_str()),
        ("x-coordinator-session", replacement_session.as_str()),
        ("x-coordinator-session-proof", replacement_proof.as_str()),
        ("idempotency-key", "claim-during-restore"),
    ];
    let claim = f
        .call(
            "POST",
            &format!("/api/v1/projects/{}/claims", records.project),
            &claim_headers,
            Some(json!({"task_id":records.task,"expected_task_revision":1,"mode":"recovery","policy_revision":1,"instruction_version":INSTRUCTION_VERSION})),
        )
        .await;
    assert_eq!(claim.status, StatusCode::CONFLICT, "{}", claim.body);
    assert_eq!(
        claim.body["error"]["code"],
        "restore_reconciliation_required"
    );

    let status = f
        .call(
            "GET",
            "/api/v1/admin/restore?limit=1",
            &[("cookie", &admin.cookie)],
            None,
        )
        .await;
    assert_eq!(status.status, StatusCode::OK, "{}", status.body);
    assert_eq!(
        status.body["data"]["service_state"]["restore_id"],
        restore_id
    );
    assert_eq!(status.body["data"]["restore"]["remaining_inspections"], 2);
    assert_eq!(status.body["data"]["items"].as_array().unwrap().len(), 1);
    assert!(status.body["data"]["next_cursor"].is_string());
    let early = f
        .call(
            "POST",
            "/api/v1/admin/restore/finish",
            &admin.headers("finish-early"),
            Some(json!({"restore_id":restore_id,"reason":"all reconciliation complete"})),
        )
        .await;
    assert_eq!(early.status, StatusCode::CONFLICT, "{}", early.body);
    assert_eq!(
        early.body["error"]["code"],
        "restore_reconciliation_incomplete"
    );

    for (kind, target) in [
        ("resource_hold", records.reservation.as_str()),
        ("integration_hold", records.integration_hold.as_str()),
    ] {
        let inspected = f.call("POST", "/api/v1/admin/restore/inspections", &admin.headers(&format!("inspect-{kind}")), Some(json!({"restore_id":restore_id,"kind":kind,"target_id":target,"disposition":"unknown","evidence":"Operator inspected the external owner and retained the hold."}))).await;
        assert_eq!(inspected.status, StatusCode::OK, "{}", inspected.body);
    }
    for (path, key, evidence) in [
        (
            "/api/v1/admin/restore/old-installation-fenced",
            "fence-old",
            "The old installation is offline and cannot serve or write.",
        ),
        (
            "/api/v1/admin/restore/post-snapshot-gap",
            "reconcile-gap",
            "Compared the snapshot boundary with external systems; uncertain effects remain held.",
        ),
    ] {
        let attested = f
            .call(
                "POST",
                path,
                &admin.headers(key),
                Some(json!({"restore_id":restore_id,"evidence":evidence})),
            )
            .await;
        assert_eq!(attested.status, StatusCode::OK, "{}", attested.body);
    }
    let finished = f.call("POST", "/api/v1/admin/restore/finish", &admin.headers("finish-restore"), Some(json!({"restore_id":restore_id,"reason":"Required reconciliation evidence is complete."}))).await;
    assert_eq!(finished.status, StatusCode::OK, "{}", finished.body);
    assert_eq!(finished.body["data"]["coordination_state"], "ready");
    assert_eq!(finished.body["data"]["holds_released"], false);
    let reauthorized = f
        .call(
            "POST",
            &format!(
                "/api/v1/projects/{}/workflow-activities/{}/authorization",
                records.project, records.queued_activity
            ),
            &admin.headers("reauthorize-queued-integration"),
            Some(json!({
                "submission_id":records.queued_submission,
                "expected_project_policy_revision":1,
                "expected_workflow_policy_revision":0,
                "summary":"Fresh human authorization after restore reconciliation."
            })),
        )
        .await;
    assert_eq!(reauthorized.status, StatusCode::OK, "{}", reauthorized.body);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT authorization_revision FROM integration_authorizations WHERE activity_id=?"
        )
        .bind(&records.queued_activity)
        .fetch_one(&f.state.pool)
        .await
        .unwrap(),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM integration_authorization_history WHERE activity_id=?"
        )
        .bind(&records.queued_activity)
        .fetch_one(&f.state.pool)
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM reservations WHERE id=?")
            .bind(&records.reservation)
            .fetch_one(&f.state.pool)
            .await
            .unwrap(),
        "held"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM integration_holds WHERE id=?")
            .bind(&records.integration_hold)
            .fetch_one(&f.state.pool)
            .await
            .unwrap(),
        "held"
    );
    let events = sqlx::query(
        "SELECT kind,evidence FROM restore_reconciliation_events WHERE restore_id=? ORDER BY seq",
    )
    .bind(restore_id)
    .fetch_all(&f.state.pool)
    .await
    .unwrap();
    assert_eq!(events.len(), 5);
    assert_eq!(
        events.last().unwrap().get::<String, _>("kind"),
        "restore_completed"
    );
}
