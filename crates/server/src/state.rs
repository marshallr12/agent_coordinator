use sqlx::{
    ConnectOptions, Row, Sqlite, SqlitePool, Transaction,
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

use crate::error::AppError;

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
    /// Production clocks advance protected service time from a monotonic
    /// anchor. Deterministic injected test clocks may explicitly opt out.
    fn use_monotonic_elapsed(&self) -> bool {
        true
    }
}
pub struct SystemClock;
impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        chrono::Utc::now().timestamp_millis()
    }
    fn use_monotonic_elapsed(&self) -> bool {
        true
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
    clock_runtime: Arc<Mutex<ClockRuntime>>,
}

struct ClockRuntime {
    high_water_ms: i64,
    anchor_time_ms: i64,
    anchor: Instant,
    initialized: bool,
    incident_active: bool,
}

pub(crate) const MATERIAL_CLOCK_ROLLBACK_MS: i64 = 5_000;

pub(crate) struct ClockSample {
    pub now: i64,
    pub incident_active: bool,
    pub incident_detected: bool,
}

pub(crate) struct ClockReconciliation {
    pub incident_id: String,
    pub now: i64,
    pub observed_wall_time_ms: i64,
    pub high_water_time_ms: i64,
}

impl AppState {
    pub fn now(&self) -> i64 {
        let raw = self.clock.now_ms().max(0);
        let mut runtime = self
            .clock_runtime
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !runtime.initialized {
            runtime.high_water_ms = raw;
            runtime.anchor_time_ms = raw;
            runtime.anchor = Instant::now();
            runtime.initialized = true;
        } else {
            let elapsed = if self.clock.use_monotonic_elapsed() {
                runtime.anchor.elapsed().as_millis().min(i64::MAX as u128) as i64
            } else {
                0
            };
            let expected = runtime.anchor_time_ms.saturating_add(elapsed);
            runtime.high_water_ms = runtime.high_water_ms.max(expected).max(raw);
            if raw > expected {
                runtime.anchor_time_ms = raw;
                runtime.anchor = Instant::now();
            }
        }
        runtime.high_water_ms
    }

    pub(crate) fn raw_now(&self) -> i64 {
        self.clock.now_ms().max(0)
    }

