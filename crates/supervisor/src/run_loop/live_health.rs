//! The live driver's vendor checks: each harness's own sign-in status, run
//! as the implementer with the same credential a launch gets, the Claude
//! token's expiry, and a finished launch's event stream read as root
//! without following the role's symlinks.
use super::{Launch, rooted};
use crate::config::Config;
use crate::profile::{Harness, Role};
use crate::push_helper::accounts::{self, Account};
use anyhow::{Context, Result, bail, ensure};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// The longest a sign-in check may take.
const STATUS_TIMEOUT: Duration = Duration::from_secs(30);
/// The largest event stream read for cost and rate limits.
const MAX_EVENTS: u64 = 256 * 1024 * 1024;

/// `<state_dir>/impl`.
fn role_dir(config: &Config) -> PathBuf {
    config.state_dir.join(Role::Implementer.slug())
}

/// Runs `claude auth status` (with the role's token) or `codex login
/// status` as the implementer; a non-zero exit means not signed in. Only
/// the exit code is used, so nothing the check prints reaches the logs.
pub fn status(config: &Config, account: &Account, harness: Harness) -> Result<()> {
    let (program, args): (&str, &[&str]) = match harness {
        Harness::Claude => ("claude", &["auth", "status", "--json"]),
        Harness::Codex => ("codex", &["login", "status"]),
    };
    let mut command = Command::new(config.bin_dir.join(program));
    command
        .args(args)
        .env_clear()
        .envs(environment(config, harness));
    if harness == Harness::Claude {
        let token = crate::launch::role_token(Role::Implementer, config)?;
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
fn environment(config: &Config, harness: Harness) -> Vec<(&'static str, String)> {
    let dir = role_dir(config);
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

/// When the role's Claude token expires: its file's modification time plus
/// `[health] token_lifetime_days` (a `claude setup-token` token lasts a
/// year). Codex refreshes its own login, so it has no known expiry.
pub fn expiry_ms(config: &Config, harness: Harness) -> Option<i64> {
    if harness != Harness::Claude {
        return None;
    }
    let path = crate::profile::claude_token(Role::Implementer, config);
    let modified = std::fs::symlink_metadata(path).ok()?.modified().ok()?;
    let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    let days = i64::try_from(config.health.token_lifetime_days).ok()?;
    let lifetime = days.saturating_mul(super::health::DAY_MS);
    i64::try_from(since.as_millis()).ok()?.checked_add(lifetime)
}

/// The launch's `$RUN/events.jsonl`, owned by the role, read beneath the
/// root-owned role directory.
pub fn events(config: &Config, account: &Account, launch: &Launch) -> Option<Vec<u8>> {
    let base = role_dir(config);
    let run = launch.run.strip_prefix(&base).ok()?;
    let relative = Path::new(run).join("events.jsonl");
    rooted::read(&base, &relative, account.uid, MAX_EVENTS).ok()
}
