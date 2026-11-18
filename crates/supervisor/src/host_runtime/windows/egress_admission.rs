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
        fn serve(&self, _: &Grant, _: &mut Session<'_>, _: &Halt) -> Result<()> {
            anyhow::bail!("no launch reaches this broker")
        }
    }

    fn me() -> String {
        token::current_user_sid().unwrap()
    }

    /// A journal entry of a prepared launch with `role`.
    fn entry(role: Option<Role>) -> Entry {
        Entry {
            launch: "adm-1".into(),
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
        assert_eq!(pipe, r"\\.\pipe\agentc-broker-egress-adm-1");
        assert_eq!(admitted.environment["A"], "1");
        assert_eq!(admitted.argv, command().argv);
        assert!(route.release("adm-1").unwrap());
        assert!(!route.release("adm-1").unwrap());
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
    fn a_route_takes_only_a_plain_pipe_name_and_sid_text() {
        assert!(EgressRoute::new(r"..\x", &me()).is_err());
        assert!(EgressRoute::new("agentc-egress-control", "WD").is_err());
    }
}
