//! The difference between a host as observed and the desired state: either
//! the reasons the installer must leave the host alone, or the changes that
//! make it the desired one. Each kind of drift has a change of its own, so
//! a repair touches only what drifted.
use super::access::{ADMINISTRATORS, SYSTEM};
use super::config_text;
use super::model::{Desired, KeySpec, ServiceSpec};
use super::observed::{KeySource, Observed, PathState, ServiceState};
use super::release::Releases;
use serde::Serialize;
use std::fmt;

/// One change to a host. It names what to change; the values come from the
/// desired state when it is applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// Make a directory with its access list.
    CreateDirectory { path: String },
    /// Replace the access list of a directory, the key or the credential.
    SetAccessList { path: String },
    /// Copy the binary into the program directory.
    InstallBinary,
    /// Record what the owner asked for.
    WriteManifest,
    /// Record the installed release as the current one.
    RecordRelease,
    /// Write the configuration; `entries` are the managed ones that differ.
    WriteConfig { entries: Vec<String> },
    /// Copy the push App's key from this run's key file.
    InstallKey,
    /// Copy the coordinator credential from this run's credential file.
    InstallCredential,
    /// Copy the coordinator CLI from this run's CLI file.
    InstallCli,
    /// Register a service, stopped.
    CreateService { service: String },
    /// Set a service's command line.
    SetBinPath { service: String },
    /// Set when Windows starts a service.
    SetStartType { service: String },
    /// Set the services a service waits for.
    SetDependencies { service: String },
    /// Set the account a service runs as.
    SetAccount { service: String },
    /// Set what Windows does when a service fails.
    SetRecovery { service: String },
    /// Set the time a service gets to stop at preshutdown.
    SetPreshutdown { service: String },
    /// Set the only privileges a service's token may hold.
    SetPrivileges { service: String },
}

impl Action {
    /// The service a service change names; `None` for every other change.
    pub fn service(&self) -> Option<&str> {
        match self {
            Self::CreateService { service }
            | Self::SetBinPath { service }
            | Self::SetStartType { service }
            | Self::SetDependencies { service }
            | Self::SetAccount { service }
            | Self::SetRecovery { service }
            | Self::SetPreshutdown { service }
            | Self::SetPrivileges { service } => Some(service),
            _ => None,
        }
    }
}

/// A change and why it is needed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    /// What to change.
    #[serde(flatten)]
    pub action: Action,
    /// What was found and what is wanted.
    pub detail: String,
}

/// A reason the installer changes nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// This process is not elevated.
    NotElevated,
    /// The path is a link or another reparse point.
    Link(String),
    /// A file is where a directory belongs.
    NotADirectory(String),
    /// A directory is where the key file belongs.
    NotAFile(String),
    /// Someone the installer does not trust owns the path.
    ForeignOwner { path: String, owner: String },
    /// This run's key file cannot be used.
    KeySource(String),
    /// A key is wanted, none is installed and no key file was given.
    NoKeySource(String),
    /// This run's coordinator credential file cannot be used.
    CredentialSource(String),
    /// A credential is wanted, none is installed and no file was given.
    NoCredentialSource(String),
    /// This run's coordinator CLI file cannot be used.
    CliSource(String),
    /// The installed coordinator CLI is not the one the manifest records,
    /// and no CLI file was given to put it right.
    CliDiffers {
        path: String,
        installed: String,
        wanted: String,
    },
    /// The configuration is not one the services could load.
    ConfigUnreadable(String),
    /// The installed binary is another release: replacing it is an update.
    BinaryDiffers { installed: String, wanted: String },
    /// An update or rollback has not finished.
    UpdateOpen(String),
    /// The release record is not one the installer wrote.
    ReleasesUnreadable(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotElevated => write!(f, "this session is not elevated; run it as administrator"),
            Self::Link(path) => write!(f, "{path} is a link or reparse point"),
            Self::NotADirectory(path) => write!(f, "{path} is not a directory"),
            Self::NotAFile(path) => write!(f, "{path} is not a file"),
            Self::ForeignOwner { path, owner } => write!(
                f,
                "{path} is owned by {owner}, not the system, administrators or its service"
            ),
            Self::KeySource(reason) => write!(f, "the key file cannot be used: {reason}"),
            Self::NoKeySource(path) => {
                write!(
                    f,
                    "no key is installed at {path} and no --key-source was given"
                )
            }
            Self::CredentialSource(reason) => {
                write!(
                    f,
                    "the coordinator credential file cannot be used: {reason}"
                )
            }
            Self::NoCredentialSource(path) => write!(
                f,
                "no coordinator credential is installed at {path} and no \
                 --coordinator-credential-source was given"
            ),
            Self::CliSource(reason) => {
                write!(f, "the coordinator CLI file cannot be used: {reason}")
            }
            Self::CliDiffers {
                path,
                installed,
                wanted,
            } => write!(
                f,
                "the coordinator CLI at {path} has SHA-256 {installed}, not the recorded \
                 {wanted}, and no --coordinator-cli-source was given"
            ),
            Self::ConfigUnreadable(reason) => write!(
                f,
                "the configuration cannot be loaded, so it is left alone: {reason}"
            ),
            Self::BinaryDiffers { installed, wanted } => write!(
                f,
                "the installed binary has SHA-256 {installed}, not {wanted}; replacing an \
                 installed release is `windows-host update`, not an install"
            ),
            Self::UpdateOpen(path) => write!(
                f,
                "an update or rollback has not finished ({path}); run `windows-host update` \
                 or `windows-host rollback` to finish or undo it"
            ),
            Self::ReleasesUnreadable(reason) => {
                write!(f, "the release record cannot be read: {reason}")
            }
        }
    }
}

