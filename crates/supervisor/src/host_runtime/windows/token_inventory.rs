//! What a service's own token holds: its user, every group and every
//! privilege, as Windows reports them. A broker service logs this at start
//! (phase 6, step 6d), because the account's name proves nothing about its
//! privileges: the inventory is the evidence.
use super::sys::{Handle, check, sid_text};
use super::token::of_process;
use anyhow::Result;
use serde::Serialize;
use std::ffi::c_void;
use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, GetLastError, LUID};
use windows_sys::Win32::Security::{
    GetTokenInformation, LUID_AND_ATTRIBUTES, LookupAccountSidW, LookupPrivilegeNameW, PSID,
    SE_PRIVILEGE_ENABLED, SID_AND_ATTRIBUTES, SID_NAME_USE, TOKEN_GROUPS, TOKEN_INFORMATION_CLASS,
    TOKEN_PRIVILEGES, TokenGroups, TokenPrivileges, TokenUser,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

/// The group is enabled for access checks.
const GROUP_ENABLED: u32 = 0x4;
/// The group only ever denies access.
const GROUP_DENY_ONLY: u32 = 0x10;
/// The group is the token's integrity label.
const GROUP_INTEGRITY: u32 = 0x20;
/// The group is the token's logon session SID.
const GROUP_LOGON_ID: u32 = 0xC000_0000;
/// Room for an account or domain name.
const NAME_CHARS: usize = 256;

/// One group of a token.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Group {
    pub sid: String,
    /// `DOMAIN\name`, when Windows can name it.
    pub name: Option<String>,
    pub enabled: bool,
    pub deny_only: bool,
    pub integrity: bool,
    pub logon_id: bool,
}

/// One privilege of a token.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Privilege {
    pub name: String,
    pub enabled: bool,
}

/// A token's user, groups and privileges.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Inventory {
    pub user: String,
    pub user_name: Option<String>,
    pub groups: Vec<Group>,
    pub privileges: Vec<Privilege>,
}

/// The inventory of this process's token.
pub fn current() -> Result<Inventory> {
    // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no close.
    let token = of_process(unsafe { GetCurrentProcess() })?;
    of(&token)
}

/// The inventory of `token`, opened for queries.
pub fn of(token: &Handle) -> Result<Inventory> {
    Ok(Inventory {
        user: super::token::user_sid(token)?,
        user_name: user_name(token),
        groups: groups(token)?,
        privileges: privileges(token)?,
    })
}

/// The account name of `token`'s user, when Windows can name it.
fn user_name(token: &Handle) -> Option<String> {
    let buffer = information(token, TokenUser).ok()?;
    // TOKEN_USER starts with the user's SID_AND_ATTRIBUTES.
    let user = buffer.as_ptr().cast::<SID_AND_ATTRIBUTES>();
    // SAFETY: the buffer holds a TOKEN_USER and lives until this returns.
    account_name(unsafe { (*user).Sid })
}

/// Every group of `token`.
fn groups(token: &Handle) -> Result<Vec<Group>> {
    let buffer = information(token, TokenGroups)?;
    let header = buffer.as_ptr().cast::<TOKEN_GROUPS>();
    // SAFETY: Windows wrote a TOKEN_GROUPS whose array holds GroupCount
    // entries, all inside `buffer`.
    let entries = unsafe {
        let count = (*header).GroupCount as usize;
        std::slice::from_raw_parts((*header).Groups.as_ptr(), count)
    };
    entries.iter().map(group).collect()
}

/// One group entry as a [`Group`].
fn group(entry: &SID_AND_ATTRIBUTES) -> Result<Group> {
    let flags = entry.Attributes;
    Ok(Group {
        sid: sid_text(entry.Sid)?,
        name: account_name(entry.Sid),
        enabled: flags & GROUP_ENABLED != 0,
        deny_only: flags & GROUP_DENY_ONLY != 0,
        integrity: flags & GROUP_INTEGRITY != 0,
        logon_id: flags & GROUP_LOGON_ID == GROUP_LOGON_ID,
    })
}

