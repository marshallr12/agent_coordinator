//! The installer's side of the Service Control Manager: how a service is
//! registered, and the calls that register one or change one setting. What
//! the hand-run kits did with `sc.exe create`, `sc.exe failure` and a
//! script around `ChangeServiceConfig2W` is done here directly, so no
//! command line is ever built for another program to parse.
//!
//! Step 7a's `install` and `status` only register and read. Step 7b's
//! `update`, `rollback` and `uninstall` also start, stop and delete a
//! service, through [`start`], [`stop`] and [`delete`].
use super::super::sys::{check, last_error};
use super::super::text::wide;
use crate::windows_installer::control::RunState;
use crate::windows_installer::model::{Recovery, RecoveryAction, ServiceSpec, StartType};
use crate::windows_installer::observed::ServiceState;
use anyhow::{Context, Result, bail, ensure};
use std::ffi::c_void;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{
    ERROR_INSUFFICIENT_BUFFER, ERROR_SERVICE_ALREADY_RUNNING, ERROR_SERVICE_CANNOT_ACCEPT_CTRL,
    ERROR_SERVICE_DOES_NOT_EXIST, ERROR_SERVICE_MARKED_FOR_DELETE, ERROR_SERVICE_NOT_ACTIVE,
    GetLastError,
};
use windows_sys::Win32::Storage::FileSystem::DELETE;
use windows_sys::Win32::System::Services::{
    ChangeServiceConfig2W, ChangeServiceConfigW, CloseServiceHandle, ControlService,
    CreateServiceW, DeleteService, OpenSCManagerW, OpenServiceW, QUERY_SERVICE_CONFIGW,
    QueryServiceConfig2W, QueryServiceConfigW, QueryServiceStatusEx, SC_ACTION, SC_ACTION_NONE,
    SC_ACTION_RESTART, SC_HANDLE, SC_MANAGER_CONNECT, SC_MANAGER_CREATE_SERVICE,
    SC_STATUS_PROCESS_INFO, SERVICE_AUTO_START, SERVICE_CHANGE_CONFIG,
    SERVICE_CONFIG_FAILURE_ACTIONS, SERVICE_CONFIG_PRESHUTDOWN_INFO,
    SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO, SERVICE_CONTROL_STOP, SERVICE_DEMAND_START,
    SERVICE_DISABLED, SERVICE_ERROR_NORMAL, SERVICE_FAILURE_ACTIONSW, SERVICE_NO_CHANGE,
    SERVICE_PRESHUTDOWN_INFO, SERVICE_QUERY_CONFIG, SERVICE_QUERY_STATUS,
    SERVICE_REQUIRED_PRIVILEGES_INFOW, SERVICE_RUNNING, SERVICE_START, SERVICE_STATUS,
    SERVICE_STATUS_PROCESS, SERVICE_STOP, SERVICE_STOPPED, SERVICE_WIN32_OWN_PROCESS,
    StartServiceW,
};
use windows_sys::core::BOOL;

/// An open handle of the Service Control Manager or of a service.
struct Sc(SC_HANDLE);

impl Drop for Sc {
    fn drop(&mut self) {
        // SAFETY: the handle is owned here and closed exactly once.
        unsafe { CloseServiceHandle(self.0) };
    }
}

/// Connects to the local Service Control Manager with `access`.
fn manager(access: u32) -> Result<Sc> {
    // SAFETY: no machine name and no database name: the local active one.
    let handle = unsafe { OpenSCManagerW(null(), null(), access) };
    if handle.is_null() {
        return Err(last_error("OpenSCManagerW"));
    }
    Ok(Sc(handle))
}

/// Opens the service called `name` with `access`; `None` when no such
/// service is registered.
fn open(name: &str, access: u32) -> Result<Option<Sc>> {
    let (manager, wide_name) = (manager(SC_MANAGER_CONNECT)?, wide(name)?);
    // SAFETY: the manager handle is live and the name is NUL-terminated.
    let handle = unsafe { OpenServiceW(manager.0, wide_name.as_ptr(), access) };
    if !handle.is_null() {
        return Ok(Some(Sc(handle)));
    }
    // SAFETY: reads the calling thread's last error.
    match unsafe { GetLastError() } {
        ERROR_SERVICE_DOES_NOT_EXIST => Ok(None),
        code => bail!("opening the service {name} failed with Windows error {code}"),
    }
}

