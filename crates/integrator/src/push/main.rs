//! `agentc-push`: the candidate-push helper. The supervisor spawns one per
//! implementer launch, as the unprivileged user holding the push App key,
//! listening on a Unix socket bound into the launch's sandbox. The helper
//! publishes the implementer's candidate bundles to the one ref fixed by its
//! task and launch ids (see `coordinator_local::candidate_push`), pushing
//! with an installation token minted for each worthwhile connection and
//! revoked after it.
//!
//! The helper asks for SIGTERM when the thread that spawned it exits
//! (`PR_SET_PDEATHSIG` follows the spawning thread, not its process), so the
//! supervisor must spawn it from a thread that lives as long as the launch;
//! a short-lived worker thread would end the helper early. Passing
//! `--parent-pid` lets the helper confirm at startup that this parent is
//! still the one it has.
mod askpass;
mod budget;
mod config;
mod preamble;
mod process;
mod serve;
mod socket;

use anyhow::{Context, Result, ensure};
use budget::{MINT_BURST, MINT_INTERVAL, MintBudget};
use clap::{Args, Parser, Subcommand};
use config::PushConfig;
use coordinator_local::candidate_push::HelperSpec;
use serve::{GithubCredentials, Server};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Mutex;
use std::time::Instant;
use tokio::signal::unix::{SignalKind, signal};

#[derive(Parser)]
#[command(
    name = "agentc-push",
    version,
    about = "Candidate-push helper for Agent Coordinator implementers"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Serve candidate pushes for one launch on a Unix socket until SIGTERM,
    /// SIGINT or the parent's exit.
    Serve(ServeArgs),
}

/// What one helper is fixed to at spawn.
#[derive(Args)]
struct ServeArgs {
    /// Configuration file.
    #[arg(long, default_value = config::DEFAULT_CONFIG_PATH)]
    config: PathBuf,
    /// Socket path to create; it must not exist yet.
    #[arg(long)]
    socket: PathBuf,
    /// Task id, the first component under the candidate ref prefix.
    #[arg(long)]
    task: String,
    /// Launch id, the second component under the candidate ref prefix.
    #[arg(long)]
    launch: String,
    /// The helper's private bare repository (absolute; created if missing).
    #[arg(long)]
    work_dir: PathBuf,
    /// Lowercase SHA-256 of a credential the secret scan must refuse
    /// (repeatable).
    #[arg(long = "known-digest", value_parser = parse_digest)]
    known_digests: Vec<String>,
    /// The spawning supervisor's process id; startup fails unless it is
    /// still this process's parent. Without it, startup fails only when
    /// init is the parent.
    #[arg(long)]
    parent_pid: Option<u32>,
}

/// Entry point: Git's askpass helper when invoked by Git, else the CLI.
fn main() -> ExitCode {
    let marker = std::env::var_os(askpass::TOKEN_VARIABLE);
    if let Some((token, prompt)) = askpass::invoked_prompt(marker, std::env::args().skip(1)) {
        return askpass_main(&prompt, &token);
    }
    let Commands::Serve(args) = Cli::parse().command;
    match serve(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("agentc-push: {error:#}");
            ExitCode::from(1)
        }
    }
}

/// Answers one Git credential prompt (this process is Git's `GIT_ASKPASS`).
fn askpass_main(prompt: &str, token: &str) -> ExitCode {
    match askpass::answer(prompt, token) {
        Ok(answer) => {
            println!("{answer}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("agentc-push: {error:#}");
            ExitCode::from(1)
        }
    }
}

/// Checks the process, clears its environment, loads the configuration and
/// runs the helper until shutdown.
fn serve(args: ServeArgs) -> Result<()> {
    let (uid, euid) = process::current_ids();
    process::refuse_root(uid, euid)?;
    process::clear_environment();
    let config = PushConfig::load(&args.config)?;
    let spec = helper_spec(&config, &args)?;
    let app = agentc_integrator::github::GithubApp::new(&config.github())?;
    let credentials = GithubCredentials::new(app, &config.repository_id()?.name);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run_helper(
        &args,
        spec,
        config.max_bundle_bytes,
        credentials,
    ))
}

/// Installs the shutdown signals, ties the helper's life to its parent's,
/// binds the socket and serves it until shutdown.
async fn run_helper(
    args: &ServeArgs,
    spec: HelperSpec,
    max_bundle_bytes: u64,
    credentials: GithubCredentials,
) -> Result<()> {
    let shutdown = shutdown_signal()?;
    let launch = process::exit_with_parent(args.parent_pid)?;
    let askpass = std::env::current_exe().context("locate this binary for GIT_ASKPASS")?;
    let socket = socket::bind(&args.socket)?;
    eprintln!("agentc-push: serving {}", spec.reference());
    let server = Server {
        socket,
        spec,
        max_bundle_bytes,
        credentials,
        budget: Mutex::new(MintBudget::new(MINT_BURST, MINT_INTERVAL, Instant::now())),
        askpass,
        timeout: serve::IO_TIMEOUT,
        launch,
    };
    server.run(shutdown).await
}

/// The push spec fixed by the configuration and the command line.
fn helper_spec(config: &PushConfig, args: &ServeArgs) -> Result<HelperSpec> {
    let spec = HelperSpec::new(
        &config.remote_url()?,
        &args.task,
        &args.launch,
        &args.work_dir,
    )?;
    Ok(spec
        .with_known_digests(args.known_digests.clone())
        .with_max_bundle_bytes(config.max_bundle_bytes))
}

/// Completes on the first SIGTERM or SIGINT. The handlers are installed
/// here, before the parent-death signal is requested, so that signal also
/// ends the helper through this path.
fn shutdown_signal() -> Result<impl std::future::Future<Output = ()>> {
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    Ok(async move {
        tokio::select! {
            _ = terminate.recv() => {}
            _ = interrupt.recv() => {}
        }
    })
}

/// Accepts a SHA-256 digest as 64 hex digits and lowercases it.
fn parse_digest(value: &str) -> Result<String> {
    ensure!(
        value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "a known digest is 64 hexadecimal digits"
    );
    Ok(value.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn serve_arguments_parse_with_the_default_config_path() {
        let digest = "AB".repeat(32);
        let cli = Cli::try_parse_from([
            "agentc-push",
            "serve",
            "--socket=/run/s",
            "--task=t",
            "--launch=l",
            "--work-dir=/w",
            &format!("--known-digest={digest}"),
            "--parent-pid=4242",
        ])
        .unwrap();
        let Commands::Serve(args) = cli.command;
        assert_eq!(args.config, PathBuf::from("/etc/agentc/push.toml"));
        assert_eq!(args.known_digests, ["ab".repeat(32)]);
        assert_eq!(args.parent_pid, Some(4242));
        let bad = ["agentc-push", "serve", "--socket=/s", "--task=t"];
        assert!(Cli::try_parse_from(bad).is_err());
        assert!(parse_digest("abc").is_err());
    }
}
