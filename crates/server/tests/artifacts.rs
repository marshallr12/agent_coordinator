use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode},
};
use coordinator_server::{
    artifacts::{
        LIVE_ARTIFACT_QUOTA_BYTES, MAX_ARTIFACT_BYTES, reconcile_store,
        validate_submission_artifacts,
    },
    auth::{digest, secret},
    router,
    state::{AppState, Clock, Config},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
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
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::open(Config {
            database_path: dir.path().join("coordinator.sqlite3"),
            public_origin: "http://127.0.0.1:8080".into(),
            allow_insecure_loopback: true,
            ..Config::default()
        })
        .await
        .unwrap();
        let clock = Arc::new(TestClock(AtomicI64::new(1_800_000_000_000)));
        state.clock = clock.clone();
        let admin = seed(&state, true, "artifact-admin").await;
        let a = seed(&state, false, "artifact-agent-a").await;
        let b = seed(&state, false, "artifact-agent-b").await;
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

    async fn json(
        &self,
        caller: &Caller,
        method: &str,
        path: &str,
        key: &str,
        body: Value,
    ) -> Reply {
        request(
            self.app.clone(),
            caller,
            method,
            path,
            key,
            "application/json",
            Body::from(body.to_string()),
        )
        .await
    }

    async fn bytes(
        &self,
        caller: &Caller,
        method: &str,
        path: &str,
        key: &str,
        body: Vec<u8>,
    ) -> Reply {
        request(
            self.app.clone(),
            caller,
            method,
            path,
            key,
            "application/octet-stream",
            Body::from(body),
        )
        .await
    }

    async fn project(&self, name: &str) -> String {
        let reply = self
            .json(
                &self.admin,
                "POST",
                "/api/v1/projects",
                &Uuid::new_v4().to_string(),
                json!({"name":name,"repository_url":"https://example.test/repo.git","target_branch":"main"}),
            )
            .await;
        reply.ok();
        reply.json()["data"]["id"].as_str().unwrap().into()
    }

    async fn reserve(&self, project: &str, bytes: &[u8], name: &str) -> (String, String) {
        let sha = hex::encode(Sha256::digest(bytes));
        let reply = self
            .json(
                &self.a,
                "POST",
                &format!("/api/v1/projects/{project}/artifacts/uploads"),
                &Uuid::new_v4().to_string(),
                json!({"filename":name,"media_type":"application/octet-stream","size_bytes":bytes.len(),"sha256":sha}),
            )
            .await;
        reply.ok();
        (
            reply.json()["data"]["artifact"]["id"]
                .as_str()
                .unwrap()
                .into(),
            reply.json()["data"]["upload_path"].as_str().unwrap().into(),
        )
    }
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }

    fn ok(&self) {
        assert_eq!(self.status, StatusCode::OK, "{}", self.json());
    }

    fn error(&self, status: StatusCode, code: &str) {
        assert_eq!(self.status, status, "{}", self.json());
        assert_eq!(self.json()["error"]["code"], code, "{}", self.json());
    }
}

async fn request(
    app: Router,
    caller: &Caller,
    method: &str,
    path: &str,
    key: &str,
    content_type: &str,
    body: Body,
) -> Reply {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("Content-Type", content_type);
    if method != "GET" {
        request = request.header("Idempotency-Key", key);
    }
    if caller.human {
        request = request
            .header("Cookie", format!("coordinator_local={}", caller.token))
            .header("Origin", "http://127.0.0.1:8080")
            .header(
                "X-CSRF-Token",
                digest(&format!("coordinator-browser-csrf-v1:{}", caller.token)),
            );
    } else {
        request = request
            .header("Authorization", format!("Bearer {}", caller.token))
            .header("X-Coordinator-Session", &caller.session)
            .header("X-Coordinator-Session-Proof", &caller.proof);
    }
    let response = app.oneshot(request.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    Reply {
        status,
        headers,
        body,
    }
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
    .bind(human.then_some("unused-test-password-hash"))
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
        sqlx::query(
            "INSERT INTO agent_sessions(id,principal_id,credential_id,workstation_id,proof_hash,created_at,capabilities,harness) VALUES(?,?,?,?,?,?,'[]','test')",
        )
        .bind(&caller.session)
        .bind(&caller.principal)
        .bind(&caller.credential)
        .bind(format!("workstation-{name}"))
        .bind(digest(&caller.proof))
        .bind(state.now())
        .execute(&state.pool)
        .await
        .unwrap();
    }
    caller
}

