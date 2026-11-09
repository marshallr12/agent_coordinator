//! Starting one process inside the whole boundary, or not at all.
//!
//! The job, the token and the inherited handles are fixed by the process
//! attribute list at creation, so there is no moment when the process exists
//! outside them. It starts suspended. Before its first instruction runs the
//! launcher reads back from Windows the job membership, the AppContainer
//! flag, the package SID and the less-privileged opt-out; any mismatch ends
//! the job. There is no uncontained retry and no partial launch.
use super::desktop::Desktop;
use super::job::Job;
use super::profile::{Capabilities, Profile};
use super::sys::{Handle, check, last_error};
use super::text::{command_line, environment_block, wide};
use super::token;
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::time::Duration;
use windows_sys::Win32::Foundation::{FILETIME, GENERIC_WRITE, HANDLE};
use windows_sys::Win32::Security::{SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES};
use windows_sys::Win32::Storage::FileSystem::{CREATE_ALWAYS, CreateFileW, FILE_SHARE_READ};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetProcessTimes,
    InitializeProcThreadAttributeList, PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, ResumeThread,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute,
};
use windows_sys::Win32::System::WindowsProgramming::PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT;

/// How long a job is given to empty after a refused start.
const REFUSAL_PATIENCE: Duration = Duration::from_secs(10);

/// What to run: no shell, an exact environment and a host-owned log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    /// The program's exact path, then its arguments.
    pub argv: Vec<String>,
    /// The directory the process starts in.
    pub directory: PathBuf,
    /// Every environment variable the process gets; nothing is inherited.
    pub environment: BTreeMap<String, String>,
    /// The log its standard output and error go to. The launcher creates
    /// it; the launch receives only an inherited handle.
    pub output: PathBuf,
}

/// The boundary one process starts inside.
pub struct Boundary<'a> {
    pub profile: &'a Profile,
    /// Whether the token drops what "all application packages" are granted.
    pub less_privileged: bool,
    pub capabilities: &'a mut Capabilities,
    pub job: &'a Job,
    pub desktop: &'a Desktop,
    /// Handles the launch inherits besides its log; they must be
    /// inheritable. Every other handle the launcher holds stays behind.
    pub passed: &'a [HANDLE],
}

/// A process that Windows confirmed is inside its boundary, now running.
#[derive(Debug)]
pub struct Running {
    pub process: Handle,
    pub pid: u32,
    /// When the process was created, as a 64-bit `FILETIME`; with the pid
    /// it identifies the process for good.
    pub created: u64,
    /// Keeps the log open for the life of the launch.
    _output: Handle,
}

/// What Windows reported about a suspended launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Facts {
    pub in_job: bool,
    pub appcontainer: bool,
    pub package_matches: bool,
    pub privilege_matches: bool,
}

impl Facts {
    /// Why the launch must not run, naming each fact that is not as asked.
    pub fn refusal(&self) -> Option<String> {
        let facts = [
            (self.in_job, "job membership"),
            (self.appcontainer, "AppContainer token"),
            (self.package_matches, "package SID"),
            (self.privilege_matches, "less-privileged opt-out"),
        ];
        let missing: Vec<&str> = (facts.iter())
            .filter(|(held, _)| !held)
            .map(|(_, name)| *name)
            .collect();
        (!missing.is_empty()).then(|| missing.join(", "))
    }
}

/// Starts `command` inside `boundary` and resumes it once Windows confirms
/// the boundary. On any failure the job is emptied and nothing runs.
pub fn start(command: &Command, boundary: Boundary<'_>) -> Result<Running> {
    let output = create_log(&command.output)?;
    let (job, profile, less) = (boundary.job, boundary.profile, boundary.less_privileged);
    let started = create(command, boundary, &output).and_then(|created| {
        verify(&created.process, job, profile, less)?;
        resume(&created.thread)?;
        Ok(created)
    });
    let created = match started {
        Ok(created) => created,
        Err(error) => {
            job.terminate(REFUSAL_PATIENCE);
            return Err(error);
        }
    };
    Ok(Running {
        created: creation_time(created.process.raw())?,
        process: created.process,
        pid: created.pid,
        _output: output,
    })
}

