//! What Windows itself says about a process token. The launcher reads these
//! facts back from a suspended launch; it never trusts what it asked for.
use super::sys::{Handle, check, sid_text};
use anyhow::Result;
use std::ffi::c_void;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Security::{
    GetTokenInformation, PSID, TOKEN_INFORMATION_CLASS, TOKEN_QUERY, TokenAppContainerSid,
    TokenIsAppContainer, TokenSecurityAttributes, TokenStatistics, TokenUser,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// The token security attribute Windows records for a token that opted out
/// of "all application packages" access: a less-privileged AppContainer.
/// The evaluated build rejects the dedicated query, so the attribute's name
/// is looked for instead.
const LESS_PRIVILEGED_ATTRIBUTE: &str = "WIN://NOALLAPPPKG";
/// Room for a token's security attributes.
const ATTRIBUTES_BYTES: usize = 4096;
/// Room for a SID-bearing token information block.
const SID_BYTES: usize = 512;
/// Room for a token's statistics.
const STATISTICS_BYTES: usize = 64;

/// Opens `process`'s token for queries.
pub(crate) fn of_process(process: HANDLE) -> Result<Handle> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: `process` is a live process handle and `token` receives a handle.
    let opened = unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) };
    check(opened, "OpenProcessToken")?;
    Handle::owned(token, "OpenProcessToken")
}

/// One information class of `token`: an aligned buffer and how many of its
/// bytes Windows filled.
fn information(
    token: &Handle,
    class: TOKEN_INFORMATION_CLASS,
    bytes: usize,
) -> Result<(Vec<u64>, usize)> {
    let mut buffer = vec![0_u64; bytes.div_ceil(8)];
    let (pointer, size) = (
        buffer.as_mut_ptr().cast::<c_void>(),
        (buffer.len() * 8) as u32,
    );
    let mut filled = 0_u32;
    // SAFETY: `pointer` is writable for `size` bytes and `filled` is live.
    let read = unsafe { GetTokenInformation(token.raw(), class, pointer, size, &mut filled) };
    check(read, "GetTokenInformation")?;
    Ok((buffer, filled as usize))
}

/// The text of the SID pointer a token information block starts with, or
/// `None` when the token has no such SID.
fn leading_sid(token: &Handle, class: TOKEN_INFORMATION_CLASS) -> Result<Option<String>> {
    let (buffer, _) = information(token, class, SID_BYTES)?;
    let sid = buffer[0] as usize as PSID;
    if sid.is_null() {
        return Ok(None);
    }
    // `sid` points into `buffer`, which is alive until this returns.
    sid_text(sid).map(Some)
}

/// The account `token` belongs to.
pub fn user_sid(token: &Handle) -> Result<String> {
    let sid = leading_sid(token, TokenUser)?;
    sid.ok_or_else(|| anyhow::anyhow!("the token names no user"))
}

/// Whether `token` is an AppContainer token.
pub fn is_appcontainer(token: &Handle) -> Result<bool> {
    let (buffer, _) = information(token, TokenIsAppContainer, 8)?;
    Ok(buffer[0] as u32 != 0)
}

/// The AppContainer package SID of `token`, if it has one.
pub fn package_sid(token: &Handle) -> Result<Option<String>> {
    leading_sid(token, TokenAppContainerSid)
}

/// Whether `token` is less privileged: it opted out of everything Windows
/// grants to "all application packages". A token whose attributes cannot be
/// read is not taken to be less privileged.
pub fn is_less_privileged(token: &Handle) -> bool {
    let Ok((buffer, filled)) = information(token, TokenSecurityAttributes, ATTRIBUTES_BYTES) else {
        return false;
    };
    let bytes: Vec<u8> = buffer.iter().flat_map(|word| word.to_le_bytes()).collect();
    let name: Vec<u8> = LESS_PRIVILEGED_ATTRIBUTE
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let filled = &bytes[..filled.min(bytes.len())];
    filled.windows(name.len()).any(|window| window == name)
}

/// The account this process runs as.
pub fn current_user_sid() -> Result<String> {
    // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no close.
    let token = of_process(unsafe { GetCurrentProcess() })?;
    user_sid(&token)
}

/// The logon session `token` belongs to, as one 64-bit number. Windows
/// names a session's unnamed window station after it.
pub fn logon_session(token: &Handle) -> Result<u64> {
    let (buffer, filled) = information(token, TokenStatistics, STATISTICS_BYTES)?;
    anyhow::ensure!(filled >= 16, "the token's statistics are too short");
    // The statistics start with two 8-byte ids: the token's, then the
    // logon session's, low half first.
    Ok(buffer[1])
}

/// The logon session this process runs in.
pub fn current_logon_session() -> Result<u64> {
    // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no close.
    let token = of_process(unsafe { GetCurrentProcess() })?;
    logon_session(&token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_has_a_user_and_no_package() {
        // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no close.
        let token = of_process(unsafe { GetCurrentProcess() }).unwrap();
        assert!(user_sid(&token).unwrap().starts_with("S-1-5-"));
        assert_eq!(current_user_sid().unwrap(), user_sid(&token).unwrap());
        assert!(!is_appcontainer(&token).unwrap());
        assert_eq!(package_sid(&token).unwrap(), None);
        assert!(!is_less_privileged(&token));
        let session = logon_session(&token).unwrap();
        assert!(session != 0 && session == current_logon_session().unwrap());
    }
}
