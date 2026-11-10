//! The supervisor as a Windows service.
//!
//! The service validates its settings, makes its state directory, sends
//! its diagnostics to `service.log` there, reports running and then runs
//! the live loop ([`crate::run_loop::run`]) over a
//! [`LoopDriver`](crate::service::supervisor::LoopDriver) until a stop or
//! preshutdown request ends it. The loop's own drain and recovery do the
//! rest; while it drains, the service keeps reporting a rising checkpoint.
//!
//! It holds no launch. With the acceptance stand-in enabled, launches are
//! requests to the role runner service over its pipe
//! ([`super::remote::RemoteHost`]) and run the configured fixture command.
//! Without it the service claims nothing, because no provider harness is
//! supported on Windows; it still writes its heartbeat, and it never opens
//! the runner's pipe.
use super::job::Limits;
use super::launcher::LaunchSpec;
use super::process::Command;
use super::remote::{PipeLink, RemoteHost};
use super::scm::{self, Host};
use super::sys::check;
use super::text::wide;
use crate::config::Config;
use crate::host_runtime::Refusing;
use crate::run_loop::Launch;
use crate::service::lifecycle::Report;
use crate::service::stand_in::{Ledger, Recorded, StandIn, StandInSettings};
use crate::service::supervisor::{LoopDriver, NO_HARNESS, NoHarness, Pace, SupervisorSettings};
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

/// How long Windows may wait for the service to report that it started.
const START_HINT: Duration = Duration::from_secs(30);
/// The bounds of a stand-in launch's job.
const FIXTURE_LIMITS: Limits = Limits {
    processes: 16,
    process_memory_mb: 512,
    job_memory_mb: 1024,
};

/// Runs the supervisor as the service `[windows_supervisor] name`. From a
/// console this fails at once saying it must be started by the service
/// manager.
pub fn run(config: Config) -> Result<()> {
    let name = config.windows_supervisor.name.clone();
    scm::dispatch(&name, move |host| serve(&config, host))
}

/// The service's whole life. It returns true: the loop returns only after
/// it released what it held, and the launches belong to the runner.
fn serve(config: &Config, host: &Host) -> Result<bool> {
    host.starting(1, START_HINT)?;
    let settings = config.windows_supervisor.clone();
    settings.validate()?;
    let state = &settings.state_dir;
    std::fs::create_dir_all(state).with_context(|| format!("create {}", state.display()))?;
    scm::errors_to(&state.join("service.log"))?;
    let config = loop_config(config, &settings);
    host.report(&Report::Running)?;
    eprintln!("agentc-supervisor windows-service: supervisor running");
    let pace = ServicePace::new(host, &settings);
    if settings.stand_in.enabled {
        with_stand_in(&config, &settings, host, pace)?;
    } else {
        without_harness(&config, host, pace)?;
    }
    eprintln!("agentc-supervisor windows-service: supervisor stopped");
    Ok(true)
}

/// The loop's configuration: this service's state directory and, for the
/// stand-in, no hold on launches that end without a submission, since a
/// stand-in launch never submits.
fn loop_config(config: &Config, settings: &SupervisorSettings) -> Config {
    let mut config = Config {
        state_dir: settings.state_dir.clone(),
        ..config.clone()
    };
    if settings.stand_in.enabled {
        (
            config.run.empty_exit_limit,
            config.run.empty_exit_total_limit,
        ) = (0, 0);
    }
    config
}

/// Runs the loop with work from the stand-in and launches at the runner.
fn with_stand_in(
    config: &Config,
    settings: &SupervisorSettings,
    host: &Host,
    pace: ServicePace<'_>,
) -> Result<()> {
    let stand_in = &settings.stand_in;
    let ledger = Ledger::at(&stand_in.ledger);
    let link = PipeLink::new(&settings.runner_pipe, &settings.runner_sid);
    let remote = RemoteHost::new(Fixture(stand_in.clone()), link);
    let recorded = Recorded::new(remote, ledger.clone());
    let work = StandIn::new(&stand_in.queue_dir, ledger);
    let mut driver = LoopDriver::new(recorded, work, pace, host.shared_signal());
    crate::run_loop::run(&mut driver, config, false)
}

/// Runs the loop with no work: every poll is refused and the heartbeat
/// says why.
fn without_harness(config: &Config, host: &Host, pace: ServicePace<'_>) -> Result<()> {
    let refusing = Refusing::new(NO_HARNESS);
    let mut driver = LoopDriver::new(refusing, NoHarness, pace, host.shared_signal());
    crate::run_loop::run(&mut driver, config, false)
}

/// Plans every stand-in launch: the configured fixture command inside a
/// less-privileged boundary with no capability, in two directories of its
/// own below the launch root.
pub struct Fixture(pub StandInSettings);

impl Fixture {
    /// The launch's directory called `kind`.
    fn directory(&self, launch: &Launch, kind: &str) -> PathBuf {
        let name = format!("{}-{kind}", launch.session_id);
        self.0.launch_root.join(name)
    }
}

impl super::Planner for Fixture {
    /// The same plan for the same launch, so a restarted supervisor names
    /// the same directories.
    fn plan(&self, launch: &Launch) -> Result<(LaunchSpec, Command)> {
        let work = self.directory(launch, "work");
        let spec = LaunchSpec {
            profile: format!("agentc.standin.{}", launch.session_id.simple()),
            less_privileged: true,
            capabilities: Vec::new(),
            limits: FIXTURE_LIMITS,
            control: self.directory(launch, "control"),
            work: work.clone(),
        };
        let command = Command {
            argv: self.0.fixture.clone(),
            directory: work.clone(),
            environment: environment(),
            output: work.join("output.log"),
        };
        Ok((spec, command))
    }
}

