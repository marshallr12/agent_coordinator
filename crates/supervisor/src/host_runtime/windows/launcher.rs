//! One launcher: the window station it made and every launch it holds.
//!
//! A launch is prepared once (profile, directories, private desktop) and may
//! then be started, observed, stopped, terminated and cleaned up. Each start
//! gets a fresh Job Object. The launcher's handles are the only ones to each
//! job, so when the launcher process ends, Windows ends every launch.
use super::desktop::{Desktop, Station};
use super::job::{Job, Limits};
use super::process::{self, Boundary, Command, Running};
use super::profile::{Capabilities, Profile};
use super::reader::create_directory;
use super::sys::{Handle, check};
use super::text::wide;
use super::{sddl, token};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use windows_sys::Win32::Foundation::{GENERIC_READ, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetExitCodeProcess, OpenProcess,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, WaitForSingleObject,
};

/// The file a launcher puts in a launch's control directory to ask the
/// launch to end by itself.
pub const STOP_MARKER: &str = "stop";
/// How long a terminated job is given to empty.
const TERMINATE_PATIENCE: Duration = Duration::from_secs(10);

/// Everything that fixes one launch's boundary before it starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchSpec {
    /// The AppContainer profile to create; unique to this launch.
    pub profile: String,
    /// Whether the token drops what "all application packages" are granted.
    pub less_privileged: bool,
    /// The capabilities the token carries. None of them may be a network
    /// capability; the launcher does not add any.
    pub capabilities: Vec<String>,
    pub limits: Limits,
    /// A directory the launcher creates and the launch may only read.
    pub control: PathBuf,
    /// A directory the launcher creates; the only tree the launch may modify.
    pub work: PathBuf,
}

/// What a prepared launch is known by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Prepared {
    /// The launch's AppContainer package SID.
    pub package_sid: String,
    /// The launch's private desktop, `station\desktop`.
    pub desktop: String,
    /// Who owns the launch's jobs: this launcher process ([`job_owner`]).
    pub job_owner: String,
}

/// A started launch's root process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Started {
    pub pid: u32,
    /// When the process was created, as a 64-bit `FILETIME`.
    pub created: u64,
}

/// Whether a started launch's root process has ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen {
    Running,
    Exited(u32),
}

/// One prepared launch and what it currently runs.
struct Slot {
    spec: LaunchSpec,
    profile: Profile,
    capabilities: Capabilities,
    desktop: Desktop,
    /// The job of the current start; `None` once it was emptied and closed.
    job: Option<Job>,
    /// The root process of the latest start, kept after its job is gone so
    /// its exit code can still be read.
    root: Option<Running>,
    /// Inheritable handles the launcher holds for this launch.
    held: Vec<Handle>,
    /// A descendant watched by handle, so its end is seen without its pid.
    watched: Option<Handle>,
}

/// The directories made so far for a launch that is not prepared yet.
/// Dropping it removes them again, newest first, unless [`Made::keep`] was
/// called: a guard, so every early return of the preparation undoes them.
#[derive(Default)]
struct Made(Vec<PathBuf>);

impl Made {
    /// Creates `path` with the access list `sddl` and remembers it. A
    /// directory that already exists is an error and is not remembered, so
    /// only what this launch made is ever removed.
    fn directory(&mut self, path: &Path, sddl: &str) -> Result<()> {
        create_directory(path, sddl)?;
        self.0.push(path.to_owned());
        Ok(())
    }

    /// The launch is prepared: its directories stay.
    fn keep(mut self) {
        self.0.clear();
    }
}

impl Drop for Made {
    /// Nothing has run in these directories, so each is empty and is
    /// removed as such; nothing below it is followed or deleted.
    fn drop(&mut self) {
        for directory in self.0.iter().rev() {
            let _ = std::fs::remove_dir(directory);
        }
    }
}

/// The launches of one account's logon session.
pub struct Launcher {
    owner: String,
    station: Option<Station>,
    launches: BTreeMap<String, Slot>,
}

/// The identity of the process that owns every job this launcher makes:
/// `runner:<pid>:<creation time>`. Each job dies with its last handle, and
/// only this process holds one, so a launch whose owner is gone has ended.
pub fn job_owner() -> Result<String> {
    // SAFETY: both calls take no arguments; the pseudo-handle needs no close.
    let (pid, process) = unsafe { (GetCurrentProcessId(), GetCurrentProcess()) };
    let created = process::creation_time(process)?;
    Ok(format!("runner:{pid}:{created}"))
}

impl Launcher {
    /// A launcher for the calling account, holding nothing yet.
    pub fn new() -> Result<Self> {
        Ok(Self {
            owner: token::current_user_sid()?,
            station: None,
            launches: BTreeMap::new(),
        })
    }

