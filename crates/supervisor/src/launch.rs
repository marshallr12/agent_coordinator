//! Prepares a run directory and spawns one supervised launch.
use crate::candidate;
use crate::config::Config;
use crate::confine::{self, RunState};
use crate::preflight;
use crate::profile::{self, LaunchCommand, LaunchSpec, Role, run_files};
use crate::role_settings;
use crate::sandbox;
use crate::staging_login;
use crate::verification;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::fs::{File, OpenOptions};
use std::process::{Command, Stdio};

/// Writes the generated files a launch needs into `$RUN` (the prompt is the
/// caller's) and creates its private temp and build directories.
pub fn prepare_run(spec: &LaunchSpec, config: &Config) -> Result<()> {
    prepare(spec, config).map(|_| ())
}

/// Retain the lifecycle lock while writing generated files or running a harness.
fn prepare(spec: &LaunchSpec, config: &Config) -> Result<RunState> {
    let state = RunState::prepare(spec, config)?;
    for dir in ["tmp", "target"] {
        confine::private_dir(&spec.run.join(dir)).context("create run directories")?;
    }
    confine::generated_file(
        &spec.run.join(run_files::SETTINGS),
        role_settings::render(spec.role).as_bytes(),
    )?;
    if spec.role == Role::Reviewer {
        let schema = serde_json::to_string_pretty(&profile::review_schema())?;
        confine::generated_file(&spec.run.join(run_files::SCHEMA), schema.as_bytes())?;
    }
    if candidate::applies(spec) {
        candidate::write(spec, config).context("write candidate shell")?;
    }
    if let Some(described) = verification::describe(spec, config) {
        let text = serde_json::to_string_pretty(&described)?;
        confine::generated_file(&spec.run.join(run_files::VERIFICATION), text.as_bytes())?;
    }
    Ok(state)
}

/// Prepares, preflights and runs a launch; returns the harness exit code.
/// Events go to `$RUN/events.jsonl`, diagnostics to `$RUN/stderr.log`.
/// `after_harness` runs once the harness has exited (or failed to start) and
/// before the run is marked terminal, so leftovers die while it is locked.
pub fn run(spec: &LaunchSpec, config: &Config, after_harness: impl FnOnce()) -> Result<i32> {
    let state = prepare(spec, config)?;
    let problems = preflight::check(spec, config);
    if !problems.is_empty() {
        bail!("preflight refused the launch:\n- {}", problems.join("\n- "));
    }
    let command = sandbox::wrap(profile::command(spec, config), spec, config)?;
    if spec.harness == profile::Harness::Claude {
        crate::relay::start_host(spec, config).context("start loopback relays")?;
    }
    run_harness(&state, &command, spec, config, after_harness)
}

/// Marks the run started, runs the harness, runs `after_harness` once it has
/// exited (or failed to start) and only then marks the run terminal.
fn run_harness(
    state: &RunState,
    command: &LaunchCommand,
    spec: &LaunchSpec,
    config: &Config,
    after_harness: impl FnOnce(),
) -> Result<i32> {
    state.started(spec)?;
    let result = run_signed_in(command, spec, config);
    after_harness();
    state.terminal(spec)?;
    Ok(result?.code().unwrap_or(-1))
}

/// Runs the harness inside a verifying reviewer's staging session (none for
/// other launches), signing it out whether or not the harness ran.
fn run_signed_in(
    command: &LaunchCommand,
    spec: &LaunchSpec,
    config: &Config,
) -> Result<std::process::ExitStatus> {
    let session = staging_login::open(spec, config)?;
    let status =
        spawn(command, spec).and_then(|mut child| child.wait().context("wait for harness"));
    close_session(session);
    status
}

/// Signs a launch's staging session out; a failure only warns, since the
/// coordinator's session lifetime still bounds the handed-over cookie.
fn close_session(session: Option<staging_login::StagingSession>) {
    if let Some(Err(error)) = session.map(staging_login::StagingSession::close) {
        eprintln!("agentc-supervisor: warning: {error:#}");
    }
}

