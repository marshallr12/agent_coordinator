//! A launch's broker pipe with real AppContainer launches (phase 6, step
//! 6a).
//!
//! Each test runs in a new logon session of the same account, where the
//! broker and the launches run, and the broker's client is this test
//! program run again inside a launch ([`support`]). This is a development
//! check of the mechanism, not acceptance evidence.
//!
//! The refusals that need no launch (a token without the package, the
//! remote form of the pipe's path) and the generation of a launch opened
//! again are the `broker` module's own tests.
#![cfg(windows)]

mod support;

use agentc_supervisor::host_runtime::windows::broker::{
    Broker, Grant, Halt, Handler, Role, pipe_name,
};
use agentc_supervisor::host_runtime::windows::launcher::Launcher;
use agentc_supervisor::host_runtime::windows::pipe::{Line, Received, Session};
use agentc_supervisor::host_runtime::windows::token;
use agentc_supervisor::service::pipe_policy::{CLIENT_ACCESS, pipe_path};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use support::{
    Launch, Trees, child_main, client_tools, finish, in_a_new_session, outcome, prepare,
    start_client, wait_for,
};

/// The pipes the client in a launch visits, as `label=name;label=name`.
const PIPES: &str = "AGENTC_BROKER_TEST_PIPES";
/// Set when the client keeps its session open until the broker ends it.
const HOLD: &str = "AGENTC_BROKER_TEST_HOLD";
/// The request the client sends: it claims to be another launch, in
/// another role and generation.
const FORGED: &str = r#"{"launch":"forged-launch","role":"implementer","generation":999}"#;
/// The broker service the tests run.
const SERVICE: &str = "test";

/// A launch's client sends a request that claims another launch, role and
/// generation; the broker's handler answers with the grant its registry
/// holds for the client's package, and the claim changes nothing.
#[test]
fn a_launch_is_served_its_own_grant_whatever_its_request_claims() {
    in_a_new_session("served");
}

/// A launch with another package cannot open the first launch's broker
/// pipe, while it can open its own.
#[test]
fn another_launch_cannot_open_a_launchs_broker_pipe() {
    in_a_new_session("foreign");
}

/// Closing a launch ends its session that is still going, at once, and
/// its pipe's name is gone afterwards.
#[test]
fn closing_a_launch_ends_its_ongoing_session_and_removes_its_pipe() {
    in_a_new_session("closed");
}

/// The child's side. Ignored so that it runs only when a test above starts
/// it, with its role, in a new logon session or inside a launch.
#[test]
#[ignore = "started by the tests above in a new logon session"]
fn child_in_a_new_logon_session() {
    child_main(|role| match role {
        "served" => served(),
        "foreign" => foreign(),
        "closed" => closed(),
        "client" => client(),
        other => panic!("no such role: {other}"),
    });
}

/// The broker's handler in these tests: it answers one request line with
/// the grant the broker hands it and the request it reads, then holds the
/// session until the client leaves or the pipe closes.
#[derive(Default)]
struct Echo {
    /// How many requests it answered.
    answered: AtomicUsize,
}

impl Handler for Echo {
    /// Answers with the grant and the claim, then holds the session.
    fn serve(&self, grant: &Grant, session: &mut Session<'_>, halt: &Halt) -> anyhow::Result<()> {
        let Line::Request(claim) = session.read_line(Duration::from_secs(10))? else {
            anyhow::bail!("an admitted client sent no whole request");
        };
        let answer = json!({"launch": grant.launch(), "role": grant.role(),
            "generation": grant.generation(), "package": grant.package(),
            "claimed": String::from_utf8_lossy(&claim)});
        session.write_all(format!("{answer}\n").as_bytes())?;
        self.answered.fetch_add(1, Ordering::SeqCst);
        while !halt.asked() {
            if session.read(1024, Duration::from_millis(500))? == Received::Closed {
                break;
            }
        }
        Ok(())
    }
}

