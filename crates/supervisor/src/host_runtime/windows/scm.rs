//! The Windows Service Control Manager binding: the service dispatcher, the
//! control handler and status reports. All the decisions are the platform
//! neutral service core's ([`crate::service`]); this module hands it what
//! Windows says and tells Windows what it answers.
//!
//! The control handler runs on a thread of Windows' and returns at once: it
//! records a stop or preshutdown request in the shared [`Signal`], sets the
//! worker's wake event and answers. It never stops a launch or waits.
//!
//! Started from a console, [`dispatch`] fails at once with
//! [`NOT_A_SERVICE`]; it does not wait for a service manager that is not
//! there.
use super::sys::{Handle, check, last_error};
use super::text::wide;
use crate::service::control::{self, on_control};
use crate::service::lifecycle::{Report, Signal};
use anyhow::{Result, anyhow, bail};
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use windows_sys::Win32::Foundation::{
    ERROR_FAILED_SERVICE_CONTROLLER_CONNECT, ERROR_SERVICE_SPECIFIC_ERROR, GetLastError, HANDLE,
};
use windows_sys::Win32::System::Services::{
    ChangeServiceConfig2W, CloseServiceHandle, OpenSCManagerW, OpenServiceW,
    RegisterServiceCtrlHandlerExW, SC_HANDLE, SC_MANAGER_CONNECT, SERVICE_ACCEPT_PRESHUTDOWN,
    SERVICE_ACCEPT_STOP, SERVICE_CHANGE_CONFIG, SERVICE_CONFIG_PRESHUTDOWN_INFO,
    SERVICE_PRESHUTDOWN_INFO, SERVICE_RUNNING, SERVICE_START_PENDING, SERVICE_STATUS,
    SERVICE_STATUS_HANDLE, SERVICE_STOP_PENDING, SERVICE_STOPPED, SERVICE_TABLE_ENTRYW,
    SERVICE_WIN32_OWN_PROCESS, SetServiceStatus, StartServiceCtrlDispatcherW,
};
use windows_sys::Win32::System::Threading::{CreateEventW, ResetEvent, SetEvent};
use windows_sys::core::PWSTR;

/// What a console gets when it starts the service's subcommand.
pub const NOT_A_SERVICE: &str = "this command must be started by the Windows service manager; \
     it cannot be run from a console";

/// The service-specific exit code of a service that stopped with launches it
/// could not confirm ended.
pub const UNCONFIRMED: u32 = 1;
/// The service-specific exit code of a service that could not start.
pub const FAILED_TO_START: u32 = 2;

/// An event handle. Any thread may set or wait on one.
struct Event(Handle);

// SAFETY: an event handle is a process-wide value; every operation on it is
// thread-safe.
unsafe impl Sync for Event {}

/// What the control handler and the worker share.
struct Shared {
    signal: Arc<Signal>,
    /// Set by the handler when a request is recorded; the worker waits on it
    /// beside its own I/O.
    wake: Event,
}

static SHARED: OnceLock<Shared> = OnceLock::new();
static NAME: OnceLock<Vec<u16>> = OnceLock::new();

/// The work of the service, run on the thread Windows starts for it. It
/// returns whether the service stopped with nothing unconfirmed, or why it
/// could not run.
type Body = Box<dyn FnOnce(&Host) -> Result<bool> + Send>;

static BODY: Mutex<Option<Body>> = Mutex::new(None);
static FAILURE: Mutex<Option<String>> = Mutex::new(None);

/// The worker's side of the binding.
pub struct Host {
    status: SERVICE_STATUS_HANDLE,
    shared: &'static Shared,
}

/// The status a report becomes. While the service is stopping it still
/// accepts stop and preshutdown, so a preshutdown that arrives during a stop
/// is recorded and can shorten the drain.
pub fn status_of(report: &Report) -> SERVICE_STATUS {
    let accepts = SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_PRESHUTDOWN;
    let mut status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        ..SERVICE_STATUS::default()
    };
    match *report {
        Report::Running => {
            status.dwCurrentState = SERVICE_RUNNING;
            status.dwControlsAccepted = accepts;
        }
        Report::Stopping {
            checkpoint,
            wait_hint,
        } => {
            status.dwCurrentState = SERVICE_STOP_PENDING;
            status.dwControlsAccepted = accepts;
            status.dwCheckPoint = checkpoint;
            status.dwWaitHint = u32::try_from(wait_hint.as_millis()).unwrap_or(u32::MAX);
        }
        Report::Stopped { quiescent } => {
            status.dwCurrentState = SERVICE_STOPPED;
            if !quiescent {
                status.dwWin32ExitCode = ERROR_SERVICE_SPECIFIC_ERROR;
                status.dwServiceSpecificExitCode = UNCONFIRMED;
            }
        }
    }
    status
}

impl Host {
    /// The requests the control handler recorded.
    pub fn signal(&self) -> &Signal {
        &self.shared.signal
    }

    /// The event the control handler sets, for the worker's waits.
    pub fn wake(&self) -> HANDLE {
        self.shared.wake.0.raw()
    }

