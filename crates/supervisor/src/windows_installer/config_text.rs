//! The generated `supervisor.toml`. The installer manages a fixed set of
//! entries: the service names, their directories and pipes, each other's
//! SIDs, the remote, the push App, the stand-in and the coordinator. It
//! writes one only when its value is not the in-code default, so a default
//! installation's file holds the SIDs and the remote and little else. Every
//! other entry is the owner's: it is kept as found, though comments are not.
use super::model::{Inputs, Layout, Role, Sids};
use crate::config::Config;
use crate::service::broker_settings::{EgressSettings, PublicationSettings};
use crate::service::coordinator::CoordinatorSettings;
use crate::service::settings::ServiceSettings;
use crate::service::supervisor::SupervisorSettings;
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::PathBuf;
use toml::{Table, Value};

/// The first lines of a file the installer wrote.
const HEADER: &str = "# Written by `agentc-supervisor windows-host install`. It sets what has no\n\
     # in-code default; `windows-host status` reports a managed entry that was\n\
     # changed and `install` puts it back. Other entries are kept.\n";

/// The entries the installer manages, by table.
const MANAGED: [(&str, &[&str]); 4] = [
    (
        "windows_service",
        &[
            "name",
            "state_dir",
            "pipe_name",
            "client_sid",
            "egress_control_pipe",
            "egress_broker_sid",
            "publication_control_pipe",
            "publication_broker_sid",
        ],
    ),
    (
        "windows_supervisor",
        &[
            "name",
            "state_dir",
            "runner_pipe",
            "runner_sid",
            "authority_pipe",
            "publication_broker_sid",
            "stand_in",
            "coordinator",
        ],
    ),
    (
        "windows_egress",
        &[
            "name",
            "state_dir",
            "control_pipe",
            "runner_sid",
            "must_not_open",
        ],
    ),
    (
        "windows_publication",
        &[
            "name",
            "state_dir",
            "control_pipe",
            "runner_sid",
            "remote",
            "authority_pipe",
            "supervisor_sid",
            "app_id",
            "installation_id",
            "private_key",
        ],
    ),
];

/// The four services' settings.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Settings {
    /// `[windows_service]`: the runner.
    pub runner: ServiceSettings,
    /// `[windows_supervisor]`.
    pub supervisor: SupervisorSettings,
    /// `[windows_egress]`.
    pub egress: EgressSettings,
    /// `[windows_publication]`.
    pub publication: PublicationSettings,
}

impl Settings {
    /// Refuses settings a service would refuse at start.
    pub fn validate(&self) -> Result<()> {
        self.runner.validate()?;
        self.supervisor.validate()?;
        self.egress.core().validate()?;
        self.publication.validate()
    }

    /// The settings a configuration file's text holds.
    pub fn parse(text: &str) -> Result<Self> {
        let config: Config = toml::from_str(text)?;
        Ok(Self {
            runner: config.windows_service,
            supervisor: config.windows_supervisor,
            egress: config.windows_egress,
            publication: config.windows_publication,
        })
    }

    /// Every managed entry by dotted name, whatever its value.
    fn managed(&self) -> Result<BTreeMap<String, Value>> {
        let tables = [
            Table::try_from(&self.runner)?,
            Table::try_from(&self.supervisor)?,
            Table::try_from(&self.egress)?,
            Table::try_from(&self.publication)?,
        ];
        let mut entries = BTreeMap::new();
        for ((table, keys), values) in MANAGED.iter().zip(tables) {
            for key in *keys {
                leaves(&format!("{table}.{key}"), values.get(*key), &mut entries);
            }
        }
        Ok(entries)
    }
}

/// Adds `value` under `name`, or each entry of a table under its own name.
fn leaves(name: &str, value: Option<&Value>, entries: &mut BTreeMap<String, Value>) {
    match value {
        Some(Value::Table(table)) => {
            for (key, inner) in table {
                leaves(&format!("{name}.{key}"), Some(inner), entries);
            }
        }
        Some(value) => {
            entries.insert(name.to_owned(), value.clone());
        }
        None => {}
    }
}

/// The settings of a host laid out as `layout` for the owner's `inputs`.
pub fn settings(layout: &Layout, inputs: &Inputs, sids: &Sids) -> Settings {
    Settings {
        runner: runner(layout, sids),
        supervisor: supervisor(layout, inputs, sids),
        egress: egress(layout, sids),
        publication: publication(layout, inputs, sids),
    }
}

/// A pipe's name: the layout's prefix and `rest`.
fn pipe(layout: &Layout, rest: &str) -> String {
    format!("{}-{rest}", layout.prefix)
}

/// The runner serves the supervisor and admits launches through both brokers.
fn runner(layout: &Layout, sids: &Sids) -> ServiceSettings {
    ServiceSettings {
        name: layout.service(Role::Runner),
        state_dir: PathBuf::from(layout.state("runner")),
        pipe_name: pipe(layout, "runner"),
        client_sid: sids.supervisor.clone(),
        egress_control_pipe: pipe(layout, "egress-control"),
        egress_broker_sid: sids.egress.clone(),
        publication_control_pipe: pipe(layout, "publication-control"),
        publication_broker_sid: sids.publication.clone(),
        ..ServiceSettings::default()
    }
}

