//! Prepares a run directory and spawns one supervised launch.
use crate::config::Config;
use crate::preflight;
use crate::profile::{self, LaunchCommand, LaunchSpec, Role, run_files};
use crate::role_settings;
use crate::verification;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::fs::{self, File};
use std::process::{Command, Stdio};

/// Writes the generated files a launch needs into `$RUN` (the prompt is the
/// caller's) and creates its private temp and build directories.
pub fn prepare_run(spec: &LaunchSpec, config: &Config) -> Result<()> {
    for dir in ["tmp", "target"] {
        fs::create_dir_all(spec.run.join(dir)).context("create run directories")?;
    }
    fs::write(
        spec.run.join(run_files::SETTINGS),
        role_settings::render(spec.role),
    )?;
    if spec.role == Role::Reviewer {
        let schema = serde_json::to_string_pretty(&profile::review_schema())?;
        fs::write(spec.run.join(run_files::SCHEMA), schema)?;
    }
    if let Some(described) = verification::describe(spec, config) {
        let text = serde_json::to_string_pretty(&described)?;
        fs::write(spec.run.join(run_files::VERIFICATION), text)?;
    }
    Ok(())
}

/// Prepares, preflights and runs a launch; returns the harness exit code.
/// Events go to `$RUN/events.jsonl`, diagnostics to `$RUN/stderr.log`.
pub fn run(spec: &LaunchSpec, config: &Config) -> Result<i32> {
    prepare_run(spec, config)?;
    let problems = preflight::check(spec, config);
    if !problems.is_empty() {
        bail!("preflight refused the launch:\n- {}", problems.join("\n- "));
    }
    let command = profile::command(spec, config);
    let status = spawn(&command, spec)?.wait().context("wait for harness")?;
    Ok(status.code().unwrap_or(-1))
}

/// Spawns the harness with exactly the profile's environment.
fn spawn(command: &LaunchCommand, spec: &LaunchSpec) -> Result<std::process::Child> {
    let stdout = File::create(spec.run.join("events.jsonl"))?;
    let stderr = File::create(spec.run.join("stderr.log"))?;
    Command::new(&command.program)
        .args(&command.args)
        .env_clear()
        .envs(command.env.iter().map(|(k, v)| (k, v)))
        .current_dir(&command.cwd)
        .stdin(File::open(&command.stdin).context("open prompt")?)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .with_context(|| format!("spawn {}", command.program.display()))
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
