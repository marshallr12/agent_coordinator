//! `GET /api/v1/projects/{p}/sessions`: which open agent sessions are bound
//! to a project, how recently each acted, and which attempts each holds.
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use coordinator_core::timestamp;
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

/// One hour in milliseconds.
const HOUR: i64 = 3_600_000;

/// A deterministic service clock.
struct TestClock(AtomicI64);
impl Clock for TestClock {
    /// The fixed test time.
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    /// Test time never advances on its own.
    fn use_monotonic_elapsed(&self) -> bool {
        false
    }
}

/// A seeded principal: an agent with a registered session, or a human with
/// a browser session.
#[derive(Clone)]
struct Caller {
    token: String,
    session: String,
    proof: String,
    principal: String,
    credential: String,
    human: bool,
}

/// A service over a temporary database with one administrator.
struct Fixture {
    state: AppState,
    app: Router,
    _dir: tempfile::TempDir,
    admin: Caller,
}

impl Fixture {
    /// Opens a fresh service on the deterministic clock.
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::open(Config {
            database_path: dir.path().join("sessions.sqlite3"),
            public_origin: "http://127.0.0.1:8080".into(),
            allow_insecure_loopback: true,
            ..Config::default()
        })
        .await
        .unwrap();
        state.clock = Arc::new(TestClock(AtomicI64::new(1_800_000_000_000)));
        let admin = seed(&state, true, "sessions-admin").await;
        Self {
            app: router(state.clone()),
            state,
            _dir: dir,
            admin,
        }
    }

    /// Seeds an agent with an open session.
    async fn agent(&self, name: &str) -> Caller {
        seed(&self.state, false, name).await
    }

    /// Calls the API as `c` with a fresh idempotency key.
    async fn call(&self, c: &Caller, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
        send(self.app.clone(), Some(c), method, path, body).await
    }

    /// Creates a project and returns its id.
    async fn project(&self, name: &str) -> String {
        let body = json!({"name":name,"repository_url":format!("https://example.test/{name}.git"),"target_branch":"main"});
        let (s, v) = self
            .call(&self.admin, "POST", "/api/v1/projects", body)
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["data"]["id"].as_str().unwrap().into()
    }

    /// Creates a general task in project `p` as `c`.
    async fn task(&self, c: &Caller, p: &str, title: &str) -> Value {
        let body = json!({"title":title,"description":"sessions test","acceptance_criteria":["listed"],"kind":"general"});
        let path = format!("/api/v1/projects/{p}/tasks");
        let (s, v) = self.call(c, "POST", &path, body).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["data"].clone()
    }

    /// Acknowledges project `p`'s instructions for `c`'s session.
    async fn ack(&self, c: &Caller, p: &str) {
        let body = json!({"project_id":p,"policy_revision":1,"instruction_version":coordinator_core::INSTRUCTION_VERSION,"sections":[coordinator_core::REQUIRED_SECTION]});
        let path = format!("/api/v1/sessions/{}/instruction-acknowledgments", c.session);
        let (s, v) = self.call(c, "POST", &path, body).await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }

    /// Claims task `t` for work as `c` and returns the attempt.
    async fn claim(&self, c: &Caller, p: &str, t: &Value) -> Value {
        self.ack(c, p).await;
        let body = json!({"task_id":t["id"],"expected_task_revision":t["revision"],"mode":"work","policy_revision":1,"instruction_version":coordinator_core::INSTRUCTION_VERSION});
        let path = format!("/api/v1/projects/{p}/claims");
        let (s, v) = self.call(c, "POST", &path, body).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["data"]["claim"]["attempt"].clone()
    }

    /// Records a checkpoint on attempt `a` as `c`.
    async fn checkpoint(&self, c: &Caller, p: &str, a: &Value) {
        let path = format!("/api/v1/projects/{p}/attempts/{}/checkpoints", id(a));
        let body =
            json!({"generation":a["generation"],"summary":"Working","current_action":"Testing"});
        let (s, v) = self.call(c, "POST", &path, body).await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }

    /// Submits attempt `a` on task `t` and returns its agent review activity.
    async fn submit(&self, c: &Caller, p: &str, t: &Value, a: &Value) -> Value {
        let path = format!("/api/v1/projects/{p}/attempts/{}/submissions", id(a));
        let body = json!({"generation":a["generation"],"task_revision":t["revision"],"project_policy_revision":1,"workflow_policy_revision":0,"kind":"general","summary":"ready","acceptance_evidence":[{"criterion":"listed","evidence":"verified"}],"handoff":"review it"});
        let (s, v) = self.call(c, "POST", &path, body).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        let activities = v["data"]["activities"].as_array().unwrap();
        activities
            .iter()
            .find(|x| x["kind"] == "agent_review")
            .unwrap()
            .clone()
    }

    /// Claims review activity `r` as `c`.
    async fn claim_review(&self, c: &Caller, p: &str, r: &Value) {
        self.ack(c, p).await;
        let path = format!("/api/v1/projects/{p}/workflow-activities/{}/claim", id(r));
        let body = json!({"expected_submission_id":r["submission_id"],"expected_project_policy_revision":1,"expected_workflow_policy_revision":0});
        let (s, v) = self.call(c, "POST", &path, body).await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }

    /// Lists project `p`'s sessions as `c` with the given query string.
    async fn list(&self, c: &Caller, p: &str, query: &str) -> (StatusCode, Value) {
        let path = format!("/api/v1/projects/{p}/sessions{query}");
        self.call(c, "GET", &path, Value::Null).await
    }

    /// Lists successfully and returns the listed session ids in order.
    async fn ids(&self, p: &str, query: &str) -> Vec<String> {
        let (s, v) = self.list(&self.admin, p, query).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        items(&v)
            .iter()
            .map(|i| i["session_id"].as_str().unwrap().to_owned())
            .collect()
    }

    /// Runs one SQL statement with string and integer bindings.
    async fn exec(&self, sql: &'static str, text: &[&str], ints: &[i64]) {
        let mut q = sqlx::query(sql);
        for t in text {
            q = q.bind(t.to_string());
        }
        for i in ints {
            q = q.bind(*i);
        }
        q.execute(&self.state.pool).await.unwrap();
    }

    /// Runs an `UPDATE … SET column=? WHERE key=?` with a time and an id.
    async fn update(&self, sql: &'static str, value: i64, id: &str) {
        sqlx::query(sql)
            .bind(value)
            .bind(id.to_owned())
            .execute(&self.state.pool)
            .await
            .unwrap();
    }

    /// The current service time.
    fn now(&self) -> i64 {
        self.state.now()
    }
}

