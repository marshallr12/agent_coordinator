//! What Windows says about two tokens: whether this process is elevated,
//! and which privileges a running service's process holds. The account a
//! service runs as proves nothing about its privileges; its token does.
use super::super::sys::{Handle, check};
use super::super::{token, token_inventory};
use anyhow::{Context, Result};
use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TokenElevation};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// Whether this process's token is elevated.
pub fn elevated() -> Result<bool> {
    // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no close.
    let token = token::of_process(unsafe { GetCurrentProcess() })?;
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let (size, mut filled) = (size_of::<TOKEN_ELEVATION>() as u32, 0_u32);
    let buffer = std::ptr::from_mut(&mut elevation).cast();
    // SAFETY: the token is open for queries and `buffer` is writable for
    // `size` bytes, the structure this information class fills.
    let read =
        unsafe { GetTokenInformation(token.raw(), TokenElevation, buffer, size, &mut filled) };
    check(read, "GetTokenInformation")?;
    Ok(elevation.TokenIsElevated != 0)
}

/// The name of every privilege the token of the process `process` holds,
/// enabled or not.
pub fn privileges(process: u32) -> Result<Vec<String>> {
    // SAFETY: asks only for the right to query; the handle is not inherited.
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process) };
    let handle = Handle::owned(raw, "OpenProcess")?;
    let token = token::of_process(handle.raw()).context("opening the service's token")?;
    let inventory = token_inventory::of(&token)?;
    Ok(inventory.privileges.into_iter().map(|p| p.name).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_reads_its_own_elevation_and_privileges() {
        elevated().unwrap();
        let held = privileges(std::process::id()).unwrap();
        assert!(
            held.iter().any(|name| name == "SeChangeNotifyPrivilege"),
            "{held:?}"
        );
    }

    /// Reads the token of a service Windows runs under another account
    /// (the event log, as the local service). Nothing is changed.
    #[test]
    fn a_running_service_of_another_account_is_inventoried_when_elevated() {
        let service = super::super::services::observe("EventLog").unwrap();
        let process = service.and_then(|s| s.process).expect("the event log runs");
        if !elevated().unwrap() {
            eprintln!("skipped: reading another account's token needs elevation");
            return;
        }
        let held = privileges(process).unwrap();
        assert!(
            held.iter().any(|name| name == "SeChangeNotifyPrivilege"),
            "{held:?}"
        );
    }

    #[test]
    fn a_process_that_does_not_exist_is_an_error_not_an_empty_list() {
        assert!(privileges(u32::MAX - 3).is_err());
    }
}