/// Opens a registered service to change it.
fn registered(name: &str, access: u32) -> Result<Sc> {
    open(name, access)?.with_context(|| format!("the service {name} is not registered"))
}

/// Runs a query that reports the room it needs, and returns what it wrote
/// in an aligned buffer. A query with nothing to say leaves the buffer
/// zeroed, which every structure read from it takes as empty.
fn query(operation: &str, call: impl Fn(*mut u8, u32, &mut u32) -> BOOL) -> Result<Vec<u64>> {
    let mut needed = 0_u32;
    let mut buffer = vec![0_u64; 8];
    if call(null_mut(), 0, &mut needed) != 0 {
        return Ok(buffer);
    }
    // SAFETY: reads the calling thread's last error.
    let code = unsafe { GetLastError() };
    ensure!(
        code == ERROR_INSUFFICIENT_BUFFER,
        "{operation} failed with Windows error {code}"
    );
    buffer.resize((needed as usize).div_ceil(8).max(8), 0);
    let size = (buffer.len() * 8) as u32;
    check(
        call(buffer.as_mut_ptr().cast(), size, &mut needed),
        operation,
    )?;
    Ok(buffer)
}

/// One level of a service's extended configuration.
fn extended(service: &Sc, level: u32, operation: &str) -> Result<Vec<u64>> {
    // SAFETY: the service handle is live; the pointer is writable for the
    // size given with it.
    query(operation, |pointer, size, needed| unsafe {
        QueryServiceConfig2W(service.0, level, pointer, size, needed)
    })
}

/// The NUL-terminated UTF-16 string at `text`; empty for a null pointer.
///
/// # Safety
/// `text` is null or points to a NUL-terminated string that is live.
unsafe fn text(text: *const u16) -> String {
    if text.is_null() {
        return String::new();
    }
    // SAFETY: the caller's string ends with a NUL before `length`.
    let length = (0..).take_while(|&i| unsafe { *text.add(i) } != 0).count();
    // SAFETY: `length` units before the terminator are initialized.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) })
}

/// The strings of the double-NUL-terminated list at `list`.
///
/// # Safety
/// `list` is null or points to a live list that ends with an empty string.
unsafe fn strings(list: *const u16) -> Vec<String> {
    let mut found = Vec::new();
    let mut next = list;
    loop {
        // SAFETY: `next` is null or at the start of a string of the list.
        let one = unsafe { text(next) };
        if one.is_empty() {
            return found;
        }
        // SAFETY: the string and its terminator lie inside the list.
        next = unsafe { next.add(one.encode_utf16().count() + 1) };
        found.push(one);
    }
}

/// `names` as a double-NUL-terminated list.
fn list(names: &[String]) -> Result<Vec<u16>> {
    let mut units = Vec::new();
    for name in names {
        units.extend(wide(name)?);
    }
    if names.is_empty() {
        units.push(0);
    }
    units.push(0);
    Ok(units)
}

/// How the service called `name` is registered, when it is.
pub fn observe(name: &str) -> Result<Option<ServiceState>> {
    let Some(service) = open(name, SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS)? else {
        return Ok(None);
    };
    let mut spec = basic(&service, name)?;
    spec.recovery = recovery(&service)?;
    spec.preshutdown_ms = preshutdown(&service)?;
    spec.privileges = privileges(&service)?;
    let process = process(&service)?;
    Ok(Some(ServiceState { spec, process }))
}

/// The settings `sc.exe qc` prints; the rest are filled in by the caller.
fn basic(service: &Sc, name: &str) -> Result<ServiceSpec> {
    // SAFETY: the service handle is live; the pointer is writable for the
    // size given with it.
    let buffer = query("QueryServiceConfigW", |pointer, size, needed| unsafe {
        QueryServiceConfigW(service.0, pointer.cast(), size, needed)
    })?;
    // SAFETY: Windows wrote a QUERY_SERVICE_CONFIGW whose strings lie in
    // `buffer`, which is aligned and lives until this returns.
    let raw = unsafe { &*buffer.as_ptr().cast::<QUERY_SERVICE_CONFIGW>() };
    Ok(ServiceSpec {
        name: name.to_owned(),
        // SAFETY: each pointer is null or a string inside `buffer`.
        account: unsafe { text(raw.lpServiceStartName) },
        bin_path: unsafe { text(raw.lpBinaryPathName) },
        start: start_type(raw.dwStartType),
        dependencies: unsafe { strings(raw.lpDependencies) },
        recovery: Recovery {
            reset_seconds: 0,
            actions: Vec::new(),
            command: String::new(),
        },
        preshutdown_ms: 0,
        privileges: Vec::new(),
    })
}