/// A new, inheritable, write-only log file at `path`.
fn create_log(path: &Path) -> Result<Handle> {
    let name = wide(&path.display().to_string())?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    };
    // SAFETY: the name is NUL-terminated and `attributes` outlives the call.
    let raw = unsafe {
        let (share, none) = (FILE_SHARE_READ, std::ptr::null_mut());
        CreateFileW(
            name.as_ptr(),
            GENERIC_WRITE,
            share,
            &attributes,
            CREATE_ALWAYS,
            0,
            none,
        )
    };
    Handle::owned(raw, "creating the launch log")
}

/// A created, still suspended process.
struct Created {
    process: Handle,
    thread: Handle,
    pid: u32,
}

/// The values a process attribute list points at, and the list itself. The
/// list holds pointers into this structure, so it lives on the heap and is
/// not moved while the list exists.
struct Attributes {
    list: Vec<u64>,
    jobs: [HANDLE; 1],
    handles: Vec<HANDLE>,
    security: SECURITY_CAPABILITIES,
    policy: u32,
}

impl Attributes {
    /// The attribute list for `boundary`, inheriting `output` and the
    /// handles the boundary passes.
    fn new(boundary: Boundary<'_>, output: &Handle) -> Result<Box<Self>> {
        let mut handles = vec![output.raw()];
        handles.extend_from_slice(boundary.passed);
        let security = SECURITY_CAPABILITIES {
            AppContainerSid: boundary.profile.sid(),
            Capabilities: boundary.capabilities.pointer(),
            CapabilityCount: boundary.capabilities.count(),
            Reserved: 0,
        };
        let count = if boundary.less_privileged { 4 } else { 3 };
        let mut attributes = Box::new(Self {
            list: allocate(count)?,
            jobs: [boundary.job.handle()],
            handles,
            security,
            policy: PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT,
        });
        attributes.fill(boundary.less_privileged)?;
        Ok(attributes)
    }

    /// Points the list at the job, the token shape and the handle list.
    fn fill(&mut self, less_privileged: bool) -> Result<()> {
        let jobs = (self.jobs.as_ptr().cast(), size_of_val(&self.jobs));
        self.update(PROC_THREAD_ATTRIBUTE_JOB_LIST, jobs)?;
        let security = (
            std::ptr::from_ref(&self.security).cast(),
            size_of_val(&self.security),
        );
        self.update(PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, security)?;
        let handles = (
            self.handles.as_ptr().cast(),
            size_of_val(self.handles.as_slice()),
        );
        self.update(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, handles)?;
        if less_privileged {
            let policy = (
                std::ptr::from_ref(&self.policy).cast(),
                size_of_val(&self.policy),
            );
            self.update(
                PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY,
                policy,
            )?;
        }
        Ok(())
    }

    /// Sets one attribute to the `value` (pointer, size) it must point at.
    fn update(&mut self, attribute: u32, value: (*const c_void, usize)) -> Result<()> {
        let (none, no_size) = (std::ptr::null_mut(), std::ptr::null());
        // SAFETY: the list was initialized for enough attributes, and the
        // value lives in `self`, which outlives the list.
        let updated = unsafe {
            UpdateProcThreadAttribute(
                self.pointer(),
                0,
                attribute as usize,
                value.0,
                value.1,
                none,
                no_size,
            )
        };
        check(updated, "UpdateProcThreadAttribute")
    }

    /// The list, for process creation.
    fn pointer(&mut self) -> *mut c_void {
        self.list.as_mut_ptr().cast()
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        // SAFETY: the list was initialized by `allocate` and is deleted once.
        unsafe { DeleteProcThreadAttributeList(self.pointer()) };
    }
}

/// An initialized, empty attribute list with room for `count` attributes.
fn allocate(count: u32) -> Result<Vec<u64>> {
    let mut size = 0_usize;
    // SAFETY: a null list asks only for the size, written to `size`.
    unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), count, 0, &mut size) };
    let mut list = vec![0_u64; size.div_ceil(8).max(1)];
    // SAFETY: `list` is writable for at least `size` bytes.
    let initialized =
        unsafe { InitializeProcThreadAttributeList(list.as_mut_ptr().cast(), count, 0, &mut size) };
    check(initialized, "InitializeProcThreadAttributeList")?;
    Ok(list)
}

