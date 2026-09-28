//! End-to-end cycles against a real local bare remote, file-backed checks and
//! an in-process mock of the coordinator's integrator routes (same paths,
//! envelopes, field names, idempotency-key semantics, held-authority rule and
//! first-write-wins reports as the server; the server's own rules are covered by
//! `crates/server/tests/workflow.rs`).
use crate::checks::FakeChecks;
use crate::config::{ChecksKind, Config};
use crate::git::testing::{Remote, commit, git, remote};
use crate::integrate::{Integrator, Step};
use crate::service::Service;
use crate::state::LoopState;
use axum::extract::State;
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

/// What the mock service has been told, and the one queued subject.
#[derive(Default)]
struct Mock {
    item: Option<Value>,
    results: Vec<Value>,
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
    let error = json!({"code": code, "message": code, "details": {}});
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
        item["results"] = json!(mock.results);
        item
    };
    let items: Vec<Value> = mock.item.iter().map(with_results).collect();
    let roster = json!({"revision": 1, "required_checks": [{"identity": "tests"}]});
    ok(
        json!({"project_id": "p", "roster": roster, "items": items, "skipped_ineligible": 0, "retry_after_seconds": 30}),
    )
}

async fn results(State(mock): State<Shared>, headers: HeaderMap, Json(body): Json<Value>) -> Reply {
    idempotent(&mock, &headers, body, |mock, body| {
        if let Some(existing) = mock.results.iter().find(|r| r["t0"] == body["t0"]) {
            return ok(existing.clone());
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

async fn revise(State(mock): State<Shared>, headers: HeaderMap, Json(body): Json<Value>) -> Reply {
    idempotent(&mock, &headers, body, |mock, body| {
        mock.item = None;
        mock.revises.push(body.clone());
        ok(json!({"revise": body}))
    })
}

/// Stores the first report per (kind, dedupe_key) and returns the stored row.
async fn reports(State(mock): State<Shared>, headers: HeaderMap, Json(body): Json<Value>) -> Reply {
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
    git(
        &remote.source,
        &["checkout", "--quiet", "-B", "candidate", base],
    );
    let c = commit(&remote.source, file, content);
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
    let sha = commit(&remote.source, file, "x\n");
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
    /// Starts the mock with `item` queued and builds the integrator.
    async fn new(item: Value) -> Self {
        let mock: Shared = Arc::new(Mutex::new(Mock {
            item: Some(item),
            ..Mock::default()
        }));
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
    h.checks(Some("failure"), &RULES);
    assert_eq!(h.cycle().await, Step::Revised("check_failed".into()));
    assert_eq!(
        h.peek(|m| m.revises[1]["reason_code"].clone()),
        "check_failed"
    );
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