/// A start type as the model names it.
fn start_type(code: u32) -> StartType {
    match code {
        SERVICE_AUTO_START => StartType::Auto,
        SERVICE_DEMAND_START => StartType::Demand,
        SERVICE_DISABLED => StartType::Disabled,
        _ => StartType::Other,
    }
}

/// The start type's number; only the two a service of ours may have.
fn start_code(start: StartType) -> Result<u32> {
    match start {
        StartType::Auto => Ok(SERVICE_AUTO_START),
        StartType::Demand => Ok(SERVICE_DEMAND_START),
        other => bail!("{other:?} is not a start type the installer sets"),
    }
}

/// What Windows does when the service fails.
fn recovery(service: &Sc) -> Result<Recovery> {
    let buffer = extended(
        service,
        SERVICE_CONFIG_FAILURE_ACTIONS,
        "reading failure actions",
    )?;
    // SAFETY: Windows wrote a SERVICE_FAILURE_ACTIONSW (or nothing, which
    // leaves zeroes) whose array and strings lie in `buffer`.
    let raw = unsafe { &*buffer.as_ptr().cast::<SERVICE_FAILURE_ACTIONSW>() };
    let actions: &[SC_ACTION] = match raw.lpsaActions.is_null() {
        true => &[],
        // SAFETY: the array holds `cActions` entries inside `buffer`.
        false => unsafe { std::slice::from_raw_parts(raw.lpsaActions, raw.cActions as usize) },
    };
    Ok(Recovery {
        reset_seconds: raw.dwResetPeriod,
        actions: actions.iter().map(recovery_action).collect(),
        // SAFETY: the pointer is null or a string inside `buffer`.
        command: unsafe { text(raw.lpCommand) },
    })
}

/// One failure action as the model names it.
fn recovery_action(raw: &SC_ACTION) -> RecoveryAction {
    match raw.Type {
        SC_ACTION_RESTART => RecoveryAction::Restart(raw.Delay),
        SC_ACTION_NONE => RecoveryAction::None,
        other => RecoveryAction::Other(other as u32),
    }
}

/// The time the service gets to stop at preshutdown, in milliseconds.
fn preshutdown(service: &Sc) -> Result<u32> {
    let buffer = extended(
        service,
        SERVICE_CONFIG_PRESHUTDOWN_INFO,
        "reading the preshutdown limit",
    )?;
    // SAFETY: Windows wrote a SERVICE_PRESHUTDOWN_INFO into the aligned buffer.
    Ok(unsafe { &*buffer.as_ptr().cast::<SERVICE_PRESHUTDOWN_INFO>() }.dwPreshutdownTimeout)
}

/// The only privileges the service's token may hold; empty when Windows
/// was never given a list, which leaves the account's defaults.
fn privileges(service: &Sc) -> Result<Vec<String>> {
    let level = SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO;
    let buffer = extended(service, level, "reading required privileges")?;
    // SAFETY: Windows wrote a SERVICE_REQUIRED_PRIVILEGES_INFOW (or nothing)
    // whose list lies in `buffer`.
    let raw = unsafe { &*buffer.as_ptr().cast::<SERVICE_REQUIRED_PRIVILEGES_INFOW>() };
    // SAFETY: the pointer is null or a double-NUL-terminated list.
    Ok(unsafe { strings(raw.pmszRequiredPrivileges) })
}

/// The service's process, while it has one.
fn process(service: &Sc) -> Result<Option<u32>> {
    // SAFETY: the service handle is live; the pointer is writable for the
    // size given with it.
    let buffer = query("QueryServiceStatusEx", |pointer, size, needed| unsafe {
        QueryServiceStatusEx(service.0, SC_STATUS_PROCESS_INFO, pointer, size, needed)
    })?;
    // SAFETY: Windows wrote a SERVICE_STATUS_PROCESS into the aligned buffer.
    let id = unsafe { &*buffer.as_ptr().cast::<SERVICE_STATUS_PROCESS>() }.dwProcessId;
    Ok((id != 0).then_some(id))
}

