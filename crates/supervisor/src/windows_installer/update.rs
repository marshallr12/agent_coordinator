//! `windows-host update` and `rollback` (phase 7, step 7b): replacing the
//! installed release with another while the services are stopped, and
//! going back when the new one does not come up.
//!
//! An update is a [`Record`] written to `installer\update.json`, synced,
//! before anything changes, and rewritten after every stage: the record
//! always names the last stage that is done. Each stage is one
//! [`advance`], so a crash, a kill or a power loss stops the update between
//! two stages or inside one, and every stage may be repeated. Until the new
//! release has started and passed its checks ([`Stage::Checked`], the
//! commit point), the next run puts the release it replaced back in place
//! and running; after it, the next run finishes the update. A host is
//! therefore never left with one release's binary and another's
//! configuration, or with services that do not run what is installed.
//!
//! The update is run by the new release itself: it copies its own binary
//! into `releases\<sha256>\`, beside the installed one, and nothing a caller
//! supplies is ever run. The release it replaces is kept beside it with
//! the configuration it ran with, which is what `rollback` returns to.
use super::config_text;
use super::control::{self, RunState, STOP_ORDER, Services};
use super::install::{Machine, recorded};
use super::model::{Desired, Layout, Role};
use super::plan::{self, Refusal};
use super::release::{Release, Releases};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// What the record is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Installing a new release, which this run's binary is.
    Update,
    /// Returning to the kept previous release.
    Rollback,
}

/// The last stage that is done. Forward stages come first; from
/// [`Stage::Reverting`] on, the record puts the release it replaced back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// The record exists; nothing has changed.
    Begun,
    /// The new release's binary is kept, and its digest checked.
    Staged,
    /// The installed release's binary and configuration are kept.
    Retained,
    /// Every service has stopped, each with a clean exit.
    Stopped,
    /// The new release's binary and configuration are in place.
    Switched,
    /// The length of each log is recorded, so a start line is new.
    Measured,
    /// Every service was started.
    Started,
    /// Every service runs and logged its start: the commit point.
    Checked,
    /// The services that did not run before the update are stopped again.
    Settled,
    /// The update is being undone; nothing of it is undone yet.
    Reverting,
    /// Every service has stopped again.
    Halted,
    /// The earlier release's binary and configuration are back.
    Restored,
    /// The length of each log is recorded again.
    Remeasured,
    /// The services that ran before are started again.
    Restarted,
    /// They run and logged their start.
    Rechecked,
}

/// An update or rollback that has begun: everything the next run needs to
/// finish or undo it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    /// What it is doing.
    pub kind: Kind,
    /// The release installed before it began.
    pub from: Release,
    /// The release it installs.
    pub to: Release,
    /// The services that ran before it began; they run again after it.
    pub running_before: Vec<String>,
    /// The last stage that is done.
    pub stage: Stage,
    /// Whether the installed release was kept, after which services may
    /// have been stopped.
    pub retained: bool,
    /// Whether the new release's files were put in place.
    pub switched: bool,
    /// Each log's length before the services were started.
    pub offsets: BTreeMap<String, u64>,
    /// Whether the owner asked again for a release that was rolled back.
    pub retry: bool,
    /// Why it is being undone.
    pub reason: Option<String>,
}

impl Record {
    /// Whether the record is undoing what it did.
    pub fn reverting(&self) -> bool {
        self.stage >= Stage::Reverting
    }

    /// The release whose files belong in place now.
    fn target(&self) -> &Release {
        if self.reverting() {
            &self.from
        } else {
            &self.to
        }
    }

    /// The record turned to undoing itself, for `reason`.
    fn revert(mut self, reason: String) -> Self {
        self.stage = Stage::Reverting;
        self.reason = Some(reason);
        self
    }

    /// The record with `stage` done.
    fn at(mut self, stage: Stage) -> Self {
        self.stage = stage;
        self
    }
}

