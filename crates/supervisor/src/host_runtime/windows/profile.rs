//! Package identity: a disposable AppContainer profile per launch, and the
//! capability SIDs its token carries. The package SID is the launch's
//! identity to every access check Windows makes.
use super::sys::{Local, check, sid_text};
use super::text::wide;
use anyhow::{Result, ensure};
use std::ffi::c_void;
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile,
};
use windows_sys::Win32::Security::{
    DeriveCapabilitySidsFromName, FreeSid, PSID, SID_AND_ATTRIBUTES,
};
use windows_sys::Win32::System::SystemServices::SE_GROUP_ENABLED;

/// What the profile's description says in the account's profile list.
const DESCRIPTION: &str = "Agentc disposable launch";

/// One launch's AppContainer profile.
#[derive(Debug)]
pub struct Profile {
    name: String,
    sid: PSID,
    sid_text: String,
}

// SAFETY: the SID is an allocation owned here; any thread may read it.
unsafe impl Send for Profile {}

impl Profile {
    /// Creates the profile `name` for the calling account. A name already
    /// in use is an error: a launch never adopts someone else's profile.
    pub fn create(name: &str) -> Result<Self> {
        let (wide_name, description) = (wide(name)?, wide(DESCRIPTION)?);
        let mut sid: PSID = std::ptr::null_mut();
        // SAFETY: the strings are NUL-terminated, no capabilities are
        // passed, and `sid` receives a SID to free with FreeSid.
        let result = unsafe {
            CreateAppContainerProfile(
                wide_name.as_ptr(),
                wide_name.as_ptr(),
                description.as_ptr(),
                std::ptr::null(),
                0,
                &mut sid,
            )
        };
        ensure!(
            result == 0,
            "CreateAppContainerProfile {name} failed with HRESULT {result:#010x}"
        );
        Self::adopt(name, sid)
    }

    /// Wraps the freshly created profile's SID, deleting the profile again
    /// if its SID cannot be printed.
    fn adopt(name: &str, sid: PSID) -> Result<Self> {
        let printed = sid_text(sid);
        let profile = Self {
            name: name.to_owned(),
            sid,
            sid_text: printed.as_ref().cloned().unwrap_or_default(),
        };
        match printed {
            Ok(_) => Ok(profile),
            Err(error) => {
                let _ = profile.delete();
                Err(error)
            }
        }
    }

    /// The profile's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The package SID, for a Windows call; it stays owned here.
    pub fn sid(&self) -> PSID {
        self.sid
    }

    /// The package SID as `S-1-15-2-...` text.
    pub fn sid_text(&self) -> &str {
        &self.sid_text
    }

    /// Deletes the profile and its package folder.
    pub fn delete(self) -> Result<()> {
        let name = wide(&self.name)?;
        // SAFETY: `name` is NUL-terminated.
        let result = unsafe { DeleteAppContainerProfile(name.as_ptr()) };
        ensure!(
            result == 0,
            "DeleteAppContainerProfile {} failed with HRESULT {result:#010x}",
            self.name
        );
        Ok(())
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        // SAFETY: the SID came from CreateAppContainerProfile and is freed once.
        unsafe { FreeSid(self.sid) };
    }
}

/// The capability SIDs of one launch's token, in the array form process
/// creation takes.
pub struct Capabilities {
    /// Keeps each SID alive while `entries` points at it.
    _sids: Vec<Local>,
    entries: Vec<SID_AND_ATTRIBUTES>,
}

impl Capabilities {
    /// Derives the SID of each named capability. Any name yields a SID, so
    /// a tool directory can name one custom capability in its access list
    /// instead of gaining an entry per launch.
    pub fn named(names: &[String]) -> Result<Self> {
        let mut sids = Vec::new();
        for name in names {
            sids.push(capability_sid(name)?);
        }
        let entry = |sid: &Local| SID_AND_ATTRIBUTES {
            Sid: sid.pointer(),
            Attributes: SE_GROUP_ENABLED as u32,
        };
        let entries = sids.iter().map(entry).collect();
        Ok(Self {
            _sids: sids,
            entries,
        })
    }

    /// The array, or null when the launch has no capability.
    pub fn pointer(&mut self) -> *mut SID_AND_ATTRIBUTES {
        if self.entries.is_empty() {
            return std::ptr::null_mut();
        }
        self.entries.as_mut_ptr()
    }

    /// How many capabilities the array holds.
    pub fn count(&self) -> u32 {
        self.entries.len() as u32
    }
}

/// The SID Windows assigns to the capability `name`.
fn capability_sid(name: &str) -> Result<Local> {
    let wide_name = wide(name)?;
    let (mut groups, mut group_count) = (std::ptr::null_mut::<PSID>(), 0_u32);
    let (mut sids, mut count) = (std::ptr::null_mut::<PSID>(), 0_u32);
    // SAFETY: the name is NUL-terminated and every output is live; Windows
    // allocates both arrays and each SID with LocalAlloc.
    let derived = unsafe {
        DeriveCapabilitySidsFromName(
            wide_name.as_ptr(),
            &mut groups,
            &mut group_count,
            &mut sids,
            &mut count,
        )
    };
    check(derived, "DeriveCapabilitySidsFromName")?;
    let _group_sids = owned_sids(groups, group_count);
    let mut capability_sids = owned_sids(sids, count);
    ensure!(!capability_sids.is_empty(), "capability {name} has no SID");
    Ok(capability_sids.remove(0))
}

/// Takes ownership of an array of `count` SIDs Windows allocated, and of
/// the array itself, which is freed before this returns.
fn owned_sids(array: *mut PSID, count: u32) -> Vec<Local> {
    if array.is_null() {
        return Vec::new();
    }
    let _array = Local::owned(array.cast::<c_void>());
    // SAFETY: Windows filled `count` SID pointers at `array`.
    let pointers = unsafe { std::slice::from_raw_parts(array, count as usize) };
    pointers.iter().map(|sid| Local::owned(*sid)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profile_is_made_named_and_deleted() {
        let name = format!("agentc.test.{}", uuid::Uuid::new_v4().simple());
        let profile = Profile::create(&name).unwrap();
        assert!(profile.sid_text().starts_with("S-1-15-2-"));
        assert!(
            Profile::create(&name).is_err(),
            "an existing profile is never adopted"
        );
        profile.delete().unwrap();
    }

    #[test]
    fn each_capability_name_has_its_own_sid() {
        let names = ["registryRead".to_owned(), "agentcProbeTools".to_owned()];
        let mut capabilities = Capabilities::named(&names).unwrap();
        assert_eq!(capabilities.count(), 2);
        assert!(!capabilities.pointer().is_null());
        let texts: Vec<String> = (capabilities.entries.iter())
            .map(|entry| sid_text(entry.Sid).unwrap())
            .collect();
        assert!(texts.iter().all(|text| text.starts_with("S-1-15-3-")));
        assert_ne!(texts[0], texts[1]);
        let mut none = Capabilities::named(&[]).unwrap();
        assert!(none.count() == 0 && none.pointer().is_null());
    }
}
