//! Facts about the launch's own token and job, read from the OS.
//!
//! The driver compares these with what it asked for, so a launch that started
//! outside the intended boundary is reported instead of trusted.

use crate::ffi;
use crate::report::Report;
use std::ffi::c_void;
use std::io;

const TOKEN_INTEGRITY_LEVEL: u32 = 25;
const TOKEN_IS_APP_CONTAINER: u32 = 29;
const TOKEN_APP_CONTAINER_SID: u32 = 31;
const TOKEN_SECURITY_ATTRIBUTES: u32 = 39;
/// Windows records the "all application packages" opt-out as this security attribute.
const LPAC_ATTRIBUTE: &str = "WIN://NOALLAPPPKG";

/// Records the package identity, boundary kind, integrity level and job membership.
pub fn run(report: &mut Report) {
    report.value("token.package_sid", &text_or_error(package_sid()));
    report.control("token.appcontainer", require(flag(TOKEN_IS_APP_CONTAINER)));
    report.value("token.lpac", &yes_no(lpac()));
    report.value("token.integrity", &text_or_error(integrity_sid()));
    report.control("token.in_job", require(in_job()));
}

/// The AppContainer package SID of this process, as text.
pub fn package_sid() -> io::Result<String> {
    sid_field(TOKEN_APP_CONTAINER_SID)
}

/// Whether this process runs inside an AppContainer.
pub fn is_appcontainer() -> io::Result<bool> {
    flag(TOKEN_IS_APP_CONTAINER)
}

/// Whether this process is assigned to a Job Object.
pub fn in_job() -> io::Result<bool> {
    let mut result: ffi::Bool = 0;
    // SAFETY: a null job asks about any job; `result` outlives the call.
    ffi::succeeded(unsafe {
        ffi::IsProcessInJob(ffi::GetCurrentProcess(), std::ptr::null_mut(), &mut result)
    })?;
    Ok(result != 0)
}

/// The mandatory integrity label SID, for example `S-1-16-4096` for low.
fn integrity_sid() -> io::Result<String> {
    sid_field(TOKEN_INTEGRITY_LEVEL)
}

/// Opens this process's token for queries.
fn token() -> io::Result<ffi::Owned> {
    let mut handle: ffi::Handle = std::ptr::null_mut();
    // SAFETY: the pseudo-handle is always valid and `handle` outlives the call.
    ffi::succeeded(unsafe {
        ffi::OpenProcessToken(ffi::GetCurrentProcess(), ffi::TOKEN_QUERY, &mut handle)
    })?;
    Ok(ffi::Owned(handle))
}

/// Reads a token information class into a pointer-aligned buffer.
fn information(class: u32) -> io::Result<Vec<usize>> {
    let token = token()?;
    let mut buffer = vec![0_usize; 512];
    let mut needed = 0_u32;
    let bytes = (buffer.len() * size_of::<usize>()) as u32;
    // SAFETY: the buffer is writable for `bytes` bytes and both pointers are live.
    ffi::succeeded(unsafe {
        ffi::GetTokenInformation(
            token.0,
            class,
            buffer.as_mut_ptr().cast(),
            bytes,
            &mut needed,
        )
    })?;
    Ok(buffer)
}

/// Whether the token opted out of "all application packages" access.
///
/// The evaluated build rejects the dedicated information class, so the opt-out
/// is found by its name in the token's security attributes, stored as UTF-16.
fn lpac() -> io::Result<bool> {
    let block: Vec<u8> = information(TOKEN_SECURITY_ATTRIBUTES)?
        .into_iter()
        .flat_map(usize::to_ne_bytes)
        .collect();
    let marker: Vec<u8> = LPAC_ATTRIBUTE
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    Ok(block.windows(marker.len()).any(|window| window == marker))
}

/// Reads a `DWORD` token flag.
fn flag(class: u32) -> io::Result<bool> {
    Ok(information(class)?[0] as u32 != 0)
}

/// Reads a token class whose data starts with a SID pointer and renders that SID.
fn sid_field(class: u32) -> io::Result<String> {
    let buffer = information(class)?;
    sid_text(buffer[0] as *const c_void)
}

/// Converts a SID to its `S-1-...` text form.
fn sid_text(sid: *const c_void) -> io::Result<String> {
    if sid.is_null() {
        return Err(io::Error::from_raw_os_error(1337));
    }
    let mut text: *mut u16 = std::ptr::null_mut();
    // SAFETY: `sid` points into a live token buffer and `text` receives an allocation.
    ffi::succeeded(unsafe { ffi::ConvertSidToStringSidW(sid, &mut text) })?;
    // SAFETY: on success `text` is a null-terminated string that LocalFree releases.
    let result = unsafe { wide_text(text) };
    unsafe { ffi::LocalFree(text.cast()) };
    Ok(result)
}

/// Copies a null-terminated UTF-16 string.
///
/// # Safety
/// `text` must point to a live null-terminated UTF-16 buffer.
unsafe fn wide_text(text: *const u16) -> String {
    let mut length = 0;
    // SAFETY: the caller guarantees a terminator, so every offset read is in bounds.
    while unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    // SAFETY: `length` code units were just read from the same buffer.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) })
}

/// Turns a boolean fact into a control result.
fn require(fact: io::Result<bool>) -> io::Result<()> {
    match fact? {
        true => Ok(()),
        false => Err(io::Error::from_raw_os_error(1)),
    }
}

/// Renders a boolean fact, keeping a query failure visible.
fn yes_no(fact: io::Result<bool>) -> String {
    match fact {
        Ok(true) => "yes".to_string(),
        Ok(false) => "no".to_string(),
        Err(error) => format!("query-failed:{}", crate::report::code(&error)),
    }
}

/// Renders a text fact, keeping a query failure visible.
fn text_or_error(fact: io::Result<String>) -> String {
    fact.unwrap_or_else(|error| format!("query-failed:{}", crate::report::code(&error)))
}
