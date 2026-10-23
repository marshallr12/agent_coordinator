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

/// 2027-01-15T00:00:00Z.
const DAY_1: i64 = 1_800_000_000_000;
const DAY_MS: i64 = 86_400_000;
const HOUR_MS: i64 = 3_600_000;

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
    clock: Arc<TestClock>,
    app: Router,
    _dir: tempfile::TempDir,
    admin: Caller,
    /// A supervisor host's credential: no session, as `agentc-supervisor` calls.
    host: Caller,
    other: Caller,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::open(Config {
            database_path: dir.path().join("telemetry.sqlite3"),
            public_origin: "http://127.0.0.1:8080".into(),
            allow_insecure_loopback: true,
            ..Config::default()
        })
        .await
        .unwrap();
        let clock = Arc::new(TestClock(AtomicI64::new(DAY_1)));
        state.clock = clock.clone();
        let admin = seed(&state, true, "telemetry-admin").await;
        let host = seed(&state, false, "telemetry-host").await;
        let other = seed(&state, false, "telemetry-other").await;
        Self {
            app: router(state.clone()),
            state,
            clock,
            _dir: dir,
            admin,
            host,
            other,
        }
    }

    async fn call(
        &self,
        caller: &Caller,
        method: &str,
        path: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let key = Uuid::new_v4().to_string();
        call(self.app.clone(), caller, method, path, &key, body).await
    }

    async fn keyed(
        &self,
        caller: &Caller,
        path: &str,
        key: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        call(self.app.clone(), caller, "POST", path, key, body).await
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

    async fn task(&self, project: &str, title: &str) -> String {
        let (status, value) = self
            .call(
                &self.admin,
                "POST",
                &format!("/api/v1/projects/{project}/tasks"),
                json!({"title":title,"description":"telemetry test",
                    "acceptance_criteria":["required behavior verified"],"kind":"general"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        value["data"]["id"].as_str().unwrap().into()
    }

    async fn get(&self, caller: &Caller, path: &str) -> (StatusCode, Value) {
        self.call(caller, "GET", path, json!({})).await
    }

    /// Reports `rows` of `host` and returns the reply's data.
    async fn report_costs(&self, project: &str, host: &str, rows: Vec<Value>) -> Value {
        let path = format!("/api/v1/projects/{project}/telemetry/cost-rows");
        let (status, value) = self
            .call(&self.host, "POST", &path, json!({"host":host,"rows":rows}))
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        value["data"].clone()
    }

    async fn report_canaries(&self, project: &str, host: &str, rows: Vec<Value>) -> Value {
        let path = format!("/api/v1/projects/{project}/telemetry/canary-results");
        let (status, value) = self
            .call(&self.host, "POST", &path, json!({"host":host,"rows":rows}))
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        value["data"].clone()
    }
}

/// A cost row `id` of `role` for `task`, settled `ms` after [`DAY_1`].
fn cost(id: &str, role: &str, task: Option<&str>, usd: Option<f64>, ms: i64) -> Value {
    json!({"row_id":id,"role":role,"harness":"claude","model":"claude-opus-5-5",
        "input_tokens":10,"cached_input_tokens":100,"output_tokens":5,"usd":usd,
        "task_id":task,"session_id":"s1","attempt_id":"a1","occurred_at_ms":DAY_1 + ms})
}

/// A canary result `id` of `outcome`, ended `ms` after [`DAY_1`].
fn canary(id: &str, harness: &str, outcome: &str, ms: i64) -> Value {
    json!({"row_id":id,"harness":harness,"outcome":outcome,"duration_seconds":61.5,
        "detail":"done","task_id":"t-canary","served_by":[harness],"occurred_at_ms":DAY_1 + ms})
}

fn costs_path(project: &str) -> String {
    format!("/api/v1/projects/{project}/telemetry/cost-rows")
}

#[tokio::test]
async fn a_cost_report_is_stored_once_whatever_the_retry() {
    let f = Fixture::new().await;
    let p = f.project("telemetry-once").await;
    let rows = vec![
        cost("r1", "impl", Some("t1"), Some(1.25), 0),
        cost("r2", "rev", Some("t1"), None, 1000),
    ];
    let body = json!({"host":"host-a","rows":rows});
    let (status, first) = f
        .keyed(&f.host, &costs_path(&p), "key-1", body.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["data"]["accepted"], 2);
    assert_eq!(first["data"]["duplicates"], 0);

    // The same key replays the saved reply.
    let (status, replay) = f
        .keyed(&f.host, &costs_path(&p), "key-1", body.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay["data"], first["data"]);

    // A new key, or a batch of other composition, finds its rows stored.
    let (_, again) = f.keyed(&f.host, &costs_path(&p), "key-2", body).await;
    assert_eq!(
        (&again["data"]["accepted"], &again["data"]["duplicates"]),
        (&json!(0), &json!(2))
    );
    let mixed = vec![
        cost("r2", "rev", Some("t1"), None, 1000),
        cost("r3", "impl", Some("t2"), Some(2.0), 2000),
    ];
    let data = f.report_costs(&p, "host-a", mixed).await;
    assert_eq!(
        (&data["accepted"], &data["duplicates"]),
        (&json!(1), &json!(1))
    );

    // Another host's row of the same id is its own row.
    let data = f
        .report_costs(
            &p,
            "host-b",
            vec![cost("r1", "impl", Some("t1"), Some(1.0), 0)],
        )
        .await;
    assert_eq!(data["accepted"], 1);
    let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM cost_rows WHERE project_id=?")
        .bind(&p)
        .fetch_one(&f.state.pool)
        .await
        .unwrap();
    assert_eq!(stored, 4);
}

#[tokio::test]
async fn a_reused_key_with_other_rows_is_a_conflict() {
    let f = Fixture::new().await;
    let p = f.project("telemetry-conflict").await;
    let one = json!({"host":"h","rows":[cost("r1", "impl", None, Some(1.0), 0)]});
    let other = json!({"host":"h","rows":[cost("r2", "impl", None, Some(1.0), 0)]});
    let (status, _) = f.keyed(&f.host, &costs_path(&p), "same-key", one).await;
    assert_eq!(status, StatusCode::OK);
    let (status, value) = f.keyed(&f.host, &costs_path(&p), "same-key", other).await;
    assert_eq!(status, StatusCode::CONFLICT, "{value}");
    assert_eq!(value["error"]["code"], "idempotency_conflict");
}

#[tokio::test]
async fn rows_record_the_reporting_principal_host_and_one_event() {
    let f = Fixture::new().await;
    let p = f.project("telemetry-attribution").await;
    f.report_costs(
        &p,
        "host-a",
        vec![cost("r1", "impl", Some("t1"), Some(1.0), 0)],
    )
    .await;
    f.report_canaries(&p, "host-a", vec![canary("c1", "claude", "ok", 0)])
        .await;
    let (reporter, host): (String, String) =
        sqlx::query_as("SELECT reported_by,host FROM cost_rows WHERE row_id='r1'")
            .fetch_one(&f.state.pool)
            .await
            .unwrap();
    assert_eq!(
        (reporter, host),
        (f.host.principal.clone(), "host-a".into())
    );
    let (status, canaries) = f
        .get(
            &f.other,
            &format!("/api/v1/projects/{p}/telemetry/canaries"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{canaries}");
    assert_eq!(
        canaries["data"]["items"][0]["reported_by"],
        json!(f.host.principal)
    );
    assert_eq!(canaries["data"]["items"][0]["host"], "host-a");
    for kind in ["telemetry.cost_reported", "telemetry.canary_reported"] {
        let (project, data): (String, String) =
            sqlx::query_as("SELECT project_id,data_json FROM events WHERE kind=?")
                .bind(kind)
                .fetch_one(&f.state.pool)
                .await
                .unwrap();
        assert_eq!(project, p);
        assert!(
            data.contains("host-a") && data.contains("\"accepted\":1"),
            "{data}"
        );
    }
}

#[tokio::test]
async fn only_a_write_agent_credential_reports_and_anyone_signed_in_reads() {
    let f = Fixture::new().await;
    let p = f.project("telemetry-auth").await;
    let body = json!({"host":"h","rows":[cost("r1", "impl", None, Some(1.0), 0)]});
    let canary_body = json!({"host":"h","rows":[canary("c1", "claude", "ok", 0)]});
    let canary_path = format!("/api/v1/projects/{p}/telemetry/canary-results");

    // Without a credential nothing is read or written.
    for (method, path) in [
        ("POST", costs_path(&p)),
        ("POST", canary_path.clone()),
        ("GET", format!("/api/v1/projects/{p}/telemetry/cost")),
        ("GET", format!("/api/v1/projects/{p}/telemetry/canaries")),
    ] {
        let request = Request::builder()
            .method(method)
            .uri(&path)
            .header("content-type", "application/json")
            .header("idempotency-key", "anonymous")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = f.app.clone().oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {path}"
        );
    }

    // A human browser session is not a reporting host.
    let (status, value) = f
        .keyed(&f.admin, &costs_path(&p), "human", body.clone())
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{value}");
    let (status, _) = f
        .keyed(&f.admin, &canary_path, "human", canary_body.clone())
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // A read-only credential (a shadow or reviewer principal) reads but never writes.
    sqlx::query("UPDATE credentials SET access='read' WHERE id=?")
        .bind(&f.other.credential)
        .execute(&f.state.pool)
        .await
        .unwrap();
    let (status, value) = f
        .keyed(&f.other, &costs_path(&p), "read-only", body.clone())
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{value}");
    let (status, _) = f
        .keyed(&f.other, &canary_path, "read-only", canary_body.clone())
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = f
        .get(&f.other, &format!("/api/v1/projects/{p}/telemetry/cost"))
        .await;
    assert_eq!(status, StatusCode::OK);

    // The integrator credential may only call the integrator API.
    sqlx::query("UPDATE credentials SET class='integrator',access='write' WHERE id=?")
        .bind(&f.other.credential)
        .execute(&f.state.pool)
        .await
        .unwrap();
    let (status, _) = f
        .keyed(&f.other, &costs_path(&p), "integrator", body.clone())
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // A supervised credential reports.
    sqlx::query("UPDATE credentials SET class='supervised' WHERE id=?")
        .bind(&f.host.credential)
        .execute(&f.state.pool)
        .await
        .unwrap();
    let (status, value) = f.keyed(&f.host, &costs_path(&p), "supervised", body).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM cost_rows")
        .fetch_one(&f.state.pool)
        .await
        .unwrap();
    assert_eq!(stored, 1);

    // An unknown project is unknown to reporters and readers alike.
    let missing = Uuid::new_v4();
    // The 404 names the missing project, so a host can tell it from a
    // coordinator without telemetry routes (`record_not_found`).
    let (status, body) = f
        .keyed(
            &f.host,
            &costs_path(&missing.to_string()),
            "missing",
            json!({"host":"h","rows":[cost("r9", "impl", None, None, 0)]}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "project_not_found", "{body}");
    assert_eq!(body["error"]["details"]["project_id"], missing.to_string());
    let (status, body) = f
        .get(
            &f.host,
            &format!("/api/v1/projects/{missing}/telemetry/canaries"),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "project_not_found", "{body}");
}

#[tokio::test]
async fn invalid_reports_are_refused_before_anything_is_stored() {
    let f = Fixture::new().await;
    let p = f.project("telemetry-invalid").await;
    let good = || cost("r1", "impl", None, Some(1.0), 0);
    let with = |field: &str, value: Value| {
        let mut row = good();
        row[field] = value;
        row
    };
    let rows_that_fail = vec![
        with("role", json!("planner")),
        with("usd", json!(-1.0)),
        with("usd", json!(1e12)),
        with("row_id", json!("")),
        with("row_id", json!("has space")),
        with("harness", json!("")),
        with("input_tokens", json!(-1)),
        with("input_tokens", json!(u64::MAX)),
        with("occurred_at_ms", json!(-5)),
        with("task_id", json!("x".repeat(101))),
        with("unknown_field", json!(1)),
    ];
    for row in rows_that_fail {
        let (status, value) = f
            .keyed(
                &f.host,
                &costs_path(&p),
                &Uuid::new_v4().to_string(),
                json!({"host":"h","rows":[row.clone()]}),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{row} {value}");
    }
    for body in [
        json!({"host":"h","rows":[]}),
        json!({"host":"","rows":[good()]}),
        json!({"rows":[good()]}),
        json!({"host":"h","rows":(0..201).map(|n| cost(&format!("r{n}"), "impl", None, None, 0)).collect::<Vec<_>>()}),
    ] {
        let (status, value) = f
            .keyed(&f.host, &costs_path(&p), &Uuid::new_v4().to_string(), body)
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
    }
    let canary_path = format!("/api/v1/projects/{p}/telemetry/canary-results");
    for (field, value) in [
        ("outcome", json!("passed")),
        ("duration_seconds", json!(-1)),
        ("harness", json!("")),
    ] {
        let mut row = canary("c1", "claude", "ok", 0);
        row[field] = value;
        let (status, v) = f
            .keyed(
                &f.host,
                &canary_path,
                &Uuid::new_v4().to_string(),
                json!({"host":"h","rows":[row]}),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    }
    let stored: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM cost_rows)+(SELECT count(*) FROM canary_results)",
    )
    .fetch_one(&f.state.pool)
    .await
    .unwrap();
    assert_eq!(stored, 0);
}

#[tokio::test]
async fn cost_reads_group_per_task_and_per_day_by_role() {
    let f = Fixture::new().await;
    let p = f.project("telemetry-cost").await;
    let t1 = f.task(&p, "First task").await;
    let t2 = f.task(&p, "Second task").await;
    let rows = vec![
        cost("a", "impl", Some(&t1), Some(1.5), 0),
        cost("b", "impl", Some(&t1), Some(0.25), 60_000),
        cost("c", "rev", Some(&t1), Some(0.5), 120_000),
        cost("d", "impl", Some(&t2), None, DAY_MS + 1000),
        cost("e", "rev", Some(&t2), Some(0.75), DAY_MS + 2000),
        cost("f", "impl", None, Some(3.0), DAY_MS + 3000),
    ];
    let mut estimated = cost("g", "impl", Some(&t2), Some(2.0), DAY_MS + 4000);
    estimated["usd_estimated"] = json!(true);
    f.report_costs(&p, "host-a", [rows, vec![estimated]].concat())
        .await;
    let url = format!("/api/v1/projects/{p}/telemetry/cost");

    let (status, by_task) = f.get(&f.other, &format!("{url}?group_by=task")).await;
    assert_eq!(status, StatusCode::OK, "{by_task}");
    let data = &by_task["data"];
    assert_eq!(data["group_by"], "task");
    let items = data["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    // Most recent group first: the second task, the task-less launch, the first.
    assert_eq!(items[0]["task_id"], json!(t2));
    assert_eq!(items[0]["title"], "Second task");
    assert_eq!(items[1]["task_id"], Value::Null);
    assert_eq!(items[1]["impl"]["usd"], 3.0);
    let second = &items[0];
    assert_eq!(second["impl"]["launches"], 2);
    assert_eq!(second["impl"]["usd"], 2.0);
    assert_eq!(second["impl"]["unpriced_launches"], 1);
    assert_eq!(second["impl"]["estimated_launches"], 1);
    assert_eq!(second["rev"]["usd"], 0.75);
    assert_eq!(second["total_usd"], 2.75);
    let first = &items[2];
    assert_eq!(first["task_id"], json!(t1));
    assert_eq!(first["impl"]["usd"], 1.75);
    assert_eq!(first["impl"]["input_tokens"], 20);
    assert_eq!(first["impl"]["cached_input_tokens"], 200);
    assert_eq!(first["impl"]["output_tokens"], 10);
    assert_eq!(first["rev"]["launches"], 1);
    assert_eq!(first["total_usd"], 2.25);
    assert_eq!(data["totals"]["impl"]["launches"], 5);
    assert_eq!(data["totals"]["rev"]["usd"], 1.25);
    assert_eq!(data["totals"]["total_usd"], 8.0);
    assert_eq!(data["truncated"], false);

    let (_, by_day) = f.get(&f.other, &format!("{url}?group_by=day")).await;
    let days = by_day["data"]["items"].as_array().unwrap();
    assert_eq!(days.len(), 2, "{by_day}");
    assert_eq!(days[0]["day"], "2027-01-16");
    assert_eq!(days[0]["total_usd"], 5.75);
    assert_eq!(days[1]["day"], "2027-01-15");
    assert_eq!(days[1]["impl"]["usd"], 1.75);
    assert_eq!(days[1]["rev"]["usd"], 0.5);
    assert_eq!(by_day["data"]["totals"]["total_usd"], 8.0);

    // Window, host and size filters.
    let since = DAY_1 + DAY_MS;
    let (_, windowed) = f
        .get(&f.other, &format!("{url}?group_by=day&since_ms={since}"))
        .await;
    assert_eq!(windowed["data"]["items"].as_array().unwrap().len(), 1);
    let (_, none) = f.get(&f.other, &format!("{url}?host=host-b")).await;
    assert_eq!(none["data"]["items"], json!([]));
    let (_, small) = f.get(&f.other, &format!("{url}?limit=1")).await;
    assert_eq!(small["data"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(small["data"]["truncated"], true);
    assert_eq!(small["data"]["totals"]["total_usd"], 8.0);
    for bad in ["group_by=week", "limit=0", "limit=1001"] {
        let (status, _) = f.get(&f.other, &format!("{url}?{bad}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
}

#[tokio::test]
async fn canary_cost_is_left_out_unless_asked_for() {
    let f = Fixture::new().await;
    let p = f.project("telemetry-canary-cost").await;
    let probe = f.task(&p, "Canary probe").await;
    let real = f.task(&p, "Real work").await;
    sqlx::query("UPDATE tasks SET budget_exempt='canary' WHERE id=?")
        .bind(&probe)
        .execute(&f.state.pool)
        .await
        .unwrap();
    let mut flagged = cost("c", "impl", Some("x"), Some(4.0), 0);
    flagged["canary"] = json!(true);
    let rows = vec![
        cost("a", "impl", Some(&probe), Some(1.0), 0),
        cost("b", "impl", Some(&real), Some(2.0), 0),
        flagged,
    ];
    f.report_costs(&p, "h", rows).await;
    let url = format!("/api/v1/projects/{p}/telemetry/cost");
    let (_, left_out) = f.get(&f.other, &url).await;
    assert_eq!(left_out["data"]["totals"]["total_usd"], 2.0, "{left_out}");
    let (_, all) = f.get(&f.other, &format!("{url}?include_canary=true")).await;
    assert_eq!(all["data"]["totals"]["total_usd"], 7.0, "{all}");
}

#[tokio::test]
async fn telemetry_is_scoped_to_its_project() {
    let f = Fixture::new().await;
    let (a, b) = (
        f.project("telemetry-a").await,
        f.project("telemetry-b").await,
    );
    f.report_costs(&a, "h", vec![cost("r1", "impl", Some("t1"), Some(9.0), 0)])
        .await;
    f.report_canaries(&a, "h", vec![canary("c1", "claude", "ok", 0)])
        .await;
    // The same row id in the other project is a different row.
    let data = f
        .report_costs(&b, "h", vec![cost("r1", "impl", Some("t1"), Some(1.0), 0)])
        .await;
    assert_eq!(data["accepted"], 1);
    let (_, costs) = f
        .get(&f.other, &format!("/api/v1/projects/{b}/telemetry/cost"))
        .await;
    assert_eq!(costs["data"]["totals"]["total_usd"], 1.0);
    let (_, canaries) = f
        .get(
            &f.other,
            &format!("/api/v1/projects/{b}/telemetry/canaries"),
        )
        .await;
    assert_eq!(canaries["data"]["items"], json!([]));
    assert_eq!(canaries["data"]["latest"], json!([]));
    let (status, _) = f
        .get(
            &f.other,
            &format!(
                "/api/v1/projects/{b}/telemetry/canaries?before={}",
                Uuid::new_v4()
            ),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn canary_history_pages_newest_first_and_names_the_latest_per_host_and_harness() {
    let f = Fixture::new().await;
    let p = f.project("telemetry-canaries").await;
    let rows = vec![
        canary("c1", "claude", "ok", 1000),
        canary("c2", "claude", "failed", 3000),
        canary("c3", "codex", "ok", 2000),
    ];
    let data = f.report_canaries(&p, "host-a", rows.clone()).await;
    assert_eq!(
        (&data["accepted"], &data["duplicates"]),
        (&json!(3), &json!(0))
    );
    // A backfill of rows older than the ones already stored, and a re-import.
    let data = f
        .report_canaries(
            &p,
            "host-a",
            [rows, vec![canary("c0", "claude", "timeout", 0)]].concat(),
        )
        .await;
    assert_eq!(
        (&data["accepted"], &data["duplicates"]),
        (&json!(1), &json!(3))
    );
    f.report_canaries(&p, "host-b", vec![canary("c1", "claude", "ok", 500)])
        .await;

    let url = format!("/api/v1/projects/{p}/telemetry/canaries");
    let (status, all) = f.get(&f.other, &url).await;
    assert_eq!(status, StatusCode::OK, "{all}");
    let outcomes: Vec<_> = all["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["host"].as_str().unwrap(),
                i["harness"].as_str().unwrap(),
                i["outcome"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        outcomes,
        [
            ("host-a", "claude", "failed"),
            ("host-a", "codex", "ok"),
            ("host-a", "claude", "ok"),
            ("host-b", "claude", "ok"),
            ("host-a", "claude", "timeout"),
        ]
    );
    assert_eq!(all["data"]["items"][0]["served_by"], json!(["claude"]));
    assert_eq!(all["data"]["items"][0]["duration_seconds"], 61.5);
    assert_eq!(all["data"]["next_before"], Value::Null);
    let latest: Vec<_> = all["data"]["latest"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["host"].as_str().unwrap(),
                i["harness"].as_str().unwrap(),
                i["outcome"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        latest,
        [
            ("host-a", "claude", "failed"),
            ("host-a", "codex", "ok"),
            ("host-b", "claude", "ok"),
        ]
    );

    let (_, page_one) = f.get(&f.other, &format!("{url}?limit=2")).await;
    assert_eq!(page_one["data"]["items"].as_array().unwrap().len(), 2);
    let cursor = page_one["data"]["next_before"].as_str().unwrap();
    let (_, page_two) = f
        .get(&f.other, &format!("{url}?limit=2&before={cursor}"))
        .await;
    assert_eq!(page_two["data"]["items"][0]["outcome"], "ok");
    assert_eq!(page_two["data"]["items"][0]["host"], "host-a");
    assert_eq!(page_two["data"]["items"].as_array().unwrap().len(), 2);

    let (_, failed) = f.get(&f.other, &format!("{url}?outcome=failed")).await;
    assert_eq!(failed["data"]["items"].as_array().unwrap().len(), 1);
    let (_, host_b) = f
        .get(&f.other, &format!("{url}?host=host-b&harness=claude"))
        .await;
    assert_eq!(host_b["data"]["items"].as_array().unwrap().len(), 1);
    let since = DAY_1 + 2000;
    let (_, recent) = f.get(&f.other, &format!("{url}?since_ms={since}")).await;
    assert_eq!(recent["data"]["items"].as_array().unwrap().len(), 2);
    let (status, _) = f.get(&f.other, &format!("{url}?limit=0")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// Seeds the workflow, review, report and revise records the autonomy read
/// counts, on one connection with foreign keys off so a test names only the
/// columns that matter.
struct Records {
    conn: sqlx::pool::PoolConnection<sqlx::Sqlite>,
    project: String,
    agent: String,
}

impl Records {
    async fn new(f: &Fixture, project: &str) -> Self {
        let mut conn = f.state.pool.acquire().await.unwrap();
        sqlx::query("PRAGMA foreign_keys=OFF")
            .execute(&mut *conn)
            .await
            .unwrap();
        Self {
            conn,
            project: project.into(),
            agent: f.host.principal.clone(),
        }
    }

    /// A submission of `task`, created `ms` after [`DAY_1`]; a task has one
    /// current submission, so an earlier one is `superseded`.
    async fn submission(&mut self, id: &str, task: &str, ms: i64, superseded: bool) {
        sqlx::query("INSERT INTO submissions(id,project_id,task_id,attempt_id,kind,task_revision,project_policy_revision,workflow_policy_revision,summary,acceptance_evidence_json,handoff,created_by,contributor_session_id,created_at,superseded_at) VALUES(?,?,?,?,'general',1,1,0,'s','[]','h',?,'s',?,?)")
            .bind(id).bind(&self.project).bind(task).bind(format!("attempt-{id}"))
            .bind(&self.agent).bind(DAY_1 + ms).bind(superseded.then_some(DAY_1 + ms + 1))
            .execute(&mut *self.conn).await.unwrap();
    }

    /// A review of `submission` (of `subject`) by `activity_task`, deciding `decision`.
    async fn review(
        &mut self,
        submission: &str,
        subject: &str,
        activity_task: &str,
        decision: &str,
    ) {
        let activity = format!("activity-{activity_task}");
        sqlx::query("INSERT INTO workflow_activities(id,project_id,subject_task_id,submission_id,activity_task_id,kind,slot,state,created_at) VALUES(?,?,?,?,?,'agent_review',1,'completed',?)")
            .bind(&activity).bind(&self.project).bind(subject).bind(submission).bind(activity_task).bind(DAY_1)
            .execute(&mut *self.conn).await.unwrap();
        sqlx::query("INSERT INTO review_decisions(activity_id,submission_id,attempt_id,reviewer_id,reviewer_session_id,decision,summary,created_at) VALUES(?,?,?,?,?,?,'r',?)")
            .bind(&activity).bind(submission).bind(format!("review-attempt-{activity_task}"))
            .bind(&self.agent).bind("rs").bind(decision).bind(DAY_1)
            .execute(&mut *self.conn).await.unwrap();
    }

    /// `task`'s workflow reached done `ms` after [`DAY_1`].
    async fn finished(&mut self, task: &str, submission: &str, ms: i64) {
        sqlx::query("INSERT INTO workflow_subjects(project_id,task_id,current_submission_id,phase,updated_at) VALUES(?,?,?,'done',?)")
            .bind(&self.project).bind(task).bind(submission).bind(DAY_1 + ms)
            .execute(&mut *self.conn).await.unwrap();
    }

    /// An agent reopened `submission` `ms` after [`DAY_1`].
    async fn reopened(&mut self, submission: &str, ms: i64) {
        sqlx::query("INSERT INTO events(project_id,actor_id,kind,record_id,data_json,created_at) VALUES(?,?,'submission.reopened',?,'{}',?)")
            .bind(&self.project).bind(&self.agent).bind(submission).bind(DAY_1 + ms)
            .execute(&mut *self.conn).await.unwrap();
    }

    /// A report the integrator opened `ms` after [`DAY_1`].
    async fn report(&mut self, id: &str, task: Option<&str>, human: bool, ms: i64) {
        sqlx::query("INSERT INTO integrator_reports(id,project_id,kind,task_id,dedupe_key,details_json,requires_human,created_by,created_at) VALUES(?,?,'privilege_gate',?,?,'{}',?,?,?)")
            .bind(id).bind(&self.project).bind(task).bind(id).bind(i64::from(human))
            .bind(&self.agent).bind(DAY_1 + ms)
            .execute(&mut *self.conn).await.unwrap();
    }
}

#[tokio::test]
async fn an_empty_project_reads_zeros_and_empty_lists() {
    let f = Fixture::new().await;
    let p = f.project("telemetry-autonomy-empty").await;
    let url = format!("/api/v1/projects/{p}/telemetry/autonomy");
    let (status, value) = f.get(&f.other, &url).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    let data = &value["data"];
    assert_eq!(data["window_days"], 14);
    assert_eq!(data["finished"], json!({"count": 0, "by_day": []}));
    assert_eq!(data["human_required"]["count"], 0);
    assert_eq!(data["human_required"]["per_finished_task"], Value::Null);
    assert_eq!(data["parked"], json!({"count": 0, "items": []}));
    assert_eq!(data["reviews"]["submissions_reviewed"], 0);
    assert_eq!(data["caps"], json!([]));
    for bad in ["days=0", "days=91"] {
        let (status, _) = f.get(&f.other, &format!("{url}?{bad}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
    let (status, _) = f
        .get(
            &f.other,
            "/api/v1/projects/no-such-project/telemetry/autonomy",
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let anonymous = Request::builder().uri(&url).body(Body::empty()).unwrap();
    let response = f.app.clone().oneshot(anonymous).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn autonomy_counts_finished_tasks_interventions_reviews_and_parked_items() {
    let f = Fixture::new().await;
    let p = f.project("telemetry-autonomy").await;
    let (done_a, done_b, stuck) = (
        f.task(&p, "Done A").await,
        f.task(&p, "Done B").await,
        f.task(&p, "Stuck").await,
    );
    let probe = f.task(&p, "Canary probe").await;
    sqlx::query("UPDATE tasks SET budget_exempt='canary' WHERE id=?")
        .bind(&probe)
        .execute(&f.state.pool)
        .await
        .unwrap();
    let activities: Vec<String> = {
        let mut ids = Vec::new();
        for n in 0..6 {
            ids.push(f.task(&p, &format!("Review {n}")).await);
        }
        ids
    };
    let mut r = Records::new(&f, &p).await;
    // Done A: approved on its first submission. Done B: changes requested, then
    // approved by the next submission. The canary probe finishes but is not counted.
    r.submission("sub-a", &done_a, 0, false).await;
    r.review("sub-a", &done_a, &activities[0], "approved").await;
    r.finished(&done_a, "sub-a", 1000).await;
    r.submission("sub-b1", &done_b, 10, true).await;
    r.review("sub-b1", &done_b, &activities[1], "changes_requested")
        .await;
    r.submission("sub-b2", &done_b, 20, false).await;
    r.review("sub-b2", &done_b, &activities[2], "approved")
        .await;
    r.finished(&done_b, "sub-b2", DAY_MS + 1000).await;
    r.submission("sub-p", &probe, 30, false).await;
    r.review("sub-p", &probe, &activities[3], "changes_requested")
        .await;
    r.finished(&probe, "sub-p", 2000).await;
    // Two reviewers disagree about the stuck task's submission, and agents
    // reopened it three times, which parks it.
    r.submission("sub-s1", &stuck, 40, true).await;
    r.review("sub-s1", &stuck, &activities[4], "approved").await;
    sqlx::query("INSERT INTO workflow_activities(id,project_id,subject_task_id,submission_id,activity_task_id,kind,slot,state,created_at) VALUES('activity-second',?,?,?,?,'human_review',2,'completed',?)")
        .bind(&p).bind(&stuck).bind("sub-s1").bind(&activities[5]).bind(DAY_1)
        .execute(&mut *r.conn).await.unwrap();
    sqlx::query("INSERT INTO review_decisions(activity_id,submission_id,attempt_id,reviewer_id,reviewer_session_id,decision,summary,created_at) VALUES('activity-second','sub-s1','a2',?,'rs','changes_requested','r',?)")
        .bind(&r.agent).bind(DAY_1)
        .execute(&mut *r.conn).await.unwrap();
    r.submission("sub-s2", &stuck, 50, true).await;
    r.submission("sub-s3", &stuck, 60, false).await;
    for (n, submission) in ["sub-s1", "sub-s2", "sub-s3"].iter().enumerate() {
        r.reopened(submission, 100 + n as i64).await;
    }
    // Human-required reports: two about real tasks, one about the probe (left
    // out), one that needs no person (left out).
    r.report("rep-1", Some(&done_a), true, 5000).await;
    r.report("rep-2", None, true, DAY_MS + 5000).await;
    r.report("rep-3", Some(&probe), true, 5000).await;
    r.report("rep-4", Some(&done_b), false, 5000).await;
    drop(r);

    let (status, value) = f
        .get(
            &f.other,
            &format!("/api/v1/projects/{p}/telemetry/autonomy"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    let data = &value["data"];
    assert_eq!(
        data["finished"],
        json!({"count": 2, "by_day": [{"day":"2027-01-15","count":1},{"day":"2027-01-16","count":1}]})
    );
    let hri = &data["human_required"];
    assert_eq!(hri["integrator_reports"], 2);
    assert_eq!(hri["count"], 2);
    assert_eq!(hri["per_finished_task"], 1.0);
    assert_eq!(
        hri["by_day"],
        json!([{"day":"2027-01-15","count":1},{"day":"2027-01-16","count":1}])
    );
    // Four reviewed submissions (the probe's is left out): A approved first
    // time; B's first needed changes; B's second was approved but is not a
    // first submission; the stuck one's reviewers disagreed.
    let reviews = &data["reviews"];
    assert_eq!(reviews["submissions_reviewed"], 4, "{reviews}");
    assert_eq!(reviews["approved_first_pass"], 1);
    assert_eq!(reviews["revised"], 2);
    assert_eq!(reviews["disagreements"], 1);
    assert_eq!(reviews["decisions"], 5);
    let parked = data["parked"]["items"].as_array().unwrap();
    assert_eq!(parked.len(), 1, "{data}");
    assert_eq!(parked[0]["task_id"], json!(stuck));
    assert_eq!(parked[0]["title"], "Stuck");
    assert_eq!(parked[0]["agent_revises_24h"], 3);
    assert_eq!(parked[0]["counted_revises_24h"], 3);

    // A narrower window drops what happened before it.
    f.clock.0.store(DAY_1 + 2 * DAY_MS, Ordering::SeqCst);
    let (_, narrow) = f
        .get(
            &f.other,
            &format!("/api/v1/projects/{p}/telemetry/autonomy?days=1"),
        )
        .await;
    // Only Done B (a day later) is inside the last day; the reopens are older than 24 hours.
    assert_eq!(narrow["data"]["finished"]["count"], 1);
    assert_eq!(narrow["data"]["human_required"]["count"], 1);
    assert_eq!(narrow["data"]["parked"]["count"], 0);
}

#[tokio::test]
async fn host_caps_are_reported_replaced_and_read_back() {
    let f = Fixture::new().await;
    let p = f.project("telemetry-caps").await;
    let path = format!("/api/v1/projects/{p}/telemetry/caps");
    let (status, value) = f
        .call(
            &f.host,
            "POST",
            &path,
            json!({"host":"h1","impl_daily_usd":150.0,"rev_daily_usd":50.0}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    let (status, _) = f
        .call(
            &f.host,
            "POST",
            &path,
            json!({"host":"h2","impl_daily_usd":0.0,"rev_daily_usd":null}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    // The newest report of a host replaces its earlier one.
    let (status, _) = f
        .call(
            &f.host,
            "POST",
            &path,
            json!({"host":"h1","impl_daily_usd":120.5,"rev_daily_usd":40.0}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, read) = f
        .get(
            &f.other,
            &format!("/api/v1/projects/{p}/telemetry/autonomy"),
        )
        .await;
    let caps = read["data"]["caps"].as_array().unwrap();
    assert_eq!(caps.len(), 2, "{read}");
    assert_eq!(
        (caps[0]["host"].as_str(), caps[0]["impl_daily_usd"].as_f64()),
        (Some("h1"), Some(120.5))
    );
    assert_eq!(caps[0]["rev_daily_usd"], 40.0);
    assert_eq!(caps[1]["impl_daily_usd"], 0.0);
    assert_eq!(caps[1]["rev_daily_usd"], Value::Null);

    // The same key replays; invalid, human, read-only and unknown-project calls are refused.
    let body = json!({"host":"h3","impl_daily_usd":1.0,"rev_daily_usd":1.0});
    let first = f.keyed(&f.host, &path, "caps-key", body.clone()).await;
    let replay = f.keyed(&f.host, &path, "caps-key", body.clone()).await;
    assert_eq!(first.0, replay.0);
    assert_eq!(first.1["data"], replay.1["data"]);
    for bad in [
        json!({"host":"h","impl_daily_usd":-1.0}),
        json!({"host":"","impl_daily_usd":1.0}),
        json!({"host":"h","impl_daily_usd":1.0,"extra":1}),
    ] {
        let (status, _) = f.call(&f.host, "POST", &path, bad.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
    let (status, _) = f.call(&f.admin, "POST", &path, body.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    sqlx::query("UPDATE credentials SET access='read' WHERE id=?")
        .bind(&f.other.credential)
        .execute(&f.state.pool)
        .await
        .unwrap();
    let (status, _) = f.call(&f.other, "POST", &path, body.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = f
        .call(
            &f.host,
            "POST",
            "/api/v1/projects/nope/telemetry/caps",
            body,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
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
            .bind(&caller.session).bind(&caller.principal).bind(&caller.credential).bind(name)
            .bind(digest(&caller.proof)).bind(state.now()).execute(&state.pool).await.unwrap();
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

/// Records the exact-window read counts, on top of those [`Records`] seeds.
impl Records {
    /// An audit event `ms` after [`DAY_1`] by `actor` through a credential of `class`.
    async fn event(&mut self, kind: &str, record: &str, actor: &str, class: Option<&str>, ms: i64) {
        sqlx::query("INSERT INTO events(project_id,actor_id,kind,record_id,data_json,created_at,credential_class) VALUES(?,?,?,?,'{}',?,?)")
            .bind(&self.project).bind(actor).bind(kind).bind(record).bind(DAY_1 + ms).bind(class)
            .execute(&mut *self.conn).await.unwrap();
    }

    /// An attempt of `task` that began and ended `ms` after [`DAY_1`] in `state`.
    async fn attempt(&mut self, task: &str, generation: i64, state: &str, ms: i64, ended: bool) {
        sqlx::query("INSERT INTO attempts(id,project_id,task_id,owner_id,session_id,generation,state,mode,expires_at,last_heartbeat_at,last_progress_at,created_at,ended_at) VALUES(?,?,?,?,'s',?,?,'work',?,?,?,?,?)")
            .bind(format!("{task}-{generation}")).bind(&self.project).bind(task).bind(&self.agent)
            .bind(generation).bind(state).bind(DAY_1 + ms).bind(DAY_1 + ms).bind(DAY_1 + ms)
            .bind(DAY_1 + ms - 10).bind(ended.then_some(DAY_1 + ms))
            .execute(&mut *self.conn).await.unwrap();
    }

    /// An integrator result for `submission` with one observation `ms` after [`DAY_1`].
    async fn observation(&mut self, submission: &str, disposition: &str, ms: i64) {
        let result = format!("result-{submission}");
        sqlx::query("INSERT INTO integrator_results(id,project_id,submission_id,t0,t0_tree,c,r,r_tree,landing_range_json,roster_json,created_by,created_at) VALUES(?,?,?,'t0','t0t','c','r','rt','[]','[]',?,?)")
            .bind(&result).bind(&self.project).bind(submission).bind(&self.agent).bind(DAY_1 + ms - 5)
            .execute(&mut *self.conn).await.unwrap();
        sqlx::query("INSERT INTO integrator_observations(id,result_id,tip,ancestry,disposition,evidence,observed_by,observed_at) VALUES(?,?,'tip','contained',?,'e',?,?)")
            .bind(format!("obs-{submission}")).bind(&result).bind(disposition).bind(&self.agent).bind(DAY_1 + ms)
            .execute(&mut *self.conn).await.unwrap();
    }

    /// The integrator parked `submission` of `task` `ms` after [`DAY_1`].
    async fn park(&mut self, submission: &str, task: &str, ms: i64) {
        sqlx::query("INSERT INTO integrator_revises(submission_id,task_id,reason_code,park_reason,revised_at) VALUES(?,?,'conflict','too many revises',?)")
            .bind(submission).bind(task).bind(DAY_1 + ms)
            .execute(&mut *self.conn).await.unwrap();
    }
}

/// Sets the columns of `task` a window read looks at.
async fn shape(f: &Fixture, task: &str, kind: &str, origin: &str, created_ms: i64) {
    sqlx::query("UPDATE tasks SET kind=?,origin=?,created_at=? WHERE id=?")
        .bind(kind)
        .bind(origin)
        .bind(DAY_1 + created_ms)
        .bind(task)
        .execute(&f.state.pool)
        .await
        .unwrap();
}

fn window_url(project: &str, start: i64, end: i64) -> String {
    format!(
        "/api/v1/projects/{project}/telemetry/autonomy/window?start_ms={}&end_ms={}",
        DAY_1 + start,
        DAY_1 + end
    )
}

#[tokio::test]
async fn a_window_needs_both_bounds_and_must_be_closed_and_bounded() {
    let f = Fixture::new().await;
    let p = f.project("telemetry-window-bounds").await;
    f.clock.0.store(DAY_1 + 200 * DAY_MS, Ordering::SeqCst);
    let base = format!("/api/v1/projects/{p}/telemetry/autonomy/window");
    for (query, code) in [
        ("", "invalid_request"),
        (&format!("?start_ms={DAY_1}"), "invalid_request"),
        (&format!("?end_ms={DAY_1}"), "invalid_request"),
        (
            &format!("?start_ms={}&end_ms={DAY_1}", DAY_1 + 1),
            "invalid_request",
        ),
        ("?start_ms=-1&end_ms=5", "invalid_request"),
        (
            &format!("?start_ms={DAY_1}&end_ms={}", DAY_1 + 93 * DAY_MS),
            "invalid_request",
        ),
        (
            &format!(
                "?start_ms={}&end_ms={}",
                DAY_1 + 190 * DAY_MS,
                DAY_1 + 201 * DAY_MS
            ),
            "window_not_closed",
        ),
    ] {
        let (status, value) = f.get(&f.other, &format!("{base}{query}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}: {value}");
        assert_eq!(value["error"]["code"], code, "{query}: {value}");
    }
    // The bounds are inclusive: a window of one instant, and one of exactly 92 days, are fine.
    for (start, end) in [(DAY_1, DAY_1), (DAY_1, DAY_1 + 92 * DAY_MS)] {
        let (status, value) = f
            .get(&f.other, &format!("{base}?start_ms={start}&end_ms={end}"))
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert_eq!(value["data"]["window"]["bounds"], "both inclusive");
        assert_eq!(value["data"]["window"]["start_ms"], start);
        assert_eq!(value["data"]["window"]["end_ms"], end);
    }
    let (status, _) = f
        .get(
            &f.other,
            "/api/v1/projects/no-such-project/telemetry/autonomy/window?start_ms=0&end_ms=1",
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let anonymous = Request::builder().uri(&base).body(Body::empty()).unwrap();
    let response = f.app.clone().oneshot(anonymous).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_window_includes_both_bounds_and_does_not_depend_on_the_query_time() {
    let f = Fixture::new().await;
    let p = f.project("telemetry-window-fixture").await;
    let tasks = [
        f.task(&p, "Just before").await,
        f.task(&p, "At start").await,
        f.task(&p, "At end").await,
        f.task(&p, "Just after").await,
    ];
    let mut r = Records::new(&f, &p).await;
    for (n, (task, at)) in tasks.iter().zip([999, 1000, 5000, 5001]).enumerate() {
        shape(
            &f,
            task,
            "general",
            if n == 1 { "agent" } else { "human" },
            0,
        )
        .await;
        let submission = format!("sub-{n}");
        r.submission(&submission, task, 10, false).await;
        r.finished(task, &submission, at).await;
    }
    r.event("task.edited", &tasks[0], &f.admin.principal, None, 999)
        .await;
    r.event("task.edited", &tasks[1], &f.admin.principal, None, 1000)
        .await;
    r.event("task.edited", &tasks[2], &f.admin.principal, None, 5000)
        .await;
    r.event("task.edited", &tasks[3], &f.admin.principal, None, 5001)
        .await;
    drop(r);
    let url = window_url(&p, 1000, 5000);

    f.clock.0.store(DAY_1 + HOUR_MS, Ordering::SeqCst);
    let (status, first) = f.get(&f.other, &url).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let data = &first["data"];
    assert_eq!(data["value"]["completed"]["count"], 2, "{data}");
    assert_eq!(data["value"]["completed"]["by_origin"]["agent"], 1);
    assert_eq!(data["value"]["completed"]["by_origin"]["human"], 1);
    assert_eq!(data["value"]["human_origin"]["count"], 1);
    assert_eq!(data["attention_events"]["by_class"]["human_session"], 2);
    assert_eq!(data["attention_events"]["interventions"]["events"], 2);

    // Asking again days later, after unrelated activity, reproduces every windowed section.
    f.clock.0.store(DAY_1 + 3 * HOUR_MS, Ordering::SeqCst);
    let late = f.task(&p, "Created after the window").await;
    let (_, second) = f.get(&f.other, &url).await;
    let (mut a, mut b) = (first["data"].clone(), second["data"].clone());
    assert_ne!(a["current_state"]["as_of"], b["current_state"]["as_of"]);
    a.as_object_mut().unwrap().remove("current_state");
    b.as_object_mut().unwrap().remove("current_state");
    assert_eq!(a, b);
    assert!(!late.is_empty());
}

#[tokio::test]
async fn interactive_events_are_kept_apart_from_supervised_and_integrator_ones() {
    let f = Fixture::new().await;
    let p = f.project("telemetry-window-classes").await;
    let task = f.task(&p, "Recovered by a person").await;
    let probe = f.task(&p, "Canary probe").await;
    sqlx::query("UPDATE tasks SET budget_exempt='canary' WHERE id=?")
        .bind(&probe)
        .execute(&f.state.pool)
        .await
        .unwrap();
    let mut r = Records::new(&f, &p).await;
    let agent = r.agent.clone();
    r.event("attempt.claimed", &task, &agent, Some("interactive"), 2000)
        .await;
    r.event("task.edited", &task, &agent, Some("interactive"), 2100)
        .await;
    r.event("attempt.claimed", &task, &agent, Some("supervised"), 2200)
        .await;
    r.event(
        "integrator.result_recorded",
        &task,
        &agent,
        Some("integrator"),
        2300,
    )
    .await;
    r.event("task.canceled", &task, &f.admin.principal, None, 2400)
        .await;
    r.event("job.observed", "job-1", &agent, None, 2500).await;
    // A canary task's events are counted apart, under the reserved exclusions.
    r.event("attempt.claimed", &probe, &agent, Some("interactive"), 2600)
        .await;
    // Reports and parks the integrator raised: one needs a person, one about the probe does not count.
    r.report("rep-1", Some(&task), true, 3000).await;
    r.report("rep-2", Some(&probe), true, 3000).await;
    r.submission("sub-park", &task, 10, false).await;
    r.park("sub-park", &task, 3500).await;
    drop(r);

    f.clock.0.store(DAY_1 + DAY_MS, Ordering::SeqCst);
    let (status, value) = f.get(&f.other, &window_url(&p, 1000, 9000)).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    let data = &value["data"];
    let events = &data["attention_events"];
    assert_eq!(events["by_class"]["interactive"], 2, "{events}");
    assert_eq!(events["by_class"]["supervised"], 1);
    assert_eq!(events["by_class"]["integrator"], 1);
    assert_eq!(events["by_class"]["human_session"], 1);
    assert_eq!(events["by_class"]["job_reporter"], 1);
    assert_eq!(events["by_class"]["unclassified"], 0);
    assert_eq!(events["interventions"]["events"], 3);
    assert_eq!(events["classification"]["status"], "complete");
    let kinds = events["by_kind"].as_array().unwrap();
    assert!(
        kinds.iter().any(|k| k["kind"] == "attempt.claimed"
            && k["class"] == "supervised"
            && k["events"] == 1)
    );
    let reserved = &data["reserved_exclusions"]["canary_tasks"];
    assert_eq!(reserved["attention_events"], 1);
    assert_eq!(reserved["human_required_reports"], 1);
    assert_eq!(data["integrator_signals"]["human_required_reports"], 1);
    assert_eq!(data["episodes"]["parks"]["count"], 1);
    assert_eq!(
        data["episodes"]["parks"]["items"][0]["reason_code"],
        "conflict"
    );
    // Every source was available, so the total is stated: 3 events + 1 report + 1 park.
    let hri = &data["hri"];
    assert_eq!(hri["status"], "complete", "{hri}");
    assert_eq!(hri["count"], 5);
    assert_eq!(hri["outcome"], "recorded");
    assert_eq!(hri["components"]["interactive_and_human_events"], 3);
}

#[tokio::test]
async fn missing_evidence_withholds_the_total_instead_of_reporting_zero() {
    let f = Fixture::new().await;
    let p = f.project("telemetry-window-unknown").await;
    let task = f.task(&p, "Quiet task").await;
    f.clock.0.store(DAY_1 + DAY_MS, Ordering::SeqCst);

    // A quiet window with full evidence is a recorded zero.
    let (_, quiet) = f.get(&f.other, &window_url(&p, 1000, 9000)).await;
    assert_eq!(quiet["data"]["hri"]["status"], "complete");
    assert_eq!(quiet["data"]["hri"]["count"], 0);
    assert_eq!(quiet["data"]["hri"]["outcome"], "none_recorded");

    // An agent event with no recorded class cannot be split, so the total is unknown.
    let mut r = Records::new(&f, &p).await;
    let agent = r.agent.clone();
    r.event("task.edited", &task, &agent, None, 2000).await;
    drop(r);
    let (_, partial) = f.get(&f.other, &window_url(&p, 1000, 9000)).await;
    let hri = &partial["data"]["hri"];
    assert_eq!(hri["status"], "partial", "{hri}");
    assert_eq!(hri["count"], Value::Null);
    assert_eq!(hri["outcome"], "unknown");
    assert_eq!(hri["lower_bound"], 0);
    assert_eq!(hri["unavailable_sources"][0]["source"], "credential_class");
    assert_eq!(
        partial["data"]["attention_events"]["classification"]["status"],
        "partial"
    );

    // A window that begins before the project's first audit event is also unknown.
    let (_, early) = f.get(&f.other, &window_url(&p, -500, 100)).await;
    // (-500 is before DAY_1 but not negative as an epoch time.)
    let sources: Vec<_> = early["data"]["hri"]["unavailable_sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["source"].as_str().unwrap().to_owned())
        .collect();
    assert!(sources.contains(&"audit_events".to_owned()), "{early}");
    assert_eq!(early["data"]["hri"]["count"], Value::Null);
    assert_eq!(
        early["data"]["attention_events"]["audit_log"]["covers_window_start"],
        false
    );
}

#[tokio::test]
async fn stall_episodes_are_reconstructed_from_attempt_history() {
    let f = Fixture::new().await;
    let p = f.project("telemetry-window-stalls").await;
    let (stalled, recovered, old, fine) = (
        f.task(&p, "Stalls and stays stalled").await,
        f.task(&p, "Stalls then submits").await,
        f.task(&p, "Stalled before the window").await,
        f.task(&p, "Two failures only").await,
    );
    let probe = f.task(&p, "Canary probe").await;
    sqlx::query("UPDATE tasks SET budget_exempt='canary' WHERE id=?")
        .bind(&probe)
        .execute(&f.state.pool)
        .await
        .unwrap();
    let mut r = Records::new(&f, &p).await;
    for (n, ms) in [2000, 2100, 2200, 2300].into_iter().enumerate() {
        r.attempt(&stalled, n as i64 + 1, "expired", ms, true).await;
    }
    for (n, ms) in [3000, 3100, 3200].into_iter().enumerate() {
        r.attempt(&recovered, n as i64 + 1, "released", ms, true)
            .await;
    }
    r.attempt(&recovered, 4, "submitted", 3500, true).await;
    for (n, ms) in [10, 20, 30].into_iter().enumerate() {
        r.attempt(&old, n as i64 + 1, "blocked", ms, true).await;
    }
    r.attempt(&fine, 1, "released", 4000, true).await;
    r.attempt(&fine, 2, "released", 4100, true).await;
    r.attempt(&fine, 3, "active", 4200, false).await;
    for (n, ms) in [5000, 5100, 5200].into_iter().enumerate() {
        r.attempt(&probe, n as i64 + 1, "expired", ms, true).await;
    }
    drop(r);

    f.clock.0.store(DAY_1 + DAY_MS, Ordering::SeqCst);
    let (status, value) = f.get(&f.other, &window_url(&p, 1000, 9000)).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    let stalls = &value["data"]["episodes"]["stalls"];
    // One episode per task however many nightly readings saw it.
    assert_eq!(stalls["started_in_window"], 2, "{stalls}");
    let items = stalls["items"].as_array().unwrap();
    let find = |task: &str| items.iter().find(|i| i["task_id"] == task).unwrap();
    assert_eq!(find(&stalled)["failed_attempts"], 4);
    assert_eq!(find(&stalled)["resolved_at"], Value::Null);
    assert_eq!(find(&recovered)["failed_attempts"], 3);
    assert_ne!(find(&recovered)["resolved_at"], Value::Null);
    assert_eq!(
        stalls["carried_in"], 1,
        "the pre-window stall has no submit"
    );
    assert_eq!(
        value["data"]["reserved_exclusions"]["canary_tasks"]["stall_episodes"],
        1
    );
    assert_eq!(
        value["data"]["hri"]["components"]["stall_episodes_started"],
        2
    );

    // The same window ending before the third failure of `stalled` does not see it,
    // whatever attempts were recorded afterwards.
    let (_, early) = f.get(&f.other, &window_url(&p, 1000, 2150)).await;
    assert_eq!(early["data"]["episodes"]["stalls"]["started_in_window"], 0);
    // The digest's current view still lists the task, as current state.
    let now = &value["data"]["current_state"];
    assert_eq!(now["depends_on_query_time"], true);
}

#[tokio::test]
async fn value_and_direct_push_coverage_state_their_denominators() {
    let f = Fixture::new().await;
    let p = f.project("telemetry-window-push").await;
    let names = ["pushed", "agent", "contained", "bare", "general"];
    let mut tasks = Vec::new();
    for name in names {
        tasks.push(f.task(&p, name).await);
    }
    let mut r = Records::new(&f, &p).await;
    for (n, task) in tasks.iter().enumerate() {
        let submission = format!("sub-{n}");
        shape(
            &f,
            task,
            if n == 4 { "general" } else { "code" },
            if n == 0 { "human" } else { "agent" },
            1000,
        )
        .await;
        r.submission(&submission, task, 10, false).await;
        r.finished(task, &submission, 4000 + n as i64 * 100).await;
    }
    r.observation("sub-0", "published", 3900).await;
    r.observation("sub-2", "already_contained", 4150).await;
    sqlx::query("INSERT INTO workflow_activities(id,project_id,subject_task_id,submission_id,activity_task_id,kind,slot,state,created_at) VALUES('act-int',?,?,?,?,'integration',1,'completed',?)")
        .bind(&p).bind(&tasks[1]).bind("sub-1").bind(&tasks[4]).bind(DAY_1)
        .execute(&mut *r.conn).await.unwrap();
    sqlx::query("INSERT INTO integration_results(activity_id,submission_id,attempt_id,publication_state,observed_target_revision,result_revision,result_tree,check_job_ids_json,summary,reported_by,created_at) VALUES('act-int','sub-1','att','published','a','b','c','[]','s',?,?)")
        .bind(&r.agent).bind(DAY_1 + 4050)
        .execute(&mut *r.conn).await.unwrap();
    drop(r);

    f.clock.0.store(DAY_1 + DAY_MS, Ordering::SeqCst);
    let (status, value) = f.get(&f.other, &window_url(&p, 3000, 6000)).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    let data = &value["data"];
    assert_eq!(data["value"]["completed"]["count"], 5);
    assert_eq!(data["value"]["completed"]["by_origin"]["human"], 1);
    assert_eq!(data["value"]["human_origin"]["count"], 1);
    // Human-origin latency: created 1000 ms and done 4000 ms after DAY_1.
    assert_eq!(
        data["value"]["latency"]["human_origin"]["median_seconds"],
        3.0
    );
    assert_eq!(data["value"]["latency"]["all"]["count"], 5);
    let push = &data["direct_push"];
    assert_eq!(push["denominator"]["completed_code_tasks"], 4, "{push}");
    assert_eq!(push["denominator"]["not_applicable_general_tasks"], 1);
    assert_eq!(push["integrator_direct_push"]["count"], 1);
    assert_eq!(push["integrator_direct_push"]["ratio"], 0.25);
    assert_eq!(push["agent_integration_published"]["count"], 1);
    assert_eq!(push["already_contained_only"]["count"], 1);
    assert_eq!(push["no_publication_evidence"]["count"], 1);
    assert_eq!(push["status"], "partial");
    assert_eq!(push["timestamps"]["push_time"], "unavailable");
    assert!(
        push["timestamps"]["commit_times"]
            .as_str()
            .unwrap()
            .contains("neither stored nor used")
    );

    // An observation recorded after the window's end is not evidence for it.
    let (_, narrow) = f.get(&f.other, &window_url(&p, 3000, 4020)).await;
    assert_eq!(
        narrow["data"]["direct_push"]["denominator"]["completed_code_tasks"],
        1
    );
    assert_eq!(
        narrow["data"]["direct_push"]["integrator_direct_push"]["count"],
        1
    );
    let (_, empty) = f.get(&f.other, &window_url(&p, 7000, 8000)).await;
    assert_eq!(
        empty["data"]["direct_push"]["integrator_direct_push"]["ratio"],
        Value::Null
    );
    assert_eq!(empty["data"]["direct_push"]["status"], "complete");
}
