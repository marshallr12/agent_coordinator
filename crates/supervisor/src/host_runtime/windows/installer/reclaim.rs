//! Taking back a tree an account locked to itself, so `uninstall` can
//! remove it (phase 8, step 8a).
//!
//! The coordinator CLI keeps its session proofs and mutation journals in a
//! directory whose access list it sets to its owner and the system alone,
//! with nothing inherited. Run by the supervisor service, that owner is the
//! service's account, so an elevated administrator can neither list nor
//! delete what it holds. This is the one place the installer takes
//! ownership: of that tree only, entry by entry from the top, never through
//! a link. Each entry is given to the administrators, then its access list
//! is set to the system and administrators alone, after which the ordinary
//! removal works.
use super::super::state_directory::set_access_list;
use super::super::sys::{Descriptor, Handle, check};
use super::super::text::wide;
use super::access::is_link;
use anyhow::{Context, Result, ensure};
use std::path::Path;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{ERROR_NOT_ALL_ASSIGNED, ERROR_SUCCESS, GetLastError, LUID};
use windows_sys::Win32::Security::Authorization::{SE_FILE_OBJECT, SetNamedSecurityInfoW};
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, GetSecurityDescriptorOwner, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW,
    OWNER_SECURITY_INFORMATION, PSID, SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES,
    TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// The administrators as owner, and an access list of the system and the
/// administrators only, inherited by everything below.
const RECLAIMED: &str = "O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";

/// Gives the tree at `path` to the administrators so it can be removed,
/// and returns each entry that could not be taken, with why. A link is
/// never taken or followed. Nothing at `path` is no error.
pub fn reclaim_tree(path: &str) -> Result<Vec<String>> {
    enable("SeTakeOwnershipPrivilege")?;
    let descriptor = Descriptor::parse(RECLAIMED)?;
    let mut left = Vec::new();
    reclaim_entry(Path::new(path), &descriptor, &mut left);
    Ok(left)
}

/// Takes one entry, then each entry below it when it is a directory.
fn reclaim_entry(path: &Path, descriptor: &Descriptor, left: &mut Vec<String>) {
    let metadata = match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => return left.push(format!("{}: {error}", path.display())),
        Ok(metadata) => metadata,
    };
    if is_link(&metadata) {
        return left.push(format!("{}: a link is not taken", path.display()));
    }
    if let Err(error) = take(path, descriptor) {
        return left.push(format!("{}: {error:#}", path.display()));
    }
    if metadata.is_dir() {
        reclaim_children(path, descriptor, left);
    }
}

/// Takes every entry directly in the directory `path`.
fn reclaim_children(path: &Path, descriptor: &Descriptor, left: &mut Vec<String>) {
    match std::fs::read_dir(path) {
        Ok(entries) => {
            for entry in entries.flatten() {
                reclaim_entry(&entry.path(), descriptor, left);
            }
        }
        Err(error) => left.push(format!("{}: {error}", path.display())),
    }
}

/// Makes the administrators the owner of `path`, which the take-ownership
/// privilege allows whatever its access list says, then, as its owner,
/// sets its access list.
fn take(path: &Path, descriptor: &Descriptor) -> Result<()> {
    let name = wide(&path.display().to_string())?;
    let owner = owner_of(descriptor)?;
    // SAFETY: the name is NUL-terminated and `owner` points into the live
    // descriptor; only the owner is set.
    #[rustfmt::skip]
    let status = unsafe {
        SetNamedSecurityInfoW(name.as_ptr(), SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION,
            owner, null_mut(), null(), null())
    };
    ensure!(
        status == ERROR_SUCCESS,
        "taking ownership failed with Windows error {status}"
    );
    set_access_list(path, RECLAIMED)
}

/// The owner `descriptor` names.
fn owner_of(descriptor: &Descriptor) -> Result<PSID> {
    let (mut owner, mut defaulted): (PSID, i32) = (null_mut(), 0);
    // SAFETY: the descriptor is live and both outputs are writable.
    let read =
        unsafe { GetSecurityDescriptorOwner(descriptor.pointer(), &mut owner, &mut defaulted) };
    check(read, "GetSecurityDescriptorOwner")?;
    ensure!(!owner.is_null(), "the descriptor names no owner");
    Ok(owner)
}

