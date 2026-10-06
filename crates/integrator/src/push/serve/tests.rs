use super::*;
use crate::budget::MINT_BURST;
use crate::socket::bind;
use agentc_integrator::config::GithubConfig;
use aws_lc_rs::encoding::AsDer;
use aws_lc_rs::rsa::{KeyPair, KeySize};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{delete, post};
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use coordinator_local::candidate_push::send_candidate;
use std::fs;
use std::io::{BufRead, BufReader};
use std::net::Shutdown;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use tempfile::TempDir;

const REFERENCE: &str = "refs/agent-coordinator/candidates/task-1/launch-1";

/// What the mock GitHub saw and how it answers.
#[derive(Default)]
struct Record {
    mint_bodies: Vec<Value>,
    mint_bearers: Vec<String>,
    revoked: Vec<String>,
    fail_mint: bool,
}

type Shared = Arc<Mutex<Record>>;

/// The bearer credential of a request, or an empty string.
fn bearer(headers: &HeaderMap) -> String {
    let value = headers.get("authorization").and_then(|v| v.to_str().ok());
    value
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default()
        .to_owned()
}

/// `POST /app/installations/{id}/access_tokens`: records the request and
/// issues `ghs_minted_<n>`, or fails when told to.
async fn mint(
    State(record): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let mut record = record.lock().unwrap();
    if record.fail_mint {
        return (StatusCode::FORBIDDEN, Json(json!({"message": "nope"})));
    }
    record.mint_bodies.push(body);
    record.mint_bearers.push(bearer(&headers));
    let token = format!("ghs_minted_{}", record.mint_bodies.len());
    (StatusCode::CREATED, Json(json!({"token": token})))
}

/// `DELETE /installation/token`: records the revoked bearer token.
async fn revoke(State(record): State<Shared>, headers: HeaderMap) -> StatusCode {
    record.lock().unwrap().revoked.push(bearer(&headers));
    StatusCode::NO_CONTENT
}

/// Starts the mock GitHub API on a loopback port in its own thread.
fn start_github(record: Shared) -> String {
    let app = Router::new()
        .route("/app/installations/{id}/access_tokens", post(mint))
        .route("/installation/token", delete(revoke))
        .with_state(record);
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            sender.send(listener.local_addr().unwrap()).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });
    format!("http://{}", receiver.recv().unwrap())
}

/// Push App credentials against the mock, with a fresh key in `directory`.
fn github_credentials(directory: &Path, record: Shared) -> GithubCredentials {
    let der = KeyPair::generate(KeySize::Rsa2048)
        .unwrap()
        .as_der()
        .unwrap();
    let pem = format!(
        "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
        STANDARD.encode(der.as_ref())
    );
    let private_key = directory.join("app.pem");
    fs::write(&private_key, pem).unwrap();
    let config = GithubConfig {
        api_base: start_github(record),
        app_id: 7,
        installation_id: 9,
        private_key,
    };
    GithubCredentials::new(GithubApp::new(&config).unwrap(), "repo")
}

/// Credentials that issue `fake-token-<n>` and record mints and
/// revocations.
#[derive(Clone, Default)]
struct FakeCredentials {
    minted: Arc<Mutex<Vec<String>>>,
    revoked: Arc<Mutex<Vec<String>>>,
}

impl Credentials for FakeCredentials {
    /// Issues and records the next fake token.
    async fn mint(&self) -> Result<String> {
        let mut minted = self.minted.lock().unwrap();
        let token = format!("fake-token-{}", minted.len());
        minted.push(token.clone());
        Ok(token)
    }

    /// Records `token` as revoked.
    async fn revoke(&self, token: &str) -> Result<()> {
        self.revoked.lock().unwrap().push(token.to_owned());
        Ok(())
    }
}

/// Runs Git in `directory` with a fixed identity, panicking on failure.
fn git(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
        ])
        .args(arguments)
        .current_dir(directory)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {arguments:?} failed");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// A bare remote, an implementer clone whose `main` it holds, the base
/// commit, and room for the helper's files.
struct Fixture {
    directory: TempDir,
    remote: PathBuf,
    clone: PathBuf,
    base: String,
}