/// Registers `spec`'s service, stopped, under its own virtual account.
/// Windows makes that account with the service. It has no password, and
/// Windows wants none passed: an empty one is refused with error 1057.
pub fn create(spec: &ServiceSpec) -> Result<()> {
    let manager = manager(SC_MANAGER_CREATE_SERVICE)?;
    let (name, path) = (wide(&spec.name)?, wide(&spec.bin_path)?);
    let (account, depends) = (wide(&spec.account)?, list(&spec.dependencies)?);
    let start = start_code(spec.start)?;
    // SAFETY: every string is NUL-terminated and outlives the call; no
    // load-order group or tag is asked for.
    #[rustfmt::skip]
    let handle = unsafe {
        CreateServiceW(manager.0, name.as_ptr(), name.as_ptr(), SERVICE_QUERY_CONFIG,
            SERVICE_WIN32_OWN_PROCESS, start, SERVICE_ERROR_NORMAL, path.as_ptr(), null(),
            null_mut(), depends.as_ptr(), account.as_ptr(), null())
    };
    if handle.is_null() {
        return Err(last_error("CreateServiceW"));
    }
    drop(Sc(handle));
    Ok(())
}

/// The settings `ChangeServiceConfigW` takes; `None` leaves one as it is.
#[derive(Default)]
struct Basic<'a> {
    start: Option<StartType>,
    bin_path: Option<&'a str>,
    dependencies: Option<&'a [String]>,
    account: Option<&'a str>,
}

/// A pointer to `units`, or null to leave the setting as it is.
fn pointer(units: &Option<Vec<u16>>) -> *const u16 {
    units.as_ref().map_or(null(), |units| units.as_ptr())
}

/// Changes the given settings of the service called `name`.
fn change(name: &str, basic: Basic) -> Result<()> {
    let service = registered(name, SERVICE_CHANGE_CONFIG)?;
    let start = basic
        .start
        .map(start_code)
        .transpose()?
        .unwrap_or(SERVICE_NO_CHANGE);
    let path = basic.bin_path.map(wide).transpose()?;
    let depends = basic.dependencies.map(list).transpose()?;
    let account = basic.account.map(wide).transpose()?;
    // A virtual account has no password, and none is passed with it.
    // SAFETY: the handle is live; each pointer is null or a NUL-terminated
    // string that outlives the call.
    #[rustfmt::skip]
    let changed = unsafe {
        ChangeServiceConfigW(service.0, SERVICE_NO_CHANGE, start, SERVICE_NO_CHANGE,
            pointer(&path), null(), null_mut(), pointer(&depends), pointer(&account),
            null(), null())
    };
    check(changed, "ChangeServiceConfigW")
}

/// Sets the service's command line.
pub fn set_bin_path(spec: &ServiceSpec) -> Result<()> {
    let bin_path = Some(spec.bin_path.as_str());
    change(
        &spec.name,
        Basic {
            bin_path,
            ..Basic::default()
        },
    )
}

/// Sets when Windows starts the service.
pub fn set_start_type(spec: &ServiceSpec) -> Result<()> {
    let start = Some(spec.start);
    change(
        &spec.name,
        Basic {
            start,
            ..Basic::default()
        },
    )
}

/// Sets the services the service waits for.
pub fn set_dependencies(spec: &ServiceSpec) -> Result<()> {
    let dependencies = Some(&spec.dependencies[..]);
    change(
        &spec.name,
        Basic {
            dependencies,
            ..Basic::default()
        },
    )
}

/// Sets the account the service runs as.
pub fn set_account(spec: &ServiceSpec) -> Result<()> {
    let account = Some(spec.account.as_str());
    change(
        &spec.name,
        Basic {
            account,
            ..Basic::default()
        },
    )
}

/// Sets one level of the service's extended configuration from `info`.
fn set_extended<T>(spec: &ServiceSpec, access: u32, level: u32, info: &T) -> Result<()> {
    let service = registered(&spec.name, SERVICE_CHANGE_CONFIG | access)?;
    let info = std::ptr::from_ref(info).cast::<c_void>();
    // SAFETY: the handle is live and `info` is the structure `level` takes,
    // alive with everything it points to until the call returns.
    let changed = unsafe { ChangeServiceConfig2W(service.0, level, info) };
    check(changed, "ChangeServiceConfig2W")
}

