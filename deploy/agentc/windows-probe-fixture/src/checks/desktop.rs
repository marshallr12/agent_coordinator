//! Window station and desktop isolation.
//!
//! A launch gets a private window station and desktop, which separates its
//! clipboard, atoms and window messages from the interactive session and from
//! other launches. `user32` is loaded on demand so that a boundary which
//! cannot load it is reported instead of preventing the fixture from starting.

use crate::ffi;
use crate::report::{Report, code};
use std::ffi::c_void;
use std::io;

type GetStation = unsafe extern "system" fn() -> ffi::Handle;
type GetDesktop = unsafe extern "system" fn(u32) -> ffi::Handle;
type ObjectInformation =
    unsafe extern "system" fn(ffi::Handle, i32, *mut c_void, u32, *mut u32) -> ffi::Bool;
type OpenStation = unsafe extern "system" fn(*const u16, ffi::Bool, u32) -> ffi::Handle;
type CloseStation = unsafe extern "system" fn(ffi::Handle) -> ffi::Bool;
type FindWindow = unsafe extern "system" fn(*const u16, *const u16) -> ffi::Handle;
type OpenClipboard = unsafe extern "system" fn(ffi::Handle) -> ffi::Bool;
type CountFormats = unsafe extern "system" fn() -> i32;
type CloseClipboard = unsafe extern "system" fn() -> ffi::Bool;

/// Records the launch's station and attacks on the interactive one.
pub fn run(report: &mut Report) {
    let user = User32::load();
    report.value(
        "desktop.station",
        &describe(user.as_ref().map_err(copy).and_then(station_name)),
    );
    report.attack(
        "host.station_open",
        user.as_ref().map_err(copy).and_then(open_interactive),
    );
    report.attack(
        "host.window_find",
        user.as_ref().map_err(copy).and_then(find_shell),
    );
    report.value(
        "desktop.clipboard_formats",
        &describe(user.as_ref().map_err(copy).and_then(clipboard)),
    );
}

/// The loaded `user32` module.
struct User32(ffi::Handle);

impl User32 {
    /// Loads `user32.dll`, which connects the process to its window station.
    fn load() -> io::Result<Self> {
        let name = ffi::wide("user32.dll");
        // SAFETY: `name` is a live null-terminated buffer.
        let module = unsafe { ffi::LoadLibraryW(name.as_ptr()) };
        match module.is_null() {
            true => Err(io::Error::last_os_error()),
            false => Ok(Self(module)),
        }
    }

    /// Looks up an export and views it as the function type `F`.
    ///
    /// # Safety
    /// `F` must be the export's real signature and `name` must be null-terminated.
    unsafe fn symbol<F: Copy>(&self, name: &[u8]) -> io::Result<F> {
        // SAFETY: the module is loaded and the caller supplied a terminated name.
        let address = unsafe { ffi::GetProcAddress(self.0, name.as_ptr()) };
        if address.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a function pointer and a data pointer have the same size here,
        // and the caller guarantees the signature.
        Ok(unsafe { std::mem::transmute_copy::<*mut c_void, F>(&address) })
    }
}

/// Rebuilds an error from a borrowed one, so one load failure can feed several rows.
fn copy(error: &io::Error) -> io::Error {
    match error.raw_os_error() {
        Some(code) => io::Error::from_raw_os_error(code),
        None => io::Error::from(error.kind()),
    }
}

/// `STATION\DESKTOP` for the calling thread.
fn station_name(user: &User32) -> io::Result<String> {
    // SAFETY: the three exports have exactly these documented signatures.
    let (station, desktop, info) = unsafe {
        (
            user.symbol::<GetStation>(b"GetProcessWindowStation\0")?,
            user.symbol::<GetDesktop>(b"GetThreadDesktop\0")?,
            user.symbol::<ObjectInformation>(b"GetUserObjectInformationW\0")?,
        )
    };
    // SAFETY: both calls take no pointers and return borrowed handles.
    let (station, desktop) = unsafe { (station(), desktop(ffi::GetCurrentThreadId())) };
    Ok(format!(
        "{}\\{}",
        object_name(info, station)?,
        object_name(info, desktop)?
    ))
}

/// The name of a window station or desktop object.
fn object_name(info: ObjectInformation, object: ffi::Handle) -> io::Result<String> {
    const UOI_NAME: i32 = 2;
    let mut buffer = [0_u16; 256];
    let mut needed = 0_u32;
    // SAFETY: the buffer is writable for the byte length passed.
    ffi::succeeded(unsafe {
        info(
            object,
            UOI_NAME,
            buffer.as_mut_ptr().cast(),
            512,
            &mut needed,
        )
    })?;
    let length = buffer
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(buffer.len());
    Ok(String::from_utf16_lossy(&buffer[..length]))
}

/// Tries to open the interactive window station with clipboard access.
fn open_interactive(user: &User32) -> io::Result<()> {
    const WINSTA_READATTRIBUTES_AND_CLIPBOARD: u32 = 0x0002 | 0x0004;
    // SAFETY: both exports have exactly these documented signatures.
    let (open, close) = unsafe {
        (
            user.symbol::<OpenStation>(b"OpenWindowStationW\0")?,
            user.symbol::<CloseStation>(b"CloseWindowStation\0")?,
        )
    };
    let name = ffi::wide("WinSta0");
    // SAFETY: `name` is a live null-terminated buffer.
    let station = unsafe { open(name.as_ptr(), 0, WINSTA_READATTRIBUTES_AND_CLIPBOARD) };
    if station.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the station handle was just opened and is closed once.
    unsafe { close(station) };
    Ok(())
}

/// Tries to find the interactive shell's taskbar window.
fn find_shell(user: &User32) -> io::Result<()> {
    // SAFETY: the export has exactly this documented signature.
    let find = unsafe { user.symbol::<FindWindow>(b"FindWindowW\0")? };
    let class = ffi::wide("Shell_TrayWnd");
    // SAFETY: `class` is a live null-terminated buffer; a null title matches any.
    let window = unsafe { find(class.as_ptr(), std::ptr::null()) };
    match window.is_null() {
        true => Err(io::Error::from_raw_os_error(1400)),
        false => Ok(()),
    }
}

/// How many clipboard formats this launch's window station currently holds.
fn clipboard(user: &User32) -> io::Result<String> {
    // SAFETY: the three exports have exactly these documented signatures.
    let (open, count, close) = unsafe {
        (
            user.symbol::<OpenClipboard>(b"OpenClipboard\0")?,
            user.symbol::<CountFormats>(b"CountClipboardFormats\0")?,
            user.symbol::<CloseClipboard>(b"CloseClipboard\0")?,
        )
    };
    // SAFETY: a null owner window is allowed; the clipboard is closed after counting.
    ffi::succeeded(unsafe { open(std::ptr::null_mut()) })?;
    let formats = unsafe { count() };
    unsafe { close() };
    Ok(formats.to_string())
}

/// Renders a text fact, keeping a query failure visible.
fn describe(fact: io::Result<String>) -> String {
    fact.unwrap_or_else(|error| format!("query-failed:{}", code(&error)))
}
