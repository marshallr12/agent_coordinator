//! The Job Object that owns one launch's processes. It bounds how many
//! there are and how much memory they take, applies every user-interface
//! restriction, suppresses error dialogs and kills every member when its
//! last handle closes. Breakaway is never allowed. A job controls lifetime
//! and resources; it is not the access boundary.
use super::sys::{Handle, check};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::ffi::c_void;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_JOB_MEMORY,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_BASIC_UI_RESTRICTIONS,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation,
    JobObjectBasicUIRestrictions, JobObjectExtendedLimitInformation, QueryInformationJobObject,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::SystemServices::JOB_OBJECT_UILIMIT_ALL;

/// How often an emptying job is looked at.
const POLL: Duration = Duration::from_millis(20);
/// The exit code of a process the launcher killed.
const KILLED: u32 = 1;

/// The bounds of one launch's job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// The most processes alive in the job at once.
    pub processes: u32,
    /// The most memory one process may commit, in MiB.
    pub process_memory_mb: usize,
    /// The most memory the whole job may commit, in MiB.
    pub job_memory_mb: usize,
}

/// One launch's unnamed Job Object.
#[derive(Debug)]
pub struct Job(Handle);

impl Job {
    /// Creates an unnamed job with `limits` and every restriction applied.
    pub fn create(limits: Limits) -> Result<Self> {
        // SAFETY: no attributes and no name: an unnamed job only this
        // process holds a handle to.
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        let job = Self(Handle::owned(raw, "CreateJobObjectW")?);
        job.bound(limits)?;
        job.restrict_interface()?;
        Ok(job)
    }

    /// Applies the process, memory, dialog and kill-on-close limits.
    fn bound(&self, limits: Limits) -> Result<()> {
        // SAFETY: the structure is plain data; all-zero is a valid value.
        let mut information: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        information.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_ACTIVE_PROCESS
            | JOB_OBJECT_LIMIT_PROCESS_MEMORY
            | JOB_OBJECT_LIMIT_JOB_MEMORY
            | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION
            | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        information.BasicLimitInformation.ActiveProcessLimit = limits.processes;
        information.ProcessMemoryLimit = limits.process_memory_mb << 20;
        information.JobMemoryLimit = limits.job_memory_mb << 20;
        self.set(
            JobObjectExtendedLimitInformation,
            &information,
            "job limits",
        )
    }

    /// Applies every user-interface restriction a job offers.
    fn restrict_interface(&self) -> Result<()> {
        let restrictions = JOBOBJECT_BASIC_UI_RESTRICTIONS {
            UIRestrictionsClass: JOB_OBJECT_UILIMIT_ALL,
        };
        let class = JobObjectBasicUIRestrictions;
        self.set(class, &restrictions, "job user-interface restrictions")
    }

    /// Sets one information class of the job from `value`.
    fn set<T>(&self, class: i32, value: &T, operation: &str) -> Result<()> {
        let (pointer, size) = (std::ptr::from_ref(value).cast::<c_void>(), size_of::<T>());
        // SAFETY: `pointer` is readable for `size` bytes of the structure
        // this information class takes.
        let set = unsafe { SetInformationJobObject(self.0.raw(), class, pointer, size as u32) };
        check(set, operation)
    }

    /// The job handle, for process creation; it stays owned here.
    pub fn handle(&self) -> HANDLE {
        self.0.raw()
    }

    /// Whether `process` is a member of this job, as Windows reports it.
    pub(crate) fn contains(&self, process: HANDLE) -> Result<bool> {
        let mut inside = 0;
        // SAFETY: both handles are live and `inside` receives the answer.
        let asked = unsafe { IsProcessInJob(process, self.0.raw(), &mut inside) };
        check(asked, "IsProcessInJob")?;
        Ok(inside != 0)
    }

    /// How many processes the job holds now; `None` when Windows would not
    /// say.
    pub fn active(&self) -> Option<u32> {
        // SAFETY: the structure is plain data; all-zero is a valid value.
        let mut accounting: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
        let pointer = std::ptr::from_mut(&mut accounting).cast::<c_void>();
        let size = size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32;
        let class = JobObjectBasicAccountingInformation;
        // SAFETY: `pointer` is writable for `size` bytes; the length output
        // is optional.
        let read = unsafe {
            QueryInformationJobObject(self.0.raw(), class, pointer, size, std::ptr::null_mut())
        };
        (read != 0).then_some(accounting.ActiveProcesses)
    }

    /// Kills every process in the job without waiting for it to empty.
    pub fn kill(&self) {
        // SAFETY: the job handle is live; the exit code is a plain integer.
        unsafe { TerminateJobObject(self.0.raw(), KILLED) };
    }

    /// Kills every process in the job and waits up to `patience` for it to
    /// empty; true when Windows reports no process left.
    pub fn terminate(&self, patience: Duration) -> bool {
        self.kill();
        let deadline = Instant::now() + patience;
        loop {
            if self.active() == Some(0) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(POLL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    #[test]
    fn a_new_job_is_empty_and_does_not_hold_this_process() {
        let limits = Limits {
            processes: 4,
            process_memory_mb: 64,
            job_memory_mb: 128,
        };
        let job = Job::create(limits).unwrap();
        assert_eq!(job.active(), Some(0));
        // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no close.
        assert!(!job.contains(unsafe { GetCurrentProcess() }).unwrap());
        assert!(job.terminate(Duration::from_secs(1)));
    }
}
