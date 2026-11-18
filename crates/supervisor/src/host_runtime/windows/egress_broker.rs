//! The egress broker's handler (phase 6, step 6b): what a launch's session
//! on its broker pipe ([`super::broker`]) may do with the network.
//!
//! A launch's token carries no network capability, so the only way out is
//! this pipe. The client sends one HTTP request head; only `CONNECT
//! host:443` to a host the broker's allowlist names is honoured, with the
//! policy of the Linux proxy ([`crate::egress`]): the name is resolved by
//! the broker and only a publicly routable address is contacted. Anything
//! else is answered `403` and nothing is contacted. An honoured request is
//! answered `200` and the session becomes a byte tunnel to the destination
//! until either side ends it or the broker closes the launch, which ends
//! the tunnel at once: losing the broker loses the egress.
//!
//! Each decision is one JSON line on standard error naming the launch and
//! its role from the broker's grant, never from what the client sent.
use super::broker::{Grant, Halt, Handler};
use super::pipe::{Reader, Received, Session, Writer};
use crate::egress;
use anyhow::{Context, Result, anyhow, bail, ensure};
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::os::windows::io::AsRawSocket;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};
use windows_sys::Win32::Networking::WinSock::{
    POLLRDNORM, POLLWRNORM, SOCKET, WSAGetLastError, WSAPOLLFD, WSAPoll,
};

/// The egress broker's service name, which its launch pipes carry.
pub const SERVICE: &str = "egress";
/// The variable a launch is told its egress pipe's path in.
pub const PIPE_VARIABLE: &str = "AGENTC_EGRESS_PIPE";
/// How long one wait of a tunnel direction lasts before it looks whether
/// the tunnel or the launch's pipe is ending.
const TICK: Duration = Duration::from_millis(250);
/// The most bytes one step of a tunnel carries.
const CHUNK: usize = 16 * 1024;
/// How long a session's last bytes wait for the client to read them.
const LINGER: Duration = Duration::from_secs(5);
/// How long the broker tries to reach one address of a destination.
const CONNECT_PATIENCE: Duration = Duration::from_secs(10);

/// How the broker reaches a destination it allows.
pub trait Upstream: Send + Sync {
    /// A connection to `host` on port 443, or `None` when `host` has no
    /// address egress may reach.
    fn connect(&self, host: &str) -> Result<Option<TcpStream>>;
}

/// The production upstream: `host`'s public addresses only
/// ([`egress::public_addresses_blocking`]), tried in turn.
pub struct Public;

impl Upstream for Public {
    /// Resolves `host` and connects to its first public address that
    /// answers.
    fn connect(&self, host: &str) -> Result<Option<TcpStream>> {
        connect_first(&egress::public_addresses_blocking(host))
    }
}

/// A connection to the first of `addresses` that answers; `None` when
/// there are none, and the last failure when none answers.
pub fn connect_first(addresses: &[SocketAddr]) -> Result<Option<TcpStream>> {
    let mut failure = None;
    for address in addresses {
        match TcpStream::connect_timeout(address, CONNECT_PATIENCE) {
            Ok(stream) => return Ok(Some(stream)),
            Err(error) => failure = Some(error),
        }
    }
    match failure {
        Some(error) => Err(error).context("no address of the destination answered"),
        None => Ok(None),
    }
}

/// The egress broker's [`Handler`]: an allowlist and a way upstream.
pub struct Egress {
    allow: Vec<String>,
    upstream: Arc<dyn Upstream>,
}

impl Egress {
    /// A handler honouring CONNECTs to `allow` (exact hosts and `.suffix`
    /// entries, as [`egress::allowed`]) through `upstream`.
    pub fn new(allow: Vec<String>, upstream: Arc<dyn Upstream>) -> Self {
        Self { allow, upstream }
    }

    /// The production handler: `allow` reached through [`Public`].
    pub fn public(allow: Vec<String>) -> Self {
        Self::new(allow, Arc::new(Public))
    }

