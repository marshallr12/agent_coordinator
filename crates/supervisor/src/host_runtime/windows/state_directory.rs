//! A broker service's own directories (phase 6, step 6d): its state
//! directory and the publication broker's work root. A missing directory
//! is made with its protected access list. An existing one is used only
//! when it is a plain directory, not a link or other reparse point, owned
//! by this account, the system or administrators; its access list is then
//! set again, so a directory made before the service by someone else, or
//! with a looser list, never holds what the broker writes.
//!
//! Every default sits below `C:\ProgramData`, where any local user may
//! create a directory and would own it; the owner check is what refuses
//! such a directory.
use super::reader;
use super::sys::{Descriptor, Local, sid_text};
use anyhow::{Context, Result, bail, ensure};
use std::path::Path;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::Security::Authorization::{
    GetNamedSecurityInfoW, SE_FILE_OBJECT, SetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, OWNER_SECURITY_INFORMATION,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
};

/// The local system.
const SYSTEM: &str = "S-1-5-18";
/// The local Administrators group.
const ADMINISTRATORS: &str = "S-1-5-32-544";

/// Makes `directory` with the access list `sddl`, or secures the one that
/// exists for the account `me` (SID text).
pub fn secure(directory: &Path, me: &str, sddl: &str) -> Result<()> {
    if !directory.exists() {
        if let Some(parent) = directory.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        return reader::create_directory(directory, sddl);
    }
    refuse_links(directory)?;
    let owner = owner_of(directory)?;
    ensure!(
        [me, SYSTEM, ADMINISTRATORS].contains(&owner.as_str()),
        "{} is owned by {owner}, not this account, the system or administrators",
        directory.display()
    );
    set_access_list(directory, sddl).with_context(|| format!("secure {}", directory.display()))
}

/// Refuses `path` unless it is a plain directory.
fn refuse_links(path: &Path) -> Result<()> {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
    let metadata = std::fs::symlink_metadata(path)?;
    let reparse = metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    if reparse || metadata.file_type().is_symlink() {
        bail!("{} is a link or reparse point", path.display());
    }
    ensure!(metadata.is_dir(), "{} is not a directory", path.display());
    Ok(())
}

/// The owner of `path`, as SID text.
pub fn owner_of(path: &Path) -> Result<String> {
    let name = super::text::wide(&path.display().to_string())?;
    let (mut owner, mut descriptor): (PSID, PSECURITY_DESCRIPTOR) = (null_mut(), null_mut());
    // SAFETY: the name is NUL-terminated; the owner points into the
    // descriptor, a LocalAlloc block kept alive below until it is printed.
    let status = unsafe {
        #[rustfmt::skip]
        let status = GetNamedSecurityInfoW(name.as_ptr(), SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION, &mut owner, null_mut(), null_mut(), null_mut(),
            &mut descriptor);
        status
    };
    ensure!(
        status == ERROR_SUCCESS,
        "reading the owner of {} failed with Windows error {status}",
        path.display()
    );
    let _descriptor = Local::owned(descriptor);
    sid_text(owner)
}

/// Replaces `path`'s access list with the protected one `sddl` holds.
fn set_access_list(path: &Path, sddl: &str) -> Result<()> {
    let descriptor = Descriptor::parse(sddl)?;
    let dacl = access_list_of(&descriptor)?;
    let name = super::text::wide(&path.display().to_string())?;
    let parts = DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION;
    // SAFETY: the name is NUL-terminated and `dacl` points into the live
    // descriptor; no owner, group or system list is set.
    #[rustfmt::skip]
    let status = unsafe {
        SetNamedSecurityInfoW(name.as_ptr(), SE_FILE_OBJECT, parts, null_mut(), null_mut(),
            dacl, null())
    };
    ensure!(
        status == ERROR_SUCCESS,
        "SetNamedSecurityInfoW failed with Windows error {status}"
    );
    Ok(())
}

/// The access list inside `descriptor`, which must have one.
fn access_list_of(descriptor: &Descriptor) -> Result<*mut ACL> {
    let (mut present, mut defaulted, mut dacl) = (0, 0, null_mut());
    // SAFETY: the descriptor is live; the out-pointers are writable.
    let read = unsafe {
        GetSecurityDescriptorDacl(
            descriptor.pointer(),
            &mut present,
            &mut dacl,
            &mut defaulted,
        )
    };
    ensure!(
        read != 0 && present != 0 && !dacl.is_null(),
        "the descriptor has no access list"
    );
    Ok(dacl)
}

#[cfg(test)]
mod tests {
    use super::super::{sddl, token};
    use super::*;
    use std::process::Command;

    /// `icacls` output for `path`.
    fn icacls(path: &Path) -> String {
        let output = Command::new("icacls").arg(path).output().unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    #[test]
    fn an_existing_directory_this_account_owns_loses_a_looser_access_list() {
        let dir = tempfile::tempdir().unwrap();
        let (state, me) = (dir.path().join("state"), token::current_user_sid().unwrap());
        std::fs::create_dir(&state).unwrap();
        let mut grant = Command::new("icacls");
        grant.arg(&state).args(["/grant", "*S-1-1-0:(OI)(CI)F"]);
        assert!(grant.output().unwrap().status.success());
        assert!(
            icacls(&state).contains("Everyone"),
            "the grant did not land"
        );
        secure(&state, &me, &sddl::state_directory(&me).unwrap()).unwrap();
        assert!(!icacls(&state).contains("Everyone"), "{}", icacls(&state));
    }

    #[test]
    fn a_missing_directory_is_made_with_its_access_list() {
        let dir = tempfile::tempdir().unwrap();
        let (state, me) = (
            dir.path().join("a").join("state"),
            token::current_user_sid().unwrap(),
        );
        secure(&state, &me, &sddl::state_directory(&me).unwrap()).unwrap();
        assert!(state.is_dir());
        assert_eq!(owner_of(&state).unwrap(), me);
        assert!(!icacls(&state).contains("Everyone"), "{}", icacls(&state));
    }

    #[test]
    fn a_directory_another_account_owns_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir(&state).unwrap();
        // The directory is this account's; the broker claims to be another.
        let other = "S-1-5-80-1-2-3-4-5";
        let refused = secure(&state, other, &sddl::state_directory(other).unwrap());
        let refused = format!("{:#}", refused.unwrap_err());
        assert!(refused.contains("is owned by"), "{refused}");
    }

    #[test]
    fn a_junction_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (target, junction) = (dir.path().join("target"), dir.path().join("junction"));
        std::fs::create_dir(&target).unwrap();
        let mut link = Command::new("cmd");
        link.args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&target);
        assert!(link.output().unwrap().status.success());
        let me = token::current_user_sid().unwrap();
        let refused = secure(&junction, &me, &sddl::state_directory(&me).unwrap());
        assert!(format!("{:#}", refused.unwrap_err()).contains("link"));
    }
}