/// Creates the remote and a clone with one published `base` commit.
fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let remote = directory.path().join("remote.git");
    let clone = directory.path().join("clone");
    git(
        directory.path(),
        &["init", "--quiet", "--bare", remote.to_str().unwrap()],
    );
    git(
        directory.path(),
        &[
            "clone",
            "--quiet",
            remote.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    let fixture = Fixture {
        directory,
        remote,
        clone,
        base: String::new(),
    };
    let base = fixture.commit("base.txt");
    git(
        &fixture.clone,
        &["push", "--quiet", "origin", "HEAD:refs/heads/main"],
    );
    Fixture { base, ..fixture }
}

impl Fixture {
    /// Commits a file named `name` in the clone and returns the commit.
    fn commit(&self, name: &str) -> String {
        fs::write(self.clone.join(name), name).unwrap();
        git(&self.clone, &["add", "--", name]);
        git(&self.clone, &["commit", "--quiet", "-m", name]);
        git(&self.clone, &["rev-parse", "HEAD"])
    }

    /// The helper spec for task `task-1`, launch `launch-1`.
    fn spec(&self) -> HelperSpec {
        let work_dir = self.directory.path().join("helper");
        HelperSpec::new(
            self.remote.to_str().unwrap(),
            "task-1",
            "launch-1",
            &work_dir,
        )
        .unwrap()
    }

    /// The commit the remote holds at the candidate ref, if any.
    fn candidate(&self) -> Option<String> {
        let listing = git(
            &self.remote,
            &["for-each-ref", "--format=%(objectname)", REFERENCE],
        );
        (!listing.is_empty()).then_some(listing)
    }

    /// The socket path the helper binds.
    fn socket(&self) -> PathBuf {
        self.directory.path().join("push.sock")
    }

    /// Connects to the helper with a generous client-side timeout.
    fn connect(&self) -> UnixStream {
        let stream = UnixStream::connect(self.socket()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        stream
    }

    /// Pushes `revision` through the helper, excluding the base.
    fn push(&self, revision: &str) -> Result<String> {
        let mut stream = self.connect();
        let receipt = send_candidate(&mut stream, &self.clone, revision, &[&self.base])?;
        Ok(receipt.revision)
    }
}

/// A helper serving on its own thread until stopped.
struct Running {
    stop: tokio::sync::oneshot::Sender<()>,
    thread: JoinHandle<Result<()>>,
}

/// Binds the fixture's socket and serves it with `credentials`, `timeout`
/// and a budget of `burst` mints that does not refill during a test.
fn start<C>(fixture: &Fixture, credentials: C, timeout: Duration, burst: u32) -> Running
where
    C: Credentials + Send + 'static,
{
    start_for_launch(fixture, credentials, timeout, burst, std::process::id())
}

/// As [`start`], serving only descendants of process `launch`.
fn start_for_launch<C>(
    fixture: &Fixture,
    credentials: C,
    timeout: Duration,
    burst: u32,
    launch: u32,
) -> Running
where
    C: Credentials + Send + 'static,
{
    let socket = bind(&fixture.socket()).unwrap();
    let hour = Duration::from_secs(3600);
    let server = Server {
        socket,
        spec: fixture.spec(),
        max_bundle_bytes: 1024 * 1024,
        credentials,
        budget: Mutex::new(MintBudget::new(burst, hour, Instant::now())),
        askpass: PathBuf::from("/nonexistent/agentc-push"),
        timeout,
        launch,
    };
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(server.run(async move { drop(stopped.await) }))
    });
    Running { stop, thread }
}

impl Running {
    /// Requests shutdown and waits for the loop to return.
    fn stop(self) {
        self.stop.send(()).unwrap();
        self.thread.join().unwrap().unwrap();
    }
}

/// Sends `request` raw and returns the helper's parsed reply.
fn exchange_raw(fixture: &Fixture, request: &[u8]) -> PushReply {
    let mut stream = fixture.connect();
    stream.write_all(request).unwrap();
    stream.shutdown(Shutdown::Write).unwrap();
    read_reply(stream)
}

/// A well-formed request for made-up object IDs, followed by a 4-byte
/// bundle that is not a bundle: worth a mint, refused as `bundle_invalid`.
fn junk_bundle_request() -> Vec<u8> {
    let oid = "a".repeat(40);
    let line = format!("{{\"version\":1,\"revision\":\"{oid}\",\"tree\":\"{oid}\"}}\n");
    [line.as_bytes(), &4_u64.to_be_bytes(), b"junk"].concat()
}

/// Reads and parses one reply line from `stream`.
fn read_reply(stream: UnixStream) -> PushReply {
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    PushReply::parse(line.trim_end().as_bytes()).unwrap()
}

/// The refusal of `reply`, panicking on an acceptance.
fn refusal(reply: PushReply) -> PushRefusal {
    match reply {
        PushReply::Refused(refusal) => refusal,
        PushReply::Accepted(receipt) => panic!("unexpectedly accepted: {receipt:?}"),
    }
}

/// The refusal code of `reply`, panicking on an acceptance.
fn refused_code(reply: PushReply) -> RefusalCode {
    refusal(reply).code
}

#[test]
fn token_scope_names_one_repository_with_contents_write_only() {
    assert_eq!(
        token_scope("agent_coordinator"),
        json!({"repositories": ["agent_coordinator"], "permissions": {"contents": "write"}})
    );
}

#[test]
fn pushes_end_to_end_and_revokes_the_token_after_acceptance_and_refusal() {
    let fixture = fixture();
    let record = Shared::default();
    let credentials = github_credentials(fixture.directory.path(), record.clone());
    let running = start(&fixture, credentials, IO_TIMEOUT, MINT_BURST);
    let refused = exchange_raw(&fixture, &junk_bundle_request());
    assert_eq!(refused_code(refused), RefusalCode::BundleInvalid);
    let candidate = fixture.commit("candidate.txt");
    assert_eq!(fixture.push(&candidate).unwrap(), candidate);
    assert_eq!(fixture.candidate().as_deref(), Some(candidate.as_str()));
    running.stop();
    let record = record.lock().unwrap();
    let scope = json!({"repositories": ["repo"], "permissions": {"contents": "write"}});
    assert_eq!(record.mint_bodies, [scope.clone(), scope]);
    let jwt_shaped = |jwt: &String| jwt.split('.').count() == 3;
    assert!(record.mint_bearers.iter().all(jwt_shaped));
    assert_eq!(record.revoked, ["ghs_minted_1", "ghs_minted_2"]);
    assert!(!fixture.socket().exists(), "socket left behind");
}

