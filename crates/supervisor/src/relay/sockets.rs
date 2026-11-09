//! The socket half of the loopback bridge: the host half listening on a Unix
//! socket in `$RUN/net`, and the namespace half that `netns-relay` runs.
//! Pathname Unix sockets cross network namespaces, so this half is Unix-only.
use super::{endpoints, parse_relay, socket_dir, socket_path};
use crate::config::Config;
use crate::profile::LaunchSpec;
use anyhow::{Context, Result};
use std::ffi::OsString;
use std::io;
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread;

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
