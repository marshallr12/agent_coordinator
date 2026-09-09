//! Durable, local-only supervision for explicitly registered workstation jobs.
//!
//! The service never invokes this crate. A workstation CLI persists identities
//! here before registration, adds a scoped reporter credential after successful
//! registration, and only then starts a detached guardian. A durable launch
//! intent prevents an interrupted guardian from launching the same producer a
//! second time.

mod persist;
mod process;

use std::collections::BTreeMap;
use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use chrono::DateTime;
use coordinator_client::CoordinatorClient;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

pub use process::ProcessIdentity;

const STATE_VERSION: u32 = 1;
const MAX_LOG_BYTES: u64 = 64 * 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const RUNNING_OBSERVATION_INTERVAL_MS: i64 = 30_000;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct JobIdentities {
    pub job_id: String,
    pub producer_id: String,
    pub runner_instance_id: String,
    pub reporter_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// An exact foreground producer invocation.
///
/// The program must remain attached for the full lifetime of the registered
/// producer. A launcher that exits after starting a detached descendant cannot
/// supply terminal evidence for that descendant and requires separate recovery.
pub struct CommandSpec {
    pub program: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    pub working_directory: PathBuf,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceSnapshot {
    pub checkout: PathBuf,
    pub revision: String,
    pub tree: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InitializeJob {
    pub state_file: PathBuf,
    pub identities: JobIdentities,
    pub command: CommandSpec,
    pub source: SourceSnapshot,
    pub log_limit_bytes: u64,
    pub harness: Option<ProcessIdentity>,
}

#[derive(Clone, Eq, PartialEq)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty() || value.chars().any(char::is_whitespace) {
            bail!("reporter bearer token is empty or contains whitespace");
        }
        Ok(Self(value))
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RenewalConfig {
    pub generation: i64,
    pub renew_after_seconds: u64,
    pub renew_until: String,
}

#[derive(Clone, Eq, PartialEq)]
pub struct ReporterRegistration {
    pub service_origin: String,
    pub allow_insecure_loopback: bool,
    pub bearer_token: SecretString,
    pub renewal: Option<RenewalConfig>,
}

impl ReporterRegistration {
    pub fn new(
        service_origin: impl Into<String>,
        allow_insecure_loopback: bool,
        bearer_token: impl Into<String>,
        renewal: Option<RenewalConfig>,
    ) -> Result<Self> {
        Ok(Self {
            service_origin: service_origin.into(),
            allow_insecure_loopback,
            bearer_token: SecretString::new(bearer_token)?,
            renewal,
        })
    }
}

impl fmt::Debug for ReporterRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReporterRegistration")
            .field("service_origin", &self.service_origin)
            .field("allow_insecure_loopback", &self.allow_insecure_loopback)
            .field("bearer_token", &self.bearer_token)
            .field("renewal", &self.renewal)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardianMode {
    Run,
    Observe,
}

impl GuardianMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Observe => "observe",
        }
    }
}

impl std::str::FromStr for GuardianMode {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "run" => Ok(Self::Run),
            "observe" => Ok(Self::Observe),
            _ => bail!("guardian mode must be run or observe"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobPhase {
    Prepared,
    Registered,
    LaunchIntent,
    Running,
    Succeeded,
    Failed,
    NotStarted,
    Unknown,
}

impl JobPhase {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::NotStarted)
    }