    /// The account every launch of this launcher runs as.
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// The station, made on first use; an existing one is taken over only
    /// when Windows made it for this process alone, and refused otherwise.
    fn station(&mut self) -> Result<&mut Station> {
        if self.station.is_none() {
            self.station = Some(Station::create(&self.owner)?);
        }
        Ok(self.station.as_mut().expect("the station was just made"))
    }

    /// The prepared launch `id`.
    fn slot(&mut self, id: &str) -> Result<&mut Slot> {
        let slot = self.launches.get_mut(id);
        slot.with_context(|| format!("launch {id} is not prepared"))
    }

    /// Creates the launch's profile, its two directories and its private
    /// desktop. Nothing is left behind when any of them fails.
    pub fn prepare(&mut self, id: &str, spec: LaunchSpec) -> Result<Prepared> {
        ensure!(
            !self.launches.contains_key(id),
            "launch {id} is already prepared"
        );
        let capabilities = Capabilities::named(&spec.capabilities)?;
        let job_owner = job_owner()?;
        let profile = Profile::create(&spec.profile)?;
        let desktop = match self.furnish(&spec, &profile) {
            Ok(desktop) => desktop,
            Err(error) => {
                let _ = profile.delete();
                return Err(error);
            }
        };
        let prepared = Prepared {
            package_sid: profile.sid_text().to_owned(),
            desktop: desktop.name().to_owned(),
            job_owner,
        };
        let slot = Slot {
            spec,
            profile,
            capabilities,
            desktop,
            job: None,
            root: None,
            held: Vec::new(),
            watched: None,
        };
        self.launches.insert(id.to_owned(), slot);
        Ok(prepared)
    }

    /// The launch's directories, each with its complete access list, and
    /// its desktop in the launcher's station. A directory made here is
    /// removed again when a later step fails or is refused.
    fn furnish(&mut self, spec: &LaunchSpec, profile: &Profile) -> Result<Desktop> {
        let (owner, package) = (self.owner.clone(), profile.sid_text());
        let mut made = Made::default();
        made.directory(&spec.control, &sddl::control_directory(&owner, package)?)?;
        made.directory(&spec.work, &sddl::work_directory(&owner, package)?)?;
        let label = format!("agentc-{}", uuid::Uuid::new_v4().simple());
        let desktop = Desktop::create(self.station()?, package, &label)?;
        made.keep();
        Ok(desktop)
    }

    /// Opens `path` for reading with an inheritable handle the launcher
    /// keeps for launch `id`, and returns the handle's value. The launch
    /// can use it only when a start lists it.
    pub fn hold_file(&mut self, id: &str, path: &Path) -> Result<u64> {
        let handle = open_inheritable(path)?;
        let value = handle.raw() as usize as u64;
        self.slot(id)?.held.push(handle);
        Ok(value)
    }

    /// Starts `command` for the prepared launch `id` in a fresh job,
    /// inheriting its log and the held handles named in `listed`.
    pub fn start(&mut self, id: &str, command: &Command, listed: &[u64]) -> Result<Started> {
        let slot = self.slot(id)?;
        ensure!(slot.job.is_none(), "launch {id} already runs");
        let passed = slot.listed(listed)?;
        let job = Job::create(slot.spec.limits)?;
        let boundary = Boundary {
            profile: &slot.profile,
            less_privileged: slot.spec.less_privileged,
            capabilities: &mut slot.capabilities,
            job: &job,
            desktop: &slot.desktop,
            passed: &passed,
        };
        let running = process::start(command, boundary)?;
        let started = Started {
            pid: running.pid,
            created: running.created,
        };
        (slot.job, slot.root) = (Some(job), Some(running));
        Ok(started)
    }

    /// Whether launch `id` has a start whose job the launcher still holds.
    pub fn runs(&mut self, id: &str) -> Result<bool> {
        Ok(self.slot(id)?.job.is_some())
    }

    /// Waits up to `patience` for the launch's root process to end.
    pub fn observe(&mut self, id: &str, patience: Duration) -> Result<Seen> {
        let slot = self.slot(id)?;
        let running = slot.root.as_ref().context("the launch is not started")?;
        let millis = u32::try_from(patience.as_millis()).unwrap_or(u32::MAX - 1);
        if !ended_within(running.process.raw(), millis)? {
            return Ok(Seen::Running);
        }
        let mut code = 0_u32;
        // SAFETY: the process handle is live and `code` is writable.
        let read = unsafe { GetExitCodeProcess(running.process.raw(), &mut code) };
        check(read, "GetExitCodeProcess")?;
        Ok(Seen::Exited(code))
    }

