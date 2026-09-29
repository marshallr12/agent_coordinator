//! End-to-end cycles against a real local bare remote, file-backed checks and
//! an in-process mock of the coordinator's integrator routes (same paths,
//! envelopes, field names, idempotency-key semantics, held-authority rule and
//! first-write-wins reports, receipt-backed `check_failed` revises and the
//! revert routes as the server; the server's own rules are covered by
//! `crates/server/tests/workflow.rs`).
use crate::checks::{FakeChecks, FakeFile};
use crate::config::{ChecksKind, Config};
use crate::git::testing::{Remote, commit, commit_message, git, remote};
use crate::integrate::{Integrator, Step};
use crate::service::Service;
use crate::state::LoopState;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Two required checks that are jobs of the same workflow run.
const ROSTER: &str = "[[required_checks]]\nidentity = \"tests\"\ncheck_name = \"Linux tests\"\nworkflow_path = \".github/workflows/ci.yml\"\n\n[[required_checks]]\nidentity = \"lint\"\ncheck_name = \"Lint\"\nworkflow_path = \".github/workflows/ci.yml\"\n";
const CANDIDATE_REF: &str = "refs/agent-coordinator/candidates/s1";
const RULES: [&str; 2] = ["non_fast_forward", "required_status_checks"];

type Reply = (StatusCode, Json<Value>);

/// What the mock service has been told, the one queued subject, the
/// targets and reverts it lists, and the refusals it is told to give.
#[derive(Default)]
struct Mock {
    item: Option<Value>,
    targets: Vec<Value>,
    /// Reverts awaiting a candidate (the queue's `reverts`).
    reverts: Vec<Value>,
    /// The `reverted` landings every served item carries.
    reverted: Vec<Value>,
    /// While set, a no-op result is refused `candidate_reverted_in_history`
    /// with these details.
    reverted_history: Option<Value>,
    /// While set, every revise is refused with this code.
    revise_refusal: Option<String>,
    /// While set, both revert routes refuse with this code.
    revert_refusal: Option<String>,
    /// While set, the reports route answers 500.
    reports_down: bool,
    results: Vec<Value>,
    candidates: Vec<Value>,
    not_mechanical: Vec<Value>,
    receipts: Vec<Value>,
    observations: Vec<Value>,
    revises: Vec<Value>,
    reports: Vec<Value>,
    keys: HashMap<String, (Value, Reply)>,
}

type Shared = Arc<Mutex<Mock>>;

/// A success envelope.
fn ok(data: Value) -> Reply {
    (
        StatusCode::OK,
        Json(json!({"data": data, "request_id": "r", "server_time": "t"})),
    )
}

/// A 409 refusal envelope.
fn refuse(code: &str) -> Reply {
    refuse_with(code, json!({}))
}

/// A 409 refusal envelope with `details`.
fn refuse_with(code: &str, details: Value) -> Reply {
    let error = json!({"code": code, "message": code, "details": details});
    (
        StatusCode::CONFLICT,
        Json(json!({"error": error, "request_id": "r", "server_time": "t"})),
    )
}

/// Replays or refuses a reused idempotency key, like `Mutation::begin`.
fn replay(mock: &Mock, headers: &HeaderMap, body: &Value) -> Option<Reply> {
    let key = headers.get("idempotency-key")?.to_str().ok()?;
    let (stored, reply) = mock.keys.get(key)?;
    Some(if stored == body {
        reply.clone()
    } else {
        refuse("idempotency_conflict")
    })
}

/// Records a key's body and reply for later replays.
fn remember(mock: &mut Mock, headers: &HeaderMap, body: Value, reply: Reply) -> Reply {
    if let Some(key) = headers.get("idempotency-key").and_then(|k| k.to_str().ok()) {
        mock.keys.insert(key.to_owned(), (body, reply.clone()));
    }
    reply
}

/// Wraps a POST handler body with idempotency replay and recording.
fn idempotent(
    mock: &Shared,
    headers: &HeaderMap,
    body: Value,
    handle: impl FnOnce(&mut Mock, &Value) -> Reply,
) -> Reply {
    let mut mock = mock.lock().unwrap();
    if let Some(reply) = replay(&mock, headers, &body) {
        return reply;
    }
    let reply = handle(&mut mock, &body);
    remember(&mut mock, headers, body, reply)
}

async fn queue(State(mock): State<Shared>) -> Reply {
    let mock = mock.lock().unwrap();
    let with_results = |item: &Value| {
        let mut item = item.clone();
        let own = |r: &&Value| r["submission_id"] == item["submission_id"];
        item["results"] = json!(mock.results.iter().filter(own).collect::<Vec<_>>());
        item["reverted"] = json!(mock.reverted);
        item
    };
    let items: Vec<Value> = mock.item.iter().map(with_results).collect();
    let roster = json!({"revision": 1, "required_checks": [{"identity": "tests"}]});
    ok(
        json!({"project_id": "p", "roster": roster, "targets": mock.targets, "items": items, "reverts": mock.reverts, "skipped_ineligible": 0, "retry_after_seconds": 30}),
    )
}

async fn results(State(mock): State<Shared>, headers: HeaderMap, Json(body): Json<Value>) -> Reply {
    idempotent(&mock, &headers, body, |mock, body| {
        let same =
            |r: &&Value| r["t0"] == body["t0"] && r["submission_id"] == body["submission_id"];
        if let Some(existing) = mock.results.iter().find(same) {
            return ok(existing.clone());
        }
        if let Some(details) = mock
            .reverted_history
            .clone()
            .filter(|_| body["r"] == body["t0"])
        {
            return refuse_with("candidate_reverted_in_history", details);
        }
        let mut record = body.clone();
        record["id"] = json!(format!("res{}", mock.results.len() + 1));
        record["authority_expires_at"] = Value::Null;
        mock.results.push(record.clone());
        ok(record)
    })
}

