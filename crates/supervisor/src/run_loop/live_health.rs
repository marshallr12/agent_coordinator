//! The live driver's vendor checks: each harness's own sign-in status, run
//! as the role with the same credential a launch gets, the Claude token's
//! expiry, Codex's usage windows, and a finished launch's event stream read
//! as root without following the role's symlinks.
use super::usage::{self, Window};
use super::{Launch, rooted};
use crate::config::Config;
use crate::profile::{Harness, Role};
use crate::push_helper::accounts::{self, Account};
use anyhow::{Context, Result, bail, ensure};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// The longest a sign-in check may take.
const STATUS_TIMEOUT: Duration = Duration::from_secs(30);
/// The largest event stream read for cost and rate limits.
const MAX_EVENTS: u64 = 256 * 1024 * 1024;

/// `<state_dir>/<role slug>`.
fn role_dir(config: &Config, role: Role) -> PathBuf {
    config.state_dir.join(role.slug())
}

/// Whether the harness's binary is installed in the pinned directory.
pub fn installed(config: &Config, harness: Harness) -> bool {
    let program = match harness {
        Harness::Claude => "claude",
        Harness::Codex => "codex",
    };
    (config.bin_dir.join(program).metadata()).is_ok_and(|meta| meta.is_file())
}

/// Runs `claude auth status` (with the role's token) or `codex login
/// status` as `role`; a non-zero exit means not signed in. Only the exit
/// code is used, so nothing the check prints reaches the logs.
pub fn status(config: &Config, account: &Account, role: Role, harness: Harness) -> Result<()> {
    let (program, args): (&str, &[&str]) = match harness {
        Harness::Claude => ("claude", &["auth", "status", "--json"]),
        Harness::Codex => ("codex", &["login", "status"]),
    };
    let mut command = Command::new(config.bin_dir.join(program));
    command
        .args(args)
        .env_clear()
        .envs(environment(config, role, harness));
    if harness == Harness::Claude {
        let token = crate::launch::role_token(role, config)?;
        command.env(crate::profile::CLAUDE_TOKEN_ENV, token);
    }
    command.current_dir("/").stdin(Stdio::null());
    command.stdout(Stdio::null()).stderr(Stdio::null());
    crate::reaper::forbid_new_privileges(&mut command);
    accounts::run_as(&mut command, account);
    let child = command.spawn().with_context(|| format!("run {program}"))?;
    let status = bounded(child)?;
    ensure!(status.success(), "{program} reports no sign-in ({status})");
    Ok(())
}

/// The check's environment: pinned binaries, the role's home and the
/// harness's persistent config directory, through the egress proxy.
fn environment(config: &Config, role: Role, harness: Harness) -> Vec<(&'static str, String)> {
    let dir = role_dir(config, role);
    let proxy = config.egress_proxy_url();
    let path = format!("{}:/usr/bin:/bin", config.bin_dir.display());
    let home = match harness {
        Harness::Claude => ("CLAUDE_CONFIG_DIR", dir.join("claude-config")),
        Harness::Codex => ("CODEX_HOME", dir.join("codex-home")),
    };
    vec![
        ("PATH", path),
        ("HOME", dir.join("home").display().to_string()),
        ("LANG", "C.UTF-8".into()),
        ("DISABLE_AUTOUPDATER", "1".into()),
        ("HTTPS_PROXY", proxy.clone()),
        ("https_proxy", proxy),
        (home.0, home.1.display().to_string()),
    ]
}

/// Waits for `child` at most [`STATUS_TIMEOUT`], killing it after that.
fn bounded(mut child: Child) -> Result<ExitStatus> {
    let deadline = Instant::now() + STATUS_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("the sign-in check timed out");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The longest the usage query may take.
const USAGE_TIMEOUT: Duration = Duration::from_secs(30);

/// Asks `codex app-server`, the documented JSON-RPC interface of the Codex
/// clients, for the role's account usage windows
/// (`account/rateLimits/read`), as the role with its own `CODEX_HOME`. No
/// inference runs. Claude has no such interface: `None`.
pub fn poll_usage(
    config: &Config,
    account: &Account,
    role: Role,
    harness: Harness,
) -> Result<Option<Vec<Window>>> {
    if harness != Harness::Codex {
        return Ok(None);
    }
    let mut command = Command::new(config.bin_dir.join("codex"));
    command
        .arg("app-server")
        .env_clear()
        .envs(environment(config, role, harness));
    command.current_dir("/").stdin(Stdio::piped());
    command.stdout(Stdio::piped()).stderr(Stdio::null());
    crate::reaper::forbid_new_privileges(&mut command);
    accounts::run_as(&mut command, account);
    let mut child = command.spawn().context("run codex app-server")?;
    let answer = ask_usage(&mut child);
    let _ = child.kill();
    let _ = child.wait();
    answer.map(Some)
}

/// The requests of one usage query: initialize, then the read (id 2).
const USAGE_REQUESTS: [&str; 3] = [
    r#"{"id":1,"method":"initialize","params":{"clientInfo":{"name":"agentc-supervisor","version":"1"}}}"#,
    r#"{"method":"initialized"}"#,
    r#"{"id":2,"method":"account/rateLimits/read"}"#,
];

/// Sends [`USAGE_REQUESTS`] to the app server and waits for answer 2.
fn ask_usage(child: &mut Child) -> Result<Vec<Window>> {
    let mut stdin = child.stdin.take().context("codex stdin")?;
    let stdout = child.stdout.take().context("codex stdout")?;
    for request in USAGE_REQUESTS {
        writeln!(stdin, "{request}").context("write to codex app-server")?;
    }
    let (sender, lines) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + USAGE_TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let line = lines
            .recv_timeout(left)
            .map_err(|_| anyhow::anyhow!("codex app-server gave no usage answer in time"))?;
        if let Some(answer) = usage::codex_answer(&line, 2) {
            return answer;
        }
    }
}

/// When the role's Claude token expires: its file's modification time plus
/// `[health] token_lifetime_days` (a `claude setup-token` token lasts a
/// year). Codex refreshes its own login, so it has no known expiry.
pub fn expiry_ms(config: &Config, role: Role, harness: Harness) -> Option<i64> {
    if harness != Harness::Claude {
        return None;
    }
    let path = crate::profile::claude_token(role, config);
    let modified = std::fs::symlink_metadata(path).ok()?.modified().ok()?;
    let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    let days = i64::try_from(config.health.token_lifetime_days).ok()?;
    let lifetime = days.saturating_mul(super::health::DAY_MS);
    i64::try_from(since.as_millis()).ok()?.checked_add(lifetime)
}

/// The launch's `$RUN/events.jsonl`, owned by the role, read beneath the
/// root-owned role directory.
pub fn events(config: &Config, account: &Account, launch: &Launch) -> Option<Vec<u8>> {
    run_file(config, account, launch, "events.jsonl")
}

/// The launch's `$RUN/stderr.log`, read the same way.
pub fn stderr(config: &Config, account: &Account, launch: &Launch) -> Option<Vec<u8>> {
    run_file(config, account, launch, "stderr.log")
}

/// The role-owned `$RUN/<name>`, never following a symlink.
fn run_file(config: &Config, account: &Account, launch: &Launch, name: &str) -> Option<Vec<u8>> {
    let base = role_dir(config, Role::Implementer);
    let run = launch.run.strip_prefix(&base).ok()?;
    let relative = Path::new(run).join(name);
    rooted::read(&base, &relative, account.uid, MAX_EVENTS).ok()
}
