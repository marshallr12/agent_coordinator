//! What a host looks like now, as far as the installer may look. A host
//! that fails an early check is not inspected further: nothing below a
//! link, or below a directory someone else owns, is ever read.
use super::access::AccessList;
use super::model::ServiceSpec;
use std::collections::BTreeMap;

/// What is at a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathState {
    /// Nothing.
    Missing,
    /// A plain directory.
    Directory {
        /// Its owner, as SID text.
        owner: String,
        /// Its access list.
        access: AccessList,
    },
    /// A plain file.
    File {
        /// Its owner, as SID text.
        owner: String,
        /// Its access list.
        access: AccessList,
    },
    /// A symbolic link, a junction or any other reparse point.
    Link,
}

/// A registered service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceState {
    /// How it is registered.
    pub spec: ServiceSpec,
    /// Its process, while it has one.
    pub process: Option<u32>,
}

/// The user profile Windows keeps for a service's virtual account once the
/// service has run: its directory under `C:\Windows\ServiceProfiles\` and
/// its hive. An AppContainer profile a launch made lives in it, because
/// the runner made it for its own account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileState {
    /// Windows has no profile for the account.
    Absent,
    /// The profile exists and its hive is not loaded: it can be deleted.
    Unloaded,
    /// The hive is loaded, as Windows keeps it after the service stopped,
    /// often until a restart; it cannot be deleted yet.
    Loaded,
}

/// This run's file for a secret, the key or the coordinator credential, as
/// far as the installer could tell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
    /// No file was given.
    NotGiven,
    /// The file cannot be used; the reason names the path and never what
    /// the file holds.
    Unreadable(String),
    /// A plain file the installer can read.
    Readable {
        /// Whether the installed secret already holds the same bytes.
        installed: bool,
    },
}

/// One host, as observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    /// Whether this process is elevated. When it is not, nothing else was
    /// looked at.
    pub elevated: bool,
    /// Every modeled directory, and the key and credential files, that
    /// could be looked at without passing through something refused.
    pub paths: BTreeMap<String, PathState>,
    /// Every modeled service: how it is registered, or `None`.
    pub services: BTreeMap<String, Option<ServiceState>>,
    /// The installed binary's SHA-256 in hex, when there is one.
    pub binary_sha256: Option<String>,
    /// The configuration's text, when there is one.
    pub config: Option<String>,
    /// The manifest's text, when there is one.
    pub manifest: Option<String>,
    /// The release record's text, when there is one.
    pub releases: Option<String>,
    /// Whether an update or rollback has not finished.
    pub update_open: bool,
    /// This run's key file.
    pub key_source: KeySource,
    /// This run's coordinator credential file.
    pub credential_source: KeySource,
    /// The installed coordinator CLI's SHA-256 in hex, when there is one.
    pub cli_sha256: Option<String>,
}

impl Observed {
    /// A host nothing was read from.
    pub fn unread(elevated: bool) -> Self {
        Self {
            elevated,
            paths: BTreeMap::new(),
            services: BTreeMap::new(),
            binary_sha256: None,
            config: None,
            manifest: None,
            releases: None,
            update_open: false,
            key_source: KeySource::NotGiven,
            credential_source: KeySource::NotGiven,
            cli_sha256: None,
        }
    }

    /// What is at `path`; a path that was not looked at reads as missing.
    pub fn path(&self, path: &str) -> &PathState {
        self.paths.get(path).unwrap_or(&PathState::Missing)
    }

    /// The service called `name`, when it is registered.
    pub fn service(&self, name: &str) -> Option<&ServiceState> {
        self.services.get(name).and_then(Option::as_ref)
    }
}
