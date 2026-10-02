//! Runs the real `agentc-supervisor netns-relay` inside a Bubblewrap network
//! namespace (R-P3b.4, decision U21): a relayed loopback port reaches its
//! host socket, while an unrelayed host loopback listener stays unreachable.
#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::Command;
use std::thread;

/// Serves `socket`, answering each line with the same line uppercased.
fn line_echo(socket: &Path) {
    let listener = UnixListener::bind(socket).unwrap();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut writer = stream.try_clone().unwrap();
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                writeln!(writer, "{}", line.to_uppercase()).unwrap();
            }
        }
    });
}

/// The in-namespace check: talk through port 3128, then fail if the host
/// listener on `host_port` answers.
fn script(host_port: u16) -> String {
    format!(
        "exec 3<>/dev/tcp/127.0.0.1/3128; echo hello >&3; read -r reply <&3; \
         test \"$reply\" = HELLO || exit 8; \
         if (exec 4<>/dev/tcp/127.0.0.1/{host_port}) 2>/dev/null; then exit 9; fi; exit 5"
    )
}

#[test]
fn relayed_port_reaches_its_socket_and_other_host_ports_do_not() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("proxy.sock");
    line_echo(&socket);
    let host = TcpListener::bind("127.0.0.1:0").unwrap();
    let relay = format!("127.0.0.1:3128={}", socket.display());
    let status = Command::new("/usr/bin/bwrap")
        .args(["--unshare-user", "--unshare-net", "--die-with-parent"])
        .args([
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            "--",
        ])
        .arg(env!("CARGO_BIN_EXE_agentc-supervisor"))
        .args(["netns-relay", "--relay", &relay, "--", "/bin/bash", "-c"])
        .arg(script(host.local_addr().unwrap().port()))
        .status()
        .unwrap();
    // 5 is the script's own success code, passed back through the relay.
    assert_eq!(status.code(), Some(5));
}
