//! The few owned Windows resources every launcher layer shares: a handle
//! that closes itself, memory Windows allocated, SID text and a security
//! descriptor built from SDDL. Each wrapper frees what it owns exactly once.
use super::text::wide;
use anyhow::{Context, Result, anyhow, ensure};
use std::ffi::c_void;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertSidToStringSidW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES};
use windows_sys::core::BOOL;

/// The calling thread's last Windows error, named after the `operation`
/// that failed. Call it before anything else can overwrite the error.
pub fn last_error(operation: &str) -> anyhow::Error {
    // SAFETY: GetLastError reads thread-local state and takes no arguments.
    let code = unsafe { GetLastError() };
    anyhow!("{operation} failed with Windows error {code}")
}

/// `Ok` when a Windows call that returns `BOOL` succeeded.
pub fn check(succeeded: BOOL, operation: &str) -> Result<()> {
    if succeeded == 0 {
        return Err(last_error(operation));
    }
    Ok(())
}

/// A kernel handle this process owns. Dropping it closes it, which for a
/// Job Object also ends every process inside.
#[derive(Debug)]
pub struct Handle(HANDLE);

// SAFETY: a kernel handle is a process-wide value; any thread may use it.
unsafe impl Send for Handle {}

impl Handle {
    /// Takes ownership of `raw`, the result of `operation`; a null or
    /// invalid value is that operation's failure.
    pub fn owned(raw: HANDLE, operation: &str) -> Result<Self> {
        if raw.is_null() || raw == INVALID_HANDLE_VALUE {
            return Err(last_error(operation));
        }
        Ok(Self(raw))
    }

    /// The handle value, for a Windows call; it stays owned here.
    pub fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle is owned here and closed exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

/// Memory Windows allocated with `LocalAlloc` and handed to the caller.
#[derive(Debug)]
pub struct Local(*mut c_void);

impl Local {
    /// Takes ownership of `pointer`, which must come from `LocalAlloc`.
    pub fn owned(pointer: *mut c_void) -> Self {
        Self(pointer)
    }

    /// The allocation, for a Windows call; it stays owned here.
    pub fn pointer(&self) -> *mut c_void {
        self.0
    }
}

impl Drop for Local {
    fn drop(&mut self) {
        // SAFETY: the allocation is owned here and freed exactly once.
        unsafe { LocalFree(self.0) };
    }
}

/// `sid` as `S-1-...` text. `sid` must point to a valid SID.
pub(crate) fn sid_text(sid: PSID) -> Result<String> {
    ensure!(!sid.is_null(), "there is no SID to print");
    let mut text: *mut u16 = std::ptr::null_mut();
    // SAFETY: `sid` is a valid SID and `text` receives a LocalAlloc string.
    check(
        unsafe { ConvertSidToStringSidW(sid, &mut text) },
        "ConvertSidToStringSidW",
    )?;
    local_text(text)
}

/// The NUL-terminated UTF-16 string Windows allocated at `text` with
/// `LocalAlloc`, which is freed here.
fn local_text(text: *mut u16) -> Result<String> {
    let owned = Local::owned(text.cast());
    // SAFETY: Windows returned a NUL-terminated string that `owned` keeps alive.
    let length = (0..).take_while(|&i| unsafe { *text.add(i) } != 0).count();
    // SAFETY: `length` code units before the terminator are initialized.
    let units = unsafe { std::slice::from_raw_parts(owned.pointer().cast::<u16>(), length) };
    String::from_utf16(units).context("text from Windows is not UTF-16")
}

/// The `parts` of `descriptor` as SDDL text, the form `sc.exe sdshow` and
/// `icacls /save` print. `descriptor` must point to a valid descriptor.
pub(crate) fn descriptor_text(descriptor: PSECURITY_DESCRIPTOR, parts: u32) -> Result<String> {
    let mut text: *mut u16 = std::ptr::null_mut();
    // SAFETY: `descriptor` is valid; `text` receives a LocalAlloc string;
    // the length output is optional.
    let converted = unsafe {
        let length = std::ptr::null_mut();
        #[rustfmt::skip]
        let converted = ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor, SDDL_REVISION_1, parts, &mut text, length);
        converted
    };
    check(converted, "printing a security descriptor")?;
    local_text(text)
}

/// A security descriptor built from SDDL text.
#[derive(Debug)]
pub struct Descriptor(Local);

impl Descriptor {
    /// Parses `sddl`; the text comes from [`super::sddl`], never a caller.
    pub fn parse(sddl: &str) -> Result<Self> {
        let text = wide(sddl)?;
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `text` is NUL-terminated; `descriptor` receives a
        // LocalAlloc block; the size output is optional.
        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        };
        check(converted, "building a security descriptor")?;
        Ok(Self(Local::owned(descriptor)))
    }

    /// The descriptor, for a Windows call; it stays owned here.
    pub fn pointer(&self) -> PSECURITY_DESCRIPTOR {
        self.0.pointer()
    }

    /// Creation attributes naming this descriptor. The result borrows the
    /// descriptor, so use it while `self` is alive. `inherit` marks the
    /// created handle inheritable.
    pub fn attributes(&self, inherit: bool) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.pointer(),
            bInheritHandle: BOOL::from(inherit),
        }
    }
}
