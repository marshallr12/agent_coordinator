//! The Task Scheduler as a process-creation broker.
//!
//! A scheduled task runs outside the launch's job and token, so reaching the
//! scheduler would be an escape. The check connects through the scheduler's COM
//! interface, as `schtasks.exe` does, and reports the result code. It only
//! connects: nothing is registered, so a success leaves nothing behind.

use crate::report::Report;
use std::ffi::c_void;
use std::io;

/// A COM class or interface identifier.
#[repr(C)]
struct Guid(u32, u16, u16, [u8; 8]);

/// `CLSID_TaskScheduler`.
const TASK_SCHEDULER: Guid = Guid(
    0x0F87_369F,
    0xA4E5,
    0x4CFC,
    [0xBD, 0x3E, 0x73, 0xE6, 0x15, 0x45, 0x72, 0xDD],
);
/// `IID_ITaskService`.
const TASK_SERVICE: Guid = Guid(
    0x2FAB_A4C7,
    0x4DA9,
    0x4013,
    [0x96, 0x97, 0x20, 0xCC, 0x3F, 0xD4, 0x0F, 0x85],
);
const IN_PROCESS_SERVER: u32 = 0x1;
/// Position of `Release` in every COM interface.
const RELEASE: usize = 2;
/// Position of `ITaskService::Connect`, after `IUnknown` and `IDispatch`.
const CONNECT: usize = 10;

/// An empty `VARIANT`: sixteen bytes on the wire, twenty-four in memory.
type Variant = [u64; 3];
type Release = unsafe extern "system" fn(*mut c_void) -> u32;
/// `Connect` takes four variants by value, which this ABI passes as pointers to copies.
type Connect = unsafe extern "system" fn(
    *mut c_void,
    *const Variant,
    *const Variant,
    *const Variant,
    *const Variant,
) -> i32;

#[link(name = "ole32")]
unsafe extern "system" {
    fn CoInitializeEx(reserved: *mut c_void, model: u32) -> i32;
    fn CoCreateInstance(
        class: *const Guid,
        outer: *mut c_void,
        context: u32,
        interface: *const Guid,
        object: *mut *mut c_void,
    ) -> i32;
    fn CoUninitialize();
}

/// Records whether the launch can connect to the local Task Scheduler service.
pub fn run(report: &mut Report) {
    report.attack("escape.scheduler_connect", in_helper());
}

/// Connects from a helper process and reads the result code from its exit status.
///
/// On the evaluated build a refused activation can end the calling process, so
/// the attempt must not share a process with the other checks.
fn in_helper() -> io::Result<()> {
    let status = super::process::helper(&["scheduler-connect"])?.status()?;
    match status.code() {
        Some(0) => Ok(()),
        code => Err(io::Error::from_raw_os_error(code.unwrap_or(-1))),
    }
}

/// Helper mode: exit status 0 when connected, otherwise the COM result code.
pub fn helper_status() -> i32 {
    match connect() {
        Ok(()) => 0,
        Err(error) => error.raw_os_error().unwrap_or(1),
    }
}

/// Initialises COM, connects to the scheduler and tears COM down again.
fn connect() -> io::Result<()> {
    // SAFETY: a null reserved pointer and the multithreaded model (0) are valid.
    hresult(unsafe { CoInitializeEx(std::ptr::null_mut(), 0) })?;
    let result = service().and_then(|service| service.connect());
    // SAFETY: balances the successful initialisation above.
    unsafe { CoUninitialize() };
    result
}

/// Creates the scheduler's in-process service object.
fn service() -> io::Result<Service> {
    let mut object: *mut c_void = std::ptr::null_mut();
    // SAFETY: both identifiers and the output pointer are live for the call.
    hresult(unsafe {
        CoCreateInstance(
            &TASK_SCHEDULER,
            std::ptr::null_mut(),
            IN_PROCESS_SERVER,
            &TASK_SERVICE,
            &mut object,
        )
    })?;
    Ok(Service(object))
}

/// An `ITaskService` reference released when dropped.
struct Service(*mut c_void);

impl Service {
    /// The interface method at `index`, read from the object's method table.
    ///
    /// # Safety
    /// `F` must be the real signature of the method at `index`.
    unsafe fn method<F: Copy>(&self, index: usize) -> F {
        // SAFETY: a COM object starts with a pointer to its method table, and the
        // caller guarantees the slot and signature.
        unsafe {
            let table = *(self.0 as *const *const *const c_void);
            std::mem::transmute_copy::<*const c_void, F>(&*table.add(index))
        }
    }

    /// Connects to the local scheduler with the caller's own identity.
    fn connect(&self) -> io::Result<()> {
        let empty: Variant = [0; 3];
        // SAFETY: slot 10 is `Connect` with this signature, and the four variants
        // are live, empty and unchanged by the call.
        hresult(unsafe { self.method::<Connect>(CONNECT)(self.0, &empty, &empty, &empty, &empty) })
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        // SAFETY: slot 2 is `Release`, and this reference is released exactly once.
        unsafe { self.method::<Release>(RELEASE)(self.0) };
    }
}

/// Turns a COM result code into a result; failures keep the code.
fn hresult(code: i32) -> io::Result<()> {
    if code < 0 {
        return Err(io::Error::from_raw_os_error(code));
    }
    Ok(())
}
