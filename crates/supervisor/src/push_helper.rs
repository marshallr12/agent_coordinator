//! `launch-root` (decision U25): runs one launch as its role account and,
//! for an implementer, the candidate-push helper beside it.
//!
//! For an implementer launch, run as root, it creates the launch's helper
//! directory (see `layout`), digests the implementer's coordinator
//! credentials so the helper's secret scan refuses them, starts `agentc-push
//! serve` as the helper account, waits for its socket, runs `agentc-supervisor
//! launch --push-socket …` as the implementer, then stops the helper and
//! removes the directory, whatever the launch's outcome. A reviewer launch
//! gets no helper; `launch-root` only runs it as the reviewer account.
//!
//! Both children start from an empty environment plus `PATH`, `LANG` and
//! `HOME`. The helper asks for SIGTERM when the thread that spawned it ends;
//! `run` spawns it on the calling thread, which then waits for the launch, so
//! the helper outlives neither the launch nor a killed `launch-root`.
//!
//! A Claude launch sees only its own socket directory (see
//! `sandbox::bind_push_socket`). A Codex launch runs in the host namespace as
//! the implementer account, the group of every launch's socket directory, so
//! it can connect to a concurrent implementer launch's helper; that helper
//! refuses it because the client is not in its `launch-root`'s process tree.
pub(crate) mod accounts;
pub(crate) mod files;
mod layout;

pub use layout::{MAX_SOCKET_PATH, check_socket_path};

use crate::config::Config;
use crate::profile::{LaunchSpec, Role};
use accounts::Account;
use anyhow::{Context, Result, bail, ensure};
use clap::ValueEnum;
use layout::{LaunchDir, Owners};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// How long the helper may take to publish its socket.
const START_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the helper may take to exit after SIGTERM before SIGKILL.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
/// The search path both children get.
const CHILD_PATH: &str = "/usr/local/bin:/usr/bin:/bin";
/// The helper's log, created in `$RUN`.
pub const LOG_NAME: &str = "push-helper.log";

/// Everything one `launch-root` run needs to know.
struct Plan<'a> {
    spec: &'a LaunchSpec,
    config: &'a Config,
    /// Passed on to the launch as `--config`, when given.
    config_path: Option<&'a Path>,
    /// The helper's account; a reviewer, which gets no helper, has its own.
    helper: Account,
    /// The role account the launch runs as.
    launcher: Account,
    /// Whether children switch to `helper` and `launcher`; only root can.
    switch: bool,
    start_timeout: Duration,
    stop_timeout: Duration,
}

/// Runs `spec` as its role account, with a push helper for an implementer,
/// and returns the launch's exit code. Requires root and a clean host check.
pub fn launch_root(spec: &LaunchSpec, config_path: Option<&Path>, config: &Config) -> Result<i32> {
    accounts::require_root(accounts::effective_uid())?;
    let problems = host_problems(spec, config);
    if !problems.is_empty() {
        bail!(
            "launch-root refused the launch:\n- {}",
            problems.join("\n- ")
        );
    }
    let (helper, launcher) = role_accounts(spec, config)?;
    let plan = Plan {
        spec,
        config,
        config_path,
        helper,
        launcher,
        switch: true,
        start_timeout: START_TIMEOUT,
        stop_timeout: STOP_TIMEOUT,
    };
    // A Codex harness shares the launcher's uid and PID namespace, so it can
    // kill `launch`; as subreaper, launch-root still kills what it left.
    crate::reaper::reaped(|cleanup| {
        let result = run(&plan);
        cleanup();
        result
    })
}

/// The helper's and the launch's accounts. A reviewer gets no helper, so
/// its own account stands in for the helper's.
fn role_accounts(spec: &LaunchSpec, config: &Config) -> Result<(Account, Account)> {
    let launcher = Account::lookup(spec.role.user(config))?;
    let helper = match spec.role {
        Role::Implementer => Account::lookup(&config.push_helper.user)?,
        Role::Reviewer => launcher.clone(),
    };
    Ok((helper, launcher))
}

/// What on this host blocks `launch-root` for `spec`: a missing or
/// replaceable supervisor or helper binary, a missing helper configuration,
/// or a missing account.
pub fn host_problems(spec: &LaunchSpec, config: &Config) -> Vec<String> {
    let mut problems = Vec::new();
    let mut record = |result: Result<()>| {
        if let Err(error) = result {
            problems.push(format!("{error:#}"));
        }
    };
    record(crate::confine::protected_executable(
        &crate::relay::program(config),
    ));
    record(Account::lookup(spec.role.user(config)).map(drop));
    if spec.role == Role::Implementer {
        let helper = &config.push_helper;
        record(crate::confine::protected_executable(&helper.program).context("push helper"));
        record(protected_config(&helper.config));
        record(
            Account::lookup(&helper.user)
                .map(drop)
                .context("push helper account"),
        );
    }
    problems
}

