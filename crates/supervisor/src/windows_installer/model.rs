//! The desired state of an installed host: one binary, four services, a
//! state tree with its access lists, a generated configuration, the record
//! of what the owner asked for and, when there is a push App, its key.
//!
//! Everything is derived from a [`Layout`] (where things go) and the
//! owner's [`Inputs`] (what has no default). Paths are Windows paths kept
//! as text, so the model reads the same on every platform.
use super::access::{ADMINISTRATORS, AccessList, FULL, MODIFY, READ, READ_EXECUTE, SYSTEM, USERS};
use super::config_text::{self, Settings};
use super::service_sid::service_sid;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

/// Where the binary and the configuration go by default.
pub const PROGRAM_DIR: &str = r"C:\Program Files\agentc";
/// Where the services keep their state by default.
pub const STATE_ROOT: &str = r"C:\ProgramData\agentc";
/// What every service's name starts with by default.
pub const PREFIX: &str = "agentc";
/// The installed binary's file name.
pub const BINARY: &str = "agentc-supervisor.exe";
/// The generated configuration's file name.
pub const CONFIG: &str = "supervisor.toml";

/// The privileges a service's token may hold, and no others. Every service
/// serves a named pipe and identifies its client by impersonating it, which
/// is why `SeImpersonatePrivilege` stays; `SeChangeNotifyPrivilege` is what
/// lets any process walk a directory path. Whether impersonation at
/// identification level would do without the privilege is not yet tested.
pub const REQUIRED_PRIVILEGES: [&str; 2] = ["SeChangeNotifyPrivilege", "SeImpersonatePrivilege"];
/// Seconds without a failure after which Windows forgets earlier failures.
pub const RECOVERY_RESET_SECONDS: u32 = 86_400;
/// Milliseconds before each of the two restarts.
pub const RESTART_DELAY_MS: u32 = 60_000;
/// Milliseconds the Service Control Manager gives a service to stop at
/// preshutdown: the runner's `preshutdown_timeout_seconds` default.
pub const PRESHUTDOWN_MS: u32 = 120_000;

/// Where an installation lives. The defaults are the product's; the others
/// exist so a test can install beside a real host without touching it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The directory of the binary and the configuration.
    pub program_dir: String,
    /// The root of the services' state tree.
    pub state_root: String,
    /// What every service's name, and every pipe's, starts with.
    pub prefix: String,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            program_dir: PROGRAM_DIR.into(),
            state_root: STATE_ROOT.into(),
            prefix: PREFIX.into(),
        }
    }
}

impl Layout {
    /// Refuses a layout that could name something outside its two trees.
    pub fn validate(&self) -> Result<()> {
        absolute(&self.program_dir).context("the program directory")?;
        absolute(&self.state_root).context("the state directory")?;
        let plain = |c: char| c.is_ascii_alphanumeric() || c == '-';
        ensure!(
            !self.prefix.is_empty() && self.prefix.len() <= 32 && self.prefix.chars().all(plain),
            "the service prefix {:?} is not 1 to 32 letters, digits and hyphens",
            self.prefix
        );
        Ok(())
    }

    /// `rest` below the state root.
    pub fn state(&self, rest: &str) -> String {
        format!(r"{}\{rest}", self.state_root)
    }

    /// `rest` below the program directory.
    pub fn program(&self, rest: &str) -> String {
        format!(r"{}\{rest}", self.program_dir)
    }

    /// The installed binary.
    pub fn binary(&self) -> String {
        self.program(BINARY)
    }

    /// The generated configuration.
    pub fn config(&self) -> String {
        self.program(CONFIG)
    }

    /// The installer's own directory: the system and administrators only.
    pub fn installer_dir(&self) -> String {
        self.state("installer")
    }

    /// The record of what the owner asked for.
    pub fn manifest(&self) -> String {
        self.state(r"installer\manifest.json")
    }

    /// The journal of applied changes.
    pub fn journal(&self) -> String {
        self.state(r"installer\journal.jsonl")
    }

    /// The directory only the publication account can read.
    pub fn key_dir(&self) -> String {
        self.state("publication-key")
    }

    /// The push App's key.
    pub fn key(&self) -> String {
        self.state(r"publication-key\push-app.pem")
    }

    /// The name of `role`'s service.
    pub fn service(&self, role: Role) -> String {
        format!("{}-{}", self.prefix, role.suffix())
    }
}