    /// Clears the wake event. Call it before looking at [`Host::signal`]: a
    /// request recorded after the reset sets the event again.
    pub fn reset_wake(&self) {
        // SAFETY: the event handle is live.
        unsafe { ResetEvent(self.wake()) };
    }

    /// Reports that the service is starting, with a checkpoint and the time
    /// Windows may wait for the next report.
    pub fn starting(&self, checkpoint: u32, wait_hint: Duration) -> Result<()> {
        let mut status = status_of(&Report::Running);
        status.dwCurrentState = SERVICE_START_PENDING;
        status.dwControlsAccepted = 0;
        status.dwCheckPoint = checkpoint;
        status.dwWaitHint = u32::try_from(wait_hint.as_millis()).unwrap_or(u32::MAX);
        self.send(&status)
    }

    /// Reports `report` to the Service Control Manager.
    pub fn report(&self, report: &Report) -> Result<()> {
        self.send(&status_of(report))
    }

    /// Reports that the service could not start.
    fn failed(&self) -> Result<()> {
        let mut status = status_of(&Report::Stopped { quiescent: true });
        status.dwWin32ExitCode = ERROR_SERVICE_SPECIFIC_ERROR;
        status.dwServiceSpecificExitCode = FAILED_TO_START;
        self.send(&status)
    }

    fn send(&self, status: &SERVICE_STATUS) -> Result<()> {
        // SAFETY: the status handle came from the registration and `status`
        // is a live structure.
        check(
            unsafe { SetServiceStatus(self.status, status) },
            "SetServiceStatus",
        )
    }
}

/// The control handler Windows calls. It records and answers; the work of a
/// stop is the worker's.
unsafe extern "system" fn handler(
    control: u32,
    _event_type: u32,
    _event_data: *mut c_void,
    _context: *mut c_void,
) -> u32 {
    let Some(shared) = SHARED.get() else {
        return control::NOT_IMPLEMENTED;
    };
    let handled = on_control(&shared.signal, control);
    if handled.wake {
        // SAFETY: the event handle is live for the whole process.
        unsafe { SetEvent(shared.wake.0.raw()) };
    }
    handled.answer
}

/// The service's entry point on the thread Windows starts for it.
unsafe extern "system" fn service_main(_arguments: u32, _argument_vector: *mut PWSTR) {
    let Some(name) = NAME.get() else { return };
    let created = Handle::owned(
        // SAFETY: no attributes and no name; a manual-reset event, not set.
        unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) },
        "CreateEventW",
    );
    let Ok(wake) = created else { return };
    let shared = SHARED.get_or_init(|| Shared {
        signal: Arc::new(Signal::default()),
        wake: Event(wake),
    });
    // SAFETY: the name is NUL-terminated and outlives the call; the
    // handler is a function that lives for the whole process.
    let status =
        unsafe { RegisterServiceCtrlHandlerExW(name.as_ptr(), Some(handler), std::ptr::null()) };
    if status.is_null() {
        let error = last_error("RegisterServiceCtrlHandlerExW");
        *FAILURE.lock().unwrap_or_else(|e| e.into_inner()) = Some(format!("{error:#}"));
        return;
    }
    let host = Host { status, shared };
    let body = BODY.lock().unwrap_or_else(|e| e.into_inner()).take();
    let Some(body) = body else { return };
    let outcome = catch_unwind(AssertUnwindSafe(|| body(&host)));
    let reported = match outcome {
        Ok(Ok(quiescent)) => host.report(&Report::Stopped { quiescent }),
        Ok(Err(error)) => {
            *FAILURE.lock().unwrap_or_else(|e| e.into_inner()) = Some(format!("{error:#}"));
            host.failed()
        }
        Err(_) => {
            *FAILURE.lock().unwrap_or_else(|e| e.into_inner()) =
                Some("the service stopped on a panic".into());
            host.failed()
        }
    };
    let _ = reported;
}

/// Runs `body` as the service called `name`: connects to the Service Control
/// Manager, registers the control handler and runs `body` on the service
/// thread until it returns, then reports the service stopped. Returns when
/// the service has stopped, with what stopped it if it failed. From a
/// console it fails at once with [`NOT_A_SERVICE`].
pub fn dispatch(
    name: &str,
    body: impl FnOnce(&Host) -> Result<bool> + Send + 'static,
) -> Result<()> {
    let mut service_name = wide(name)?;
    let _ = NAME.set(service_name.clone());
    *BODY.lock().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(body));
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: service_name.as_mut_ptr(),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: std::ptr::null_mut(),
            lpServiceProc: None,
        },
    ];
    // SAFETY: the table ends with a zeroed entry and, with the name it
    // points to, outlives the call.
    let connected = unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) };
    if connected == 0 {
        // SAFETY: reads the calling thread's last error.
        let code = unsafe { GetLastError() };
        if code == ERROR_FAILED_SERVICE_CONTROLLER_CONNECT {
            bail!(NOT_A_SERVICE);
        }
        bail!("StartServiceCtrlDispatcherW failed with Windows error {code}");
    }
    match FAILURE.lock().unwrap_or_else(|e| e.into_inner()).take() {
        Some(reason) => Err(anyhow!(reason)),
        None => Ok(()),
    }
}

