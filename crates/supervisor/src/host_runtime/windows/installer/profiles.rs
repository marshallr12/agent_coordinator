//! The user profiles Windows keeps for the services' virtual accounts
//! (phase 7, step 7b). A service account gets a profile under
//! `C:\Windows\ServiceProfiles\` the first time its service runs; the
//! AppContainer profiles the runner makes for its launches live inside the
//! runner's. Windows keeps a profile's hive loaded after its service has
//! stopped, often until a restart, and a loaded profile cannot be deleted.
use super::super::text::wide;
use crate::windows_installer::observed::ProfileState;
use anyhow::{Result, bail};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, GetLastError};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, HKEY_USERS, KEY_READ, RegCloseKey, RegOpenKeyExW,
};
use windows_sys::Win32::UI::Shell::DeleteProfileW;

/// Where Windows lists every profile, one key per account SID.
const PROFILE_LIST: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList";

/// Whether the registry key `path` below `root` exists.
fn key_exists(root: HKEY, path: &str) -> Result<bool> {
    let name = wide(path)?;
    let mut key: HKEY = null_mut();
    // SAFETY: the name is NUL-terminated and `key` receives a handle that
    // is closed below.
    let code = unsafe { RegOpenKeyExW(root, name.as_ptr(), 0, KEY_READ, &mut key) };
    match code {
        ERROR_SUCCESS => {
            // SAFETY: `key` was opened above and is closed once.
            unsafe { RegCloseKey(key) };
            Ok(true)
        }
        ERROR_FILE_NOT_FOUND => Ok(false),
        code => bail!("opening the registry key {path} failed with Windows error {code}"),
    }
}

/// Refuses anything but a SID's text, so no other key is ever named.
fn plain_sid(sid: &str) -> Result<()> {
    let plain =
        sid.starts_with("S-1-") && sid[4..].bytes().all(|b| b.is_ascii_digit() || b == b'-');
    if !plain {
        bail!("{sid:?} is not a SID");
    }
    Ok(())
}

/// Where the profile of the account with `sid` stands.
pub fn state(sid: &str) -> Result<ProfileState> {
    plain_sid(sid)?;
    if !key_exists(HKEY_LOCAL_MACHINE, &format!(r"{PROFILE_LIST}\{sid}"))? {
        return Ok(ProfileState::Absent);
    }
    Ok(match key_exists(HKEY_USERS, sid)? {
        true => ProfileState::Loaded,
        false => ProfileState::Unloaded,
    })
}

/// Deletes the profile of the account with `sid`: its directory, its hive
/// and its entry in the profile list.
pub fn delete(sid: &str) -> Result<()> {
    plain_sid(sid)?;
    let name = wide(sid)?;
    // SAFETY: the SID text is NUL-terminated; Windows finds the profile's
    // path itself, on this machine.
    if unsafe { DeleteProfileW(name.as_ptr(), null(), null()) } != 0 {
        return Ok(());
    }
    // SAFETY: reads the calling thread's last error.
    let code = unsafe { GetLastError() };
    bail!("deleting the profile of {sid} failed with Windows error {code}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_account_with_no_profile_is_absent() {
        let none = "S-1-5-80-1-2-3-4-5";
        assert_eq!(state(none).unwrap(), ProfileState::Absent);
    }

    #[test]
    fn the_system_account_has_a_loaded_profile() {
        assert_eq!(state("S-1-5-18").unwrap(), ProfileState::Loaded);
    }

    #[test]
    fn nothing_but_a_sid_is_ever_named() {
        assert!(state(r"S-1-5-18\..\other").is_err());
        assert!(delete("ProfileList").is_err());
    }
}
