//! The Windows host runtime: the launch boundary of the containment
//! prototype (`deploy/agentc/windows-probe-native.py`), reproduced layer by
//! layer.
//!
//! 1. **Role identity.** A launch runs as the account the launcher runs as.
//!    That must be a dedicated role account: this module does not switch
//!    accounts, and it records which account it ran as.
//! 2. **Package identity** ([`profile`]). A disposable AppContainer profile
//!    per launch; a less-privileged token; no network capability.
//! 3. **Job Object** ([`job`]). Assigned atomically at creation; kills on
//!    close, forbids breakaway, bounds processes and memory, restricts the
//!    user interface.
//! 4. **Private desktop** ([`desktop`]) in a window station the launcher
//!    made; a station it did not make is refused.
//! 5. **Explicit inheritance** ([`process`]). An exact environment and an
//!    explicit handle list.
//!
//! [`process::start`] creates the process suspended and resumes it only
//! after Windows confirms the job, the AppContainer flag, the package SID
//! and the less-privileged opt-out. [`reader`] reads worker-written files by
//! handle. [`runner`] serves all of it to a driver as typed requests and
//! journals each launch before it starts ([`journal`], [`recovery`]).
//!
//! [`runner_service`] hosts the runner as a Windows service: [`scm`] binds it
//! to the Service Control Manager, [`pipe`] serves it over a named pipe that
//! admits one configured client, and the platform-neutral
//! [`crate::service`] decides its life.
//!
//! [`WindowsHost`] is the [`HostRuntime`] over one [`launcher::Launcher`].
//! No provider harness is supported on Windows yet, so what a launch runs
//! comes from a [`Planner`] the caller supplies; nothing in this crate
//! builds a harness launch for it.
pub mod desktop;
pub mod job;
pub mod journal;
pub mod launcher;
pub mod pipe;
pub mod pipe_client;
pub mod process;
pub mod profile;
pub mod reader;
pub mod recovery;
pub mod runner;
pub mod runner_service;
pub mod scm;
pub mod sddl;
pub mod sys;
pub mod text;
pub mod token;

use super::{HostRuntime, LaunchIdentity, Observation, Recovery, StartIntent};
use crate::run_loop::Launch;
use crate::run_loop::record::LaunchRecord;
use anyhow::{Context, Result};
use launcher::{LaunchSpec, Launcher, Seen};
use process::Command;
use std::collections::BTreeMap;
use std::time::Duration;
use uuid::Uuid;

/// Decides what a launch runs and inside which boundary. The same launch
/// must always get the same plan, so a restarted host finds its directories.
pub trait Planner {
    /// The boundary and the command for `launch`.
    fn plan(&self, launch: &Launch) -> Result<(LaunchSpec, Command)>;
}

/// Runs launches on Windows through one [`Launcher`].
pub struct WindowsHost<P: Planner> {
    launcher: Launcher,
    planner: P,
    /// The command of each prepared launch, kept for its start.
    planned: BTreeMap<Uuid, Command>,
}

impl<P: Planner> WindowsHost<P> {
    /// A runtime for the calling account whose launches follow `planner`.
    pub fn new(planner: P) -> Result<Self> {
        Ok(Self {
            launcher: Launcher::new()?,
            planner,
            planned: BTreeMap::new(),
        })
    }

    /// The account every launch runs as.
    pub fn account_sid(&self) -> &str {
        self.launcher.owner()
    }
}

impl<P: Planner> HostRuntime for WindowsHost<P> {
    /// Creates the launch's profile, directories and desktop, and says
    /// which package, job owner and executable identify it. A process is
    /// told apart for good by its pid and creation time, so there is no
    /// boot id.
    fn prepare(&mut self, launch: &Launch) -> Result<StartIntent> {
        let (spec, command) = self.planner.plan(launch)?;
        let id = launch.session_id.to_string();
        let prepared = self.launcher.prepare(&id, spec)?;
        let executable = command.argv.first().cloned();
        self.planned.insert(launch.session_id, command);
        Ok(StartIntent {
            boot_id: String::new(),
            source: None,
            executable,
            container: Some(prepared.package_sid),
            job: Some(prepared.job_owner),
        })
    }

    /// Starts the planned command inside the whole boundary, or fails.
    fn start(&mut self, launch: &Launch) -> Result<LaunchIdentity> {
        let command = self.planned.get(&launch.session_id);
        let command = command.context("the launch was not prepared")?;
        let id = launch.session_id.to_string();
        let started = self.launcher.start(&id, command, &[])?;
        Ok(LaunchIdentity {
            pid: started.pid,
            started: Some(started.created),
        })
    }

    /// The root process's exit, once Windows reports it. The exit code is
    /// the 32-bit value reinterpreted, so an NTSTATUS shows as negative.
    fn observe(&mut self, launch: &Launch) -> Observation {
        let id = launch.session_id.to_string();
        match self.launcher.observe(&id, Duration::ZERO) {
            Ok(Seen::Exited(code)) => Observation::Exited { code: code as i32 },
            Ok(Seen::Running) => Observation::Running,
            Err(error) => {
                eprintln!("agentc-supervisor run: observe the launch: {error:#}");
                Observation::Running
            }
        }
    }

    /// Puts the stop marker in the launch's control directory.
    fn request_stop(&mut self, launch: &Launch) {
        let id = launch.session_id.to_string();
        if let Err(error) = self.launcher.request_stop(&id) {
            eprintln!("agentc-supervisor run: ask the launch to stop: {error:#}");
        }
    }

    /// Ends every process in the launch's job.
    fn terminate(&mut self, launch: &Launch) {
        let id = launch.session_id.to_string();
        match self.launcher.terminate(&id) {
            Ok(true) => {}
            Ok(false) => eprintln!("agentc-supervisor run: the launch's job did not empty"),
            Err(error) => eprintln!("agentc-supervisor run: terminate the launch: {error:#}"),
        }
    }

    /// A launch this runtime holds may run. Any other is judged from its
    /// saved root process and job owner ([`recovery::judge`]).
    fn recover(&self, record: &LaunchRecord, _now_ms: i64) -> Recovery {
        if self
            .launcher
            .held()
            .contains(&record.session_id.to_string())
        {
            return Recovery::MayBeRunning;
        }
        let root = record.pid.map(|pid| (pid, record.start_ticks));
        recovery::judge(root, record.job.as_deref(), recovery::lookup)
    }

    /// Ends the launch, deletes its desktop and profile, and removes its
    /// two directories. A launch of an earlier runtime has only its
    /// directories left to remove here.
    fn cleanup(&mut self, launch: &Launch) -> Result<()> {
        let id = launch.session_id.to_string();
        if self.launcher.held().contains(&id) {
            self.launcher.cleanup(&id)?;
        }
        self.planned.remove(&launch.session_id);
        let (spec, _) = self.planner.plan(launch)?;
        for directory in [&spec.control, &spec.work] {
            match std::fs::remove_dir_all(directory) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    Err(error).with_context(|| format!("remove {}", directory.display()))?;
                }
                _ => {}
            }
        }
        Ok(())
    }
}