/// An open handle of the Service Control Manager or of a service.
struct ScHandle(SC_HANDLE);

impl Drop for ScHandle {
    fn drop(&mut self) {
        // SAFETY: the handle is owned here and closed exactly once.
        unsafe { CloseServiceHandle(self.0) };
    }
}

/// Asks the Service Control Manager to give the service `name` `timeout` to
/// stop at preshutdown. Needs the right to change the service's
/// configuration, which the account the service runs as may not have; the
/// caller decides what a refusal means.
pub fn set_preshutdown_timeout(name: &str, timeout: Duration) -> Result<()> {
    let name = wide(name)?;
    // SAFETY: no machine name and no database name: the local active one.
    let manager = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT) };
    if manager.is_null() {
        return Err(last_error("OpenSCManagerW"));
    }
    let manager = ScHandle(manager);
    // SAFETY: the manager handle is live and the name is NUL-terminated.
    let service = unsafe { OpenServiceW(manager.0, name.as_ptr(), SERVICE_CHANGE_CONFIG) };
    if service.is_null() {
        return Err(last_error("OpenServiceW"));
    }
    let service = ScHandle(service);
    let info = SERVICE_PRESHUTDOWN_INFO {
        dwPreshutdownTimeout: u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX),
    };
    let pointer = std::ptr::from_ref(&info).cast::<c_void>();
    // SAFETY: the service handle is live and `pointer` is readable for the
    // structure this information level takes.
    let changed =
        unsafe { ChangeServiceConfig2W(service.0, SERVICE_CONFIG_PRESHUTDOWN_INFO, pointer) };
    check(changed, "ChangeServiceConfig2W")
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Foundation::NO_ERROR;
    use windows_sys::Win32::System::Services::{
        SERVICE_CONTROL_INTERROGATE, SERVICE_CONTROL_PRESHUTDOWN, SERVICE_CONTROL_STOP,
    };

    #[test]
    fn the_neutral_control_codes_are_the_ones_windows_defines() {
        assert_eq!(control::STOP, SERVICE_CONTROL_STOP);
        assert_eq!(control::INTERROGATE, SERVICE_CONTROL_INTERROGATE);
        assert_eq!(control::PRESHUTDOWN, SERVICE_CONTROL_PRESHUTDOWN);
        assert_eq!(control::HANDLED, NO_ERROR);
        assert_eq!(
            control::NOT_IMPLEMENTED,
            windows_sys::Win32::Foundation::ERROR_CALL_NOT_IMPLEMENTED
        );
    }

    #[test]
    fn a_stopping_service_reports_a_checkpoint_and_a_wait_hint_and_still_hears_preshutdown() {
        let report = Report::Stopping {
            checkpoint: 7,
            wait_hint: Duration::from_secs(6),
        };
        let status = status_of(&report);
        assert_eq!(status.dwCurrentState, SERVICE_STOP_PENDING);
        assert_eq!((status.dwCheckPoint, status.dwWaitHint), (7, 6000));
        assert_ne!(status.dwControlsAccepted & SERVICE_ACCEPT_PRESHUTDOWN, 0);
        assert_eq!(status.dwWin32ExitCode, 0);
    }

    #[test]
    fn a_running_service_accepts_stop_and_preshutdown_and_a_stopped_one_nothing() {
        let running = status_of(&Report::Running);
        assert_eq!(running.dwCurrentState, SERVICE_RUNNING);
        let both = SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_PRESHUTDOWN;
        assert_eq!(running.dwControlsAccepted, both);
        let stopped = status_of(&Report::Stopped { quiescent: true });
        assert_eq!(stopped.dwCurrentState, SERVICE_STOPPED);
        assert_eq!(
            (stopped.dwControlsAccepted, stopped.dwWin32ExitCode),
            (0, 0)
        );
    }

    #[test]
    fn a_stop_with_launches_unconfirmed_says_so_in_its_exit_code() {
        let status = status_of(&Report::Stopped { quiescent: false });
        assert_eq!(status.dwWin32ExitCode, ERROR_SERVICE_SPECIFIC_ERROR);
        assert_eq!(status.dwServiceSpecificExitCode, UNCONFIRMED);
    }

    #[test]
    fn started_from_a_console_the_dispatcher_refuses_at_once_and_says_why() {
        // An interactive or CI test process is not a service. If this ever
        // runs as one, the dispatcher would succeed; the test then proves
        // nothing and says so by failing loudly rather than hanging.
        let refused = dispatch("agentc-test", |_| Ok(true))
            .unwrap_err()
            .to_string();
        assert!(refused.contains("service manager"), "{refused}");
    }

    #[test]
    fn an_unknown_service_cannot_have_its_preshutdown_time_set() {
        // Opening the manager may be refused, and so may opening a service
        // that does not exist; either way nothing is changed.
        let name = "agentc-no-such-service-for-the-test";
        assert!(set_preshutdown_timeout(name, Duration::from_secs(1)).is_err());
    }
}
