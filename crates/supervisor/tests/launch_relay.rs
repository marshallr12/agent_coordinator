//! A real wrapped Claude reviewer launch (R-P3b.4, decision U21): the host
//! half started by `relay::start_host` and the built `netns-relay` carry the
//! proxy and staging ports into the launch's network namespace, for the
//! harness and for its nested candidate sandbox, while a host-only loopback
//! listener stays unreachable and the relay sockets cannot be replaced.
#![cfg(target_os = "linux")]

use agentc_supervisor::config::Config;
use agentc_supervisor::profile::{self, Harness, LaunchSpec, Role, run_files};
use agentc_supervisor::verification::Verification;
use agentc_supervisor::{confine, launch, relay, role_settings, sandbox};
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

/// A host loopback server answering each line uppercased.
fn line_echo() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            thread::spawn(move || {
                let mut writer = stream.try_clone().unwrap();
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    let _ = writeln!(writer, "{}", line.to_uppercase());
                }
            });
        }
    });
    address
}

/// A config under `root` whose relay is the built supervisor, with the given
/// proxy and staging servers.
fn config(root: &Path, proxy: SocketAddr, staging: SocketAddr) -> Config {
    let mut config = Config {
        state_dir: root.join("roles"),
        cargo_config_seed: root.join("cargo-seed.toml"),
        bin_dir: root.join("bin"),
        egress_listen: proxy.to_string(),
        ..Config::default()
    };
    let url = format!("http://127.0.0.1:{}/ui", staging.port());
    let staging = Verification {
        url,
        browser: false,
    };
    config.verification.insert("p1".into(), staging);
    fs::write(&config.cargo_config_seed, confine::CARGO_CONFIG_SEED).unwrap();
    fs::create_dir_all(&config.bin_dir).unwrap();
    let built = env!("CARGO_BIN_EXE_agentc-supervisor");
    fs::copy(built, relay::program(&config)).unwrap();
    config
}

/// A prepared reviewer run with its persistent Claude configuration.
fn reviewer(config: &Config) -> LaunchSpec {
    let base = config.state_dir.join(Role::Reviewer.slug());
    let spec = LaunchSpec {
        role: Role::Reviewer,
        harness: Harness::Claude,
        clone: base.join("clones/current"),
        run: base.join("runs/current"),
        model: "mock".into(),
        effort: "low".into(),
        session_id: uuid::Uuid::new_v4(),
        project: Some("p1".into()),
        task: None,
        push_socket: None,
    };
    fs::create_dir_all(&spec.clone).unwrap();
    launch::prepare_run(&spec, config).unwrap();
    fs::write(spec.run.join(run_files::PROMPT), "prompt").unwrap();
    let persistent = base.join("claude-config");
    fs::create_dir(&persistent).unwrap();
    fs::write(
        persistent.join("settings.json"),
        role_settings::render(Role::Reviewer),
    )
    .unwrap();
    fs::write(persistent.join("CLAUDE.md"), "").unwrap();
    fs::write(persistent.join(".credentials.json"), "fixture").unwrap();
    spec
}

/// Bash that exchanges one line with loopback `port` inside the namespace.
fn talk(port: u16) -> String {
    format!(
        "exec 3<>/dev/tcp/127.0.0.1/{port}; echo hi >&3; read -r r <&3; test \"$r\" = HI; exec 3>&-"
    )
}

/// Bash that fails with `code` if loopback `port` accepts a connection.
fn unreachable(port: u16, code: u8) -> String {
    format!("if (exec 4<>/dev/tcp/127.0.0.1/{port}) 2>/dev/null; then exit {code}; fi")
}

/// The harness script: both relays from the harness and from a candidate
/// command, no host-only port, and no change to `$RUN/net`.
fn script(proxy: u16, staging: u16, host_only: u16) -> String {
    let candidate = format!(
        "{} && {} && {}",
        talk(proxy),
        talk(staging),
        unreachable(host_only, 9)
    );
    [
        talk(proxy),
        talk(staging),
        unreachable(host_only, 9),
        "if touch \"$RUN/net/planted\" 2>/dev/null; then exit 11; fi".into(),
        "if rm \"$RUN/net/proxy.sock\" 2>/dev/null; then exit 12; fi".into(),
        "test -S \"$RUN/net/proxy.sock\"".into(),
        format!(
            // Like Claude Code, the harness names its own cwd file literally.
            "\"$CLAUDE_CODE_SHELL_PREFIX\" '{}'\" && pwd -P >| $TMPDIR/claude-relay-cwd\" || exit 7",
            candidate.replace('\'', r"'\''")
        ),
    ]
    .join("\n")
}

/// Waits up to 20 seconds for the launch; returns its exit code.
fn wait(mut child: std::process::Child) -> Option<i32> {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            return status.code();
        }
        thread::sleep(Duration::from_millis(20));
    }
    child.kill().unwrap();
    panic!("relayed launch timed out");
}

#[test]
fn a_wrapped_reviewer_reaches_only_the_relayed_host_services() {
    let root = tempfile::tempdir().unwrap();
    let (proxy, staging) = (line_echo(), line_echo());
    let host_only = TcpListener::bind("127.0.0.1:0").unwrap();
    let config = config(root.path(), proxy, staging);
    let spec = reviewer(&config);
    relay::start_host(&spec, &config).unwrap();
    let mut command = profile::command(&spec, &config);
    command.program = "/bin/bash".into();
    let text = script(
        proxy.port(),
        staging.port(),
        host_only.local_addr().unwrap().port(),
    );
    command.args = vec![OsString::from("-euc"), text.into()];
    command.env.push(("RUN".into(), spec.run.clone().into()));
    let wrapped = sandbox::wrap(command, &spec, &config).unwrap();
    let code = wait(launch::spawn(&wrapped, &spec).unwrap());
    let stderr = fs::read_to_string(spec.run.join("stderr.log")).unwrap();
    assert_eq!(code, Some(0), "launch failed: {stderr}");
}
