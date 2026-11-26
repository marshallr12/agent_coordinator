//! Observing a host and installing onto it. [`install`] applies one change
//! at a time, and decides each on a fresh observation: a refusal that
//! appears part-way stops it, and nothing is ever applied to a host that
//! was only checked earlier. [`status`] observes and changes nothing.
use super::coordinator;
use super::journal::{Journal, Stage};
use super::model::{Desired, Inputs, KeySpec, Layout, Overrides, ServiceSpec};
use super::observed::{KeySource, Observed, PathState, ProfileState, ServiceState};
use super::plan::{self, Action, Change, Refusal};
use anyhow::{Context, Result, bail};
use serde::Serialize;

/// What the installer asks of a machine. The Windows host answers with the
/// Service Control Manager and the file system; a test answers from memory.
pub trait Host {
    /// Whether this process is elevated.
    fn elevated(&self) -> Result<bool>;
    /// What is at `path`, without following a link.
    fn path(&self, path: &str) -> Result<PathState>;
    /// The text of the file at `path`, when there is one.
    fn text(&self, path: &str) -> Result<Option<String>>;
    /// The SHA-256, in hex, of the file at `path`, when there is one.
    fn sha256(&self, path: &str) -> Result<Option<String>>;
    /// How the service called `name` is registered, when it is.
    fn service(&self, name: &str) -> Result<Option<ServiceState>>;
    /// Whether the secret file `source`, a key or a coordinator credential,
    /// can be used, and whether the one at `installed` already holds the
    /// same bytes.
    fn key_source(&self, source: &str, installed: Option<&str>) -> KeySource;
    /// The names of every privilege the token of `process` holds.
    fn privileges(&self, process: u32) -> Result<Vec<String>>;
    /// Makes one change.
    fn apply(&mut self, desired: &Desired, action: &Action) -> Result<()>;
}

/// What updating and removing a host asks of a machine beyond what
/// installing asks (phase 7, step 7b).
pub trait Machine: Host {
    /// Copies the file `from` to `to`, synced to disk and whole or not at
    /// all. A link at either is refused.
    fn copy(&mut self, from: &str, to: &str) -> Result<()>;
    /// Writes `text` as the file at `path`, synced and whole or not at all.
    fn write(&mut self, path: &str, text: &str) -> Result<()>;
    /// Removes the file at `path`; nothing there is no error.
    fn remove(&mut self, path: &str) -> Result<()>;
    /// Makes a plain directory at `path` that inherits its parent's access
    /// list; one already there is kept.
    fn make_directory(&mut self, path: &str) -> Result<()>;
    /// The names of what is directly in the directory at `path`; none when
    /// nothing is there.
    fn entries(&self, path: &str) -> Result<Vec<String>>;
    /// How many bytes the file at `path` holds; 0 when there is none.
    fn length(&self, path: &str) -> Result<u64>;
    /// The text of the file at `path` after its first `offset` bytes; `None`
    /// when there is no such file.
    fn read_after(&self, path: &str, offset: u64) -> Result<Option<String>>;
    /// Removes the tree at `path` without following any link in it and
    /// without taking ownership of anything, and returns each path that
    /// could not be removed with why. Nothing there is no error.
    fn remove_tree(&mut self, path: &str) -> Result<Vec<String>>;
    /// Gives the tree at `path` to the administrators, with an access list
    /// of the system and administrators only, never through a link, and
    /// returns each path that could not be taken with why. Only `uninstall`
    /// calls it, and only on the coordinator CLI's session state, which the
    /// CLI locks to the supervisor's account.
    fn reclaim_tree(&mut self, path: &str) -> Result<Vec<String>>;
    /// Deletes the registered service `name`; one that is not registered,
    /// or is already marked for deletion, is done.
    fn delete_service(&mut self, name: &str) -> Result<()>;
    /// Where the profile of the account with `sid` stands.
    fn profile(&self, sid: &str) -> Result<ProfileState>;
    /// Deletes the unloaded profile of the account with `sid`.
    fn delete_profile(&mut self, sid: &str) -> Result<()>;
}

