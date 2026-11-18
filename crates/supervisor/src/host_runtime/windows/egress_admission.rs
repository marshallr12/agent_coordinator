//! The runner's side of brokered egress (phase 6, step 6b): with an
//! egress broker configured, no launch starts until the broker has admitted
//! it, and a launch the broker does not admit is not started.
//!
//! Admission fails closed. A broker that is not running, a control pipe
//! another account owns, a launch with no role and a request the broker
//! refuses each keep the launch from starting; nothing falls back to an
//! unbrokered start. The launch learns its pipe's path from
//! [`PIPE_VARIABLE`]; its token still has no network capability, so the
//! pipe is its only way out.
use super::broker_control::{ControlClient, Opened};
use super::egress_broker::PIPE_VARIABLE;
use super::journal::Entry;
use super::process::Command;
use crate::service::pipe_policy::{checked_sid, pipe_path};
use anyhow::{Context, Result};
use std::collections::BTreeSet;

/// The generation a launch is admitted at when its caller named none.
const FIRST_GENERATION: u64 = 1;

/// The egress broker a runner has every launch admitted by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressRoute {
    control_pipe: String,
    broker_sid: String,
}

impl EgressRoute {
    /// The broker serving the control pipe `control_pipe` as the account
    /// `broker_sid` (SID text).
    pub fn new(control_pipe: &str, broker_sid: &str) -> Result<Self> {
        pipe_path(control_pipe)?;
        Ok(Self {
            control_pipe: control_pipe.to_owned(),
            broker_sid: checked_sid(broker_sid)?.to_owned(),
        })
    }

    /// Has the broker admit `entry`'s launch and returns `command` with the
    /// launch's pipe in [`PIPE_VARIABLE`]. An error means the launch must
    /// not start.
    pub fn admit(&self, entry: &Entry, command: &Command) -> Result<Command> {
        let launch = &entry.launch;
        let opened = self.open(entry).with_context(|| {
            format!("the egress broker did not admit launch {launch}; it is not started")
        })?;
        let mut command = command.clone();
        let path = pipe_path(&opened.pipe)?;
        command.environment.insert(PIPE_VARIABLE.into(), path);
        Ok(command)
    }

    /// Asks the broker to serve `entry`'s launch.
    fn open(&self, entry: &Entry) -> Result<Opened> {
        let role = entry.role.context("the launch has no role")?;
        let generation = entry.generation.unwrap_or(FIRST_GENERATION);
        let mut control = self.connect()?;
        control.open(&entry.launch, role, &entry.package_sid, generation)
    }

    /// Asks the broker to stop serving `launch`, which ends its tunnels and
    /// removes its pipe; false when the broker served none.
    pub fn release(&self, launch: &str) -> Result<bool> {
        self.connect()?.close(launch)
    }

    /// A connection to the broker's control pipe, which its account owns.
    fn connect(&self) -> Result<ControlClient> {
        ControlClient::connect(&self.control_pipe, &self.broker_sid)
            .with_context(|| format!("reach the egress broker's pipe {}", self.control_pipe))
    }
}

/// A runner's egress broker, if it has one, and the launches of this run
/// the broker admitted and has not yet been seen to release.
#[derive(Debug, Default)]
pub struct Admissions {
    route: Option<EgressRoute>,
    admitted: BTreeSet<String>,
}

impl Admissions {
    /// Admissions through `route`; `None` offers no launch any egress.
    pub fn new(route: Option<EgressRoute>) -> Self {
        Self {
            route,
            admitted: BTreeSet::new(),
        }
    }

    /// `command` as `entry`'s launch starts with it: unchanged without a
    /// broker, and with a broker only once the broker admitted the launch.
    /// An error means the launch must not start.
    pub fn admit(&mut self, entry: &Entry, command: &Command) -> Result<Command> {
        let Some(route) = &self.route else {
            return Ok(command.clone());
        };
        let admitted = route.admit(entry, command)?;
        self.admitted.insert(entry.launch.clone());
        Ok(admitted)
    }

