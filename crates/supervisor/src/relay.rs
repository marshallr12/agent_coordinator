//! Loopback bridge for Claude launches in their own network namespace
//! (R-P3b.4, decision U21).
//!
//! A Claude launch runs with `--unshare-net`, so it cannot reach any host
//! loopback listener: not the other agent account's, nor the owner's. It
//! still needs exactly two host services, the egress proxy and (for a
//! verifying reviewer) a loopback staging coordinator. For each, the
//! supervisor listens on a Unix socket in `$RUN/net` (the host half, outside
//! the namespace, still under the host firewall) and relays to that one TCP
//! address. Inside the namespace, `agentc-supervisor netns-relay` listens on
//! the same loopback port and forwards to the socket (the namespace half),
//! then runs the harness. Pathname Unix sockets cross network namespaces;
//! nothing else does.
use crate::config::Config;
use crate::profile::LaunchSpec;
use anyhow::{Context, Result, bail, ensure};
use std::ffi::OsString;
use std::io;
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread;

/// One host TCP service reachable from inside the namespace.
#[derive(Debug, Clone, PartialEq)]
pub struct Endpoint {
    /// Short name; the socket is `$RUN/net/<name>.sock`.
    pub name: &'static str,
    /// Host loopback address the host half connects to; the namespace half
    /// listens on the same address inside the namespace.
    pub address: SocketAddr,
}

/// The supervisor binary that runs the namespace half inside the sandbox.
pub fn program(config: &Config) -> PathBuf {
    config.bin_dir.join("agentc-supervisor")
}

/// The directory holding a launch's relay sockets.
pub fn socket_dir(spec: &LaunchSpec) -> PathBuf {
    spec.run.join("net")
}

/// The socket an endpoint is relayed through.
pub fn socket_path(spec: &LaunchSpec, endpoint: &Endpoint) -> PathBuf {
    socket_dir(spec).join(format!("{}.sock", endpoint.name))
}

/// The endpoints a launch needs: always the egress proxy, a verifying
/// reviewer's staging coordinator when its URL is on loopback, and the
/// `[run.binding]` coordinator of a loop-claimed implementer when that is on
/// loopback (any other host is reached through the proxy).
pub fn endpoints(spec: &LaunchSpec, config: &Config) -> Result<Vec<Endpoint>> {
    let proxy = loopback(&config.egress_listen).context("egress_listen")?;
    let mut list = vec![Endpoint {
        name: "proxy",
        address: proxy,
    }];
    if let Some(address) = coordinator_address(spec, config)?
        && address != proxy
    {
        list.push(Endpoint {
            name: "coordinator",
            address,
        });
    }
    if let Some(verification) = crate::verification::for_launch(spec, config)
        && let Some(address) = staging_address(&verification.url)?
        && address != proxy
    {
        list.push(Endpoint {
            name: "staging",
            address,
        });
    }
    Ok(list)
}

/// The loopback address of the `[run.binding]` coordinator, for an
/// implementer launch the loop claimed a task for.
fn coordinator_address(spec: &LaunchSpec, config: &Config) -> Result<Option<SocketAddr>> {
    let implementer = spec.role == crate::profile::Role::Implementer && spec.task.is_some();
    match &config.run.binding {
        Some(binding) if implementer => staging_address(&binding.service_url),
        _ => Ok(None),
    }
}

/// The lowest port the namespace half can bind without privileges.
const MIN_PORT: u16 = 1024;

/// Parses an IPv4 loopback `ip:port` with an unprivileged port; anything
/// else is refused.
fn loopback(text: &str) -> Result<SocketAddr> {
    let address: SocketAddr = text
        .parse()
        .with_context(|| format!("{text:?} is not an ip:port"))?;
    if !address.is_ipv4() || !address.ip().is_loopback() {
        bail!("{text:?} must be an IPv4 loopback address");
    }
    ensure!(
        address.port() >= MIN_PORT,
        "{text:?} needs a port of at least {MIN_PORT}: the namespace relay binds it unprivileged"
    );
    Ok(address)
}

