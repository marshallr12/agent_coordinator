use clap::{Parser, Subcommand};
use coordinator_server::{
    auth::init_admin,
    context_rerank::{ApiKey, ContextRerankConfig},
    router,
    state::{AppState, Config},
};
use std::{
    io::{self, Read},
    net::SocketAddr,
    path::PathBuf,
};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(version, about = "Agent Coordinator service")]
struct Options {
    #[arg(
        long,
        env = "COORDINATOR_DATABASE",
        default_value = "data/coordinator.sqlite3",
        global = true
    )]
    database: PathBuf,
    #[arg(
        long,
        env = "COORDINATOR_LISTEN",
        default_value = "127.0.0.1:8080",
        global = true
    )]
    listen: SocketAddr,
    #[arg(
        long,
        env = "COORDINATOR_PUBLIC_ORIGIN",
        default_value = "https://localhost",
        global = true
    )]
    public_origin: String,
    #[arg(long, env = "COORDINATOR_ALLOW_INSECURE_LOOPBACK", global = true)]
    allow_insecure_loopback: bool,
    #[arg(long, env = "COORDINATOR_JSON_BODY_LIMIT_BYTES", default_value_t = 1024 * 1024, global = true)]
    json_body_limit_bytes: usize,
    #[arg(long, env = "COORDINATOR_ARTIFACT_QUOTA_BYTES", default_value_t = 10 * 1024 * 1024 * 1024, global = true)]
    artifact_quota_bytes: i64,
    #[arg(long, env = "COORDINATOR_ARTIFACT_DISK_RESERVE_BYTES", default_value_t = 256 * 1024 * 1024, global = true)]
    artifact_disk_reserve_bytes: u64,
    /// Hours of ready work without task progress before the digest counts a stall.
    #[arg(long, env = "COORDINATOR_STALL_HOURS", default_value_t = coordinator_server::attention::DEFAULT_STALL_HOURS, global = true)]
    stall_hours: i64,
    /// UTC hours excluded from the stall clock, as START-END (for example 22-07).
    #[arg(long, env = "COORDINATOR_QUIET_HOURS", global = true)]
    quiet_hours: Option<coordinator_server::attention::QuietHours>,
    /// Agent-created tasks without an admission class admitted per ISO week in each project.
    #[arg(long, env = "COORDINATOR_AGENT_TASK_WEEKLY_BUDGET", default_value_t = coordinator_server::admission::DEFAULT_WEEKLY_BUDGET, global = true)]
    agent_task_weekly_budget: i64,
    /// Comma-separated names of the agent principals that may create tasks in the `canary` admission class.
    #[arg(
        long,
        env = "COORDINATOR_CANARY_PRINCIPALS",
        value_delimiter = ',',
        global = true
    )]
    canary_principals: Vec<String>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Print exact source and target build identity without opening service state.
    BuildInfo,
    /// Start the service on its private loopback listener.
    Serve,
    /// Publish and verify a consistent database/artifact snapshot, then apply retention.
    Backup {
        #[arg(long)]
        repository: PathBuf,
    },
    /// Verify a completed snapshot without opening the live service database.
    BackupVerify {
        #[arg(long)]
        snapshot: PathBuf,
    },
    /// Restore into a new data directory with old authority invalidated and coordination paused.
    Restore {
        #[arg(long)]
        snapshot: PathBuf,
        /// Must not exist. The running installation is never overwritten.
        #[arg(long)]
        destination: PathBuf,
        /// Audited reason and source context; do not include credentials.
        #[arg(long)]
        reason: String,
    },
    /// Reconcile a detected server clock rollback after trustworthy time is restored.
    RecoverClock {
        #[arg(long)]
        reason: String,
    },
    /// Compact expired replay payloads and redundant health evidence in bounded batches.
    Maintenance {
        #[arg(long, default_value_t = 500)]
        batch_size: usize,
        #[arg(long, default_value_t = 20)]
        max_batches: usize,
        /// Close agent sessions idle for longer than this many days (0 disables).
        #[arg(long, env = "COORDINATOR_SESSION_IDLE_DAYS", default_value_t = coordinator_server::session_idle::DEFAULT_SESSION_IDLE_DAYS)]
        session_idle_days: i64,
    },
    /// Recover a human account locally and invalidate all of its browser sessions.
    RecoverOperatorPassword {
        #[arg(long)]
        username: String,
        /// Audited reason; never include the password.
        #[arg(long)]
        reason: String,
        /// Read a single new password from stdin; otherwise use a hidden prompt.
        #[arg(long)]
        password_stdin: bool,
    },
    /// Create the first local administrator in an empty installation.
    InitAdmin {
        #[arg(long)]
        username: String,
        /// Read a single password from stdin; otherwise use a hidden prompt.
        #[arg(long)]
        password_stdin: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Do not log request bodies, headers, URLs, password hashes, or SQL values.
    // rmcp's diagnostic events can include decoded messages or request
    // headers. Keep that target disabled regardless of ambient RUST_LOG.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new("info,rmcp=off"))
        .init();
    let options = Options::parse();
    if matches!(&options.command, Command::BuildInfo) {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "version": env!("CARGO_PKG_VERSION"),
                "build": coordinator_core::build_identity(),
                "client_compatibility": coordinator_core::service_client_compatibility(),
            }))?
        );
        return Ok(());
    }
    // Verification and restore must not create, migrate, or otherwise touch the
    // configured live database. Restore prepares an isolated destination itself.
    match &options.command {
        Command::BackupVerify { snapshot } => {
            let report = coordinator_server::backup::verify_backup(snapshot).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            return Ok(());
        }
        Command::Restore {
            snapshot,
            destination,
            reason,
        } => {
            let report =
                coordinator_server::backup::restore_backup(snapshot, destination, reason).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            return Ok(());
        }
        Command::Backup { .. } | Command::Maintenance { .. } | Command::RecoverClock { .. } => {
            anyhow::ensure!(
                options.database.is_file(),
                "This operation requires an existing service database."
            );
        }
        _ => {}
    }
    let config = Config {
        database_path: options.database,
        listen: options.listen,
        public_origin: options.public_origin,
        allow_insecure_loopback: options.allow_insecure_loopback,
        json_body_limit_bytes: options.json_body_limit_bytes,
        artifact_quota_bytes: options.artifact_quota_bytes,
        artifact_disk_reserve_bytes: options.artifact_disk_reserve_bytes,
        context_rerank: context_rerank_config(&options.command),
        stall_hours: options.stall_hours,
        quiet_hours: options.quiet_hours,
        agent_task_weekly_budget: options.agent_task_weekly_budget,
        canary_principals: options.canary_principals,
    };
    let state = if matches!(&options.command, Command::Backup { .. }) {
        AppState::open_existing_read_only(config).await?
    } else {
        AppState::open(config).await?
    };
    match options.command {
        Command::BuildInfo => unreachable!("handled before opening service state"),
        Command::RecoverClock { reason } => {
            let report =
                coordinator_server::operator_access::recover_clock(&state, &reason).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::Maintenance {
            batch_size,
            max_batches,
            session_idle_days,
        } => {
            let report = coordinator_server::maintenance::run_maintenance(
                &state,
                coordinator_server::maintenance::MaintenanceOptions {
                    batch_size,
                    max_batches,
                    session_idle_days,
                },
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::Backup { repository } => {
            let report = coordinator_server::backup::create_backup(&state, &repository).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::BackupVerify { .. } | Command::Restore { .. } => {
            unreachable!("handled before opening the database")
        }
        Command::InitAdmin {
            username,
            password_stdin,
        } => {
            let password = read_password(password_stdin).await?;
            init_admin(&state, &username, password).await?;
            println!(
                "Administrator created. Start the service and sign in through its configured HTTPS origin."
            );
        }
        Command::RecoverOperatorPassword {
            username,
            reason,
            password_stdin,
        } => {
            let password = read_password(password_stdin).await?;
            coordinator_server::operator_access::recover_operator_password(
                &state, &username, password, &reason,
            )
            .await?;
            println!(
                "Account recovered. All of its browser sessions have ended. Sign in with the new password."
            );
        }
        Command::Serve => {
            let listener = tokio::net::TcpListener::bind(state.config.listen).await?;
            coordinator_server::artifacts::reconcile_store(&state)
                .await
                .map_err(|error| {
                    anyhow::anyhow!("Artifact store initialization failed: {}", error.code)
                })?;
            tracing::info!(listen = %state.config.listen, "Agent Coordinator service started");
            tokio::spawn(sweep_decision_timeouts(state.clone()));
            axum::serve(listener, router(state).into_make_service())
                .with_graceful_shutdown(shutdown())
                .await?;
        }
    }
    Ok(())
}
/// Answers reversible decisions that waited 24 hours, every few minutes.
async fn sweep_decision_timeouts(state: AppState) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(300));
    loop {
        tick.tick().await;
        match coordinator_server::attention::sweep_timed_out_decisions(&state).await {
            Ok(answered) if !answered.is_empty() => {
                tracing::info!(
                    count = answered.len(),
                    "Reversible decisions proceeded after 24 hours"
                );
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "Decision timeout sweep failed"),
        }
    }
}

/// Reranking settings for `command`. Only `serve` reads TYPESAFE_API_KEY from
/// the environment, and it logs one warning when the key is unset or blank;
/// every other command gets the keyless default, which reranks nothing.
fn context_rerank_config(command: &Command) -> ContextRerankConfig {
    if !matches!(command, Command::Serve) {
        return ContextRerankConfig::default();
    }
    let api_key = std::env::var("TYPESAFE_API_KEY").ok().and_then(ApiKey::new);
    if api_key.is_none() {
        tracing::warn!(
            "TypeSafe context reranking is off because TYPESAFE_API_KEY is not set; context results keep their search order."
        );
    }
    ContextRerankConfig {
        api_key,
        ..ContextRerankConfig::default()
    }
}

async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = terminate => {} }
}

async fn read_password(password_stdin: bool) -> anyhow::Result<String> {
    tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        if password_stdin {
            let mut password = String::new();
            io::stdin()
                .lock()
                .take(1027)
                .read_to_string(&mut password)?;
            if password.ends_with('\n') {
                password.pop();
                if password.ends_with('\r') {
                    password.pop();
                }
            }
            anyhow::ensure!(
                !password.contains(['\n', '\r']) && password.len() <= 1024,
                "Read one password of at most 1024 bytes from stdin."
            );
            Ok(password)
        } else {
            let password = rpassword::prompt_password("New password: ")?;
            let confirmation = rpassword::prompt_password("Confirm password: ")?;
            anyhow::ensure!(password == confirmation, "Passwords did not match.");
            Ok(password)
        }
    })
    .await?
}