    /// Sample service time while the caller holds SQLite's writer lock. A
    /// rollback incident is durable even when the requested operation is later
    /// rejected. Callers that reject a newly detected incident must commit the
    /// clock-only transaction first.
    pub(crate) async fn sample_clock(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
    ) -> Result<ClockSample, AppError> {
        let raw = self.raw_now();
        let (runtime_high_water, expected_runtime_time, runtime_initialized) = {
            let runtime = self
                .clock_runtime
                .lock()
                .map_err(|_| AppError::internal())?;
            let elapsed = if self.clock.use_monotonic_elapsed() {
                runtime.anchor.elapsed().as_millis().min(i64::MAX as u128) as i64
            } else {
                0
            };
            (
                runtime.high_water_ms,
                runtime.anchor_time_ms.saturating_add(elapsed),
                runtime.initialized,
            )
        };
        let row = sqlx::query("SELECT last_safe_time_ms,status FROM clock_state WHERE singleton=1")
            .fetch_one(&mut **tx)
            .await?;
        let stored_high_water: i64 = row.get("last_safe_time_ms");
        let previous_high_water = stored_high_water.max(runtime_high_water);
        let was_active = row.get::<String, _>("status") == "clock_reconciliation";
        let expected = if runtime_initialized {
            expected_runtime_time.max(stored_high_water)
        } else {
            stored_high_water
        };
        let rollback = !was_active && raw.saturating_add(MATERIAL_CLOCK_ROLLBACK_MS) < expected;
        // Even a small rollback is clamped to elapsed monotonic time. This
        // avoids extending a deadline while tolerating normal wall-clock
        // adjustment below the material-incident threshold.
        let safe_now = expected.max(raw).max(previous_high_water);
        let mut incident_detected = false;
        if rollback && !was_active {
            let incident_id = uuid::Uuid::new_v4().to_string();
            sqlx::query("INSERT INTO clock_incidents(id,observed_wall_time_ms,high_water_time_ms,detected_at) VALUES(?,?,?,?)")
                .bind(&incident_id)
                .bind(raw)
                .bind(safe_now)
                .bind(safe_now)
                .execute(&mut **tx)
                .await?;
            sqlx::query("INSERT INTO clock_reconciliation_events(incident_id,kind,initiator_kind,observed_wall_time_ms,high_water_time_ms,reason,created_at) VALUES(?,'rollback_detected','service',?,?,?,?)")
                .bind(&incident_id)
                .bind(raw)
                .bind(safe_now)
                .bind("The wall clock moved behind the durable service-time high-water mark.")
                .bind(safe_now)
                .execute(&mut **tx)
                .await?;
            sqlx::query("UPDATE clock_state SET last_safe_time_ms=?,status='clock_reconciliation',incident_id=?,observed_wall_time_ms=?,detected_at=? WHERE singleton=1")
                .bind(safe_now)
                .bind(&incident_id)
                .bind(raw)
                .bind(safe_now)
                .execute(&mut **tx)
                .await?;
            sqlx::query("UPDATE attempts SET state='expired',ended_at=COALESCE(ended_at,?),outcome=COALESCE(outcome,'Authority expired after a material service-clock rollback.') WHERE state='active'")
                .bind(safe_now)
                .execute(&mut **tx)
                .await?;
            sqlx::query(
                "UPDATE reporters SET expires_at=MIN(expires_at,?),renew_until=MIN(renew_until,?)",
            )
            .bind(safe_now)
            .bind(safe_now)
            .execute(&mut **tx)
            .await?;
            incident_detected = true;
        } else if safe_now > stored_high_water {
            sqlx::query("UPDATE clock_state SET last_safe_time_ms=? WHERE singleton=1")
                .bind(safe_now)
                .execute(&mut **tx)
                .await?;
        }
        let effective_now = {
            let mut runtime = self
                .clock_runtime
                .lock()
                .map_err(|_| AppError::internal())?;
            let elapsed = if self.clock.use_monotonic_elapsed() {
                runtime.anchor.elapsed().as_millis().min(i64::MAX as u128) as i64
            } else {
                0
            };
            let live_expected = runtime.anchor_time_ms.saturating_add(elapsed);
            let live_high_water = runtime.high_water_ms.max(live_expected);
            let effective_now = safe_now.max(live_high_water);
            // Re-anchor only for a forward jump beyond the value another
            // thread may have advanced while this sample awaited SQLite.
            if safe_now > live_high_water {
                runtime.anchor_time_ms = safe_now;
                runtime.anchor = Instant::now();
            } else if raw > live_expected && raw >= live_high_water {
                runtime.anchor_time_ms = raw;
                runtime.anchor = Instant::now();
            }
            runtime.high_water_ms = effective_now;
            runtime.initialized = true;
            runtime.incident_active = was_active || rollback;
            effective_now
        };
        Ok(ClockSample {
            now: effective_now,
            incident_active: was_active || rollback,
            incident_detected,
        })
    }

