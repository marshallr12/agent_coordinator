//! The egress broker with real AppContainer launches (phase 6, step 6b).
//!
//! Each test runs in a new logon session of the same account ([`support`]).
//! There the broker serves its control pipe, the test opens each launch
//! through that pipe as the runner would, and the broker's client is this
//! test program run again inside the launch. The broker's destination is a
//! listener of the test on loopback, which the broker reaches and the
//! launch must not: the same listener is the positive control for the
//! direct attempts. This is a development check of the mechanism, not
//! acceptance evidence, and it contacts nothing off the host.
#![cfg(windows)]

mod support;

use agentc_supervisor::host_runtime::windows::broker::{Broker, Role};
use agentc_supervisor::host_runtime::windows::broker_control::{
    Control, ControlClient, Opened, control_pipe_name,
};
use agentc_supervisor::host_runtime::windows::egress_broker::{
    Egress, PIPE_VARIABLE, SERVICE, Upstream, connect_first,
};
use agentc_supervisor::host_runtime::windows::launcher::Launcher;
use agentc_supervisor::host_runtime::windows::token;
use agentc_supervisor::service::pipe_policy::{CLIENT_ACCESS, pipe_path};
use std::fs::File;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::os::windows::fs::OpenOptionsExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use support::{
    Launch, Trees, child_main, client_tools, finish, in_a_new_session, outcome, prepare,
    start_client, wait_for,
};

/// Which probe the client in a launch runs.
const PROBE: &str = "AGENTC_EGRESS_TEST_PROBE";
/// The test's TCP listener on IPv4 loopback, which the broker reaches.
const ECHO_V4: &str = "AGENTC_EGRESS_TEST_ECHO_V4";
/// The same on IPv6 loopback, when the host has one.
const ECHO_V6: &str = "AGENTC_EGRESS_TEST_ECHO_V6";
/// The test's UDP receiver on IPv4 loopback.
const UDP: &str = "AGENTC_EGRESS_TEST_UDP";
/// A public name the host itself resolves, for the launch to try.
const DNS_NAME: &str = "AGENTC_EGRESS_TEST_DNS";
/// The address a loopback peer connects to.
const PEER: &str = "AGENTC_EGRESS_TEST_PEER";
/// The only name the test broker allows.
const ALLOWED: &str = "allowed.test";
/// How long a direct attempt from inside a launch may take.
const ATTEMPT: Duration = Duration::from_secs(5);

/// An allowed CONNECT is tunnelled to the destination; a host not on the
/// allowlist, another port and a request that is not a CONNECT get 403;
/// direct TCP over IPv4 and IPv6, UDP and name resolution fail from inside
/// the launch while the host reaches the same listeners.
#[test]
fn an_allowed_route_works_through_the_broker_and_nothing_else_leaves_the_launch() {
    in_a_new_session("routed");
}

/// Closing a launch through the control pipe ends its open tunnel at once
/// and its pipe is gone.
#[test]
fn closing_a_launch_ends_its_tunnel_and_its_pipe() {
    in_a_new_session("lost");
}

/// Records whether a launch reaches a loopback listener of its own
/// AppContainer package, in one process and from a second one. Nothing is
/// asserted about the outcome: it is the probe the owner asked for.
#[test]
fn a_launch_listening_on_loopback_is_probed_from_its_own_package() {
    in_a_new_session("loopback");
}

/// The runner refuses to start a launch while its egress broker is not
/// there, starts it with its pipe once the broker admits it, keeps a
/// release the broker missed during cleanup and makes it before the next
/// admission.
#[test]
fn the_runner_starts_a_launch_only_once_the_broker_admits_it() {
    in_a_new_session("runner");
}

/// The child's side. Ignored so that it runs only when a test above starts
/// it, with its role, in a new logon session or inside a launch.
#[test]
#[ignore = "started by the tests above in a new logon session"]
fn child_in_a_new_logon_session() {
    child_main(|role| match role {
        "routed" => routed(),
        "lost" => lost(),
        "loopback" => loopback(),
        "runner" => runner::admitted_starts(),
        "client" => client(),
        "peer" => peer(),
        other => panic!("no such role: {other}"),
    });
}

// ---- The host's side, in the new logon session ----