/// Enables the privilege `name` in this process's token, which must hold
/// it: an elevated administrator's does.
fn enable(name: &str) -> Result<()> {
    let token = process_token()?;
    let mut luid = LUID::default();
    let wide_name = wide(name)?;
    // SAFETY: the name is NUL-terminated and `luid` is writable.
    let found = unsafe { LookupPrivilegeValueW(null(), wide_name.as_ptr(), &mut luid) };
    check(found, "LookupPrivilegeValueW")?;
    let wanted = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: SE_PRIVILEGE_ENABLED,
        }],
    };
    // SAFETY: the token was opened for adjusting; no previous state is
    // asked for.
    let adjusted =
        unsafe { AdjustTokenPrivileges(token.raw(), 0, &wanted, 0, null_mut(), null_mut()) };
    check(adjusted, "AdjustTokenPrivileges")?;
    // SAFETY: reads this thread's last error, which the call above set.
    let missing = unsafe { GetLastError() } == ERROR_NOT_ALL_ASSIGNED;
    ensure!(!missing, "this process does not hold {name}");
    Ok(())
}

/// This process's token, opened for adjusting its privileges.
fn process_token() -> Result<Handle> {
    let mut token = null_mut();
    let access = TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY;
    // SAFETY: the pseudo-handle needs no closing; `token` is writable.
    let opened = unsafe { OpenProcessToken(GetCurrentProcess(), access, &mut token) };
    check(opened, "OpenProcessToken")?;
    Handle::owned(token, "OpenProcessToken").context("the process token")
}

#[cfg(test)]
mod tests {
    use super::super::tokens::elevated;
    use super::*;

    /// A directory and a file in it locked the way the CLI locks its state,
    /// then given to the system, so this account has no right to either.
    fn locked(root: &Path) -> std::path::PathBuf {
        let state = root.join("state");
        std::fs::create_dir(&state).unwrap();
        std::fs::write(state.join("proof.json"), b"{}").unwrap();
        for (path, inherit) in [(state.join("proof.json"), ""), (state.clone(), "OICI")] {
            let sddl = format!("O:SYD:P(A;{inherit};FA;;;OW)(A;{inherit};FA;;;SY)");
            lock(&path, &sddl);
        }
        state
    }

    /// Sets `path`'s owner and list to `sddl`, which the restore privilege
    /// of an elevated session allows for another owner.
    fn lock(path: &Path, sddl: &str) {
        enable("SeRestorePrivilege").unwrap();
        let descriptor = Descriptor::parse(sddl).unwrap();
        let name = wide(&path.display().to_string()).unwrap();
        let owner = owner_of(&descriptor).unwrap();
        // SAFETY: as in `take`.
        #[rustfmt::skip]
        let status = unsafe {
            SetNamedSecurityInfoW(name.as_ptr(), SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION,
                owner, null_mut(), null(), null())
        };
        assert_eq!(status, ERROR_SUCCESS);
        set_access_list(path, sddl).unwrap();
    }

    #[test]
    fn a_tree_locked_to_another_owner_is_taken_and_then_removed() {
        if !elevated().unwrap() {
            eprintln!("skipped: giving a file to another owner needs elevation");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let state = locked(dir.path());
        let proof = state.join("proof.json");
        // No removal is tried before: `lock` enabled the restore privilege
        // in this process, with which a deletion passes any list. What is
        // checked is that every entry is given to the administrators with
        // the system's and administrators' list, which `lock` did not set.
        let left = reclaim_tree(&state.display().to_string()).unwrap();
        assert!(left.is_empty(), "{left:?}");
        for (path, inherit) in [(&state, "OICI"), (&proof, "")] {
            let owner = super::super::super::state_directory::owner_of(path).unwrap();
            assert_eq!(owner, "S-1-5-32-544", "{}", path.display());
            let shown = super::super::access::observe(&path.display().to_string()).unwrap();
            let shown = format!("{shown:?}");
            assert!(!shown.contains("S-1-3-4"), "{inherit}: {shown}");
        }
        let gone = super::super::files::remove_tree(&state.display().to_string()).unwrap();
        assert!(gone.is_empty(), "{gone:?}");
        assert!(!state.exists());
    }

    #[test]
    fn nothing_there_is_no_error_and_a_link_is_not_taken() {
        if !elevated().unwrap() {
            eprintln!("skipped: taking ownership needs elevation");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing").display().to_string();
        assert!(reclaim_tree(&missing).unwrap().is_empty());
        let target = dir.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let link = dir.path().join("link");
        let made = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .unwrap();
        assert!(made.status.success());
        let left = reclaim_tree(&link.display().to_string()).unwrap();
        assert_eq!(left.len(), 1, "{left:?}");
        assert!(left[0].contains("a link is not taken"), "{left:?}");
    }
}