    /// Asks the broker to stop serving this run's admitted `launch`; what
    /// went wrong, if anything. The launch stays recorded as admitted until
    /// a release succeeds, so a later cleanup asks again.
    pub fn release(&mut self, launch: &str) -> Option<String> {
        let route = self.route.as_ref()?;
        if !self.admitted.contains(launch) {
            return None;
        }
        match route.release(launch) {
            Ok(_) => {
                self.admitted.remove(launch);
                None
            }
            Err(error) => Some(format!("{error:#}")),
        }
    }

    /// Asks the broker, if there is one, to stop serving an earlier
    /// runner's ended launch, which it may still list; what went wrong, if
    /// anything. A broker that never served it says so, which is no error.
    pub fn release_earlier(&self, launch: &str) -> Option<String> {
        let released = self.route.as_ref()?.release(launch);
        released.err().map(|error| format!("{error:#}"))
    }

    /// Whether `launch` is recorded as admitted and not yet released.
    pub fn holds(&self, launch: &str) -> bool {
        self.admitted.contains(launch)
    }
}

#[cfg(test)]
mod tests {
    use super::super::broker::{Broker, Grant, Halt, Handler, Role};
    use super::super::broker_control::Control;
    use super::super::journal::Stage;
    use super::super::pipe::Session;
    use super::super::token;
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    /// A handler nobody reaches in these tests.
    struct Unused;

    impl Handler for Unused {
        /// Refuses: no test token carries a package, so none is admitted.
        fn serve(&self, _: &Grant, _: &mut Session<'_>, _: &Halt) -> Result<()> {
            anyhow::bail!("no launch reaches this broker")
        }
    }

    /// This process's account, which the test brokers run as.
    fn me() -> String {
        token::current_user_sid().unwrap()
    }

    /// The launch id of the running test: its broker pipe's name must
    /// not be another test's, as the tests run at once.
    fn id() -> String {
        let test = std::thread::current().name().unwrap_or("main").to_owned();
        let plain = test.rsplit("::").next().unwrap_or("main").replace('_', "-");
        format!("adm-{plain}")
    }

    /// A journal entry of the test's prepared launch with `role`.
    fn entry(role: Option<Role>) -> Entry {
        Entry {
            launch: id(),
            stage: Stage::Prepared,
            profile: "agentc.test".into(),
            package_sid: "S-1-15-2-1-2-3-4-5-6-7".into(),
            job_owner: "runner:1:1".into(),
            executable: None,
            source: None,
            attempt: None,
            generation: Some(3),
            role,
            pid: None,
            created: None,
            control: None,
            work: None,
            candidate: None,
        }
    }

    /// A command that names no egress pipe.
    fn command() -> Command {
        Command {
            argv: vec!["tool.exe".into()],
            directory: PathBuf::from("w"),
            environment: BTreeMap::from([("A".into(), "1".into())]),
            output: PathBuf::from("w/log"),
        }
    }

    /// Serves a control pipe for a test broker until `done` is set.
    fn broker_serving(name: &str, done: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
        let broker = Broker::new(
            super::super::egress_broker::SERVICE,
            &me(),
            Arc::new(Unused),
        );
        let mut control = Control::new(name, broker.unwrap(), &me(), None).unwrap();
        std::thread::spawn(move || {
            let stop = || done.load(Ordering::SeqCst);
            while !stop() {
                control
                    .serve_one(Duration::from_millis(100), &stop)
                    .unwrap();
            }
        })
    }

    /// A control pipe name no other test or run uses.
    fn unique() -> String {
        format!("agentc-test-admission-{}", uuid::Uuid::new_v4().simple())
    }

    #[test]
    fn an_admitted_launch_starts_with_its_pipe_and_nothing_else_changes() {
        let (name, done) = (unique(), Arc::new(AtomicBool::new(false)));
        let server = broker_serving(&name, done.clone());
        let route = EgressRoute::new(&name, &me()).unwrap();
        let admitted = route
            .admit(&entry(Some(Role::Implementer)), &command())
            .unwrap();
        let pipe = &admitted.environment[PIPE_VARIABLE];
        assert_eq!(*pipe, format!(r"\\.\pipe\agentc-broker-egress-{}", id()));
        assert_eq!(admitted.environment["A"], "1");
        assert_eq!(admitted.argv, command().argv);
        assert!(route.release(&id()).unwrap());
        assert!(!route.release(&id()).unwrap());
        done.store(true, Ordering::SeqCst);
        server.join().unwrap();
    }