#[tokio::test]
async fn links_are_https_metadata_and_references_stay_in_project() {
    let fixture = Fixture::new().await;
    let project = fixture.project("links").await;
    let other = fixture.project("other-links").await;
    let task = fixture
        .json(
            &fixture.a,
            "POST",
            &format!("/api/v1/projects/{other}/tasks"),
            &Uuid::new_v4().to_string(),
            json!({"title":"other task","acceptance_criteria":["done"]}),
        )
        .await;
    task.ok();
    let task = task.json()["data"]["id"].as_str().unwrap().to_owned();

    fixture
        .json(
            &fixture.a,
            "POST",
            &format!("/api/v1/projects/{project}/artifacts"),
            &Uuid::new_v4().to_string(),
            json!({"display_name":"unsafe","media_type":"text/plain","external_url":"http://127.0.0.1/private"}),
        )
        .await
        .error(StatusCode::BAD_REQUEST, "invalid_request");
    fixture
        .json(
            &fixture.a,
            "POST",
            &format!("/api/v1/projects/{project}/artifacts"),
            &Uuid::new_v4().to_string(),
            json!({"display_name":"wrong project","media_type":"text/plain","external_url":"https://example.test/evidence","task_id":task}),
        )
        .await
        .error(StatusCode::CONFLICT, "artifact_reference_invalid");

    let created = fixture
        .json(
            &fixture.a,
            "POST",
            &format!("/api/v1/projects/{project}/artifacts"),
            "stable-link-key",
            json!({"display_name":"report","media_type":"text/plain","external_url":"https://unreachable.invalid/report","pinned":true}),
        )
        .await;
    created.ok();
    assert_eq!(
        created.json()["data"]["artifact"]["availability"],
        "available"
    );
    let id = created.json()["data"]["artifact"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let detail = fixture
        .json(
            &fixture.b,
            "GET",
            &format!("/api/v1/projects/{project}/artifacts/{id}"),
            "",
            json!(null),
        )
        .await;
    detail.ok();
    let list = fixture
        .json(
            &fixture.b,
            "GET",
            &format!("/api/v1/projects/{project}/artifacts"),
            "",
            json!(null),
        )
        .await;
    list.ok();
    assert_eq!(list.json()["data"]["storage"]["quota_used_bytes"], 0);
    assert_eq!(
        list.json()["data"]["storage"]["max_artifact_bytes"],
        MAX_ARTIFACT_BYTES
    );
    fixture
        .json(
            &fixture.b,
            "POST",
            &format!("/api/v1/projects/{project}/artifacts/{id}/delete"),
            &Uuid::new_v4().to_string(),
            json!({"reason":"not mine"}),
        )
        .await
        .error(StatusCode::FORBIDDEN, "operation_not_permitted");
    let deleted = fixture
        .json(
            &fixture.admin,
            "POST",
            &format!("/api/v1/projects/{project}/artifacts/{id}/delete"),
            "delete-link",
            json!({"reason":"retention policy"}),
        )
        .await;
    deleted.ok();
    assert_eq!(
        deleted.json()["data"]["artifact"]["availability"],
        "deleted"
    );
}

#[tokio::test]
async fn uploads_are_streamed_digest_bound_replayable_and_safe_to_download() {
    let fixture = Fixture::new().await;
    let project = fixture.project("uploads").await;
    let bytes = vec![b'x'; 300 * 1024];
    let (id, path) = fixture.reserve(&project, &bytes, "../../report.html").await;

    fixture
        .bytes(
            &fixture.b,
            "PUT",
            &path,
            "upload-owner-check",
            bytes.clone(),
        )
        .await
        .error(StatusCode::FORBIDDEN, "operation_not_permitted");
    let mut wrong = bytes.clone();
    wrong[0] = b'y';
    fixture
        .bytes(&fixture.a, "PUT", &path, "upload-retry-key", wrong)
        .await
        .error(StatusCode::CONFLICT, "artifact_digest_mismatch");
    let uploaded = fixture
        .bytes(&fixture.a, "PUT", &path, "upload-retry-key", bytes.clone())
        .await;
    uploaded.ok();
    assert_eq!(
        uploaded.json()["data"]["artifact"]["availability"],
        "available"
    );
    fixture
        .bytes(&fixture.a, "PUT", &path, "upload-retry-key", bytes.clone())
        .await
        .ok();

    let downloaded = fixture
        .bytes(&fixture.b, "GET", &path, "", Vec::new())
        .await;
    assert_eq!(downloaded.status, StatusCode::OK);
    assert_eq!(downloaded.body, bytes);
    assert_eq!(downloaded.headers["x-content-type-options"], "nosniff");
    let disposition = downloaded.headers["content-disposition"].to_str().unwrap();
    assert!(disposition.starts_with(&format!("attachment; filename=\"{}-", &id[..8])));
    assert!(!disposition.contains('/'));

    let mut altered = bytes;
    altered[1] = b'z';
    fixture
        .bytes(&fixture.a, "PUT", &path, "upload-retry-key", altered)
        .await
        .error(StatusCode::CONFLICT, "artifact_digest_mismatch");
    let still_original = fixture
        .bytes(&fixture.a, "GET", &path, "", Vec::new())
        .await;
    assert_eq!(still_original.body[0], b'x');
    assert_eq!(still_original.body[1], b'x');
}

#[tokio::test]
async fn reservations_enforce_bounds_expiry_quota_and_revocation() {
    let fixture = Fixture::new().await;
    let project = fixture.project("limits").await;
    fixture
        .json(
            &fixture.a,
            "POST",
            &format!("/api/v1/projects/{project}/artifacts/uploads"),
            &Uuid::new_v4().to_string(),
            json!({"filename":"huge.bin","media_type":"application/octet-stream","size_bytes":MAX_ARTIFACT_BYTES + 1,"sha256":"0".repeat(64)}),
        )
        .await
        .error(StatusCode::PAYLOAD_TOO_LARGE, "artifact_too_large");

    let (expired, expired_path) = fixture.reserve(&project, b"a", "expired.bin").await;
    fixture.clock.0.fetch_add(3_600_001, Ordering::SeqCst);
    let detail = fixture
        .json(
            &fixture.a,
            "GET",
            &format!("/api/v1/projects/{project}/artifacts/{expired}"),
            "",
            json!(null),
        )
        .await;
    detail.ok();
    assert_eq!(detail.json()["data"]["artifact"]["availability"], "expired");
    fixture
        .bytes(
            &fixture.a,
            "PUT",
            &expired_path,
            "expired-upload",
            b"a".to_vec(),
        )
        .await
        .error(StatusCode::GONE, "artifact_unavailable");

    sqlx::query("INSERT INTO artifacts(id,project_id,kind,display_name,media_type,size_bytes,sha256,storage_key,state,created_by,created_at,reservation_expires_at,retention_until,pinned) VALUES(?,?,'upload','quota','application/octet-stream',?,'0000000000000000000000000000000000000000000000000000000000000000',?,'reserved',?,?,?, ?,0)")
        .bind(Uuid::new_v4().to_string()).bind(&project).bind(LIVE_ARTIFACT_QUOTA_BYTES)
        .bind(Uuid::new_v4().to_string()).bind(&fixture.a.principal).bind(fixture.state.now())
        .bind(fixture.state.now() + 3_600_000).bind(fixture.state.now() + 86_400_000)
        .execute(&fixture.state.pool).await.unwrap();
    fixture
        .json(
            &fixture.a,
            "POST",
            &format!("/api/v1/projects/{project}/artifacts/uploads"),
            &Uuid::new_v4().to_string(),
            json!({"filename":"over-quota.bin","media_type":"application/octet-stream","size_bytes":1,"sha256":hex::encode(Sha256::digest(b"z"))}),
        )
        .await
        .error(StatusCode::INSUFFICIENT_STORAGE, "artifact_quota_exceeded");

    sqlx::query("UPDATE credentials SET revoked_at=? WHERE id=?")
        .bind(fixture.state.now())
        .bind(&fixture.a.credential)
        .execute(&fixture.state.pool)
        .await
        .unwrap();
    fixture
        .json(
            &fixture.a,
            "GET",
            &format!("/api/v1/projects/{project}/artifacts"),
            "",
            json!(null),
        )
        .await
        .error(StatusCode::UNAUTHORIZED, "authentication_required");
}

#[tokio::test]
async fn retention_and_deletion_keep_explicit_unavailable_metadata() {
    let fixture = Fixture::new().await;
    let project = fixture.project("retention").await;
    let bytes = b"retained evidence".to_vec();
    let (id, path) = fixture.reserve(&project, &bytes, "evidence.txt").await;
    fixture
        .bytes(
            &fixture.a,
            "PUT",
            &path,
            "finalize-retention",
            bytes.clone(),
        )
        .await
        .ok();

    let mut connection = fixture.state.pool.acquire().await.unwrap();
    validate_submission_artifacts(
        &mut connection,
        &project,
        std::slice::from_ref(&id),
        fixture.state.now(),
    )
    .await
    .unwrap();
    drop(connection);

    let retained = fixture
        .json(
            &fixture.a,
            "POST",
            &format!("/api/v1/projects/{project}/artifacts/{id}/retention"),
            "short-retention",
            json!({"pinned":false,"retention_days":1}),
        )
        .await;
    retained.ok();
    fixture.clock.0.fetch_add(86_400_001, Ordering::SeqCst);
    let expired = fixture
        .json(
            &fixture.b,
            "GET",
            &format!("/api/v1/projects/{project}/artifacts/{id}"),
            "",
            json!(null),
        )
        .await;
    expired.ok();
    assert_eq!(
        expired.json()["data"]["artifact"]["availability"],
        "expired"
    );
    fixture
        .bytes(&fixture.b, "GET", &path, "", Vec::new())
        .await
        .error(StatusCode::GONE, "artifact_unavailable");
    fixture
        .bytes(&fixture.a, "PUT", &path, "finalize-retention", bytes)
        .await
        .error(StatusCode::GONE, "artifact_unavailable");
    let mut connection = fixture.state.pool.acquire().await.unwrap();
    let validation = validate_submission_artifacts(
        &mut connection,
        &project,
        std::slice::from_ref(&id),
        fixture.state.now(),
    )
    .await
    .unwrap_err();
    assert_eq!(validation.code, "submission_artifact_unavailable");
    drop(connection);

    let deleted = fixture
        .json(
            &fixture.a,
            "POST",
            &format!("/api/v1/projects/{project}/artifacts/{id}/delete"),
            "delete-upload",
            json!({"reason":"superseded evidence"}),
        )
        .await;
    deleted.ok();
    assert_eq!(
        deleted.json()["data"]["artifact"]["availability"],
        "deleted"
    );
    assert_eq!(
        deleted.json()["data"]["artifact"]["deletion_reason"],
        "superseded evidence"
    );
    let mut held = Vec::new();
    for _ in 0..7 {
        held.push(fixture.state.pool.acquire().await.unwrap());
    }
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        fixture.json(
            &fixture.a,
            "POST",
            &format!("/api/v1/projects/{project}/artifacts/{id}/delete"),
            "delete-upload",
            json!({"reason":"superseded evidence"}),
        ),
    )
    .await
    .expect("delete replay released its writer transaction before cleanup")
    .ok();
}