    pub fn was_launched(self) -> bool {
        matches!(
            self,
            Self::Running | Self::Succeeded | Self::Failed | Self::Unknown
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct JobSummary {
    pub state_file: PathBuf,
    pub identities: JobIdentities,
    pub phase: JobPhase,
    pub registered: bool,
    pub process: Option<ProcessIdentity>,
    pub exit_code: Option<i32>,
    pub inputs_unchanged: Option<bool>,
    pub pending_observations: usize,
    pub reporting_disabled: bool,
    pub last_report_error: Option<String>,
    pub stdout_log: PathBuf,
    pub stderr_log: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardianOutcome {
    Completed,
    AlreadyActive,
    ObservedUnknown,
    NothingToObserve,
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
struct StoredReporter {
    service_origin: String,
    allow_insecure_loopback: bool,
    bearer_token: String,
    renewal: Option<RenewalConfig>,
    next_renew_ms: Option<i64>,
    pending_renewal: Option<PendingRenewal>,
    reporting_disabled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct PendingRenewal {
    idempotency_key: String,
    body: Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct PendingObservation {
    idempotency_key: String,
    body: Observation,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct Observation {
    sequence: u64,
    producer_id: String,
    state: JobPhase,
    pid: Option<u32>,
    process_started_at: Option<String>,
    exit_code: Option<i32>,
    inputs_unchanged: Option<bool>,
    summary: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct TerminalEvidence {
    phase: JobPhase,
    exit_code: Option<i32>,
    inputs_unchanged: bool,
    summary: String,
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum JournalKind {
    Initialized,
    Registered {
        reporter: StoredReporter,
        observation: PendingObservation,
    },
    LaunchIntent,
    ProducerStarted {
        process: ProcessIdentity,
        started_at_ms: i64,
        observation: PendingObservation,
    },
    Terminal {
        evidence: TerminalEvidence,
        observation: PendingObservation,
    },
    Unknown {
        inputs_unchanged: Option<bool>,
        summary: String,
        observation: PendingObservation,
    },
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
struct JournalEvent {
    revision: u64,
    at_ms: i64,
    event: JournalKind,
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
struct StoredJob {
    version: u32,
    revision: u64,
    identities: JobIdentities,
    command: CommandSpec,
    source: SourceSnapshot,
    log_limit_bytes: u64,
    harness: Option<ProcessIdentity>,
    reporter: Option<StoredReporter>,
    phase: JobPhase,
    process: Option<ProcessIdentity>,
    process_started_at_ms: Option<i64>,
    exit_code: Option<i32>,
    inputs_unchanged: Option<bool>,
    next_sequence: u64,
    pending_observations: Vec<PendingObservation>,
    last_running_observation_ms: Option<i64>,
    last_report_error: Option<String>,
}

pub fn capture_process_identity(pid: u32) -> Result<Option<ProcessIdentity>> {
    process::capture(pid)
}

pub fn verify_clean_snapshot(snapshot: &SourceSnapshot) -> Result<()> {
    if snapshot.revision.is_empty() || snapshot.tree.is_empty() {
        bail!("source revision and tree must be full non-empty identities");
    }
    let revision = git_output(&snapshot.checkout, &["rev-parse", "HEAD^{commit}"])?;
    let tree = git_output(&snapshot.checkout, &["rev-parse", "HEAD^{tree}"])?;
    let status = git_output(
        &snapshot.checkout,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?;
    if revision != snapshot.revision {
        bail!("checkout HEAD does not match the supplied source revision");
    }
    if tree != snapshot.tree {
        bail!("checkout tree does not match the supplied source tree");
    }
    if !status.is_empty() {
        bail!("checkout is not clean; local jobs require a committed source snapshot");
    }
    Ok(())
}

pub fn initialize_launch_state(input: InitializeJob) -> Result<JobSummary> {
    validate_initialize(&input)?;
    verify_clean_snapshot(&input.source)?;
    let paths = persist::paths(&input.state_file)?;
    persist::prepare_directory(&paths.directory)?;
    let _lock = persist::lock(&paths, true)?.context("acquire local job lock")?;
    if paths.state.exists() {
        let existing = recover(&paths)?;
        if existing.identities != input.identities
            || existing.command != input.command
            || existing.source != input.source
            || existing.log_limit_bytes != input.log_limit_bytes
            || existing.harness != input.harness
        {
            bail!("local job state already exists with different launch inputs");
        }
        return Ok(summary(&paths, &existing));
    }
    let state = StoredJob {
        version: STATE_VERSION,
        revision: 1,
        identities: input.identities,
        command: input.command,
        source: input.source,
        log_limit_bytes: input.log_limit_bytes,
        harness: input.harness,
        reporter: None,
        phase: JobPhase::Prepared,
        process: None,
        process_started_at_ms: None,
        exit_code: None,
        inputs_unchanged: None,
        next_sequence: 1,
        pending_observations: Vec::new(),
        last_running_observation_ms: None,
        last_report_error: None,
    };
    persist::save(&paths, &state)?;
    persist::append_event(
        &paths,
        &JournalEvent {
            revision: state.revision,
            at_ms: now_ms(),
            event: JournalKind::Initialized,
        },
    )?;
    Ok(summary(&paths, &state))
}

pub fn record_registration(
    state_file: &Path,
    registration: ReporterRegistration,
) -> Result<JobSummary> {
    let paths = persist::paths(state_file)?;
    let _lock = persist::lock(&paths, true)?.context("acquire local job lock")?;
    let mut state = recover(&paths)?;
    if let Some(renewal) = registration.renewal.as_ref() {
        renewal_until_ms(renewal)?;
        if renewal.renew_after_seconds == 0 {
            bail!("renewal cadence must be positive when renewal is enabled");
        }
    }
    let reporter = StoredReporter {
        service_origin: registration.service_origin,
        allow_insecure_loopback: registration.allow_insecure_loopback,
        bearer_token: registration.bearer_token.0,
        next_renew_ms: registration
            .renewal
            .as_ref()
            .map(|renewal| now_ms() + seconds_ms(renewal.renew_after_seconds)),
        renewal: registration.renewal,
        pending_renewal: None,
        reporting_disabled: false,
    };
    if let Some(existing) = &state.reporter {
        if existing.service_origin != reporter.service_origin
            || existing.allow_insecure_loopback != reporter.allow_insecure_loopback
            || existing.bearer_token != reporter.bearer_token
            || existing.renewal != reporter.renewal
        {
            bail!("local job is already bound to a different reporter registration");
        }
        return Ok(summary(&paths, &state));
    }
    if state.phase != JobPhase::Prepared {
        bail!("reporter registration cannot be changed after launch preparation");
    }
    validate_reporter_token(&state.identities.reporter_id, &reporter.bearer_token)?;
    state.reporter = Some(reporter);
    state.phase = JobPhase::Registered;
    enqueue_observation(
        &mut state,
        JobPhase::Registered,
        None,
        None,
        None,
        "Local job registration is durable; producer has not been launched.".into(),
    );
    let observation = state
        .pending_observations
        .last()
        .expect("registered observation was just enqueued")
        .clone();
    let journal_reporter = state.reporter.clone().expect("reporter was just installed");
    commit_event(
        &paths,
        &mut state,
        JournalKind::Registered {
            reporter: journal_reporter,
            observation,
        },
    )?;
    Ok(summary(&paths, &state))
}

pub fn start_guardian(executable: &Path, state_file: &Path, mode: GuardianMode) -> Result<()> {
    process::spawn_guardian_process(executable, state_file, mode)
}

pub fn inspect_job(state_file: &Path) -> Result<JobSummary> {
    let paths = persist::paths(state_file)?;
    let state = recover_readonly(&paths)?;
    Ok(summary(&paths, &state))
}

pub async fn run_guardian(state_file: &Path, mode: GuardianMode) -> Result<GuardianOutcome> {
    let paths = persist::paths(state_file)?;
    let Some(_lock) = persist::lock(&paths, false)? else {
        return Ok(GuardianOutcome::AlreadyActive);
    };
    let mut state = recover(&paths)?;
    if state.reporter.is_none() {
        bail!("local job has not been successfully registered");
    }
    flush_reports(&paths, &mut state).await?;

    match (mode, state.phase) {
        (GuardianMode::Run, JobPhase::Registered) => launch_and_watch(&paths, &mut state).await,
        (GuardianMode::Run, JobPhase::Prepared) => {
            bail!("local job has not been successfully registered")
        }
        (_, JobPhase::LaunchIntent) => {
            set_unknown(
                &paths,
                &mut state,
                None,
                "A durable launch intent exists without a recorded process identity; refusing to relaunch.",
            )?;
            flush_reports(&paths, &mut state).await?;
            Ok(GuardianOutcome::ObservedUnknown)
        }
        (_, JobPhase::Running) => observe_running(&paths, &mut state).await,
        (_, JobPhase::Unknown) => {
            flush_reports(&paths, &mut state).await?;
            Ok(GuardianOutcome::ObservedUnknown)
        }
        (_, phase) if phase.is_terminal() => {
            flush_reports(&paths, &mut state).await?;
            Ok(GuardianOutcome::Completed)
        }
        (GuardianMode::Observe, JobPhase::Prepared | JobPhase::Registered) => {
            Ok(GuardianOutcome::NothingToObserve)
        }
        _ => Ok(GuardianOutcome::NothingToObserve),
    }
}

async fn launch_and_watch(
    paths: &persist::JobPaths,
    state: &mut StoredJob,
) -> Result<GuardianOutcome> {
    match check_launch_authority(state).await? {
        LaunchAuthority::Allowed(deadline) => {
            if let Err(error) = verify_clean_snapshot(&state.source) {
                let evidence = TerminalEvidence {
                    phase: JobPhase::NotStarted,
                    exit_code: None,
                    inputs_unchanged: false,
                    summary: "The committed source snapshot changed before launch.".into(),
                };
                apply_terminal(paths, state, evidence)?;
                flush_reports(paths, state).await?;
                return Err(error);
            }
            prepare_logs(paths, state.log_limit_bytes)?;
            state.phase = JobPhase::LaunchIntent;
            commit_event(paths, state, JournalKind::LaunchIntent)?;
            if std::time::Instant::now() >= deadline {
                let evidence = TerminalEvidence {
                    phase: JobPhase::NotStarted,
                    exit_code: None,
                    inputs_unchanged: inputs_unchanged(&state.source),
                    summary:
                        "Launch authority expired before the operating system spawn was attempted."
                            .into(),
                };
                apply_terminal(paths, state, evidence)?;
                flush_reports(paths, state).await?;
                return Ok(GuardianOutcome::Completed);
            }
        }
        LaunchAuthority::Denied(message) => {
            let evidence = TerminalEvidence {
                phase: JobPhase::NotStarted,
                exit_code: None,
                inputs_unchanged: inputs_unchanged(&state.source),
                summary: message,
            };
            apply_terminal(paths, state, evidence)?;
            flush_reports(paths, state).await?;
            return Ok(GuardianOutcome::Completed);
        }
        LaunchAuthority::Uncertain(message) => {
            state.last_report_error = Some(message);
            persist_state(paths, state)?;
            return Ok(GuardianOutcome::NothingToObserve);
        }
    }
    let mut child = match process::spawn_producer(&state.command, state.log_limit_bytes > 0) {
        Ok(child) => child,
        Err(error) => {
            let unchanged = inputs_unchanged(&state.source);
            let evidence = TerminalEvidence {
                phase: JobPhase::NotStarted,
                exit_code: None,
                inputs_unchanged: unchanged,
                summary: "The operating system rejected the producer launch.".into(),
            };
            apply_terminal(paths, state, evidence)?;
            flush_reports(paths, state).await?;
            return Err(error);
        }
    };
    let pid = child.id();
    let mut log_drains = Vec::new();
    if state.log_limit_bytes > 0 {
        if let Some(stdout) = child.stdout.take() {
            log_drains.push(spawn_log_drain(
                stdout,
                paths.stdout.clone(),
                state.log_limit_bytes,
            ));
        }
        if let Some(stderr) = child.stderr.take() {
            log_drains.push(spawn_log_drain(
                stderr,
                paths.stderr.clone(),
                state.log_limit_bytes,
            ));
        }
    }
    let process = process::capture(pid)?;
    let started_at_ms = now_ms();
    if let Some(process) = process {
        state.phase = JobPhase::Running;
        state.process = Some(process.clone());
        state.process_started_at_ms = Some(started_at_ms);
        enqueue_observation(
            state,
            JobPhase::Running,
            Some(pid),
            None,
            None,
            "The registered local producer is running.".into(),
        );
        state.last_running_observation_ms = Some(started_at_ms);
        let observation = state
            .pending_observations
            .last()
            .expect("running observation was just enqueued")
            .clone();
        commit_event(
            paths,
            state,
            JournalKind::ProducerStarted {
                process,
                started_at_ms,
                observation,
            },
        )?;
    }

    loop {
        flush_reports(paths, state).await?;
        if let Some(status) = child.try_wait().context("observe local producer")? {
            for drain in &log_drains {
                let _ = drain.recv_timeout(Duration::from_secs(2));
            }
            let exit_code = status.code().unwrap_or(platform_interrupted_exit_code());
            let evidence = TerminalEvidence {
                phase: if status.success() {
                    JobPhase::Succeeded
                } else {
                    JobPhase::Failed
                },
                exit_code: Some(exit_code),
                inputs_unchanged: inputs_unchanged(&state.source),
                summary: if status.success() {
                    "The local producer exited successfully.".into()
                } else {
                    "The local producer exited unsuccessfully.".into()
                },
            };
            apply_terminal(paths, state, evidence)?;
            flush_reports(paths, state).await?;
            return Ok(GuardianOutcome::Completed);
        }
        maybe_enqueue_running(paths, state)?;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

enum LaunchAuthority {
    Allowed(std::time::Instant),
    Denied(String),
    Uncertain(String),
}

async fn check_launch_authority(state: &StoredJob) -> Result<LaunchAuthority> {
    const SAFETY_MARGIN_MS: i64 = 2_000;
    let reporter = state.reporter.as_ref().context("reporter is registered")?;
    let client = match CoordinatorClient::new(
        &reporter.service_origin,
        reporter.bearer_token.clone(),
        reporter.allow_insecure_loopback,
    ) {
        Ok(client) => client,
        Err(error) => return Ok(LaunchAuthority::Uncertain(error.to_string())),
    };
    let path = format!("/api/v1/reporters/{}", state.identities.reporter_id);
    let started = std::time::Instant::now();
    let response = match client.get(&path, None).await {
        Ok(response) => response,
        Err(error) => return Ok(LaunchAuthority::Uncertain(error.to_string())),
    };
    if matches!(response.status, 401 | 403 | 404) {
        return Ok(LaunchAuthority::Denied(
            "The service denied launch authority before the producer was started.".into(),
        ));
    }
    if !response.is_success() {
        return Ok(LaunchAuthority::Uncertain(format!(
            "service launch-authority check returned HTTP {}",
            response.status
        )));
    }
    let Some(data) = response.body.get("data") else {
        return Ok(LaunchAuthority::Uncertain(
            "launch-authority response omitted data".into(),
        ));
    };
    let Some(job) = data.get("job") else {
        return Ok(LaunchAuthority::Uncertain(
            "launch-authority response omitted job identity".into(),
        ));
    };
    let Some(reporter) = data.get("reporter") else {
        return Ok(LaunchAuthority::Uncertain(
            "launch-authority response omitted reporter identity".into(),
        ));
    };
    let identity_matches = job.get("id").and_then(Value::as_str)
        == Some(state.identities.job_id.as_str())
        && job.get("producer_id").and_then(Value::as_str)
            == Some(state.identities.producer_id.as_str())
        && job.get("runner_instance_id").and_then(Value::as_str)
            == Some(state.identities.runner_instance_id.as_str())
        && reporter.get("id").and_then(Value::as_str)
            == Some(state.identities.reporter_id.as_str());
    let source_matches = job.get("source_revision").and_then(Value::as_str)
        == Some(state.source.revision.as_str())
        && job.get("source_tree").and_then(Value::as_str) == Some(state.source.tree.as_str());
    if !identity_matches || !source_matches {
        return Ok(LaunchAuthority::Denied(
            "The service reporter is bound to different local job or source identities.".into(),
        ));
    }
    let launch_allowed = reporter
        .get("launch_allowed")
        .and_then(Value::as_bool)
        .context("launch-authority response omitted launch_allowed")?;
    if !launch_allowed {
        return Ok(LaunchAuthority::Denied(
            "The service reported that launch authority is no longer valid.".into(),
        ));
    }
    let remaining = reporter
        .get("lease_remaining_ms")
        .and_then(Value::as_i64)
        .context("launch-authority response omitted lease_remaining_ms")?;
    let elapsed = started.elapsed().as_millis().min(i64::MAX as u128) as i64;
    let safe_remaining = remaining
        .saturating_sub(elapsed)
        .saturating_sub(SAFETY_MARGIN_MS);
    if safe_remaining <= 0 {
        return Ok(LaunchAuthority::Denied(
            "The remaining task lease was too short to safely launch the producer.".into(),
        ));
    }
    Ok(LaunchAuthority::Allowed(
        std::time::Instant::now() + Duration::from_millis(safe_remaining as u64),
    ))
}

async fn observe_running(
    paths: &persist::JobPaths,
    state: &mut StoredJob,
) -> Result<GuardianOutcome> {
    loop {
        flush_reports(paths, state).await?;
        let Some(identity) = state.process.as_ref() else {
            set_unknown(
                paths,
                state,
                Some(inputs_unchanged(&state.source)),
                "The job was recorded running without a strong process identity.",
            )?;
            flush_reports(paths, state).await?;
            return Ok(GuardianOutcome::ObservedUnknown);
        };
        if !process::is_alive(identity)? {
            set_unknown(
                paths,
                state,
                Some(inputs_unchanged(&state.source)),
                "The exact producer process is no longer observable and no terminal exit was captured.",
            )?;
            flush_reports(paths, state).await?;
            return Ok(GuardianOutcome::ObservedUnknown);
        }
        maybe_enqueue_running(paths, state)?;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

fn maybe_enqueue_running(paths: &persist::JobPaths, state: &mut StoredJob) -> Result<()> {
    let now = now_ms();
    if now - state.last_running_observation_ms.unwrap_or(0) >= RUNNING_OBSERVATION_INTERVAL_MS {
        enqueue_observation(
            state,
            JobPhase::Running,
            state.process.as_ref().map(|process| process.pid),
            None,
            None,
            "The exact registered local producer remains observable.".into(),
        );
        state.last_running_observation_ms = Some(now);
        persist_state(paths, state)?;
    }
    Ok(())
}

fn set_unknown(
    paths: &persist::JobPaths,
    state: &mut StoredJob,
    unchanged: Option<bool>,
    message: &str,
) -> Result<()> {
    state.phase = JobPhase::Unknown;
    state.inputs_unchanged = unchanged;
    enqueue_observation(
        state,
        JobPhase::Unknown,
        state.process.as_ref().map(|process| process.pid),
        None,
        unchanged,
        message.into(),
    );
    commit_event(
        paths,
        state,
        JournalKind::Unknown {
            inputs_unchanged: unchanged,
            summary: message.into(),
            observation: state
                .pending_observations
                .last()
                .expect("unknown observation was just enqueued")
                .clone(),
        },
    )
}

fn apply_terminal(
    paths: &persist::JobPaths,
    state: &mut StoredJob,
    evidence: TerminalEvidence,
) -> Result<()> {
    state.phase = evidence.phase;
    state.exit_code = evidence.exit_code;
    state.inputs_unchanged = Some(evidence.inputs_unchanged);
    enqueue_observation(
        state,
        evidence.phase,
        state.process.as_ref().map(|process| process.pid),
        evidence.exit_code,
        Some(evidence.inputs_unchanged),
        evidence.summary.clone(),
    );
    let observation = state
        .pending_observations
        .last()
        .expect("terminal observation was just enqueued")
        .clone();
    commit_event(
        paths,
        state,
        JournalKind::Terminal {
            evidence,
            observation,
        },
    )
}

fn enqueue_observation(
    state: &mut StoredJob,
    phase: JobPhase,
    pid: Option<u32>,
    exit_code: Option<i32>,
    inputs_unchanged: Option<bool>,
    summary: String,
) {
    let sequence = state.next_sequence;
    state.next_sequence += 1;
    state.pending_observations.push(PendingObservation {
        idempotency_key: Uuid::new_v4().to_string(),
        body: Observation {
            sequence,
            producer_id: state.identities.producer_id.clone(),
            state: phase,
            pid,
            process_started_at: state
                .process
                .as_ref()
                .map(|process| process.start_identity.clone()),
            exit_code,
            inputs_unchanged,
            summary,
        },
    });
}

async fn flush_reports(paths: &persist::JobPaths, state: &mut StoredJob) -> Result<()> {
    let Some(reporter) = state.reporter.as_ref() else {
        return Ok(());
    };
    if reporter.reporting_disabled {
        return Ok(());
    }
    let client = match CoordinatorClient::new(
        &reporter.service_origin,
        reporter.bearer_token.clone(),
        reporter.allow_insecure_loopback,
    ) {
        Ok(client) => client,
        Err(error) => {
            state.last_report_error = Some(error.to_string());
            persist_state(paths, state)?;
            return Ok(());
        }
    };
    while let Some(pending) = state.pending_observations.first().cloned() {
        let path = format!(
            "/api/v1/reporters/{}/observations",
            state.identities.reporter_id
        );
        let body = serde_json::to_value(&pending.body).context("encode job observation")?;
        match client
            .mutate(&path, &body, &pending.idempotency_key, None)
            .await
        {
            Ok(response) if response.is_success() => {
                state.pending_observations.remove(0);
                state.last_report_error = None;
                persist_state(paths, state)?;
            }
            Ok(response) => {
                state.last_report_error = Some(format!(
                    "service rejected observation with HTTP {}",
                    response.status
                ));
                if matches!(response.status, 401 | 403) {
                    state
                        .reporter
                        .as_mut()
                        .expect("reporter exists")
                        .reporting_disabled = true;
                }
                persist_state(paths, state)?;
                if matches!(response.status, 401 | 403) {
                    return Ok(());
                }
                break;
            }
            Err(error) => {
                state.last_report_error = Some(error.to_string());
                persist_state(paths, state)?;
                break;
            }
        }
    }
    maybe_renew(paths, state, &client).await
}

async fn maybe_renew(
    paths: &persist::JobPaths,
    state: &mut StoredJob,
    client: &CoordinatorClient,
) -> Result<()> {
    let now = now_ms();
    let Some(config) = state
        .reporter
        .as_ref()
        .and_then(|reporter| reporter.renewal.clone())
    else {
        return Ok(());
    };
    let renew_until_ms = renewal_until_ms(&config)?;
    if now >= renew_until_ms {
        let reporter = state.reporter.as_mut().expect("reporter exists");
        reporter.renewal = None;
        reporter.pending_renewal = None;
        persist_state(paths, state)?;
        return Ok(());
    }
    let Some(harness) = state.harness.as_ref() else {
        let reporter = state.reporter.as_mut().expect("reporter exists");
        reporter.renewal = None;
        persist_state(paths, state)?;
        return Ok(());
    };
    if !process::is_alive(harness).unwrap_or(false) {
        let reporter = state.reporter.as_mut().expect("reporter exists");
        reporter.renewal = None;
        reporter.pending_renewal = None;
        persist_state(paths, state)?;
        return Ok(());
    }
    let reporter = state.reporter.as_ref().expect("reporter exists");
    if reporter.pending_renewal.is_none() && now < reporter.next_renew_ms.unwrap_or(renew_until_ms)
    {
        return Ok(());
    }
    if reporter.pending_renewal.is_none() {
        let reporter = state.reporter.as_mut().expect("reporter exists");
        reporter.pending_renewal = Some(PendingRenewal {
            idempotency_key: Uuid::new_v4().to_string(),
            body: json!({"generation": config.generation}),
        });
        persist_state(paths, state)?;
    }
    let pending = state
        .reporter
        .as_ref()
        .and_then(|reporter| reporter.pending_renewal.clone())
        .expect("pending renewal was just created");
    let path = format!("/api/v1/reporters/{}/renew", state.identities.reporter_id);
    match client
        .mutate(&path, &pending.body, &pending.idempotency_key, None)
        .await
    {
        Ok(response) if response.is_success() => {
            let reporter = state.reporter.as_mut().expect("reporter exists");
            reporter.pending_renewal = None;
            reporter.next_renew_ms = Some(now + seconds_ms(config.renew_after_seconds));
            state.last_report_error = None;
        }
        Ok(response) => {
            state.last_report_error = Some(format!(
                "service rejected renewal with HTTP {}",
                response.status
            ));
            if matches!(response.status, 401 | 403 | 409) {
                let reporter = state.reporter.as_mut().expect("reporter exists");
                reporter.renewal = None;
                reporter.pending_renewal = None;
            }
        }
        Err(error) => state.last_report_error = Some(error.to_string()),
    }
    persist_state(paths, state)
}

fn recover(paths: &persist::JobPaths) -> Result<StoredJob> {
    let (state, replayed) = recover_inner(paths)?;
    if replayed {
        persist::save(paths, &state)?;
    }
    Ok(state)
}

fn recover_readonly(paths: &persist::JobPaths) -> Result<StoredJob> {
    recover_inner(paths).map(|(state, _)| state)
}

fn recover_inner(paths: &persist::JobPaths) -> Result<(StoredJob, bool)> {
    let mut state = persist::load(paths)?;
    if state.version != STATE_VERSION {
        bail!("unsupported local job state version {}", state.version);
    }
    let events = persist::events(paths)?;
    let saved_revision = state.revision;
    let mut replayed = false;
    for event in events
        .iter()
        .filter(|event| event.revision > saved_revision)
    {
        replayed = true;
        state.revision = event.revision;
        match &event.event {
            JournalKind::LaunchIntent => state.phase = JobPhase::LaunchIntent,
            JournalKind::Registered {
                reporter,
                observation,
            } => {
                state.phase = JobPhase::Registered;
                state.reporter = Some(reporter.clone());
                restore_observation(&mut state, observation);
            }
            JournalKind::ProducerStarted {
                process,
                started_at_ms,
                observation,
            } => {
                state.phase = JobPhase::Running;
                state.process = Some(process.clone());
                state.process_started_at_ms = Some(*started_at_ms);
                restore_observation(&mut state, observation);
            }
            JournalKind::Terminal {
                evidence,
                observation,
            } => {
                state.phase = evidence.phase;
                state.exit_code = evidence.exit_code;
                state.inputs_unchanged = Some(evidence.inputs_unchanged);
                restore_observation(&mut state, observation);
            }
            JournalKind::Unknown {
                inputs_unchanged,
                observation,
                ..
            } => {
                state.phase = JobPhase::Unknown;
                state.inputs_unchanged = *inputs_unchanged;
                restore_observation(&mut state, observation);
            }
            JournalKind::Initialized => {}
        }
    }
    Ok((state, replayed))
}

fn restore_observation(state: &mut StoredJob, observation: &PendingObservation) {
    state.next_sequence = state.next_sequence.max(observation.body.sequence + 1);
    if !state
        .pending_observations
        .iter()
        .any(|pending| pending.idempotency_key == observation.idempotency_key)
    {
        state.pending_observations.push(observation.clone());
        state
            .pending_observations
            .sort_by_key(|pending| pending.body.sequence);
    }
}

fn commit_event(
    paths: &persist::JobPaths,
    state: &mut StoredJob,
    event: JournalKind,
) -> Result<()> {
    state.revision += 1;
    persist::append_event(
        paths,
        &JournalEvent {
            revision: state.revision,
            at_ms: now_ms(),
            event,
        },
    )?;
    persist::save(paths, state)
}

fn persist_state(paths: &persist::JobPaths, state: &mut StoredJob) -> Result<()> {
    state.revision += 1;
    persist::save(paths, state)
}

fn summary(paths: &persist::JobPaths, state: &StoredJob) -> JobSummary {
    JobSummary {
        state_file: paths.state.clone(),
        identities: state.identities.clone(),
        phase: state.phase,
        registered: state.reporter.is_some(),
        process: state.process.clone(),
        exit_code: state.exit_code,
        inputs_unchanged: state.inputs_unchanged,
        pending_observations: state.pending_observations.len(),
        reporting_disabled: state
            .reporter
            .as_ref()
            .is_some_and(|reporter| reporter.reporting_disabled),
        last_report_error: state.last_report_error.clone(),
        stdout_log: paths.stdout.clone(),
        stderr_log: paths.stderr.clone(),
    }
}

fn validate_initialize(input: &InitializeJob) -> Result<()> {
    if !input.state_file.is_absolute()
        || !input.command.program.is_absolute()
        || !input.command.working_directory.is_absolute()
        || !input.source.checkout.is_absolute()
    {
        bail!("state, program, working directory, and checkout paths must be absolute");
    }
    if input.log_limit_bytes > MAX_LOG_BYTES {
        bail!("log limit exceeds the 64 MiB local maximum");
    }
    let working_directory = input
        .command
        .working_directory
        .canonicalize()
        .context("resolve producer working directory")?;
    let checkout = input
        .source
        .checkout
        .canonicalize()
        .context("resolve source checkout")?;
    if working_directory != checkout {
        bail!("producer working directory must be the registered source checkout");
    }
    for (name, value) in [
        ("job_id", &input.identities.job_id),
        ("producer_id", &input.identities.producer_id),
        ("runner_instance_id", &input.identities.runner_instance_id),
        ("reporter_id", &input.identities.reporter_id),
    ] {
        Uuid::parse_str(value).with_context(|| format!("{name} must be a UUID"))?;
    }
    for name in input.command.environment.keys() {
        let normalized = name.to_ascii_uppercase();
        if normalized.starts_with("AGENT_COORDINATOR_") || normalized.starts_with("COORDINATOR_") {
            bail!("coordinator credential and session environment may not enter a local job");
        }
    }
    if let Some(harness) = &input.harness
        && !process::is_alive(harness)?
    {
        bail!("the supplied harness process identity is not currently alive");
    }
    Ok(())
}

fn validate_reporter_token(reporter_id: &str, token: &str) -> Result<()> {
    let prefix = format!("acr_{reporter_id}.");
    if !token.starts_with(&prefix) || token.len() <= prefix.len() {
        bail!("reporter bearer token does not match the prepared reporter identity");
    }
    Ok(())
}

fn git_output(checkout: &Path, arguments: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(checkout)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .context("run Git to verify the local job source")?;
    if !output.status.success() {
        bail!("Git could not verify the local job source snapshot");
    }
    String::from_utf8(output.stdout)
        .context("Git returned a non-UTF-8 source identity")
        .map(|value| value.trim().to_owned())
}

fn inputs_unchanged(snapshot: &SourceSnapshot) -> bool {
    verify_clean_snapshot(snapshot).is_ok()
}

fn prepare_logs(paths: &persist::JobPaths, limit: u64) -> Result<()> {
    if limit == 0 {
        return Ok(());
    }
    for path in [&paths.stdout, &paths.stderr] {
        persist::reset_log(path)?;
    }
    Ok(())
}

fn spawn_log_drain<R>(mut source: R, path: PathBuf, limit: u64) -> std::sync::mpsc::Receiver<()>
where
    R: Read + Send + 'static,
{
    let (finished_tx, finished_rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let Ok(mut destination) = persist::protected_log(&path) else {
            let _ = std::io::copy(&mut source, &mut std::io::sink());
            let _ = finished_tx.send(());
            return;
        };
        let mut written = 0_u64;
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            let Ok(count) = source.read(&mut buffer) else {
                let _ = finished_tx.send(());
                return;
            };
            if count == 0 {
                let _ = destination.sync_all();
                let _ = finished_tx.send(());
                return;
            }
            let remaining = limit.saturating_sub(written) as usize;
            let keep = count.min(remaining);
            if keep > 0 && destination.write_all(&buffer[..keep]).is_err() {
                written = limit;
            } else {
                written = written.saturating_add(keep as u64);
            }
        }
    });
    finished_rx
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn seconds_ms(seconds: u64) -> i64 {
    seconds.saturating_mul(1000).min(i64::MAX as u64) as i64
}

fn renewal_until_ms(config: &RenewalConfig) -> Result<i64> {
    DateTime::parse_from_rfc3339(&config.renew_until)
        .context("renew_until must be an RFC3339 timestamp")
        .map(|value| value.timestamp_millis())
}

#[cfg(unix)]
fn platform_interrupted_exit_code() -> i32 {
    128
}

#[cfg(windows)]
fn platform_interrupted_exit_code() -> i32 {
    -1
}

#[cfg(test)]
mod tests;

#[cfg(not(any(unix, windows)))]
fn platform_interrupted_exit_code() -> i32 {
    -1
}
