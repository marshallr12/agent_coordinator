//! What is at a path, as the installer sees it: nothing, a link, or a plain
//! directory or file with its owner and its access list read entry by
//! entry. A link is reported as a link and never opened.
use super::super::state_directory::set_access_list;
use super::super::sys::{Local, check, sid_text};
use super::super::text::wide;
use crate::windows_installer::access::{AccessList, Entry, INHERITANCE_BITS, Kind};
use crate::windows_installer::observed::PathState;
use anyhow::{Context, Result, ensure};
use std::collections::BTreeSet;
use std::ffi::c_void;
use std::path::Path;
use std::ptr::{addr_of, null_mut};
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, GetAce,
    GetSecurityDescriptorControl, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    SE_DACL_PROTECTED,
};
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

/// The entry type that grants access.
const ALLOWED: u8 = 0;
/// The entry type that denies access.
const DENIED: u8 = 1;
/// The flag of an entry a parent handed down.
const INHERITED: u8 = 0x10;

/// What is at `path`, without following a link.
pub fn observe(path: &str) -> Result<PathState> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PathState::Missing);
        }
        Err(error) => return Err(error).with_context(|| format!("inspect {path}")),
    };
    if is_link(&metadata) {
        return Ok(PathState::Link);
    }
    let (owner, access) = security(path)?;
    Ok(match metadata.is_dir() {
        true => PathState::Directory { owner, access },
        false => PathState::File { owner, access },
    })
}

/// Whether `metadata` is a symbolic link, a junction or any reparse point.
pub fn is_link(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    let reparse = metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    reparse || metadata.file_type().is_symlink()
}

/// The owner and the access list of the file or directory at `path`.
fn security(path: &str) -> Result<(String, AccessList)> {
    let name = wide(path)?;
    let (mut owner, mut dacl): (PSID, *mut ACL) = (null_mut(), null_mut());
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    let parts = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
    // SAFETY: the name is NUL-terminated; the owner and the list point into
    // the descriptor, a LocalAlloc block kept alive below until both are read.
    #[rustfmt::skip]
    let status = unsafe {
        GetNamedSecurityInfoW(name.as_ptr(), SE_FILE_OBJECT, parts, &mut owner, null_mut(),
            &mut dacl, null_mut(), &mut descriptor)
    };
    ensure!(
        status == ERROR_SUCCESS,
        "reading the security of {path} failed with Windows error {status}"
    );
    let descriptor = Local::owned(descriptor);
    let access = AccessList {
        protected: protected(&descriptor)?,
        entries: entries(dacl)?,
    };
    Ok((sid_text(owner)?, access))
}

/// Whether the descriptor's access list ignores what the parent hands down.
fn protected(descriptor: &Local) -> Result<bool> {
    let (mut control, mut revision) = (0_u16, 0_u32);
    // SAFETY: the descriptor is live and both out-pointers are writable.
    let read =
        unsafe { GetSecurityDescriptorControl(descriptor.pointer(), &mut control, &mut revision) };
    check(read, "GetSecurityDescriptorControl")?;
    Ok(control & SE_DACL_PROTECTED != 0)
}

/// Every entry of the access list at `dacl`. An object with no list at all
/// grants everyone everything; it reads as one entry no wanted list has.
fn entries(dacl: *const ACL) -> Result<BTreeSet<Entry>> {
    if dacl.is_null() {
        let open = Entry {
            kind: Kind::Other(u8::MAX),
            sid: "no access list".into(),
            mask: u32::MAX,
            inheritance: 0,
            inherited: false,
        };
        return Ok(BTreeSet::from([open]));
    }
    // SAFETY: `dacl` points to a live access list.
    let count = unsafe { (*dacl).AceCount };
    (0..u32::from(count))
        .map(|index| entry(dacl, index))
        .collect()
}

/// The entry at `index` of the access list at `dacl`.
fn entry(dacl: *const ACL, index: u32) -> Result<Entry> {
    let mut ace: *mut c_void = null_mut();
    // SAFETY: `dacl` is live and `index` is below its entry count.
    check(unsafe { GetAce(dacl, index, &mut ace) }, "GetAce")?;
    // SAFETY: every entry starts with an ACE_HEADER.
    let header = unsafe { *ace.cast::<ACE_HEADER>() };
    let (kind, mask, sid) = match header.AceType {
        ALLOWED => named(Kind::Allow, ace)?,
        DENIED => named(Kind::Deny, ace)?,
        other => (Kind::Other(other), 0, String::new()),
    };
    let inheritance = header.AceFlags & INHERITANCE_BITS;
    let inherited = header.AceFlags & INHERITED != 0;
    Ok(Entry {
        kind,
        sid,
        mask,
        inheritance,
        inherited,
    })
}