    #[test]
    fn a_missing_broker_or_a_launch_without_a_role_is_not_started() {
        let missing = EgressRoute::new(&unique(), &me()).unwrap();
        let refused = missing.admit(&entry(Some(Role::Reviewer)), &command());
        let refused = format!("{:#}", refused.unwrap_err());
        assert!(refused.contains("not started"), "{refused}");
        let (name, done) = (unique(), Arc::new(AtomicBool::new(false)));
        let server = broker_serving(&name, done.clone());
        let route = EgressRoute::new(&name, &me()).unwrap();
        let roleless = format!("{:#}", route.admit(&entry(None), &command()).unwrap_err());
        assert!(roleless.contains("no role"), "{roleless}");
        done.store(true, Ordering::SeqCst);
        server.join().unwrap();
    }

    #[test]
    fn a_control_pipe_another_account_owns_admits_nothing() {
        let (name, done) = (unique(), Arc::new(AtomicBool::new(false)));
        let server = broker_serving(&name, done.clone());
        let route = EgressRoute::new(&name, "S-1-5-80-1-2-3-4-5").unwrap();
        let refused = route.admit(&entry(Some(Role::Implementer)), &command());
        let refused = format!("{:#}", refused.unwrap_err());
        assert!(refused.contains("not by the serving account"), "{refused}");
        done.store(true, Ordering::SeqCst);
        server.join().unwrap();
    }

    #[test]
    fn without_a_broker_a_command_is_unchanged_and_nothing_is_recorded() {
        let mut none = Admissions::default();
        let started = none.admit(&entry(None), &command()).unwrap();
        assert_eq!(started, command());
        assert!(!none.holds(&id()));
        assert_eq!(none.release(&id()), None);
        assert_eq!(none.release_earlier(&id()), None);
    }

    #[test]
    fn a_refused_launch_is_not_recorded_as_admitted() {
        let route = EgressRoute::new(&unique(), &me()).unwrap();
        let mut admissions = Admissions::new(Some(route));
        let refused = admissions.admit(&entry(Some(Role::Implementer)), &command());
        assert!(refused.is_err());
        assert!(!admissions.holds(&id()));
    }

    #[test]
    fn a_release_that_fails_keeps_the_launch_for_the_next_one() {
        let name = unique();
        let route = EgressRoute::new(&name, &me()).unwrap();
        let mut admissions = Admissions::new(Some(route));
        let done = Arc::new(AtomicBool::new(false));
        let server = broker_serving(&name, done.clone());
        admissions
            .admit(&entry(Some(Role::Implementer)), &command())
            .unwrap();
        done.store(true, Ordering::SeqCst);
        server.join().unwrap();
        let failed = admissions.release(&id());
        assert!(failed.is_some(), "the broker is gone");
        assert!(admissions.holds(&id()), "forgotten after a failed release");
        let done = Arc::new(AtomicBool::new(false));
        let server = broker_serving(&name, done.clone());
        assert_eq!(admissions.release(&id()), None);
        assert!(!admissions.holds(&id()));
        done.store(true, Ordering::SeqCst);
        server.join().unwrap();
    }

    #[test]
    fn back_to_back_exchanges_on_the_one_control_pipe_instance_all_arrive() {
        let (name, done) = (unique(), Arc::new(AtomicBool::new(false)));
        let server = broker_serving(&name, done.clone());
        let route = EgressRoute::new(&name, &me()).unwrap();
        let launch = entry(Some(Role::Implementer));
        for round in 0..100 {
            let admitted = route.admit(&launch, &command());
            admitted.unwrap_or_else(|error| panic!("admit {round}: {error:#}"));
            let released = route.release(&id());
            assert!(released.unwrap_or_else(|error| panic!("release {round}: {error:#}")));
        }
        done.store(true, Ordering::SeqCst);
        server.join().unwrap();
    }

    #[test]
    fn a_route_takes_only_a_plain_pipe_name_and_sid_text() {
        assert!(EgressRoute::new(r"..\x", &me()).is_err());
        assert!(EgressRoute::new("agentc-egress-control", "WD").is_err());
    }
}