/// The client inside a launch: visits every pipe [`PIPES`] names and
/// reports one line per pipe.
fn client() -> String {
    let pipes = std::env::var(PIPES).unwrap();
    let hold = std::env::var_os(HOLD).is_some();
    let visit = |pair: &str| {
        let (label, name) = pair.split_once('=').unwrap();
        format!("{label} = {}", visit(name, hold))
    };
    pipes.split(';').map(visit).collect::<Vec<_>>().join("\n")
}

/// Opens the pipe `name` with a client's rights, sends [`FORGED`] and reads
/// the answer; with `hold`, then reads on until the broker ends the session.
fn visit(name: &str, hold: bool) -> String {
    use std::os::windows::fs::OpenOptionsExt;
    let mut options = std::fs::OpenOptions::new();
    options.access_mode(CLIENT_ACCESS);
    let mut file = match options.open(pipe_path(name).unwrap()) {
        Ok(file) => file,
        Err(error) => return format!("error {}", error.raw_os_error().unwrap_or(-1)),
    };
    file.write_all(format!("{FORGED}\n").as_bytes()).unwrap();
    let mut reader = BufReader::new(file);
    let mut answer = String::new();
    reader.read_line(&mut answer).unwrap();
    if !hold {
        return format!("answer {}", answer.trim());
    }
    let mut rest = Vec::new();
    let ended = match reader.read_to_end(&mut rest) {
        Ok(_) => "closed".to_owned(),
        Err(error) => format!("broken {}", error.raw_os_error().unwrap_or(-1)),
    };
    format!("answer {} then {ended}", answer.trim())
}

/// Starts the client in `launch`, visiting `pipes` (label and pipe name);
/// with `hold` it keeps its first session open. Returns its report's path.
fn start_visits(
    launcher: &mut Launcher,
    launch: &Launch,
    tools: &std::path::Path,
    (pipes, hold): (&[(&str, String)], bool),
) -> PathBuf {
    let listed: Vec<String> = pipes.iter().map(|(l, n)| format!("{l}={n}")).collect();
    let mut extra = vec![(PIPES, listed.join(";"))];
    if hold {
        extra.push((HOLD, "1".to_owned()));
    }
    start_client(launcher, launch, tools, extra)
}

/// The answer an outcome carries, as JSON.
fn answer_in(outcome: &str) -> Value {
    let text = outcome
        .strip_prefix("answer ")
        .unwrap_or_else(|| panic!("{outcome}"));
    let text = text.split(" then ").next().unwrap();
    serde_json::from_str(text).unwrap_or_else(|error| panic!("{error}: {outcome}"))
}

/// Checks that `answer` is `launch`'s registry grant as a reviewer of
/// `generation`, and that the handler read the forged claim.
fn assert_grant(answer: &Value, launch: &Launch, generation: u64) {
    assert_eq!(answer["launch"], launch.id.as_str(), "{answer}");
    assert_eq!(answer["role"], "reviewer", "{answer}");
    assert_eq!(answer["generation"], generation, "{answer}");
    assert_eq!(answer["package"], launch.package.as_str(), "{answer}");
    assert_eq!(answer["claimed"], FORGED, "{answer}");
}

/// A broker running as this account whose sessions go to `echo`.
fn broker(echo: &Arc<Echo>) -> Broker {
    Broker::new(SERVICE, &token::current_user_sid().unwrap(), echo.clone()).unwrap()
}

/// Opens `launch` in `broker` as a reviewer asking for `generation`.
fn open(broker: &mut Broker, launch: &Launch, generation: u64) -> Arc<Grant> {
    let me = token::current_user_sid().unwrap();
    let grant = Grant::new(&launch.id, Role::Reviewer, &me, &launch.package, generation);
    broker.open(grant.unwrap()).unwrap()
}

/// Ends `launches` and their pipes.
fn clean(launcher: &mut Launcher, broker: &mut Broker, launches: &[&Launch]) {
    for launch in launches {
        broker.close(&launch.id).unwrap();
        launcher.cleanup(&launch.id).unwrap();
    }
}