/// Sends [`ALLOWED`] to the test's listener and counts the connections.
struct ToListener {
    address: SocketAddr,
    connected: AtomicUsize,
}

impl ToListener {
    /// An upstream to `listener`.
    fn to(listener: &Listener) -> Arc<Self> {
        let (address, connected) = (listener.address, AtomicUsize::new(0));
        Arc::new(Self { address, connected })
    }
}

impl Upstream for ToListener {
    /// Connects [`ALLOWED`] to the listener and counts it; reaches nothing
    /// for any other name.
    fn connect(&self, host: &str) -> anyhow::Result<Option<TcpStream>> {
        if host != ALLOWED {
            return Ok(None);
        }
        let stream = connect_first(&[self.address])?;
        self.connected.fetch_add(1, Ordering::SeqCst);
        Ok(stream)
    }
}

/// A TCP listener of the test that sends back what it gets.
struct Listener {
    address: SocketAddr,
    /// How many connections reached it.
    connections: Arc<AtomicUsize>,
    /// How many bytes it sent back.
    echoed: Arc<AtomicUsize>,
}

impl Listener {
    /// Listens on `address`; `None` when it cannot be bound.
    fn bind(address: &str) -> Option<Self> {
        let listener = TcpListener::bind(address).ok()?;
        let address = listener.local_addr().unwrap();
        let connections: Arc<AtomicUsize> = Arc::default();
        let echoed: Arc<AtomicUsize> = Arc::default();
        let (counted, sent) = (Arc::clone(&connections), Arc::clone(&echoed));
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                counted.fetch_add(1, Ordering::SeqCst);
                let sent = Arc::clone(&sent);
                std::thread::spawn(move || echo_back(stream, &sent));
            }
        });
        Some(Self {
            address,
            connections,
            echoed,
        })
    }

    /// How many connections reached it so far.
    fn reached(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}

/// Sends back what `stream` sends until it ends, counting into `sent`.
fn echo_back(mut stream: TcpStream, sent: &AtomicUsize) {
    let mut buffer = [0_u8; 4096];
    while let Ok(count @ 1..) = stream.read(&mut buffer) {
        if stream.write_all(&buffer[..count]).is_err() {
            break;
        }
        sent.fetch_add(count, Ordering::SeqCst);
    }
}

/// The broker's control pipe served on a thread until it is stopped.
struct Served {
    name: String,
    done: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Served {
    /// Serves a broker running as this account, its sessions going to an
    /// egress handler that allows [`ALLOWED`] through `upstream`.
    fn start(upstream: Arc<ToListener>) -> Self {
        let id = uuid::Uuid::new_v4().simple();
        Self::named(&format!("{}-{id}", control_pipe_name("test")), upstream)
    }

    /// As [`Served::start`] on the control pipe `name`.
    fn named(name: &str, upstream: Arc<ToListener>) -> Self {
        let (me, name) = (token::current_user_sid().unwrap(), name.to_owned());
        let egress = Egress::new(vec![ALLOWED.into()], upstream);
        let broker = Broker::new(SERVICE, &me, Arc::new(egress)).unwrap();
        let control = Control::new(&name, broker, &me, None).unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let thread = Some(serve_control(control, done.clone()));
        Self { name, done, thread }
    }

    /// A runner's connection to the control pipe.
    fn client(&self) -> ControlClient {
        ControlClient::connect(&self.name, &token::current_user_sid().unwrap()).unwrap()
    }
}

/// Serves `control` on a thread until `done` is set.
fn serve_control(mut control: Control, done: Arc<AtomicBool>) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let stopping = || done.load(Ordering::SeqCst);
        while !stopping() {
            let patience = Duration::from_millis(100);
            control.serve_one(patience, &stopping).unwrap();
        }
    })
}