    /// The connection a request `head` is honoured with, if any. An
    /// upstream failure is the same as no route: the client is refused.
    /// The broker's halt ends the wait for the destination at once
    /// ([`reach`]).
    fn route(&self, grant: &Grant, head: &str, halt: &Halt) -> Option<TcpStream> {
        let target = egress::permitted_target(head, &self.allow);
        let stream = target.as_deref().and_then(|host| {
            let reached = reach(&self.upstream, host, halt);
            reached.unwrap_or_else(|error| {
                eprintln!("agentc-supervisor: egress to {host}: {error:#}");
                None
            })
        });
        let asked = egress::connect_target(head);
        log(grant, asked.as_deref().unwrap_or("-"), stream.is_some());
        stream
    }
}

impl Handler for Egress {
    /// Reads the request head, refuses or opens the tunnel, then carries
    /// bytes both ways until either side or the broker ends it.
    fn serve(&self, grant: &Grant, session: &mut Session<'_>, halt: &Halt) -> Result<()> {
        let Some((head, early)) = read_head(session, halt)? else {
            return Ok(());
        };
        let route = self.route(grant, &head, halt);
        if halt.asked() {
            return Ok(());
        }
        let Some(stream) = route else {
            session.split().1.write_all(egress::FORBIDDEN)?;
            return linger(session, halt);
        };
        session.split().1.write_all(egress::ESTABLISHED)?;
        let carried = tunnel(session, stream, &early, halt);
        // A tunnel the broker ended fails its pending write; that is the
        // close working, not an error of the session.
        if halt.asked() {
            return Ok(());
        }
        carried?;
        linger(session, halt)
    }
}

/// Connects to `host` through `upstream` on a thread of its own, so the
/// broker's halt ends the wait at once: name resolution and a connect
/// cannot be interrupted. A connection that arrives after the halt is
/// dropped by that thread, which ends within the connect's own patience.
fn reach(upstream: &Arc<dyn Upstream>, host: &str, halt: &Halt) -> Result<Option<TcpStream>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let (upstream, host) = (Arc::clone(upstream), host.to_owned());
    std::thread::spawn(move || {
        let _ = sender.send(upstream.connect(&host));
    });
    loop {
        match receiver.recv_timeout(TICK) {
            Ok(reached) => return reached,
            Err(RecvTimeoutError::Timeout) if halt.asked() => return Ok(None),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => bail!("the connecting thread ended unheard"),
        }
    }
}

/// Waits, up to [`LINGER`] and never past the broker's halt, for the client
/// to leave after its last bytes were written. Ending the session at once
/// would disconnect the pipe, and Windows discards what the client has not
/// read yet: a refusal or a destination's last bytes would never arrive.
/// What the client still sends is discarded.
fn linger(session: &mut Session<'_>, halt: &Halt) -> Result<()> {
    let deadline = Instant::now() + LINGER;
    while !halt.asked() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || session.read(CHUNK, left.min(TICK))? == Received::Closed {
            break;
        }
    }
    Ok(())
}

/// One JSON line per decision, naming the launch and role of the grant.
fn log(grant: &Grant, host: &str, allowed: bool) {
    let line = serde_json::json!({"egress": host, "allowed": allowed,
        "launch": grant.launch(), "role": grant.role()});
    eprintln!("{line}");
}

/// Reads the client's request head and the bytes that came after it in the
/// same reads. `None` when the client left or the launch is closing first;
/// an error when the head is too long or late.
fn read_head(session: &mut Session<'_>, halt: &Halt) -> Result<Option<(String, Vec<u8>)>> {
    let deadline = Instant::now() + egress::HEAD_TIMEOUT;
    let mut bytes = Vec::new();
    while egress::head_end(&bytes).is_none() {
        ensure!(
            bytes.len() < egress::MAX_HEAD,
            "the request head is too long"
        );
        let left = deadline.saturating_duration_since(Instant::now());
        ensure!(
            !left.is_zero(),
            "the client sent no whole request head in time"
        );
        if halt.asked() || !read_more(session, &mut bytes, left.min(TICK))? {
            return Ok(None);
        }
    }
    Ok(Some(split_head(bytes)))
}

/// Adds what the client sends within `patience` to `bytes`, never past
/// [`egress::MAX_HEAD`]; false once the client is gone.
fn read_more(session: &mut Session<'_>, bytes: &mut Vec<u8>, patience: Duration) -> Result<bool> {
    match session.read(egress::MAX_HEAD - bytes.len(), patience)? {
        Received::Data(data) => bytes.extend(data),
        Received::Idle => {}
        Received::Closed => return Ok(false),
    }
    Ok(true)
}

