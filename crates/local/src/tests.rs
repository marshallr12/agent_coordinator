use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;

struct MockService {
    origin: String,
    identities: Arc<Mutex<Option<(JobIdentities, String, String)>>>,
    requests: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for MockService {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockService {
    async fn start(delay_from_request: Option<usize>) -> Result<Self> {
        Self::start_with_statuses(delay_from_request, None, None).await
    }

    async fn start_with_renew_status(
        delay_from_request: Option<usize>,
        renew_status: Option<u16>,
    ) -> Result<Self> {
        Self::start_with_statuses(delay_from_request, renew_status, None).await
    }

    async fn start_with_statuses(
        delay_from_request: Option<usize>,
        renew_status: Option<u16>,
        observation_status: Option<u16>,
    ) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let requests = Arc::new(AtomicUsize::new(0));
        let request_counter = requests.clone();
        let identities = Arc::new(Mutex::new(None::<(JobIdentities, String, String)>));
        let response_identities = identities.clone();
        let request_lines = Arc::new(Mutex::new(Vec::new()));
        let recorded_lines = request_lines.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let sequence = request_counter.fetch_add(1, Ordering::SeqCst) + 1;
                let response_identities = response_identities.clone();
                let recorded_lines = recorded_lines.clone();
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    let mut buffer = [0_u8; 4096];
                    loop {
                        let Ok(count) = stream.read(&mut buffer).await else {
                            return;
                        };
                        if count == 0 {
                            return;
                        }
                        bytes.extend_from_slice(&buffer[..count]);
                        let Some(header_end) =
                            bytes.windows(4).position(|part| part == b"\r\n\r\n")
                        else {
                            continue;
                        };
                        let headers = String::from_utf8_lossy(&bytes[..header_end]);
                        let content_length = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        if bytes.len() >= header_end + 4 + content_length {
                            break;
                        }
                    }
                    if delay_from_request.is_some_and(|minimum| sequence >= minimum) {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                    let first_line = String::from_utf8_lossy(&bytes)
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .to_owned();
                    recorded_lines.lock().unwrap().push(first_line.clone());
                    let status = if first_line.contains("/renew ") {
                        renew_status.unwrap_or(200)
                    } else if first_line.contains("/observations ") {
                        observation_status.unwrap_or(200)
                    } else {
                        200
                    };
                    let data = if first_line.starts_with("GET ") {
                        let (identities, source_revision, source_tree) =
                            response_identities.lock().unwrap().clone().unwrap();
                        json!({
                            "job": {
                                "id": identities.job_id,
                                "producer_id": identities.producer_id,
                                "runner_instance_id": identities.runner_instance_id,
                                "source_revision": source_revision,
                                "source_tree": source_tree
                            },
                            "reporter": {
                                "id": identities.reporter_id,
                                "launch_allowed": true,
                                "lease_remaining_ms": 60_000
                            }
                        })
                    } else {
                        json!({})
                    };
                    let body = if (200..300).contains(&status) {
                        json!({
                            "data": data,
                            "request_id": Uuid::new_v4().to_string(),
                            "server_time": "2026-09-09T12:00:00Z"
                        })
                    } else {
                        json!({
                            "error": {"code":"renewal_denied", "message":"renewal denied"},
                            "request_id": Uuid::new_v4().to_string(),
                            "server_time": "2026-09-09T12:00:00Z"
                        })
                    }
                    .to_string();
                    let reason = match status {
                        200 => "OK",
                        409 => "Conflict",
                        _ => "Forbidden",
                    };
                    let response = format!(
                        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        Ok(Self {
            origin: format!("http://{address}"),
            identities,
            requests: request_lines,
            task,
        })
    }

    fn bind(&self, identities: &JobIdentities, source: &SourceSnapshot) {
        *self.identities.lock().unwrap() = Some((
            identities.clone(),
            source.revision.clone(),
            source.tree.clone(),
        ));
    }

    fn request_count(&self, fragment: &str) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|line| line.contains(fragment))
            .count()
    }

    fn replace_job_identity(&self) {
        let mut binding = self.identities.lock().unwrap();
        let (identities, _, _) = binding.as_mut().unwrap();
        identities.job_id = Uuid::new_v4().to_string();
    }
}

struct Fixture {
    _temp: TempDir,
    state_file: PathBuf,
    marker: PathBuf,
}

impl Fixture {
    fn new(
        service: &MockService,
        sleep_ms: u64,
        output_bytes: usize,
        log_limit: u64,
    ) -> Result<Self> {
        Self::new_with_renewal(service, sleep_ms, output_bytes, log_limit, None, None)
    }