/// What an update or rollback found and did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UpdateReport {
    /// `updated`, `returned` (a rollback finished), `reverted` (the earlier
    /// release is back), `unchanged` or `refused`.
    pub outcome: &'static str,
    /// Why nothing was changed.
    pub refusals: Vec<String>,
    /// The release installed before.
    pub from: Option<Release>,
    /// The release asked for.
    pub to: Option<Release>,
    /// Why the update was undone.
    pub reason: Option<String>,
    /// Whether this run finished or undid an earlier run's update.
    pub resumed: bool,
    /// Every stage this run completed, in order.
    pub stages: Vec<Stage>,
}

impl UpdateReport {
    /// A report of a host that was left alone.
    fn refused(refusals: Vec<String>) -> Self {
        Self {
            outcome: "refused",
            refusals,
            from: None,
            to: None,
            reason: None,
            resumed: false,
            stages: Vec::new(),
        }
    }

    /// Whether the release asked for is installed and running.
    pub fn succeeded(&self) -> bool {
        matches!(self.outcome, "updated" | "returned" | "unchanged")
    }
}

/// One run: the machine, its services, the model of the release this binary
/// is, and the binary itself.
pub struct Run<'a> {
    /// The files, the services' registrations and the profiles.
    pub host: &'a mut dyn Machine,
    /// Starting and stopping.
    pub services: &'a mut dyn Services,
    /// The host as this binary models it.
    pub desired: &'a Desired,
    /// The binary this run is, which an update stages.
    pub source: String,
}

impl Run<'_> {
    /// Where everything is.
    fn layout(&self) -> &Layout {
        &self.desired.layout
    }

    /// The SHA-256 of the file at `path`, which must exist.
    fn digest(&self, path: &str) -> Result<String> {
        self.host
            .sha256(path)?
            .with_context(|| format!("{path} does not exist"))
    }

    /// Fails unless the file at `path` is `release`'s binary.
    fn verify(&self, path: &str, release: &Release) -> Result<()> {
        let found = self.digest(path)?;
        ensure!(
            found == release.sha256,
            "{path} has SHA-256 {found}, not {}",
            release.sha256
        );
        Ok(())
    }

    /// The text of the file at `path`, which must exist.
    fn text(&self, path: &str) -> Result<String> {
        self.host
            .text(path)?
            .with_context(|| format!("{path} does not exist"))
    }
}

/// What the owner asked an update for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The release this binary is, as the owner states it.
    pub to: Release,
    /// Whether this binary was built from a tree with uncommitted changes.
    pub dirty: bool,
    /// The source commit this binary was built from.
    pub built_from: String,
    /// Whether a release that was rolled back may be installed again.
    pub retry: bool,
}

/// `windows-host update`: finishes or undoes an earlier run's update if one
/// is open, and otherwise installs this binary as the new release.
pub fn update(run: &mut Run, request: &Request) -> Result<UpdateReport> {
    if let Some(open) = open(run)? {
        return resume(run, open);
    }
    let releases = match ready(run)? {
        Ok(releases) => releases,
        Err(refusals) => return Ok(UpdateReport::refused(refusals)),
    };
    let from = installed(run, &releases)?;
    if from.sha256 == request.to.sha256 {
        return Ok(unchanged(from));
    }
    if let Err(refusals) = admissible(run, request, &releases) {
        return Ok(UpdateReport::refused(refusals));
    }
    let record = begin(run, Kind::Update, from, request.to.clone(), request.retry)?;
    drive(run, record, false)
}

/// `windows-host rollback`: finishes or undoes an earlier run's update if
/// one is open, and otherwise returns to the kept previous release. A host
/// that keeps none is refused.
pub fn rollback(run: &mut Run) -> Result<UpdateReport> {
    if let Some(open) = open(run)? {
        return resume(run, open);
    }
    let releases = match ready(run)? {
        Ok(releases) => releases,
        Err(refusals) => return Ok(UpdateReport::refused(refusals)),
    };
    let Some(previous) = releases.previous.clone() else {
        let none = "no previous release is kept: there is nothing to roll back to";
        return Ok(UpdateReport::refused(vec![none.into()]));
    };
    let from = installed(run, &releases)?;
    let record = begin(run, Kind::Rollback, from, previous, false)?;
    drive(run, record, false)
}