impl Drop for Served {
    /// Stops the control thread, which closes every launch of the broker.
    fn drop(&mut self) {
        self.done.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Has the broker admit `launch` as an implementer, as the runner does.
fn admit(served: &Served, launch: &Launch) -> Opened {
    let mut control = served.client();
    let opened = control.open(&launch.id, Role::Implementer, &launch.package, 1);
    opened.unwrap()
}

/// Starts the client in `launch` running `probe` with `extra` variables
/// and, when the broker admitted it, its pipe in [`PIPE_VARIABLE`].
fn start_probe(
    launcher: &mut Launcher,
    (launch, tools): (&Launch, &std::path::Path),
    probe: &str,
    (opened, mut extra): (Option<&Opened>, Vec<(&'static str, String)>),
) -> std::path::PathBuf {
    extra.push((PROBE, probe.to_owned()));
    if let Some(opened) = opened {
        extra.push((PIPE_VARIABLE, pipe_path(&opened.pipe).unwrap()));
    }
    start_client(launcher, launch, tools, extra)
}

/// A UDP receiver on IPv4 loopback that the host shows working first.
fn udp_receiver() -> UdpSocket {
    let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
    let patience = Some(Duration::from_secs(2));
    receiver.set_read_timeout(patience).unwrap();
    let control = UdpSocket::bind("127.0.0.1:0").unwrap();
    let address = receiver.local_addr().unwrap();
    control.send_to(b"control", address).unwrap();
    let mut buffer = [0_u8; 64];
    let (count, _) = receiver.recv_from(&mut buffer).expect("the receiver works");
    assert_eq!(&buffer[..count], b"control");
    receiver
}

/// Whether anything reached `receiver` within its timeout.
fn received(receiver: &UdpSocket) -> bool {
    let mut buffer = [0_u8; 64];
    receiver.recv_from(&mut buffer).is_ok()
}

/// A public name the host resolves now, or `None` when it resolves none:
/// then the launch's failure would prove nothing and is not checked.
fn resolvable_name() -> Option<String> {
    let name = "github.com";
    let resolved = (name, 443).to_socket_addrs().ok()?.next();
    resolved.map(|_| name.to_owned())
}

/// The environment the routed probe needs, and what the host can check.
struct Targets {
    v4: Listener,
    v6: Option<Listener>,
    udp: UdpSocket,
    dns: Option<String>,
}

impl Targets {
    /// The listeners, each shown working from the host first: the IPv6
    /// one only where the host has IPv6 loopback.
    fn new() -> Self {
        let v4 = Listener::bind("127.0.0.1:0").expect("IPv4 loopback");
        TcpStream::connect(v4.address).expect("the IPv4 listener works");
        let v6 = Listener::bind("[::1]:0");
        if let Some(v6) = &v6 {
            TcpStream::connect(v6.address).expect("the IPv6 listener works");
        }
        let (udp, dns) = (udp_receiver(), resolvable_name());
        Self { v4, v6, udp, dns }
    }

    /// The variables the client reads them from.
    fn variables(&self) -> Vec<(&'static str, String)> {
        let mut extra = vec![(ECHO_V4, self.v4.address.to_string())];
        extra.push((UDP, self.udp.local_addr().unwrap().to_string()));
        if let Some(v6) = &self.v6 {
            extra.push((ECHO_V6, v6.address.to_string()));
        }
        if let Some(name) = &self.dns {
            extra.push((DNS_NAME, name.clone()));
        }
        extra
    }
}

/// Test (routed): the allowed route, the refusals and the direct attempts.
fn routed() -> String {
    let dir = tempfile::tempdir().unwrap();
    let (trees, mut launcher) = (Trees::new(&dir), Launcher::new().unwrap());
    let targets = Targets::new();
    let upstream = ToListener::to(&targets.v4);
    let served = Served::start(upstream.clone());
    let own = prepare(&mut launcher, &trees, "egr-routed");
    let opened = admit(&served, &own);
    let tools = client_tools(&trees, &[&own.package]);
    let before = targets.v4.reached();
    let variables = (Some(&opened), targets.variables());
    let report = start_probe(&mut launcher, (&own, &tools), "routes", variables);
    let lines = finish(&mut launcher, &own, &report);
    check_answers(&lines);
    check_direct(&lines, &targets, before);
    assert_eq!(upstream.connected.load(Ordering::SeqCst), 1, "{lines:?}");
    launcher.cleanup(&own.id).unwrap();
    lines.join("; ")
}

/// Checks the broker's answers in the routed probe's report.
fn check_answers(lines: &[String]) {
    let allowed = outcome(lines, "allowed");
    assert_eq!(allowed, "status 200, echoed ping", "{lines:?}");
    for refused in ["other_host", "other_port", "plain_get", "malformed"] {
        let said = outcome(lines, refused);
        assert_eq!(said, "status 403", "{refused}: {lines:?}");
    }
}

/// Checks the direct attempts of the routed probe against what reached
/// the host: only the broker's tunnel reached the IPv4 listener.
fn check_direct(lines: &[String], targets: &Targets, before: usize) {
    let failed = |label: &str| outcome(lines, label).starts_with("error");
    assert!(failed("direct_v4"), "{lines:?}");
    let reached = targets.v4.reached() - before;
    assert_eq!(reached, 1, "only the broker's tunnel: {lines:?}");
    if let Some(v6) = &targets.v6 {
        assert!(failed("direct_v6"), "{lines:?}");
        assert_eq!(v6.reached(), 1, "only the host's own control");
    }
    assert!(!received(&targets.udp), "a datagram arrived: {lines:?}");
    if targets.dns.is_some() {
        assert!(failed("dns"), "{lines:?}");
    }
}

/// Test (lost): the client holds a tunnel; closing the launch ends it.
fn lost() -> String {
    let dir = tempfile::tempdir().unwrap();
    let (trees, mut launcher) = (Trees::new(&dir), Launcher::new().unwrap());
    let listener = Listener::bind("127.0.0.1:0").unwrap();
    let served = Served::start(ToListener::to(&listener));
    let own = prepare(&mut launcher, &trees, "egr-lost");
    let opened = admit(&served, &own);
    let tools = client_tools(&trees, &[&own.package]);
    let held = (Some(&opened), Vec::new());
    let report = start_probe(&mut launcher, (&own, &tools), "hold", held);
    // The client's ping went through the tunnel and came back.
    let echoed = || listener.echoed.load(Ordering::SeqCst) == 4;
    wait_for(echoed, "the client's ping through its tunnel");
    let started = Instant::now();
    assert!(served.client().close(&own.id).unwrap());
    let took = started.elapsed();
    // The close is woken by the halt, not found by a tick of the threads.
    assert!(took < Duration::from_secs(1), "closing took {took:?}");
    let lines = finish(&mut launcher, &own, &report);
    check_lost(&lines);
    launcher.cleanup(&own.id).unwrap();
    format!("closed in {took:?}; {}", lines.join("; "))
}

/// Checks that the held tunnel worked, then ended, and the pipe was gone.
fn check_lost(lines: &[String]) {
    let held = outcome(lines, "held");
    let ended = held.strip_prefix("status 200, echoed ping, then ");
    assert!(ended.is_some(), "{lines:?}");
    assert_eq!(outcome(lines, "reopened"), "error 2", "{lines:?}");
}

/// Test (loopback): the probe of loopback inside one package.
fn loopback() -> String {
    let dir = tempfile::tempdir().unwrap();
    let (trees, mut launcher) = (Trees::new(&dir), Launcher::new().unwrap());
    let own = prepare(&mut launcher, &trees, "egr-loop");
    let tools = client_tools(&trees, &[&own.package]);
    let report = start_probe(
        &mut launcher,
        (&own, &tools),
        "loopback",
        (None, Vec::new()),
    );
    let lines = finish(&mut launcher, &own, &report);
    for label in ["bind", "same_process", "other_process"] {
        outcome(&lines, label);
    }
    launcher.cleanup(&own.id).unwrap();
    lines.join("; ")
}

// ---- The client's side, inside a launch ----

/// The client inside a launch: runs the probe [`PROBE`] names.
fn client() -> String {
    match std::env::var(PROBE).unwrap().as_str() {
        "routes" => routes(),
        "hold" => hold(),
        "loopback" => loopback_probe(),
        other => panic!("no such probe: {other}"),
    }
}

/// The launch's egress pipe, as the runner told it.
fn egress_pipe() -> String {
    std::env::var(PIPE_VARIABLE).unwrap()
}

/// An attempt's error as `error <code>`.
fn failed(error: &std::io::Error) -> String {
    format!("error {}", error.raw_os_error().unwrap_or(-1))
}

/// Opens the pipe at `path` with a client's rights.
fn open_pipe(path: &str) -> std::io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.access_mode(CLIENT_ACCESS);
    options.open(path)
}

/// Sends `request` on a new session of the egress pipe and reads the
/// answer's head: the session and `status <code>`.
fn request(request: &str) -> Result<(File, String), String> {
    let mut pipe = open_pipe(&egress_pipe()).map_err(|error| failed(&error))?;
    pipe.write_all(request.as_bytes())
        .map_err(|error| failed(&error))?;
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match pipe.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            Ok(_) => return Err("closed before an answer".into()),
            Err(error) => return Err(failed(&error)),
        }
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let code = head.split(' ').nth(1).unwrap_or("?").to_owned();
    Ok((pipe, format!("status {code}")))
}

/// The status a request gets, or how it failed.
fn status(text: &str) -> String {
    request(text).map_or_else(|error| error, |(_, status)| status)
}

/// Opens a tunnel to [`ALLOWED`] and sends `ping` through it: the session
/// and what happened.
fn tunnel_ping() -> (Option<File>, String) {
    let (mut pipe, status) = match request(&format!("CONNECT {ALLOWED}:443 HTTP/1.1\r\n\r\n")) {
        Ok(opened) => opened,
        Err(error) => return (None, error),
    };
    if status != "status 200" {
        return (None, status);
    }
    pipe.write_all(b"ping").unwrap();
    let mut echoed = [0_u8; 4];
    match pipe.read_exact(&mut echoed) {
        Ok(()) => (
            Some(pipe),
            format!("{status}, echoed {}", String::from_utf8_lossy(&echoed)),
        ),
        Err(error) => (None, format!("{status}, then {}", failed(&error))),
    }
}

/// A direct TCP connection to the address in `variable`.
fn direct(variable: &str) -> String {
    let address: SocketAddr = std::env::var(variable).unwrap().parse().unwrap();
    match TcpStream::connect_timeout(&address, ATTEMPT) {
        Ok(_) => "connected".into(),
        Err(error) => failed(&error),
    }
}

/// One direct UDP datagram to the test's receiver.
fn udp() -> String {
    let target: SocketAddr = std::env::var(UDP).unwrap().parse().unwrap();
    let sent = UdpSocket::bind("0.0.0.0:0").and_then(|socket| socket.send_to(b"probe", target));
    sent.map_or_else(|error| failed(&error), |_| "sent".into())
}

/// A name lookup through the system resolver.
fn dns(name: &str) -> String {
    match (name, 443).to_socket_addrs().map(|mut found| found.next()) {
        Ok(Some(_)) => "resolved".into(),
        Ok(None) => "error none".into(),
        Err(error) => failed(&error),
    }
}

/// The requests the broker must refuse, by report label.
fn refused_requests() -> [(&'static str, String); 4] {
    let other_host = "CONNECT other.test:443 HTTP/1.1\r\n\r\n".to_owned();
    let other_port = format!("CONNECT {ALLOWED}:22 HTTP/1.1\r\n\r\n");
    let plain_get = format!("GET http://{ALLOWED}/ HTTP/1.1\r\n\r\n");
    let malformed = format!("CONNECT {ALLOWED}:443\r\n\r\n");
    [
        ("other_host", other_host),
        ("other_port", other_port),
        ("plain_get", plain_get),
        ("malformed", malformed),
    ]
}

/// The routed probe: one report line per attempt.
fn routes() -> String {
    let mut lines = vec![format!("allowed = {}", tunnel_ping().1)];
    for (label, text) in refused_requests() {
        lines.push(format!("{label} = {}", status(&text)));
    }
    lines.push(format!("direct_v4 = {}", direct(ECHO_V4)));
    if std::env::var_os(ECHO_V6).is_some() {
        lines.push(format!("direct_v6 = {}", direct(ECHO_V6)));
    }
    lines.push(format!("udp = {}", udp()));
    if let Ok(name) = std::env::var(DNS_NAME) {
        lines.push(format!("dns = {}", dns(&name)));
    }
    lines.join("\n")
}

/// The hold probe: a tunnel kept open until the broker ends it, then an
/// attempt to open the pipe again.
fn hold() -> String {
    let (pipe, said) = tunnel_ping();
    let ended = match pipe {
        Some(mut pipe) => {
            let mut rest = Vec::new();
            match pipe.read_to_end(&mut rest) {
                Ok(_) => "then closed".to_owned(),
                Err(error) => format!("then broken {}", failed(&error)),
            }
        }
        None => "no tunnel".to_owned(),
    };
    let reopened =
        open_pipe(&egress_pipe()).map_or_else(|error| failed(&error), |_| "opened".into());
    format!("held = {said}, {ended}\nreopened = {reopened}")
}

/// The loopback probe: a listener on IPv4 loopback inside the launch, and
/// connections to it from this process and from a second process of the
/// same launch, so of the same package.
fn loopback_probe() -> String {
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(error) => {
            return format!(
                "bind = {}\nsame_process = -\nother_process = -",
                failed(&error)
            );
        }
    };
    let address = listener.local_addr().unwrap();
    let same = TcpStream::connect_timeout(&address, ATTEMPT);
    let same = same.map_or_else(|error| failed(&error), |_| "connected".into());
    let other = other_process(&address.to_string());
    format!("bind = ok {address}\nsame_process = {same}\nother_process = {other}")
}

/// Starts this program again inside the launch as a [`peer`] connecting to
/// `address`, and returns what it reported.
fn other_process(address: &str) -> String {
    // The launch's working directory, the one place it may write.
    let work = std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap());
    let report = work.join("peer-report.txt");
    let exe = std::env::current_exe().unwrap();
    let ran = std::process::Command::new(exe)
        .args([support::CHILD, "--exact", "--ignored"])
        .env(support::ROLE, "peer")
        .env(support::REPORT, &report)
        .env(PEER, address)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    match ran {
        Ok(_) => std::fs::read_to_string(&report).unwrap_or_else(|_| "no report".into()),
        Err(error) => format!("not started: {}", failed(&error)),
    }
}

/// The loopback peer: connects to [`PEER`].
fn peer() -> String {
    let address: SocketAddr = std::env::var(PEER).unwrap().parse().unwrap();
    match TcpStream::connect_timeout(&address, ATTEMPT) {
        Ok(_) => "connected".into(),
        Err(error) => failed(&error),
    }
}

/// Test (runner): the runner's own admission of its launches, through
/// real prepare, start, observe and cleanup requests.
mod runner {
    use super::*;
    use agentc_supervisor::host_runtime::windows::broker::pipe_name;
    use agentc_supervisor::host_runtime::windows::egress_admission::EgressRoute;
    use agentc_supervisor::host_runtime::windows::runner::{Request, Runner, answer};
    use serde_json::{Value, json};
    use std::path::{Path, PathBuf};

