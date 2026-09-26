//! `agentc-supervisor`: host-side containment for supervised agent launches.
//!
//! P2 scope (autonomy plan §2.3): exact launch profiles, generated role
//! settings, hardened per-launch clones, a launch preflight and a single
//! supervised launch. Scheduling, leases and reviews arrive in P3.
mod clone;
mod config;
mod egress;
mod launch;
mod preflight;
mod profile;
mod role_settings;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use profile::{Harness, LaunchSpec, Role};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "agentc-supervisor",
    version,
    about = "Containment for supervised agent launches"
)]
struct Cli {
    /// Configuration file (default: /etc/agentc/supervisor.toml if present).
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Print the generated Claude settings for a role.
    Settings {
        #[arg(long, value_enum)]
        role: Role,
    },
    /// Create a hardened per-launch clone at an exact revision.
    Clone {
        #[arg(long)]
        url: String,
        #[arg(long)]
        revision: String,
        #[arg(long)]
        dest: PathBuf,
        /// Read-only local mirror to borrow objects from.
        #[arg(long)]
        mirror: Option<PathBuf>,
    },
    /// Run the egress allowlist proxy on the configured loopback address.
    EgressProxy,
    /// Write a run directory's generated files (settings, schema, dirs) only.
    Prepare(SpecArgs),
    /// Report every containment problem that would block a launch.
    Preflight(SpecArgs),
    /// Preflight and run one launch (or print it with --dry-run).
    Launch {
        #[command(flatten)]
        spec: SpecArgs,
        #[arg(long)]
        dry_run: bool,
    },
}

/// Identifies one launch.
#[derive(Args)]
struct SpecArgs {
    #[arg(long, value_enum)]
    role: Role,
    #[arg(long, value_enum)]
    harness: Harness,
    #[arg(long)]
    clone: PathBuf,
    #[arg(long)]
    run: PathBuf,
    #[arg(long, default_value = "default")]
    model: String,
    #[arg(long, default_value = "high")]
    effort: String,
}

impl SpecArgs {
    /// Converts arguments into a launch spec with a fresh session id.
    fn spec(&self) -> LaunchSpec {
        LaunchSpec {
            role: self.role,
            harness: self.harness,
            clone: self.clone.clone(),
            run: self.run.clone(),
            model: self.model.clone(),
            effort: self.effort.clone(),
            session_id: uuid::Uuid::new_v4(),
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("agentc-supervisor: {error:#}");
            ExitCode::from(1)
        }
    }
}

/// Dispatches one subcommand.
fn run(cli: Cli) -> Result<ExitCode> {
    let config = config::Config::load(cli.config.as_deref())?;
    match cli.command {
        Commands::Settings { role } => print!("{}", role_settings::render(role)),
        Commands::Clone {
            url,
            revision,
            dest,
            mirror,
        } => clone::create(&url, mirror.as_deref(), &revision, &dest)?,
        Commands::EgressProxy => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(egress::serve(&config.egress_listen, config.egress_hosts()))?
        }
        Commands::Prepare(args) => launch::prepare_run(&args.spec())?,
        Commands::Preflight(args) => return Ok(report(&preflight::check(&args.spec(), &config))),
        Commands::Launch { spec, dry_run } => {
            return launch_command(&spec.spec(), &config, dry_run);
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Prints preflight problems as JSON; non-zero exit when any exist.
fn report(problems: &[String]) -> ExitCode {
    let ok = problems.is_empty();
    println!("{}", serde_json::json!({"ok": ok, "problems": problems}));
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// Runs or describes a launch.
fn launch_command(spec: &LaunchSpec, config: &config::Config, dry_run: bool) -> Result<ExitCode> {
    if dry_run {
        let command = profile::command(spec, config);
        println!(
            "{}",
            serde_json::to_string_pretty(&launch::describe(&command))?
        );
        return Ok(ExitCode::SUCCESS);
    }
    let code = launch::run(spec, config)?;
    println!(
        "{}",
        serde_json::json!({"exit_code": code, "session_id": spec.session_id})
    );
    Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)))
}