    /// Authenticate reads against a durable, nondecreasing service time. The
    /// short writer transaction never spans request or process work.
    pub(crate) async fn authoritative_now(&self) -> Result<ClockSample, AppError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let sample = self.sample_clock(&mut tx).await?;
        tx.commit().await?;
        Ok(sample)
    }

    pub(crate) async fn reconcile_clock_in_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        incident_id: &str,
        reason: &str,
        initiator_kind: &str,
        actor_id: Option<&str>,
    ) -> Result<ClockReconciliation, AppError> {
        if reason.trim() != reason
            || reason.is_empty()
            || reason.len() > 2_000
            || reason
                .chars()
                .any(|value| value.is_control() && !matches!(value, '\n' | '\t'))
        {
            return Err(AppError::bad_request(
                "reason must contain 1 to 2000 bytes without surrounding whitespace or control characters other than newlines and tabs.",
            ));
        }
        if !matches!(initiator_kind, "authenticated_admin" | "host_operator") {
            return Err(AppError::internal());
        }
        if initiator_kind == "authenticated_admin" && actor_id.is_none()
            || initiator_kind == "host_operator" && actor_id.is_some()
        {
            return Err(AppError::internal());
        }
        let row = sqlx::query("SELECT status,incident_id,last_safe_time_ms,observed_wall_time_ms FROM clock_state WHERE singleton=1")
            .fetch_one(&mut **tx)
            .await?;
        if row.get::<String, _>("status") != "clock_reconciliation" {
            return Err(AppError::conflict(
                "clock_not_paused",
                "There is no active service-clock incident to reconcile.",
            ));
        }
        let current_incident: String = row.get("incident_id");
        if current_incident != incident_id {
            return Err(AppError::conflict(
                "clock_incident_changed",
                "The active service-clock incident changed. Reload its status before reconciling it.",
            ));
        }
        let high_water_time_ms: i64 = row.get("last_safe_time_ms");
        let observed_wall_time_ms: i64 = row.get("observed_wall_time_ms");
        let raw = self.raw_now();
        if raw < high_water_time_ms {
            return Err(AppError::conflict(
                "clock_still_untrusted",
                "The wall clock remains behind durable service time. Correct the host clock before reconciling this incident.",
            )
            .with_details(serde_json::json!({
                "observed_wall_time_ms":raw,
                "required_safe_time_ms":high_water_time_ms,
            })));
        }
        let now = raw.max(high_water_time_ms);
        sqlx::query("UPDATE clock_incidents SET recovered_at=?,recovery_reason=?,recovered_by=?,recovery_kind=? WHERE id=? AND recovered_at IS NULL")
            .bind(now)
            .bind(reason)
            .bind(actor_id)
            .bind(initiator_kind)
            .bind(incident_id)
            .execute(&mut **tx)
            .await?;
        sqlx::query("INSERT INTO clock_reconciliation_events(incident_id,kind,initiator_kind,actor_id,observed_wall_time_ms,high_water_time_ms,reason,created_at) VALUES(?,'clock_reconciled',?,?,?,?,?,?)")
            .bind(incident_id)
            .bind(initiator_kind)
            .bind(actor_id)
            .bind(raw)
            .bind(high_water_time_ms)
            .bind(reason)
            .bind(now)
            .execute(&mut **tx)
            .await?;
        sqlx::query("UPDATE clock_state SET last_safe_time_ms=?,status='ready',observed_wall_time_ms=? WHERE singleton=1")
            .bind(now)
            .bind(raw)
            .execute(&mut **tx)
            .await?;
        {
            let mut runtime = self
                .clock_runtime
                .lock()
                .map_err(|_| AppError::internal())?;
            runtime.high_water_ms = now;
            runtime.anchor_time_ms = now;
            runtime.anchor = Instant::now();
            runtime.initialized = true;
            runtime.incident_active = false;
        }
        Ok(ClockReconciliation {
            incident_id: incident_id.to_owned(),
            now,
            observed_wall_time_ms,
            high_water_time_ms,
        })
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
        let clock_row =
            sqlx::query("SELECT last_safe_time_ms,status FROM clock_state WHERE singleton=1")
                .fetch_one(&pool)
                .await?;
        let durable_high_water: i64 = clock_row.get("last_safe_time_ms");
        let clock_runtime = ClockRuntime {
            high_water_ms: durable_high_water,
            anchor_time_ms: durable_high_water,
            anchor: Instant::now(),
            initialized: true,
            incident_active: clock_row.get::<String, _>("status") == "clock_reconciliation",
        };
        Ok(Self {
            pool,
            config,
            clock: Arc::new(SystemClock),
            login_limits: Arc::new(Mutex::new(LoginLimits::default())),
            password_workers: Arc::new(Semaphore::new(2)),
            dummy_password_hash: Arc::new(dummy_password_hash),
            clock_runtime: Arc::new(Mutex::new(clock_runtime)),
        })
    }
}

pub(crate) fn clock_reconciliation_error() -> AppError {
    AppError::conflict(
        "clock_reconciliation_required",
        "The service clock moved backward. Restore trustworthy time and reconcile the recorded incident before granting or changing authority.",
    )
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