    /// Sends one request and returns the runner's answer line.
    fn ask(runner: &mut Runner, request: Value) -> Value {
        let parsed = serde_json::from_value::<Request>(request).map_err(anyhow::Error::from);
        answer(parsed.and_then(|request| runner.handle(request)))
    }

    /// `ask`, failing the test unless the runner answered `ok`.
    fn ok(runner: &mut Runner, request: Value) -> Value {
        let reply = ask(runner, request.clone());
        assert_eq!(reply["ok"], true, "{request} -> {reply}");
        reply
    }

    /// One launch of the runner below `root`.
    struct Case {
        launch: String,
        root: PathBuf,
    }

    impl Case {
        /// The launch `launch`, its directories below `root`.
        fn new(root: &Path, launch: &str) -> Self {
            let root = root.join(launch);
            std::fs::create_dir_all(root.join("logs")).unwrap();
            let launch = launch.to_owned();
            Self { launch, root }
        }

        /// The `prepare` request of an implementer launch.
        fn prepare(&self) -> Value {
            let profile = format!("agentc.test.{}", uuid::Uuid::new_v4().simple());
            let limits = json!({"processes": 8, "process_memory_mb": 256, "job_memory_mb": 512});
            json!({"op": "prepare", "launch": self.launch, "role": "implementer",
                   "spec": {"profile": profile, "less_privileged": true,
                            "capabilities": ["registryRead"], "limits": limits,
                            "control": self.root.join("control"),
                            "work": self.root.join("work")}})
        }