/// One failure action as Windows takes it.
fn sc_action(action: &RecoveryAction) -> Result<SC_ACTION> {
    match *action {
        RecoveryAction::Restart(delay) => Ok(SC_ACTION {
            Type: SC_ACTION_RESTART,
            Delay: delay,
        }),
        RecoveryAction::None => Ok(SC_ACTION {
            Type: SC_ACTION_NONE,
            Delay: 0,
        }),
        RecoveryAction::Other(kind) => bail!("the installer never sets failure action {kind}"),
    }
}

/// Sets what Windows does when the service fails, and clears any program
/// or restart message a failure would use. A restart action needs the
/// right to start the service as well as to configure it.
pub fn set_recovery(spec: &ServiceSpec) -> Result<()> {
    ensure!(
        spec.recovery.command.is_empty(),
        "the installer never has Windows run a program"
    );
    let mut actions: Vec<SC_ACTION> = spec
        .recovery
        .actions
        .iter()
        .map(sc_action)
        .collect::<Result<_>>()?;
    // An empty string, unlike a null pointer, removes what was set.
    let mut empty = [0_u16];
    let info = SERVICE_FAILURE_ACTIONSW {
        dwResetPeriod: spec.recovery.reset_seconds,
        lpRebootMsg: empty.as_mut_ptr(),
        lpCommand: empty.as_mut_ptr(),
        cActions: actions.len() as u32,
        lpsaActions: actions.as_mut_ptr(),
    };
    set_extended(spec, SERVICE_START, SERVICE_CONFIG_FAILURE_ACTIONS, &info)
}

/// Sets the time the service gets to stop at preshutdown. A service's own
/// account may not set it (error 5 on every phase 4c run): it is the
/// installer's.
pub fn set_preshutdown(spec: &ServiceSpec) -> Result<()> {
    let info = SERVICE_PRESHUTDOWN_INFO {
        dwPreshutdownTimeout: spec.preshutdown_ms,
    };
    set_extended(spec, 0, SERVICE_CONFIG_PRESHUTDOWN_INFO, &info)
}

/// Sets the only privileges the service's token may hold. Windows removes
/// every other privilege from the token when it starts the service.
pub fn set_privileges(spec: &ServiceSpec) -> Result<()> {
    let mut names = list(&spec.privileges)?;
    let info = SERVICE_REQUIRED_PRIVILEGES_INFOW {
        pmszRequiredPrivileges: names.as_mut_ptr(),
    };
    set_extended(spec, 0, SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO, &info)
}

/// The service's status, as `QueryServiceStatusEx` reports it.
fn status(service: &Sc) -> Result<SERVICE_STATUS_PROCESS> {
    // SAFETY: the service handle is live; the pointer is writable for the
    // size given with it.
    let buffer = query("QueryServiceStatusEx", |pointer, size, needed| unsafe {
        QueryServiceStatusEx(service.0, SC_STATUS_PROCESS_INFO, pointer, size, needed)
    })?;
    // SAFETY: Windows wrote a SERVICE_STATUS_PROCESS into the aligned buffer.
    Ok(unsafe { *buffer.as_ptr().cast::<SERVICE_STATUS_PROCESS>() })
}

/// Where the service called `name` stands (phase 7, step 7b).
pub fn state(name: &str) -> Result<RunState> {
    let Some(service) = open(name, SERVICE_QUERY_STATUS)? else {
        return Ok(RunState::Missing);
    };
    let raw = status(&service)?;
    Ok(match raw.dwCurrentState {
        SERVICE_STOPPED => RunState::Stopped {
            exit: raw.dwWin32ExitCode,
            specific: raw.dwServiceSpecificExitCode,
        },
        SERVICE_RUNNING => RunState::Running,
        _ => RunState::Pending,
    })
}

/// Asks Windows to start the service called `name`, with no arguments: it
/// runs the command line it is registered with. One already running is
/// left as it is.
pub fn start(name: &str) -> Result<()> {
    let service = registered(name, SERVICE_START)?;
    // SAFETY: the handle is live; no arguments are passed.
    if unsafe { StartServiceW(service.0, 0, null()) } != 0 {
        return Ok(());
    }
    // SAFETY: reads the calling thread's last error.
    match unsafe { GetLastError() } {
        ERROR_SERVICE_ALREADY_RUNNING => Ok(()),
        code => bail!("starting the service {name} failed with Windows error {code}"),
    }
}

