//! The settings of the role runner when it runs as a service. Every one has
//! an in-code default, so the service starts from an empty configuration
//! file; only the client account has no usable default, because the pipe
//! serves nobody until the owner names it.
use super::lifecycle::Limits;
use super::pipe_policy::{checked_sid, pipe_path};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// The `[windows_service]` table of the supervisor configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ServiceSettings {
    /// The service's name, as registered with the Service Control Manager.
    pub name: String,
    /// Where the runner journals its launches. It is created with an access
    /// list for the runner's account, the system and administrators only.
    pub state_dir: PathBuf,
    /// The pipe's name below `\\.\pipe\`.
    pub pipe_name: String,
    /// The one account whose process may use the pipe, as SID text (the
    /// supervisor's service identity). Empty: the pipe serves nobody and the
    /// service refuses to start.
    pub client_sid: String,
    /// Seconds a stop request gives launches to end by themselves.
    pub stop_grace_seconds: u64,
    /// Seconds a preshutdown request gives launches to end by themselves.
    pub preshutdown_grace_seconds: u64,
    /// The time, in seconds, the Service Control Manager gives the service
    /// to stop at preshutdown. The service sets it on itself at start when
    /// it is permitted to.
    pub preshutdown_timeout_seconds: u64,
    /// Seconds termination may take after the grace before the service gives
    /// up waiting for Windows to confirm it.
    pub terminate_patience_seconds: u64,
    /// Seconds between status reports while the service is stopping.
    pub status_interval_seconds: u64,
    /// The egress broker's control pipe below `\\.\pipe\`. Empty: no
    /// launch is offered brokered egress, and a launch's token still has
    /// no network at all. Set: every launch must be admitted by the broker
    /// before it starts, and a launch the broker does not admit, or whose
    /// broker is not there, is not started.
    pub egress_control_pipe: String,
    /// The egress broker's account, as SID text: its control pipe is used
    /// only when this account owns it. Required with `egress_control_pipe`.
    pub egress_broker_sid: String,
    /// The publication broker's control pipe below `\\.\pipe\`. Empty: no
    /// launch is offered brokered publication. Set: every implementer
    /// launch must be admitted by the broker before it starts, as with
    /// egress, and is told its pipe in the CLI's candidate push variable.
    pub publication_control_pipe: String,
    /// The publication broker's account, as SID text. Required with
    /// `publication_control_pipe`.
    pub publication_broker_sid: String,
}

impl Default for ServiceSettings {
    fn default() -> Self {
        Self {
            name: "agentc-runner".into(),
            state_dir: PathBuf::from(r"C:\ProgramData\agentc\runner"),
            pipe_name: "agentc-runner".into(),
            client_sid: String::new(),
            stop_grace_seconds: 30,
            preshutdown_grace_seconds: 60,
            preshutdown_timeout_seconds: 120,
            terminate_patience_seconds: 20,
            status_interval_seconds: 3,
            egress_control_pipe: String::new(),
            egress_broker_sid: String::new(),
            publication_control_pipe: String::new(),
            publication_broker_sid: String::new(),
        }
    }
}

impl ServiceSettings {
    /// The stages' time limits.
    pub fn limits(&self) -> Limits {
        let seconds = Duration::from_secs;
        Limits {
            stop_grace: seconds(self.stop_grace_seconds),
            preshutdown_grace: seconds(self.preshutdown_grace_seconds),
            terminate_patience: seconds(self.terminate_patience_seconds),
            status_interval: seconds(self.status_interval_seconds),
        }
    }

    /// The pipe's full path.
    pub fn pipe(&self) -> Result<String> {
        pipe_path(&self.pipe_name)
    }

    /// The egress broker's control pipe and account, when launches are
    /// to be admitted by one; an error when only one of them is set or
    /// either is malformed.
    pub fn egress(&self) -> Result<Option<(&str, &str)>> {
        let pair = (
            self.egress_control_pipe.as_str(),
            self.egress_broker_sid.as_str(),
        );
        broker_pair("egress", pair)
    }

    /// The publication broker's control pipe and account, as
    /// [`ServiceSettings::egress`].
    pub fn publication(&self) -> Result<Option<(&str, &str)>> {
        let pair = (
            self.publication_control_pipe.as_str(),
            self.publication_broker_sid.as_str(),
        );
        broker_pair("publication", pair)
    }