/// The loopback address an `http`/`https` staging URL reaches, or `None`
/// when it names another host (reached through the proxy). Loopback forms
/// the relay cannot carry faithfully (userinfo, IPv6, shorthand IPv4) and
/// privileged ports are refused rather than silently left unrelayed.
pub(crate) fn staging_address(url: &str) -> Result<Option<SocketAddr>> {
    let (scheme, rest) = url
        .split_once("://")
        .with_context(|| format!("staging URL {url:?} is not absolute"))?;
    let default_port = match scheme.to_ascii_lowercase().as_str() {
        "http" => 80,
        "https" => 443,
        _ => bail!("staging URL {url:?} must use http or https"),
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let (host, port) = host_and_port(&authority.to_ascii_lowercase(), default_port)?;
    let Some(ip) = loopback_host(&host)? else {
        return Ok(None);
    };
    loopback(&format!("{ip}:{port}")).map(Some)
}

/// Splits a lowercase URL authority into a host (trailing dot removed) and
/// a port (`default` when absent).
fn host_and_port(authority: &str, default: u16) -> Result<(String, u16)> {
    ensure!(
        !authority.contains('@'),
        "staging URLs cannot carry userinfo"
    );
    ensure!(
        !authority.starts_with('['),
        "IPv6 staging URLs are not supported"
    );
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, port.parse().context("staging URL port")?),
        None => (authority, default),
    };
    Ok((host.strip_suffix('.').unwrap_or(host).to_owned(), port))
}

/// The IPv4 address of a loopback host, `None` for any other host, and an
/// error for numeric forms other than dotted-quad (`127.1`, `0x7f.1`).
fn loopback_host(host: &str) -> Result<Option<Ipv4Addr>> {
    if host == "localhost" || host.ends_with(".localhost") {
        return Ok(Some(Ipv4Addr::LOCALHOST));
    }
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        return Ok(ip.is_loopback().then_some(ip));
    }
    let numeric = |label: &str| {
        let hex = label.strip_prefix("0x").filter(|rest| !rest.is_empty());
        hex.map_or(
            !label.is_empty() && label.bytes().all(|b| b.is_ascii_digit()),
            |rest| rest.bytes().all(|b| b.is_ascii_hexdigit()),
        )
    };
    ensure!(
        !host.split('.').all(numeric),
        "staging host {host:?} must be a dotted-quad IPv4 address"
    );
    Ok(None)
}

/// A stream whose sending side can be closed on its own.
trait HalfClose: io::Write {
    fn close_write(&self);
}

impl HalfClose for TcpStream {
    fn close_write(&self) {
        let _ = self.shutdown(Shutdown::Write);
    }
}

impl HalfClose for UnixStream {
    fn close_write(&self) {
        let _ = self.shutdown(Shutdown::Write);
    }
}

/// Copies until `from` finishes sending, then half-closes `to`.
fn copy_then_close(mut from: impl io::Read, mut to: impl HalfClose) {
    let _ = io::copy(&mut from, &mut to);
    to.close_write();
}

/// Copies bytes both ways until each side has finished sending.
fn pump(tcp: TcpStream, unix: UnixStream) {
    let (Ok(tcp_back), Ok(unix_back)) = (tcp.try_clone(), unix.try_clone()) else {
        return;
    };
    let forward = thread::spawn(move || copy_then_close(tcp, unix));
    copy_then_close(unix_back, tcp_back);
    let _ = forward.join();
}

/// Host half: listens on `socket` and relays each connection to `target`.
/// The listener's thread lives as long as the process (one launch).
pub fn serve_host(socket: &Path, target: SocketAddr) -> Result<()> {
    let listener =
        UnixListener::bind(socket).with_context(|| format!("bind {}", socket.display()))?;
    thread::spawn(move || {
        for unix in listener.incoming().flatten() {
            thread::spawn(move || {
                if let Ok(tcp) = TcpStream::connect(target) {
                    pump(tcp, unix);
                }
            });
        }
    });
    Ok(())
}

/// Starts the host half for every endpoint of `spec` in a fresh, private
/// `$RUN/net`.
pub fn start_host(spec: &LaunchSpec, config: &Config) -> Result<()> {
    crate::confine::private_dir(&socket_dir(spec)).context("create relay directory")?;
    for endpoint in endpoints(spec, config)? {
        serve_host(&socket_path(spec, &endpoint), endpoint.address)?;
    }
    Ok(())
}

/// Namespace half for one endpoint: listens on `address` and forwards each
/// connection to `socket`. Returns the bound address.
pub fn serve_namespace(address: SocketAddr, socket: PathBuf) -> Result<SocketAddr> {
    let listener = TcpListener::bind(address).with_context(|| format!("listen on {address}"))?;
    let bound = listener.local_addr()?;
    thread::spawn(move || {
        for tcp in listener.incoming().flatten() {
            let socket = socket.clone();
            thread::spawn(move || {
                if let Ok(unix) = UnixStream::connect(&socket) {
                    pump(tcp, unix);
                }
            });
        }
    });
    Ok(bound)
}