/// Spawns the harness with exactly the profile's environment and
/// `no_new_privs`.
pub fn spawn(command: &LaunchCommand, spec: &LaunchSpec) -> Result<std::process::Child> {
    confine::regular_file(&command.stdin).context("inspect prompt")?;
    let prompt = File::open(&command.stdin).context("open prompt")?;
    let prompt = if spec.harness == profile::Harness::Claude {
        sandbox::sealed_prompt(prompt)?
    } else {
        prompt
    };
    let stdout = new_log(&spec.run.join("events.jsonl"))?;
    let stderr = new_log(&spec.run.join("stderr.log"))?;
    let mut process = Command::new(&command.program);
    if spec.harness == profile::Harness::Claude {
        sandbox::fence_descriptors(&mut process)?;
    }
    crate::reaper::forbid_new_privileges(&mut process);
    process
        .args(&command.args)
        .env_clear()
        .envs(command.env.iter().map(|(k, v)| (k, v)))
        .current_dir(&command.cwd)
        .stdin(prompt)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .with_context(|| format!("spawn {}", command.program.display()))
}

/// A pre-existing output, including a planted link, is never truncated.
fn new_log(path: &std::path::Path) -> Result<File> {
    confine::path_without_symlinks(path, true)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

/// The launch as JSON, for `--dry-run` and audit logs.
pub fn describe(command: &LaunchCommand) -> Value {
    let lossy = |s: &std::ffi::OsStr| s.to_string_lossy().into_owned();
    json!({
        "program": command.program,
        "args": command.args.iter().map(|a| lossy(a)).collect::<Vec<_>>(),
        "env": command.env.iter().map(|(k, v)| format!("{k}={}", lossy(v))).collect::<Vec<_>>(),
        "cwd": command.cwd,
        "stdin": command.stdin,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn launch_logs_are_private_and_never_truncate_existing_files_or_links() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("events.jsonl");
        drop(new_log(&output).unwrap());
        assert_eq!(
            fs::metadata(&output).unwrap().permissions().mode() & 0o7777,
            0o600
        );
        assert!(new_log(&output).is_err());
        let sentinel = root.path().join("sentinel");
        fs::write(&sentinel, "untouched").unwrap();
        let symlinked = root.path().join("stderr.log");
        symlink(&sentinel, &symlinked).unwrap();
        assert!(new_log(&symlinked).is_err());
        let hardlinked = root.path().join("hardlink");
        fs::hard_link(&sentinel, &hardlinked).unwrap();
        assert!(new_log(&hardlinked).is_err());
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "untouched");
    }

    /// A Codex reviewer run in a fresh state directory, with its prompt.
    fn codex_run(root: &std::path::Path) -> (Config, LaunchSpec, RunState) {
        let config = Config {
            state_dir: root.join("roles"),
            cargo_config_seed: root.join("cargo-seed.toml"),
            ..Config::default()
        };
        fs::write(&config.cargo_config_seed, confine::CARGO_CONFIG_SEED).unwrap();
        let base = config.state_dir.join(Role::Reviewer.slug());
        let spec = LaunchSpec {
            role: Role::Reviewer,
            harness: profile::Harness::Codex,
            clone: base.join("clones/current"),
            run: base.join("runs/current"),
            model: "mock".into(),
            effort: "low".into(),
            session_id: uuid::Uuid::new_v4(),
            project: None,
            task: None,
            push_socket: None,
        };
        fs::create_dir_all(&spec.clone).unwrap();
        let state = prepare(&spec, &config).unwrap();
        fs::write(spec.run.join(run_files::PROMPT), "prompt").unwrap();
        (config, spec, state)
    }

    #[test]
    fn harness_runs_without_new_privileges_and_cleanup_precedes_terminal() {
        let root = tempfile::tempdir().unwrap();
        let (config, spec, state) = codex_run(root.path());
        let command = LaunchCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "grep NoNewPrivs: /proc/self/status".into()],
            env: Vec::new(),
            cwd: spec.clone.clone(),
            stdin: spec.run.join(run_files::PROMPT),
        };
        let terminal = spec.run.join(".state-terminal.json");
        let mut terminal_at_cleanup = None;
        let cleanup = || terminal_at_cleanup = Some(terminal.exists());
        let code = run_harness(&state, &command, &spec, &config, cleanup).unwrap();
        assert_eq!(code, 0);
        assert_eq!(terminal_at_cleanup, Some(false), "cleanup missing or late");
        assert!(terminal.exists());
        let events = fs::read_to_string(spec.run.join("events.jsonl")).unwrap();
        assert_eq!(events.split_whitespace().nth(1), Some("1"), "{events}");
    }
}
