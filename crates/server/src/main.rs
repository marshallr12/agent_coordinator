use clap::{Parser, Subcommand};
use coordinator_server::{
    auth::init_admin,
    router,
    state::{AppState, Config},
};
use std::{
    io::{self, Read},
    net::SocketAddr,
    path::PathBuf,
};

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
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
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
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
    let options = Options::parse();
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
    };
    let state = if matches!(&options.command, Command::Backup { .. }) {
        AppState::open_existing_read_only(config).await?
    } else {
        AppState::open(config).await?
    };
    match options.command {
        Command::RecoverClock { reason } => {
            let report =
                coordinator_server::operator_access::recover_clock(&state, &reason).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::Maintenance {
            batch_size,
            max_batches,
        } => {
            let report = coordinator_server::maintenance::run_maintenance(
                &state,
                coordinator_server::maintenance::MaintenanceOptions {
                    batch_size,
                    max_batches,
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
            axum::serve(listener, router(state).into_make_service())
                .with_graceful_shutdown(shutdown())
                .await?;
        }
    }
    Ok(())
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
