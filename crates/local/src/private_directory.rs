//! A directory only its owner and the system may use, for a candidate-push
//! helper's private repository (phase 6, step 6d). On Unix that is mode
//! 0700. On Windows it is a protected access list naming only Owner Rights
//! and SYSTEM, inherited by everything below it, so neither worker role nor
//! any other account reaches what the helper imports and scans. Before this
//! step the Windows protection was a no-op.
//!
//! A path that is a link or junction is refused rather than followed: the
//! access list would otherwise land on whatever it points to.
use anyhow::{Context, Result};
use std::path::Path;

/// The access list of a private directory: protected from inheritance,
/// full control to the directory's owner and the system, inherited by
/// files and subdirectories.
#[cfg(windows)]
pub(crate) const PRIVATE_DIRECTORY_SDDL: &str = "D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)";

/// Makes the existing directory `path` private to its owner. A trailing
/// separator is dropped first, since `lstat` of `link/` follows the link.
pub(crate) fn protect(path: &Path) -> Result<()> {
    let path: std::path::PathBuf = path.components().collect();
    refuse_links(&path)?;
    restrict(&path).with_context(|| format!("protect {}", path.display()))
}

/// Refuses `path` when it is not a plain directory: a symbolic link,
/// junction or other reparse point, or not a directory at all.
fn refuse_links(path: &Path) -> Result<()> {
    let metadata =
        std::fs::symlink_metadata(path).with_context(|| format!("inspect {}", path.display()))?;
    anyhow::ensure!(
        !metadata.file_type().is_symlink() && !is_reparse_point(&metadata),
        "{} is a link, not a directory",
        path.display()
    );
    anyhow::ensure!(metadata.is_dir(), "{} is not a directory", path.display());
    Ok(())
}

/// Whether Windows marks the entry as a reparse point (a junction is one).
#[cfg(windows)]
fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

/// Unix has no reparse points; links are caught as links.
#[cfg(not(windows))]
fn is_reparse_point(_metadata: &std::fs::Metadata) -> bool {
    false
}