/// The exact environment of a stand-in launch: where Windows is, and
/// nothing of the supervisor's own.
fn environment() -> BTreeMap<String, String> {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_owned());
    BTreeMap::from([("SystemRoot".to_owned(), root)])
}

/// The service's clock, waits and disk. While the service is stopping,
/// each wait first reports a rising checkpoint if a report is due.
struct ServicePace<'a> {
    host: &'a Host,
    state: PathBuf,
    interval: Duration,
    checkpoint: u32,
    reported: Option<Instant>,
}

impl<'a> ServicePace<'a> {
    fn new(host: &'a Host, settings: &SupervisorSettings) -> Self {
        Self {
            host,
            state: settings.state_dir.clone(),
            interval: Duration::from_secs(settings.status_interval_seconds),
            checkpoint: 0,
            reported: None,
        }
    }

    /// Tells Windows the service is still stopping, at most once per
    /// interval, allowing twice the interval until the next report.
    fn report_stopping(&mut self) {
        if (self.reported).is_some_and(|at| at.elapsed() < self.interval) {
            return;
        }
        (self.reported, self.checkpoint) = (Some(Instant::now()), self.checkpoint + 1);
        let report = Report::Stopping {
            checkpoint: self.checkpoint,
            wait_hint: self.interval * 2,
        };
        if let Err(error) = self.host.report(&report) {
            eprintln!("agentc-supervisor windows-service: report progress: {error:#}");
        }
    }
}

impl Pace for ServicePace<'_> {
    fn now_ms(&self) -> i64 {
        i64::try_from(crate::shadow::now_ms()).unwrap_or(i64::MAX)
    }

    fn pause(&mut self, duration: Duration, stopping: bool) {
        if stopping {
            self.report_stopping();
        }
        std::thread::sleep(duration);
    }

    fn free_bytes(&self) -> Result<u64> {
        free_bytes(&self.state)
    }
}

/// Free bytes available to this account on the volume holding `directory`.
pub fn free_bytes(directory: &Path) -> Result<u64> {
    let path = wide(&directory.display().to_string())?;
    let mut free = 0_u64;
    // SAFETY: the path is NUL-terminated and `free` is writable; the two
    // totals are not asked for.
    let read = unsafe {
        let none = std::ptr::null_mut();
        GetDiskFreeSpaceExW(path.as_ptr(), &mut free, none, none)
    };
    check(read, "GetDiskFreeSpaceExW")?;
    Ok(free)
}

#[cfg(test)]
mod tests {
    use super::super::Planner;
    use super::*;
    use crate::run_loop::Suggestion;

    fn launch() -> Launch {
        let suggestion = Suggestion {
            task: "item-a".into(),
            revision: 1,
            title: String::new(),
        };
        Launch::plan(&Config::default(), "local", suggestion)
    }

    #[test]
    fn a_stand_in_launch_runs_the_fixture_in_its_own_directories_with_no_capability() {
        let settings = StandInSettings {
            fixture: vec!["fixture.exe".into(), "run".into()],
            launch_root: PathBuf::from(r"C:\launches"),
            ..StandInSettings::default()
        };
        let (fixture, launch) = (Fixture(settings), launch());
        let (spec, command) = fixture.plan(&launch).unwrap();
        assert!(spec.less_privileged && spec.capabilities.is_empty());
        let id = launch.session_id.to_string();
        assert_eq!(
            spec.work,
            Path::new(r"C:\launches").join(format!("{id}-work"))
        );
        assert_eq!(
            spec.control,
            Path::new(r"C:\launches").join(format!("{id}-control"))
        );
        assert_ne!(spec.work, spec.control);
        assert_eq!(command.argv, ["fixture.exe", "run"]);
        assert_eq!(command.directory, spec.work);
        let names: Vec<&String> = command.environment.keys().collect();
        assert_eq!(names, ["SystemRoot"]);
        // The same launch always gets the same plan.
        assert_eq!(fixture.plan(&launch).unwrap().0, spec);
        let other = fixture.plan(&self::launch()).unwrap().0;
        assert_ne!((other.profile, other.work), (spec.profile, spec.work));
    }

    #[test]
    fn the_loop_gets_the_services_state_directory_and_the_stand_in_no_empty_exit_hold() {
        let mut settings = SupervisorSettings {
            state_dir: PathBuf::from(r"C:\state"),
            ..SupervisorSettings::default()
        };
        let plain = loop_config(&Config::default(), &settings);
        assert_eq!(plain.state_dir, Path::new(r"C:\state"));
        assert_eq!(plain.run, Config::default().run);
        settings.stand_in.enabled = true;
        let stand_in = loop_config(&Config::default(), &settings);
        let limits = (
            stand_in.run.empty_exit_limit,
            stand_in.run.empty_exit_total_limit,
        );
        assert_eq!(limits, (0, 0));
    }

    #[test]
    fn free_space_is_read_for_a_directory_that_exists_and_refused_for_one_that_does_not() {
        let dir = tempfile::tempdir().unwrap();
        assert!(free_bytes(dir.path()).unwrap() > 0);
        assert!(free_bytes(&dir.path().join("missing")).is_err());
    }
}