async fn receipts(
    State(mock): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Reply {
    idempotent(&mock, &headers, body, |mock, body| {
        let r = mock
            .results
            .iter()
            .find(|r| r["id"] == body["result_id"])
            .map(|r| r["r"].clone());
        if r.as_ref() != Some(&body["head_sha"]) {
            return refuse("receipt_head_mismatch");
        }
        mock.receipts.push(body.clone());
        ok(json!({"receipt": body}))
    })
}

async fn authority(
    State(mock): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Reply {
    idempotent(&mock, &headers, body, grant)
}

/// Grants authority when every check passed and no other result holds it.
fn grant(mock: &mut Mock, body: &Value) -> Reply {
    let id = &body["result_id"];
    if mock
        .results
        .iter()
        .any(|r| &r["id"] != id && !r["authority_expires_at"].is_null())
    {
        return refuse("observation_required");
    }
    let passed = mock
        .receipts
        .iter()
        .filter(|r| &r["result_id"] == id && r["conclusion"] == "success")
        .count();
    if passed < 2 {
        return refuse("checks_not_passed");
    }
    let expires = (chrono::Utc::now() + chrono::Duration::seconds(600)).to_rfc3339();
    let result = mock.results.iter_mut().find(|r| &r["id"] == id).unwrap();
    result["authority_expires_at"] = json!(expires);
    ok(
        json!({"granted": true, "result_id": id, "r": result["r"], "t0": result["t0"], "expires_at": expires}),
    )
}

async fn observations(
    State(mock): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Reply {
    idempotent(&mock, &headers, body, |mock, body| {
        let disposition = match body["ancestry"].as_str() {
            Some("contained") => "published",
            Some("equal_t0") => "not_published",
            _ => "target_moved",
        };
        if disposition == "published" {
            mock.item = None;
        }
        for result in mock
            .results
            .iter_mut()
            .filter(|r| r["id"] == body["result_id"])
        {
            result["authority_expires_at"] = Value::Null;
        }
        mock.observations.push(body.clone());
        ok(json!({"disposition": disposition, "revise": null}))
    })
}

/// True when the cited result has at least two failed receipts of one check.
fn reproduced(mock: &Mock, result: &Value) -> bool {
    let failed = mock
        .receipts
        .iter()
        .filter(|r| &r["result_id"] == result && r["conclusion"] != "success");
    let mut per_check: HashMap<String, usize> = HashMap::new();
    for receipt in failed {
        *per_check
            .entry(receipt["check_name"].to_string())
            .or_default() += 1;
    }
    per_check.values().any(|count| *count >= 2)
}

async fn revise(State(mock): State<Shared>, headers: HeaderMap, Json(body): Json<Value>) -> Reply {
    idempotent(&mock, &headers, body, |mock, body| {
        if body["reason_code"] == "check_failed" && !reproduced(mock, &body["result_id"]) {
            return refuse("check_failure_not_reproduced");
        }
        if let Some(code) = &mock.revise_refusal {
            return refuse(code);
        }
        mock.item = None;
        mock.revises.push(body.clone());
        ok(json!({"revise": body}))
    })
}

/// Stores the first report per (kind, dedupe_key) and returns the stored row;
/// answers 500 while `reports_down` is set.
async fn reports(State(mock): State<Shared>, headers: HeaderMap, Json(body): Json<Value>) -> Reply {
    if mock.lock().unwrap().reports_down {
        let error = json!({"error": {"code": "internal", "message": "down"}});
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(error));
    }
    idempotent(&mock, &headers, body, |mock, body| {
        let same = |r: &&Value| r["kind"] == body["kind"] && r["dedupe_key"] == body["dedupe_key"];
        if let Some(existing) = mock.reports.iter().find(same) {
            return ok(existing.clone());
        }
        let mut record = body.clone();
        record["id"] = json!(format!("rep{}", mock.reports.len() + 1));
        record["resolved_at"] = Value::Null;
        record["allowed"] = json!(false);
        mock.reports.push(record.clone());
        ok(record)
    })
}

/// Records revert `id`'s candidate and queues it as the revert's subject
/// (`rs-<id>`, marked with `revert_task_id`), like the server's candidate
/// route for a human's revert, which needs no review.
async fn revert_candidate(
    State(mock): State<Shared>,
    Path((_, id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Reply {
    idempotent(&mock, &headers, body, |mock, body| {
        if let Some(code) = &mock.revert_refusal {
            return refuse(code);
        }
        let revert = mock
            .reverts
            .iter()
            .find(|r| r["id"] == id.as_str())
            .cloned();
        let Some(revert) = revert else {
            return refuse("revert_candidate_exists");
        };
        mock.reverts.retain(|r| r["id"] != id.as_str());
        mock.candidates.push(body.clone());
        mock.item = Some(
            json!({"subject_task_id": id, "submission_id": format!("rs-{id}"),
            "title": revert["title"], "priority": 0, "candidate_revision": body["candidate_commit"],
            "candidate_tree": body["candidate_tree"], "candidate_ref": body["candidate_ref"],
            "reviewed_base": body["t0"], "repository_url": revert["repository_url"],
            "target_branch": revert["target_branch"], "task_digest": null,
            "revert_task_id": id, "results": []}),
        );
        ok(json!({"revert_task_id": id, "candidate_submission_id": format!("rs-{id}")}))
    })
}

/// Records a not-mechanical report and drops revert `id` from the queue,
/// both as a pending revert and as a queued candidate.
async fn not_mechanical(
    State(mock): State<Shared>,
    Path((_, id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Reply {
    idempotent(&mock, &headers, body, |mock, body| {
        if let Some(code) = &mock.revert_refusal {
            return refuse(code);
        }
        mock.reverts.retain(|r| r["id"] != id.as_str());
        let queued = mock
            .item
            .as_ref()
            .map(|i| i["revert_task_id"] == id.as_str());
        if queued == Some(true) {
            mock.item = None;
        }
        let mut record = body.clone();
        record["id"] = json!(id);
        mock.not_mechanical.push(record);
        ok(json!({"id": id, "revert": {"mode": "not_mechanical"}}))
    })
}

/// Serves the mock on a loopback port and returns its origin.
async fn serve(mock: Shared) -> String {
    let base = "/api/v1/projects/{p}/integrator";
    let app = Router::new()
        .route(&format!("{base}/queue"), get(queue))
        .route(&format!("{base}/results"), post(results))
        .route(&format!("{base}/receipts"), post(receipts))
        .route(&format!("{base}/push-authority"), post(authority))
        .route(&format!("{base}/observations"), post(observations))
        .route(&format!("{base}/revise"), post(revise))
        .route(&format!("{base}/reports"), post(reports))
        .route(
            &format!("{base}/reverts/{{id}}/candidate"),
            post(revert_candidate),
        )
        .route(
            &format!("{base}/reverts/{{id}}/not-mechanical"),
            post(not_mechanical),
        )
        .with_state(mock);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    origin
}

/// A remote whose `main` carries the roster and workflow, plus the base SHA.
fn seeded_remote() -> (Remote, String) {
    let remote = remote();
    commit(&remote.source, ".github/workflows/ci.yml", "on: push\n");
    let base = commit(&remote.source, ".agent-coordinator/roster.toml", ROSTER);
    git(&remote.source, &["push", "--quiet", "origin", "main"]);
    (remote, base)
}

/// Pushes a candidate branch off `base` changing `file` to `content`.
fn push_candidate(remote: &Remote, base: &str, file: &str, content: &str) -> String {
    push_candidate_message(remote, base, (file, content), file)
}

/// Pushes a candidate branch off `base` writing `(file, content)`, with the
/// commit message `message`.
fn push_candidate_message(
    remote: &Remote,
    base: &str,
    (file, content): (&str, &str),
    message: &str,
) -> String {
    git(
        &remote.source,
        &["checkout", "--quiet", "-B", "candidate", base],
    );
    let c = commit_message(&remote.source, file, content, message);
    git(
        &remote.source,
        &[
            "push",
            "--quiet",
            "--force",
            "origin",
            &format!("{c}:{CANDIDATE_REF}"),
        ],
    );
    git(&remote.source, &["checkout", "--quiet", "main"]);
    c
}

/// Moves the remote's `main` forward with one new commit.
fn advance_main(remote: &Remote, file: &str) -> String {
    land(remote, file, file)
}

/// Lands one commit writing `file` with `message` on the remote's `main`,
/// out of band.
fn land(remote: &Remote, file: &str, message: &str) -> String {
    let sha = commit_message(&remote.source, file, "x\n", message);
    git(&remote.source, &["push", "--quiet", "origin", "main"]);
    sha
}

/// The queue item the mock serves for candidate `c` reviewed at `base`.
fn item(remote: &Remote, base: &str, c: &str) -> Value {
    json!({"subject_task_id": "t1", "submission_id": "s1", "title": "task", "priority": 1,
        "candidate_revision": c, "candidate_tree": "x", "candidate_ref": CANDIDATE_REF,
        "reviewed_base": base, "repository_url": remote.url, "target_branch": "main",
        "task_digest": null, "results": []})
}

/// The remote's current `main`.
fn remote_main(remote: &Remote) -> String {
    let line = git(&remote.source, &["ls-remote", "origin", "refs/heads/main"]);
    line.split('\t').next().unwrap().to_owned()
}

/// Makes the remote reject the next update of `main` (a pre-receive hook).
fn reject_next_push(remote: &Remote) {
    let bare = std::path::Path::new(&remote.url);
    let hook = bare.join("hooks/pre-receive");
    let script = "#!/bin/sh\nif [ -f \"$GIT_DIR/reject\" ] && grep -q ' refs/heads/main$'; then rm \"$GIT_DIR/reject\"; exit 1; fi\nexit 0\n";
    std::fs::write(&hook, script).unwrap();
    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&hook)
        .status()
        .unwrap();
    std::fs::write(bare.join("reject"), "").unwrap();
}

/// An integrator over the mock with fake checks.
struct Harness {
    integrator: Integrator<FakeChecks>,
    mock: Shared,
    _state: tempfile::TempDir,
}

impl Harness {
    /// Starts the mock with `item` queued and no listed targets (the
    /// integrator watches the item's) and builds the integrator.
    async fn new(item: Value) -> Self {
        Self::start(Mock {
            item: Some(item),
            ..Mock::default()
        })
        .await
    }

    /// Starts the mock with no item and `remote`'s `main` as the one target.
    async fn idle(remote: &Remote) -> Self {
        let h = Self::start(Mock::default()).await;
        h.watch(remote);
        h
    }

    /// Lists `remote`'s `main` as a target of the project.
    fn watch(&self, remote: &Remote) {
        let target = json!({"repository_url": remote.url, "target_branch": "main"});
        self.mock.lock().unwrap().targets = vec![target];
    }

    /// Starts `mock` and builds the integrator over it.
    async fn start(mock: Mock) -> Self {
        let mock: Shared = Arc::new(Mutex::new(mock));
        let origin = serve(mock.clone()).await;
        let state = tempfile::tempdir().unwrap();
        let credentials = state.path().join("credentials.toml");
        let entry = format!("[[credentials]]\norigin = \"{origin}\"\ntoken = \"test-token\"\n");
        std::fs::write(&credentials, entry).unwrap();
        let config = test_config(state.path());
        let service = Service::from_credential_file(&credentials, "", true).unwrap();
        let checks = FakeChecks {
            path: config.fake_checks_file.clone(),
        };
        let loop_state = LoopState::load(&state.path().join("state.json")).unwrap();
        let integrator = Integrator {
            config,
            service,
            checks,
            state: loop_state,
        };
        Self {
            integrator,
            mock,
            _state: state,
        }
    }

    /// Rewrites the fake checks file: the rules, and an optional default.
    fn checks(&self, conclusion: Option<&str>, rules: &[&str]) {
        let body = json!({"rules": {"main": rules}, "default_conclusion": conclusion});
        std::fs::write(&self.integrator.config.fake_checks_file, body.to_string()).unwrap();
    }

    /// Scripts the per-attempt conclusions of `check` on `sha` in the fake
    /// checks file, keeping its rules, other scripts and recorded reruns.
    fn script(&self, sha: &str, check: &str, outcomes: &[Option<&str>]) {
        let outcomes = outcomes.iter().map(|o| o.map(str::to_owned)).collect();
        self.edit_fake(|file| {
            let scripts = file.scripts.entry(sha.to_owned()).or_default();
            scripts.insert(check.to_owned(), outcomes);
        });
    }

    /// Applies `change` to the fake checks file, keeping everything else.
    fn edit_fake(&self, change: impl FnOnce(&mut FakeFile)) {
        let path = &self.integrator.config.fake_checks_file;
        let mut file: FakeFile = std::fs::read_to_string(path)
            .map(|text| serde_json::from_str(&text).unwrap())
            .unwrap_or_default();
        change(&mut file);
        std::fs::write(path, serde_json::to_string(&file).unwrap()).unwrap();
    }

    /// Reruns the fake has recorded, over all runs.
    fn reruns(&self) -> i64 {
        let text = std::fs::read_to_string(&self.integrator.config.fake_checks_file).unwrap();
        let file: FakeFile = serde_json::from_str(&text).unwrap();
        file.reruns.values().sum()
    }

    /// The R of the latest pinned result.
    fn latest_r(&self) -> String {
        self.peek(|m| m.results.last().unwrap()["r"].as_str().unwrap().to_owned())
    }

    /// Runs one cycle for project `p`.
    async fn cycle(&mut self) -> Step {
        self.integrator.cycle("p").await.unwrap()
    }

    /// A value from the mock's state.
    fn peek<T>(&self, read: impl FnOnce(&Mock) -> T) -> T {
        read(&self.mock.lock().unwrap())
    }
}

/// Test configuration: fake checks under a temporary state directory.
fn test_config(state: &std::path::Path) -> Config {
    Config {
        projects: vec!["p".into()],
        state_dir: state.to_path_buf(),
        checks: ChecksKind::Fake,
        fake_checks_file: state.join("fake.json"),
        ..Config::default()
    }
}

#[tokio::test]
async fn approved_candidate_lands_as_a_deterministic_merge() {
    let (remote, base) = seeded_remote();
    let c = push_candidate(&remote, &base, "feature.txt", "feature\n");
    let side = advance_main(&remote, "side.txt");
    let mut h = Harness::new(item(&remote, &base, &c)).await;
    h.checks(Some("success"), &RULES);
    assert_eq!(h.cycle().await, Step::Observed("published".into()));
    let r = remote_main(&remote);
    git(&remote.source, &["fetch", "--quiet", "origin"]);
    let parents = git(
        &remote.source,
        &["rev-parse", &format!("{r}^1"), &format!("{r}^2")],
    );
    assert_eq!(parents, format!("{side}\n{c}"));
    let branch = git(
        &remote.source,
        &["ls-remote", "origin", "refs/heads/ac/results/res1"],
    );
    assert!(branch.starts_with(&r));
    assert_eq!(
        h.peek(|m| m.results[0]["landing_range"].clone()),
        json!([c])
    );
    assert_eq!(
        h.peek(|m| m.receipts.len()),
        2,
        "both jobs of one run get receipts"
    );
    assert_eq!(
        h.peek(|m| m.observations[0]["ancestry"].clone()),
        "contained"
    );
    assert_eq!(h.cycle().await, Step::Idle);
}

#[tokio::test]
async fn conflicts_and_failed_checks_revise() {
    let (remote, base) = seeded_remote();
    let c = push_candidate(&remote, &base, "base.txt", "candidate\n");
    commit(&remote.source, "base.txt", "target\n");
    git(&remote.source, &["push", "--quiet", "origin", "main"]);
    let mut h = Harness::new(item(&remote, &base, &c)).await;
    h.checks(Some("success"), &RULES);
    assert_eq!(h.cycle().await, Step::Revised("conflict".into()));
    let c2 = push_candidate(&remote, &base, "other.txt", "other\n");
    h.mock.lock().unwrap().item = Some(item(&remote, &base, &c2));
    h.checks(None, &RULES);
    assert_eq!(h.cycle().await, Step::ChecksPending);
    script_attempts(&h, &remote, &[FAIL, FAIL], &[PASS]);
    assert_eq!(h.cycle().await, Step::ChecksPending, "first failure reruns");
    assert_eq!(h.cycle().await, Step::Revised("check_failed".into()));
    assert_eq!(
        h.peek(|m| m.revises[1]["reason_code"].clone()),
        "check_failed"
    );
}

const FAIL: Option<&str> = Some("failure");
const PASS: Option<&str> = Some("success");

/// Scripts "Linux tests" on the latest R and on the remote's `main` (X);
/// "Lint" passes once on R.
fn script_attempts(h: &Harness, remote: &Remote, on_r: &[Option<&str>], on_x: &[Option<&str>]) {
    let r = h.latest_r();
    h.script(&r, "Linux tests", on_r);
    h.script(&r, "Lint", &[PASS]);
    if !on_x.is_empty() {
        h.script(&remote_main(remote), "Linux tests", on_x);
    }
}

/// A harness whose first cycle pinned R with no check runs yet.
async fn pinned_harness() -> (Harness, Remote) {
    let (remote, base) = seeded_remote();
    let c = push_candidate(&remote, &base, "feature.txt", "feature\n");
    let mut h = Harness::new(item(&remote, &base, &c)).await;
    h.checks(None, &RULES);
    assert_eq!(h.cycle().await, Step::ChecksPending);
    (h, remote)
}

#[tokio::test]
async fn a_reproduced_failure_that_passes_on_the_target_revises_with_receipts() {
    let (mut h, remote) = pinned_harness().await;
    script_attempts(&h, &remote, &[FAIL], &[PASS]);
    assert_eq!(h.cycle().await, Step::ChecksPending);
    assert_eq!(
        h.cycle().await,
        Step::ChecksPending,
        "rerun not started yet"
    );
    assert_eq!(h.reruns(), 1, "a rerun in flight is not requested again");
    script_attempts(&h, &remote, &[FAIL, FAIL], &[PASS]);
    assert_eq!(h.cycle().await, Step::Revised("check_failed".into()));
    let revise = h.peek(|m| m.revises[0].clone());
    assert_eq!(revise["result_id"], "res1");
    let evidence = revise["evidence"].as_str().unwrap();
    assert!(evidence.contains("attempt 1, run"), "{evidence}");
    assert!(
        evidence.contains("attempt 2) and passed on target"),
        "{evidence}"
    );
    assert_eq!(
        h.peek(|m| m.receipts.len()),
        4,
        "every attempt of both jobs has a receipt"
    );
    assert!(report_kinds(&h).is_empty());
}

const CANCELLED: Option<&str> = Some("cancelled");

/// The conclusions listed in report `index`'s attempts.
fn reported_attempts(h: &Harness, index: usize) -> Vec<Value> {
    let details = h.peek(|m| m.reports[index]["details"].clone());
    let attempts = details["attempts"].as_array().unwrap().iter();
    attempts.map(|a| a["conclusion"].clone()).collect()
}

#[tokio::test]
async fn a_failure_then_a_pass_is_flaky_and_publishes() {
    let (mut h, remote) = pinned_harness().await;
    script_attempts(&h, &remote, &[FAIL, PASS], &[]);
    assert_eq!(h.cycle().await, Step::ChecksPending);
    assert_eq!(h.cycle().await, Step::Observed("published".into()));
    assert_eq!(h.reruns(), 1, "a passing run is never rerun");
    assert_eq!(report_kinds(&h), ["flaky"]);
    let details = h.peek(|m| m.reports[0]["details"].clone());
    assert_eq!(
        (details["check_name"].clone(), details["verdict"].clone()),
        (json!("Linux tests"), json!("flaky"))
    );
    assert_eq!(details["blocks_subject"], false, "a flaky pass publishes");
    assert_eq!(
        reported_attempts(&h, 0),
        [json!("failure"), json!("success")]
    );
    assert!(h.peek(|m| m.revises.is_empty()));
}

#[tokio::test]
async fn a_cancelled_attempt_is_rerun_and_is_not_flaky() {
    let (mut h, remote) = pinned_harness().await;
    script_attempts(&h, &remote, &[CANCELLED, PASS], &[]);
    assert_eq!(h.cycle().await, Step::ChecksPending);
    assert_eq!(h.cycle().await, Step::Observed("published".into()));
    assert!(report_kinds(&h).is_empty());
}

#[tokio::test]
async fn a_check_without_a_result_blocks_after_the_last_rerun() {
    let (mut h, remote) = pinned_harness().await;
    script_attempts(&h, &remote, &[CANCELLED, CANCELLED, CANCELLED], &[]);
    assert_eq!(h.cycle().await, Step::ChecksPending);
    assert_eq!(h.cycle().await, Step::ChecksPending);
    for _ in 0..2 {
        let blocked = h.cycle().await;
        assert!(
            matches!(&blocked, Step::Blocked(r) if r.starts_with("no_result")),
            "{blocked:?}"
        );
    }
    assert_eq!(h.reruns(), 2);
    assert_eq!(report_kinds(&h), ["flaky"]);
    let verdict = h.peek(|m| m.reports[0]["details"]["verdict"].clone());
    assert_eq!(verdict, "no_result");
    assert!(h.peek(|m| m.revises.is_empty()));
}

#[tokio::test]
async fn a_refused_rerun_blocks_and_is_asked_again() {
    let (mut h, remote) = pinned_harness().await;
    script_attempts(&h, &remote, &[FAIL, FAIL], &[PASS]);
    h.edit_fake(|file| file.rerun_error = Some("403 actions: write".into()));
    let blocked = h.cycle().await;
    assert!(
        matches!(&blocked, Step::Blocked(r) if r.starts_with("rerun_refused")),
        "{blocked:?}"
    );
    let verdict = h.peek(|m| m.reports[0]["details"]["verdict"].clone());
    assert_eq!(verdict, "rerun_refused");
    let blocks = h.peek(|m| m.reports[0]["details"]["blocks_subject"].clone());
    assert_eq!(blocks, true, "a refused rerun needs a human");
    h.edit_fake(|file| file.rerun_error = None);
    assert_eq!(h.cycle().await, Step::ChecksPending);
    assert_eq!(h.cycle().await, Step::Revised("check_failed".into()));
}

#[tokio::test]
async fn a_target_run_in_progress_waits_without_a_report() {
    let (mut h, remote) = pinned_harness().await;
    script_attempts(&h, &remote, &[FAIL, FAIL], &[None]);
    assert_eq!(h.cycle().await, Step::ChecksPending);
    assert_eq!(h.cycle().await, Step::ChecksPending, "X is still running");
    assert!(report_kinds(&h).is_empty());
    assert!(h.peek(|m| m.revises.is_empty()));
    h.script(&remote_main(&remote), "Linux tests", &[PASS]);
    assert_eq!(h.cycle().await, Step::Revised("check_failed".into()));
}

#[tokio::test]
async fn a_failure_on_the_target_too_blocks_and_reports_fix_target_once() {
    let (mut h, remote) = pinned_harness().await;
    script_attempts(&h, &remote, &[FAIL, FAIL], &[FAIL]);
    assert_eq!(h.cycle().await, Step::ChecksPending);
    for _ in 0..2 {
        let blocked = h.cycle().await;
        assert!(
            matches!(&blocked, Step::Blocked(r) if r.starts_with("target_failing")),
            "{blocked:?}"
        );
    }
    assert!(
        h.peek(|m| m.revises.is_empty()),
        "the author is not revised"
    );
    assert_eq!(report_kinds(&h), ["fix_target"]);
    let details = h.peek(|m| m.reports[0]["details"].clone());
    assert_eq!(details["verdict"], "target_failing");
    assert_eq!(
        details["blocks_subject"], true,
        "a blocked subject needs a human"
    );
    advance_main(&remote, "fix.txt");
    assert_eq!(h.cycle().await, Step::ChecksPending, "a new X re-evaluates");
    assert_eq!(h.peek(|m| m.results.len()), 2);
}

#[tokio::test]
async fn a_failure_with_no_run_on_the_target_is_unverified() {
    let (mut h, remote) = pinned_harness().await;
    script_attempts(&h, &remote, &[FAIL, FAIL], &[]);
    assert_eq!(h.cycle().await, Step::ChecksPending);
    let blocked = h.cycle().await;
    assert!(
        matches!(&blocked, Step::Blocked(r) if r.starts_with("target_unverified")),
        "{blocked:?}"
    );
    assert!(h.peek(|m| m.revises.is_empty()));
    assert_eq!(report_kinds(&h), ["fix_target"]);
    let details = h.peek(|m| m.reports[0]["details"].clone());
    assert_eq!(details["verdict"], "target_unverified");
}

#[tokio::test]
async fn pending_checks_roll_forward_and_survive_lost_local_state() {
    let (remote, base) = seeded_remote();
    let c = push_candidate(&remote, &base, "feature.txt", "feature\n");
    let mut h = Harness::new(item(&remote, &base, &c)).await;
    h.checks(None, &RULES);
    assert_eq!(h.cycle().await, Step::ChecksPending);
    let moved = advance_main(&remote, "moved.txt");
    assert_eq!(h.cycle().await, Step::ChecksPending);
    let state = h.integrator.config.state_dir.clone();
    std::fs::remove_dir_all(state.join("mirrors")).unwrap();
    std::fs::remove_dir_all(state.join("intents")).unwrap();
    std::fs::remove_dir_all(state.join("worktrees")).unwrap();
    h.checks(Some("success"), &RULES);
    assert_eq!(h.cycle().await, Step::Observed("published".into()));
    assert_eq!(h.peek(|m| m.results.len()), 2);
    assert_eq!(h.peek(|m| m.results[1]["t0"].clone()), json!(moved));
}

#[tokio::test]
async fn a_rejected_push_is_attested_and_retried() {
    let (remote, base) = seeded_remote();
    let c = push_candidate(&remote, &base, "feature.txt", "feature\n");
    advance_main(&remote, "side.txt");
    reject_next_push(&remote);
    let mut h = Harness::new(item(&remote, &base, &c)).await;
    h.checks(Some("success"), &RULES);
    assert_eq!(h.cycle().await, Step::Observed("not_published".into()));
    assert_eq!(h.cycle().await, Step::Observed("published".into()));
    assert_eq!(
        h.peek(|m| m.results.len()),
        1,
        "the same pinned R is retried"
    );
    assert_eq!(h.peek(|m| m.observations.len()), 2);
}

#[tokio::test]
async fn held_authority_elsewhere_is_observed_first() {
    let (remote, base) = seeded_remote();
    let c = push_candidate(&remote, &base, "feature.txt", "feature\n");
    git(
        &remote.source,
        &["push", "--quiet", "origin", &format!("{c}:refs/heads/main")],
    );
    let mut h = Harness::new(item(&remote, &base, &c)).await;
    let held = json!({"id": "res0", "submission_id": "s1", "t0": base, "t0_tree": "x", "c": c,
        "r": c, "r_tree": "x", "landing_range": [c], "roster": {}, "authority_expires_at": "2099-01-01T00:00:00.000Z"});
    h.mock.lock().unwrap().results.push(held);
    h.checks(None, &RULES);
    assert_eq!(h.cycle().await, Step::Observed("published".into()));
    assert_eq!(h.peek(|m| m.observations[0]["result_id"].clone()), "res0");
}

#[tokio::test]
async fn missing_rules_rewrites_and_workflow_edits_stop_the_integrator() {
    let (remote, base) = seeded_remote();
    let c = push_candidate(&remote, &base, ".github/workflows/ci.yml", "on: [push]\n");
    let mut h = Harness::new(item(&remote, &base, &c)).await;
    h.checks(Some("success"), &["non_fast_forward"]);
    assert_eq!(
        h.cycle().await,
        Step::Frozen("ruleset_missing: required_status_checks".into())
    );
    h.checks(Some("success"), &RULES);
    let blocked = h.cycle().await;
    assert!(
        matches!(&blocked, Step::Blocked(r) if r.starts_with("privilege_gate")),
        "{blocked:?}"
    );
    git(&remote.source, &["reset", "--quiet", "--hard", "HEAD~1"]);
    git(
        &remote.source,
        &[
            "push",
            "--quiet",
            "--force",
            "origin",
            "HEAD:refs/heads/main",
        ],
    );
    let frozen = h.cycle().await;
    assert!(
        matches!(&frozen, Step::Frozen(r) if r.starts_with("target_rewritten")),
        "{frozen:?}"
    );
    assert!(matches!(h.cycle().await, Step::Frozen(_)));
    assert_eq!(
        report_kinds(&h),
        ["ruleset_missing", "privilege_gate", "target_rewritten"],
        "one report per freeze and gate"
    );
}

/// The kinds of the reports the mock has stored.
fn report_kinds(h: &Harness) -> Vec<Value> {
    h.peek(|m| m.reports.iter().map(|r| r["kind"].clone()).collect())
}

#[tokio::test]
async fn privilege_gated_results_wait_for_a_human_allow() {
    let (remote, base) = seeded_remote();
    let deploy = "on: push\njobs:\n  d:\n    env:\n      T: ${{ secrets.TOKEN }}\n";
    let c = push_candidate(&remote, &base, ".github/workflows/deploy.yml", deploy);
    let mut h = Harness::new(item(&remote, &base, &c)).await;
    h.checks(Some("success"), &RULES);
    for _ in 0..2 {
        let blocked = h.cycle().await;
        assert!(
            matches!(&blocked, Step::Blocked(r) if r.contains("awaits a human decision")),
            "{blocked:?}"
        );
    }
    assert_eq!(
        report_kinds(&h),
        ["privilege_gate"],
        "a retry does not duplicate"
    );
    let workflow = h.peek(|m| m.reports[0]["details"]["workflows"][0].clone());
    assert_eq!(workflow["path"], ".github/workflows/deploy.yml");
    assert_eq!(
        workflow["reasons"],
        json!(["new_file", "adds_secrets", "adds_token"])
    );
    assert_eq!(h.peek(|m| m.reports[0]["result_id"].clone()), "res1");
    assert!(h.peek(|m| m.receipts.is_empty()), "nothing ran on R");
    resolve_report(&h, 0, true);
    assert_eq!(h.cycle().await, Step::Observed("published".into()));
}

/// Marks the mock's report `index` resolved by a human, allowed or not.
fn resolve_report(h: &Harness, index: usize, allowed: bool) {
    let mut mock = h.mock.lock().unwrap();
    mock.reports[index]["resolved_at"] = json!("2026-09-28T00:00:00.000Z");
    mock.reports[index]["allowed"] = json!(allowed);
}

#[tokio::test]
async fn a_denied_privilege_gate_keeps_r_unpushed() {
    let (remote, base) = seeded_remote();
    let deploy = "on: push\njobs:\n  d:\n    permissions:\n      contents: write\n";
    let c = push_candidate(&remote, &base, ".github/workflows/deploy.yml", deploy);
    let mut h = Harness::new(item(&remote, &base, &c)).await;
    h.checks(Some("success"), &RULES);
    assert!(matches!(h.cycle().await, Step::Blocked(_)));
    resolve_report(&h, 0, false);
    let blocked = h.cycle().await;
    assert!(
        matches!(&blocked, Step::Blocked(r) if r.contains("denied")),
        "{blocked:?}"
    );
    assert_eq!(remote_main(&remote), base, "the target did not move");
    assert!(h.peek(|m| m.receipts.is_empty() && m.observations.is_empty()));
    let branch = git(
        &remote.source,
        &["ls-remote", "origin", "refs/heads/ac/results/res1"],
    );
    assert!(branch.is_empty(), "R was never pushed for checks");
}

#[tokio::test]
async fn a_ruleset_lost_again_after_resolution_is_reported_again() {
    let (remote, base) = seeded_remote();
    let c = push_candidate(&remote, &base, "feature.txt", "feature\n");
    let mut h = Harness::new(item(&remote, &base, &c)).await;
    h.checks(None, &[]);
    assert!(matches!(h.cycle().await, Step::Frozen(_)));
    resolve_report(&h, 0, false);
    h.checks(None, &RULES);
    assert_eq!(h.cycle().await, Step::ChecksPending);
    h.checks(None, &[]);
    assert!(matches!(h.cycle().await, Step::Frozen(_)));
    assert!(matches!(h.cycle().await, Step::Frozen(_)));
    assert_eq!(report_kinds(&h), ["ruleset_missing", "ruleset_missing"]);
    let keys = h.peek(|m| {
        (
            m.reports[0]["dedupe_key"].clone(),
            m.reports[1]["dedupe_key"].clone(),
        )
    });
    assert_ne!(keys.0, keys.1, "each freeze episode has its own key");
}

#[tokio::test]
async fn a_missing_ruleset_is_reported_once_across_cycles() {
    let (remote, base) = seeded_remote();
    let c = push_candidate(&remote, &base, "feature.txt", "feature\n");
    let mut h = Harness::new(item(&remote, &base, &c)).await;
    h.checks(Some("success"), &[]);
    let frozen = Step::Frozen("ruleset_missing: non_fast_forward, required_status_checks".into());
    assert_eq!(h.cycle().await, frozen);
    assert_eq!(h.cycle().await, frozen);
    assert_eq!(report_kinds(&h), ["ruleset_missing"]);
    let missing = h.peek(|m| m.reports[0]["details"]["missing_rules"].clone());
    assert_eq!(missing, json!(RULES));
}

#[tokio::test]
async fn already_contained_candidates_are_observed_without_a_push() {
    let (remote, base) = seeded_remote();
    let c = push_candidate(&remote, &base, "feature.txt", "feature\n");
    git(
        &remote.source,
        &["push", "--quiet", "origin", &format!("{c}:refs/heads/main")],
    );
    let mut h = Harness::new(item(&remote, &base, &c)).await;
    h.checks(None, &RULES);
    assert_eq!(h.cycle().await, Step::Observed("published".into()));
    assert!(h.peek(|m| m.receipts.is_empty()));
}

/// A commit message whose trailers mark an agent-authored commit.
const AGENT_MESSAGE: &str =
    "Agent change\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>";

/// The details of the mock's report `index`.
fn report_details(h: &Harness, index: usize) -> Value {
    h.peek(|m| m.reports[index]["details"].clone())
}

/// The SHAs a report lists as flagged.
fn flagged_shas(details: &Value) -> Vec<Value> {
    let flagged = details["flagged"].as_array().unwrap().iter();
    flagged.map(|commit| commit["sha"].clone()).collect()
}

#[tokio::test]
async fn an_idle_target_is_watched_for_missing_rules() {
    let (remote, _) = seeded_remote();
    let mut h = Harness::idle(&remote).await;
    h.checks(None, &["non_fast_forward"]);
    let frozen = Step::Frozen("ruleset_missing: required_status_checks".into());
    assert_eq!(h.cycle().await, frozen);
    assert_eq!(h.cycle().await, frozen);
    assert_eq!(report_kinds(&h), ["ruleset_missing"]);
    h.checks(None, &RULES);
    assert_eq!(h.cycle().await, Step::Idle, "the rules are back");
    h.checks(None, &["non_fast_forward"]);
    assert_eq!(h.cycle().await, frozen);
    assert_eq!(report_kinds(&h), ["ruleset_missing", "ruleset_missing"]);
    let keys = h.peek(|m| {
        (
            m.reports[0]["dedupe_key"].clone(),
            m.reports[1]["dedupe_key"].clone(),
        )
    });
    assert_ne!(keys.0, keys.1, "the idle cycle ended the first episode");
}

#[tokio::test]
async fn an_agent_trailer_landing_is_reported_once() {
    let (remote, base) = seeded_remote();
    let mut h = Harness::idle(&remote).await;
    h.checks(None, &RULES);
    assert_eq!(h.cycle().await, Step::Idle);
    let agent = land(&remote, "agent.txt", AGENT_MESSAGE);
    let own = advance_main(&remote, "own.txt");
    for _ in 0..2 {
        assert_eq!(h.cycle().await, Step::Idle);
    }
    assert_eq!(report_kinds(&h), ["unreviewed_landing"]);
    let details = report_details(&h, 0);
    assert_eq!(
        (details["from"].clone(), details["to"].clone()),
        (json!(base), json!(own))
    );
    assert_eq!(flagged_shas(&details), [json!(agent)]);
    let trailers = &details["flagged"][0]["trailers"];
    assert_eq!(trailers, &json!([AGENT_MESSAGE.lines().last().unwrap()]));
    assert_eq!(details["flagged"][0]["subject"], "Agent change");
    assert_eq!(
        (
            details["flagged_count"].clone(),
            details["unflagged_count"].clone()
        ),
        (json!(1), json!(1))
    );
    assert_eq!(h.peek(|m| m.reports[0]["task_id"].clone()), Value::Null);
}

#[tokio::test]
async fn the_users_own_landing_is_not_reported() {
    let (remote, _) = seeded_remote();
    let mut h = Harness::idle(&remote).await;
    h.checks(None, &RULES);
    assert_eq!(h.cycle().await, Step::Idle);
    let human = "Pair work\n\nCo-authored-by: Jane Doe <jane@example.com>";
    land(&remote, "pair.txt", human);
    advance_main(&remote, "solo.txt");
    assert_eq!(h.cycle().await, Step::Idle);
    assert!(report_kinds(&h).is_empty());
}

/// A harness whose first cycle published an agent-authored candidate, with
/// `main` listed as a target; returns it with the remote and R.
async fn published_agent_candidate() -> (Harness, Remote, String) {
    let (remote, base) = seeded_remote();
    let file = ("feature.txt", "feature\n");
    let c = push_candidate_message(&remote, &base, file, AGENT_MESSAGE);
    let mut h = Harness::new(item(&remote, &base, &c)).await;
    h.watch(&remote);
    h.checks(Some("success"), &RULES);
    assert_eq!(h.cycle().await, Step::Observed("published".into()));
    let r = remote_main(&remote);
    (h, remote, r)
}

#[tokio::test]
async fn the_integrators_own_publish_is_not_reported() {
    let (mut h, _remote, r) = published_agent_candidate().await;
    assert_eq!(h.latest_r(), r);
    assert_eq!(h.cycle().await, Step::Idle);
    assert_eq!(h.cycle().await, Step::Idle);
    assert!(report_kinds(&h).is_empty());
}

#[tokio::test]
async fn only_commits_after_a_publish_are_out_of_band() {
    let (mut h, remote, r) = published_agent_candidate().await;
    git(
        &remote.source,
        &["pull", "--quiet", "--ff-only", "origin", "main"],
    );
    let late = land(&remote, "late.txt", AGENT_MESSAGE);
    assert_eq!(h.cycle().await, Step::Idle);
    assert_eq!(report_kinds(&h), ["unreviewed_landing"]);
    let details = report_details(&h, 0);
    assert_eq!(flagged_shas(&details), [json!(late)]);
    assert_eq!(details["integrator_results"], json!([r]));
    assert_eq!(details["unflagged_count"], 0);
}

#[tokio::test]
async fn an_unreachable_service_keeps_the_move_for_the_next_cycle() {
    let (remote, base) = seeded_remote();
    let mut h = Harness::idle(&remote).await;
    h.checks(None, &RULES);
    assert_eq!(h.cycle().await, Step::Idle);
    let agent = land(&remote, "agent.txt", AGENT_MESSAGE);
    h.mock.lock().unwrap().reports_down = true;
    assert_unsettled(h.cycle().await);
    h.mock.lock().unwrap().reports_down = false;
    let own = advance_main(&remote, "own.txt");
    assert_eq!(h.cycle().await, Step::Idle);
    assert_eq!(report_kinds(&h), ["unreviewed_landing"]);
    let details = report_details(&h, 0);
    assert_eq!(
        (details["from"].clone(), details["to"].clone()),
        (json!(base), json!(own))
    );
    assert_eq!(flagged_shas(&details), [json!(agent)]);
}

#[tokio::test]
async fn a_rewritten_idle_target_freezes() {
    let (remote, _) = seeded_remote();
    let mut h = Harness::idle(&remote).await;
    h.checks(None, &RULES);
    assert_eq!(h.cycle().await, Step::Idle);
    git(&remote.source, &["reset", "--quiet", "--hard", "HEAD~1"]);
    git(
        &remote.source,
        &[
            "push",
            "--quiet",
            "--force",
            "origin",
            "HEAD:refs/heads/main",
        ],
    );
    for _ in 0..2 {
        let frozen = h.cycle().await;
        assert!(
            matches!(&frozen, Step::Frozen(r) if r.starts_with("target_rewritten")),
            "{frozen:?}"
        );
    }
    assert_eq!(report_kinds(&h), ["target_rewritten"]);
}

/// Asserts the step that holds a target whose tip move is not yet reported.
fn assert_unsettled(step: Step) {
    assert!(
        matches!(&step, Step::Blocked(r) if r.starts_with("tip_move_unsettled")),
        "{step:?}"
    );
}

#[tokio::test]
async fn nothing_publishes_on_a_tip_whose_landing_is_unreported() {
    let (remote, base) = seeded_remote();
    let mut h = Harness::idle(&remote).await;
    h.checks(Some("success"), &RULES);
    assert_eq!(h.cycle().await, Step::Idle);
    let agent = land(&remote, "agent.txt", AGENT_MESSAGE);
    let file = ("feature.txt", "feature\n");
    let c = push_candidate_message(&remote, &base, file, AGENT_MESSAGE);
    h.mock.lock().unwrap().item = Some(item(&remote, &base, &c));
    h.mock.lock().unwrap().reports_down = true;
    assert_unsettled(h.cycle().await);
    assert_eq!(remote_main(&remote), agent, "nothing was published");
    h.mock.lock().unwrap().reports_down = false;
    assert_eq!(h.cycle().await, Step::Observed("published".into()));
    for _ in 0..2 {
        assert_eq!(h.cycle().await, Step::Idle);
    }
    assert_eq!(report_kinds(&h), ["unreviewed_landing"]);
    assert_eq!(flagged_shas(&report_details(&h, 0)), [json!(agent)]);
}

/// The queue item for a second subject `s2` with candidate `c` reviewed at
/// `base`.
fn second_item(remote: &Remote, base: &str, c: &str) -> Value {
    let mut second = item(remote, base, c);
    second["subject_task_id"] = json!("t2");
    second["submission_id"] = json!("s2");
    second
}

/// The `moved_by_result_id` of the first revise the mock received.
fn cited_landing(h: &Harness) -> Value {
    h.peek(|m| m.revises[0]["moved_by_result_id"].clone())
}

#[tokio::test]
async fn a_conflict_with_a_published_subject_cites_its_result() {
    let (remote, base) = seeded_remote();
    let c = push_candidate(&remote, &base, "shared.txt", "first\n");
    let mut h = Harness::new(item(&remote, &base, &c)).await;
    h.checks(Some("success"), &RULES);
    assert_eq!(h.cycle().await, Step::Observed("published".into()));
    let c2 = push_candidate(&remote, &base, "shared.txt", "second\n");
    h.mock.lock().unwrap().item = Some(second_item(&remote, &base, &c2));
    assert_eq!(h.cycle().await, Step::Revised("conflict".into()));
    assert_eq!(cited_landing(&h), "res1");
}

#[tokio::test]
async fn a_conflict_with_an_out_of_band_landing_cites_no_result() {
    let (remote, base) = seeded_remote();
    let c = push_candidate(&remote, &base, "feature.txt", "feature\n");
    let mut h = Harness::new(item(&remote, &base, &c)).await;
    h.checks(Some("success"), &RULES);
    assert_eq!(h.cycle().await, Step::Observed("published".into()));
    let r = remote_main(&remote);
    let pull = ["pull", "--quiet", "--ff-only", "origin", "main"];
    git(&remote.source, &pull);
    let c2 = push_candidate(&remote, &r, "shared.txt", "candidate\n");
    land(&remote, "shared.txt", "Out of band");
    h.mock.lock().unwrap().item = Some(second_item(&remote, &r, &c2));
    assert_eq!(h.cycle().await, Step::Revised("conflict".into()));
    assert_eq!(cited_landing(&h), Value::Null);
}

/// The queue's `reverts` entry for revert task `rt1` of the mock's first
/// result (subject `t1`'s published candidate) on `remote`'s `main`.
fn revert_of(h: &Harness, remote: &Remote) -> Value {
    let result = h.peek(|m| m.results[0].clone());
    let target = json!({"submission_id": "s1", "result_id": result["id"],
        "original_task_id": "t1", "r": result["r"], "t0": result["t0"], "c": result["c"],
        "landing_range": result["landing_range"], "reason": "human", "evidence": null});
    json!({"id": "rt1", "task_id": "rt1", "result_id": result["id"], "r": result["r"],
        "title": "Revert: task", "priority": 0, "repository_url": remote.url,
        "target_branch": "main", "target": target})
}

/// Publishes candidate `c` (reviewed at `base`) with `main` watched and
/// passing checks, then queues its revert.
async fn published_then_reverted(remote: &Remote, base: &str, c: &str) -> Harness {
    let mut h = Harness::new(item(remote, base, c)).await;
    h.watch(remote);
    h.checks(Some("success"), &RULES);
    assert_eq!(h.cycle().await, Step::Observed("published".into()));
    let revert = revert_of(&h, remote);
    h.mock.lock().unwrap().reverts = vec![revert];
    h
}

/// The files in the remote's `main`, fetched into the source clone.
fn main_files(remote: &Remote) -> String {
    git(&remote.source, &["fetch", "--quiet", "origin"]);
    git(&remote.source, &["ls-tree", "--name-only", "origin/main"])
}

/// The commit the remote's `ref` names; empty when it is absent.
fn remote_ref(remote: &Remote, reference: &str) -> String {
    let line = git(&remote.source, &["ls-remote", "origin", reference]);
    line.split('\t').next().unwrap_or_default().to_owned()
}

#[tokio::test]
async fn a_clean_merge_revert_is_proposed_on_the_tip_and_publishes() {
    let (remote, base) = seeded_remote();
    let c = push_candidate(&remote, &base, "feature.txt", "feature\n");
    advance_main(&remote, "side.txt");
    let mut h = published_then_reverted(&remote, &base, &c).await;
    let x = remote_main(&remote);
    assert_eq!(h.cycle().await, Step::RevertCandidate("rt1".into()));
    let candidate = h.peek(|m| m.candidates[0].clone());
    assert_eq!(candidate["t0"], x.as_str());
    assert_eq!(candidate["mechanical"], true);
    let commit = candidate["candidate_commit"].as_str().unwrap().to_owned();
    let reference = candidate["candidate_ref"].as_str().unwrap();
    assert_eq!(
        reference,
        format!("refs/agent-coordinator/candidates/reverts/rt1/{x}")
    );
    assert_eq!(
        remote_ref(&remote, reference),
        commit,
        "candidate ref pushed"
    );
    assert_eq!(h.cycle().await, Step::Observed("published".into()));
    assert_eq!(remote_main(&remote), commit, "the revert fast-forwards X");
    let files = main_files(&remote);
    assert!(
        !files.contains("feature.txt") && files.contains("side.txt"),
        "{files}"
    );
    assert_eq!(
        h.peek(|m| m.observations[1]["ancestry"].clone()),
        "contained"
    );
}

/// Pushes a candidate branch off `base` whose commits write `one.txt` and
/// then `two.txt`, and returns its tip.
fn push_two_commit_candidate(remote: &Remote, base: &str) -> String {
    push_candidate(remote, base, "one.txt", "one\n");
    git(&remote.source, &["checkout", "--quiet", "candidate"]);
    let c = commit(&remote.source, "two.txt", "two\n");
    let refspec = format!("{c}:{CANDIDATE_REF}");
    git(
        &remote.source,
        &["push", "--quiet", "--force", "origin", &refspec],
    );
    git(&remote.source, &["checkout", "--quiet", "main"]);
    c
}

#[tokio::test]
async fn a_fast_forward_range_revert_undoes_every_commit() {
    let (remote, base) = seeded_remote();
    let c = push_two_commit_candidate(&remote, &base);
    let mut h = published_then_reverted(&remote, &base, &c).await;
    assert_eq!(remote_main(&remote), c, "the candidate landed fast-forward");
    assert_eq!(h.cycle().await, Step::RevertCandidate("rt1".into()));
    assert_eq!(h.cycle().await, Step::Observed("published".into()));
    let tree = |rev: &str| git(&remote.source, &["rev-parse", &format!("{rev}^{{tree}}")]);
    assert_eq!(main_files(&remote), ".agent-coordinator\n.github\nbase.txt");
    assert_eq!(tree("origin/main"), tree(&base), "the range is undone");
}

#[tokio::test]
async fn a_conflicting_revert_is_reported_not_mechanical() {
    let (remote, base) = seeded_remote();
    let c = push_candidate(&remote, &base, "feature.txt", "feature\n");
    let mut h = published_then_reverted(&remote, &base, &c).await;
    let pull = ["pull", "--quiet", "--ff-only", "origin", "main"];
    git(&remote.source, &pull);
    let x = land(&remote, "feature.txt", "Later change");
    assert_eq!(h.cycle().await, Step::NotMechanical("conflict".into()));
    let report = h.peek(|m| m.not_mechanical[0].clone());
    assert_eq!(
        (report["id"].clone(), report["t0"].clone()),
        (json!("rt1"), json!(x))
    );
    let evidence = report["evidence"].as_str().unwrap();
    assert!(
        evidence.contains("feature.txt") && evidence.contains(&c),
        "{evidence}"
    );
    assert!(h.peek(|m| m.candidates.is_empty() && m.revises.is_empty()));
    assert_eq!(h.cycle().await, Step::Idle, "the revert left the queue");
}

#[tokio::test]
async fn a_reproduced_check_failure_on_a_revert_is_not_mechanical() {
    let (remote, base) = seeded_remote();
    let c = push_candidate(&remote, &base, "feature.txt", "feature\n");
    let mut h = published_then_reverted(&remote, &base, &c).await;
    h.checks(None, &RULES);
    assert_eq!(h.cycle().await, Step::RevertCandidate("rt1".into()));
    assert_eq!(h.cycle().await, Step::ChecksPending);
    script_attempts(&h, &remote, &[FAIL, FAIL], &[PASS]);
    assert_eq!(h.cycle().await, Step::ChecksPending, "first failure reruns");
    assert_eq!(h.cycle().await, Step::NotMechanical("check_failed".into()));
    assert!(
        h.peek(|m| m.revises.is_empty()),
        "a revert is never revised"
    );
    let report = h.peek(|m| m.not_mechanical[0].clone());
    assert_eq!(report["t0"], remote_main(&remote).as_str());
    let evidence = report["evidence"].as_str().unwrap();
    assert!(evidence.contains("passed on target"), "{evidence}");
    assert_eq!(remote_main(&remote), c, "nothing was published");
}

#[tokio::test]
async fn a_frozen_target_skips_its_reverts() {
    let (remote, base) = seeded_remote();
    let c = push_candidate(&remote, &base, "feature.txt", "feature\n");
    let mut h = published_then_reverted(&remote, &base, &c).await;
    h.checks(Some("success"), &["non_fast_forward"]);
    let frozen = Step::Frozen("ruleset_missing: required_status_checks".into());
    assert_eq!(h.cycle().await, frozen);
    assert!(h.peek(|m| m.candidates.is_empty() && m.not_mechanical.is_empty()));
    let prefix = "refs/agent-coordinator/candidates/reverts/";
    assert!(remote_ref(&remote, &format!("{prefix}*")).is_empty());
    h.checks(Some("success"), &RULES);
    assert_eq!(h.cycle().await, Step::RevertCandidate("rt1".into()));
}

/// A harness whose queued candidate `c` is already contained in `main`
/// (a no-op), with the harness's mock configured by `setup`; returns it with
/// the remote and `c`.
async fn contained_candidate(setup: impl FnOnce(&mut Mock, &str)) -> (Harness, Remote, String) {
    let (remote, base) = seeded_remote();
    let c = push_candidate(&remote, &base, "feature.txt", "feature\n");
    let refspec = format!("{c}:refs/heads/main");
    git(&remote.source, &["push", "--quiet", "origin", &refspec]);
    let h = Harness::new(item(&remote, &base, &c)).await;
    setup(&mut h.mock.lock().unwrap(), &c);
    h.checks(None, &RULES);
    (h, remote, c)
}

/// The `reverted` entry of a landing `[c]` a revert undid on `main`.
fn reverted_landing(c: &str) -> Value {
    json!({"revert_task_id": "rt0", "result_id": "res0", "landing_range": [c],
        "repository_url": null, "target_branch": "main"})
}

/// Asserts the first revise is `reverted_in_history` naming `c`, and that
/// the mock holds no result and no observation.
fn assert_reverted_revise(h: &Harness, c: &str) {
    let revise = h.peek(|m| m.revises[0].clone());
    assert_eq!(revise["reason_code"], "reverted_in_history");
    let evidence = revise["evidence"].as_str().unwrap();
    assert!(evidence.ends_with(&format!(": {c}")), "{evidence}");
    assert!(h.peek(|m| m.results.is_empty() && m.observations.is_empty()));
}

#[tokio::test]
async fn a_no_op_over_a_reverted_landing_is_revised_not_pinned() {
    let (mut h, _remote, c) =
        contained_candidate(|mock, c| mock.reverted = vec![reverted_landing(c)]).await;
    assert_eq!(h.cycle().await, Step::Revised("reverted_in_history".into()));
    assert_reverted_revise(&h, &c);
}

#[tokio::test]
async fn a_no_op_the_service_refuses_as_reverted_is_revised() {
    let (mut h, _remote, c) = contained_candidate(|mock, c| {
        mock.reverted_history = Some(json!({"reverted_results": ["res0"], "commits": [c]}));
    })
    .await;
    assert_eq!(h.cycle().await, Step::Revised("reverted_in_history".into()));
    assert_reverted_revise(&h, &c);
}

#[tokio::test]
async fn a_disputed_reverted_history_blocks_and_reports() {
    let (mut h, _remote, c) = contained_candidate(|mock, c| {
        mock.reverted = vec![reverted_landing(c)];
        mock.revise_refusal = Some("not_reverted_in_history".into());
    })
    .await;
    let blocked = h.cycle().await;
    assert!(
        matches!(&blocked, Step::Blocked(r) if r.starts_with("not_reverted_in_history")),
        "{blocked:?}"
    );
    assert_eq!(report_kinds(&h), ["fix_target"]);
    let details = report_details(&h, 0);
    assert_eq!(details["verdict"], "not_reverted_in_history");
    assert_eq!(
        (
            details["blocks_subject"].clone(),
            details["commits"].clone()
        ),
        (json!(true), json!([c]))
    );
    assert!(h.peek(|m| m.results.is_empty()));
}

/// A `reverts` entry `rt1` for R `r` (computed on `t0`, candidate `r`) on
/// `remote`'s `main`, as if the service had published it.
fn revert_entry(remote: &Remote, r: &str, t0: &str) -> Value {
    let target = json!({"submission_id": "s0", "result_id": "res0",
        "original_task_id": "t0", "r": r, "t0": t0, "c": r, "landing_range": [r],
        "reason": "human", "evidence": null});
    json!({"id": "rt1", "task_id": "rt1", "result_id": "res0", "r": r, "title": "Revert",
        "priority": 0, "repository_url": remote.url, "target_branch": "main", "target": target})
}

#[tokio::test]
async fn a_refused_revert_is_skipped_and_items_still_integrate() {
    let (remote, base) = seeded_remote();
    let t0 = advance_main(&remote, "a.txt");
    let r = advance_main(&remote, "b.txt");
    let c = push_candidate(&remote, &base, "feature.txt", "feature\n");
    let mut h = Harness::new(item(&remote, &base, &c)).await;
    h.watch(&remote);
    h.checks(None, &RULES);
    {
        let mut mock = h.mock.lock().unwrap();
        mock.reverts = vec![revert_entry(&remote, &r, &t0)];
        mock.revert_refusal = Some("workflow_policy_required".into());
    }
    assert_eq!(h.cycle().await, Step::ChecksPending, "the item is reached");
    assert_eq!(h.peek(|m| m.results.len()), 1);
    assert!(h.peek(|m| m.candidates.is_empty() && m.reverts.len() == 1));
}

/// Runs the cycle that reports revert `rt1` not mechanical and returns the
/// report's evidence; asserts the mock holds no candidate.
async fn unmechanical_evidence(h: &mut Harness) -> String {
    assert_eq!(h.cycle().await, Step::NotMechanical("conflict".into()));
    assert!(h.peek(|m| m.candidates.is_empty()), "no empty candidate");
    let report = h.peek(|m| m.not_mechanical[0].clone());
    report["evidence"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn a_revert_of_a_result_outside_the_history_is_not_mechanical() {
    let (remote, base) = seeded_remote();
    let r = push_candidate(&remote, &base, "never.txt", "never landed\n");
    let mut h = Harness::idle(&remote).await;
    h.checks(None, &RULES);
    h.mock.lock().unwrap().reverts = vec![revert_entry(&remote, &r, &base)];
    let evidence = unmechanical_evidence(&mut h).await;
    assert!(
        evidence.contains("is not in the target history"),
        "{evidence}"
    );
}

#[tokio::test]
async fn a_revert_already_undone_on_the_tip_is_not_mechanical() {
    let (remote, base) = seeded_remote();
    let c = push_candidate(&remote, &base, "feature.txt", "feature\n");
    let mut h = published_then_reverted(&remote, &base, &c).await;
    let pull = ["pull", "--quiet", "--ff-only", "origin", "main"];
    git(&remote.source, &pull);
    git(&remote.source, &["rm", "--quiet", "feature.txt"]);
    git(&remote.source, &["commit", "--quiet", "-m", "Undo by hand"]);
    git(&remote.source, &["push", "--quiet", "origin", "main"]);
    let evidence = unmechanical_evidence(&mut h).await;
    assert!(evidence.contains("is already undone on"), "{evidence}");
    assert_eq!(h.cycle().await, Step::Idle, "nothing was published");
}
