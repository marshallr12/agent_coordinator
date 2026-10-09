//! `agentc-supervisor` command line; the containment logic lives in the
//! library (see `lib.rs`) so integration tests can drive real launches.
use agentc_supervisor::{
    clone, config, egress, launch, preflight, profile, reaper, relay, role_settings, sandbox,
    shadow,
};
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
    /// Print the generated persistent CLAUDE.md for a role.
    Instructions {
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
        /// Canonical fetch URL for `origin` when `--url` is a local mirror.
        #[arg(long)]
        origin_url: Option<String>,
        /// Repository-local commit author and committer name.
        #[arg(long, requires = "user_email")]
        user_name: Option<String>,
        /// Repository-local commit author and committer email.
        #[arg(long, requires = "user_name")]
        user_email: Option<String>,
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
    /// As root: run one launch as its role account and, for an implementer,
    /// the candidate-push helper beside it as the helper account.
    LaunchRoot(SpecArgs),
    /// Poll `next` read-only and log what would be launched (P3a shadow mode).
    Shadow {
        /// Poll once and exit instead of looping.
        #[arg(long)]
        once: bool,
    },
    /// As root: poll `next`, claim, launch and clean up in a loop (P3b live mode).
    Run {
        /// Poll once and exit instead of looping.
        #[arg(long)]
        once: bool,
    },
    /// Summarise a would-launch log (default: the configured shadow log).
    ShadowReport {
        #[arg(long)]
        log: Option<PathBuf>,
    },
    /// Inside a launch's network namespace: relay each loopback `ip:port`
    /// to its host socket, run the command and return its exit code.
    #[command(hide = true)]
    NetnsRelay {
        #[arg(long = "relay")]
        relays: Vec<String>,
        #[arg(last = true, required = true)]
        command: Vec<std::ffi::OsString>,
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
    /// Coordinator project id; selects the project's verification environment.
    #[arg(long)]
    project: Option<String>,
    /// Coordinator task id; an implementer's push helper names its candidate
    /// ref after it and the session id.
    #[arg(long)]
    task: Option<String>,
    /// Session id (default: a fresh one); `launch-root` passes the id it
    /// gives its push helper as the launch id.
    #[arg(long)]
    session_id: Option<uuid::Uuid>,
    /// The implementer's candidate-push helper socket (set by `launch-root`):
    /// an absolute path of at most 93 bytes, exported to the harness as
    /// AGENT_COORDINATOR_CANDIDATE_PUSH_SOCKET.
    #[arg(long)]
    push_socket: Option<PathBuf>,
}

impl SpecArgs {
    /// Converts arguments into a launch spec; the session id is the given
    /// one or a fresh one.
    fn spec(&self) -> LaunchSpec {
        LaunchSpec {
            role: self.role,
            harness: self.harness,
            clone: self.clone.clone(),
            run: self.run.clone(),
            model: self.model.clone(),
            effort: self.effort.clone(),
            session_id: self.session_id.unwrap_or_else(uuid::Uuid::new_v4),
            project: self.project.clone(),
            task: self.task.clone(),
            push_socket: self.push_socket.clone(),
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
    if let Commands::NetnsRelay { relays, command } = &cli.command {
        // Needs no configuration: everything arrives in its arguments.
        let code = relay::run_namespace(relays, command)?;
        return Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)));
    }
    let config = config::Config::load(cli.config.as_deref())?;
    match cli.command {
        Commands::Settings { role } => print!("{}", role_settings::render(role)),
        Commands::Instructions { role } => print!("{}", role_settings::instructions(role)),
        Commands::Clone {
            url,
            revision,
            dest,
            mirror,
            origin_url,
            user_name,
            user_email,
        } => {
            clone::create(&url, mirror.as_deref(), &revision, &dest)?;
            if let Some(origin) = origin_url {
                clone::set_origin(&dest, &origin)?;
            }
            if let (Some(name), Some(email)) = (user_name, user_email) {
                clone::set_identity(&dest, &name, &email)?;
            }
        }
        Commands::EgressProxy => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(egress::serve(&config.egress_listen, config.egress_hosts()))?
        }
        Commands::Prepare(args) => launch::prepare_run(&args.spec(), &config)?,
        Commands::Preflight(args) => return Ok(report(&preflight::check(&args.spec(), &config))),
        Commands::Launch { spec, dry_run } => {
            return launch_command(&spec.spec(), &config, dry_run);
        }
        Commands::LaunchRoot(args) => {
            return launch_root(&args.spec(), cli.config.as_deref(), &config);
        }
        Commands::Shadow { once } => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(shadow::run(&config.shadow, once))?
        }
        Commands::Run { once } => live_run(&config, cli.config.as_deref(), once)?,
        Commands::NetnsRelay { .. } => unreachable!("dispatched before loading config"),
        Commands::ShadowReport { log } => {
            let log = log.unwrap_or_else(|| config.shadow.log.clone());
            println!("{}", serde_json::to_string_pretty(&shadow::report(&log)?)?);
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
        let command = sandbox::wrap(profile::command(spec, config), spec, config)?;
        println!(
            "{}",
            serde_json::to_string_pretty(&launch::describe(&command))?
        );
        return Ok(ExitCode::SUCCESS);
    }
    let code = run_reaped(spec, config)?;
    let mut outcome = serde_json::json!({"exit_code": code, "session_id": spec.session_id});
    if let Some(task) = &spec.task {
        outcome["task"] = task.as_str().into();
    }
    println!("{outcome}");
    Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)))
}

/// Runs a launch as the subreaper of its harness and kills whatever the
/// harness left running (R-P3b.5(b)) before the run is marked terminal, or
/// whatever a refused launch had already started.
fn run_reaped(spec: &LaunchSpec, config: &config::Config) -> Result<i32> {
    reaper::reaped(|cleanup| launch::run(spec, config, cleanup))
}

/// Runs a launch through `launch-root`; its exit code is the launch's.
#[cfg(target_os = "linux")]
fn launch_root(
    spec: &LaunchSpec,
    config_path: Option<&std::path::Path>,
    config: &config::Config,
) -> Result<ExitCode> {
    let code = agentc_supervisor::push_helper::launch_root(spec, config_path, config)?;
    Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)))
}

/// `launch-root` needs Linux account switching.
#[cfg(not(target_os = "linux"))]
fn launch_root(
    _spec: &LaunchSpec,
    _config_path: Option<&std::path::Path>,
    _config: &config::Config,
) -> Result<ExitCode> {
    anyhow::bail!("launch-root requires Linux")
}

/// Runs the live supervisor loop.
#[cfg(target_os = "linux")]
fn live_run(config: &config::Config, path: Option<&std::path::Path>, once: bool) -> Result<()> {
    agentc_supervisor::run_loop::live::run(config, path, once)
}

/// The live loop needs Linux account switching.
#[cfg(not(target_os = "linux"))]
fn live_run(_config: &config::Config, _path: Option<&std::path::Path>, _once: bool) -> Result<()> {
    anyhow::bail!("run requires Linux")
}