/// The changes that make `observed` the `desired` host, or why nothing
/// may be changed.
pub fn plan(desired: &Desired, observed: &Observed) -> Result<Vec<Change>, Vec<Refusal>> {
    let refusals = refusals(desired, observed);
    if !refusals.is_empty() {
        return Err(refusals);
    }
    let mut changes = Vec::new();
    directories(desired, observed, &mut changes);
    files(desired, observed, &mut changes);
    secrets(desired, observed, &mut changes);
    cli(desired, observed, &mut changes);
    for service in &desired.services {
        service_changes(service, observed.service(&service.name), &mut changes);
    }
    Ok(changes)
}

/// Every reason to leave the host alone.
pub(super) fn refusals(desired: &Desired, observed: &Observed) -> Vec<Refusal> {
    if !observed.elevated {
        return vec![Refusal::NotElevated];
    }
    let mut found = Vec::new();
    for directory in &desired.directories {
        let state = observed.path(&directory.path);
        found.extend(directory_refusal(&directory.path, &directory.owners, state));
    }
    let key = (desired.key.as_ref(), &observed.key_source);
    found.extend(secret_refusals(key, observed, &KEY));
    let credential = (desired.credential.as_ref(), &observed.credential_source);
    found.extend(secret_refusals(credential, observed, &CREDENTIAL));
    found.extend(config_refusal(desired, observed));
    found.extend(binary_refusal(desired, observed));
    found.extend(cli_refusal(desired, observed));
    found.extend(release_refusals(desired, observed));
    found
}

/// An unfinished update, and a release record that cannot be read, are
/// left for the owner.
fn release_refusals(desired: &Desired, observed: &Observed) -> Vec<Refusal> {
    let mut found = Vec::new();
    if observed.update_open {
        found.push(Refusal::UpdateOpen(desired.layout.update_record()));
    }
    if let Some(Err(error)) = observed.releases.as_deref().map(Releases::parse) {
        found.push(Refusal::ReleasesUnreadable(format!("{error:#}")));
    }
    found
}

/// Whether `owner` is the system, administrators or one of `others`.
pub(super) fn trusted(owner: &str, others: &[String]) -> bool {
    owner == SYSTEM || owner == ADMINISTRATORS || others.iter().any(|sid| sid == owner)
}

/// Why the directory at `path` cannot be adopted, if it cannot.
pub(super) fn directory_refusal(
    path: &str,
    owners: &[String],
    state: &PathState,
) -> Option<Refusal> {
    match state {
        PathState::Missing => None,
        PathState::Link => Some(Refusal::Link(path.to_owned())),
        PathState::File { .. } => Some(Refusal::NotADirectory(path.to_owned())),
        PathState::Directory { owner, .. } if trusted(owner, owners) => None,
        PathState::Directory { owner, .. } => Some(Refusal::ForeignOwner {
            path: path.to_owned(),
            owner: owner.clone(),
        }),
    }
}

/// A secret file the model wants, the key or the credential, if it wants
/// one, and what this run's file for it is like.
type Secret<'a> = (Option<&'a KeySpec>, &'a KeySource);

/// The refusals of one kind of secret file that name it.
struct SecretRefusals {
    /// None is installed and no file was given; given the installed path.
    missing: fn(String) -> Refusal,
    /// This run's file cannot be used; given the reason.
    unusable: fn(String) -> Refusal,
}

/// The push App's key's.
const KEY: SecretRefusals = SecretRefusals {
    missing: Refusal::NoKeySource,
    unusable: Refusal::KeySource,
};
/// The coordinator credential's.
const CREDENTIAL: SecretRefusals = SecretRefusals {
    missing: Refusal::NoCredentialSource,
    unusable: Refusal::CredentialSource,
};