/// Every privilege of `token`, by name.
fn privileges(token: &Handle) -> Result<Vec<Privilege>> {
    let buffer = information(token, TokenPrivileges)?;
    let header = buffer.as_ptr().cast::<TOKEN_PRIVILEGES>();
    // SAFETY: Windows wrote a TOKEN_PRIVILEGES whose array holds
    // PrivilegeCount entries, all inside `buffer`.
    let entries: &[LUID_AND_ATTRIBUTES] = unsafe {
        let count = (*header).PrivilegeCount as usize;
        std::slice::from_raw_parts((*header).Privileges.as_ptr(), count)
    };
    let named = |entry: &LUID_AND_ATTRIBUTES| {
        Ok(Privilege {
            name: privilege_name(&entry.Luid)?,
            enabled: entry.Attributes & SE_PRIVILEGE_ENABLED != 0,
        })
    };
    entries.iter().map(named).collect()
}

/// One information class of `token`, in an aligned buffer as large as
/// Windows says it needs.
fn information(token: &Handle, class: TOKEN_INFORMATION_CLASS) -> Result<Vec<u64>> {
    let mut needed = 0_u32;
    // SAFETY: a zero-length query only reports the size needed.
    let sized =
        unsafe { GetTokenInformation(token.raw(), class, std::ptr::null_mut(), 0, &mut needed) };
    // SAFETY: reads the calling thread's last error.
    let code = unsafe { GetLastError() };
    anyhow::ensure!(
        sized == 0 && code == ERROR_INSUFFICIENT_BUFFER,
        "GetTokenInformation did not report a size (Windows error {code})"
    );
    let mut buffer = vec![0_u64; (needed as usize).div_ceil(8)];
    let (pointer, size) = (
        buffer.as_mut_ptr().cast::<c_void>(),
        (buffer.len() * 8) as u32,
    );
    // SAFETY: `pointer` is writable for `size` bytes and `needed` is live.
    let read = unsafe { GetTokenInformation(token.raw(), class, pointer, size, &mut needed) };
    check(read, "GetTokenInformation")?;
    Ok(buffer)
}

/// The name of the privilege `luid`.
fn privilege_name(luid: &LUID) -> Result<String> {
    let mut name = vec![0_u16; NAME_CHARS];
    let mut length = NAME_CHARS as u32;
    // SAFETY: no system name: this machine; `name` holds `length` chars.
    let found =
        unsafe { LookupPrivilegeNameW(std::ptr::null(), luid, name.as_mut_ptr(), &mut length) };
    check(found, "LookupPrivilegeNameW")?;
    Ok(String::from_utf16_lossy(&name[..length as usize]))
}

/// `DOMAIN\name` of `sid`, or `None` when Windows cannot name it (a logon
/// session SID, for one).
fn account_name(sid: PSID) -> Option<String> {
    let (mut name, mut domain) = (vec![0_u16; NAME_CHARS], vec![0_u16; NAME_CHARS]);
    let (mut name_length, mut domain_length) = (NAME_CHARS as u32, NAME_CHARS as u32);
    let mut kind: SID_NAME_USE = 0;
    // SAFETY: no system name: this machine; the buffers hold the lengths
    // given and `sid` is a valid SID.
    let found = unsafe {
        let (name_at, domain_at) = (name.as_mut_ptr(), domain.as_mut_ptr());
        LookupAccountSidW(
            std::ptr::null(),
            sid,
            name_at,
            &mut name_length,
            domain_at,
            &mut domain_length,
            &mut kind,
        )
    };
    if found == 0 {
        return None;
    }
    let name = String::from_utf16_lossy(&name[..name_length as usize]);
    let domain = String::from_utf16_lossy(&domain[..domain_length as usize]);
    Some(if domain.is_empty() {
        name
    } else {
        format!(r"{domain}\{name}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_has_its_user_groups_and_named_privileges() {
        let inventory = current().unwrap();
        assert_eq!(
            inventory.user,
            super::super::token::current_user_sid().unwrap()
        );
        assert!(inventory.user_name.is_some(), "{inventory:?}");
        assert!(
            inventory.groups.iter().any(|g| g.sid == "S-1-1-0"),
            "Everyone"
        );
        assert!(
            inventory.groups.iter().any(|g| g.integrity),
            "an integrity label"
        );
        assert!(
            inventory.groups.iter().any(|g| g.logon_id),
            "a logon session SID"
        );
        let change_notify = inventory
            .privileges
            .iter()
            .find(|p| p.name == "SeChangeNotifyPrivilege");
        assert!(
            change_notify.is_some_and(|p| p.enabled),
            "{:?}",
            inventory.privileges
        );
    }
}
