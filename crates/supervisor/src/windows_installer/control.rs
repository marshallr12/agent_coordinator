//! Starting and stopping the four services (phase 7, step 7b), and what
//! counts as a service that started: Windows reports it running, its log
//! gained its start line after the start was asked for, and it is still
//! running a moment later.
//!
//! Service control is a trait of its own, apart from the [`Machine`]'s
//! files, so a test can answer it with a runner in its own process while
//! the files are real.
use super::install::Machine;
use super::model::{Layout, PRESHUTDOWN_MS, Role};
use anyhow::{Result, bail};
use std::time::Duration;

/// Where a service stands, as the Service Control Manager reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    /// No such service is registered.
    Missing,
    /// Stopped. `exit` is its Win32 exit code and `specific` its own, which
    /// the runner sets when it stopped with launches whose end Windows did
    /// not confirm.
    Stopped { exit: u32, specific: u32 },
    /// Starting, stopping, pausing or paused.
    Pending,
    /// Running.
    Running,
}

/// What the updater asks of the Service Control Manager.
pub trait Services {
    /// Where the service called `name` stands.
    fn state(&mut self, name: &str) -> Result<RunState>;
    /// Asks Windows to start the service; does not wait.
    fn start(&mut self, name: &str) -> Result<()>;
    /// Asks the service to stop; does not wait.
    fn stop(&mut self, name: &str) -> Result<()>;
    /// Waits `duration` before the next question. A test answers at once.
    fn pause(&mut self, duration: Duration);
}

/// How often a wait asks again.
pub const POLL: Duration = Duration::from_millis(500);
/// How long a stop may take: the runner's drain, its patience after the
/// deadline, and a margin. Its `preshutdown_timeout_seconds` default is the
/// sum of the first two.
pub const STOP_PATIENCE: Duration = Duration::from_millis(PRESHUTDOWN_MS as u64 + 30_000);
/// How long a start may take before the service reports running and logs it.
pub const START_PATIENCE: Duration = Duration::from_secs(60);
/// How long a started service must still be running afterwards.
pub const SETTLE: Duration = Duration::from_secs(5);

/// The order services stop in: the supervisor before the runner it depends
/// on (Windows refuses to stop the runner under it), then the brokers.
pub const STOP_ORDER: [Role; 4] = [
    Role::Supervisor,
    Role::Runner,
    Role::Egress,
    Role::Publication,
];

/// The line `role`'s service writes to its log once it runs.
pub fn start_line(role: Role) -> &'static str {
    match role {
        Role::Runner => "running",
        Role::Supervisor => "agentc-supervisor windows-service: supervisor running",
        Role::Egress => "agentc-supervisor windows-service: egress broker running",
        Role::Publication => "agentc-supervisor windows-service: publication broker running",
    }
}

/// Whether `text` holds `role`'s start line. The runner's lines begin with
/// a time; the others' do not.
pub fn has_start_line(role: Role, text: &str) -> bool {
    let line = start_line(role);
    text.lines()
        .map(str::trim_end)
        .any(|seen| seen == line || (role == Role::Runner && seen.ends_with(&format!(" {line}"))))
}

impl Layout {
    /// The log of `role`'s service.
    pub fn log(&self, role: Role) -> String {
        self.state(&format!(r"{}\service.log", role.suffix()))
    }

    /// Every service with its role, in `order`.
    pub fn named(&self, order: &[Role]) -> Vec<(Role, String)> {
        order
            .iter()
            .map(|&role| (role, self.service(role)))
            .collect()
    }
}

/// Stops the service `name` and waits until it has stopped. A service that
/// is already stopped is left as it is. One that stops with an exit code,
/// such as the runner when launches may still run, is an error: nothing may
/// be switched under it.
pub fn stop(services: &mut dyn Services, name: &str) -> Result<()> {
    match services.state(name)? {
        RunState::Missing => bail!("the service {name} is not registered"),
        RunState::Stopped { .. } => return Ok(()),
        RunState::Running => services.stop(name)?,
        RunState::Pending => {}
    }
    match wait(services, name, STOP_PATIENCE, |s| {
        s != RunState::Pending && s != RunState::Running
    })? {
        RunState::Stopped { exit: 0, .. } => Ok(()),
        RunState::Stopped { exit, specific } => bail!(
            "{name} stopped with exit code {exit} (its own code {specific}); a launch may still run"
        ),
        other => bail!("{name} did not stop within {STOP_PATIENCE:?}: {other:?}"),
    }
}

/// Asks the service `name` to start unless it runs, and waits until
/// Windows reports it running.
pub fn start(services: &mut dyn Services, name: &str) -> Result<()> {
    match services.state(name)? {
        RunState::Missing => bail!("the service {name} is not registered"),
        RunState::Running => return Ok(()),
        RunState::Stopped { .. } => services.start(name)?,
        RunState::Pending => {}
    }
    match wait(services, name, START_PATIENCE, |s| s != RunState::Pending)? {
        RunState::Running => Ok(()),
        other => bail!("{name} did not start: {other:?}"),
    }
}

/// Asks for `name`'s state until `done` holds or `patience` runs out, and
/// returns the last state seen.
fn wait(
    services: &mut dyn Services,
    name: &str,
    patience: Duration,
    done: impl Fn(RunState) -> bool,
) -> Result<RunState> {
    let mut waited = Duration::ZERO;
    loop {
        let state = services.state(name)?;
        if done(state) || waited >= patience {
            return Ok(state);
        }
        services.pause(POLL);
        waited += POLL;
    }
}

/// Checks that `role`'s service `name` is healthy: running, its log grown
/// past `offset` with its start line, and still running after [`SETTLE`].
pub fn check(
    host: &dyn Machine,
    services: &mut dyn Services,
    layout: &Layout,
    (role, name): (Role, &str),
    offset: u64,
) -> Result<()> {
    let log = layout.log(role);
    let mut waited = Duration::ZERO;
    while !host
        .read_after(&log, offset)?
        .is_some_and(|text| has_start_line(role, &text))
    {
        if waited >= START_PATIENCE {
            bail!("{name} runs but its log {log} gained no start line");
        }
        services.pause(POLL);
        waited += POLL;
    }
    services.pause(SETTLE);
    match services.state(name)? {
        RunState::Running => Ok(()),
        other => bail!("{name} logged its start and then stopped: {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_start_line_is_recognized_and_nothing_else() {
        let runner = "2026-11-24T03:00:00Z start-up: 0 reclaimed\n2026-11-24T03:00:01Z running\n";
        assert!(has_start_line(Role::Runner, runner));
        assert!(!has_start_line(
            Role::Runner,
            "2026-11-24T03:00:00Z not running yet\n"
        ));
        let broker = "{\"tokens\":[]}\nagentc-supervisor windows-service: egress broker running\n";
        assert!(has_start_line(Role::Egress, broker));
        assert!(!has_start_line(Role::Publication, broker));
        assert!(!has_start_line(Role::Supervisor, ""));
    }

    #[test]
    fn services_stop_with_the_supervisor_first_and_the_brokers_last() {
        assert_eq!(STOP_ORDER[0], Role::Supervisor);
        assert_eq!(STOP_ORDER[1], Role::Runner);
        let mut sorted = STOP_ORDER;
        sorted.sort();
        assert_eq!(sorted, Role::ALL);
    }
}