/// Refuses `path` unless it is a drive path with plain components.
fn absolute(path: &str) -> Result<()> {
    let bytes = path.as_bytes();
    let drive = bytes.len() > 3 && bytes[0].is_ascii_alphabetic() && &bytes[1..3] == b":\\";
    ensure!(drive, "{path:?} is not a path on a drive, such as C:\\dir");
    let rest = &path[3..];
    let odd = |part: &str| part.is_empty() || part == "." || part == ".." || part.ends_with(' ');
    ensure!(
        !rest.split('\\').any(odd),
        "{path:?} has an empty or relative component"
    );
    // A quote would end the service's command line early; the others name
    // a stream, a wildcard or another separator.
    let reserved = ['/', '"', ':', '*', '?', '<', '>', '|'];
    ensure!(
        !rest.contains(reserved),
        "{path:?} holds a reserved character"
    );
    Ok(())
}

/// One of the four services, in the order they start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The egress broker.
    Egress,
    /// The publication broker.
    Publication,
    /// The role runner.
    Runner,
    /// The supervisor.
    Supervisor,
}

impl Role {
    /// Every role, in start order.
    pub const ALL: [Role; 4] = [
        Role::Egress,
        Role::Publication,
        Role::Runner,
        Role::Supervisor,
    ];

    /// The end of the service's name, and its `--role` argument.
    pub fn suffix(self) -> &'static str {
        match self {
            Role::Egress => "egress",
            Role::Publication => "publication",
            Role::Runner => "runner",
            Role::Supervisor => "supervisor",
        }
    }
}

/// The four services' SIDs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sids {
    /// The egress broker's.
    pub egress: String,
    /// The publication broker's.
    pub publication: String,
    /// The runner's.
    pub runner: String,
    /// The supervisor's.
    pub supervisor: String,
}

impl Sids {
    /// The SIDs Windows derives from the layout's service names.
    pub fn of(layout: &Layout) -> Self {
        let sid = |role| service_sid(&layout.service(role));
        Self {
            egress: sid(Role::Egress),
            publication: sid(Role::Publication),
            runner: sid(Role::Runner),
            supervisor: sid(Role::Supervisor),
        }
    }

    /// The SID of `role`'s service.
    pub fn get(&self, role: Role) -> &str {
        match role {
            Role::Egress => &self.egress,
            Role::Publication => &self.publication,
            Role::Runner => &self.runner,
            Role::Supervisor => &self.supervisor,
        }
    }
}

/// When Windows starts a service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartType {
    /// At boot, without a logon.
    Auto,
    /// Only when asked. The default: no provider harness runs on Windows
    /// yet, so nothing needs the services at boot.
    #[default]
    Demand,
    /// Never.
    Disabled,
    /// A start type no service of ours has (boot or system).
    Other,
}

/// What the owner decides: everything about a host that has no default.
/// `install` records it as the manifest, so `status` and a later `install`
/// need no arguments.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Inputs {
    /// The one remote candidates are published to.
    pub remote: String,
    /// The push App's id; 0 for none.
    pub app_id: u64,
    /// The push App's installation on the repository; set with `app_id`.
    pub installation_id: u64,
    /// When Windows starts the services.
    pub start: StartType,
    /// The acceptance stand-in's command; empty leaves the stand-in off.
    pub stand_in_fixture: Vec<String>,
    /// Whether the push App's key is installed. It goes with `app_id`.
    pub push_key: bool,
}

/// What one `install` run was told on its command line. Each `None` keeps
/// what the manifest records, or the default on a first install.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Overrides {
    /// The remote candidates are published to.
    pub remote: Option<String>,
    /// The push App's id and its installation.
    pub app: Option<(u64, u64)>,
    /// This run's key file. Giving one also asks for the key to be installed.
    pub key_source: Option<String>,
    /// When Windows starts the services.
    pub start: Option<StartType>,
    /// The stand-in's command; an empty one turns the stand-in off.
    pub stand_in_fixture: Option<Vec<String>>,
}

impl Inputs {
    /// These inputs with what one run's command line says instead.
    pub fn with(mut self, overrides: &Overrides) -> Self {
        let Overrides {
            remote,
            app,
            key_source,
            start,
            stand_in_fixture,
        } = overrides;
        self.remote = remote.clone().unwrap_or(self.remote);
        (self.app_id, self.installation_id) = app.unwrap_or((self.app_id, self.installation_id));
        self.push_key |= key_source.is_some();
        self.start = start.unwrap_or(self.start);
        self.stand_in_fixture = stand_in_fixture.clone().unwrap_or(self.stand_in_fixture);
        self
    }