/// Asks the service called `name` to stop. One that is not running, or
/// cannot take a stop now because it is already stopping, is left as it is.
pub fn stop(name: &str) -> Result<()> {
    let service = registered(name, SERVICE_STOP)?;
    let mut status = SERVICE_STATUS::default();
    // SAFETY: the handle is live and `status` is writable.
    if unsafe { ControlService(service.0, SERVICE_CONTROL_STOP, &mut status) } != 0 {
        return Ok(());
    }
    // SAFETY: reads the calling thread's last error.
    match unsafe { GetLastError() } {
        ERROR_SERVICE_NOT_ACTIVE | ERROR_SERVICE_CANNOT_ACCEPT_CTRL => Ok(()),
        code => bail!("stopping the service {name} failed with Windows error {code}"),
    }
}

/// Deletes the service called `name`. Windows removes it once nothing
/// holds a handle to it; one that is not registered, or is already marked
/// for deletion, is done.
pub fn delete(name: &str) -> Result<()> {
    let Some(service) = open(name, DELETE)? else {
        return Ok(());
    };
    // SAFETY: the handle is live and was opened with DELETE.
    if unsafe { DeleteService(service.0) } != 0 {
        return Ok(());
    }
    // SAFETY: reads the calling thread's last error.
    match unsafe { GetLastError() } {
        ERROR_SERVICE_MARKED_FOR_DELETE => Ok(()),
        code => bail!("deleting the service {name} failed with Windows error {code}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_service_that_is_not_registered_is_missing_and_deleting_it_is_done() {
        assert_eq!(
            state("agentc-no-such-service-7b").unwrap(),
            RunState::Missing
        );
        delete("agentc-no-such-service-7b").unwrap();
        assert!(start("agentc-no-such-service-7b").is_err());
    }

    /// Reads a service Windows itself runs. Nothing is changed.
    #[test]
    fn a_running_service_reads_as_running() {
        assert_eq!(state("EventLog").unwrap(), RunState::Running);
    }

    #[test]
    fn a_list_ends_with_an_empty_string_and_reads_back() {
        let names = [
            "SeChangeNotifyPrivilege".to_owned(),
            "SeImpersonatePrivilege".to_owned(),
        ];
        let units = list(&names).unwrap();
        assert_eq!(&units[units.len() - 2..], [0, 0]);
        // SAFETY: `units` is a live double-NUL-terminated list.
        assert_eq!(unsafe { strings(units.as_ptr()) }, names);
        assert_eq!(list(&[]).unwrap(), [0, 0]);
        // SAFETY: a null pointer and an empty list are both accepted.
        assert!(unsafe { strings(null()) }.is_empty());
        assert!(unsafe { strings([0_u16, 0].as_ptr()) }.is_empty());
    }

    #[test]
    fn a_service_that_is_not_registered_is_none() {
        assert_eq!(observe("agentc-no-such-service-7a").unwrap(), None);
    }

    /// Reads a service Windows itself registers. Nothing is changed.
    #[test]
    fn a_registered_service_reads_back_whole() {
        let seen = observe("EventLog").unwrap().expect("the event log service");
        assert_eq!(seen.spec.name, "EventLog");
        assert!(
            seen.spec.bin_path.to_lowercase().contains("svchost.exe"),
            "{seen:?}"
        );
        assert_eq!(seen.spec.start, StartType::Auto);
        assert!(!seen.spec.account.is_empty(), "{seen:?}");
        assert!(seen.process.is_some(), "the event log runs: {seen:?}");
        let held = &seen.spec.privileges;
        assert!(held.iter().all(|name| name.starts_with("Se")), "{held:?}");
    }

    #[test]
    fn only_demand_and_automatic_start_are_ever_set() {
        assert_eq!(start_code(StartType::Demand).unwrap(), SERVICE_DEMAND_START);
        assert_eq!(start_code(StartType::Auto).unwrap(), SERVICE_AUTO_START);
        assert!(start_code(StartType::Disabled).is_err());
        assert_eq!(start_type(SERVICE_DISABLED), StartType::Disabled);
        assert_eq!(start_type(0), StartType::Other);
    }
}