/// The helper's configuration must be a root-owned regular file nobody else
/// can write.
fn protected_config(path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let metadata = crate::confine::regular_file(path).context("push helper configuration")?;
    ensure!(
        metadata.uid() == 0 && metadata.mode() & 0o022 == 0,
        "{} must be root-owned and not group/world-writable",
        path.display()
    );
    Ok(())
}

/// Runs the launch, beside a helper for an implementer.
fn run(plan: &Plan<'_>) -> Result<i32> {
    validate(plan.spec)?;
    if plan.spec.role != Role::Implementer {
        return run_launch(plan, None);
    }
    let owners = Owners {
        supervisor: accounts::effective_uid(),
        helper: (plan.helper.uid, plan.helper.gid),
        implementer_group: plan.launcher.gid,
    };
    let root = crate::push_helper_root(plan.config);
    let directory = LaunchDir::create(&root, &plan.spec.session_id.to_string(), &owners)?;
    let outcome = serve_launch(plan, &directory);
    warn(directory.remove());
    outcome
}

/// Reports a teardown failure without hiding the launch's own outcome.
fn warn(result: Result<()>) {
    if let Err(error) = result {
        eprintln!("agentc-supervisor: warning: launch-root teardown: {error:#}");
    }
}

/// `launch-root` assigns the push socket itself, and an implementer's helper
/// needs the task its candidate ref is named after.
fn validate(spec: &LaunchSpec) -> Result<()> {
    ensure!(
        spec.push_socket.is_none(),
        "launch-root assigns the push socket itself; do not pass --push-socket"
    );
    if spec.role == Role::Implementer {
        let task = spec.task.as_deref().unwrap_or_default();
        let safe = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-');
        ensure!(
            !task.is_empty() && task.bytes().all(safe) && !task.starts_with(['.', '-']),
            "an implementer launch-root needs --task: letters, digits, '.', '_' or '-', not leading '.' or '-'"
        );
    }
    Ok(())
}

/// Starts the helper, waits for its socket, runs the launch, then stops the
/// helper whatever happened.
fn serve_launch(plan: &Plan<'_>, directory: &LaunchDir) -> Result<i32> {
    let mut helper = start_helper(plan, directory)?;
    let socket = directory.socket();
    let outcome = await_socket(&mut helper, &socket, plan.start_timeout)
        .and_then(|()| run_launch(plan, Some(&socket)));
    warn(stop(helper, plan.stop_timeout));
    outcome
}

/// The role directory and the run directory relative to it: the paths
/// `files` walks one component at a time without following symlinks.
fn run_below_role(plan: &Plan<'_>) -> Result<(PathBuf, PathBuf)> {
    let base = plan.config.state_dir.join(plan.spec.role.slug());
    crate::confine::managed_paths(plan.spec, plan.config, false)?;
    let relative = plan.spec.run.strip_prefix(&base)?.to_path_buf();
    Ok((base, relative))
}

/// Spawns the helper with its log open and its known credential digests.
fn start_helper(plan: &Plan<'_>, directory: &LaunchDir) -> Result<Child> {
    let (base, run) = run_below_role(plan)?;
    let credentials = [
        run.join("state/coordinator/credentials.toml"),
        PathBuf::from("coordinator/credentials.toml"),
    ];
    let files: Vec<&Path> = credentials.iter().map(PathBuf::as_path).collect();
    let digests = files::credential_digests(&base, &files, plan.launcher.uid)?;
    let log = files::helper_log(&base, &run.join(LOG_NAME))?;
    helper_command(plan, directory, &digests, log)?
        .spawn()
        .with_context(|| format!("start {}", plan.config.push_helper.program.display()))
}

/// `--name=value`, a form no value can turn into another option.
fn flag(name: &str, value: impl AsRef<OsStr>) -> OsString {
    let mut flag = OsString::from(format!("--{name}="));
    flag.push(value);
    flag
}

/// The helper's arguments: one task and launch, its directories, the
/// digests its scan refuses, and this process as its expected parent.
fn helper_args(plan: &Plan<'_>, directory: &LaunchDir, digests: &[String]) -> Vec<OsString> {
    let spec = plan.spec;
    let mut args = vec![
        OsString::from("serve"),
        flag("config", &plan.config.push_helper.config),
        flag("socket", directory.socket()),
        flag("task", spec.task.as_deref().unwrap_or_default()),
        flag("launch", spec.session_id.to_string()),
        flag("work-dir", directory.work_dir()),
    ];
    args.extend(digests.iter().map(|digest| flag("known-digest", digest)));
    args.push(flag("parent-pid", std::process::id().to_string()));
    args
}

