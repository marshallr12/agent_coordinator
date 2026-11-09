//! Hand-declared Win32 bindings and the small safe wrappers the checks share.
//!
//! The contained build is offline and dependency-free, so the fixture cannot
//! use `windows-sys`. Only the calls the checks need are declared.

#![allow(non_snake_case)]

use std::ffi::c_void;
use std::io;

pub type Handle = *mut c_void;
pub type Bool = i32;

pub const PROCESS_TERMINATE: u32 = 0x0001;
pub const PROCESS_VM_ALL: u32 = 0x0008 | 0x0010 | 0x0020;
pub const PROCESS_DUP_HANDLE: u32 = 0x0040;
pub const PROCESS_QUERY_LIMITED: u32 = 0x1000;
pub const TOKEN_QUERY: u32 = 0x0008;
pub const KEY_QUERY_VALUE: u32 = 0x0001;
pub const KEY_SET_VALUE: u32 = 0x0002;
/// `HKEY_USERS`, a sign-extended predefined handle.
pub const HKEY_USERS: Handle = 0x8000_0003_u32 as i32 as isize as Handle;

#[link(name = "kernel32")]
unsafe extern "system" {
    pub fn GetCurrentProcess() -> Handle;
    pub fn GetCurrentThreadId() -> u32;
    pub fn OpenProcess(access: u32, inherit: Bool, pid: u32) -> Handle;
    pub fn CloseHandle(handle: Handle) -> Bool;
    pub fn IsProcessInJob(process: Handle, job: Handle, result: *mut Bool) -> Bool;
    pub fn ReadFile(
        file: Handle,
        buffer: *mut u8,
        length: u32,
        read: *mut u32,
        overlapped: *mut c_void,
    ) -> Bool;
    pub fn CreateNamedPipeW(
        name: *const u16,
        open_mode: u32,
        pipe_mode: u32,
        instances: u32,
        out_size: u32,
        in_size: u32,
        timeout: u32,
        security: *const c_void,
    ) -> Handle;
    pub fn CreateEventW(
        security: *const c_void,
        manual: Bool,
        initial: Bool,
        name: *const u16,
    ) -> Handle;
    pub fn OpenEventW(access: u32, inherit: Bool, name: *const u16) -> Handle;
    pub fn VirtualAlloc(address: *mut c_void, size: usize, kind: u32, protect: u32) -> *mut c_void;
    pub fn LocalFree(memory: *mut c_void) -> *mut c_void;
    pub fn LoadLibraryW(name: *const u16) -> Handle;
    pub fn GetProcAddress(module: Handle, name: *const u8) -> *mut c_void;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    pub fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> Bool;
    pub fn GetTokenInformation(
        token: Handle,
        class: u32,
        buffer: *mut c_void,
        length: u32,
        needed: *mut u32,
    ) -> Bool;
    pub fn ConvertSidToStringSidW(sid: *const c_void, text: *mut *mut u16) -> Bool;
    pub fn RegOpenKeyExW(
        key: Handle,
        path: *const u16,
        options: u32,
        access: u32,
        result: *mut Handle,
    ) -> u32;
    pub fn RegQueryValueExW(
        key: Handle,
        name: *const u16,
        reserved: *mut u32,
        kind: *mut u32,
        data: *mut u8,
        length: *mut u32,
    ) -> u32;
    pub fn RegCloseKey(key: Handle) -> u32;
}

/// A kernel handle closed when dropped.
pub struct Owned(pub Handle);

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: the handle came from a successful Win32 call and is closed once.
        unsafe { CloseHandle(self.0) };
    }
}

/// A null-terminated UTF-16 copy of `text` for a wide-character API.
pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Turns a Win32 `BOOL` into a result carrying the thread's last error.
pub fn succeeded(result: Bool) -> io::Result<()> {
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Wraps a returned handle; null and `INVALID_HANDLE_VALUE` both mean failure.
pub fn owned(handle: Handle) -> io::Result<Owned> {
    if handle.is_null() || handle as isize == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(Owned(handle))
}

/// Turns a registry status code, which is returned rather than set, into a result.
pub fn status(code: u32) -> io::Result<()> {
    if code != 0 {
        return Err(io::Error::from_raw_os_error(code as i32));
    }
    Ok(())
}

/// Opens another process with exactly the requested access.
pub fn open_process(access: u32, pid: u32) -> io::Result<Owned> {
    // SAFETY: plain value arguments; the returned handle is checked and owned.
    owned(unsafe { OpenProcess(access, 0, pid) })
}

/// Opens an existing named event for signalling.
pub fn open_event(name: &str) -> io::Result<Owned> {
    const EVENT_MODIFY_STATE_AND_SYNCHRONIZE: u32 = 0x0002 | 0x0010_0000;
    let name = wide(name);
    // SAFETY: `name` is a live null-terminated buffer for the duration of the call.
    owned(unsafe { OpenEventW(EVENT_MODIFY_STATE_AND_SYNCHRONIZE, 0, name.as_ptr()) })
}