/// A complete head in `bytes` as text, and the bytes after it.
fn split_head(mut bytes: Vec<u8>) -> (String, Vec<u8>) {
    let end = egress::head_end(&bytes).unwrap_or(bytes.len());
    let early = bytes.split_off(end);
    (String::from_utf8_lossy(&bytes).into_owned(), early)
}

/// What both directions of one tunnel share: the destination's socket,
/// which is non-blocking so that no wait on it outlasts a tick, and how to
/// learn that the tunnel is ending.
struct Ends<'a> {
    upstream: &'a TcpStream,
    halt: &'a Halt,
    ended: AtomicBool,
}

impl Ends<'_> {
    /// Whether a tunnel direction should stop: the broker's halt, or the
    /// other direction ended.
    fn stopping(&self) -> bool {
        self.halt.asked() || self.ended.load(Ordering::SeqCst)
    }

    /// Runs one direction, then marks the tunnel ended and shuts the
    /// destination's connection, which wakes the other direction's wait.
    fn ending(&self, direction: impl FnOnce() -> Result<()>) -> Result<()> {
        let carried = direction();
        self.ended.store(true, Ordering::SeqCst);
        let _ = self.upstream.shutdown(Shutdown::Both);
        carried
    }

    /// Waits up to a tick for the destination's socket to be ready for
    /// `events` (or to report an error or hang-up, which the next read or
    /// write then reports).
    fn ready(&self, events: i16) -> Result<bool> {
        let mut polled = WSAPOLLFD {
            fd: self.upstream.as_raw_socket() as SOCKET,
            events,
            revents: 0,
        };
        let millis = i32::try_from(TICK.as_millis()).unwrap_or(i32::MAX);
        // SAFETY: `polled` is one valid entry for the call's duration.
        let count = unsafe { WSAPoll(&mut polled, 1, millis) };
        ensure!(count >= 0, "WSAPoll failed with error {}", unsafe {
            // SAFETY: reads the calling thread's last socket error.
            WSAGetLastError()
        });
        Ok(count > 0)
    }

    /// Sends all of `bytes` to the destination, unless the tunnel ends
    /// first; a destination that stops reading holds it only until then.
    fn send_all(&self, mut bytes: &[u8]) -> Result<()> {
        while !bytes.is_empty() && !self.stopping() {
            if !self.ready(POLLWRNORM)? {
                continue;
            }
            match (&mut &*self.upstream).write(bytes) {
                Ok(count) => bytes = &bytes[count..],
                Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                Err(error) => return Err(error).context("write to the destination"),
            }
        }
        Ok(())
    }
}

/// Carries bytes between the session and `upstream`: `early` (sent with the
/// head) first, then the client's bytes up from one thread and the
/// destination's down from this one. Whichever direction ends first ends
/// the other within a tick; the broker's halt ends both.
fn tunnel(session: &mut Session<'_>, upstream: TcpStream, early: &[u8], halt: &Halt) -> Result<()> {
    upstream.set_nonblocking(true)?;
    let ends = Ends {
        upstream: &upstream,
        halt,
        ended: AtomicBool::new(false),
    };
    ends.send_all(early)?;
    let (mut reader, mut writer) = session.split();
    std::thread::scope(|scope| {
        let upward = scope.spawn(|| ends.ending(|| up(&mut reader, &ends)));
        let downward = ends.ending(|| down(&mut writer, &ends));
        let upward = upward.join();
        let upward = upward.map_err(|_| anyhow!("the tunnel's upward thread panicked"))?;
        downward.and(upward)
    })
}

/// The client's bytes to the destination, until the client leaves.
fn up(reader: &mut Reader<'_>, ends: &Ends<'_>) -> Result<()> {
    while !ends.stopping() {
        match reader.read(CHUNK, TICK)? {
            Received::Data(data) => ends.send_all(&data)?,
            Received::Idle => {}
            Received::Closed => break,
        }
    }
    Ok(())
}