/// Test (a): the forged request reaches the handler and the answer is the
/// registry's grant.
fn served() -> String {
    let dir = tempfile::tempdir().unwrap();
    let (trees, mut launcher, echo) = (Trees::new(&dir), Launcher::new().unwrap(), Arc::default());
    let mut broker = broker(&echo);
    let own = prepare(&mut launcher, &trees, "brk-served");
    let granted = open(&mut broker, &own, 7);
    let tools = client_tools(&trees, &[&own.package]);
    let pipes = [("own", pipe_name(SERVICE, &own.id))];
    let report = start_visits(&mut launcher, &own, &tools, (&pipes, false));
    let lines = finish(&mut launcher, &own, &report);
    assert_grant(
        &answer_in(outcome(&lines, "own")),
        &own,
        granted.generation(),
    );
    assert_eq!(echo.answered.load(Ordering::SeqCst), 1);
    clean(&mut launcher, &mut broker, &[&own]);
    lines.join("; ")
}

/// Test (b): another launch's package is denied the pipe by its access
/// list, while that launch's own pipe serves it.
fn foreign() -> String {
    let dir = tempfile::tempdir().unwrap();
    let (trees, mut launcher, echo) = (Trees::new(&dir), Launcher::new().unwrap(), Arc::default());
    let mut broker = broker(&echo);
    let first = prepare(&mut launcher, &trees, "brk-first");
    let other = prepare(&mut launcher, &trees, "brk-other");
    open(&mut broker, &first, 1);
    open(&mut broker, &other, 1);
    let tools = client_tools(&trees, &[&first.package, &other.package]);
    let pipes = [
        ("own", pipe_name(SERVICE, &other.id)),
        ("foreign", pipe_name(SERVICE, &first.id)),
    ];
    let report = start_visits(&mut launcher, &other, &tools, (&pipes, false));
    let lines = finish(&mut launcher, &other, &report);
    assert_grant(&answer_in(outcome(&lines, "own")), &other, 1);
    assert_eq!(outcome(&lines, "foreign"), "error 5", "{lines:?}");
    assert_eq!(
        echo.answered.load(Ordering::SeqCst),
        1,
        "the first launch served nobody"
    );
    clean(&mut launcher, &mut broker, &[&first, &other]);
    lines.join("; ")
}

/// Test (e): the client holds its session; closing the launch ends it at
/// once and removes the pipe's name.
fn closed() -> String {
    let dir = tempfile::tempdir().unwrap();
    let (trees, mut launcher, echo) = (Trees::new(&dir), Launcher::new().unwrap(), Arc::default());
    let mut broker = broker(&echo);
    let own = prepare(&mut launcher, &trees, "brk-closed");
    open(&mut broker, &own, 3);
    let tools = client_tools(&trees, &[&own.package]);
    let name = pipe_name(SERVICE, &own.id);
    let pipes = [("own", name.clone())];
    let report = start_visits(&mut launcher, &own, &tools, (&pipes, true));
    wait_for(
        || echo.answered.load(Ordering::SeqCst) == 1,
        "the client's answer",
    );
    let started = Instant::now();
    assert!(broker.close(&own.id).unwrap());
    let took = started.elapsed();
    // Below the serving threads' 500 ms tick, so the close must come from
    // the halt event waking them, not from their next poll.
    assert!(took < Duration::from_millis(250), "closing took {took:?}");
    let gone = std::fs::File::open(pipe_path(&name).unwrap()).unwrap_err();
    assert_eq!(gone.raw_os_error(), Some(2), "{gone}");
    let lines = finish(&mut launcher, &own, &report);
    let said = outcome(&lines, "own");
    assert_grant(&answer_in(said), &own, 3);
    assert!(
        said.ends_with(" then closed") || said.contains(" then broken"),
        "{said}"
    );
    launcher.cleanup(&own.id).unwrap();
    format!("closed in {took:?}; {}", lines.join("; "))
}
