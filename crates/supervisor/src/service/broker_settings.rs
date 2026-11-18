//! The settings of the egress and publication brokers when they run as
//! Windows services (phase 6, step 6d). Every entry has an in-code default
//! except the accounts and the publication remote: a broker serves nobody
//! until the owner names the runner's account, and so neither broker
//! service starts from an empty configuration. That is the off position;
//! the runner offers no launch a broker until its own
//! `egress_control_pipe` or `publication_control_pipe` is set either.
use super::pipe_policy::{checked_sid, pipe_path};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// What every broker service needs, whichever broker it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrokerCore<'a> {
    /// The table the settings came from, for messages.
    pub table: &'static str,
    /// The service's name, as registered with the Service Control Manager.
    pub name: &'a str,
    /// The service's own directory, made with an access list for the
    /// broker's account, the system and administrators only.
    pub state_dir: &'a Path,
    /// The control pipe below `\\.\pipe\`.
    pub control_pipe: &'a str,
    /// The one account the control pipe serves, as SID text.
    pub runner_sid: &'a str,
}

impl BrokerCore<'_> {
    /// Refuses settings the service could not honor.
    pub fn validate(&self) -> Result<()> {
        let table = self.table;
        ensure!(!self.name.is_empty(), "{table}.name is empty");
        ensure!(
            !self.state_dir.as_os_str().is_empty(),
            "{table}.state_dir is empty"
        );
        pipe_path(self.control_pipe)?;
        ensure!(
            !self.runner_sid.is_empty(),
            "{table}.runner_sid names no runner account; the control pipe would serve nobody"
        );
        checked_sid(self.runner_sid)?;
        Ok(())
    }
}

/// The `[windows_egress]` table. The destinations the broker allows are
/// the Linux proxy's: `egress_allow` and `egress_allow_extra`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct EgressSettings {
    /// The service's name in the Service Control Manager.
    pub name: String,
    /// The service's directory, holding its `service.log`.
    pub state_dir: PathBuf,
    /// The control pipe the runner admits launches through.
    pub control_pipe: String,
    /// The runner service's account, as SID text. Empty: the service
    /// refuses to start.
    pub runner_sid: String,
}

impl Default for EgressSettings {
    fn default() -> Self {
        Self {
            name: "agentc-egress".into(),
            state_dir: PathBuf::from(r"C:\ProgramData\agentc\egress"),
            control_pipe: "agentc-egress-control".into(),
            runner_sid: String::new(),
        }
    }
}

impl EgressSettings {
    /// The settings every broker service has.
    pub fn core(&self) -> BrokerCore<'_> {
        BrokerCore {
            table: "windows_egress",
            name: &self.name,
            state_dir: &self.state_dir,
            control_pipe: &self.control_pipe,
            runner_sid: &self.runner_sid,
        }
    }
}

/// The `[windows_publication]` table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct PublicationSettings {
    /// The service's name in the Service Control Manager.
    pub name: String,
    /// The service's directory, holding its `service.log` and, below
    /// `work`, each launch's private repository.
    pub state_dir: PathBuf,
    /// The control pipe the runner admits implementer launches through.
    pub control_pipe: String,
    /// The runner service's account, as SID text. Empty: the service
    /// refuses to start.
    pub runner_sid: String,
    /// The one remote candidates are published to. Empty: the service
    /// refuses to start.
    pub remote: String,
    /// The supervisor's authority pipe below `\\.\pipe\`, asked before
    /// each push.
    pub authority_pipe: String,
    /// The supervisor service's account, as SID text: the authority pipe
    /// is asked only when this account owns it. Empty: the service refuses
    /// to start.
    pub supervisor_sid: String,
}

impl Default for PublicationSettings {
    fn default() -> Self {
        Self {
            name: "agentc-publication".into(),
            state_dir: PathBuf::from(r"C:\ProgramData\agentc\publication"),
            control_pipe: "agentc-publication-control".into(),
            runner_sid: String::new(),
            remote: String::new(),
            authority_pipe: "agentc-supervisor-authority".into(),
            supervisor_sid: String::new(),
        }
    }
}

impl PublicationSettings {
    /// The settings every broker service has.
    pub fn core(&self) -> BrokerCore<'_> {
        BrokerCore {
            table: "windows_publication",
            name: &self.name,
            state_dir: &self.state_dir,
            control_pipe: &self.control_pipe,
            runner_sid: &self.runner_sid,
        }
    }

    /// Where the broker keeps each launch's private repository.
    pub fn work_root(&self) -> PathBuf {
        self.state_dir.join("work")
    }

    /// Refuses settings the service could not honor.
    pub fn validate(&self) -> Result<()> {
        self.core().validate()?;
        ensure!(
            !self.remote.is_empty(),
            "windows_publication.remote names no remote to publish to"
        );
        pipe_path(&self.authority_pipe)?;
        ensure!(
            !self.supervisor_sid.is_empty(),
            "windows_publication.supervisor_sid names no supervisor account to ask"
        );
        checked_sid(&self.supervisor_sid)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUNNER: &str = "S-1-5-80-1-2-3-4-5";
    const SUPERVISOR: &str = "S-1-5-80-6-7-8-9-10";

    /// Publication settings that name everything they must.
    fn publication() -> PublicationSettings {
        PublicationSettings {
            runner_sid: RUNNER.into(),
            remote: "https://github.com/owner/repository.git".into(),
            supervisor_sid: SUPERVISOR.into(),
            ..PublicationSettings::default()
        }
    }

    #[test]
    fn neither_broker_starts_from_its_defaults() {
        let refused = EgressSettings::default().core().validate().unwrap_err();
        assert!(refused.to_string().contains("runner_sid"), "{refused}");
        let refused = PublicationSettings::default().validate().unwrap_err();
        assert!(refused.to_string().contains("runner_sid"), "{refused}");
    }

    #[test]
    fn the_defaults_name_the_pipes_the_runner_and_supervisor_use() {
        let egress = EgressSettings::default();
        assert_eq!(egress.control_pipe, "agentc-egress-control");
        let publication = PublicationSettings::default();
        assert_eq!(publication.control_pipe, "agentc-publication-control");
        let supervisor = super::super::supervisor::SupervisorSettings::default();
        assert_eq!(publication.authority_pipe, supervisor.authority_pipe);
        assert!(publication.work_root().starts_with(&publication.state_dir));
    }

    #[test]
    fn named_accounts_and_a_remote_are_enough() {
        let egress = EgressSettings {
            runner_sid: RUNNER.into(),
            ..EgressSettings::default()
        };
        egress.core().validate().unwrap();
        publication().validate().unwrap();
    }

    #[test]
    fn settings_a_broker_could_not_honor_are_refused() {
        let broken: [fn(&mut PublicationSettings); 8] = [
            |s| s.name.clear(),
            |s| s.state_dir = PathBuf::new(),
            |s| s.control_pipe = r"..\x".into(),
            |s| s.runner_sid = "everyone".into(),
            |s| s.remote.clear(),
            |s| s.authority_pipe = r"..\x".into(),
            |s| s.supervisor_sid.clear(),
            |s| s.supervisor_sid = "S-1-5-1)(A;;GA;;;WD".into(),
        ];
        for (index, change) in broken.iter().enumerate() {
            let mut settings = publication();
            change(&mut settings);
            assert!(settings.validate().is_err(), "case {index}");
        }
    }
}
