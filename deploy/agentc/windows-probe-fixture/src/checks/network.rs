//! Direct network egress, which no launch is allowed.
//!
//! The driver listens on every target and treats its own receipt log as the
//! authority; these rows add the error each attempt saw from inside.

use crate::report::Report;
use crate::spec::Spec;
use std::io;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Records a TCP connect, a UDP send and a name lookup for every listed target.
pub fn run(spec: &Spec, report: &mut Report) {
    for (name, address) in spec.group("tcp") {
        report.attack(&format!("net.tcp.{name}"), connect(address));
    }
    for (name, address) in spec.group("udp") {
        report.attack(&format!("net.udp.{name}"), send(address));
    }
    if let Some(name) = spec.get("dns_name") {
        report.attack("net.dns", resolve(name));
    }
}

/// Parses a socket address written by the driver.
fn address(text: &str) -> io::Result<SocketAddr> {
    text.parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bad address"))
}

/// Opens a TCP connection directly, bypassing any broker.
fn connect(target: &str) -> io::Result<()> {
    TcpStream::connect_timeout(&address(target)?, CONNECT_TIMEOUT).map(|_| ())
}

/// Sends one UDP datagram directly.
fn send(target: &str) -> io::Result<()> {
    let target = address(target)?;
    let local = if target.is_ipv6() {
        "[::]:0"
    } else {
        "0.0.0.0:0"
    };
    UdpSocket::bind(local)?
        .send_to(b"agentc-probe", target)
        .map(|_| ())
}

/// Resolves a public name through the system resolver.
fn resolve(name: &str) -> io::Result<()> {
    match (name, 443).to_socket_addrs()?.next() {
        Some(_) => Ok(()),
        None => Err(io::Error::from_raw_os_error(11001)),
    }
}