/// Creates the process suspended, with the attribute list, the private
/// desktop, the exact environment and its log as standard output and error.
fn create(command: &Command, boundary: Boundary<'_>, output: &Handle) -> Result<Created> {
    let program = wide(command.argv.first().context("a launch needs a program")?)?;
    let mut line = command_line(&command.argv)?;
    let environment = environment_block(&command.environment)?;
    let directory = wide(&command.directory.display().to_string())?;
    let mut desktop = wide(boundary.desktop.name())?;
    let mut attributes = Attributes::new(boundary, output)?;
    let startup = startup(&mut desktop, output, attributes.pointer());
    // SAFETY: an all-zero PROCESS_INFORMATION is a valid value to overwrite.
    let mut information: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let flags = EXTENDED_STARTUPINFO_PRESENT
        | CREATE_UNICODE_ENVIRONMENT
        | CREATE_SUSPENDED
        | CREATE_NO_WINDOW;
    // SAFETY: every string is NUL-terminated and every buffer outlives the
    // call; handles are inherited only as the attribute list names them.
    let created = unsafe {
        let none = std::ptr::null();
        CreateProcessW(
            program.as_ptr(),
            line.as_mut_ptr(),
            none,
            none,
            1,
            flags,
            environment.as_ptr().cast(),
            directory.as_ptr(),
            std::ptr::from_ref(&startup).cast(),
            &mut information,
        )
    };
    check(created, "CreateProcessW")?;
    adopt(&information)
}

/// The startup block: the launch's desktop, its log and the attribute list.
fn startup(desktop: &mut [u16], output: &Handle, attributes: *mut c_void) -> STARTUPINFOEXW {
    // SAFETY: an all-zero STARTUPINFOEXW is a valid value to fill in.
    let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdOutput = output.raw();
    startup.StartupInfo.hStdError = output.raw();
    startup.StartupInfo.lpDesktop = desktop.as_mut_ptr();
    startup.lpAttributeList = attributes;
    startup
}

/// Takes ownership of a created process's handles.
fn adopt(information: &PROCESS_INFORMATION) -> Result<Created> {
    let process = Handle::owned(information.hProcess, "CreateProcessW");
    let thread = Handle::owned(information.hThread, "CreateProcessW");
    Ok(Created {
        process: process?,
        thread: thread?,
        pid: information.dwProcessId,
    })
}

/// Reads the boundary back from Windows and refuses any mismatch.
fn verify(process: &Handle, job: &Job, profile: &Profile, less_privileged: bool) -> Result<()> {
    let token = token::of_process(process.raw())?;
    let package = token::package_sid(&token)?;
    let facts = Facts {
        in_job: job.contains(process.raw())?,
        appcontainer: token::is_appcontainer(&token)?,
        package_matches: package.as_deref() == Some(profile.sid_text()),
        privilege_matches: token::is_less_privileged(&token) == less_privileged,
    };
    if let Some(missing) = facts.refusal() {
        bail!("the launch started outside its boundary ({missing}); it was ended");
    }
    Ok(())
}

/// Lets the verified process run its first instruction.
fn resume(thread: &Handle) -> Result<()> {
    // SAFETY: the thread handle is live.
    if unsafe { ResumeThread(thread.raw()) } == u32::MAX {
        return Err(last_error("ResumeThread"));
    }
    Ok(())
}

/// When `process` was created, as a 64-bit `FILETIME`.
pub(crate) fn creation_time(process: HANDLE) -> Result<u64> {
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: the process handle is live and every output is writable.
    let read =
        unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) };
    check(read, "GetProcessTimes")?;
    Ok(u64::from(created.dwHighDateTime) << 32 | u64::from(created.dwLowDateTime))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_launch_runs_only_when_every_fact_is_as_asked() {
        let held = Facts {
            in_job: true,
            appcontainer: true,
            package_matches: true,
            privilege_matches: true,
        };
        assert_eq!(held.refusal(), None);
        let outside = Facts {
            in_job: false,
            privilege_matches: false,
            ..held
        };
        assert_eq!(
            outside.refusal().unwrap(),
            "job membership, less-privileged opt-out"
        );
        let wrong_package = Facts {
            package_matches: false,
            ..held
        };
        assert_eq!(wrong_package.refusal().unwrap(), "package SID");
    }
}
