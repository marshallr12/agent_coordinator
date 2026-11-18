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
use anyhow::{Context, Result, anyhow, ensure};
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

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
    fn route(&self, grant: &Grant, head: &str) -> Option<TcpStream> {
        let target = egress::permitted_target(head, &self.allow);
        let stream = target.as_deref().and_then(|host| {
            let reached = self.upstream.connect(host);
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
        let Some(stream) = self.route(grant, &head) else {
            session.write_all(egress::FORBIDDEN)?;
            return linger(session, halt);
        };
        session.write_all(egress::ESTABLISHED)?;
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

/// Carries bytes between the session and `upstream`: `early` (sent with the
/// head) first, then the client's bytes up from one thread and the
/// destination's down from this one. Whichever direction ends first ends
/// the other within a tick; the broker's halt ends both.
fn tunnel(session: &mut Session<'_>, upstream: TcpStream, early: &[u8], halt: &Halt) -> Result<()> {
    upstream.set_read_timeout(Some(TICK))?;
    let mut sending = upstream.try_clone()?;
    sending.write_all(early)?;
    let ended = AtomicBool::new(false);
    let (mut reader, mut writer) = session.split();
    std::thread::scope(|scope| {
        let upward = || up(&mut reader, &mut sending, halt, &ended);
        let upward = scope.spawn(|| ending(&ended, &upstream, upward));
        let downward = ending(&ended, &upstream, || {
            down(&upstream, &mut writer, halt, &ended)
        });
        let upward = upward.join();
        let upward = upward.map_err(|_| anyhow!("the tunnel's upward thread panicked"))?;
        downward.and(upward)
    })
}

/// Runs one direction of a tunnel, then marks the tunnel ended and shuts
/// the destination's connection, which ends the other direction's read.
fn ending(
    ended: &AtomicBool,
    upstream: &TcpStream,
    direction: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let carried = direction();
    ended.store(true, Ordering::SeqCst);
    let _ = upstream.shutdown(Shutdown::Both);
    carried
}

/// Whether a tunnel direction should stop.
fn stopping(halt: &Halt, ended: &AtomicBool) -> bool {
    halt.asked() || ended.load(Ordering::SeqCst)
}

/// The client's bytes to the destination, until the client leaves.
fn up(
    reader: &mut Reader<'_>,
    sending: &mut TcpStream,
    halt: &Halt,
    ended: &AtomicBool,
) -> Result<()> {
    while !stopping(halt, ended) {
        match reader.read(CHUNK, TICK)? {
            Received::Data(data) => sending.write_all(&data)?,
            Received::Idle => {}
            Received::Closed => break,
        }
    }
    Ok(())
}

/// The destination's bytes to the client, until the destination ends.
fn down(
    upstream: &TcpStream,
    writer: &mut Writer<'_>,
    halt: &Halt,
    ended: &AtomicBool,
) -> Result<()> {
    let (mut upstream, mut buffer) = (upstream, vec![0_u8; CHUNK]);
    while !stopping(halt, ended) {
        match upstream.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => writer.write_all(&buffer[..count])?,
            Err(error) if waited(&error) => {}
            Err(error) => return Err(error).context("read from the destination"),
        }
    }
    Ok(())
}

/// Whether a read error only says that nothing came within the timeout.
fn waited(error: &std::io::Error) -> bool {
    matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
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

    /// A package no token on this host carries; the handler never checks it.
    const PACKAGE: &str = "S-1-15-2-1-2-3-4-5-6-7";
    const PATIENCE: Duration = Duration::from_secs(10);

    /// Routes names to fixed local addresses and records what it was asked.
    #[derive(Default)]
    struct Fixed {
        routes: BTreeMap<String, SocketAddr>,
        asked: Mutex<Vec<String>>,
    }

    impl Upstream for Fixed {
        fn connect(&self, host: &str) -> Result<Option<TcpStream>> {
            self.asked.lock().unwrap().push(host.to_owned());
            let address = self.routes.get(host).copied();
            connect_first(address.as_slice())
        }
    }

    /// A destination that sends back what it gets, then ends when its
    /// client does. Its address.
    fn echo_server() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut back = stream.try_clone().unwrap();
                std::thread::spawn(move || std::io::copy(&mut stream, &mut back));
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

    /// A grant of this account for the handler.
    fn grant() -> Grant {
        let me = token::current_user_sid().unwrap();
        Grant::new("egress-test", Role::Implementer, &me, PACKAGE, 1).unwrap()
    }

    /// A pipe of this account and its client, connected.
    fn pipe_and_client() -> (Pipe, File) {
        let name = format!("agentc-test-egress-{}", uuid::Uuid::new_v4().simple());
        let me = token::current_user_sid().unwrap();
        let pipe = Pipe::create(&name, &me, &me, None).unwrap();
        let mut options = OpenOptions::new();
        options.access_mode(CLIENT_ACCESS);
        let client = options.open(pipe_path(&name).unwrap()).unwrap();
        (pipe, client)
    }

    /// Serves the client of `pipe` with `egress` on a thread; the thread
    /// returns what `serve` did. `halt` lets the test close the launch.
    fn serving(
        mut pipe: Pipe,
        egress: Egress,
        halt: Arc<Halt>,
    ) -> std::thread::JoinHandle<Result<()>> {
        std::thread::spawn(move || {
            let Accepted::Client(mut session) = pipe.accept(PATIENCE)? else {
                anyhow::bail!("the client was not admitted");
            };
            egress.serve(&grant(), &mut session, &halt)
        })
    }

    /// Sends `request` and reads the answer's status line.
    fn ask(client: &mut File, request: &[u8]) -> String {
        client.write_all(request).unwrap();
        let mut answer = Vec::new();
        let mut byte = [0_u8; 1];
        while !answer.ends_with(b"\r\n\r\n") && client.read(&mut byte).unwrap() == 1 {
            answer.push(byte[0]);
        }
        String::from_utf8(answer).unwrap()
    }

    /// Drops `client` and checks that the session it had then ends at once,
    /// well within the broker's wait for a client to leave.
    fn leaves(client: File, served: std::thread::JoinHandle<Result<()>>) {
        let started = Instant::now();
        drop(client);
        served.join().unwrap().unwrap();
        let took = started.elapsed();
        assert!(
            took < LINGER / 2,
            "the session ended {took:?} after the client left"
        );
    }

    /// Whether the pipe has nothing more for the client: it was closed.
    fn ends(client: &mut File) -> bool {
        let mut rest = Vec::new();
        client
            .read_to_end(&mut rest)
            .map_or(true, |_| rest.is_empty())
    }

    #[test]
    fn an_allowed_connect_tunnels_both_ways_with_the_bytes_sent_after_its_head() {
        let upstream = fixed(echo_server());
        let (pipe, mut client) = pipe_and_client();
        let egress = Egress::new(vec!["allowed.test".into()], upstream.clone());
        let served = serving(pipe, egress, Arc::new(Halt::new().unwrap()));
        let answer = ask(
            &mut client,
            b"CONNECT allowed.test:443 HTTP/1.1\r\n\r\nearly-",
        );
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
        client.write_all(b"late").unwrap();
        let mut echoed = [0_u8; 10];
        client.read_exact(&mut echoed).unwrap();
        assert_eq!(&echoed, b"early-late");
        drop(client);
        served.join().unwrap().unwrap();
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
            let (pipe, mut client) = pipe_and_client();
            let egress = Egress::new(vec!["allowed.test".into()], upstream.clone());
            let served = serving(pipe, egress, Arc::new(Halt::new().unwrap()));
            let answer = ask(&mut client, request.as_bytes());
            assert!(answer.starts_with("HTTP/1.1 403"), "{request}: {answer}");
            leaves(client, served);
            assert!(upstream.asked.lock().unwrap().is_empty(), "{request}");
        }
    }

    #[test]
    fn an_allowed_name_with_no_route_or_no_answer_gets_403() {
        let closed = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        for allow in ["nowhere.test", "allowed.test"] {
            let (pipe, mut client) = pipe_and_client();
            let egress = Egress::new(vec![allow.into()], fixed(closed));
            let served = serving(pipe, egress, Arc::new(Halt::new().unwrap()));
            let request = format!("CONNECT {allow}:443 HTTP/1.1\r\n\r\n");
            let answer = ask(&mut client, request.as_bytes());
            assert!(answer.starts_with("HTTP/1.1 403"), "{allow}: {answer}");
            leaves(client, served);
        }
    }

    #[test]
    fn the_public_upstream_never_reaches_a_local_address() {
        let reached = Public.connect("localhost").unwrap();
        assert!(reached.is_none(), "localhost was reached");
    }

    #[test]
    fn a_head_that_is_too_long_is_refused_without_an_answer() {
        let (pipe, mut client) = pipe_and_client();
        let egress = Egress::new(vec!["allowed.test".into()], fixed(echo_server()));
        let served = serving(pipe, egress, Arc::new(Halt::new().unwrap()));
        let long = vec![b'a'; egress::MAX_HEAD + 1];
        let _ = client.write_all(&long);
        let error = served.join().unwrap().unwrap_err();
        assert!(error.to_string().contains("too long"), "{error:#}");
        assert!(ends(&mut client));
    }

    #[test]
    fn closing_the_launch_ends_a_tunnel_that_is_still_open() {
        let (pipe, mut client) = pipe_and_client();
        let halt = Arc::new(Halt::new().unwrap());
        let egress = Egress::new(vec!["allowed.test".into()], fixed(echo_server()));
        let served = serving(pipe, egress, halt.clone());
        let answer = ask(&mut client, b"CONNECT allowed.test:443 HTTP/1.1\r\n\r\n");
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
        let started = Instant::now();
        halt.ask().unwrap();
        served.join().unwrap().unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        assert!(ends(&mut client), "the tunnel went on after the close");
    }

    #[test]
    fn a_destination_that_ends_ends_the_session() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(b"bye").unwrap();
        });
        let (pipe, mut client) = pipe_and_client();
        let egress = Egress::new(vec!["allowed.test".into()], fixed(address));
        let served = serving(pipe, egress, Arc::new(Halt::new().unwrap()));
        let answer = ask(&mut client, b"CONNECT allowed.test:443 HTTP/1.1\r\n\r\n");
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
        let mut last = [0_u8; 3];
        client.read_exact(&mut last).unwrap();
        assert_eq!(&last, b"bye", "the destination's last bytes were lost");
        leaves(client, served);
    }
}