/// The helper as a child: cleared environment, `/` as its directory, output
/// to `log`, fenced descriptors, and the helper account when switching.
fn helper_command(
    plan: &Plan<'_>,
    directory: &LaunchDir,
    digests: &[String],
    log: File,
) -> Result<Command> {
    let mut command = Command::new(&plan.config.push_helper.program);
    command
        .args(helper_args(plan, directory, digests))
        .stdout(log.try_clone()?)
        .stderr(log);
    child_basics(&mut command, &directory.work_home())?;
    if plan.switch {
        accounts::run_as(&mut command, &plan.helper);
    }
    Ok(command)
}

/// Settings both children share: only `PATH`, `LANG` and `home` as `HOME`,
/// `/` as the working directory, no stdin, and no inherited descriptor above
/// stderr.
fn child_basics(command: &mut Command, home: &Path) -> Result<()> {
    command
        .env_clear()
        .env("PATH", CHILD_PATH)
        .env("LANG", "C.UTF-8")
        .env("HOME", home)
        .current_dir("/")
        .stdin(Stdio::null());
    crate::sandbox::fence_descriptors(command)
}

/// Waits until the helper publishes `socket`; fails if it exits first or
/// `timeout` passes.
fn await_socket(helper: &mut Child, socket: &Path, timeout: Duration) -> Result<()> {
    use std::os::unix::fs::FileTypeExt;
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = helper.try_wait()? {
            bail!(
                "the push helper exited before serving ({status}); see {LOG_NAME} in the run directory"
            );
        }
        if std::fs::symlink_metadata(socket).is_ok_and(|meta| meta.file_type().is_socket()) {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "the push helper did not publish its socket within {timeout:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Sends the helper SIGTERM, waits up to `timeout`, then kills it; always
/// reaps it.
fn stop(mut helper: Child, timeout: Duration) -> Result<()> {
    if helper.try_wait()?.is_none() {
        let pid = libc::pid_t::try_from(helper.id())?;
        // SAFETY: kill takes plain integers; `pid` is our unreaped child, so
        // the id cannot have been reused.
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if helper.try_wait()?.is_some() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    helper.kill().context("kill the push helper")?;
    helper.wait().context("reap the push helper")?;
    Ok(())
}

/// The CLI name of a value-enum argument.
fn value_name(value: impl ValueEnum) -> String {
    value
        .to_possible_value()
        .map(|value| value.get_name().to_owned())
        .unwrap_or_default()
}

/// `agentc-supervisor launch` arguments for the same launch, with its fixed
/// session id and, for an implementer, the helper's socket.
fn launch_args(plan: &Plan<'_>, socket: Option<&Path>) -> Vec<OsString> {
    let spec = plan.spec;
    let config = plan.config_path.map(|path| flag("config", path));
    let mut args: Vec<OsString> = config.into_iter().collect();
    args.extend([
        OsString::from("launch"),
        flag("role", value_name(spec.role)),
        flag("harness", value_name(spec.harness)),
        flag("clone", &spec.clone),
        flag("run", &spec.run),
        flag("model", &spec.model),
        flag("effort", &spec.effort),
        flag("session-id", spec.session_id.to_string()),
    ]);
    args.extend(
        spec.project
            .as_ref()
            .map(|project| flag("project", project)),
    );
    args.extend(spec.task.as_ref().map(|task| flag("task", task)));
    args.extend(socket.map(|socket| flag("push-socket", socket)));
    args
}

/// Runs the installed supervisor's `launch` as the role account, with the
/// role's home as `HOME`, and returns its exit code.
fn run_launch(plan: &Plan<'_>, socket: Option<&Path>) -> Result<i32> {
    let program = crate::relay::program(plan.config);
    let mut command = Command::new(&program);
    command.args(launch_args(plan, socket));
    let home = plan
        .config
        .state_dir
        .join(plan.spec.role.slug())
        .join("home");
    child_basics(&mut command, &home)?;
    if plan.switch {
        accounts::run_as(&mut command, &plan.launcher);
    }
    let status = command
        .status()
        .with_context(|| format!("run {}", program.display()))?;
    Ok(exit_code(status))
}

/// A shell-style exit code: the code, or the killing signal's number plus 128.
fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}

#[cfg(test)]
mod tests;