/// The `id` field of a record.
fn id(v: &Value) -> &str {
    v["id"].as_str().unwrap()
}

/// The listed items of a list response.
fn items(v: &Value) -> &Vec<Value> {
    v["data"]["items"].as_array().unwrap()
}

/// The listed item for session `session`.
fn item<'a>(v: &'a Value, session: &str) -> &'a Value {
    items(v)
        .iter()
        .find(|i| i["session_id"] == session)
        .unwrap()
}

/// Seeds a principal; agents also get a credential and an open session.
async fn seed(state: &AppState, human: bool, name: &str) -> Caller {
    let c = Caller {
        token: secret(),
        session: Uuid::new_v4().to_string(),
        proof: secret(),
        principal: Uuid::new_v4().to_string(),
        credential: Uuid::new_v4().to_string(),
        human,
    };
    seed_principal(state, &c, name).await;
    if human {
        seed_browser(state, &c).await;
    } else {
        seed_agent(state, &c).await;
    }
    c
}

/// Seeds the principal row: a human administrator or an agent.
async fn seed_principal(state: &AppState, c: &Caller, name: &str) {
    let (kind, role) = if c.human {
        ("human", "admin")
    } else {
        ("agent", "agent")
    };
    sqlx::query(
        "INSERT INTO principals(id,name,kind,role,password_hash,created_at) VALUES(?,?,?,?,?,?)",
    )
    .bind(&c.principal)
    .bind(name)
    .bind(kind)
    .bind(role)
    .bind(c.human.then_some("unused"))
    .bind(state.now())
    .execute(&state.pool)
    .await
    .unwrap();
}

/// Seeds a human's browser session.
async fn seed_browser(state: &AppState, c: &Caller) {
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
}

/// Seeds an agent's credential and open session.
async fn seed_agent(state: &AppState, c: &Caller) {
    sqlx::query("INSERT INTO credentials(id,principal_id,token_hash,created_at) VALUES(?,?,?,?)")
        .bind(&c.credential)
        .bind(&c.principal)
        .bind(digest(&c.token))
        .bind(state.now())
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO agent_sessions(id,principal_id,credential_id,workstation_id,proof_hash,created_at,capabilities,harness) VALUES(?,?,?,?,?,?,'[\"git\"]','test')")
        .bind(&c.session).bind(&c.principal).bind(&c.credential)
        .bind(format!("{}-ws", c.principal)).bind(digest(&c.proof))
        .bind(state.now()).execute(&state.pool).await.unwrap();
}