/// Mode 0700.
#[cfg(unix)]
fn restrict(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// The protected access list [`PRIVATE_DIRECTORY_SDDL`]. A directory that
/// already has exactly that list is left alone: setting it again would
/// walk every file of the repository below it on every push.
#[cfg(windows)]
fn restrict(path: &Path) -> Result<()> {
    if windows::access_list(path)? == PROTECTED_AS_READ {
        return Ok(());
    }
    windows::set_access_list(path, PRIVATE_DIRECTORY_SDDL)
}

/// [`PRIVATE_DIRECTORY_SDDL`] as Windows reads it back, marked
/// auto-inherited.
#[cfg(windows)]
const PROTECTED_AS_READ: &str = "D:PAI(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)";

/// No other platform can protect the directory, so none is used.
#[cfg(not(any(unix, windows)))]
fn restrict(_path: &Path) -> Result<()> {
    anyhow::bail!("protected directories are unsupported on this platform")
}

#[cfg(windows)]
pub(crate) mod windows {
    //! Setting and reading a file object's access list by name.
    use anyhow::{Result, bail, ensure};
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::{ERROR_SUCCESS, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_FILE_OBJECT,
        SetNamedSecurityInfoW,
    };
    use windows_sys::Win32::Security::{
        ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    };

    /// Memory Windows allocated with `LocalAlloc` (a security descriptor
    /// or its text), freed when dropped.
    struct Local(*mut std::ffi::c_void);

    impl Drop for Local {
        /// Frees the allocation.
        fn drop(&mut self) {
            // SAFETY: Windows allocated it with LocalAlloc; it is freed
            // exactly once.
            unsafe { LocalFree(self.0) };
        }
    }

    /// `text` as a NUL-terminated UTF-16 string.
    fn wide(text: &std::ffi::OsStr) -> Vec<u16> {
        text.encode_wide().chain(Some(0)).collect()
    }

    /// Replaces `path`'s access list with the protected one `sddl` holds.
    pub(crate) fn set_access_list(path: &Path, sddl: &str) -> Result<()> {
        let descriptor = parse(sddl)?;
        let dacl = access_list_of(&descriptor)?;
        let name = wide(path.as_os_str());
        let parts = DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION;
        // SAFETY: the name is NUL-terminated and `dacl` points into the
        // live descriptor; no owner, group or system list is set.
        let status = unsafe {
            let (none, kind) = (null_mut(), SE_FILE_OBJECT);
            SetNamedSecurityInfoW(name.as_ptr(), kind, parts, none, none, dacl, null())
        };
        ensure!(
            status == ERROR_SUCCESS,
            "SetNamedSecurityInfoW failed with Windows error {status}"
        );
        Ok(())
    }

    /// The descriptor `sddl` describes.
    fn parse(sddl: &str) -> Result<Local> {
        let text = wide(std::ffi::OsStr::new(sddl));
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: the text is NUL-terminated; `descriptor` receives a
        // LocalAlloc allocation and no size is asked for.
        let parsed = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut(),
            )
        };
        if parsed == 0 {
            bail!(
                "{sddl:?} is not a security descriptor: {}",
                std::io::Error::last_os_error()
            );
        }
        Ok(Local(descriptor))
    }

    /// The access list inside `descriptor`, which must have one.
    fn access_list_of(descriptor: &Local) -> Result<*mut ACL> {
        let (mut present, mut defaulted, mut dacl) = (0, 0, null_mut());
        // SAFETY: the descriptor is live; the out-pointers are writable.
        let read = unsafe {
            GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted)
        };
        ensure!(
            read != 0 && present != 0 && !dacl.is_null(),
            "the descriptor has no access list"
        );
        Ok(dacl)
    }

    /// `path`'s access list as SDDL text.
    pub(crate) fn access_list(path: &Path) -> Result<String> {
        let name = wide(path.as_os_str());
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        let none = null_mut();
        // SAFETY: the name is NUL-terminated; only the descriptor, a
        // LocalAlloc allocation, is asked for.
        #[rustfmt::skip]
        let status = unsafe {
            GetNamedSecurityInfoW(name.as_ptr(), SE_FILE_OBJECT, DACL_SECURITY_INFORMATION,
                none, none, null_mut(), null_mut(), &mut descriptor)
        };
        ensure!(
            status == ERROR_SUCCESS,
            "GetNamedSecurityInfoW failed with Windows error {status}"
        );
        text_of(&Local(descriptor))
    }

    /// The access list of `descriptor` as SDDL text.
    fn text_of(descriptor: &Local) -> Result<String> {
        let mut text: *mut u16 = null_mut();
        // SAFETY: the descriptor is live; `text` receives a LocalAlloc
        // string, freed when `owned` drops.
        #[rustfmt::skip]
        let converted = unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(descriptor.0, SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION, &mut text, null_mut())
        };
        ensure!(converted != 0, "the access list could not be printed");
        let _owned = Local(text.cast());
        // SAFETY: Windows wrote a NUL-terminated string at `text`.
        let length = (0..).take_while(|&i| unsafe { *text.add(i) } != 0).count();
        // SAFETY: `length` characters precede the NUL.
        Ok(String::from_utf16_lossy(unsafe {
            std::slice::from_raw_parts(text, length)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_protected_directory_keeps_out_everyone_but_its_owner_and_the_system() {
        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("private");
        std::fs::create_dir(&private).unwrap();
        protect(&private).unwrap();
        assert_private(&private);
        // Protecting it again changes nothing, and Git can still use it.
        protect(&private).unwrap();
        std::fs::write(private.join("file"), b"x").unwrap();
        assert_eq!(std::fs::read(private.join("file")).unwrap(), b"x");
    }

    #[test]
    fn a_missing_path_and_a_file_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(protect(&dir.path().join("missing")).is_err());
        let file = dir.path().join("file");
        std::fs::write(&file, b"x").unwrap();
        let refused = protect(&file).unwrap_err().to_string();
        assert!(refused.contains("not a directory"), "{refused}");
    }

    /// The directory's own protection, as the platform shows it.
    #[cfg(windows)]
    fn assert_private(path: &Path) {
        // Windows reads the list back marked auto-inherited (AI).
        let sddl = windows::access_list(path).unwrap();
        assert_eq!(sddl, "D:PAI(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)");
    }

    /// The directory's own protection, as the platform shows it.
    #[cfg(unix)]
    fn assert_private(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[cfg(windows)]
    #[test]
    fn a_junction_is_refused_and_its_target_left_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let (target, junction) = (dir.path().join("target"), dir.path().join("junction"));
        std::fs::create_dir(&target).unwrap();
        let before = windows::access_list(&target).unwrap();
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&target)
            .output()
            .unwrap();
        assert!(made.status.success(), "{made:?}");
        let refused = protect(&junction).unwrap_err().to_string();
        assert!(refused.contains("is a link"), "{refused}");
        assert_eq!(windows::access_list(&target).unwrap(), before);
    }

    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_spelled_with_a_trailing_slash_is_refused_and_its_target_left_as_it_was() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let (target, link) = (dir.path().join("target"), dir.path().join("link"));
        std::fs::create_dir(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let mut spelled = link.into_os_string();
        spelled.push("/");
        assert!(
            protect(Path::new(&spelled)).is_err(),
            "accepted {spelled:?}"
        );
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
    }

    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (target, link) = (dir.path().join("target"), dir.path().join("link"));
        std::fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let refused = protect(&link).unwrap_err().to_string();
        assert!(refused.contains("is a link"), "{refused}");
    }
}