/// The destination's bytes to the client, until the destination ends. A
/// client that reads slowly holds the destination back, however long, and
/// only the broker's halt or the client's leaving ends that wait.
fn down(writer: &mut Writer<'_>, ends: &Ends<'_>) -> Result<()> {
    let mut buffer = vec![0_u8; CHUNK];
    while !ends.stopping() {
        if !ends.ready(POLLRDNORM)? {
            continue;
        }
        match (&mut &*ends.upstream).read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => writer.stream_all(&buffer[..count])?,
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            Err(error) => return Err(error).context("read from the destination"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::broker::Role;
    use super::super::pipe::{Accepted, Pipe};
    use super::super::token;
    use super::*;
    use crate::service::pipe_policy::{CLIENT_ACCESS, pipe_path};
    use std::collections::BTreeMap;
    use std::fs::{File, OpenOptions};
    use std::net::TcpListener;
    use std::os::windows::fs::OpenOptionsExt;
    use std::sync::Mutex;
    use std::thread::JoinHandle;

    /// A package no token on this host carries; the handler never checks it.
    const PACKAGE: &str = "S-1-15-2-1-2-3-4-5-6-7";
    const PATIENCE: Duration = Duration::from_secs(10);
    /// How soon a halt must end a session.
    const PROMPTLY: Duration = Duration::from_secs(2);
    const CONNECT: &[u8] = b"CONNECT allowed.test:443 HTTP/1.1\r\n\r\n";

    /// Routes names to fixed local addresses and records what it was asked.
    #[derive(Default)]
    struct Fixed {
        routes: BTreeMap<String, SocketAddr>,
        asked: Mutex<Vec<String>>,
    }

    impl Upstream for Fixed {
        /// Records `host` and connects to its fixed address, if any.
        fn connect(&self, host: &str) -> Result<Option<TcpStream>> {
            self.asked.lock().unwrap().push(host.to_owned());
            let address = self.routes.get(host).copied();
            connect_first(address.as_slice())
        }
    }

    /// An upstream whose connect takes far longer than any test, like a
    /// destination that never answers.
    struct Stalled;

    impl Upstream for Stalled {
        /// Waits half a minute and reaches nothing.
        fn connect(&self, _: &str) -> Result<Option<TcpStream>> {
            std::thread::sleep(Duration::from_secs(30));
            Ok(None)
        }
    }

    /// An upstream whose every connect fails, like a destination whose
    /// addresses all refuse the connection.
    struct Refusing;

    impl Upstream for Refusing {
        /// Fails as a refused connection does.
        fn connect(&self, _: &str) -> Result<Option<TcpStream>> {
            let refused = std::io::Error::from(ErrorKind::ConnectionRefused);
            Err(refused).context("no address of the destination answered")
        }
    }

    /// A destination that sends back what it gets, then ends when its
    /// client does. Its address.
    fn echo_server() -> SocketAddr {
        serve_each(|mut stream| {
            let mut back = stream.try_clone().unwrap();
            let _ = std::io::copy(&mut stream, &mut back);
        })
    }

    /// A destination that runs `each` on every connection, on a thread of
    /// its own. Its address.
    fn serve_each(each: impl Fn(TcpStream) + Send + Sync + 'static) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let each = Arc::new(each);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let each = Arc::clone(&each);
                std::thread::spawn(move || each(stream));
            }
        });
        address
    }

    /// An upstream sending `allowed.test` to `address`.
    fn fixed(address: SocketAddr) -> Arc<Fixed> {
        let routes = BTreeMap::from([("allowed.test".to_owned(), address)]);
        Arc::new(Fixed {
            routes,
            ..Fixed::default()
        })
    }

    /// A handler allowing `allowed.test` through `upstream`.
    fn egress(upstream: Arc<dyn Upstream>) -> Egress {
        Egress::new(vec!["allowed.test".into()], upstream)
    }

    /// A grant of this account for the handler.
    fn grant() -> Grant {
        let me = token::current_user_sid().unwrap();
        Grant::new("egress-test", Role::Implementer, &me, PACKAGE, 1).unwrap()
    }

    /// A served session: the test's client of it, the thread serving it
    /// (which returns what `serve` did) and the launch's halt.
    struct Case {
        client: File,
        served: JoinHandle<Result<()>>,
        halt: Arc<Halt>,
    }

    /// Serves one client with `egress` on a pipe of this account whose
    /// wake event is the launch's halt, as the broker's pipes are.
    fn case(egress: Egress) -> Case {
        let name = format!("agentc-test-egress-{}", uuid::Uuid::new_v4().simple());
        let (me, halt) = (
            token::current_user_sid().unwrap(),
            Arc::new(Halt::new().unwrap()),
        );
        let mut pipe = Pipe::create(&name, &me, &me, Some(halt.wake())).unwrap();
        let mut options = OpenOptions::new();
        options.access_mode(CLIENT_ACCESS);
        let client = options.open(pipe_path(&name).unwrap()).unwrap();
        let held = Arc::clone(&halt);
        let served = std::thread::spawn(move || {
            let Accepted::Client(mut session) = pipe.accept(PATIENCE)? else {
                anyhow::bail!("the client was not admitted");
            };
            egress.serve(&grant(), &mut session, &held)
        });
        Case {
            client,
            served,
            halt,
        }
    }

    /// Sends `request` and reads the answer's head.
    fn ask(client: &mut File, request: &[u8]) -> String {
        client.write_all(request).unwrap();
        let mut answer = Vec::new();
        let mut byte = [0_u8; 1];
        while !answer.ends_with(b"\r\n\r\n") && client.read(&mut byte).unwrap() == 1 {
            answer.push(byte[0]);
        }
        String::from_utf8(answer).unwrap()
    }

    /// Drops the client and checks that its session then ends at once, well
    /// within the broker's wait for a client to leave.
    fn leaves(case: Case) {
        let started = Instant::now();
        drop(case.client);
        case.served.join().unwrap().unwrap();
        let took = started.elapsed();
        assert!(took < LINGER / 2, "ended {took:?} after the client left");
    }

    /// Asks the launch's halt and checks that its session ends promptly.
    fn halted(halt: &Halt, served: JoinHandle<Result<()>>) {
        let started = Instant::now();
        halt.ask().unwrap();
        served.join().unwrap().unwrap();
        let took = started.elapsed();
        assert!(took < PROMPTLY, "the session ended {took:?} after the halt");
    }

    /// Whether the pipe has nothing more for the client: it was closed.
    fn ends(client: &mut File) -> bool {
        let mut rest = Vec::new();
        let read = client.read_to_end(&mut rest);
        read.map_or(true, |_| rest.is_empty())
    }

    #[test]
    fn an_allowed_connect_tunnels_both_ways_with_the_bytes_sent_after_its_head() {
        let upstream = fixed(echo_server());
        let mut case = case(egress(upstream.clone()));
        let request = b"CONNECT allowed.test:443 HTTP/1.1\r\n\r\nearly-";
        let answer = ask(&mut case.client, request);
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
        case.client.write_all(b"late").unwrap();
        let mut echoed = [0_u8; 10];
        case.client.read_exact(&mut echoed).unwrap();
        assert_eq!(&echoed, b"early-late");
        leaves(case);
        assert_eq!(*upstream.asked.lock().unwrap(), ["allowed.test"]);
    }

    #[test]
    fn a_request_the_policy_refuses_gets_403_and_nothing_is_contacted() {
        for request in [
            "CONNECT other.test:443 HTTP/1.1\r\n\r\n",
            "CONNECT allowed.test:80 HTTP/1.1\r\n\r\n",
            "GET http://allowed.test/ HTTP/1.1\r\n\r\n",
            "CONNECT allowed.test:443 SMTP\r\n\r\n",
        ] {
            let upstream = fixed(echo_server());
            let mut case = case(egress(upstream.clone()));
            let answer = ask(&mut case.client, request.as_bytes());
            assert!(answer.starts_with("HTTP/1.1 403"), "{request}: {answer}");
            leaves(case);
            assert!(upstream.asked.lock().unwrap().is_empty(), "{request}");
        }
    }

    #[test]
    fn an_allowed_name_with_no_route_or_no_answer_gets_403() {
        // `nowhere.test` is allowed but has no address; the refusing
        // upstream stands for one whose addresses do not answer.
        let no_route: Arc<dyn Upstream> = fixed(echo_server());
        let no_answer: Arc<dyn Upstream> = Arc::new(Refusing);
        for (allow, upstream) in [("nowhere.test", no_route), ("allowed.test", no_answer)] {
            let mut case = case(Egress::new(vec![allow.into()], upstream));
            let request = format!("CONNECT {allow}:443 HTTP/1.1\r\n\r\n");
            let answer = ask(&mut case.client, request.as_bytes());
            assert!(answer.starts_with("HTTP/1.1 403"), "{allow}: {answer}");
            leaves(case);
        }
    }

    #[test]
    fn the_public_upstream_never_reaches_a_local_address() {
        let reached = Public.connect("localhost").unwrap();
        assert!(reached.is_none(), "localhost was reached");
    }

    #[test]
    fn a_head_that_is_too_long_is_refused_without_an_answer() {
        let mut case = case(egress(fixed(echo_server())));
        let long = vec![b'a'; egress::MAX_HEAD + 1];
        let _ = case.client.write_all(&long);
        let error = case.served.join().unwrap().unwrap_err();
        assert!(error.to_string().contains("too long"), "{error:#}");
        assert!(ends(&mut case.client));
    }

    #[test]
    fn closing_the_launch_ends_a_tunnel_that_is_still_open() {
        let mut case = case(egress(fixed(echo_server())));
        let answer = ask(&mut case.client, CONNECT);
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
        halted(&case.halt, case.served);
        assert!(ends(&mut case.client), "the tunnel went on after the close");
    }

    #[test]
    fn closing_the_launch_ends_a_tunnel_whose_destination_stopped_reading() {
        // The destination accepts and never reads, so the broker's sends
        // stall once the socket's buffers are full.
        let silent = serve_each(|stream| {
            std::thread::sleep(Duration::from_secs(60));
            drop(stream);
        });
        let mut case = case(egress(fixed(silent)));
        let answer = ask(&mut case.client, CONNECT);
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
        let mut pushing = case.client.try_clone().unwrap();
        let pushed = std::thread::spawn(move || {
            let chunk = vec![b'x'; 64 * 1024];
            let mut sent = 0_usize;
            while pushing.write_all(&chunk).is_ok() {
                sent += chunk.len();
            }
            sent
        });
        std::thread::sleep(Duration::from_secs(2));
        halted(&case.halt, case.served);
        drop(case.client);
        assert!(
            pushed.join().unwrap() > 0,
            "nothing was sent before the halt"
        );
    }

    #[test]
    fn closing_the_launch_ends_a_session_still_waiting_for_its_destination() {
        let mut case = case(egress(Arc::new(Stalled)));
        case.client.write_all(CONNECT).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        halted(&case.halt, case.served);
    }

    #[test]
    fn a_client_that_pauses_reading_still_gets_every_byte() {
        // Longer than a pipe write's patience for an answer.
        const PAUSE: Duration = Duration::from_secs(11);
        const SIZE: usize = 1024 * 1024;
        let sender = serve_each(|mut stream| {
            let _ = stream.write_all(&vec![b'd'; SIZE]);
        });
        let mut case = case(egress(fixed(sender)));
        let answer = ask(&mut case.client, CONNECT);
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
        std::thread::sleep(PAUSE);
        let mut got = vec![0_u8; SIZE];
        case.client.read_exact(&mut got).unwrap();
        assert!(got.iter().all(|&byte| byte == b'd'));
        leaves(case);
    }

    #[test]
    fn a_destination_that_ends_ends_the_session() {
        let address = serve_each(|mut stream| {
            stream.write_all(b"bye").unwrap();
        });
        let mut case = case(egress(fixed(address)));
        let answer = ask(&mut case.client, CONNECT);
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
        let mut last = [0_u8; 3];
        case.client.read_exact(&mut last).unwrap();
        assert_eq!(&last, b"bye", "the destination's last bytes were lost");
        leaves(case);
    }

    #[test]
    fn a_pipe_without_a_wake_event_cannot_stream() {
        let name = format!("agentc-test-egress-{}", uuid::Uuid::new_v4().simple());
        let me = token::current_user_sid().unwrap();
        let mut pipe = Pipe::create(&name, &me, &me, None).unwrap();
        let mut options = OpenOptions::new();
        options.access_mode(CLIENT_ACCESS);
        let mut client = options.open(pipe_path(&name).unwrap()).unwrap();
        client.write_all(b"x").unwrap();
        let Accepted::Client(mut session) = pipe.accept(PATIENCE).unwrap() else {
            panic!("the client was not admitted");
        };
        let refused = session.split().1.stream_all(b"y").unwrap_err();
        assert!(refused.to_string().contains("wake"), "{refused:#}");
    }
}
