//! Taking over a window station Windows made before the launcher ran.
//!
//! A service's logon session already has its unnamed station when the
//! service's first instruction runs, so the launcher cannot make it. It may
//! use that station only when Windows confirms every fact below; otherwise
//! it refuses, exactly as it does for any station it did not make.
//!
//! 1. The station is the one named after this process's own logon session.
//! 2. It is the station this process runs in: Windows attached the process
//!    to it at start, as it does for a service.
//! 3. Its owner is this account, the system or administrators. An owner can
//!    always rewrite an access list, so any other owner keeps control.
//! 4. No other process is in the logon session, so nobody the launcher does
//!    not trust can hold a handle from before the access list is rewritten.
//!
//! The launcher then rewrites the access list and reads it back
//! ([`allowed`]). What this does not establish is recorded in
//! `windows-host-design.md`.
use super::desktop::StationNotOurs;
use super::sys::{Handle, check, sid_text};
use super::token;
use anyhow::{Result, ensure};
use std::ffi::c_void;
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, GetLastError, HANDLE,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, AclSizeInformation, DACL_SECURITY_INFORMATION,
    GetAce, GetAclInformation, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner,
    GetUserObjectSecurity, OWNER_SECURITY_INFORMATION, PSID, TOKEN_QUERY,
};
use windows_sys::Win32::System::ProcessStatus::K32EnumProcesses;
use windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// The system account and the administrators group.
const TRUSTED_OWNERS: [&str; 2] = ["S-1-5-18", "S-1-5-32-544"];
/// Room for a station's or desktop's security descriptor.
const DESCRIPTOR_BYTES: usize = 4096;
/// Room for the id of every process on the machine.
const MOST_PROCESSES: usize = 16_384;

/// The name Windows gives the unnamed station of logon session `session`.
pub fn station_name(session: u64) -> String {
    format!("Service-0x{:x}-{:x}$", session >> 32, session & 0xFFFF_FFFF)
}

/// A refusal to adopt, for `reason`.
fn refuse<T>(reason: String) -> Result<T> {
    Err(StationNotOurs(reason).into())
}

/// The facts about the station that do not depend on other processes:
/// `name` is this logon session's station and `process_station` is the
/// same one.
pub fn is_own(name: &str, process_station: &str, session: u64) -> Result<()> {
    let expected = station_name(session);
    if !name.eq_ignore_ascii_case(&expected) {
        return refuse(format!("it is {name}, not this logon session's {expected}"));
    }
    if !process_station.eq_ignore_ascii_case(name) {
        return refuse(format!(
            "this process runs in {process_station}, so Windows did not make {name} for it"
        ));
    }
    Ok(())
}

/// Whether `owner_of_station` may be left owning a station `account` adopts.
pub fn owner_is_trusted(owner_of_station: &str, account: &str) -> Result<()> {
    if owner_of_station == account || TRUSTED_OWNERS.contains(&owner_of_station) {
        return Ok(());
    }
    refuse(format!(
        "its owner is {owner_of_station}, who can rewrite its access list"
    ))
}

/// Refuses unless this process is alone in logon session `session`.
pub fn is_alone(session: u64) -> Result<()> {
    let others = companions(session)?;
    if others.is_empty() {
        return Ok(());
    }
    refuse(format!(
        "processes {others:?} share the logon session and may hold it open"
    ))
}

/// Part of the security descriptor of the station or desktop `object`, in
/// an aligned buffer.
pub(super) fn descriptor(object: HANDLE, requested: u32) -> Result<Vec<u64>> {
    let mut buffer = vec![0_u64; DESCRIPTOR_BYTES / 8];
    let (pointer, mut needed) = (buffer.as_mut_ptr().cast::<c_void>(), 0_u32);
    // SAFETY: `pointer` is writable for `DESCRIPTOR_BYTES` bytes; `requested`
    // and `needed` are live.
    let read = unsafe {
        let size = DESCRIPTOR_BYTES as u32;
        GetUserObjectSecurity(object, &requested, pointer, size, &mut needed)
    };
    check(read, "GetUserObjectSecurity")?;
    Ok(buffer)
}

/// The account that owns the station or desktop `object`.
pub fn owner(object: HANDLE) -> Result<String> {
    let buffer = descriptor(object, OWNER_SECURITY_INFORMATION)?;
    let (mut sid, mut defaulted): (PSID, i32) = (std::ptr::null_mut(), 0);
    // SAFETY: `buffer` holds a descriptor; `sid` points into it afterwards.
    let read = unsafe {
        GetSecurityDescriptorOwner(buffer.as_ptr() as *mut c_void, &mut sid, &mut defaulted)
    };
    check(read, "GetSecurityDescriptorOwner")?;
    sid_text(sid)
}

/// The access list inside the descriptor in `buffer`. An object without
/// one admits everyone, so that is an error here.
fn access_list(buffer: &[u64]) -> Result<*mut ACL> {
    let (mut present, mut defaulted) = (0, 0);
    let mut list: *mut ACL = std::ptr::null_mut();
    // SAFETY: `buffer` holds a descriptor; `list` points into it afterwards.
    let read = unsafe {
        let descriptor = buffer.as_ptr() as *mut c_void;
        GetSecurityDescriptorDacl(descriptor, &mut present, &mut list, &mut defaulted)
    };
    check(read, "GetSecurityDescriptorDacl")?;
    ensure!(
        present != 0 && !list.is_null(),
        "the object has no access list"
    );
    Ok(list)
}