/// Why a secret file or this run's file for it cannot be used, if either
/// cannot.
fn secret_refusals(secret: Secret, observed: &Observed, kind: &SecretRefusals) -> Vec<Refusal> {
    let (Some(spec), source) = secret else {
        return Vec::new();
    };
    let path = || spec.path.clone();
    let mut found = Vec::new();
    match (observed.path(&spec.path), source) {
        (PathState::Link, _) => found.push(Refusal::Link(path())),
        (PathState::Directory { .. }, _) => found.push(Refusal::NotAFile(path())),
        (PathState::File { owner, .. }, _) if !trusted(owner, &[]) => {
            let owner = owner.clone();
            found.push(Refusal::ForeignOwner {
                path: path(),
                owner,
            });
        }
        (PathState::Missing, KeySource::NotGiven) => found.push((kind.missing)(path())),
        _ => {}
    }
    if let KeySource::Unreadable(reason) = source {
        found.push((kind.unusable)(reason.clone()));
    }
    found
}

/// A configuration file the services could not load is the owner's to fix.
fn config_refusal(desired: &Desired, observed: &Observed) -> Option<Refusal> {
    let unreadable = config_text::changed(observed.config.as_deref(), &desired.settings).err()?;
    Some(Refusal::ConfigUnreadable(format!("{unreadable:#}")))
}

/// A binary is installed once: another release replaces it only through an
/// update, which keeps the one it replaces and can go back to it.
fn binary_refusal(desired: &Desired, observed: &Observed) -> Option<Refusal> {
    let installed = observed.binary_sha256.as_ref()?;
    (*installed != desired.binary_sha256).then(|| Refusal::BinaryDiffers {
        installed: installed.clone(),
        wanted: desired.binary_sha256.clone(),
    })
}

/// The coordinator CLI is installed from a file this run was given: one
/// that is not the CLI the manifest records, with no file to put it right,
/// is the owner's to fix, as another release's binary is.
fn cli_refusal(desired: &Desired, observed: &Observed) -> Option<Refusal> {
    let cli = desired.cli.as_ref()?;
    if cli.source.is_some() || observed.cli_sha256.as_ref() == Some(&cli.sha256) {
        return None;
    }
    Some(Refusal::CliDiffers {
        path: cli.path.clone(),
        installed: observed.cli_sha256.clone().unwrap_or_else(|| "none".into()),
        wanted: cli.sha256.clone(),
    })
}

/// Adds `action` with its `detail`.
fn push(changes: &mut Vec<Change>, action: Action, detail: String) {
    changes.push(Change { action, detail });
}

/// A missing directory is made; one whose access list differs gets its own.
fn directories(desired: &Desired, observed: &Observed, changes: &mut Vec<Change>) {
    for directory in &desired.directories {
        let path = directory.path.clone();
        let wanted = directory.access.describe();
        match observed.path(&directory.path) {
            PathState::Directory { access, .. } if *access == directory.access => {}
            PathState::Directory { access, .. } => {
                let detail = format!("found {}; wanted {wanted}", access.describe());
                push(changes, Action::SetAccessList { path }, detail);
            }
            _ => push(
                changes,
                Action::CreateDirectory { path },
                format!("missing; wanted {wanted}"),
            ),
        }
    }
}

/// The binary, the manifest and the configuration.
fn files(desired: &Desired, observed: &Observed, changes: &mut Vec<Change>) {
    if observed.binary_sha256.as_ref() != Some(&desired.binary_sha256) {
        let found = observed.binary_sha256.as_deref().unwrap_or("none");
        let detail = format!("found SHA-256 {found}; wanted {}", desired.binary_sha256);
        push(changes, Action::InstallBinary, detail);
    }
    if observed.manifest.as_ref() != Some(&desired.manifest) {
        push(
            changes,
            Action::WriteManifest,
            "the recorded inputs differ".into(),
        );
    }
    let recorded = observed.releases.as_deref().map(Releases::parse);
    let current = recorded.and_then(Result::ok).and_then(|r| r.current);
    if current.as_ref() != Some(&desired.release()) {
        let detail = format!("the current release is {}", desired.release().short());
        push(changes, Action::RecordRelease, detail);
    }
    let entries = config_text::changed(observed.config.as_deref(), &desired.settings);
    let entries = entries.unwrap_or_default();
    if !entries.is_empty() {
        let detail = format!("differs in {}", entries.join(", "));
        push(changes, Action::WriteConfig { entries }, detail);
    }
}