/// The report of an update to the release already installed.
fn unchanged(release: Release) -> UpdateReport {
    UpdateReport {
        outcome: "unchanged",
        from: Some(release.clone()),
        to: Some(release),
        ..UpdateReport::refused(Vec::new())
    }
}

/// The open record, if an earlier run left one.
pub fn open(run: &Run) -> Result<Option<Record>> {
    let path = run.layout().update_record();
    let Some(text) = run.host.text(&path)? else {
        return Ok(None);
    };
    let record = serde_json::from_str(&text)
        .with_context(|| format!("{path} is not a record the updater wrote"))?;
    Ok(Some(record))
}

/// Writes `record` as the open record, synced.
pub fn save(run: &mut Run, record: &Record) -> Result<()> {
    let text = serde_json::to_string_pretty(record)? + "\n";
    let path = run.layout().update_record();
    run.host.write(&path, &text)
}

/// The host's release record, or why it may not be updated: it must be an
/// installed host the installer may change, and this binary must not run
/// from it.
fn ready(run: &mut Run) -> Result<Result<Releases, Vec<String>>> {
    let layout = run.layout().clone();
    match recorded(&*run.host, &layout)? {
        Err(refusals) => return Ok(Err(texts(&refusals))),
        Ok(None) => {
            return Ok(Err(vec![format!(
                "nothing is installed: there is no {}",
                layout.manifest()
            )]));
        }
        Ok(Some(_)) => {}
    }
    let mut found = host_refusals(run)?;
    found.extend(source_refusal(&layout, &run.source));
    if !found.is_empty() {
        return Ok(Err(found));
    }
    let text = run.host.text(&layout.releases())?;
    Ok(Ok(text
        .as_deref()
        .map(Releases::parse)
        .transpose()?
        .unwrap_or_default()))
}

/// What the installer would refuse about the host, besides its binary
/// being another release, which is what an update changes.
fn host_refusals(run: &Run) -> Result<Vec<String>> {
    let seen = super::install::observe(&*run.host, run.desired)?;
    let refusals = plan::refusals(run.desired, &seen);
    let kept = refusals
        .into_iter()
        .filter(|r| !matches!(r, Refusal::BinaryDiffers { .. }));
    let mut found: Vec<String> = kept.map(|r| r.to_string()).collect();
    for service in &run.desired.services {
        if seen.service(&service.name).is_none() {
            found.push(format!(
                "the service {} is not registered; run install first",
                service.name
            ));
        }
    }
    Ok(found)
}

