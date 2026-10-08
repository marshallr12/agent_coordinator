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
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::open(Config {
            database_path: dir.path().join("attention.sqlite3"),
            public_origin: "http://127.0.0.1:8080".into(),
            allow_insecure_loopback: true,
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
async fn the_sixth_agent_task_in_a_week_lands_planned_across_all_projects() {
    let f = Fixture::new().await;
    let (p, q) = (
        f.project("admission-p").await,
        f.project("admission-q").await,
    );
    f.admitted(&p, 3).await;
    f.admitted(&q, 2).await;

    let held = f.create_task(&f.a, &p, "Sixth", json!({})).await;
    assert_eq!(held["lifecycle"], "planned", "{held}");
    assert_eq!(held["origin"], "agent");
    assert_eq!(held["admission"]["held"], true);
    assert_eq!(held["admission"]["weekly_budget"]["limit"], 5);
    assert_eq!(held["admission"]["weekly_budget"]["admitted_this_week"], 5);
    let reason = held["admission"]["reason"].as_str().unwrap();
    assert!(reason.contains("held as planned"), "{reason}");
    let also_held = f.create_task(&f.a, &q, "Seventh", json!({})).await;
    assert_eq!(also_held["lifecycle"], "planned", "{also_held}");

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
async fn fixes_and_human_tasks_are_never_held_and_do_not_use_the_budget() {
    let f = Fixture::new().await;
    let p = f.project("admission-fixes").await;
    f.admitted(&p, 5).await;

    for class in ["revert", "fix_target", "deflake", "refusal_fix"] {
        let fix = f
            .create_task(
                &f.a,
                &p,
                &format!("Fix {class}"),
                json!({"admission_class":class}),
            )
            .await;
        assert_eq!(fix["lifecycle"], "open", "{fix}");
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
            &f.a,
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