    /// Asks the launch to end by itself: the stop marker appears in its
    /// control directory, which it can read and cannot change.
    pub fn request_stop(&mut self, id: &str) -> Result<()> {
        let marker = self.slot(id)?.spec.control.join(STOP_MARKER);
        std::fs::write(&marker, b"").with_context(|| format!("write {}", marker.display()))
    }

    /// Ends every process of the launch's current start and closes its job;
    /// true when Windows reported the job empty.
    pub fn terminate(&mut self, id: &str) -> Result<bool> {
        let slot = self.slot(id)?;
        let Some(job) = slot.job.take() else {
            return Ok(true);
        };
        Ok(job.terminate(TERMINATE_PATIENCE))
    }

    /// Kills every process of the launch's current start without waiting
    /// for its job to empty; the job stays held until [`Launcher::emptied`]
    /// sees it empty.
    pub fn kill(&mut self, id: &str) -> Result<()> {
        if let Some(job) = &self.slot(id)?.job {
            job.kill();
        }
        Ok(())
    }

    /// Whether the launch's job is empty, as Windows reports it; the job is
    /// closed once it is. A launch without a job has nothing running.
    pub fn emptied(&mut self, id: &str) -> Result<bool> {
        let slot = self.slot(id)?;
        let empty = slot.job.as_ref().is_none_or(|job| job.active() == Some(0));
        if empty {
            slot.job = None;
        }
        Ok(empty)
    }

    /// Starts watching process `pid` by handle; true when it is alive and a
    /// member of the launch's current job. The pid comes from the worker,
    /// so it counts only with both facts from Windows.
    pub fn watch(&mut self, id: &str, pid: u32) -> Result<bool> {
        let slot = self.slot(id)?;
        let Ok(process) = open_process(pid) else {
            return Ok(false);
        };
        let member = match &slot.job {
            Some(job) => job.contains(process.raw())?,
            None => false,
        };
        let alive = !ended_within(process.raw(), 0)?;
        slot.watched = Some(process);
        Ok(alive && member)
    }

    /// Whether the watched process ended within `patience`; false when
    /// nothing is watched.
    pub fn watched_ended(&mut self, id: &str, patience: Duration) -> Result<bool> {
        let slot = self.slot(id)?;
        let Some(process) = slot.watched.as_ref() else {
            return Ok(false);
        };
        let millis = u32::try_from(patience.as_millis()).unwrap_or(u32::MAX - 1);
        ended_within(process.raw(), millis)
    }

    /// Ends the launch, closes its desktop, withdraws its package from the
    /// station and deletes its profile. The directories stay for the caller.
    pub fn cleanup(&mut self, id: &str) -> Result<()> {
        self.terminate(id)?;
        let slot = self
            .launches
            .remove(id)
            .context("the launch is not prepared")?;
        let package = slot.profile.sid_text().to_owned();
        let Slot {
            profile, desktop, ..
        } = slot;
        drop(desktop);
        if let Some(station) = self.station.as_mut() {
            station.dismiss(&package)?;
        }
        profile.delete()
    }

    /// The ids of every launch this launcher holds.
    pub fn held(&self) -> Vec<String> {
        self.launches.keys().cloned().collect()
    }
}

impl Slot {
    /// The held handles named by value in `listed`; any other value is
    /// refused, so a launch inherits only what the launcher opened for it.
    fn listed(&self, listed: &[u64]) -> Result<Vec<HANDLE>> {
        let mut passed = Vec::new();
        for value in listed {
            let found = (self.held.iter()).find(|held| held.raw() as usize as u64 == *value);
            let Some(handle) = found else {
                bail!("handle {value} is not held for this launch");
            };
            passed.push(handle.raw());
        }
        Ok(passed)
    }
}

/// Opens `path` read-only with an inheritable handle.
fn open_inheritable(path: &Path) -> Result<Handle> {
    let name = wide(&path.display().to_string())?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    };
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    // SAFETY: the name is NUL-terminated and `attributes` outlives the call.
    let raw = unsafe {
        let none = std::ptr::null_mut();
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ,
            share,
            &attributes,
            OPEN_EXISTING,
            0,
            none,
        )
    };
    Handle::owned(raw, "opening a file to pass to a launch")
}

/// Opens process `pid` to wait on and to query.
pub fn open_process(pid: u32) -> Result<Handle> {
    let access = PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION;
    // SAFETY: OpenProcess takes plain integers and returns a new handle.
    let raw = unsafe { OpenProcess(access, 0, pid) };
    Handle::owned(raw, "OpenProcess")
}

/// Whether `process` ended within `millis` milliseconds.
pub(crate) fn ended_within(process: HANDLE, millis: u32) -> Result<bool> {
    // SAFETY: the process handle is live.
    match unsafe { WaitForSingleObject(process, millis) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        _ => Err(super::sys::last_error("WaitForSingleObject")),
    }
}