/// The key's changes, then the credential's.
fn secrets(desired: &Desired, observed: &Observed, changes: &mut Vec<Change>) {
    let key = (desired.key.as_ref(), &observed.key_source);
    secret(key, observed, Action::InstallKey, "key", changes);
    let credential = (desired.credential.as_ref(), &observed.credential_source);
    let install = Action::InstallCredential;
    secret(credential, observed, install, "credential", changes);
}

/// A secret file, the key or the credential (`what`), is copied by
/// `install` when it is missing or this run's file differs from it, and
/// its access list is put right when it differs.
fn secret(
    secret: Secret,
    observed: &Observed,
    install: Action,
    what: &str,
    changes: &mut Vec<Change>,
) {
    let (Some(spec), source) = secret else { return };
    let state = observed.path(&spec.path);
    let differs = *source == KeySource::Readable { installed: false };
    if *state == PathState::Missing || differs {
        let detail = format!("copy this run's {what} file; its bytes are never shown");
        return push(changes, install, detail);
    }
    if let PathState::File { access, .. } = state
        && *access != spec.access
    {
        let detail = format!(
            "found {}; wanted {}",
            access.describe(),
            spec.access.describe()
        );
        push(
            changes,
            Action::SetAccessList {
                path: spec.path.clone(),
            },
            detail,
        );
    }
}

/// The coordinator CLI is copied when the installed one is missing or is
/// not the one the manifest records; [`cli_refusal`] has refused the host
/// when no CLI file was given.
fn cli(desired: &Desired, observed: &Observed, changes: &mut Vec<Change>) {
    let Some(cli) = &desired.cli else { return };
    if observed.cli_sha256.as_ref() != Some(&cli.sha256) {
        let found = observed.cli_sha256.as_deref().unwrap_or("none");
        let detail = format!("found SHA-256 {found}; wanted {}", cli.sha256);
        push(changes, Action::InstallCli, detail);
    }
}

/// Makes the change to one setting of the service it is given.
type Setting = fn(String) -> Action;
const BIN_PATH: Setting = |service| Action::SetBinPath { service };
const START: Setting = |service| Action::SetStartType { service };
const DEPENDENCIES: Setting = |service| Action::SetDependencies { service };
const ACCOUNT: Setting = |service| Action::SetAccount { service };
const RECOVERY: Setting = |service| Action::SetRecovery { service };
const PRESHUTDOWN: Setting = |service| Action::SetPreshutdown { service };
const PRIVILEGES: Setting = |service| Action::SetPrivileges { service };

/// A missing service is registered and given the settings registration
/// cannot carry; a registered one gets a change for each setting that
/// differs.
fn service_changes(want: &ServiceSpec, seen: Option<&ServiceState>, changes: &mut Vec<Change>) {
    let Some(seen) = seen else {
        let service = want.name.clone();
        push(
            changes,
            Action::CreateService { service },
            "not registered".into(),
        );
        for setting in [RECOVERY, PRESHUTDOWN, PRIVILEGES] {
            push(changes, setting(want.name.clone()), "new service".into());
        }
        return;
    };
    setting_changes(want, &seen.spec, changes);
}

/// One change for each setting of a registered service that differs.
fn setting_changes(w: &ServiceSpec, s: &ServiceSpec, changes: &mut Vec<Change>) {
    let mut row = |setting: Setting, same: bool, seen: &dyn fmt::Debug, want: &dyn fmt::Debug| {
        if !same {
            let detail = format!("found {seen:?}; wanted {want:?}");
            push(changes, setting(w.name.clone()), detail);
        }
    };
    let (depends, account) = (&s.dependencies, &s.account);
    row(BIN_PATH, s.bin_path == w.bin_path, &s.bin_path, &w.bin_path);
    row(START, s.start == w.start, &s.start, &w.start);
    row(
        DEPENDENCIES,
        same_names(depends, &w.dependencies),
        depends,
        &w.dependencies,
    );
    row(
        ACCOUNT,
        account.eq_ignore_ascii_case(&w.account),
        account,
        &w.account,
    );
    row(RECOVERY, s.recovery == w.recovery, &s.recovery, &w.recovery);
    let (seen, want) = (&s.preshutdown_ms, &w.preshutdown_ms);
    row(PRESHUTDOWN, seen == want, seen, want);
    row(
        PRIVILEGES,
        same_names(&s.privileges, &w.privileges),
        &s.privileges,
        &w.privileges,
    );
}

/// Whether two lists hold the same names, whatever their order and case.
fn same_names(one: &[String], other: &[String]) -> bool {
    let folded = |names: &[String]| {
        let mut names: Vec<String> = names.iter().map(|n| n.to_ascii_lowercase()).collect();
        names.sort();
        names
    };
    folded(one) == folded(other)
}