#[tokio::test]
async fn cleanup_never_removes_bytes_while_an_artifact_lock_is_live() {
    let fixture = Fixture::new().await;
    let project = fixture.project("cleanup-lock").await;
    let (id, _) = fixture.reserve(&project, b"x", "cleanup.bin").await;
    let key: String = sqlx::query_scalar("SELECT storage_key FROM artifacts WHERE id=?")
        .bind(&id)
        .fetch_one(&fixture.state.pool)
        .await
        .unwrap();
    fixture.clock.0.fetch_add(3_600_001, Ordering::SeqCst);
    let database = &fixture.state.config.database_path;
    let store = database.with_file_name(format!(
        "{}.artifacts",
        database.file_name().unwrap().to_string_lossy()
    ));
    let blob = store
        .join("blobs")
        .join(&key[..2])
        .join(format!("{key}.blob"));
    let lock = store.join("staging").join(format!("{key}.lock"));
    tokio::fs::create_dir_all(blob.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&blob, b"x").await.unwrap();
    tokio::fs::write(&lock, b"live upload lock").await.unwrap();

    reconcile_store(&fixture.state).await.unwrap();
    assert!(blob.exists(), "cleanup raced a live artifact lock");
    tokio::fs::remove_file(lock).await.unwrap();
    // The bounded cursor reaches its end, resets, then revisits this record.
    reconcile_store(&fixture.state).await.unwrap();
    reconcile_store(&fixture.state).await.unwrap();
    assert!(!blob.exists(), "expired blob was not eventually cleaned");
}