    fn new_with_renewal(
        service: &MockService,
        sleep_ms: u64,
        output_bytes: usize,
        log_limit: u64,
        harness: Option<ProcessIdentity>,
        renewal: Option<RenewalConfig>,
    ) -> Result<Self> {
        let temp = tempfile::Builder::new()
            .prefix("coordinator local spaces ")
            .tempdir()?;
        let checkout = temp.path().join("checkout with spaces");
        fs::create_dir_all(&checkout)?;
        fs::write(checkout.join("tracked.txt"), "committed\n")?;
        git(&checkout, &["init", "-q"])?;
        git(
            &checkout,
            &["config", "user.email", "local@example.invalid"],
        )?;
        git(&checkout, &["config", "user.name", "Local Test"])?;
        git(&checkout, &["add", "."])?;
        git(&checkout, &["commit", "-q", "-m", "fixture"])?;
        let source = SourceSnapshot {
            checkout: checkout.clone(),
            revision: git(&checkout, &["rev-parse", "HEAD^{commit}"])?,
            tree: git(&checkout, &["rev-parse", "HEAD^{tree}"])?,
        };
        let identities = JobIdentities {
            job_id: Uuid::new_v4().to_string(),
            producer_id: Uuid::new_v4().to_string(),
            runner_instance_id: Uuid::new_v4().to_string(),
            reporter_id: Uuid::new_v4().to_string(),
        };
        service.bind(&identities, &source);
        let marker = temp.path().join("producer started.txt");
        let state_file = temp.path().join("job state with spaces").join("state.json");
        let mut environment = BTreeMap::new();
        environment.insert(
            "LOCAL_GUARDIAN_TEST_MARKER".into(),
            marker.display().to_string(),
        );
        environment.insert("LOCAL_GUARDIAN_TEST_SLEEP_MS".into(), sleep_ms.to_string());
        environment.insert(
            "LOCAL_GUARDIAN_TEST_OUTPUT_BYTES".into(),
            output_bytes.to_string(),
        );
        let executable = std::env::current_exe()?.canonicalize()?;
        initialize_launch_state(InitializeJob {
            state_file: state_file.clone(),
            identities: identities.clone(),
            command: CommandSpec {
                program: executable,
                args: vec![
                    "--exact".into(),
                    "tests::producer_helper".into(),
                    "--nocapture".into(),
                ],
                working_directory: checkout,
                environment,
            },
            source: source.clone(),
            log_limit_bytes: log_limit,
            harness,
        })?;
        record_registration(
            &state_file,
            ReporterRegistration::new(
                &service.origin,
                true,
                format!("acr_{}.{}", identities.reporter_id, "a".repeat(64)),
                renewal,
            )?,
        )?;
        Ok(Self {
            _temp: temp,
            state_file,
            marker,
        })
    }
}

async fn wait_for_fixture_start(
    fixture: &Fixture,
    guardian: &mut tokio::task::JoinHandle<Result<GuardianOutcome>>,
) -> Result<()> {
    #[cfg(windows)]
    let deadline = Duration::from_secs(15);
    #[cfg(not(windows))]
    let deadline = Duration::from_secs(5);
    if tokio::time::timeout(deadline, wait_for_path(&fixture.marker))
        .await
        .is_ok()
    {
        return Ok(());
    }
    let summary = inspect_job(&fixture.state_file)?;
    let guardian_result = if guardian.is_finished() {
        format!("{:?}", guardian.await)
    } else {
        "still running".into()
    };
    bail!(
        "producer did not start; phase={:?}, local_summary={:?}, last_report_error={:?}, guardian={guardian_result}",
        summary.phase,
        summary.last_local_summary,
        summary.last_report_error
    )
}

fn git(directory: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(directory)
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        bail!("test Git command failed");
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn spawn_test_process(marker: &Path, sleep_ms: u64) -> Result<std::process::Child> {
    let mut command = Command::new(std::env::current_exe()?.canonicalize()?);
    command
        .args(["--exact", "tests::producer_helper", "--nocapture"])
        .env_clear()
        .env("LOCAL_GUARDIAN_TEST_MARKER", marker)
        .env("LOCAL_GUARDIAN_TEST_SLEEP_MS", sleep_ms.to_string())
        .env("LOCAL_GUARDIAN_TEST_OUTPUT_BYTES", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Ok(command.spawn()?)
}

async fn wait_for_path(path: &Path) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if path.exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .with_context(|| format!("{} was not created", path.display()))?;
    Ok(())
}

#[test]
fn producer_helper() {
    let Some(marker) = std::env::var_os("LOCAL_GUARDIAN_TEST_MARKER") else {
        return;
    };
    let mut marker_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(marker)
        .unwrap();
    marker_file.write_all(b"started\n").unwrap();
    marker_file.sync_all().unwrap();
    let output_bytes = std::env::var("LOCAL_GUARDIAN_TEST_OUTPUT_BYTES")
        .unwrap()
        .parse::<usize>()
        .unwrap();
    let chunk = vec![b'x'; output_bytes];
    std::io::stdout().write_all(&chunk).unwrap();
    std::io::stderr().write_all(&chunk).unwrap();
    let sleep_ms = std::env::var("LOCAL_GUARDIAN_TEST_SLEEP_MS")
        .unwrap()
        .parse::<u64>()
        .unwrap();
    std::thread::sleep(Duration::from_millis(sleep_ms));
}

#[test]
fn windows_git_paths_remove_supported_verbatim_prefixes() {
    fn normalize(value: &str) -> Option<String> {
        let encoded = value.encode_utf16().collect::<Vec<_>>();
        normalize_windows_git_path_wide(&encoded)
            .map(|path| String::from_utf16(&path).expect("test path remains UTF-16"))
    }

    assert_eq!(
        normalize(r"\\?\C:\checkout with spaces\数据"),
        Some(r"C:\checkout with spaces\数据".into())
    );
    assert_eq!(
        normalize(r"\\?\unc\server\share\checkout with spaces"),
        Some(r"\\server\share\checkout with spaces".into())
    );
    assert_eq!(
        normalize(r"C:\checkout with spaces"),
        Some(r"C:\checkout with spaces".into())
    );
    assert_eq!(normalize(r"\\?\Volume{identity}\checkout"), None);
    assert_eq!(normalize(r"\\.\PIPE\coordinator"), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_run_does_not_spawn_twice_and_inspect_is_nonblocking() -> Result<()> {
    let service = MockService::start(None).await?;
    let fixture = Fixture::new(&service, 1_000, 32, 4_096)?;
    let state_file = fixture.state_file.clone();
    let mut guardian =
        tokio::spawn(async move { run_guardian(&state_file, GuardianMode::Run).await });
    wait_for_fixture_start(&fixture, &mut guardian).await?;

    let inspect_path = fixture.state_file.clone();
    let inspection = tokio::time::timeout(
        Duration::from_millis(500),
        tokio::task::spawn_blocking(move || inspect_job(&inspect_path)),
    )
    .await???;
    assert_eq!(inspection.phase, JobPhase::Running);
    assert_eq!(
        run_guardian(&fixture.state_file, GuardianMode::Run).await?,
        GuardianOutcome::AlreadyActive
    );
    assert_eq!(guardian.await??, GuardianOutcome::Completed);
    assert_eq!(fs::read_to_string(&fixture.marker)?.lines().count(), 1);
    let final_state = inspect_job(&fixture.state_file)?;
    assert_eq!(final_state.phase, JobPhase::Succeeded);
    assert_eq!(final_state.inputs_unchanged, Some(true));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn producer_outlives_observer_and_reconnect_preserves_unknown_without_relaunch() -> Result<()>
{
    let service = MockService::start(None).await?;
    let fixture = Fixture::new(&service, 1_200, 0, 0)?;
    let state_file = fixture.state_file.clone();
    let mut guardian =
        tokio::spawn(async move { run_guardian(&state_file, GuardianMode::Run).await });
    wait_for_fixture_start(&fixture, &mut guardian).await?;
    guardian.abort();
    let _ = guardian.await;
    assert_eq!(inspect_job(&fixture.state_file)?.phase, JobPhase::Running);
    tokio::time::sleep(Duration::from_millis(1_300)).await;
    assert_eq!(
        run_guardian(&fixture.state_file, GuardianMode::Observe).await?,
        GuardianOutcome::ObservedUnknown
    );
    assert_eq!(inspect_job(&fixture.state_file)?.phase, JobPhase::Unknown);
    assert_eq!(
        run_guardian(&fixture.state_file, GuardianMode::Run).await?,
        GuardianOutcome::ObservedUnknown
    );
    assert_eq!(fs::read_to_string(&fixture.marker)?.lines().count(), 1);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn short_producer_logs_are_drained_and_strictly_bounded_during_slow_reporting() -> Result<()>
{
    let service = MockService::start(Some(3)).await?;
    let fixture = Fixture::new(&service, 0, 1_000_000, 1_024)?;
    let state_file = fixture.state_file.clone();
    let mut guardian =
        tokio::spawn(async move { run_guardian(&state_file, GuardianMode::Run).await });
    wait_for_fixture_start(&fixture, &mut guardian).await?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let summary = inspect_job(&fixture.state_file)?;
    assert!(fs::metadata(&summary.stdout_log)?.len() <= 1_024);
    assert!(fs::metadata(&summary.stderr_log)?.len() <= 1_024);
    assert_eq!(guardian.await??, GuardianOutcome::Completed);
    assert_eq!(fs::read(&summary.stdout_log)?.len(), 1_024);
    assert_eq!(fs::read(&summary.stderr_log)?.len(), 1_024);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn observe_after_durable_launch_intent_never_launches() -> Result<()> {
    let service = MockService::start(None).await?;
    let fixture = Fixture::new(&service, 0, 0, 0)?;
    let paths = persist::paths(&fixture.state_file)?;
    {
        let _lock = persist::lock(&paths, true)?.unwrap();
        let mut state = recover(&paths)?;
        state.phase = JobPhase::LaunchIntent;
        commit_event(&paths, &mut state, JournalKind::LaunchIntent)?;
    }
    assert_eq!(
        run_guardian(&fixture.state_file, GuardianMode::Observe).await?,
        GuardianOutcome::ObservedUnknown
    );
    assert!(!fixture.marker.exists());
    assert_eq!(
        run_guardian(&fixture.state_file, GuardianMode::Run).await?,
        GuardianOutcome::ObservedUnknown
    );
    assert!(!fixture.marker.exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_journal_replay_restores_exact_pending_observation() -> Result<()> {
    let service = MockService::start(None).await?;
    let fixture = Fixture::new(&service, 0, 0, 0)?;
    let paths = persist::paths(&fixture.state_file)?;
    let (key, sequence);
    {
        let _lock = persist::lock(&paths, true)?.unwrap();
        let mut state = recover(&paths)?;
        let evidence = TerminalEvidence {
            phase: JobPhase::Succeeded,
            exit_code: Some(0),
            inputs_unchanged: true,
            summary: "terminal before simulated publication failure".into(),
        };
        enqueue_observation(
            &mut state,
            JobPhase::Succeeded,
            None,
            Some(0),
            Some(true),
            evidence.summary.clone(),
        );
        let observation = state.pending_observations.last().unwrap().clone();
        key = observation.idempotency_key.clone();
        sequence = observation.body.sequence;
        persist::append_event(
            &paths,
            &JournalEvent {
                revision: state.revision + 1,
                at_ms: now_ms(),
                event: JournalKind::Terminal {
                    evidence,
                    observation,
                },
            },
        )?;
    }
    let recovered = recover_readonly(&paths)?;
    assert_eq!(recovered.phase, JobPhase::Succeeded);
    let pending = recovered
        .pending_observations
        .iter()
        .find(|pending| pending.idempotency_key == key)
        .context("journaled terminal observation was not restored")?;
    assert_eq!(pending.body.sequence, sequence);
    assert!(recovered.next_sequence > sequence);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn torn_journal_tail_is_repaired_before_a_launch_intent_is_appended() -> Result<()> {
    let service = MockService::start(None).await?;
    let fixture = Fixture::new(&service, 0, 0, 0)?;
    let paths = persist::paths(&fixture.state_file)?;
    {
        let mut journal = OpenOptions::new().append(true).open(&paths.journal)?;
        journal.write_all(b"{\"revision\":999")?;
        journal.sync_all()?;
    }
    {
        let _lock = persist::lock(&paths, true)?.unwrap();
        let state = recover(&paths)?;
        persist::append_event(
            &paths,
            &JournalEvent {
                revision: state.revision + 1,
                at_ms: now_ms(),
                event: JournalKind::LaunchIntent,
            },
        )?;
    }
    let recovered = recover_readonly(&paths)?;
    assert_eq!(recovered.phase, JobPhase::LaunchIntent);
    assert_eq!(
        run_guardian(&fixture.state_file, GuardianMode::Observe).await?,
        GuardianOutcome::ObservedUnknown
    );
    assert!(!fixture.marker.exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn harness_exit_stops_renewal_while_job_observations_continue() -> Result<()> {
    let service = MockService::start(None).await?;
    let harness_temp = tempfile::tempdir()?;
    let harness_marker = harness_temp.path().join("harness started");
    let mut harness = spawn_test_process(&harness_marker, 10_000)?;
    wait_for_path(&harness_marker).await?;
    let harness_identity = capture_process_identity(harness.id())?.context("harness identity")?;
    let fixture = Fixture::new_with_renewal(
        &service,
        2_800,
        0,
        0,
        Some(harness_identity),
        Some(RenewalConfig {
            generation: 7,
            renew_after_seconds: 1,
            renew_until: (chrono::Utc::now() + chrono::Duration::seconds(30)).to_rfc3339(),
        }),
    )?;
    let state_file = fixture.state_file.clone();
    let guardian = tokio::spawn(async move { run_guardian(&state_file, GuardianMode::Run).await });
    tokio::time::timeout(Duration::from_secs(4), async {
        while service.request_count("/renew ") == 0 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await?;
    harness.kill()?;
    harness.wait()?;
    let renewals_at_exit = service.request_count("/renew ");
    assert!(renewals_at_exit >= 1);
    assert_eq!(guardian.await??, GuardianOutcome::Completed);
    assert_eq!(service.request_count("/renew "), renewals_at_exit);
    assert!(service.request_count("/observations ") >= 3);
    assert_eq!(inspect_job(&fixture.state_file)?.phase, JobPhase::Succeeded);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn renewal_denial_does_not_disable_later_terminal_observation() -> Result<()> {
    let service = MockService::start_with_renew_status(None, Some(403)).await?;
    let harness = capture_process_identity(std::process::id())?.context("test harness identity")?;
    let fixture = Fixture::new_with_renewal(
        &service,
        2_200,
        0,
        0,
        Some(harness),
        Some(RenewalConfig {
            generation: 8,
            renew_after_seconds: 1,
            renew_until: (chrono::Utc::now() + chrono::Duration::seconds(30)).to_rfc3339(),
        }),
    )?;
    assert_eq!(
        run_guardian(&fixture.state_file, GuardianMode::Run).await?,
        GuardianOutcome::Completed
    );
    assert_eq!(service.request_count("/renew "), 1);
    assert!(service.request_count("/observations ") >= 3);
    let summary = inspect_job(&fixture.state_file)?;
    assert_eq!(summary.phase, JobPhase::Succeeded);
    assert!(!summary.reporting_disabled);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_observation_conflict_does_not_starve_eligible_renewal() -> Result<()> {
    let service = MockService::start_with_statuses(None, None, Some(409)).await?;
    let harness = capture_process_identity(std::process::id())?.context("test harness identity")?;
    let fixture = Fixture::new_with_renewal(
        &service,
        1_600,
        0,
        0,
        Some(harness),
        Some(RenewalConfig {
            generation: 9,
            renew_after_seconds: 1,
            renew_until: (chrono::Utc::now() + chrono::Duration::seconds(30)).to_rfc3339(),
        }),
    )?;
    assert_eq!(
        run_guardian(&fixture.state_file, GuardianMode::Run).await?,
        GuardianOutcome::Completed
    );
    assert!(service.request_count("/renew ") >= 1);
    let summary = inspect_job(&fixture.state_file)?;
    assert_eq!(summary.phase, JobPhase::Succeeded);
    assert!(summary.pending_observations >= 2);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mismatched_launch_authority_identity_never_starts_producer() -> Result<()> {
    let service = MockService::start(None).await?;
    let fixture = Fixture::new(&service, 0, 0, 0)?;
    service.replace_job_identity();
    assert_eq!(
        run_guardian(&fixture.state_file, GuardianMode::Run).await?,
        GuardianOutcome::Completed
    );
    assert!(!fixture.marker.exists());
    assert_eq!(
        inspect_job(&fixture.state_file)?.phase,
        JobPhase::NotStarted
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn log_initialization_failure_is_durable_and_never_starts_producer() -> Result<()> {
    let service = MockService::start(None).await?;
    let fixture = Fixture::new(&service, 0, 0, 1_024)?;
    let initial = inspect_job(&fixture.state_file)?;
    fs::create_dir(&initial.stdout_log)?;
    assert!(
        run_guardian(&fixture.state_file, GuardianMode::Run)
            .await
            .is_err()
    );
    let summary = inspect_job(&fixture.state_file)?;
    assert_eq!(summary.phase, JobPhase::NotStarted);
    assert_eq!(
        summary.last_local_summary.as_deref(),
        Some("Protected local logs could not be initialized before launch.")
    );
    assert!(!fixture.marker.exists());
    Ok(())
}

#[test]
fn registration_journal_restores_reporter_and_exact_first_observation() -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let service = runtime.block_on(MockService::start(None))?;
    let fixture = Fixture::new(&service, 0, 0, 0)?;
    let paths = persist::paths(&fixture.state_file)?;
    let mut state = persist::load(&paths)?;
    let event = persist::events(&paths)?
        .into_iter()
        .find(|event| matches!(event.event, JournalKind::Registered { .. }))
        .context("registered journal event")?;
    state.revision = event.revision - 1;
    state.phase = JobPhase::Prepared;
    state.reporter = None;
    state.pending_observations.clear();
    state.next_sequence = 1;
    persist::save(&paths, &state)?;
    let recovered = recover_readonly(&paths)?;
    assert_eq!(recovered.phase, JobPhase::Registered);
    assert!(recovered.reporter.is_some());
    assert_eq!(recovered.pending_observations.len(), 1);
    assert_eq!(recovered.pending_observations[0].body.sequence, 1);
    Ok(())
}

#[test]
fn process_identity_rejects_a_changed_start_marker() -> Result<()> {
    let current = capture_process_identity(std::process::id())?.context("current process")?;
    assert!(process::is_alive(&current)?);
    let mut changed = current;
    changed.start_identity.push_str("-reused");
    assert!(!process::is_alive(&changed)?);
    Ok(())
}