/// What one service's token holds, against what it may hold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TokenReport {
    /// The service.
    pub service: String,
    /// `not_registered`, `stopped`, `inventoried` or `not_inventoried`. A
    /// stopped service has no token: it is not a pass.
    pub state: &'static str,
    /// Every privilege the token holds.
    pub privileges: Vec<String>,
    /// Privileges it holds and may not.
    pub unexpected: Vec<String>,
    /// Privileges it may hold and does not.
    pub missing: Vec<String>,
    /// Why a running service's token could not be read.
    pub error: Option<String>,
}

impl TokenReport {
    /// Whether this is something the owner must look at.
    fn finding(&self) -> bool {
        self.state == "not_inventoried" || !self.unexpected.is_empty() || !self.missing.is_empty()
    }
}

/// What `install` or `status` found and did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    /// Why nothing was, or nothing more may be, changed.
    pub refusals: Vec<String>,
    /// The changes this run made, in order.
    pub applied: Vec<Change>,
    /// The changes the host still needs.
    pub pending: Vec<Change>,
    /// Each service's token inventory.
    pub tokens: Vec<TokenReport>,
}

impl Report {
    /// A report of a host that was left alone for `refusals`.
    fn refused(refusals: Vec<String>) -> Self {
        Self {
            refusals,
            applied: Vec::new(),
            pending: Vec::new(),
            tokens: Vec::new(),
        }
    }

    /// Whether the host is as desired: nothing refused, nothing pending
    /// and no token holding what it may not.
    pub fn clean(&self) -> bool {
        let findings = self.tokens.iter().any(TokenReport::finding);
        self.refusals.is_empty() && self.pending.is_empty() && !findings
    }
}

/// Observes the host as far as it may be looked at.
pub fn observe(host: &dyn Host, desired: &Desired) -> Result<Observed> {
    let mut seen = Observed::unread(host.elevated()?);
    if !seen.elevated {
        return Ok(seen);
    }
    observe_directories(host, desired, &mut seen)?;
    observe_files(host, desired, &mut seen)?;
    for service in &desired.services {
        let state = host.service(&service.name)?;
        seen.services.insert(service.name.clone(), state);
    }
    Ok(seen)
}