/// The `netns-relay` arguments for `spec`: one `--relay ip:port=socket` per
/// endpoint, then `--`. The harness command follows.
pub fn namespace_args(spec: &LaunchSpec, config: &Config) -> Result<Vec<OsString>> {
    let mut args: Vec<OsString> = vec!["netns-relay".into()];
    for endpoint in endpoints(spec, config)? {
        let mut relay = OsString::from(format!("{}=", endpoint.address));
        relay.push(socket_path(spec, &endpoint));
        args.extend(["--relay".into(), relay]);
    }
    args.push("--".into());
    Ok(args)
}

/// Parses one `ip:port=socket` relay argument.
pub fn parse_relay(text: &str) -> Result<(SocketAddr, PathBuf)> {
    let (address, socket) = text
        .split_once('=')
        .context("relay must be ip:port=socket")?;
    Ok((loopback(address)?, PathBuf::from(socket)))
}

/// Runs the namespace half: starts every relay, then the command, and
/// returns its exit code (128 + signal when it was killed).
pub fn run_namespace(relays: &[String], command: &[OsString]) -> Result<i32> {
    for relay in relays {
        let (address, socket) = parse_relay(relay)?;
        serve_namespace(address, socket)?;
    }
    let (program, args) = command
        .split_first()
        .context("netns-relay needs a command")?;
    let status = std::process::Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("run {}", Path::new(program).display()))?;
    Ok(exit_code(status))
}