    /// Refuses inputs no host could run with.
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.remote.is_empty(),
            "no remote is set: the publication broker has no default (--remote)"
        );
        ensure!(
            matches!(self.start, StartType::Auto | StartType::Demand),
            "the services start on demand or automatically, nothing else"
        );
        ensure!(
            (self.app_id != 0) == self.push_key,
            "a push App and its key go together: give --app-id, --installation-id and \
             --key-source, or none of them"
        );
        Ok(())
    }

    /// The manifest's text.
    pub fn manifest(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)? + "\n")
    }

    /// The inputs a manifest holds.
    pub fn from_manifest(text: &str) -> Result<Self> {
        serde_json::from_str(text).context("the manifest is not a record of install inputs")
    }
}

/// One step of a service's recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAction {
    /// Restart the service after this many milliseconds.
    Restart(u32),
    /// Do nothing.
    None,
    /// Restart the machine or run a command: never ours.
    Other(u32),
}

/// What Windows does when a service fails.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Recovery {
    /// Seconds without a failure after which the count starts again.
    pub reset_seconds: u32,
    /// What each failure in turn does.
    pub actions: Vec<RecoveryAction>,
    /// A program Windows would run on failure. Always empty for ours: the
    /// installer never has Windows run a program it was given.
    pub command: String,
}

impl Recovery {
    /// Two restarts a minute apart, then none.
    pub fn bounded() -> Self {
        let restart = RecoveryAction::Restart(RESTART_DELAY_MS);
        Self {
            reset_seconds: RECOVERY_RESET_SECONDS,
            actions: vec![restart, restart, RecoveryAction::None],
            command: String::new(),
        }
    }
}

/// How one service is registered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServiceSpec {
    /// The service's name.
    pub name: String,
    /// The account it runs as: its own virtual account.
    pub account: String,
    /// The command line Windows starts.
    pub bin_path: String,
    /// When Windows starts it.
    pub start: StartType,
    /// The services that must run first.
    pub dependencies: Vec<String>,
    /// What Windows does when it fails.
    pub recovery: Recovery,
    /// Milliseconds it gets to stop at preshutdown.
    pub preshutdown_ms: u32,
    /// The only privileges its token may hold.
    pub privileges: Vec<String>,
}

/// One directory and the access list it must have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectorySpec {
    /// The directory.
    pub path: String,
    /// Its whole access list.
    pub access: AccessList,
    /// Who may own it besides the system and administrators: the service
    /// it belongs to, which creates it when it starts first.
    pub owners: Vec<String>,
}

/// The push App's key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySpec {
    /// Where the key is installed.
    pub path: String,
    /// The file's whole access list.
    pub access: AccessList,
    /// The file this run copies the key from, when one was given. It is a
    /// path; the model never holds the key.
    pub source: Option<String>,
}

/// Everything an installed host has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Desired {
    /// Where it all goes.
    pub layout: Layout,
    /// What the owner asked for.
    pub inputs: Inputs,
    /// Every directory, parents before children.
    pub directories: Vec<DirectorySpec>,
    /// The four services, in start order.
    pub services: Vec<ServiceSpec>,
    /// The SHA-256 of the binary being installed, in hex.
    pub binary_sha256: String,
    /// The source commit of the binary being installed: the running one's.
    pub commit: String,
    /// The settings the generated configuration must carry.
    pub settings: Settings,
    /// The manifest's text.
    pub manifest: String,
    /// The key, when there is a push App.
    pub key: Option<KeySpec>,
}

impl Desired {
    /// The desired state for `layout` and `inputs`. `binary_sha256` is the
    /// digest of the binary to install and `key_source` this run's key
    /// file, if one was given.
    pub fn new(
        layout: Layout,
        inputs: Inputs,
        binary_sha256: String,
        key_source: Option<String>,
    ) -> Result<Self> {
        layout.validate()?;
        inputs.validate()?;
        let sids = Sids::of(&layout);
        let settings = config_text::settings(&layout, &inputs, &sids);
        settings.validate()?;
        Ok(Self {
            directories: directories(&layout, &inputs, &sids),
            services: Role::ALL.map(|role| service(&layout, &inputs, role)).into(),
            key: key(&layout, &inputs, &sids, key_source),
            manifest: inputs.manifest()?,
            commit: coordinator_core::BUILD_SOURCE_COMMIT.to_owned(),
            binary_sha256,
            settings,
            layout,
            inputs,
        })
    }

