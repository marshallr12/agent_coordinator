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
    fn use_monotonic_elapsed(&self) -> bool {
        false
    }
}

const DAY: i64 = 86_400_000;
const HOUR: i64 = 3_600_000;

#[derive(Clone)]
struct Caller {
    token: String,
    session: String,
    proof: String,
    principal: String,
    _credential: String,
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
    integrator: Caller,
    canary: Caller,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::open(Config {
            database_path: dir.path().join("attention.sqlite3"),
            public_origin: "http://127.0.0.1:8080".into(),
            allow_insecure_loopback: true,
            canary_principals: vec!["attention-canary".into()],
            ..Config::default()
        })
        .await
        .unwrap();
        let clock = Arc::new(TestClock(AtomicI64::new(1_800_000_000_000)));
        state.clock = clock.clone();
        let admin = seed(&state, true, "attention-admin").await;
        let a = seed(&state, false, "attention-a").await;
        let b = seed(&state, false, "attention-b").await;
        let integrator = seed(&state, false, "attention-integrator").await;
        let canary = seed(&state, false, "attention-canary").await;
        sqlx::query("UPDATE credentials SET class='integrator' WHERE id=?")
            .bind(&integrator._credential)
            .execute(&state.pool)
            .await
            .unwrap();
        Self {
            app: router(state.clone()),
            state,
            clock,
            _dir: dir,
            admin,
            a,
            b,
            integrator,
            canary,
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

    async fn ok(&self, caller: &Caller, method: &str, path: &str, body: Value) -> Value {
        let (status, value) = self.call(caller, method, path, body).await;
        assert_eq!(status, StatusCode::OK, "{value}");
        value["data"].clone()
    }

    async fn project(&self, name: &str) -> String {
        let created = self
            .ok(
                &self.admin,
                "POST",
                "/api/v1/projects",
                json!({"name":name,"repository_url":format!("https://example.test/{name}.git"),"target_branch":"main"}),
            )
            .await;
        let project = created["id"].as_str().unwrap().to_owned();
        sqlx::query("UPDATE projects SET integration_owner='integrator' WHERE id=?")
            .bind(&project)
            .execute(&self.state.pool)
            .await
            .unwrap();
        project
    }

    async fn task(&self, project: &str, title: &str) -> Value {
        self.ok(
            &self.a,
            "POST",
            &format!("/api/v1/projects/{project}/tasks"),
            json!({"title":title,"description":"attention budget","acceptance_criteria":["done"],"kind":"general"}),
        )
        .await
    }

    async fn ack(&self, caller: &Caller, project: &str) {
        self.ok(
            caller,
            "POST",
            &format!(
                "/api/v1/sessions/{}/instruction-acknowledgments",
                caller.session
            ),
            json!({"project_id":project,"policy_revision":self.policy(project).await,
                "instruction_version":coordinator_core::INSTRUCTION_VERSION,
                "sections":[coordinator_core::REQUIRED_SECTION]}),
        )
        .await;
    }

    async fn policy(&self, project: &str) -> i64 {
        sqlx::query_scalar("SELECT policy_revision FROM projects WHERE id=?")
            .bind(project)
            .fetch_one(&self.state.pool)
            .await
            .unwrap()
    }

    async fn next(&self, caller: &Caller, project: &str) -> Value {
        self.ok(
            caller,
            "GET",
            &format!("/api/v1/projects/{project}/next?role=implementer"),
            Value::Null,
        )
        .await
    }

    /// Claims whatever `next` offers and returns the claimed attempt.
    async fn claim_offered(&self, caller: &Caller, project: &str) -> Value {
        self.ack(caller, project).await;
        let offered = self.next(caller, project).await;
        let call = &offered["action"]["call"];
        let claimed = self
            .ok(
                caller,
                "POST",
                call["path"].as_str().unwrap(),
                call["body"].clone(),
            )
            .await;
        claimed["claim"]["attempt"].clone()
    }

    async fn release(&self, caller: &Caller, project: &str, attempt: &Value) {
        self.ok(
            caller,
            "POST",
            &format!(
                "/api/v1/projects/{project}/attempts/{}/release",
                attempt["id"].as_str().unwrap()
            ),
            json!({"generation":attempt["generation"],"summary":"Could not make progress"}),
        )
        .await;
    }

    async fn digest(&self, project: &str) -> Value {
        self.ok(
            &self.a,
            "GET",
            &format!("/api/v1/projects/{project}/digest"),
            Value::Null,
        )
        .await
    }

    async fn decision_body(&self, project: &str, task: &Value, reversible: bool) -> Value {
        json!({"question":"Rename the flag?","options":["Rename","Keep"],
            "rationale":"Either is easy to undo","required_actor":"human",
            "affected_tasks":[{"task_id":task["id"],"task_revision":task["revision"]}],
            "policy_revision":self.policy(project).await,"environment":"test",
            "conditions":"Nothing ships before review","recommendation":"Rename",
            "reversible":reversible})
    }

    /// A decision a human opened: only a human may make one both reversible
    /// and human-required.
    async fn reversible_decision(&self, project: &str, task: &Value, reversible: bool) -> Value {
        let body = self.decision_body(project, task, reversible).await;
        self.ok(
            &self.admin,
            "POST",
            &format!("/api/v1/projects/{project}/decisions"),
            body,
        )
        .await
    }
}

#[tokio::test]
async fn an_unanswered_reversible_decision_proceeds_after_a_day_and_is_in_the_digest() {
    let f = Fixture::new().await;
    let p = f.project("attention-decision").await;
    let task = f.task(&p, "Rename the flag").await;
    let decision = f.reversible_decision(&p, &task, true).await;
    let irreversible = f.reversible_decision(&p, &task, false).await;
    assert_eq!(decision["recommendation"], "Rename");
    assert_eq!(decision["status"], "pending");
    assert!(
        f.next(&f.a, &p).await["action"].is_null(),
        "blocked while pending"
    );

    f.clock.0.fetch_add(DAY - 1, Ordering::SeqCst);
    let early = coordinator_server::attention::sweep_timed_out_decisions(&f.state)
        .await
        .unwrap();
    assert!(early.is_empty(), "{early:?}");
    let digest = f.digest(&p).await;
    assert_eq!(
        digest["pending_reversible_decisions"][0]["decision_id"],
        decision["id"]
    );
    assert_eq!(digest["proceeded_decisions"], json!([]));

    f.clock.0.fetch_add(1, Ordering::SeqCst);
    let swept = coordinator_server::attention::sweep_timed_out_decisions(&f.state)
        .await
        .unwrap();
    assert_eq!(swept, vec![decision["id"].as_str().unwrap().to_owned()]);
    let swept_again = coordinator_server::attention::sweep_timed_out_decisions(&f.state)
        .await
        .unwrap();
    assert!(swept_again.is_empty());

    let path = |d: &Value| {
        format!(
            "/api/v1/projects/{p}/decisions/{}",
            d["id"].as_str().unwrap()
        )
    };
    let answered = f.ok(&f.a, "GET", &path(&decision), Value::Null).await;
    assert_eq!(answered["status"], "allowed", "{answered}");
    assert_eq!(answered["answer"]["answer"], "Rename");
    assert_eq!(answered["answer"]["timed_out"], true);
    let still_open = f.ok(&f.a, "GET", &path(&irreversible), Value::Null).await;
    assert_eq!(
        still_open["status"], "pending",
        "an irreversible decision never times out"
    );

    let digest = f.digest(&p).await;
    let listed = &digest["proceeded_decisions"][0];
    assert_eq!(listed["decision_id"], decision["id"], "{digest}");
    assert_eq!(listed["proceeded_with"], "Rename");
    assert_eq!(listed["affected_task_ids"][0], task["id"]);
    assert_eq!(digest["pending_reversible_decisions"], json!([]));
}

#[tokio::test]
async fn an_agent_cannot_open_a_reversible_human_decision_but_a_human_can() {
    let f = Fixture::new().await;
    let p = f.project("attention-agent-reversible").await;
    let task = f.task(&p, "Rename the flag").await;
    let path = format!("/api/v1/projects/{p}/decisions");

    let body = f.decision_body(&p, &task, true).await;
    let (status, refused) = f.call(&f.a, "POST", &path, body.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");

    let mut either = body.clone();
    either["required_actor"] = json!("either");
    f.ok(&f.a, "POST", &path, either).await;
    let mut irreversible = body.clone();
    irreversible["reversible"] = json!(false);
    f.ok(&f.a, "POST", &path, irreversible).await;

    let human = f.ok(&f.admin, "POST", &path, body).await;
    assert_eq!(human["reversible"], true);
    assert_eq!(human["required_actor"], "human");

    let reopen = json!({"expected_generation":1,"rationale":"Scope looked stale",
        "affected_tasks":[{"task_id":task["id"],"task_revision":task["revision"]}],
        "policy_revision":f.policy(&p).await,"environment":"test",
        "conditions":"Nothing ships before review"});
    let reopen_path = format!("{path}/{}/reopen", human["id"].as_str().unwrap());
    let (status, refused) = f.call(&f.a, "POST", &reopen_path, reopen.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
    let reopened = f.ok(&f.admin, "POST", &reopen_path, reopen).await;
    assert_eq!(reopened["generation"], 2);
}

#[tokio::test]
async fn the_sweep_skips_a_human_decision_an_agent_opened() {
    let f = Fixture::new().await;
    let p = f.project("attention-sweep-skip").await;
    let task = f.task(&p, "Rename the flag").await;
    let human = f.reversible_decision(&p, &task, true).await;
    let agent_made = f.reversible_decision(&p, &task, true).await;
    // Rows from before the service refused this: the cycle was opened by an
    // agent, as when an agent created or last reopened the decision.
    sqlx::query("DROP TRIGGER decision_cycles_immutable_update")
        .execute(&f.state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE decision_cycles SET opened_by=? WHERE decision_id=?")
        .bind(&f.a.principal)
        .bind(agent_made["id"].as_str().unwrap())
        .execute(&f.state.pool)
        .await
        .unwrap();

    f.clock.0.fetch_add(DAY, Ordering::SeqCst);
    let digest = f.digest(&p).await;
    let pending = digest["pending_reversible_decisions"].as_array().unwrap();
    assert_eq!(pending.len(), 1, "{digest}");
    assert_eq!(pending[0]["decision_id"], human["id"]);
    let swept = coordinator_server::attention::sweep_timed_out_decisions(&f.state)
        .await
        .unwrap();
    assert_eq!(swept, vec![human["id"].as_str().unwrap().to_owned()]);
    let skipped = f
        .ok(
            &f.a,
            "GET",
            &format!(
                "/api/v1/projects/{p}/decisions/{}",
                agent_made["id"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
    assert_eq!(skipped["status"], "pending", "{skipped}");
}

#[tokio::test]
async fn a_reversible_decision_needs_a_recommendation_among_its_options() {
    let f = Fixture::new().await;
    let p = f.project("attention-validation").await;
    let task = f.task(&p, "Rename the flag").await;
    for (recommendation, reversible) in [(Value::Null, true), (json!("Elsewhere"), false)] {
        let (status, value) = f
            .call(
                &f.a,
                "POST",
                &format!("/api/v1/projects/{p}/decisions"),
                json!({"question":"Rename the flag?","options":["Rename","Keep"],
                    "rationale":"Either is easy to undo","required_actor":"human",
                    "affected_tasks":[{"task_id":task["id"],"task_revision":task["revision"]}],
                    "policy_revision":f.policy(&p).await,"recommendation":recommendation,
                    "reversible":reversible}),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
    }
}

#[tokio::test]
async fn a_task_that_keeps_ending_without_a_submission_counts_as_a_human_intervention() {
    let f = Fixture::new().await;
    let p = f.project("attention-stall").await;
    f.task(&p, "Stubborn task").await;
    assert_eq!(f.digest(&p).await["hri"]["count"], 0);
    for round in 0..3 {
        let claimant = if round % 2 == 0 { &f.a } else { &f.b };
        let attempt = f.claim_offered(claimant, &p).await;
        f.release(claimant, &p, &attempt).await;
        let hri = f.digest(&p).await["hri"].clone();
        assert_eq!(
            hri["count"],
            if round < 2 { 0 } else { 1 },
            "round {round}: {hri}"
        );
    }
    let hri = f.digest(&p).await["hri"].clone();
    assert_eq!(hri["stalled_tasks"], 1);
    assert_eq!(hri["items"][0]["code"], "stalled_task", "{hri}");
    assert_eq!(hri["items"][0]["rule"], "repeated_attempt_failures");
    assert_eq!(hri["stalled_queue"], 0);
}

#[tokio::test]
async fn an_idle_queue_with_ready_work_counts_as_a_stall_after_the_threshold() {
    let f = Fixture::new().await;
    let p = f.project("attention-idle").await;
    f.task(&p, "Nobody claims this").await;
    f.clock.0.fetch_add(6 * HOUR, Ordering::SeqCst);
    let hri = f.digest(&p).await["hri"].clone();
    assert_eq!(
        hri["count"], 0,
        "exactly the threshold is not yet a stall: {hri}"
    );
    f.clock.0.fetch_add(HOUR, Ordering::SeqCst);
    let hri = f.digest(&p).await["hri"].clone();
    assert_eq!(hri["count"], 1, "{hri}");
    assert_eq!(hri["stalled_queue"], 1);
    assert_eq!(hri["stalled_tasks"], 0);
    let item = &hri["items"][0];
    assert_eq!(item["code"], "stalled_queue");
    assert_eq!(item["rule"], "no_progress");
    assert_eq!(item["ready_tasks"], 1);
    assert_eq!(item["idle_hours"], 7);
    assert_eq!(item["threshold_hours"], 6);
}

#[tokio::test]
async fn quiet_hours_do_not_count_toward_the_stall_clock() {
    let f = Fixture::new().await;
    let p = f.project("attention-quiet").await;
    f.task(&p, "Waits overnight").await;
    // The fixture clock sits at 08:00 UTC; the 7 hours since then are 08:00-15:00.
    f.clock.0.fetch_add(7 * HOUR, Ordering::SeqCst);
    let mut state = f.state.clone();
    state.config.quiet_hours = Some("8-12".parse().unwrap());
    let app = router(state);
    let digest = |app: Router| {
        let caller = f.a.clone();
        let path = format!("/api/v1/projects/{p}/digest");
        async move {
            let (status, value) = call(
                app,
                &caller,
                "GET",
                &path,
                &Uuid::new_v4().to_string(),
                Value::Null,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{value}");
            value["data"]["hri"].clone()
        }
    };
    // Four of those hours are quiet, leaving three.
    assert_eq!(digest(app.clone()).await["count"], 0);
    f.clock.0.fetch_add(4 * HOUR, Ordering::SeqCst);
    let hri = digest(app).await;
    assert_eq!(hri["count"], 1, "{hri}");
    assert_eq!(hri["items"][0]["idle_hours"], 7);
}

#[tokio::test]
async fn an_empty_queue_is_not_a_stall() {
    let f = Fixture::new().await;
    let p = f.project("attention-empty").await;
    f.clock.0.fetch_add(2 * DAY, Ordering::SeqCst);
    let hri = f.digest(&p).await["hri"].clone();
    assert_eq!(hri["count"], 0, "{hri}");
    // A task somebody owns is not ready work either.
    f.task(&p, "Being worked").await;
    f.claim_offered(&f.a, &p).await;
    f.clock.0.fetch_add(2 * DAY, Ordering::SeqCst);
    let hri = f.digest(&p).await["hri"].clone();
    assert_eq!(hri["count"], 0, "{hri}");
}

#[tokio::test]
async fn recent_progress_keeps_a_queue_with_ready_work_from_stalling() {
    let f = Fixture::new().await;
    let p = f.project("attention-progress").await;
    f.task(&p, "First").await;
    f.task(&p, "Second").await;
    f.clock.0.fetch_add(7 * HOUR, Ordering::SeqCst);
    assert_eq!(f.digest(&p).await["hri"]["count"], 1);
    // A claim is progress, even though the other task is still waiting.
    f.claim_offered(&f.a, &p).await;
    assert_eq!(f.digest(&p).await["hri"]["count"], 0);
    f.clock.0.fetch_add(5 * HOUR, Ordering::SeqCst);
    assert_eq!(f.digest(&p).await["hri"]["count"], 0);
    f.clock.0.fetch_add(2 * HOUR, Ordering::SeqCst);
    let hri = f.digest(&p).await["hri"].clone();
    assert_eq!(hri["count"], 1, "{hri}");
    assert_eq!(hri["items"][0]["rule"], "no_progress");
}

#[tokio::test]
async fn next_skips_a_task_whose_paths_overlap_a_recent_human_change() {
    let f = Fixture::new().await;
    let p = f.project("attention-overlap").await;
    let touched = f.task(&p, "Touch the server").await;
    let elsewhere = f.task(&p, "Touch the docs").await;
    let paths = |task: &Value| {
        format!(
            "/api/v1/projects/{p}/tasks/{}/paths",
            task["id"].as_str().unwrap()
        )
    };
    f.ok(
        &f.a,
        "POST",
        &paths(&touched),
        json!({"paths":["crates/server/"]}),
    )
    .await;
    f.ok(
        &f.a,
        "POST",
        &paths(&elsewhere),
        json!({"paths":["book/src/docs"]}),
    )
    .await;
    sqlx::query("UPDATE tasks SET ready_since=ready_since-1000 WHERE id=?")
        .bind(touched["id"].as_str().unwrap())
        .execute(&f.state.pool)
        .await
        .unwrap();
    f.ack(&f.b, &p).await;
    // The older task is offered first until a human change lands on its paths.
    assert_eq!(f.next(&f.b, &p).await["action"]["task_id"], touched["id"]);

    let ships = format!("/api/v1/projects/{p}/integrator/human-ships");
    let ship = json!({"commit":"abcdef1234","files":["crates/server/src/next.rs"]});
    let (status, _) = f.call(&f.admin, "POST", &ships, ship.clone()).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "only the integrator records human ships"
    );
    let (status, _) = f.call(&f.a, "POST", &ships, ship.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let recorded = f.ok(&f.integrator, "POST", &ships, ship).await;
    assert_eq!(recorded["recorded_files"], 1);

    let offered = f.next(&f.b, &p).await;
    assert_eq!(offered["action"]["task_id"], elsewhere["id"], "{offered}");
    assert_eq!(offered["skipped"]["path_overlap"], 1, "{offered}");

    f.clock.0.fetch_add(DAY, Ordering::SeqCst);
    let later = f.next(&f.b, &p).await;
    assert_eq!(
        later["action"]["task_id"], touched["id"],
        "the hold lasts 24 hours: {later}"
    );
}

/// A request with no credentials, as a mail client's browser makes it.
async fn anonymous(
    app: &Router,
    method: &str,
    path: &str,
    form: Option<&str>,
) -> (StatusCode, String) {
    let mut request = Request::builder().method(method).uri(path);
    if form.is_some() {
        request = request.header("content-type", "application/x-www-form-urlencoded");
    }
    let response = app
        .clone()
        .oneshot(
            request
                .body(Body::from(form.unwrap_or_default().to_owned()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

/// The ack link the digest hands `caller` when asked for one: path and token,
/// or `None` when it comes without a link.
async fn ack_link_for(f: &Fixture, caller: &Caller, project: &str) -> Option<(String, String)> {
    let digest = f
        .ok(
            caller,
            "GET",
            &format!("/api/v1/projects/{project}/digest?ack_link=true"),
            Value::Null,
        )
        .await;
    assert!(digest["window_hours"].is_number(), "{digest}");
    let url = digest["ack_link"]["url"].as_str()?.to_owned();
    let url = url.strip_prefix("http://127.0.0.1:8080").unwrap();
    let (path, token) = url.split_once("?token=").unwrap();
    Some((path.to_owned(), token.to_owned()))
}

/// An ack link minted by the human administrator.
async fn ack_link(f: &Fixture, project: &str) -> (String, String) {
    ack_link_for(f, &f.admin, project).await.unwrap()
}

/// The principal and mint record of the project's last read.
async fn read_record(f: &Fixture, project: &str) -> (String, Option<String>) {
    sqlx::query_as("SELECT read_via,minted_by FROM digest_reads WHERE project_id=?")
        .bind(project)
        .fetch_one(&f.state.pool)
        .await
        .unwrap()
}

async fn designate_sender(f: &Fixture, project: &str, principal: Value) -> (StatusCode, Value) {
    f.call(
        &f.admin,
        "POST",
        &format!("/api/v1/projects/{project}/digest/sender"),
        json!({"principal_id":principal}),
    )
    .await
}

#[tokio::test]
async fn a_human_opening_the_digest_records_the_read_and_an_agent_cannot() {
    let f = Fixture::new().await;
    let p = f.project("digest-read").await;
    assert_eq!(f.digest(&p).await["last_read_at"], Value::Null);
    assert_eq!(f.digest(&p).await["ack_link"], Value::Null);

    let path = format!("/api/v1/projects/{p}/digest/read");
    let (status, refused) = f.call(&f.a, "POST", &path, json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(f.digest(&p).await["last_read_at"], Value::Null);

    let read = f.ok(&f.admin, "POST", &path, json!({})).await;
    let now = coordinator_core::timestamp(f.clock.0.load(Ordering::SeqCst));
    assert_eq!(read["last_read_at"], now);
    assert_eq!(f.digest(&p).await["last_read_at"], now);

    f.clock.0.fetch_add(2 * HOUR, Ordering::SeqCst);
    f.ok(&f.admin, "POST", &path, json!({})).await;
    let later = coordinator_core::timestamp(f.clock.0.load(Ordering::SeqCst));
    assert_eq!(f.digest(&p).await["last_read_at"], later);
}

#[tokio::test]
async fn the_signed_ack_link_records_a_read_without_any_credential() {
    let f = Fixture::new().await;
    let p = f.project("digest-ack").await;
    let (path, token) = ack_link(&f, &p).await;
    assert_eq!(path, format!("/api/v1/projects/{p}/digest/ack"));

    // Opening the link only shows a button; a link preview records nothing.
    let (status, page) = anonymous(&f.app, "GET", &format!("{path}?token={token}"), None).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page.contains("I read this"), "{page}");
    assert_eq!(f.digest(&p).await["last_read_at"], Value::Null);

    let (status, done) = anonymous(&f.app, "POST", &path, Some(&format!("token={token}"))).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    let now = coordinator_core::timestamp(f.clock.0.load(Ordering::SeqCst));
    assert_eq!(f.digest(&p).await["last_read_at"], now);
    let (via, minted_by) = read_record(&f, &p).await;
    assert_eq!(via, "ack_link");
    assert_eq!(minted_by.as_deref(), Some(f.admin.principal.as_str()));
}

#[tokio::test]
async fn only_a_human_or_the_designated_sender_gets_an_ack_link() {
    let f = Fixture::new().await;
    let p = f.project("digest-sender").await;

    // An ordinary agent asking for a link gets the digest without one, and so
    // does a second agent while another is designated.
    assert_eq!(ack_link_for(&f, &f.a, &p).await, None);
    assert_eq!(f.digest(&p).await["digest_sender"], Value::Null);

    // Agents cannot designate themselves or anyone else.
    let (status, refused) = f
        .call(
            &f.a,
            "POST",
            &format!("/api/v1/projects/{p}/digest/sender"),
            json!({"principal_id":f.a.principal}),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(ack_link_for(&f, &f.a, &p).await, None);

    // The designation names an enabled agent principal, not a human.
    let (status, body) = designate_sender(&f, &p, json!(f.admin.principal)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = designate_sender(&f, &p, json!("no-such-principal")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // The designated sender gets a link; the other agent still does not.
    let (status, body) = designate_sender(&f, &p, json!(f.a.principal)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.digest(&p).await["digest_sender"], json!(f.a.principal));
    let (path, token) = ack_link_for(&f, &f.a, &p).await.expect("designated sender");
    assert_eq!(ack_link_for(&f, &f.b, &p).await, None);
    // Only for this project.
    let other = f.project("digest-sender-other").await;
    assert_eq!(ack_link_for(&f, &f.a, &other).await, None);

    // A read through the sender's link names the sender, not the human.
    let (status, done) = anonymous(&f.app, "POST", &path, Some(&format!("token={token}"))).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    let (via, minted_by) = read_record(&f, &p).await;
    assert_eq!(via, "ack_link");
    assert_eq!(minted_by.as_deref(), Some(f.a.principal.as_str()));

    // A human gets a link too, and the record names the human.
    let (path, token) = ack_link_for(&f, &f.admin, &p).await.expect("human");
    let (status, _) = anonymous(&f.app, "POST", &path, Some(&format!("token={token}"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        read_record(&f, &p).await.1.as_deref(),
        Some(f.admin.principal.as_str())
    );

    // Clearing the designation takes the sender's link away and voids the ones
    // it already minted.
    let (path, token) = ack_link_for(&f, &f.a, &p).await.unwrap();
    let (status, body) = designate_sender(&f, &p, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ack_link_for(&f, &f.a, &p).await, None);
    let (status, _) = anonymous(&f.app, "GET", &format!("{path}?token={token}"), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let before = read_record(&f, &p).await;
    let (status, _) = anonymous(&f.app, "POST", &path, Some(&format!("token={token}"))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(read_record(&f, &p).await, before);
}

#[tokio::test]
async fn the_host_operator_designates_the_digest_sender_locally() {
    let f = Fixture::new().await;
    let p = f.project("digest-sender-local").await;
    let designate = |agent: Option<&'static str>| {
        coordinator_server::attention::designate_digest_sender(&f.state, &p, agent, "timer setup")
    };
    assert!(designate(Some("no-such-agent")).await.is_err());
    assert!(designate(Some("attention-admin")).await.is_err());
    assert!(designate(None).await.is_err());
    assert_eq!(ack_link_for(&f, &f.a, &p).await, None);

    let done = designate(Some("attention-a")).await.unwrap();
    assert_eq!(done["digest_sender"], json!(f.a.principal));
    assert!(ack_link_for(&f, &f.a, &p).await.is_some());
    assert_eq!(ack_link_for(&f, &f.b, &p).await, None);

    designate(None).await.unwrap();
    assert_eq!(ack_link_for(&f, &f.a, &p).await, None);
    let events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM events WHERE project_id=? AND kind='digest.sender_set'",
    )
    .bind(&p)
    .fetch_one(&f.state.pool)
    .await
    .unwrap();
    assert_eq!(events, 2);
}

#[tokio::test]
async fn the_ack_link_cannot_do_anything_else() {
    let f = Fixture::new().await;
    let p = f.project("digest-ack-scope").await;
    let other = f.project("digest-ack-other").await;
    let (path, token) = ack_link(&f, &p).await;

    // The token is no credential: not as a bearer token, not on the digest read
    // route, and not for another project.
    let (status, _) = anonymous(&f.app, "GET", &format!("/api/v1/projects/{p}/digest"), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = anonymous(
        &f.app,
        "POST",
        &format!("/api/v1/projects/{p}/digest/read"),
        Some(&format!("token={token}")),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let bearer = Caller {
        token: token.clone(),
        ..f.a.clone()
    };
    let (status, _) = f
        .call(
            &bearer,
            "POST",
            &format!("/api/v1/projects/{p}/tasks"),
            json!({"title":"x","description":"x","acceptance_criteria":["x"],"kind":"general"}),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let other_path = format!("/api/v1/projects/{other}/digest/ack");
    let (status, _) = anonymous(&f.app, "POST", &other_path, Some(&format!("token={token}"))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(f.digest(&other).await["last_read_at"], Value::Null);

    // Other methods on the ack route need a credential, and a forged or edited
    // token is refused.
    let (status, _) = anonymous(&f.app, "DELETE", &path, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (expires, rest) = token.split_once('.').unwrap();
    let (minter, signature) = rest.split_once('.').unwrap();
    let longer: i64 = expires.parse::<i64>().unwrap() + DAY;
    for forged in [
        format!("{longer}.{minter}.{signature}"),
        format!("{expires}.{minter}.{}", "0".repeat(64)),
        // Another principal named as the minter, under the same signature.
        format!("{expires}.{}.{signature}", f.a.principal),
        format!("{expires}.{signature}"),
        "garbage".to_owned(),
        String::new(),
    ] {
        let (status, body) =
            anonymous(&f.app, "POST", &path, Some(&format!("token={forged}"))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{forged}: {body}");
        let (status, _) = anonymous(&f.app, "GET", &format!("{path}?token={forged}"), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{forged}");
    }
    assert_eq!(f.digest(&p).await["last_read_at"], Value::Null);
}

#[tokio::test]
async fn the_ack_link_expires_after_seven_days() {
    let f = Fixture::new().await;
    let p = f.project("digest-ack-expiry").await;
    let (path, token) = ack_link(&f, &p).await;

    f.clock.0.fetch_add(7 * DAY - HOUR, Ordering::SeqCst);
    let (status, _) = anonymous(&f.app, "GET", &format!("{path}?token={token}"), None).await;
    assert_eq!(status, StatusCode::OK);

    f.clock.0.fetch_add(2 * HOUR, Ordering::SeqCst);
    let (status, body) = anonymous(&f.app, "POST", &path, Some(&format!("token={token}"))).await;
    assert_eq!(status, StatusCode::GONE, "{body}");
    let (status, _) = anonymous(&f.app, "GET", &format!("{path}?token={token}"), None).await;
    assert_eq!(status, StatusCode::GONE);
    assert_eq!(f.digest(&p).await["last_read_at"], Value::Null);
}

const WEEK: i64 = 7 * DAY;

impl Fixture {
    async fn create_task(
        &self,
        caller: &Caller,
        project: &str,
        title: &str,
        extra: Value,
    ) -> Value {
        let mut body = json!({"title":title,"description":"admission","acceptance_criteria":["done"],"kind":"general"});
        for (key, value) in extra.as_object().unwrap() {
            body[key] = value.clone();
        }
        self.ok(
            caller,
            "POST",
            &format!("/api/v1/projects/{project}/tasks"),
            body,
        )
        .await
    }

    /// Creates `count` agent tasks in `project` and checks each was admitted.
    async fn admitted(&self, project: &str, count: usize) {
        for n in 0..count {
            let task = self
                .create_task(&self.a, project, &format!("Admitted {n}"), json!({}))
                .await;
            assert_eq!(task["lifecycle"], "open", "{task}");
            assert_eq!(task["admission"]["held"], false, "{task}");
        }
    }
}

#[tokio::test]
async fn the_sixth_agent_task_in_a_week_lands_planned_in_its_own_project_only() {
    let f = Fixture::new().await;
    let (p, q) = (
        f.project("admission-p").await,
        f.project("admission-q").await,
    );
    // Each project has its own budget: ten tasks in two projects are all admitted.
    f.admitted(&p, 5).await;
    f.admitted(&q, 5).await;

    let held = f.create_task(&f.a, &p, "Sixth", json!({})).await;
    assert_eq!(held["lifecycle"], "planned", "{held}");
    assert_eq!(held["origin"], "agent");
    assert_eq!(held["admission"]["held"], true);
    assert_eq!(held["admission"]["weekly_budget"]["limit"], 5);
    assert_eq!(held["admission"]["weekly_budget"]["admitted_this_week"], 5);
    let reason = held["admission"]["reason"].as_str().unwrap();
    assert!(reason.contains("held as planned"), "{reason}");
    assert!(reason.contains("in this project"), "{reason}");
    let also_held = f.create_task(&f.a, &q, "Seventh", json!({})).await;
    assert_eq!(also_held["lifecycle"], "planned", "{also_held}");

    // A third project is unaffected by the two spent budgets.
    let r = f.project("admission-r").await;
    let fresh = f.create_task(&f.a, &r, "Other project", json!({})).await;
    assert_eq!(fresh["lifecycle"], "open", "{fresh}");
    assert_eq!(fresh["admission"]["weekly_budget"]["admitted_this_week"], 1);
    assert_eq!(
        f.digest(&r).await["agent_task_weekly_budget"]["admitted_this_week"],
        1
    );

    let fetched = f
        .ok(
            &f.a,
            "GET",
            &format!(
                "/api/v1/projects/{p}/tasks/{}",
                held["id"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
    assert_eq!(fetched["origin"], "agent", "{fetched}");
    assert_eq!(fetched["held_by_budget"], true);

    let digest = f.digest(&p).await;
    let listed = digest["held_agent_tasks"].as_array().unwrap();
    assert_eq!(listed.len(), 1, "{digest}");
    assert_eq!(listed[0]["task_id"], held["id"]);
    assert_eq!(listed[0]["title"], "Sixth");
    assert_eq!(digest["agent_task_weekly_budget"]["limit"], 5);
    assert_eq!(digest["agent_task_weekly_budget"]["admitted_this_week"], 5);
    assert_eq!(
        f.digest(&q).await["held_agent_tasks"][0]["task_id"],
        also_held["id"]
    );

    // A task the agent itself asked to keep planned neither uses nor is held by the budget.
    let planned = f
        .create_task(&f.a, &p, "Parked", json!({"planned":true}))
        .await;
    assert_eq!(planned["lifecycle"], "planned");
    assert_eq!(planned["admission"]["held"], false, "{planned}");
    assert_eq!(
        f.digest(&p).await["held_agent_tasks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn a_held_task_in_one_project_leaves_another_project_admitting() {
    let f = Fixture::new().await;
    let (p, q) = (
        f.project("admission-busy").await,
        f.project("admission-quiet").await,
    );
    f.admitted(&p, 5).await;
    let held = f.create_task(&f.a, &p, "Sixth", json!({})).await;
    assert_eq!(held["lifecycle"], "planned", "{held}");

    // The other project still has its whole budget, and no held tasks.
    let digest = f.digest(&q).await;
    assert_eq!(digest["agent_task_weekly_budget"]["limit"], 5);
    assert_eq!(digest["agent_task_weekly_budget"]["admitted_this_week"], 0);
    assert!(digest["held_agent_tasks"].as_array().unwrap().is_empty());
    f.admitted(&q, 5).await;
    let digest = f.digest(&q).await;
    assert_eq!(digest["agent_task_weekly_budget"]["admitted_this_week"], 5);
    let sixth = f.create_task(&f.a, &q, "Sixth", json!({})).await;
    assert_eq!(sixth["lifecycle"], "planned", "{sixth}");
    assert_eq!(
        f.digest(&p).await["held_agent_tasks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn fixes_and_human_tasks_are_never_held_and_do_not_use_the_budget() {
    let f = Fixture::new().await;
    let p = f.project("admission-fixes").await;
    f.admitted(&p, 5).await;

    for class in ["revert", "fix_target", "deflake", "refusal_fix"] {
        let fix = f
            .create_task(
                &f.admin,
                &p,
                &format!("Fix {class}"),
                json!({"admission_class":class}),
            )
            .await;
        assert_eq!(fix["lifecycle"], "open", "{fix}");
        assert_eq!(fix["origin"], "human");
        assert_eq!(fix["admission_class"], class);
        assert_eq!(fix["admission"]["held"], false);
    }
    let human = f.create_task(&f.admin, &p, "From a human", json!({})).await;
    assert_eq!(human["lifecycle"], "open", "{human}");
    assert_eq!(human["origin"], "human");
    assert_eq!(human["admission"]["held"], false);
    assert!(human["admission"].get("reason").is_none());

    let (status, refused) = f
        .call(
            &f.admin,
            "POST",
            &format!("/api/v1/projects/{p}/tasks"),
            json!({"title":"Bad","acceptance_criteria":["done"],"admission_class":"urgent"}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");

    // Neither fixes nor human tasks used a place: the budget is still exactly spent.
    let held = f.create_task(&f.a, &p, "Plain", json!({})).await;
    assert_eq!(held["lifecycle"], "planned", "{held}");
    assert_eq!(held["admission"]["weekly_budget"]["admitted_this_week"], 5);
}

impl Fixture {
    /// Inserts, as the integrator would, a landed result with a failed check
    /// receipt, a `flaky` report and a `fix_target` report in `project`.
    /// Returns (result id, flaky report id, fix_target report id).
    async fn integration_records(&self, project: &str) -> (String, String, String) {
        let (result, flaky, target) = (
            Uuid::new_v4().to_string(),
            Uuid::new_v4().to_string(),
            Uuid::new_v4().to_string(),
        );
        let mut c = self.state.pool.acquire().await.unwrap();
        // The records stand alone: no submission or attempt is needed to test admission.
        sqlx::query("PRAGMA foreign_keys=OFF")
            .execute(&mut *c)
            .await
            .unwrap();
        let by = &self.integrator.principal;
        sqlx::query("INSERT INTO integrator_results(id,project_id,submission_id,t0,t0_tree,c,r,r_tree,landing_range_json,roster_json,created_by,created_at) VALUES(?,?,'s','t0','t0t','c','r','rt','[]','[]',?,0)")
            .bind(&result).bind(project).bind(by).execute(&mut *c).await.unwrap();
        sqlx::query("INSERT INTO integrator_observations(id,result_id,tip,ancestry,disposition,evidence,observed_by,observed_at) VALUES(?,?,'r','contained','published','x',?,0)")
            .bind(Uuid::new_v4().to_string()).bind(&result).bind(by).execute(&mut *c).await.unwrap();
        sqlx::query("INSERT INTO integrator_receipts(result_id,check_name,run_id,run_attempt,head_sha,app_id,workflow_path,workflow_blob,conclusion,observed_at) VALUES(?,'Coordination checks',1,1,'r',1,'w.yml','b','failure',0)")
            .bind(&result).execute(&mut *c).await.unwrap();
        for (id, kind) in [(&flaky, "flaky"), (&target, "fix_target")] {
            sqlx::query("INSERT INTO integrator_reports(id,project_id,kind,dedupe_key,details_json,requires_human,created_by,created_at) VALUES(?,?,?,?,'{}',0,?,0)")
                .bind(id).bind(project).bind(kind).bind(id).bind(by).execute(&mut *c).await.unwrap();
        }
        sqlx::query("PRAGMA foreign_keys=ON")
            .execute(&mut *c)
            .await
            .unwrap();
        (result, flaky, target)
    }

    async fn class_attempt(
        &self,
        caller: &Caller,
        project: &str,
        extra: Value,
    ) -> (StatusCode, Value) {
        let mut body = json!({"title":"Fix","acceptance_criteria":["done"],"kind":"general"});
        for (key, value) in extra.as_object().unwrap() {
            body[key] = value.clone();
        }
        self.call(
            caller,
            "POST",
            &format!("/api/v1/projects/{project}/tasks"),
            body,
        )
        .await
    }
}

#[tokio::test]
async fn an_agent_cannot_exempt_its_own_task_with_an_unbacked_class() {
    let f = Fixture::new().await;
    let (p, other) = (
        f.project("admission-evidence").await,
        f.project("admission-evidence-other").await,
    );
    let (result, flaky, target) = f.integration_records(&p).await;
    f.admitted(&p, 5).await;

    // Without evidence every class is refused and nothing is created.
    for class in ["revert", "fix_target", "deflake", "refusal_fix"] {
        let (status, refused) = f
            .class_attempt(&f.a, &p, json!({"admission_class":class}))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{class}: {refused}");
        assert_eq!(refused["error"]["code"], "operation_not_permitted");
        assert!(
            refused["error"]["message"]
                .as_str()
                .unwrap()
                .contains("admission_evidence")
        );
    }
    // Evidence that does not match the class, the project or any record is refused too.
    let wrong = [
        ("revert", json!({"result_id":"missing"})),
        ("revert", json!({"report_id":flaky})),
        ("revert", json!({"result_id":result,"report_id":flaky})),
        (
            "fix_target",
            json!({"result_id":result,"check_name":"Other check"}),
        ),
        ("fix_target", json!({"report_id":flaky})),
        ("deflake", json!({"report_id":target})),
        ("deflake", json!({"result_id":result})),
        ("refusal_fix", json!({"result_id":result})),
        ("refusal_fix", json!({"report_id":flaky})),
    ];
    for (class, evidence) in wrong {
        let (status, refused) = f
            .class_attempt(
                &f.a,
                &p,
                json!({"admission_class":class,"admission_evidence":evidence}),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{class} {evidence}: {refused}"
        );
    }
    let (status, refused) = f
        .class_attempt(
            &f.a,
            &other,
            json!({"admission_class":"revert","admission_evidence":{"result_id":result}}),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "another project's record: {refused}"
    );
    let (status, refused) = f
        .class_attempt(
            &f.a,
            &p,
            json!({"admission_class":"deflake","admission_evidence":{"nope":1}}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");

    // Evidence the service can verify admits the fix without the budget.
    let backed = [
        ("revert", json!({"result_id":result})),
        ("fix_target", json!({"result_id":result})),
        (
            "fix_target",
            json!({"result_id":result,"check_name":"Coordination checks"}),
        ),
        ("fix_target", json!({"report_id":target})),
        ("deflake", json!({"report_id":flaky})),
    ];
    for (class, evidence) in backed {
        let fix = f
            .create_task(
                &f.a,
                &p,
                &format!("Backed {class}"),
                json!({"admission_class":class,"admission_evidence":evidence}),
            )
            .await;
        assert_eq!(fix["lifecycle"], "open", "{fix}");
        assert_eq!(fix["origin"], "agent");
        assert_eq!(fix["admission_class"], class);
        assert_eq!(fix["admission"]["held"], false);
    }

    // The refused attempts created nothing, and the budget is still exactly spent.
    let held = f.create_task(&f.a, &p, "Plain", json!({})).await;
    assert_eq!(held["lifecycle"], "planned", "{held}");
    assert_eq!(held["admission"]["weekly_budget"]["admitted_this_week"], 5);
    let tasks: i64 = sqlx::query_scalar("SELECT count(*) FROM tasks WHERE project_id=?")
        .bind(&p)
        .fetch_one(&f.state.pool)
        .await
        .unwrap();
    assert_eq!(tasks, 5 + 5 + 1);
}

#[tokio::test]
async fn a_landed_revision_needs_an_observation_and_a_failed_check_needs_a_failure() {
    let f = Fixture::new().await;
    let p = f.project("admission-unlanded").await;
    let (result, _, _) = f.integration_records(&p).await;
    sqlx::query("UPDATE integrator_observations SET disposition='not_published' WHERE result_id=?")
        .bind(&result)
        .execute(&f.state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE integrator_receipts SET conclusion='success' WHERE result_id=?")
        .bind(&result)
        .execute(&f.state.pool)
        .await
        .unwrap();
    for class in ["revert", "fix_target"] {
        let (status, refused) = f
            .class_attempt(
                &f.a,
                &p,
                json!({"admission_class":class,"admission_evidence":{"result_id":result}}),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{class}: {refused}");
    }
}

#[tokio::test]
async fn the_operator_can_designate_an_agent_for_the_fix_classes() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = AppState::open(Config {
        database_path: dir.path().join("designated.sqlite3"),
        public_origin: "http://127.0.0.1:8080".into(),
        allow_insecure_loopback: true,
        fix_class_principals: vec!["attention-a".into()],
        ..Config::default()
    })
    .await
    .unwrap();
    state.clock = Arc::new(TestClock(AtomicI64::new(1_800_000_000_000)));
    let (admin, a, b) = (
        seed(&state, true, "attention-admin").await,
        seed(&state, false, "attention-a").await,
        seed(&state, false, "attention-b").await,
    );
    let app = router(state);
    let (status, created) = call(
        app.clone(),
        &admin,
        "POST",
        "/api/v1/projects",
        &Uuid::new_v4().to_string(),
        json!({"name":"designated","repository_url":"https://example.test/d.git","target_branch":"main"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let path = format!(
        "/api/v1/projects/{}/tasks",
        created["data"]["id"].as_str().unwrap()
    );
    let body = json!({"title":"Fix","acceptance_criteria":["done"],"kind":"general","admission_class":"refusal_fix"});
    let key = Uuid::new_v4().to_string();
    let (status, fix) = call(app.clone(), &a, "POST", &path, &key, body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{fix}");
    assert_eq!(fix["data"]["admission_class"], "refusal_fix");
    let (status, refused) = call(app, &b, "POST", &path, &Uuid::new_v4().to_string(), body).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
}

#[tokio::test]
async fn canary_tasks_never_use_or_meet_the_weekly_budget() {
    let f = Fixture::new().await;
    let p = f.project("admission-canary").await;
    let canary = json!({"admission_class":"canary"});

    // A daily canary files seven tasks in a week; every one is admitted.
    for day in 0..7 {
        let task = f
            .create_task(&f.canary, &p, &format!("Canary {day}"), canary.clone())
            .await;
        assert_eq!(task["lifecycle"], "open", "{task}");
        assert_eq!(task["origin"], "agent");
        assert_eq!(task["admission_class"], "canary");
        assert_eq!(task["admission"]["held"], false, "{task}");
        assert_eq!(task["admission"]["admission_class"], "canary");
        assert_eq!(task["admission"]["weekly_budget"]["admitted_this_week"], 0);
    }
    let digest = f.digest(&p).await;
    assert_eq!(digest["held_agent_tasks"], json!([]), "{digest}");

    // The budget is untouched: five ordinary agent tasks are still admitted and the sixth is held.
    f.admitted(&p, 5).await;
    let held = f.create_task(&f.a, &p, "Sixth", json!({})).await;
    assert_eq!(held["lifecycle"], "planned", "{held}");
    assert_eq!(held["admission"]["weekly_budget"]["admitted_this_week"], 5);

    // Canary tasks stay admitted with the budget spent, and read back as canary tasks.
    let later = f
        .create_task(&f.canary, &p, "Canary after the budget", canary)
        .await;
    assert_eq!(later["lifecycle"], "open", "{later}");
    let fetched = f
        .ok(
            &f.canary,
            "GET",
            &format!(
                "/api/v1/projects/{p}/tasks/{}",
                later["id"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
    assert_eq!(fetched["admission_class"], "canary", "{fetched}");
    assert_eq!(fetched["held_by_budget"], false);

    // The canary principal's ordinary tasks are budgeted like any agent's.
    let plain = f
        .create_task(&f.canary, &p, "Not a canary", json!({}))
        .await;
    assert_eq!(plain["lifecycle"], "planned", "{plain}");
}

#[tokio::test]
async fn only_a_designated_canary_may_use_the_canary_class() {
    let f = Fixture::new().await;
    let p = f.project("admission-canary-refused").await;
    for caller in [&f.a, &f.integrator, &f.admin] {
        let (status, refused) = f
            .call(
                caller,
                "POST",
                &format!("/api/v1/projects/{p}/tasks"),
                json!({"title":"Sneaky","acceptance_criteria":["done"],"admission_class":"canary"}),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    }
    // The refusals created nothing and used no place.
    f.admitted(&p, 5).await;
    let held = f.create_task(&f.a, &p, "Sixth", json!({})).await;
    assert_eq!(held["lifecycle"], "planned", "{held}");
    let listed = f
        .ok(
            &f.admin,
            "GET",
            &format!("/api/v1/projects/{p}/tasks"),
            Value::Null,
        )
        .await;
    assert!(
        !listed.to_string().contains("Sneaky"),
        "a refused canary task was created: {listed}"
    );
}

#[tokio::test]
async fn the_budget_starts_over_with_the_next_iso_week() {
    let f = Fixture::new().await;
    let p = f.project("admission-week").await;
    f.admitted(&p, 5).await;
    let held = f.create_task(&f.a, &p, "Held", json!({})).await;
    assert_eq!(held["lifecycle"], "planned");

    let now = f.clock.0.load(Ordering::SeqCst);
    let next_week = coordinator_server::admission::week_start(now) + WEEK;
    assert_eq!(next_week % WEEK, 4 * DAY, "weeks start on Monday");
    f.clock.0.store(next_week - 1, Ordering::SeqCst);
    let last_moment = f.create_task(&f.a, &p, "Still this week", json!({})).await;
    assert_eq!(last_moment["lifecycle"], "planned", "{last_moment}");

    f.clock.0.store(next_week, Ordering::SeqCst);
    let fresh = f.create_task(&f.a, &p, "New week", json!({})).await;
    assert_eq!(fresh["lifecycle"], "open", "{fresh}");
    assert_eq!(fresh["admission"]["weekly_budget"]["admitted_this_week"], 1);
    f.admitted(&p, 4).await;
    let again = f.create_task(&f.a, &p, "New week, sixth", json!({})).await;
    assert_eq!(again["lifecycle"], "planned", "{again}");
}

#[tokio::test]
async fn only_a_human_releases_a_task_the_budget_held() {
    let f = Fixture::new().await;
    let p = f.project("admission-release").await;
    f.admitted(&p, 5).await;
    let held = f.create_task(&f.a, &p, "Held", json!({})).await;
    f.ok(
        &f.admin,
        "POST",
        &format!("/api/v1/projects/{p}/task-definition-grants"),
        json!({"target_kind":"principal","agent_principal_id":f.a.principal}),
    )
    .await;
    let path = format!(
        "/api/v1/projects/{p}/tasks/{}",
        held["id"].as_str().unwrap()
    );
    let edit = json!({"expected_revision":held["revision"],"title":"Held","description":"admission",
        "acceptance_criteria":["done"],"priority":2,"depends_on":[],"planned":false});

    let (status, refused) = f.call(&f.a, "PATCH", &path, edit.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(
        refused["error"]["details"]["gate"], "admission_budget",
        "{refused}"
    );

    let released = f.ok(&f.admin, "PATCH", &path, edit).await;
    assert_eq!(released["lifecycle"], "open", "{released}");
    assert_eq!(released["held_by_budget"], false);
    assert_eq!(f.digest(&p).await["held_agent_tasks"], json!([]));
}

async fn seed(state: &AppState, human: bool, name: &str) -> Caller {
    let caller = Caller {
        token: secret(),
        session: Uuid::new_v4().to_string(),
        proof: secret(),
        principal: Uuid::new_v4().to_string(),
        _credential: Uuid::new_v4().to_string(),
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
        .bind(&caller._credential)
        .bind(&caller.principal)
        .bind(digest(&caller.token))
        .bind(state.now())
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO agent_sessions(id,principal_id,credential_id,workstation_id,proof_hash,created_at,capabilities,harness) VALUES(?,?,?,?,?,?,'[]','test')")
            .bind(&caller.session).bind(&caller.principal).bind(&caller._credential)
            .bind(format!("{}-workstation",caller.principal)).bind(digest(&caller.proof))
            .bind(state.now()).execute(&state.pool).await.unwrap();
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

#[tokio::test]
async fn canary_tasks_are_neither_stalls_nor_progress_in_the_digest() {
    let f = Fixture::new().await;
    let p = f.project("digest-canary").await;
    let canary = json!({"admission_class":"canary","priority":0});
    f.create_task(&f.canary, &p, "Canary", canary).await;

    // A canary task that keeps ending without a submission is not a stalled
    // task, the way an ordinary one is after three attempts.
    for round in 0..3 {
        let claimant = if round % 2 == 0 { &f.a } else { &f.b };
        let attempt = f.claim_offered(claimant, &p).await;
        f.release(claimant, &p, &attempt).await;
    }
    let hri = f.digest(&p).await["hri"].clone();
    assert_eq!(
        (hri["count"].clone(), hri["stalled_tasks"].clone()),
        (json!(0), json!(0)),
        "{hri}"
    );

    // Ready canary work alone is not a stalled queue.
    f.clock.0.fetch_add(8 * HOUR, Ordering::SeqCst);
    let hri = f.digest(&p).await["hri"].clone();
    assert_eq!(hri["count"], 0, "{hri}");

    // Real work waiting is, and the canary neither counts as ready work nor
    // keeps the clock from running by being claimed.
    f.task(&p, "Real work").await;
    f.clock.0.fetch_add(7 * HOUR, Ordering::SeqCst);
    let hri = f.digest(&p).await["hri"].clone();
    assert_eq!(hri["count"], 1, "{hri}");
    assert_eq!(hri["items"][0]["rule"], "no_progress");
    assert_eq!(hri["items"][0]["ready_tasks"], 1, "{hri}");
    let offered = f.next(&f.a, &p).await;
    assert_eq!(
        offered["action"]["title"], "Canary",
        "the canary is offered first: {offered}"
    );
    f.claim_offered(&f.a, &p).await;
    let hri = f.digest(&p).await["hri"].clone();
    assert_eq!(
        hri["count"], 1,
        "claiming the canary is not progress: {hri}"
    );
}

#[tokio::test]
async fn a_human_report_about_a_canary_task_is_not_counted_in_the_digest() {
    let f = Fixture::new().await;
    let p = f.project("digest-canary-report").await;
    let canary = f
        .create_task(&f.canary, &p, "Canary", json!({"admission_class":"canary"}))
        .await;
    let real = f.task(&p, "Real work").await;
    for (n, task) in [&canary, &real].into_iter().enumerate() {
        sqlx::query(
            "INSERT INTO integrator_reports(id,project_id,kind,task_id,dedupe_key,details_json,\
             requires_human,created_by,created_at) VALUES(?,?,'flaky',?,?,'{}',1,?,?)",
        )
        .bind(format!("report-{n}"))
        .bind(&p)
        .bind(task["id"].as_str().unwrap())
        .bind(format!("key-{n}"))
        .bind(&f.integrator.principal)
        .bind(f.clock.0.load(Ordering::SeqCst))
        .execute(&f.state.pool)
        .await
        .unwrap();
    }
    let hri = f.digest(&p).await["hri"].clone();
    assert_eq!(hri["count"], 1, "{hri}");
    assert_eq!(hri["items"][0]["report"]["task_id"], real["id"], "{hri}");
}