#[test]
fn empty_and_garbage_connections_mint_nothing() {
    let fixture = fixture();
    let record = Shared::default();
    let credentials = github_credentials(fixture.directory.path(), record.clone());
    let running = start(&fixture, credentials, IO_TIMEOUT, MINT_BURST);
    for _ in 0..5 {
        drop(fixture.connect());
    }
    let empty = exchange_raw(&fixture, b"");
    assert_eq!(refused_code(empty), RefusalCode::BadRequest);
    let garbage = exchange_raw(&fixture, b"not a request\n\0\0\0\0\0\0\0\x04junk");
    assert_eq!(refused_code(garbage), RefusalCode::BadRequest);
    running.stop();
    let record = record.lock().unwrap();
    assert!(record.mint_bodies.is_empty(), "{:?}", record.mint_bodies);
    assert!(record.revoked.is_empty());
}

#[test]
fn mints_beyond_the_budget_are_refused_without_minting() {
    let fixture = fixture();
    let credentials = FakeCredentials::default();
    let running = start(&fixture, credentials.clone(), IO_TIMEOUT, 2);
    for _ in 0..2 {
        let reply = exchange_raw(&fixture, &junk_bundle_request());
        assert_eq!(refused_code(reply), RefusalCode::BundleInvalid);
    }
    let limited = refusal(exchange_raw(&fixture, &junk_bundle_request()));
    assert_eq!(limited.code, RefusalCode::Internal);
    assert_eq!(limited.message, RATE_LIMITED);
    running.stop();
    assert_eq!(
        *credentials.minted.lock().unwrap(),
        ["fake-token-0", "fake-token-1"]
    );
    assert_eq!(
        *credentials.revoked.lock().unwrap(),
        ["fake-token-0", "fake-token-1"]
    );
}

#[test]
fn a_client_outside_the_launch_is_refused_without_minting() {
    let fixture = fixture();
    let credentials = FakeCredentials::default();
    let mut other = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let running = start_for_launch(
        &fixture,
        credentials.clone(),
        IO_TIMEOUT,
        MINT_BURST,
        other.id(),
    );
    let reply = refusal(exchange_raw(&fixture, &junk_bundle_request()));
    assert_eq!(reply.code, RefusalCode::Internal);
    assert_eq!(reply.message, FOREIGN_LAUNCH);
    running.stop();
    other.kill().unwrap();
    other.wait().unwrap();
    assert!(credentials.minted.lock().unwrap().is_empty());
    assert_eq!(fixture.candidate(), None);
}

#[test]
fn a_failed_mint_refuses_the_client_and_revokes_nothing() {
    let fixture = fixture();
    let record = Shared::default();
    record.lock().unwrap().fail_mint = true;
    let credentials = github_credentials(fixture.directory.path(), record.clone());
    let running = start(&fixture, credentials, IO_TIMEOUT, MINT_BURST);
    let reply = refusal(exchange_raw(&fixture, &junk_bundle_request()));
    assert_eq!(reply.code, RefusalCode::Internal);
    assert_eq!(reply.message, NO_CREDENTIALS);
    running.stop();
    assert!(record.lock().unwrap().revoked.is_empty());
    assert_eq!(fixture.candidate(), None);
}

#[test]
fn a_stalled_client_times_out_unminted_and_the_next_one_is_served() {
    let fixture = fixture();
    let credentials = FakeCredentials::default();
    let running = start(
        &fixture,
        credentials.clone(),
        Duration::from_millis(300),
        MINT_BURST,
    );
    let stalled = fixture.connect();
    assert_eq!(refused_code(read_reply(stalled)), RefusalCode::BadRequest);
    let candidate = fixture.commit("after-stall.txt");
    assert_eq!(fixture.push(&candidate).unwrap(), candidate);
    running.stop();
    assert_eq!(*credentials.minted.lock().unwrap(), ["fake-token-0"]);
    assert_eq!(*credentials.revoked.lock().unwrap(), ["fake-token-0"]);
}

#[test]
fn outcome_lines_name_the_code_and_revisions() {
    let refused = Ok(PushReply::Refused(PushRefusal {
        code: RefusalCode::LeaseConflict,
        message: "m".into(),
    }));
    assert_eq!(
        outcome_line(REFERENCE, &refused),
        format!("agentc-push: request reference={REFERENCE} outcome=lease_conflict")
    );
    let failed = Err(anyhow::anyhow!("broken pipe"));
    assert!(
        outcome_line(REFERENCE, &failed).ends_with("outcome=connection_failed error=broken pipe")
    );
}
