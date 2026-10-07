//! Host-approved project setup (autonomy plan §2.3 "Project environment
//! setup", audit item 6). A host may configure, per coordinator project, a
//! `setup` command (for example `cargo fetch`) and cache directories.
//! `launch` runs the command in the clone before the harness, inside the
//! same Bubblewrap sandbox, environment and network namespace the harness
//! gets, with output in `$RUN/setup.log`; a failure or timeout refuses the
//! launch. Cache directories are bound writable into that sandbox (setup and
//! harness alike), so fetched dependencies survive between launches. The
//! configuration lives in the host's file, so the host owner approves it;
//! nothing in the repository can add a command or widen the sandbox.
use crate::config::Config;
use crate::profile::{self, Harness, LaunchCommand, LaunchSpec};
use crate::{confine, reaper, sandbox};
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use std::ffi::OsString;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// The setup command's combined output, in `$RUN`.
pub const SETUP_LOG: &str = "setup.log";

/// `[setup.<project-id>]`: every entry has a default, and an empty command
/// means no setup.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectSetup {
    /// Program and arguments, run in the clone (no shell unless named).
    pub command: Vec<String>,
    /// Existing absolute directories, owned by the launch's account, that
    /// the sandbox binds writable at the same path.
    pub cache_paths: Vec<PathBuf>,
    /// The command is killed and the launch refused after this long.
    pub timeout_seconds: u64,
}

impl Default for ProjectSetup {
    /// No command, no caches, and a 15-minute limit.
    fn default() -> Self {
        Self {
            command: Vec::new(),
            cache_paths: Vec::new(),
            timeout_seconds: 900,
        }
    }
}

/// The setup configured for the launch's project, if any.
pub fn for_launch<'a>(spec: &LaunchSpec, config: &'a Config) -> Option<&'a ProjectSetup> {
    config.setup.get(spec.project.as_deref()?)
}

/// The cache directories the launch's sandbox binds writable, each checked
/// by [`checked_cache`].
pub fn caches(spec: &LaunchSpec, config: &Config) -> Result<Vec<PathBuf>> {
    let paths = for_launch(spec, config).map_or(&[][..], |s| &s.cache_paths[..]);
    paths.iter().map(|path| checked_cache(path)).collect()
}

/// `path` if it is absolute, reached without symlinks, a directory, and
/// owned by the account running the launch (never root's or another role's).
fn checked_cache(path: &Path) -> Result<PathBuf> {
    let shown = path.display();
    ensure!(path.is_absolute(), "cache path {shown} is not absolute");
    confine::path_without_symlinks(path, false).with_context(|| format!("cache path {shown}"))?;
    let metadata = std::fs::metadata(path).with_context(|| format!("cache path {shown}"))?;
    ensure!(metadata.is_dir(), "cache path {shown} is not a directory");
    owned_by_launch_account(path, &metadata)?;
    Ok(path.to_owned())
}

/// Refuses a cache directory the launch's own account does not own.
#[cfg(unix)]
fn owned_by_launch_account(path: &Path, metadata: &std::fs::Metadata) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    let shown = path.display();
    ensure!(
        metadata.uid() == uid,
        "cache path {shown} is not owned by the launch's account"
    );
    Ok(())
}

/// Ownership cannot be checked without Unix metadata, so caches are refused.
#[cfg(not(unix))]
fn owned_by_launch_account(path: &Path, _metadata: &std::fs::Metadata) -> Result<()> {
    bail!("cache path {} needs Unix ownership checks", path.display())
}

/// Whether a `harness` launch can run `project`'s setup: a project with a
/// setup command needs the Bubblewrap sandbox only Claude launches have.
/// Admission uses this to skip such a vendor before claiming.
pub fn can_run(config: &Config, project: &str, harness: Harness) -> bool {
    let has_command = config
        .setup
        .get(project)
        .is_some_and(|s| !s.command.is_empty());
    harness == Harness::Claude || !has_command
}

/// Runs the project's setup command, if one is configured, sandboxed like
/// the harness and before it starts. Only Claude launches have the
/// Bubblewrap boundary, so a Codex launch with setup is refused rather than
/// run unconfined.
pub fn run(spec: &LaunchSpec, config: &Config) -> Result<()> {
    let Some(setup) = for_launch(spec, config).filter(|s| !s.command.is_empty()) else {
        return Ok(());
    };
    ensure!(
        spec.harness == Harness::Claude,
        "project setup needs the Bubblewrap sandbox of a Claude launch; Codex launches refuse it"
    );
    let command = sandboxed(setup, spec, config)?;
    let log = crate::launch::new_log(&spec.run.join(SETUP_LOG)).context("create the setup log")?;
    execute(&command, log, Duration::from_secs(setup.timeout_seconds))
}

/// The setup command with the harness's environment and working directory,
/// wrapped in the launch's sandbox.
pub fn sandboxed(
    setup: &ProjectSetup,
    spec: &LaunchSpec,
    config: &Config,
) -> Result<LaunchCommand> {
    let mut command = profile::command(spec, config);
    let (program, args) = setup.command.split_first().context("empty setup command")?;
    command.program = PathBuf::from(program);
    command.args = args.iter().map(OsString::from).collect();
    sandbox::wrap(command, spec, config)
}

/// Runs `command` with exactly its environment, no input, no new
/// privileges and fenced descriptors, writing its output to `log`; fails on
/// a non-zero exit or once `timeout` has passed.
pub fn execute(command: &LaunchCommand, log: File, timeout: Duration) -> Result<()> {
    let mut process = Command::new(&command.program);
    sandbox::fence_descriptors(&mut process)?;
    reaper::forbid_new_privileges(&mut process);
    process
        .args(&command.args)
        .env_clear()
        .envs(command.env.iter().map(|(k, v)| (k, v)))
        .current_dir(&command.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    let child = process.spawn().context("spawn the project setup")?;
    let status = wait(child, timeout)?;
    ensure!(
        status.success(),
        "project setup failed ({status}); see $RUN/{SETUP_LOG}"
    );
    Ok(())
}

/// Waits for `child`, killing it once `timeout` has passed.
fn wait(mut child: Child, timeout: Duration) -> Result<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("project setup ran longer than {} s", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
