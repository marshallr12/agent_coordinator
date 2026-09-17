use std::{fs, io, path::Path};

#[cfg(unix)]
pub(crate) fn protect_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(windows)]
pub(crate) fn protect_directory(path: &Path) -> io::Result<()> {
    set_private_windows_acl(path, true)
}

#[cfg(unix)]
pub(crate) fn protect_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(windows)]
pub(crate) fn protect_file(path: &Path) -> io::Result<()> {
    set_private_windows_acl(path, false)
}

#[cfg(windows)]
fn set_private_windows_acl(path: &Path, directory: bool) -> io::Result<()> {
    // Owner Rights means the actual owner of this newly created/current-user
    // state directory, without embedding a localized account name. SYSTEM is
    // retained for machine recovery. The protected DACL prevents broad inherited
    // entries from exposing session proofs or pending mutation journals.
    let sddl = if directory {
        "D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)"
    } else {
        "D:P(A;;FA;;;OW)(A;;FA;;;SY)"
    };
    set_windows_acl(path, sddl)
}

#[cfg(windows)]
fn set_windows_acl(path: &Path, sddl: &str) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_FILE_OBJECT,
        SetNamedSecurityInfoW,
    };
    use windows_sys::Win32::Security::{
        ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    };

    let descriptor_text: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            descriptor_text.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        )
    };
    if converted == 0 {
        return Err(io::Error::last_os_error());
    }

    let result = (|| {
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl: *mut ACL = null_mut();
        let obtained = unsafe {
            GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted)
        };
        if obtained == 0 {
            return Err(io::Error::last_os_error());
        }
        if present == 0 || dacl.is_null() {
            return Err(io::Error::other(
                "private Windows security descriptor omitted its DACL",
            ));
        }
        let path_wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let status = unsafe {
            SetNamedSecurityInfoW(
                path_wide.as_ptr() as *mut u16,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                dacl,
                null(),
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        Ok(())
    })();
    unsafe {
        LocalFree(descriptor);
    }
    result
}