/// A binary inside the program directory would replace or remove itself.
pub fn source_refusal(layout: &Layout, source: &str) -> Option<String> {
    let inside = format!(r"{}\", layout.program_dir.to_ascii_lowercase());
    source.to_ascii_lowercase().starts_with(&inside).then(|| {
        format!(
            "this binary runs from {source}, inside {}; run it from the build output or any \
             copy outside the program directory",
            layout.program_dir
        )
    })
}

/// The release installed now, named by its binary's digest and by the
/// commit the record gives it, when the record knows that binary.
fn installed(run: &Run, releases: &Releases) -> Result<Release> {
    let sha256 = run.digest(&run.layout().binary())?;
    let known = releases.current.as_ref().filter(|r| r.sha256 == sha256);
    let commit = known.map_or_else(|| "unknown".to_owned(), |r| r.commit.clone());
    Ok(Release { sha256, commit })
}

/// Why this binary may not be installed as `request.to`, if it may not.
fn admissible(run: &Run, request: &Request, releases: &Releases) -> Result<(), Vec<String>> {
    let to = &request.to;
    let mut found = Vec::new();
    if let Err(error) = to.validate() {
        found.push(format!("{error:#}"));
    }
    match run.digest(&run.source) {
        Ok(found_sha) if found_sha == to.sha256 => {}
        Ok(found_sha) => found.push(format!(
            "this binary has SHA-256 {found_sha}, not {}",
            to.sha256
        )),
        Err(error) => found.push(format!("{error:#}")),
    }
    if request.built_from != to.commit {
        found.push(format!(
            "this binary was built from {}, not {}",
            request.built_from, to.commit
        ));
    }
    if request.dirty {
        found.push("this binary was built from a tree with uncommitted changes".into());
    }
    if releases.rejected(&to.sha256) && !request.retry {
        found.push(format!(
            "{} was rolled back before; give --retry to install it again",
            to.short()
        ));
    }
    if found.is_empty() { Ok(()) } else { Err(found) }
}

/// Writes the record of a new update or rollback, before anything changes.
fn begin(run: &mut Run, kind: Kind, from: Release, to: Release, retry: bool) -> Result<Record> {
    let mut running_before = Vec::new();
    for (_, name) in run.layout().named(&Role::ALL) {
        if run.services.state(&name)? == RunState::Running {
            running_before.push(name);
        }
    }
    let record = Record {
        kind,
        from,
        to,
        running_before,
        stage: Stage::Begun,
        retained: false,
        switched: false,
        offsets: BTreeMap::new(),
        retry,
        reason: None,
    };
    save(run, &record)?;
    Ok(record)
}

/// Finishes or undoes the record an earlier run left: one that had not
/// passed its checks is undone, one that had is finished.
pub fn resume(run: &mut Run, record: Record) -> Result<UpdateReport> {
    let record = if record.stage < Stage::Checked {
        let reason = format!("an earlier run stopped after {:?}", record.stage);
        let record = record.revert(reason);
        save(run, &record)?;
        record
    } else {
        record
    };
    drive(run, record, true)
}

/// Advances `record` stage by stage until it is done. A stage that fails
/// before the commit point turns the record to undoing itself; one that
/// fails after it, or while undoing, leaves the record for the next run.
pub fn drive(run: &mut Run, mut record: Record, resumed: bool) -> Result<UpdateReport> {
    let mut stages = Vec::new();
    loop {
        let stage = record.stage;
        match advance(run, &record) {
            Ok(None) => return Ok(finished(&record, resumed, stages)),
            Ok(Some(next)) => {
                save(run, &next)?;
                stages.push(next.stage);
                record = next;
            }
            Err(error) if stage < Stage::Checked => {
                record = record.revert(format!("{stage:?} failed: {error:#}"));
                save(run, &record)?;
                stages.push(record.stage);
            }
            Err(error) => {
                return Err(error.context(format!("{stage:?} failed; run again to continue")));
            }
        }
    }
}

/// The report of a record that is done.
fn finished(record: &Record, resumed: bool, stages: Vec<Stage>) -> UpdateReport {
    let outcome = match (record.reverting(), record.kind) {
        (true, _) => "reverted",
        (false, Kind::Update) => "updated",
        (false, Kind::Rollback) => "returned",
    };
    UpdateReport {
        outcome,
        refusals: Vec::new(),
        from: Some(record.from.clone()),
        to: Some(record.to.clone()),
        reason: record.reason.clone(),
        resumed,
        stages,
    }
}

/// Does the stage after `record`'s and returns the record with it done, or
/// `None` when the record is finished and removed. Every stage may be done
/// again after a kill: each checks what is already in place.
pub fn advance(run: &mut Run, record: &Record) -> Result<Option<Record>> {
    let next = record.clone();
    let done = match record.stage {
        Stage::Begun => stage_binary(run, record).map(|()| next.at(Stage::Staged)),
        Stage::Staged => retain(run, record).map(|()| Record {
            retained: true,
            ..next.at(Stage::Retained)
        }),
        // Nothing was stopped or switched before the installed release was
        // kept: undoing it only forgets the record.
        Stage::Reverting if !record.retained => return finish(run, record).map(|()| None),
        Stage::Retained | Stage::Reverting => {
            let stage = if record.reverting() {
                Stage::Halted
            } else {
                Stage::Stopped
            };
            stop_all(run).map(|()| next.at(stage))
        }
        Stage::Stopped => switch(run, record).map(|()| Record {
            switched: true,
            ..next.at(Stage::Switched)
        }),
        Stage::Halted => restore(run, record).map(|()| next.at(Stage::Restored)),
        Stage::Switched | Stage::Restored => measure(run, record),
        Stage::Measured | Stage::Remeasured => start_all(run, record),
        Stage::Started | Stage::Restarted => check_all(run, record),
        Stage::Checked => settle(run, record).map(|()| next.at(Stage::Settled)),
        Stage::Settled | Stage::Rechecked => return finish(run, record).map(|()| None),
    };
    done.map(Some)
}

/// Keeps the release this record installs: an update copies this binary
/// into its release directory; a rollback's release is kept already.
fn stage_binary(run: &mut Run, record: &Record) -> Result<()> {
    let (layout, to) = (run.layout().clone(), &record.to);
    let kept = layout.kept_binary(&to.sha256);
    if record.kind == Kind::Update && run.digest(&kept).ok().as_ref() != Some(&to.sha256) {
        run.host.make_directory(&layout.kept_dir())?;
        run.host.make_directory(&layout.kept(&to.sha256))?;
        let source = run.source.clone();
        run.host.copy(&source, &kept)?;
    }
    run.verify(&kept, to)?;
    let config = layout.kept_config(&to.sha256);
    if record.kind == Kind::Rollback {
        run.text(&config)?;
    }
    Ok(())
}

/// Keeps the installed release's binary and configuration beside it, so
/// they can be put back.
fn retain(run: &mut Run, record: &Record) -> Result<()> {
    let (layout, from) = (run.layout().clone(), &record.from);
    let kept = layout.kept_binary(&from.sha256);
    if run.digest(&kept).ok().as_ref() != Some(&from.sha256) {
        run.host.make_directory(&layout.kept_dir())?;
        run.host.make_directory(&layout.kept(&from.sha256))?;
        run.host.copy(&layout.binary(), &kept)?;
    }
    run.verify(&kept, from)?;
    run.host
        .copy(&layout.config(), &layout.kept_config(&from.sha256))
}

/// Stops every service, the supervisor first and the brokers last. A
/// service that stops unclean, such as a runner whose launches may still
/// run, stops the update before anything is switched.
fn stop_all(run: &mut Run) -> Result<()> {
    for (_, name) in run.layout().named(&STOP_ORDER) {
        control::stop(run.services, &name)?;
    }
    Ok(())
}

/// Puts the new release's binary in place, and its configuration: an
/// update's is the installed one with this release's managed entries; a
/// rollback's is the one the kept release ran with.
fn switch(run: &mut Run, record: &Record) -> Result<()> {
    let (layout, to) = (run.layout().clone(), &record.to);
    let binary = layout.binary();
    if run.digest(&binary).ok().as_ref() != Some(&to.sha256) {
        run.host.copy(&layout.kept_binary(&to.sha256), &binary)?;
    }
    run.verify(&binary, to)?;
    match record.kind {
        Kind::Rollback => run
            .host
            .copy(&layout.kept_config(&to.sha256), &layout.config()),
        Kind::Update => {
            let kept = run.text(&layout.kept_config(&record.from.sha256))?;
            let text = config_text::merged(Some(&kept), &run.desired.settings)?;
            run.host.write(&layout.config(), &text)
        }
    }
}

/// Puts the earlier release's binary and configuration back.
fn restore(run: &mut Run, record: &Record) -> Result<()> {
    let (layout, from) = (run.layout().clone(), &record.from);
    let binary = layout.binary();
    if run.digest(&binary).ok().as_ref() != Some(&from.sha256) {
        run.host.copy(&layout.kept_binary(&from.sha256), &binary)?;
    }
    run.verify(&binary, from)?;
    run.host
        .copy(&layout.kept_config(&from.sha256), &layout.config())
}

/// The services this record starts and checks, with their roles, in start
/// order: all four going forward, so the new release is seen to run; only
/// those that ran before when undoing.
fn started(run: &Run, record: &Record) -> Vec<(Role, String)> {
    let all = run.layout().named(&Role::ALL);
    if !record.reverting() {
        return all;
    }
    let before = |(_, name): &(Role, String)| record.running_before.contains(name);
    all.into_iter().filter(before).collect()
}

/// Records each log's length, so only a start line written after it counts.
fn measure(run: &mut Run, record: &Record) -> Result<Record> {
    let mut offsets = BTreeMap::new();
    for (role, name) in started(run, record) {
        offsets.insert(name, run.host.length(&run.layout().log(role))?);
    }
    let stage = if record.reverting() {
        Stage::Remeasured
    } else {
        Stage::Measured
    };
    Ok(Record {
        offsets,
        ..record.clone().at(stage)
    })
}

/// Starts the services in start order.
fn start_all(run: &mut Run, record: &Record) -> Result<Record> {
    for (_, name) in started(run, record) {
        control::start(run.services, &name)?;
    }
    let stage = if record.reverting() {
        Stage::Restarted
    } else {
        Stage::Started
    };
    Ok(record.clone().at(stage))
}

/// Checks that each started service runs and logged its start.
fn check_all(run: &mut Run, record: &Record) -> Result<Record> {
    for (role, name) in started(run, record) {
        let offset = *record
            .offsets
            .get(&name)
            .context("no log length was recorded")?;
        let layout = run.layout().clone();
        control::check(&*run.host, run.services, &layout, (role, &name), offset)?;
    }
    let stage = if record.reverting() {
        Stage::Rechecked
    } else {
        Stage::Checked
    };
    Ok(record.clone().at(stage))
}

/// Stops again the services that did not run before the update.
fn settle(run: &mut Run, record: &Record) -> Result<()> {
    for (_, name) in run.layout().named(&STOP_ORDER) {
        if !record.running_before.contains(&name) {
            control::stop(run.services, &name)?;
        }
    }
    Ok(())
}

/// Records the outcome in the release record, removes the releases no
/// longer kept and then the open record.
fn finish(run: &mut Run, record: &Record) -> Result<()> {
    let layout = run.layout().clone();
    let text = run.host.text(&layout.releases())?;
    let mut releases = text
        .as_deref()
        .map(Releases::parse)
        .transpose()?
        .unwrap_or_default();
    outcome(&mut releases, record);
    run.host.write(&layout.releases(), &releases.text()?)?;
    prune(run, &releases)?;
    run.host.remove(&layout.update_record())
}

/// What the record's end changes in the release record. An update that
/// put its release in place and was undone remembers that release as
/// rolled back; so does a rollback, of the release it left.
fn outcome(releases: &mut Releases, record: &Record) {
    match (record.reverting(), record.kind) {
        (false, Kind::Update) => {
            releases.forgive(&record.to.sha256);
            releases.previous = Some(record.from.clone());
        }
        (false, Kind::Rollback) => {
            releases.reject(&record.from.sha256);
            releases.previous = None;
        }
        (true, kind) => {
            if record.switched {
                releases.reject(&record.to.sha256);
            }
            if kind == Kind::Rollback && record.switched {
                releases.previous = None;
            }
        }
    }
    releases.current = Some(record.target().clone());
}

/// Removes every kept release but the current and the previous one.
fn prune(run: &mut Run, releases: &Releases) -> Result<()> {
    let layout = run.layout().clone();
    let keep: Vec<&str> = [&releases.current, &releases.previous]
        .into_iter()
        .flatten()
        .map(|r| r.sha256.as_str())
        .collect();
    for name in run.host.entries(&layout.kept_dir())? {
        if !keep.contains(&name.as_str()) {
            let left = run.host.remove_tree(&layout.kept(&name))?;
            if !left.is_empty() {
                bail!(
                    "the kept release {name} could not be removed: {}",
                    left.join("; ")
                );
            }
        }
    }
    Ok(())
}

/// Each refusal as its sentence.
fn texts(refusals: &[Refusal]) -> Vec<String> {
    refusals.iter().map(ToString::to_string).collect()
}