/// How many entries `list` has.
fn entries(list: *const ACL) -> Result<u32> {
    let mut size = ACL_SIZE_INFORMATION::default();
    // SAFETY: `list` is a valid access list and `size` is writable for its
    // own length.
    let read = unsafe {
        let (pointer, length) = ((&raw mut size).cast(), size_of::<ACL_SIZE_INFORMATION>());
        GetAclInformation(list, pointer, length as u32, AclSizeInformation)
    };
    check(read, "GetAclInformation")?;
    Ok(size.AceCount)
}

/// The account entry `index` of `list` allows. Any other kind of entry is
/// an error: the launcher writes only plain allow entries.
fn allowed_entry(list: *const ACL, index: u32) -> Result<String> {
    let mut entry: *mut c_void = std::ptr::null_mut();
    // SAFETY: `list` is a valid access list and `index` is below its count.
    check(unsafe { GetAce(list, index, &mut entry) }, "GetAce")?;
    let entry = entry.cast::<ACCESS_ALLOWED_ACE>();
    // SAFETY: every entry starts with a header that names its type.
    let kind = unsafe { (*entry).Header.AceType };
    ensure!(
        u32::from(kind) == ACCESS_ALLOWED_ACE_TYPE,
        "access list entry {index} is of type {kind}, not a plain allow"
    );
    // SAFETY: an allow entry's SID starts at its `SidStart` field.
    sid_text(unsafe { (&raw mut (*entry).SidStart).cast() })
}

/// Every account the access list of the station or desktop `object` allows,
/// in order, as Windows reports it.
pub fn allowed(object: HANDLE) -> Result<Vec<String>> {
    let buffer = descriptor(object, DACL_SECURITY_INFORMATION)?;
    let list = access_list(&buffer)?;
    (0..entries(list)?)
        .map(|index| allowed_entry(list, index))
        .collect()
}

/// The id of every process Windows lists.
fn process_ids() -> Result<Vec<u32>> {
    let mut ids = vec![0_u32; MOST_PROCESSES];
    let (size, mut filled) = ((MOST_PROCESSES * 4) as u32, 0_u32);
    // SAFETY: `ids` is writable for `size` bytes and `filled` is live.
    let listed = unsafe { K32EnumProcesses(ids.as_mut_ptr(), size, &mut filled) };
    check(listed, "K32EnumProcesses")?;
    ensure!(filled < size, "there are more processes than can be listed");
    ids.truncate(filled as usize / 4);
    Ok(ids)
}

/// `Ok(None)` when the failed call's error says the process is gone or
/// belongs to an account that does not let this one look; such a process is
/// not this account's own. Any other error is unknown and is returned.
fn not_ours<T>(operation: &str) -> Result<Option<T>> {
    // SAFETY: GetLastError reads thread-local state.
    let code = unsafe { GetLastError() };
    if code == ERROR_ACCESS_DENIED || code == ERROR_INVALID_PARAMETER {
        return Ok(None);
    }
    anyhow::bail!("{operation} failed with Windows error {code}")
}

/// The logon session of process `pid`, or `None` when Windows will not let
/// this account look at it ([`not_ours`]).
fn session_of(pid: u32) -> Result<Option<u64>> {
    // SAFETY: OpenProcess takes plain integers and returns a new handle.
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if raw.is_null() {
        return not_ours("OpenProcess");
    }
    let process = Handle::owned(raw, "OpenProcess")?;
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: `process` is a live process handle and `token` receives one.
    if unsafe { OpenProcessToken(process.raw(), TOKEN_QUERY, &mut token) } == 0 {
        return not_ours("OpenProcessToken");
    }
    let token = Handle::owned(token, "OpenProcessToken")?;
    token::logon_session(&token).map(Some)
}

/// Every other process in logon session `session`.
pub fn companions(session: u64) -> Result<Vec<u32>> {
    // SAFETY: GetCurrentProcessId takes no arguments.
    let me = unsafe { GetCurrentProcessId() };
    let mut found = Vec::new();
    for pid in process_ids()? {
        if pid != me && session_of(pid)? == Some(session) {
            found.push(pid);
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "S-1-5-80-1-2-3-4-5";

    /// Whether `outcome` is the typed refusal.
    fn refused(outcome: Result<()>) -> bool {
        outcome.is_err_and(|error| error.downcast_ref::<StationNotOurs>().is_some())
    }

    #[test]
    fn a_station_is_named_after_its_logon_session() {
        assert_eq!(station_name(0x3e7), "Service-0x0-3e7$");
        assert_eq!(station_name(0x1_0002_abcd), "Service-0x1-2abcd$");
    }

    #[test]
    fn only_the_sessions_own_station_this_process_runs_in_is_its_own() {
        let name = station_name(0x51f3);
        assert!(is_own(&name, &name.to_lowercase(), 0x51f3).is_ok());
        assert!(refused(is_own(&name, &name, 0x51f4)), "another session's");
        assert!(refused(is_own(&name, "WinSta0", 0x51f3)), "not attached");
    }

    #[test]
    fn only_this_account_the_system_and_administrators_may_own_it() {
        for owner in [ME, "S-1-5-18", "S-1-5-32-544"] {
            assert!(owner_is_trusted(owner, ME).is_ok(), "{owner}");
        }
        for owner in ["S-1-5-21-1-2-3-1001", "S-1-1-0", "S-1-5-5-0-77"] {
            assert!(refused(owner_is_trusted(owner, ME)), "{owner}");
        }
    }

    #[test]
    fn this_process_is_in_its_own_logon_session_and_is_not_its_own_companion() {
        let session = token::current_logon_session().unwrap();
        // SAFETY: GetCurrentProcessId takes no arguments.
        let me = unsafe { GetCurrentProcessId() };
        assert_eq!(session_of(me).unwrap(), Some(session));
        assert!(!companions(session).unwrap().contains(&me));
        // The idle process cannot be opened: it is nobody's companion.
        assert_eq!(session_of(0).unwrap(), None);
    }
}