/// Sends one request, authenticated as `c` when given.
async fn send(
    app: Router,
    c: Option<&Caller>,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let mut r = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("idempotency-key", Uuid::new_v4().to_string());
    if let Some(c) = c {
        r = authenticate(r, c);
    }
    let response = app
        .oneshot(r.body(request_body(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

/// An empty body for `null`, else the JSON text.
fn request_body(body: Value) -> Body {
    if body.is_null() {
        Body::empty()
    } else {
        Body::from(body.to_string())
    }
}

/// Adds `c`'s browser cookie or bearer and session headers.
fn authenticate(r: axum::http::request::Builder, c: &Caller) -> axum::http::request::Builder {
    if c.human {
        let csrf = digest(&format!("coordinator-browser-csrf-v1:{}", c.token));
        return r
            .header("cookie", format!("coordinator_local={}", c.token))
            .header("origin", "http://127.0.0.1:8080")
            .header("x-csrf-token", csrf);
    }
    r.header("authorization", format!("Bearer {}", c.token))
        .header("x-coordinator-session", &c.session)
        .header("x-coordinator-session-proof", &c.proof)
}

/// Binding comes from an acknowledgment or an attempt; closed sessions, dead
/// credentials and sessions bound only to another project are left out.
#[tokio::test]
async fn binding_comes_from_acknowledgments_or_attempts_and_excludes_dead_sessions() {
    let f = Fixture::new().await;
    let (p, q) = (f.project("bound").await, f.project("elsewhere").await);
    let agents = bound_agents(&f, &p, &q).await;
    let mut listed = f.ids(&p, "").await;
    listed.sort();
    let mut expected = vec![agents[0].session.clone(), agents[1].session.clone()];
    expected.sort();
    assert_eq!(listed, expected);
    assert_eq!(f.ids(&q, "").await, vec![agents[2].session.clone()]);
}

/// Seeds six agents: [0] bound only by an attempt in `p`, [1] by an
/// acknowledgment of `p`, [2] bound only to `q`, and [3..] bound to `p` but
/// closed, revoked and expired respectively.
async fn bound_agents(f: &Fixture, p: &str, q: &str) -> Vec<Caller> {
    let mut agents = Vec::new();
    for name in ["worker", "acked", "other", "closed", "revoked", "expired"] {
        agents.push(f.agent(name).await);
    }
    let t = f.task(&agents[0], p, "Bound work").await;
    f.claim(&agents[0], p, &t).await;
    let sql = "DELETE FROM instruction_acknowledgments WHERE session_id=?";
    f.exec(sql, &[&agents[0].session], &[]).await;
    f.ack(&agents[2], q).await;
    for a in [&agents[1], &agents[3], &agents[4], &agents[5]] {
        f.ack(a, p).await;
    }
    close_and_kill(f, &agents[3], &agents[4], &agents[5]).await;
    agents
}

/// Closes `closed`'s session, revokes `revoked`'s credential and expires
/// `expired`'s credential.
async fn close_and_kill(f: &Fixture, closed: &Caller, revoked: &Caller, expired: &Caller) {
    let now = f.now();
    f.update(
        "UPDATE agent_sessions SET closed_at=? WHERE id=?",
        now,
        &closed.session,
    )
    .await;
    f.update(
        "UPDATE credentials SET revoked_at=? WHERE id=?",
        now,
        &revoked.credential,
    )
    .await;
    f.update(
        "UPDATE credentials SET expires_at=? WHERE id=?",
        now - 1,
        &expired.credential,
    )
    .await;
}

/// A work claim and a review claim are both listed as held attempts; the
/// review names its activity kind and subject task.
#[tokio::test]
async fn held_attempts_name_work_and_review_claims() {
    let f = Fixture::new().await;
    let p = f.project("held").await;
    let (author, reviewer, other) = (
        f.agent("author").await,
        f.agent("reviewer").await,
        f.agent("other").await,
    );
    let t = f.task(&author, &p, "Reviewed work").await;
    let attempt = f.claim(&author, &p, &t).await;
    let review = f.submit(&author, &p, &t, &attempt).await;
    f.claim_review(&reviewer, &p, &review).await;
    let w = f.task(&other, &p, "Plain work").await;
    let work = f.claim(&other, &p, &w).await;
    let (s, v) = f.list(&f.admin, &p, "").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(item(&v, &author.session)["held_attempts"], json!([]));
    assert_review_hold(
        &item(&v, &reviewer.session)["held_attempts"][0],
        &t,
        &review,
    );
    assert_work_hold(&item(&v, &other.session)["held_attempts"][0], &work);
}

/// A held review claim names the review kind, its subject and its activity task.
fn assert_review_hold(held: &Value, subject: &Value, review: &Value) {
    assert_eq!(held["activity_kind"], "agent_review");
    assert_eq!(held["subject_task_id"], subject["id"]);
    assert_eq!(held["task_id"], review["activity_task_id"]);
    assert_eq!(held["mode"], "work");
}

/// A held work claim names its attempt, task title and generation, and no activity.
fn assert_work_hold(held: &Value, work: &Value) {
    assert_eq!(held["attempt_id"], work["id"]);
    assert_eq!(held["task_title"], "Plain work");
    assert_eq!(held["activity_kind"], Value::Null);
    assert_eq!(held["lease_expired"], false);
    assert_eq!(held["subject_task_id"], Value::Null);
    assert_eq!(held["generation"], work["generation"]);
}

/// The window hides an old idle session by default, `0` shows it, a session
/// holding an attempt is always shown, and the newest activity sorts first.
#[tokio::test]
async fn last_activity_window_and_ordering() {
    let f = Fixture::new().await;
    let p = f.project("window").await;
    let [fresh, holder, idle] = window_sessions(&f, &p).await;
    let both = vec![fresh.clone(), holder.clone()];
    assert_eq!(f.ids(&p, "").await, both);
    assert_eq!(f.ids(&p, "?active_within_hours=1").await, both);
    let all = vec![fresh, holder.clone(), idle];
    assert_eq!(f.ids(&p, "?active_within_hours=0").await, all);
    let (_, v) = f.list(&f.admin, &p, "?active_within_hours=0").await;
    let listed = item(&v, &holder);
    assert_eq!(listed["last_activity_at"], timestamp(f.now() - 46 * HOUR));
    assert_eq!(listed["started_at"], timestamp(f.now() - 47 * HOUR));
    assert_eq!(
        (
            v["data"]["active_within_hours"].clone(),
            v["data"]["truncated"].clone()
        ),
        (json!(0), json!(false))
    );
}

/// Seeds three sessions in `p` and returns their ids: one acknowledged now,
/// one that claimed 47 hours ago and checkpointed 46 hours ago but still
/// holds its attempt, and one idle since acknowledging 48 hours ago.
async fn window_sessions(f: &Fixture, p: &str) -> [String; 3] {
    let (holder, idle, fresh) = (
        f.agent("holder").await,
        f.agent("idle").await,
        f.agent("fresh").await,
    );
    let t = f.task(&holder, p, "Long work").await;
    let attempt = f.claim(&holder, p, &t).await;
    f.checkpoint(&holder, p, &attempt).await;
    f.ack(&idle, p).await;
    f.ack(&fresh, p).await;
    backdate(f, &holder, 47).await;
    backdate(f, &idle, 48).await;
    f.exec(
        "UPDATE checkpoints SET created_at=?",
        &[],
        &[f.now() - 46 * HOUR],
    )
    .await;
    [fresh.session, holder.session, idle.session]
}

/// Moves every recorded touch of `c`'s session `hours` into the past,
/// keeping any held attempt's lease live.
async fn backdate(f: &Fixture, c: &Caller, hours: i64) {
    let at = f.now() - hours * HOUR;
    for sql in [
        "UPDATE agent_sessions SET created_at=?1 WHERE id=?2",
        "UPDATE instruction_acknowledgments SET created_at=?1 WHERE session_id=?2",
        "UPDATE attempts SET created_at=?1,last_heartbeat_at=?1,last_progress_at=?1 WHERE session_id=?2",
    ] {
        f.update(sql, at, &c.session).await;
    }
}

/// A subagent session carries its identity name and parent session.
#[tokio::test]
async fn subagent_sessions_name_their_identity() {
    let f = Fixture::new().await;
    let p = f.project("subagents").await;
    let parent = f.agent("parent").await;
    f.ack(&parent, &p).await;
    let child = Uuid::new_v4().to_string();
    let identity = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO subagent_identities(id,project_id,principal_id,name,created_by_session_id,created_at) VALUES(?,?,?,'reviewer-1',?,?)")
        .bind(&identity).bind(&p).bind(&parent.principal).bind(&parent.session).bind(f.now())
        .execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO agent_sessions(id,principal_id,credential_id,workstation_id,proof_hash,created_at,capabilities,harness,subagent_identity_id,parent_session_id) SELECT ?,principal_id,credential_id,workstation_id,'x',created_at,capabilities,harness,?,id FROM agent_sessions WHERE id=?")
        .bind(&child).bind(&identity).bind(&parent.session).execute(&f.state.pool).await.unwrap();
    sqlx::query("INSERT INTO instruction_acknowledgments(session_id,project_id,policy_revision,instruction_version,created_at) VALUES(?,?,1,'v',?)")
        .bind(&child).bind(&p).bind(f.now()).execute(&f.state.pool).await.unwrap();
    let (_, v) = f.list(&f.admin, &p, "").await;
    assert_eq!(
        item(&v, &child)["subagent"],
        json!({"name":"reviewer-1","parent_session_id":parent.session})
    );
    assert_eq!(item(&v, &parent.session)["subagent"], Value::Null);
    assert_eq!(item(&v, &parent.session)["principal"]["name"], "parent");
    assert_eq!(item(&v, &parent.session)["capabilities"], json!(["git"]));
}

/// Bad windows are 400, unknown projects 404, anonymous callers 401; a human
/// browser session reads the list.
#[tokio::test]
async fn parameters_projects_and_callers_are_checked() {
    let f = Fixture::new().await;
    let p = f.project("checked").await;
    for bad in [
        "?active_within_hours=-1",
        "?active_within_hours=8761",
        "?active_within_hours=abc",
    ] {
        let (s, v) = f.list(&f.admin, &p, bad).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}: {v}");
    }
    assert_eq!(
        f.list(&f.admin, "no-such-project", "").await.0,
        StatusCode::NOT_FOUND
    );
    let path = format!("/api/v1/projects/{p}/sessions");
    let (s, _) = send(f.app.clone(), None, "GET", &path, Value::Null).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, v) = f.list(&f.admin, &p, "?active_within_hours=8760").await;
    assert_eq!(s, StatusCode::OK, "{v}");
}

/// A read-only credential may list, and no proof or token verifier leaks.
#[tokio::test]
async fn read_only_agents_can_list_and_no_secret_hash_is_exposed() {
    let f = Fixture::new().await;
    let p = f.project("secrets").await;
    let reader = f.agent("reader").await;
    f.ack(&reader, &p).await;
    f.exec(
        "UPDATE credentials SET access='read' WHERE id=?",
        &[&reader.credential],
        &[],
    )
    .await;
    let (s, v) = f.list(&reader, &p, "").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(items(&v).len(), 1);
    let text = v.to_string();
    for secret in [
        "proof_hash",
        "token_hash",
        &digest(&reader.proof),
        &digest(&reader.token),
    ] {
        assert!(!text.contains(secret), "{secret} leaked: {text}");
    }
}

/// The integrator class is scoped only on mutations, so it reads this like
/// any other GET route.
#[tokio::test]
async fn integrator_credentials_read_like_other_get_routes() {
    let f = Fixture::new().await;
    let p = f.project("integrator-read").await;
    let integrator = f.agent("integrator").await;
    f.exec(
        "UPDATE credentials SET class='integrator' WHERE id=?",
        &[&integrator.credential],
        &[],
    )
    .await;
    let (s, v) = f.list(&integrator, &p, "").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(items(&v).len(), 0);
}

/// An active attempt whose lease lapsed is still held, flagged
/// `lease_expired`, and keeps an otherwise idle session in the default window.
#[tokio::test]
async fn lapsed_active_attempts_stay_listed_and_flagged() {
    let f = Fixture::new().await;
    let p = f.project("lapsed").await;
    let stale = f.agent("stale").await;
    let t = f.task(&stale, &p, "Abandoned work").await;
    let attempt = f.claim(&stale, &p, &t).await;
    backdate(&f, &stale, 48).await;
    f.update(
        "UPDATE attempts SET expires_at=? WHERE id=?",
        f.now() - HOUR,
        id(&attempt),
    )
    .await;
    assert_eq!(
        f.ids(&p, "?active_within_hours=1").await,
        vec![stale.session.clone()]
    );
    let (_, v) = f.list(&f.admin, &p, "").await;
    let held = &item(&v, &stale.session)["held_attempts"][0];
    assert_eq!(held["attempt_id"], attempt["id"]);
    assert_eq!(held["lease_expired"], true);
}