    /// Refuses settings the service could not honor. The service calls it
    /// before it opens anything.
    pub fn validate(&self) -> Result<()> {
        ensure!(!self.name.is_empty(), "windows_service.name is empty");
        pipe_path(&self.pipe_name)?;
        ensure!(
            !self.client_sid.is_empty(),
            "windows_service.client_sid names no client account; the pipe would serve nobody"
        );
        checked_sid(&self.client_sid)?;
        self.egress()?;
        self.publication()?;
        ensure!(
            self.status_interval_seconds >= 1,
            "windows_service.status_interval_seconds must be at least 1"
        );
        ensure!(
            self.stop_grace_seconds >= 1 && self.preshutdown_grace_seconds >= 1,
            "the grace periods must be at least 1 second"
        );
        ensure!(
            self.terminate_patience_seconds >= 1,
            "windows_service.terminate_patience_seconds must be at least 1"
        );
        let needed = self.preshutdown_grace_seconds + self.terminate_patience_seconds;
        ensure!(
            self.preshutdown_timeout_seconds > needed,
            "windows_service.preshutdown_timeout_seconds ({}) must exceed the preshutdown grace \
             plus the termination patience ({needed}), or the system ends the service first",
            self.preshutdown_timeout_seconds
        );
        Ok(())
    }
}

/// A broker's control pipe and account from the settings named after
/// `broker`: both empty is none, and one without the other or either
/// malformed is an error.
fn broker_pair<'a>(
    broker: &str,
    (pipe, sid): (&'a str, &'a str),
) -> Result<Option<(&'a str, &'a str)>> {
    if pipe.is_empty() && sid.is_empty() {
        return Ok(None);
    }
    ensure!(
        !pipe.is_empty() && !sid.is_empty(),
        "windows_service.{broker}_control_pipe and {broker}_broker_sid are set together \
         or not at all"
    );
    pipe_path(pipe)?;
    Ok(Some((pipe, checked_sid(sid)?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> ServiceSettings {
        ServiceSettings {
            client_sid: "S-1-5-21-1-2-3-1002".into(),
            ..ServiceSettings::default()
        }
    }

    #[test]
    fn the_defaults_are_complete_except_for_the_client() {
        let settings = ServiceSettings::default();
        assert_eq!(settings.pipe().unwrap(), r"\\.\pipe\agentc-runner");
        assert!(
            settings
                .state_dir
                .to_string_lossy()
                .starts_with(r"C:\ProgramData\")
        );
        let refused = settings.validate().unwrap_err().to_string();
        assert!(refused.contains("client_sid"), "{refused}");
        valid().validate().unwrap();
        assert_eq!(valid().egress().unwrap(), None, "no egress by default");
    }

    #[test]
    fn an_egress_broker_is_named_by_its_control_pipe_and_its_account() {
        let settings = ServiceSettings {
            egress_control_pipe: "agentc-egress-control".into(),
            egress_broker_sid: "S-1-5-80-1-2".into(),
            ..valid()
        };
        settings.validate().unwrap();
        let named = settings.egress().unwrap();
        assert_eq!(named, Some(("agentc-egress-control", "S-1-5-80-1-2")));
    }

    #[test]
    fn a_publication_broker_is_named_as_the_egress_broker_is_and_none_by_default() {
        assert_eq!(valid().publication().unwrap(), None);
        let settings = ServiceSettings {
            publication_control_pipe: "agentc-publication-control".into(),
            publication_broker_sid: "S-1-5-80-3-4".into(),
            ..valid()
        };
        settings.validate().unwrap();
        let named = settings.publication().unwrap();
        assert_eq!(named, Some(("agentc-publication-control", "S-1-5-80-3-4")));
        for half in [
            ServiceSettings {
                publication_control_pipe: "agentc-publication-control".into(),
                ..valid()
            },
            ServiceSettings {
                publication_broker_sid: "S-1-5-80-3-4".into(),
                ..valid()
            },
        ] {
            let refused = half.validate().unwrap_err().to_string();
            assert!(refused.contains("publication_control_pipe"), "{refused}");
        }
    }

    #[test]
    fn the_stage_limits_follow_the_settings() {
        let limits = valid().limits();
        assert_eq!(limits.stop_grace, Duration::from_secs(30));
        assert_eq!(limits.preshutdown_grace, Duration::from_secs(60));
        assert_eq!(limits.terminate_patience, Duration::from_secs(20));
        assert_eq!(limits.status_interval, Duration::from_secs(3));
    }

    #[test]
    fn settings_the_service_could_not_honor_are_refused() {
        let broken: [fn(&mut ServiceSettings); 11] = [
            |s| s.egress_control_pipe = "agentc-egress-control".into(),
            |s| s.egress_broker_sid = "S-1-5-80-1-2".into(),
            |s| {
                s.egress_control_pipe = r"..\x".into();
                s.egress_broker_sid = "S-1-5-80-1-2".into();
            },
            |s| s.client_sid = "everyone".into(),
            |s| s.client_sid = "S-1-5-1)(A;;GA;;;WD".into(),
            |s| s.pipe_name = r"..\x".into(),
            |s| s.name.clear(),
            |s| s.status_interval_seconds = 0,
            |s| s.stop_grace_seconds = 0,
            |s| s.terminate_patience_seconds = 0,
            // 60 + 20 does not fit in 80.
            |s| s.preshutdown_timeout_seconds = 80,
        ];
        for (index, change) in broken.iter().enumerate() {
            let mut settings = valid();
            change(&mut settings);
            assert!(settings.validate().is_err(), "case {index}");
        }
    }
}