/// Looks at each directory, parents first, and at nothing below one that
/// is missing or would be refused.
fn observe_directories(host: &dyn Host, desired: &Desired, seen: &mut Observed) -> Result<()> {
    let mut closed: Vec<String> = Vec::new();
    for directory in &desired.directories {
        let path = &directory.path;
        if closed
            .iter()
            .any(|above| path.starts_with(&format!(r"{above}\")))
        {
            continue;
        }
        let state = host.path(path)?;
        if !adoptable(desired, path, &state) {
            closed.push(path.clone());
        }
        seen.paths.insert(path.clone(), state);
    }
    Ok(())
}

/// Whether `state` is a directory the installer may use as `path`.
fn adoptable(desired: &Desired, path: &str, state: &PathState) -> bool {
    let owners = desired.directory(path).map_or(&[][..], |d| &d.owners[..]);
    let PathState::Directory { owner, .. } = state else {
        return false;
    };
    plan::trusted(owner, owners)
}

/// Whether the directory at `path` was seen and may be used.
fn open(desired: &Desired, seen: &Observed, path: &str) -> bool {
    adoptable(desired, path, seen.path(path))
}

/// Reads the binary's and the CLI's digests, the configuration, the
/// manifest and the key's and the credential's state, each only from a
/// directory that may be used.
fn observe_files(host: &dyn Host, desired: &Desired, seen: &mut Observed) -> Result<()> {
    let layout = &desired.layout;
    if open(desired, seen, &layout.program_dir) {
        seen.binary_sha256 = host.sha256(&layout.binary())?;
        seen.config = host.text(&layout.config())?;
        if let Some(cli) = &desired.cli {
            seen.cli_sha256 = host.sha256(&cli.path)?;
        }
    }
    if open(desired, seen, &layout.installer_dir()) {
        seen.manifest = host.text(&layout.manifest())?;
        seen.releases = host.text(&layout.releases())?;
        seen.update_open = host.text(&layout.update_record())?.is_some();
    }
    let (key, credential) = (desired.key.as_ref(), desired.credential.as_ref());
    seen.key_source = observe_secret(host, desired, seen, key, &layout.key_dir())?;
    let directory = layout.coordinator_dir();
    seen.credential_source = observe_secret(host, desired, seen, credential, &directory)?;
    Ok(())
}

/// Looks at the secret file `spec` names when its `directory` may be used,
/// and returns what this run's file for it is like: not given when the
/// model wants no such secret or no file was given.
fn observe_secret(
    host: &dyn Host,
    desired: &Desired,
    seen: &mut Observed,
    spec: Option<&KeySpec>,
    directory: &str,
) -> Result<KeySource> {
    let Some(spec) = spec else {
        return Ok(KeySource::NotGiven);
    };
    if open(desired, seen, directory) {
        seen.paths.insert(spec.path.clone(), host.path(&spec.path)?);
    }
    let installed = matches!(seen.path(&spec.path), PathState::File { .. }).then_some(&*spec.path);
    Ok(match &spec.source {
        Some(source) => host.key_source(source, installed),
        None => KeySource::NotGiven,
    })
}

/// The inputs the host's manifest records, or `None` when there is none;
/// or why the manifest may not be read. The manifest is read only through
/// directories the installer may use: never through a link, and never from
/// a tree someone else owns.
pub(super) fn recorded(
    host: &dyn Host,
    layout: &Layout,
) -> Result<Result<Option<Inputs>, Vec<Refusal>>> {
    if !host.elevated()? {
        return Ok(Err(vec![Refusal::NotElevated]));
    }
    for path in [&layout.state_root, &layout.installer_dir()] {
        let state = host.path(path)?;
        if state == PathState::Missing {
            return Ok(Ok(None));
        }
        if let Some(refusal) = plan::directory_refusal(path, &[], &state) {
            return Ok(Err(vec![refusal]));
        }
    }
    let text = host.text(&layout.manifest())?;
    Ok(Ok(text
        .as_deref()
        .map(Inputs::from_manifest)
        .transpose()?))
}

/// The inputs the manifest records, when it may be read and there is one.
pub fn recorded_inputs(host: &dyn Host, layout: &Layout) -> Result<Option<Inputs>> {
    Ok(recorded(host, layout)?.ok().flatten())
}

/// `windows-host install`: makes the machine a host laid out as `layout`,
/// from what the manifest records and what this run's `overrides` say
/// instead. `binary_sha256` is the digest of the binary to install; the
/// digest of this run's coordinator CLI file, when one is given, is
/// recorded in the manifest.
pub fn install_from(
    host: &mut dyn Host,
    layout: Layout,
    overrides: &Overrides,
    binary_sha256: String,
    journal: &mut Journal,
) -> Result<Report> {
    let mut inputs = match recorded(host, &layout)? {
        Ok(inputs) => inputs.unwrap_or_default().with(overrides),
        Err(refusals) => return Ok(Report::refused(texts(&refusals))),
    };
    let sources = &overrides.coordinator;
    if let Err(refusal) = coordinator::record_cli(&*host, sources, &mut inputs) {
        return Ok(Report::refused(texts(&[refusal])));
    }
    let desired = Desired::new(layout, inputs, binary_sha256, overrides.key_source.clone())?;
    let desired = desired.with_sources(
        sources.credential_source.clone(),
        sources.cli_source.clone(),
    );
    install(host, &desired, journal)
}

/// `windows-host status`: what `install` would change on the host laid
/// out as `layout`, from what its manifest records. A machine with no
/// manifest is reported as not installed.
pub fn status_from(host: &dyn Host, layout: Layout, binary_sha256: String) -> Result<Report> {
    let inputs = match recorded(host, &layout)? {
        Ok(Some(inputs)) => inputs,
        Ok(None) => {
            let none = format!("nothing is installed: there is no {}", layout.manifest());
            return Ok(Report::refused(vec![none]));
        }
        Err(refusals) => return Ok(Report::refused(texts(&refusals))),
    };
    status(host, &Desired::new(layout, inputs, binary_sha256, None)?)
}

/// Reports what the host needs, changing nothing.
pub fn status(host: &dyn Host, desired: &Desired) -> Result<Report> {
    let seen = observe(host, desired)?;
    let (refusals, pending) = match plan::plan(desired, &seen) {
        Ok(changes) => (Vec::new(), changes),
        Err(refusals) => (refusals, Vec::new()),
    };
    Ok(Report {
        refusals: texts(&refusals),
        applied: Vec::new(),
        pending,
        tokens: tokens(host, desired, &seen),
    })
}

/// Makes the host the desired one, or reports why it may not. A change
/// that fails ends the run with an error; what was applied before it is
/// in the journal, and the next run plans from the host as it then is.
pub fn install(host: &mut dyn Host, desired: &Desired, journal: &mut Journal) -> Result<Report> {
    let mut applied: Vec<Change> = Vec::new();
    loop {
        let seen = observe(host, desired)?;
        let changes = match plan::plan(desired, &seen) {
            Ok(changes) => changes,
            Err(refusals) => return Ok(report(host, desired, &seen, applied, &refusals)),
        };
        let Some(next) = changes.into_iter().next() else {
            return Ok(report(host, desired, &seen, applied, &[]));
        };
        if applied.iter().any(|done| done.action == next.action) {
            bail!("{:?} was applied and the host still needs it", next.action);
        }
        apply(host, desired, journal, &next, applied.len())?;
        applied.push(next);
    }
}

/// Applies one change between its two journal lines.
fn apply(
    host: &mut dyn Host,
    desired: &Desired,
    journal: &mut Journal,
    change: &Change,
    done: usize,
) -> Result<()> {
    journal.record(Stage::Applying, change, None)?;
    if let Err(error) = host.apply(desired, &change.action) {
        let reason = format!("{error:#}");
        journal.record(Stage::Failed, change, Some(&reason))?;
        let context = format!(
            "{:?} failed after {done} changes were applied; run install again to complete it",
            change.action
        );
        return Err(error.context(context));
    }
    journal
        .record(Stage::Applied, change, None)
        .context("journal the applied change")
}

/// The report of an install that stopped at `refusals`, or finished.
fn report(
    host: &dyn Host,
    desired: &Desired,
    seen: &Observed,
    applied: Vec<Change>,
    refusals: &[Refusal],
) -> Report {
    Report {
        refusals: texts(refusals),
        applied,
        pending: Vec::new(),
        tokens: tokens(host, desired, seen),
    }
}

/// Each refusal as its sentence.
fn texts(refusals: &[Refusal]) -> Vec<String> {
    refusals.iter().map(ToString::to_string).collect()
}

/// Every service's token inventory. Nothing is inventoried on a host that
/// was not looked at.
fn tokens(host: &dyn Host, desired: &Desired, seen: &Observed) -> Vec<TokenReport> {
    if !seen.elevated {
        return Vec::new();
    }
    let report = |spec: &ServiceSpec| token(host, spec, seen.service(&spec.name));
    desired.services.iter().map(report).collect()
}

/// One service's token against the privileges it may hold.
fn token(host: &dyn Host, spec: &ServiceSpec, seen: Option<&ServiceState>) -> TokenReport {
    let mut report = TokenReport {
        service: spec.name.clone(),
        state: "not_registered",
        privileges: Vec::new(),
        unexpected: Vec::new(),
        missing: Vec::new(),
        error: None,
    };
    let Some(state) = seen else { return report };
    let Some(process) = state.process else {
        report.state = "stopped";
        return report;
    };
    match host.privileges(process) {
        Ok(held) => compare(&mut report, held, &spec.privileges),
        Err(error) => {
            report.state = "not_inventoried";
            report.error = Some(format!("{error:#}"));
        }
    }
    report
}

/// Fills `report` from the privileges a token `held` and those it `may`.
fn compare(report: &mut TokenReport, held: Vec<String>, may: &[String]) {
    let within = |list: &[String], name: &String| list.iter().any(|n| n.eq_ignore_ascii_case(name));
    report.state = "inventoried";
    report.unexpected = held.iter().filter(|n| !within(may, n)).cloned().collect();
    report.missing = may.iter().filter(|n| !within(&held, n)).cloned().collect();
    report.privileges = held;
}