/// A granting or denying entry's kind with its rights and its SID.
fn named(kind: Kind, ace: *mut c_void) -> Result<(Kind, u32, String)> {
    let (mask, sid) = mask_and_sid(ace)?;
    Ok((kind, mask, sid))
}

/// The rights and the SID of a granting or denying entry, which share one
/// layout: the header, the mask, then the SID.
fn mask_and_sid(ace: *mut c_void) -> Result<(u32, String)> {
    let ace = ace.cast::<ACCESS_ALLOWED_ACE>();
    // SAFETY: the entry is an allowed or denied one, so it has this layout
    // and its SID starts at `SidStart`.
    let (mask, sid) = unsafe { ((*ace).Mask, addr_of!((*ace).SidStart) as PSID) };
    Ok((mask, sid_text(sid)?))
}

/// Replaces the access list of the file or directory at `path`.
pub fn set(path: &str, access: &AccessList) -> Result<()> {
    set_access_list(Path::new(path), &access.sddl()?).with_context(|| format!("secure {path}"))
}

#[cfg(test)]
mod tests {
    use super::super::super::{reader, token};
    use super::*;
    use crate::windows_installer::access::{ADMINISTRATORS, FULL, READ, SYSTEM};
    use std::process::Command;

    /// A service SID nothing on this machine has.
    const SERVICE: &str = "S-1-5-80-1-2-3-4-5";

    /// `path` as text.
    fn text(path: &Path) -> String {
        path.display().to_string()
    }

    #[test]
    fn a_directory_made_with_a_list_reads_back_as_that_list() {
        let dir = tempfile::tempdir().unwrap();
        let (path, me) = (
            text(&dir.path().join("state")),
            token::current_user_sid().unwrap(),
        );
        let wanted = AccessList::directory(&[
            (SYSTEM, FULL),
            (ADMINISTRATORS, FULL),
            (&me, FULL),
            (SERVICE, READ),
        ]);
        reader::create_directory(Path::new(&path), &wanted.sddl().unwrap()).unwrap();
        let PathState::Directory { access, .. } = observe(&path).unwrap() else {
            panic!("not a directory");
        };
        assert_eq!(access, wanted);
    }

    #[test]
    fn a_widened_list_differs_and_setting_the_list_restores_it() {
        let dir = tempfile::tempdir().unwrap();
        let (path, me) = (
            text(&dir.path().join("state")),
            token::current_user_sid().unwrap(),
        );
        let wanted = AccessList::directory(&[(SYSTEM, FULL), (ADMINISTRATORS, FULL), (&me, FULL)]);
        reader::create_directory(Path::new(&path), &wanted.sddl().unwrap()).unwrap();
        let mut grant = Command::new("icacls");
        grant.arg(&path).args(["/grant", "*S-1-1-0:(OI)(CI)RX"]);
        assert!(grant.output().unwrap().status.success());
        let PathState::Directory { access, .. } = observe(&path).unwrap() else {
            panic!("not a directory");
        };
        assert_ne!(access, wanted);
        assert!(
            access.entries.iter().any(|e| e.sid == "S-1-1-0"),
            "{access:?}"
        );
        set(&path, &wanted).unwrap();
        let PathState::Directory { access, .. } = observe(&path).unwrap() else {
            panic!("not a directory");
        };
        assert_eq!(access, wanted);
    }

    #[test]
    fn a_plain_temporary_directory_inherits_and_so_differs() {
        let dir = tempfile::tempdir().unwrap();
        let PathState::Directory { access, owner } = observe(&text(dir.path())).unwrap() else {
            panic!("not a directory");
        };
        assert!(!access.protected, "{access:?}");
        assert!(access.entries.iter().all(|e| e.inherited), "{access:?}");
        assert!(owner.starts_with("S-1-5-"), "{owner}");
    }

    #[test]
    fn a_junction_is_a_link_and_a_missing_path_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let (target, junction) = (dir.path().join("target"), dir.path().join("junction"));
        std::fs::create_dir(&target).unwrap();
        let mut link = Command::new("cmd");
        link.args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&target);
        assert!(link.output().unwrap().status.success());
        assert_eq!(observe(&text(&junction)).unwrap(), PathState::Link);
        assert_eq!(
            observe(&text(&dir.path().join("none"))).unwrap(),
            PathState::Missing
        );
    }
}
