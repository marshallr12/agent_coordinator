//! `agentc-integrator`: the deterministic integrator (autonomy plan P4).
//!
//! It takes approved submissions from the coordinator's integrator queue,
//! computes the pinned result R of target tip X and candidate C, has the
//! required checks run on R, and fast-forwards the target to R under push
//! authority and a lease on X — attesting every outcome back to the service.
//! No LLM is involved. See planning/autonomy/p4-design.md.
mod action_refs;
mod askpass;
mod checks;
mod config;
#[cfg(test)]
mod e2e_tests;
mod gates;
mod git;
mod github;
mod integrate;
mod local_actions;
mod privilege;
mod publish;
mod roster;
mod service;
mod state;

use anyhow::{Result, bail};
use checks::{ChecksSource, FakeChecks, GithubChecks};
use clap::{Parser, Subcommand};
use config::{ChecksKind, Config};
use integrate::{Integrator, Step};
use service::Service;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "agentc-integrator",
    version,
    about = "Deterministic integrator for Agent Coordinator"
)]
struct Cli {
    /// Configuration file (default: /etc/agentc/integrator.toml if present).
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Integrate the configured projects' queues (forever, or one pass).
    Run {
        /// Run one pass over the projects and exit.
        #[arg(long)]
        once: bool,
    },
    /// Print the effective configuration (defaults merged with the file).
    Config,
}

/// Entry point: Git's askpass helper when invoked by Git, else the CLI.
fn main() -> ExitCode {
    if let Some((config, prompt)) = askpass::invoked_prompt() {
        return askpass_main(config, &prompt);
    }
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("agentc-integrator: {error:#}");
            ExitCode::from(1)
        }
    }
}

/// Answers one Git credential prompt (this process is Git's `GIT_ASKPASS`).
fn askpass_main(config: Option<PathBuf>, prompt: &str) -> ExitCode {
    match askpass::answer(config.as_deref(), prompt) {
        Ok(answer) => {
            println!("{answer}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("agentc-integrator: {error:#}");
            ExitCode::from(1)
        }
    }
}

/// Dispatches one subcommand.
fn run(cli: Cli) -> Result<()> {
    let config = Config::load(cli.config.as_deref())?;
    match cli.command {
        Commands::Config => println!("{config:#?}"),
        Commands::Run { once } => {
            if config.has_app() {
                askpass::install(cli.config.as_deref())?;
            }
            tokio::runtime::Runtime::new()?.block_on(serve(config, once))?;
        }
    }
    Ok(())
}

/// Builds the integrator with the configured checks source and runs it.
async fn serve(config: Config, once: bool) -> Result<()> {
    let service = Service::from_credential_file(
        &config.credential_file,
        &config.origin,
        config.allow_insecure_loopback,
    )?;
    let state = state::LoopState::load(&config.state_dir.join("state.json"))?;
    if config.checks == ChecksKind::Github {
        return serve_github(config, service, state, once).await;
    }
    let checks = FakeChecks {
        path: config.fake_checks_file.clone(),
    };
    run_loop(
        Integrator {
            config,
            service,
            checks,
            state,
        },
        once,
    )
    .await
}

/// Runs with GitHub Actions checks through the configured App.
async fn serve_github(
    config: Config,
    service: Service,
    state: state::LoopState,
    once: bool,
) -> Result<()> {
    if !config.has_app() {
        bail!("checks = \"github\" needs [github] app_id and installation_id");
    }
    let app = github::GithubApp::new(&config.github)?;
    let checks = GithubChecks { app: &app };
    run_loop(
        Integrator {
            config,
            service,
            checks,
            state,
        },
        once,
    )
    .await
}

/// Cycles over the projects, one JSON log line per project per pass.
async fn run_loop<C: ChecksSource>(mut integrator: Integrator<C>, once: bool) -> Result<()> {
    loop {
        for project in integrator.config.projects.clone() {
            let outcome = integrator.cycle(&project).await;
            log(&project, &outcome);
        }
        if once {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(integrator.config.poll_seconds.max(5))).await;
    }
}

/// Prints one cycle's outcome as a JSON line (stdout; the service unit logs it).
fn log(project: &str, outcome: &Result<Step>) {
    let (step, detail) = match outcome {
        Ok(step) => (format!("{step:?}"), String::new()),
        Err(error) => ("Error".to_owned(), format!("{error:#}")),
    };
    let time = chrono::Utc::now().to_rfc3339();
    println!(
        "{}",
        serde_json::json!({"time": time, "project": project, "step": step, "detail": detail})
    );
}