        /// The `start` request that prints the launch's egress pipe.
        fn start(&self) -> Value {
            let root = std::env::var("SystemRoot").unwrap();
            let cmd = format!("{root}\\System32\\cmd.exe");
            let script = format!("echo pipe=%{PIPE_VARIABLE}%");
            let work = self.root.join("work");
            json!({"op": "start", "launch": self.launch, "argv": [cmd, "/d", "/c", script],
                   "directory": work, "output": self.log(),
                   "environment": {"SystemRoot": root, "LOCALAPPDATA": work}})
        }

        /// The launch's log.
        fn log(&self) -> PathBuf {
            self.root.join("logs").join("start.log")
        }

        /// The stage the journal in `state` records for the launch.
        fn stage(&self, state: &Path) -> String {
            let bytes = std::fs::read(state.join(format!("{}.json", self.launch))).unwrap();
            let entry: Value = serde_json::from_slice(&bytes).unwrap();
            entry["stage"].as_str().unwrap().to_owned()
        }

        /// Starts the launch, waits for it to end and returns its log.
        fn run(&self, runner: &mut Runner) -> String {
            ok(runner, self.start());
            let wait = json!({"op": "observe", "launch": self.launch, "wait_ms": 30_000});
            let seen = ok(runner, wait);
            assert_eq!(seen["state"], "exited", "{seen}");
            ok(runner, json!({"op": "terminate", "launch": self.launch}));
            std::fs::read_to_string(self.log()).unwrap()
        }

