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
use sqlx::Row;
use tower::ServiceExt;
use uuid::Uuid;

const SITHBIT_REVISION: &str = "20368b6fdb8c457cd822480f44c509253b9ea385";
const SUBMISSION_REVISION: &str = "bbbdf8b6dbeee80ff0d1b87afaf5a91597c953c9";

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
            database_path: dir.path().join("imports.sqlite3"),
            public_origin: "http://127.0.0.1:8080".into(),
            allow_insecure_loopback: true,
            ..Config::default()
        })
        .await
        .unwrap();
        let token = secret();
        let principal = Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO principals(id,name,kind,role,password_hash,created_at) VALUES(?,?,'human','admin','unused',?)")
            .bind(&principal).bind(format!("import-admin-{principal}")).bind(state.now()).execute(&state.pool).await.unwrap();
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

    async fn call_with_key(
        &self,
        method: &str,
        path: &str,
        key: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "application/json")
                    .header("idempotency-key", key)
                    .header("cookie", format!("coordinator_local={}", self.token))
                    .header("origin", "http://127.0.0.1:8080")
                    .header(
                        "x-csrf-token",
                        digest(&format!("coordinator-browser-csrf-v1:{}", self.token)),
                    )
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    async fn call(&self, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
        self.call_with_key(method, path, &Uuid::new_v4().to_string(), body)
            .await
    }

    async fn project(&self, name: &str) -> String {
        let (status,value)=self.call("POST","/api/v1/projects",json!({"name":name,"repository_url":format!("https://example.test/{name}.git"),"target_branch":"main"})).await;
        assert_eq!(status, StatusCode::OK, "{value}");
        value["data"]["id"].as_str().unwrap().to_owned()
    }

    fn source(&self, context: &str) -> Value {
        self.source_at(context, SITHBIT_REVISION, "development")
    }

    fn source_at(&self, context: &str, revision: &str, branch: &str) -> Value {
        json!({"context":context,"git_revision":revision,"observed_at":"2026-09-09T20:00:00-04:00","branch":branch,"environment":"audited-read-only"})
    }

    async fn preview(&self, project: &str, source: Value, chunks: Value, mappings: Value) -> Value {
        let (status, value) = self
            .call(
                "POST",
                &format!("/api/v1/projects/{project}/imports/preview"),
                json!({"source":source,"chunks":chunks,"historical_mappings":mappings}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        value["data"].clone()
    }

    async fn apply(&self, project: &str, preview: &Value, key: &str) -> (StatusCode, Value) {
        self.call_with_key("POST",&format!("/api/v1/projects/{project}/imports/{}/apply",preview["id"].as_str().unwrap()),key,json!({"preview_digest":preview["digest"],"expected_project_event_revision":preview["project_event_revision"]})).await
    }
}

#[tokio::test]
async fn audited_historical_prose_is_never_eligible_and_missing_links_are_reported() {
    let f = Fixture::new().await;
    let project = f.project("audited-history").await;
    let sithbit = include_str!("fixtures/imports/sithbit-item-26.md");
    let submission = include_str!("fixtures/imports/submission-resume.md");
    let handoff = include_str!("fixtures/imports/submission-branch-handoff.md");
    let sithbit_preview=f.preview(&project,f.source_at("sithbit",SITHBIT_REVISION,"development"),json!([
        {"path":"BACKLOG.md","markdown":sithbit}
    ]),json!([
        {"path":"BACKLOG.md","section_identity":"Longer-horizon backlog / Item 26 closure record","title":"Item 26 Lockbox v2 closure","disposition":"closed","evidence":"Audited at SithBit 20368b6f; do-not-requeue closure and ancestor evidence."}
    ])).await;
    assert_eq!(sithbit_preview["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        sithbit_preview["unresolved_links"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        f.apply(&project, &sithbit_preview, &Uuid::new_v4().to_string())
            .await
            .0,
        StatusCode::OK
    );

    let submission_preview=f.preview(&project,f.source_at("submission",SUBMISSION_REVISION,"main"),json!([
        {"path":"context/RESUME.md","markdown":submission},
        {"path":"EmarsModern/docs/document-scripts/BUILDING-GROUP-MIGRATION-HANDOFF.md","markdown":handoff}
    ]),json!([
        {"path":"context/RESUME.md","section_identity":"eMARS Migration Resume Handoff / Completion State","title":"Stale July 20 resume state","disposition":"superseded","evidence":"Later audited merges supersede the historical resume prose."},
        {"path":"EmarsModern/docs/document-scripts/BUILDING-GROUP-MIGRATION-HANDOFF.md","section_identity":"Legacy document-script migration verification record / Status","title":"Historical feature-branch permission","disposition":"rejected","evidence":"Historical branch scope is not current merge authority."}
    ])).await;
    assert_eq!(submission_preview["items"].as_array().unwrap().len(), 2);
    assert!(
        submission_preview["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["record_kind"] == "historical")
    );
    assert_eq!(
        submission_preview["unresolved_links"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let key = Uuid::new_v4().to_string();
    let (status, applied) = f.apply(&project, &submission_preview, &key).await;
    assert_eq!(status, StatusCode::OK, "{applied}");
    let eligible: i64 = sqlx::query_scalar("SELECT count(*) FROM tasks WHERE project_id=?")
        .bind(&project)
        .fetch_one(&f.state.pool)
        .await
        .unwrap();
    assert_eq!(eligible, 0);
    let searchable: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM knowledge_search WHERE source_project_id=? AND knowledge_search MATCH 'Lockbox OR stale OR branch'",
    )
    .bind(&project)
    .fetch_one(&f.state.pool)
    .await
    .unwrap();
    assert_eq!(searchable, 3);
    let (_, replayed) = f.apply(&project, &submission_preview, &key).await;
    assert_eq!(applied["data"], replayed["data"]);
}

#[tokio::test]
async fn stale_apply_is_rejected_and_checked_item_records_durable_closure() {
    let f = Fixture::new().await;
    let project = f.project("stale-preview").await;
    let preview=f.preview(&project,f.source("repo-a"),json!([{"path":"TASKS.md","markdown":"# Release\n\n- [x] Ship guarded importer <!-- coordinator-id: ship-importer -->\n"}]),json!([])).await;
    let (read_status, read_preview) = f
        .call(
            "GET",
            &format!(
                "/api/v1/projects/{project}/imports/{}",
                preview["id"].as_str().unwrap()
            ),
            json!({}),
        )
        .await;
    assert_eq!(read_status, StatusCode::OK, "{read_preview}");
    assert_eq!(read_preview["data"]["digest"], preview["digest"]);
    let (created_status,_)=f.call("POST",&format!("/api/v1/projects/{project}/tasks"),json!({"title":"Concurrent service task","description":"changes project event revision","acceptance_criteria":["recorded"],"kind":"general","priority":2,"depends_on":[],"planned":true})).await;
    assert_eq!(created_status, StatusCode::OK);
    let (status, stale) = f
        .apply(&project, &preview, &Uuid::new_v4().to_string())
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    assert_eq!(stale["error"]["code"], "stale_import_preview");

    let fresh=f.preview(&project,f.source("repo-a"),json!([{"path":"TASKS.md","markdown":"# Release\n\n- [x] Ship guarded importer <!-- coordinator-id: ship-importer -->\n"}]),json!([])).await;
    let (status, applied) = f.apply(&project, &fresh, &Uuid::new_v4().to_string()).await;
    assert_eq!(status, StatusCode::OK, "{applied}");
    let row=sqlx::query("SELECT t.lifecycle,ir.closure_provenance_json FROM import_records ir JOIN tasks t ON t.id=ir.task_id WHERE ir.project_id=?")
        .bind(&project).fetch_one(&f.state.pool).await.unwrap();
    assert_eq!(row.get::<String, _>("lifecycle"), "done");
    assert!(
        row.get::<Option<String>, _>("closure_provenance_json")
            .unwrap()
            .contains(SITHBIT_REVISION)
    );
}

#[tokio::test]
async fn duplicate_implicit_checklist_identity_is_a_blocking_conflict() {
    let f = Fixture::new().await;
    let project = f.project("ambiguous-checklist").await;
    let preview = f
        .preview(
            &project,
            f.source("ambiguous"),
            json!([{
                "path":"BACKLOG.md",
                "markdown":"# Later\n- [ ] Item 26\n- [ ] Item 26\n"
            }]),
            json!([]),
        )
        .await;
    assert!(
        preview["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|conflict| {
                conflict["code"] == "ambiguous_source_identity" && conflict["blocking"] == true
            })
    );
    let (status, value) = f
        .apply(&project, &preview, &Uuid::new_v4().to_string())
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{value}");
    assert_eq!(value["error"]["code"], "import_conflicts");
}

#[tokio::test]
async fn reimport_never_reopens_closed_item_and_preserves_newer_service_edit() {
    let f = Fixture::new().await;
    let project = f.project("reimport").await;
    let closed=f.preview(&project,f.source("repo-b"),json!([{"path":"TASKS.md","markdown":"# Stable\n- [x] Original title <!-- coordinator-id: durable-item -->"}]),json!([])).await;
    assert_eq!(
        f.apply(&project, &closed, &Uuid::new_v4().to_string())
            .await
            .0,
        StatusCode::OK
    );
    let task_id: String =
        sqlx::query_scalar("SELECT task_id FROM import_records WHERE project_id=?")
            .bind(&project)
            .fetch_one(&f.state.pool)
            .await
            .unwrap();
    let unchecked=f.preview(&project,f.source("repo-b"),json!([{"path":"TASKS.md","markdown":"# Stable\n- [ ] Renamed source title <!-- coordinator-id: durable-item -->"}]),json!([])).await;
    assert_eq!(
        f.apply(&project, &unchecked, &Uuid::new_v4().to_string())
            .await
            .0,
        StatusCode::OK
    );
    let task = sqlx::query("SELECT lifecycle,title,revision FROM tasks WHERE id=?")
        .bind(&task_id)
        .fetch_one(&f.state.pool)
        .await
        .unwrap();
    assert_eq!(task.get::<String, _>("lifecycle"), "done");
    assert_eq!(task.get::<String, _>("title"), "Renamed source title");

    sqlx::query("UPDATE tasks SET title='Service-owned title',revision=revision+1 WHERE id=?")
        .bind(&task_id)
        .execute(&f.state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO events(project_id,actor_id,kind,record_id,data_json,created_at) VALUES(?,?,'test.service_edit',?,'{}',?)")
        .bind(&project).bind(&f.principal).bind(&task_id).bind(f.state.now()).execute(&f.state.pool).await.unwrap();
    let changed=f.preview(&project,f.source("repo-b"),json!([{"path":"TASKS.md","markdown":"# Stable\n- [x] Source overwrites nothing <!-- coordinator-id: durable-item -->"}]),json!([])).await;
    assert!(
        changed["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["code"] == "newer_service_state")
    );
    assert_eq!(
        f.apply(&project, &changed, &Uuid::new_v4().to_string())
            .await
            .0,
        StatusCode::OK
    );
    let task = sqlx::query("SELECT lifecycle,title FROM tasks WHERE id=?")
        .bind(&task_id)
        .fetch_one(&f.state.pool)
        .await
        .unwrap();
    assert_eq!(task.get::<String, _>("lifecycle"), "done");
    assert_eq!(task.get::<String, _>("title"), "Service-owned title");
}

#[tokio::test]
async fn checked_reimport_cannot_complete_a_task_with_attempt_history() {
    let f = Fixture::new().await;
    let project = f.project("live-imported-task").await;
    let planned=f.preview(&project,f.source("repo-live"),json!([{"path":"TASKS.md","markdown":"# Work\n- [ ] Work in service <!-- coordinator-id: live-item -->"}]),json!([])).await;
    assert_eq!(
        f.apply(&project, &planned, &Uuid::new_v4().to_string())
            .await
            .0,
        StatusCode::OK
    );
    let task_id: String = sqlx::query_scalar(
        "SELECT task_id FROM import_records WHERE project_id=? AND stable_identity=?",
    )
    .bind(&project)
    .bind(planned["items"][0]["stable_identity"].as_str().unwrap())
    .fetch_one(&f.state.pool)
    .await
    .unwrap();
    let session: String =
        sqlx::query_scalar("SELECT id FROM browser_sessions WHERE principal_id=?")
            .bind(&f.principal)
            .fetch_one(&f.state.pool)
            .await
            .unwrap();
    let attempt = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO attempts(id,project_id,task_id,owner_id,session_id,generation,state,mode,expires_at,last_heartbeat_at,last_progress_at,created_at,task_revision,policy_revision) VALUES(?,?,?,?,?,1,'active','work',?,?,?,?,1,1)")
        .bind(&attempt).bind(&project).bind(&task_id).bind(&f.principal).bind(session)
        .bind(f.state.now()+60_000).bind(f.state.now()).bind(f.state.now()).bind(f.state.now())
        .execute(&f.state.pool).await.unwrap();
    sqlx::query("UPDATE tasks SET lifecycle='open',generation=1,current_attempt_id=? WHERE id=?")
        .bind(&attempt)
        .bind(&task_id)
        .execute(&f.state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO events(project_id,actor_id,kind,record_id,data_json,created_at) VALUES(?,?,'test.attempt_started',?,'{}',?)")
        .bind(&project).bind(&f.principal).bind(&task_id).bind(f.state.now()).execute(&f.state.pool).await.unwrap();

    let checked=f.preview(&project,f.source("repo-live"),json!([{"path":"TASKS.md","markdown":"# Work\n- [x] Work in service <!-- coordinator-id: live-item -->"}]),json!([])).await;
    assert!(
        checked["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|conflict| conflict["code"] == "newer_service_state")
    );
    assert_eq!(
        f.apply(&project, &checked, &Uuid::new_v4().to_string())
            .await
            .0,
        StatusCode::OK
    );
    let task = sqlx::query("SELECT lifecycle,current_attempt_id FROM tasks WHERE id=?")
        .bind(&task_id)
        .fetch_one(&f.state.pool)
        .await
        .unwrap();
    assert_eq!(task.get::<String, _>("lifecycle"), "open");
    assert_eq!(
        task.get::<Option<String>, _>("current_attempt_id")
            .as_deref(),
        Some(attempt.as_str())
    );
}

#[tokio::test]
async fn generated_projection_is_not_import_authority_and_export_cursor_is_snapshot_bound() {
    let f = Fixture::new().await;
    let project = f.project("exports").await;
    let (seeded, _) = f.call("POST",&format!("/api/v1/projects/{project}/tasks"),json!({"title":"Initial exported task","description":"","acceptance_criteria":["exists"],"kind":"general","priority":2,"depends_on":[],"planned":true})).await;
    assert_eq!(seeded, StatusCode::OK);
    let (status, first) = f
        .call(
            "GET",
            &format!("/api/v1/projects/{project}/exports?limit=1"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["data"]["generated"], true);
    let generated = first["data"]["markdown"].as_str().unwrap();
    let preview = f
        .preview(
            &project,
            f.source("generated"),
            json!([{"path":"EXPORT.md","markdown":generated}]),
            json!([]),
        )
        .await;
    assert!(
        preview["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["code"] == "generated_projection" && c["blocking"] == true)
    );
    assert_eq!(
        f.apply(&project, &preview, &Uuid::new_v4().to_string())
            .await
            .0,
        StatusCode::CONFLICT
    );

    let cursor = first["data"]["next_cursor"].as_str().unwrap();
    let (created,_)=f.call("POST",&format!("/api/v1/projects/{project}/tasks"),json!({"title":"Changes export snapshot","description":"","acceptance_criteria":["exists"],"kind":"general","priority":2,"depends_on":[],"planned":true})).await;
    assert_eq!(created, StatusCode::OK);
    let (status, value) = f
        .call(
            "GET",
            &format!("/api/v1/projects/{project}/exports?limit=1&cursor={cursor}"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{value}");
    assert_eq!(value["error"]["code"], "export_snapshot_changed");
}

#[tokio::test]
async fn export_pages_are_byte_bounded_without_truncating_records() {
    let f = Fixture::new().await;
    let project = f.project("bounded-exports").await;
    let description = "full authoritative handoff content ".repeat(120);
    assert!(description.len() > 3_000);
    for index in 0..40 {
        let (status, value) = f
            .call(
                "POST",
                &format!("/api/v1/projects/{project}/tasks"),
                json!({
                    "title":format!("Large exported task {index:03}"),
                    "description":description,
                    "acceptance_criteria":["exists"],
                    "kind":"general",
                    "priority":2,
                    "depends_on":[],
                    "planned":true
                }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
    }
    let (status, page) = f
        .call(
            "GET",
            &format!("/api/v1/projects/{project}/exports?limit=200"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(serde_json::to_vec(&page).unwrap().len() <= 256 * 1024);
    let records = page["data"]["records"].as_array().unwrap();
    assert!(!records.is_empty());
    let task_records: Vec<_> = records
        .iter()
        .filter(|record| record["kind"] == "task")
        .collect();
    assert!(!task_records.is_empty());
    assert!(
        task_records
            .iter()
            .all(|record| record["body"] == description)
    );
    assert!(page["data"]["next_cursor"].is_string());
    assert_eq!(page["data"]["page_complete"], false);
    assert_eq!(page["data"]["omissions"], json!([]));
}
