use sqlx::{
    ConnectOptions, Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use std::{
    collections::HashMap,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;

#[derive(Clone)]
pub struct Config {
    pub database_path: PathBuf,
    pub listen: SocketAddr,
    pub public_origin: String,
    pub allow_insecure_loopback: bool,
    pub json_body_limit_bytes: usize,
    pub artifact_quota_bytes: i64,
    pub artifact_disk_reserve_bytes: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            database_path: "data/coordinator.sqlite3".into(),
            listen: "127.0.0.1:8080".parse().unwrap(),
            public_origin: "https://localhost".into(),
            allow_insecure_loopback: false,
            json_body_limit_bytes: 1024 * 1024,
            artifact_quota_bytes: 10 * 1024 * 1024 * 1024,
            artifact_disk_reserve_bytes: 256 * 1024 * 1024,
        }
    }
}

impl Config {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            (1024..=1024 * 1024).contains(&self.json_body_limit_bytes),
            "json_body_limit_bytes must be 1024 through 1048576."
        );
        anyhow::ensure!(
            self.artifact_quota_bytes > 0,
            "artifact_quota_bytes must be positive."
        );
        anyhow::ensure!(
            self.artifact_disk_reserve_bytes >= 16 * 1024 * 1024,
            "artifact_disk_reserve_bytes must retain at least 16 MiB."
        );
        anyhow::ensure!(
            self.listen.ip().is_loopback(),
            "The service listener must be loopback; expose HTTPS through the reverse proxy."
        );
        let origin = url::Url::parse(&self.public_origin)?;
        anyhow::ensure!(
            origin.username().is_empty()
                && origin.password().is_none()
                && origin.path() == "/"
                && origin.query().is_none()
                && origin.fragment().is_none()
                && origin.host_str().is_some(),
            "public_origin must be an origin without credentials, path, query, or fragment."
        );
        anyhow::ensure!(
            self.public_origin == origin.origin().ascii_serialization(),
            "public_origin must use canonical scheme://host[:port] form without a trailing slash."
        );
        let local_origin = match origin.host() {
            Some(url::Host::Domain(name)) => name == "localhost",
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        };
        anyhow::ensure!(
            origin.scheme() == "https"
                || (origin.scheme() == "http" && local_origin && self.allow_insecure_loopback),
            "HTTPS is required. Development HTTP requires --allow-insecure-loopback and a loopback origin."
        );
        Ok(())
    }
    pub(crate) fn secure_cookie(&self) -> bool {
        self.public_origin.starts_with("https://")
    }
}

pub trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
}
pub struct SystemClock;
impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        chrono::Utc::now().timestamp_millis()
    }
}

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub config: Config,
    pub clock: Arc<dyn Clock>,
    pub(crate) login_limits: Arc<Mutex<LoginLimits>>,
    pub(crate) password_workers: Arc<Semaphore>,
    pub(crate) dummy_password_hash: Arc<String>,
}

impl AppState {
    pub fn now(&self) -> i64 {
        self.clock.now_ms()
    }
    pub async fn open(config: Config) -> anyhow::Result<Self> {
        Self::open_internal(config, true).await
    }

    /// Open a current installation for backup without creating or migrating it.
    pub async fn open_existing_read_only(config: Config) -> anyhow::Result<Self> {
        anyhow::ensure!(
            config.database_path.is_file(),
            "Backup requires an existing database file."
        );
        Self::open_internal(config, false).await
    }

    async fn open_internal(config: Config, initialize: bool) -> anyhow::Result<Self> {
        config.validate()?;
        if config.database_path != std::path::Path::new(":memory:")
            && let Ok(metadata) = std::fs::symlink_metadata(&config.database_path)
        {
            anyhow::ensure!(
                metadata.is_file() && !metadata.file_type().is_symlink(),
                "Database path must be a regular file."
            );
        }
        if initialize && config.database_path != std::path::Path::new(":memory:") {
            if let Some(parent) = config
                .database_path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
            {
                let mut builder = std::fs::DirBuilder::new();
                builder.recursive(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt;
                    builder.mode(0o700);
                }
                builder.create(parent)?;
            }
            let mut file = std::fs::OpenOptions::new();
            file.create(true).append(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                file.mode(0o600);
            }
            let db = file.open(&config.database_path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                db.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            }
            drop(db);
        }
        let options = SqliteConnectOptions::new()
            .filename(&config.database_path)
            .create_if_missing(initialize)
            .read_only(!initialize)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .busy_timeout(Duration::from_secs(5))
            .disable_statement_logging();
        let pool = SqlitePoolOptions::new()
            .max_connections(
                if config.database_path == std::path::Path::new(":memory:") {
                    1
                } else {
                    8
                },
            )
            .connect_with(options)
            .await?;
        if initialize {
            sqlx::migrate!("./migrations").run(&pool).await?;
        } else {
            validate_current_schema(&pool).await?;
        }
        let dummy_password_hash = crate::auth::hash_password(crate::auth::secret()).await?;
        Ok(Self {
            pool,
            config,
            clock: Arc::new(SystemClock),
            login_limits: Arc::new(Mutex::new(LoginLimits::default())),
            password_workers: Arc::new(Semaphore::new(2)),
            dummy_password_hash: Arc::new(dummy_password_hash),
        })
    }
}

/// A backup command must never upgrade the database of a running older server.
/// Require this executable's exact successful migration set before reading it.
pub async fn validate_current_schema(pool: &SqlitePool) -> anyhow::Result<()> {
    let migrator = sqlx::migrate!("./migrations");
    let expected = migrator
        .iter()
        .filter(|migration| migration.migration_type.is_up_migration())
        .collect::<Vec<_>>();
    let applied =
        sqlx::query("SELECT version,success,checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(pool)
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "Database schema is unavailable; use the matching service executable."
                )
            })?;
    anyhow::ensure!(
        applied.len() == expected.len(),
        "Database schema differs from this executable; use the matching service version."
    );
    for (row, migration) in applied.iter().zip(expected) {
        anyhow::ensure!(
            row.get::<i64, _>("version") == migration.version
                && row.get::<bool, _>("success")
                && row.get::<Vec<u8>, _>("checksum").as_slice() == migration.checksum.as_ref(),
            "Database migration history differs from this executable; use the matching service version."
        );
    }
    Ok(())
}

/// Monotonic time avoids clock adjustments bypassing limits. The global budget
/// also bounds work across invented usernames and untrusted proxy headers.
#[derive(Default)]
pub(crate) struct LoginLimits {
    global: Option<(Instant, u32)>,
    names: HashMap<String, (Instant, u32)>,
}
impl LoginLimits {
    pub(crate) fn admit(&mut self, username: &str) -> bool {
        let now = Instant::now();
        self.names
            .retain(|_, (started, _)| now.duration_since(*started) < Duration::from_secs(60));
        let global = self.global.get_or_insert((now, 0));
        if now.duration_since(global.0) >= Duration::from_secs(60) {
            *global = (now, 0);
        }
        if global.1 >= 30 {
            return false;
        }
        global.1 += 1;
        if !self.names.contains_key(username) && self.names.len() >= 1024 {
            return false;
        }
        let count = self.names.entry(username.to_owned()).or_insert((now, 0));
        if count.1 >= 5 {
            return false;
        }
        count.1 += 1;
        true
    }
}