/// The supervisor trusts the runner's pipe and answers the publication broker.
fn supervisor(layout: &Layout, inputs: &Inputs, sids: &Sids) -> SupervisorSettings {
    let mut settings = SupervisorSettings {
        name: layout.service(Role::Supervisor),
        state_dir: PathBuf::from(layout.state("supervisor")),
        runner_pipe: pipe(layout, "runner"),
        runner_sid: sids.runner.clone(),
        authority_pipe: pipe(layout, "supervisor-authority"),
        publication_broker_sid: sids.publication.clone(),
        ..SupervisorSettings::default()
    };
    settings.stand_in.enabled = !inputs.stand_in_fixture.is_empty();
    settings.stand_in.fixture = inputs.stand_in_fixture.clone();
    settings.stand_in.queue_dir = PathBuf::from(layout.state(r"stand-in\queue"));
    settings.stand_in.ledger = PathBuf::from(layout.state(r"stand-in\ledger.jsonl"));
    settings.stand_in.launch_root = PathBuf::from(layout.state(r"runner\stand-in-launches"));
    settings.coordinator = coordinator(layout, inputs);
    settings
}

/// The coordinator the owner asked for, with its files where the layout
/// puts them; each path is written only when it is not the in-code default.
/// Without a coordinator every entry is the default, so nothing of it is
/// written whatever the layout.
fn coordinator(layout: &Layout, inputs: &Inputs) -> CoordinatorSettings {
    if !inputs.coordinator() {
        return CoordinatorSettings::default();
    }
    CoordinatorSettings {
        enabled: true,
        service_url: inputs.coordinator_url.clone(),
        project_id: inputs.coordinator_project.clone(),
        cli: PathBuf::from(layout.cli()),
        directory: PathBuf::from(layout.coordinator_dir()),
        fixture: inputs.coordinator_fixture.clone(),
        launch_root: PathBuf::from(layout.coordinator_launch_root()),
        ledger: PathBuf::from(layout.state(r"supervisor\ledger.jsonl")),
    }
}

/// The egress broker serves the runner and must not reach the push key.
fn egress(layout: &Layout, sids: &Sids) -> EgressSettings {
    EgressSettings {
        name: layout.service(Role::Egress),
        state_dir: PathBuf::from(layout.state("egress")),
        control_pipe: pipe(layout, "egress-control"),
        runner_sid: sids.runner.clone(),
        must_not_open: vec![PathBuf::from(layout.key())],
    }
}

/// The publication broker serves the runner, asks the supervisor and
/// publishes to the owner's remote.
fn publication(layout: &Layout, inputs: &Inputs, sids: &Sids) -> PublicationSettings {
    PublicationSettings {
        name: layout.service(Role::Publication),
        state_dir: PathBuf::from(layout.state("publication")),
        control_pipe: pipe(layout, "publication-control"),
        runner_sid: sids.runner.clone(),
        remote: inputs.remote.clone(),
        authority_pipe: pipe(layout, "supervisor-authority"),
        supervisor_sid: sids.supervisor.clone(),
        app_id: inputs.app_id,
        installation_id: inputs.installation_id,
        private_key: PathBuf::from(layout.key()),
        ..PublicationSettings::default()
    }
}

/// The managed entries whose value in `observed` (a configuration file's
/// text, or none) is not what `wanted` says, by dotted name. An error when
/// the text is not a configuration the services could load.
pub fn changed(observed: Option<&str>, wanted: &Settings) -> Result<Vec<String>> {
    let Some(text) = observed else {
        return Ok(vec!["the whole file".into()]);
    };
    let seen = Settings::parse(text)?.managed()?;
    let wanted = wanted.managed()?;
    let differs = |(name, value): (&String, &Value)| seen.get(name) != Some(value);
    Ok(wanted
        .iter()
        .filter(|e| differs(*e))
        .map(|e| e.0.clone())
        .collect())
}

/// The text of a configuration file that carries `wanted`: `observed`'s
/// own entries with every managed one set to its wanted value, or left out
/// when that is the default.
pub fn merged(observed: Option<&str>, wanted: &Settings) -> Result<String> {
    let mut file: Table = match observed {
        Some(text) => text.parse().context("the configuration is not TOML")?,
        None => Table::new(),
    };
    let (wanted, defaults) = (wanted.managed()?, Settings::default().managed()?);
    for (table, keys) in MANAGED {
        clear(&mut file, table, keys);
    }
    for (name, value) in wanted.iter().filter(|(k, v)| defaults.get(*k) != Some(*v)) {
        set(&mut file, name, value.clone());
    }
    Ok(format!("{HEADER}{}", toml::to_string(&file)?))
}

/// Removes the managed `keys` of `table` from `file`, and the table itself
/// when nothing of the owner's is left in it.
fn clear(file: &mut Table, table: &str, keys: &[&str]) {
    let Some(Value::Table(entries)) = file.get_mut(table) else {
        return;
    };
    for key in keys {
        entries.remove(*key);
    }
    if entries.is_empty() {
        file.remove(table);
    }
}

/// Sets the entry with the dotted `name` in `file`, making its tables.
fn set(file: &mut Table, name: &str, value: Value) {
    let mut parts: Vec<&str> = name.split('.').collect();
    let last = parts.pop().unwrap_or(name);
    let mut table = file;
    for part in parts {
        let inner = table
            .entry(part.to_owned())
            .or_insert_with(|| Value::Table(Table::new()));
        let Value::Table(next) = inner else { return };
        table = next;
    }
    table.insert(last.to_owned(), value);
}
