//! Active containment probes for preflight: prove, from the launch's own
//! identity, that the root-owned firewall and the egress proxy are in force.
//! A flushed ruleset or a dead proxy would otherwise go unnoticed, because
//! every static check still passes.
use std::io::{self, BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

/// How long each probe waits to connect or for the proxy's reply.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Direct egress to `target` must fail; reaching it means no firewall.
pub fn direct_egress_problem(target: &str, timeout: Duration) -> Option<String> {
    let Some(address) = resolve(target) else {
        return Some(format!("egress probe target {target:?} is not an address"));
    };
    classify_direct(TcpStream::connect_timeout(&address, timeout))
}

/// A successful direct connect is the only failing outcome: timeouts,
/// refusals and EPERM/EHOSTUNREACH all mean the firewall held.
fn classify_direct<T>(result: io::Result<T>) -> Option<String> {
    result
        .is_ok()
        .then(|| "firewall not enforcing: direct egress reachable".to_owned())
}

/// The proxy on `listen` must be up and refuse `blocked_host` with a 403.
pub fn proxy_problem(listen: &str, blocked_host: &str, timeout: Duration) -> Option<String> {
    let Some(address) = resolve(listen) else {
        return Some(format!("egress_listen {listen:?} is not an address"));
    };
    let reply = TcpStream::connect_timeout(&address, timeout)
        .map_err(|error| connect_error(listen, &error))
        .and_then(|stream| ask_proxy(stream, blocked_host, timeout).map_err(|e| e.to_string()));
    match reply {
        Ok(line) => classify_reply(&line, blocked_host),
        Err(problem) => Some(problem),
    }
}

/// Resolves a `host:port` string to its first socket address.
fn resolve(target: &str) -> Option<SocketAddr> {
    target.to_socket_addrs().ok()?.next()
}

/// Names a refused connection as a stopped proxy; anything else verbatim.
fn connect_error(listen: &str, error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::ConnectionRefused => format!("egress proxy not running on {listen}"),
        _ => format!("egress proxy {listen} unreachable: {error}"),
    }
}

/// Sends `CONNECT blocked_host:443` and returns the reply's status line.
fn ask_proxy(mut stream: TcpStream, blocked_host: &str, timeout: Duration) -> io::Result<String> {
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let request =
        format!("CONNECT {blocked_host}:443 HTTP/1.1\r\nHost: {blocked_host}:443\r\n\r\n");
    stream.write_all(request.as_bytes())?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    Ok(line)
}

/// Only a 403 proves the proxy is filtering.
fn classify_reply(line: &str, blocked_host: &str) -> Option<String> {
    (status_code(line) != Some(403)).then(|| {
        format!(
            "egress proxy not filtering: CONNECT {blocked_host}:443 answered {:?}",
            line.trim_end()
        )
    })
}

/// Parses the status code from an `HTTP/1.x NNN reason` status line.
fn status_code(line: &str) -> Option<u16> {
    let mut parts = line.split_ascii_whitespace();
    let version = parts.next()?;
    let code = parts.next()?;
    (version.starts_with("HTTP/1.") && code.len() == 3)
        .then(|| code.parse().ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;

    const SHORT: Duration = Duration::from_millis(500);

    /// A one-shot fake proxy that answers any request head with `reply`.
    fn fake_proxy(reply: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            let (mut client, _) = listener.accept().unwrap();
            let mut head = [0u8; 256];
            let _ = client.read(&mut head);
            let _ = client.write_all(reply.as_bytes());
        });
        address
    }

    /// A loopback address with nothing listening (bound, then released).
    fn closed_port() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().to_string()
    }

    #[test]
    fn status_lines_parse_only_http1_codes() {
        assert_eq!(status_code("HTTP/1.1 403 Forbidden\r\n"), Some(403));
        assert_eq!(
            status_code("HTTP/1.0 200 Connection Established"),
            Some(200)
        );
        assert_eq!(status_code("SSH-2.0-OpenSSH"), None);
        assert_eq!(status_code("HTTP/1.1 4033 x"), None);
        assert_eq!(status_code(""), None);
    }

    #[test]
    fn only_a_failed_direct_connect_passes() {
        assert!(classify_direct(Ok(())).unwrap().contains("not enforcing"));
        let refused = io::Error::from(io::ErrorKind::ConnectionRefused);
        assert_eq!(classify_direct::<()>(Err(refused)), None);
        let timed_out = io::Error::from(io::ErrorKind::TimedOut);
        assert_eq!(classify_direct::<()>(Err(timed_out)), None);
    }

    #[test]
    fn reachable_target_is_a_firewall_problem() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = listener.local_addr().unwrap().to_string();
        let problem = direct_egress_problem(&target, SHORT).unwrap();
        assert!(problem.contains("direct egress reachable"), "{problem}");
        assert_eq!(direct_egress_problem(&closed_port(), SHORT), None);
    }

    #[test]
    fn filtering_proxy_passes_and_tunnelling_proxy_fails() {
        let filtering = fake_proxy("HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n");
        assert_eq!(proxy_problem(&filtering, "blocked.invalid", SHORT), None);
        let open = fake_proxy("HTTP/1.1 200 Connection Established\r\n\r\n");
        let problem = proxy_problem(&open, "blocked.invalid", SHORT).unwrap();
        assert!(problem.contains("not filtering"), "{problem}");
    }

    #[test]
    fn stopped_proxy_is_reported_as_not_running() {
        let problem = proxy_problem(&closed_port(), "blocked.invalid", SHORT).unwrap();
        assert!(problem.contains("egress proxy not running"), "{problem}");
    }

    #[test]
    fn silent_proxy_times_out_as_a_problem() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let problem = proxy_problem(&address, "blocked.invalid", SHORT).unwrap();
        assert!(!problem.is_empty());
        drop(listener);
    }
}
