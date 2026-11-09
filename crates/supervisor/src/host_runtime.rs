//! The host-runtime interface: every operating-system operation the live
//! loop needs to run one launch, behind seven typed operations.
//!
//! [`crate::run_loop`] keeps selection, admission, leases, quiet hours, cost,
//! checkpoints and review orchestration; it reaches the host only through
//! [`HostRuntime`], by way of [`crate::run_loop::Driver::host`]. A backend
//! answers with typed identities and outcomes, never with a command string
//! for the loop to run. The loop persists what a backend returns in the
//! launch's [`LaunchRecord`] before it goes on, so a restart settles a launch
//! from the saved record and never starts it a second time.
//!
//! The Linux backend is [`linux::LinuxHost`]: role accounts, `launch-root`,
//! the candidate-push helper and the reaper, exactly as before the interface
//! existed. The Windows backend is [`windows::WindowsHost`]. A host without a
//! backend gets [`Refusing`], which starts nothing.
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
pub mod rooted;
#[cfg(windows)]
pub mod windows;

use crate::run_loop::Launch;
use crate::run_loop::record::LaunchRecord;
use anyhow::{Result, bail};

/// What a backend fixed for a launch before anything is claimed or started.
/// The loop writes it into the launch record ahead of the start, so the
/// record names the launch's identities even if the supervisor dies between
/// the two.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StartIntent {
    /// This boot's identity; empty when the host cannot tell boots apart.
    pub boot_id: String,
    /// The exact source revision the launch's tree was made from, where the
    /// backend records it.
    pub source: Option<String>,
    /// The exact executable the launch will run, where the backend records it.
    pub executable: Option<String>,
    /// The launch's container identity, where the host has one (on Windows
    /// the AppContainer package SID).
    pub container: Option<String>,
    /// The identity of the job that owns the launch's processes, where the
    /// host has one (on Windows the Job Object).
    pub job: Option<String>,
}

/// The operating system's identity for a started launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchIdentity {
    /// The launch's root process.
    pub pid: u32,
    /// When that process started, in the host's own unit (Linux: clock ticks
    /// since boot), so a reused pid is never taken for the launch.
    pub started: Option<u64>,
}

/// What a backend sees of the launch it started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observation {
    /// The launch has not been seen to end.
    Running,
    /// The launch ended with this exit code.
    Exited { code: i32 },
}

/// What a backend concludes about a launch recorded by an earlier supervisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    /// The launch may still run. It is never started again, and it keeps
    /// what it holds.
    MayBeRunning,
    /// The launch is known to have ended; its attempt can be released.
    Ended,
}

/// The operating-system side of one launch at a time.
pub trait HostRuntime {
    /// Makes the launch's source tree and run directory, owned by the role,
    /// and says what identifies the launch before it starts.
    fn prepare(&mut self, launch: &Launch) -> Result<StartIntent>;
    /// Starts the prepared launch inside the host's containment boundary.
    /// A backend that cannot establish the whole boundary fails here; it
    /// never starts the launch outside it.
    fn start(&mut self, launch: &Launch) -> Result<LaunchIdentity>;
    /// Whether the launch this runtime started has ended. A failure to look
    /// is [`Observation::Running`]: a lost observer proves nothing.
    fn observe(&mut self, launch: &Launch) -> Observation;
    /// Asks the launch to end by itself.
    fn request_stop(&mut self, launch: &Launch);
    /// Ends the launch and everything it started, without asking.
    fn terminate(&mut self, launch: &Launch);
    /// Judges a saved record at `now_ms` (milliseconds since the epoch)
    /// without starting anything.
    fn recover(&self, record: &LaunchRecord, now_ms: i64) -> Recovery;
    /// Removes the launch's source tree and run directory.
    fn cleanup(&mut self, launch: &Launch) -> Result<()>;
}

/// The runtime of a host with no backend: it prepares and starts nothing,
/// and it never calls a recorded launch ended, since it cannot look.
#[derive(Debug, Clone)]
pub struct Refusing {
    reason: String,
}

impl Refusing {
    /// A runtime that refuses every launch because of `reason`.
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl HostRuntime for Refusing {
    fn prepare(&mut self, _launch: &Launch) -> Result<StartIntent> {
        bail!("no launch is prepared: {}", self.reason)
    }

    fn start(&mut self, _launch: &Launch) -> Result<LaunchIdentity> {
        bail!("no launch is started: {}", self.reason)
    }

    /// Nothing was started, so there is nothing to see end.
    fn observe(&mut self, _launch: &Launch) -> Observation {
        Observation::Running
    }

    fn request_stop(&mut self, _launch: &Launch) {}

    fn terminate(&mut self, _launch: &Launch) {}

    /// A record on a host that cannot inspect its launch keeps its holds.
    fn recover(&self, _record: &LaunchRecord, _now_ms: i64) -> Recovery {
        Recovery::MayBeRunning
    }

    fn cleanup(&mut self, _launch: &Launch) -> Result<()> {
        bail!("nothing is removed: {}", self.reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::run_loop::Suggestion;
    use crate::run_loop::lease::Lease;

    /// A planned launch of task `t` in project `p`.
    fn launch() -> Launch {
        let suggestion = Suggestion {
            task: "t".into(),
            revision: 1,
            title: String::new(),
        };
        Launch::plan(&Config::default(), "p", suggestion)
    }

    #[test]
    fn a_host_without_a_backend_starts_nothing_and_says_why() {
        let (mut host, launch) = (Refusing::new("no Windows backend"), launch());
        let refused = host.prepare(&launch).unwrap_err().to_string();
        assert!(refused.contains("no Windows backend"), "{refused}");
        assert!(host.start(&launch).is_err());
        assert!(host.cleanup(&launch).is_err());
        assert_eq!(host.observe(&launch), Observation::Running);
    }

    #[test]
    fn a_host_without_a_backend_never_calls_a_recorded_launch_ended() {
        let (host, launch) = (Refusing::new("no backend"), launch());
        let lease = Lease {
            attempt: "a1".into(),
            generation: 1,
            renew_after_seconds: 60,
            progress_age_ms: 0,
        };
        let intent = StartIntent::default();
        let record = LaunchRecord::new(&launch, &lease, &intent, 0);
        assert_eq!(host.recover(&record, i64::MAX), Recovery::MayBeRunning);
    }
}