        /// The cleanup request.
        fn cleanup(&self) -> Value {
            json!({"op": "cleanup", "launch": self.launch, "remove_directories": true})
        }
    }

    /// An upstream for a broker these launches never use.
    fn unused_upstream() -> Arc<ToListener> {
        ToListener::to(&Listener::bind("127.0.0.1:0").unwrap())
    }

    /// The whole test; its steps are the functions below.
    pub fn admitted_starts() -> String {
        let dir = tempfile::tempdir().unwrap();
        let (state, me) = (dir.path().join("state"), token::current_user_sid().unwrap());
        let id = uuid::Uuid::new_v4().simple();
        let name = format!("{}-{id}", control_pipe_name("test"));
        let route = EgressRoute::new(&name, &me).unwrap();
        let mut runner = Runner::new(&state).unwrap().with_egress(Some(route));
        let first = Case::new(dir.path(), "egr-run-first");
        refused_without_a_broker(&mut runner, &first, &state);
        let served = Served::named(&name, unused_upstream());
        let log = first.run(&mut runner);
        let expected = pipe_path(&pipe_name(SERVICE, &first.launch)).unwrap();
        assert!(log.contains(&format!("pipe={expected}")), "{log}");
        drop(served);
        kept_after_a_missed_release(&mut runner, &first);
        let _served = Served::named(&name, unused_upstream());
        let second = Case::new(dir.path(), "egr-run-second");
        ok(&mut runner, second.prepare());
        second.run(&mut runner);
        assert!(
            !runner.egress().unreleased(&first.launch),
            "the sweep missed it"
        );
        let cleaned = ok(&mut runner, second.cleanup());
        assert_eq!(cleaned["egress_detail"], Value::Null, "{cleaned}");
        format!("log of the first launch: {}", log.trim())
    }

    /// With no broker serving the control pipe, a start is refused and the
    /// launch stays prepared; nothing ran.
    fn refused_without_a_broker(runner: &mut Runner, case: &Case, state: &Path) {
        ok(runner, case.prepare());
        let refused = ask(runner, case.start());
        assert_eq!(refused["ok"], false, "{refused}");
        let said = refused.to_string();
        assert!(said.contains("not started"), "{said}");
        assert_eq!(case.stage(state), "prepared");
        assert!(!case.log().exists(), "the launch ran");
    }

    /// A cleanup while the broker is gone succeeds, says the release
    /// failed and keeps the launch for release.
    fn kept_after_a_missed_release(runner: &mut Runner, case: &Case) {
        let cleaned = ok(runner, case.cleanup());
        assert!(cleaned["egress_detail"].is_string(), "{cleaned}");
        assert!(runner.egress().unreleased(&case.launch));
    }
}