/// A shell-style exit code for `status`.
fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Harness, Role};
    use crate::verification::Verification;
    use std::io::{Read, Write};

    /// A launch spec for `role` on project "p1" under `run`.
    fn spec(role: Role, run: &Path) -> LaunchSpec {
        LaunchSpec {
            role,
            harness: Harness::Claude,
            clone: "/w/clone".into(),
            run: run.into(),
            model: "m".into(),
            effort: "low".into(),
            session_id: uuid::Uuid::nil(),
            project: Some("p1".into()),
            task: None,
            push_socket: None,
        }
    }

    /// A config whose project "p1" verifies against `url`.
    fn config(url: &str) -> Config {
        let mut config = Config::default();
        let verification = Verification {
            url: url.into(),
            browser: false,
        };
        config.verification.insert("p1".into(), verification);
        config
    }

    /// A TCP server that echoes one line per connection, uppercased.
    fn echo_server() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut text = String::new();
                stream.read_to_string(&mut text).unwrap();
                stream.write_all(text.to_uppercase().as_bytes()).unwrap();
            }
        });
        address
    }

    /// Sends `text` on `stream`, half-closes it and returns the reply.
    fn exchange<S: Read + Write>(mut stream: S, text: &str, close: impl Fn(&S)) -> String {
        stream.write_all(text.as_bytes()).unwrap();
        close(&stream);
        let mut reply = String::new();
        stream.read_to_string(&mut reply).unwrap();
        reply
    }

    #[test]
    fn proxy_always_and_loopback_staging_only_for_verifying_reviewers() {
        let run = Path::new("/w/run");
        let staging = config("http://127.0.0.1:18080/ui");
        let names = |role, config: &Config| -> Vec<&str> {
            let list = endpoints(&spec(role, run), config).unwrap();
            list.iter().map(|e| e.name).collect()
        };
        assert_eq!(names(Role::Reviewer, &staging), ["proxy", "staging"]);
        assert_eq!(names(Role::Implementer, &staging), ["proxy"]);
        for remote in [
            "https://staging.example.com",
            "http://10.0.0.5:80/",
            "http://cafe.de",
        ] {
            assert_eq!(
                names(Role::Reviewer, &config(remote)),
                ["proxy"],
                "{remote}"
            );
        }
    }

    #[test]
    fn a_loopback_run_binding_is_relayed_only_for_claimed_implementer_launches() {
        let run = Path::new("/w/run");
        let mut config = Config::default();
        let bind = |url: &str| crate::run_loop::binding::Binding {
            service_url: url.into(),
            project_id: "p1".into(),
            project_name: None,
        };
        config.run.binding = Some(bind("http://127.0.0.1:18080"));
        let mut claimed = spec(Role::Implementer, run);
        claimed.task = Some("t1".into());
        let names = |spec: &LaunchSpec, config: &Config| -> Vec<&str> {
            let list = endpoints(spec, config).unwrap();
            list.iter().map(|e| e.name).collect()
        };
        assert_eq!(names(&claimed, &config), ["proxy", "coordinator"]);
        assert_eq!(names(&spec(Role::Implementer, run), &config), ["proxy"]);
        assert_eq!(names(&spec(Role::Reviewer, run), &config), ["proxy"]);
        config.run.binding = Some(bind("https://agents.example.com"));
        assert_eq!(names(&claimed, &config), ["proxy"]);
    }

    #[test]
    fn every_loopback_spelling_is_relayed_to_its_port() {
        for (url, port) in [
            ("http://localhost:18081", 18081),
            ("http://LOCALHOST:18082/", 18082),
            ("HTTP://127.0.0.1:18083", 18083),
            ("http://localhost.:18084", 18084),
            ("http://app.localhost:18085?x", 18085),
            ("https://127.0.0.1:18443#top", 18443),
            ("http://127.0.0.2:18086", 18086),
        ] {
            let expected = SocketAddr::from(([127, 0, 0, if port == 18086 { 2 } else { 1 }], port));
            assert_eq!(staging_address(url).unwrap(), Some(expected), "{url}");
        }
    }

    #[test]
    fn loopback_forms_the_relay_cannot_carry_are_refused() {
        for url in [
            "http://user:pw@127.0.0.1:18080/",
            "http://127.0.0.1/",
            "http://localhost/",
            "https://localhost",
            "http://127.1:18080",
            "http://2130706433:18080",
            "http://0x7f.0.0.1:18080",
            "http://[::1]:18080",
            "http://127.0.0.1:0",
            "ftp://127.0.0.1:18080",
            "127.0.0.1:18080",
        ] {
            assert!(staging_address(url).is_err(), "{url}");
        }
    }

    #[test]
    fn non_loopback_ipv6_or_privileged_relay_addresses_are_refused() {
        for bad in [
            "0.0.0.0:3128",
            "10.1.1.1:3128",
            "[::1]:3128",
            "proxy:3128",
            "127.0.0.1:0",
            "127.0.0.1:80",
            "127.0.0.1:1023",
        ] {
            assert!(loopback(bad).is_err(), "{bad}");
        }
        assert!(loopback("127.0.0.1:1024").is_ok());
        assert!(parse_relay("127.0.0.1:3128").is_err());
        let (address, socket) = parse_relay("127.0.0.1:3128=/r/net/proxy.sock").unwrap();
        assert_eq!((address.port(), socket), (3128, "/r/net/proxy.sock".into()));
    }

    #[test]
    fn namespace_args_name_each_socket_in_the_run() {
        let run = Path::new("/w/run");
        let args = namespace_args(
            &spec(Role::Reviewer, run),
            &config("http://127.0.0.1:18080"),
        );
        let args: Vec<String> = args
            .unwrap()
            .into_iter()
            .map(|a| a.into_string().unwrap())
            .collect();
        assert_eq!(
            args,
            [
                "netns-relay",
                "--relay",
                "127.0.0.1:3128=/w/run/net/proxy.sock",
                "--relay",
                "127.0.0.1:18080=/w/run/net/staging.sock",
                "--",
            ]
        );
    }

    #[test]
    fn both_halves_carry_a_full_exchange_in_each_direction() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("echo.sock");
        serve_host(&socket, echo_server()).unwrap();
        let unix = UnixStream::connect(&socket).unwrap();
        let reply = exchange(unix, "via host half", |s| {
            s.shutdown(Shutdown::Write).unwrap()
        });
        assert_eq!(reply, "VIA HOST HALF");
        let inner = serve_namespace("127.0.0.1:0".parse().unwrap(), socket).unwrap();
        let tcp = TcpStream::connect(inner).unwrap();
        let reply = exchange(tcp, "through both", |s| {
            s.shutdown(Shutdown::Write).unwrap()
        });
        assert_eq!(reply, "THROUGH BOTH");
    }

    #[test]
    fn the_namespace_half_returns_the_command_status() {
        let code = |script: &str| {
            let command = ["/bin/sh", "-c", script].map(OsString::from);
            run_namespace(&[], &command).unwrap()
        };
        assert_eq!(code("exit 3"), 3);
        assert_eq!(code("kill -TERM $$"), 128 + libc::SIGTERM);
        assert!(run_namespace(&["bad".into()], &[]).is_err());
    }
}