    /// The directory spec for `path`, if the model has one.
    pub fn directory(&self, path: &str) -> Option<&DirectorySpec> {
        self.directories.iter().find(|d| d.path == path)
    }

    /// The release being installed.
    pub fn release(&self) -> super::release::Release {
        super::release::Release {
            sha256: self.binary_sha256.clone(),
            commit: self.commit.clone(),
        }
    }

    /// The service spec called `name`, if the model has one.
    pub fn service(&self, name: &str) -> Option<&ServiceSpec> {
        self.services.iter().find(|s| s.name == name)
    }
}

/// How `role`'s service is registered.
fn service(layout: &Layout, inputs: &Inputs, role: Role) -> ServiceSpec {
    let name = layout.service(role);
    let depends = (role == Role::Supervisor).then(|| layout.service(Role::Runner));
    ServiceSpec {
        account: format!(r"NT SERVICE\{name}"),
        bin_path: bin_path(layout, role),
        start: inputs.start,
        dependencies: depends.into_iter().collect(),
        recovery: Recovery::bounded(),
        preshutdown_ms: PRESHUTDOWN_MS,
        privileges: REQUIRED_PRIVILEGES.map(String::from).into(),
        name,
    }
}

/// The command line of `role`'s service: always the installed binary with
/// the installed configuration, never anything a caller supplied.
fn bin_path(layout: &Layout, role: Role) -> String {
    let (binary, config) = (layout.binary(), layout.config());
    let base = format!(r#""{binary}" --config "{config}" windows-service"#);
    match role {
        Role::Runner => base,
        other => format!("{base} --role {}", other.suffix()),
    }
}

/// A directory the system and administrators control, with `extra` grants.
fn directory(path: String, extra: &[(&str, u32)], owners: &[&str]) -> DirectorySpec {
    let mut grants = vec![(SYSTEM, FULL), (ADMINISTRATORS, FULL)];
    grants.extend_from_slice(extra);
    DirectorySpec {
        path,
        access: AccessList::directory(&grants),
        owners: owners.iter().map(|sid| (*sid).to_owned()).collect(),
    }
}

/// Every directory of the host, parents first. The runner and the
/// supervisor may modify their own trees; a broker has full control of its
/// own directory, which it needs to set that directory's access list again
/// at every start. Users may read the program directory; nothing else is
/// granted to anyone.
fn directories(layout: &Layout, inputs: &Inputs, sids: &Sids) -> Vec<DirectorySpec> {
    let (runner, supervisor) = (sids.runner.as_str(), sids.supervisor.as_str());
    let (egress, publication) = (sids.egress.as_str(), sids.publication.as_str());
    let owned = |rest: &str, sid, mask| directory(layout.state(rest), &[(sid, mask)], &[sid]);
    let mut all = vec![
        directory(layout.program_dir.clone(), &[(USERS, READ_EXECUTE)], &[]),
        directory(layout.state_root.clone(), &[], &[]),
        directory(layout.installer_dir(), &[], &[]),
        owned("runner", runner, MODIFY),
        owned(r"runner\stand-in-launches", runner, MODIFY),
        owned("supervisor", supervisor, MODIFY),
        owned("stand-in", supervisor, MODIFY),
        owned(r"stand-in\queue", supervisor, MODIFY),
        owned("egress", egress, FULL),
        owned("publication", publication, FULL),
    ];
    if inputs.push_key {
        let grant = [(publication, READ_EXECUTE)];
        all.push(directory(layout.key_dir(), &grant, &[]));
    }
    all
}

/// The key's place and access list: the publication account may read it,
/// the system and administrators have full control, nobody else anything.
fn key(layout: &Layout, inputs: &Inputs, sids: &Sids, source: Option<String>) -> Option<KeySpec> {
    let grants = [
        (SYSTEM, FULL),
        (ADMINISTRATORS, FULL),
        (sids.publication.as_str(), READ),
    ];
    inputs.push_key.then(|| KeySpec {
        path: layout.key(),
        access: AccessList::file(&grants),
        source,
    })
}
