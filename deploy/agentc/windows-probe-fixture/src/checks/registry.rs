//! Registry access to the protected owner's hive.
//!
//! Every key is addressed under `HKEY_USERS\<owner SID>`, so the same check is
//! meaningful whether the launch runs as the owner or as a separate role
//! account. Real keys are only opened, never read: the one value that is read
//! is a canary the driver created.

use crate::ffi;
use crate::report::Report;
use crate::spec::Spec;
use std::io;

/// Records read and write attacks on the owner's hive.
pub fn run(spec: &Spec, report: &mut Report) -> Result<(), String> {
    let canary = spec.require("owner_hive_key")?;
    report.attack("host.registry_owner_read", read_canary(canary));
    report.attack(
        "host.registry_owner_write",
        open(canary, ffi::KEY_SET_VALUE),
    );
    let environment = spec.require("owner_environment_key")?;
    report.attack(
        "host.registry_environment_open",
        open(environment, ffi::KEY_QUERY_VALUE),
    );
    let run = spec.require("owner_run_key")?;
    report.attack("host.registry_run_write", open(run, ffi::KEY_SET_VALUE));
    Ok(())
}

/// A registry key closed when dropped.
struct Key(ffi::Handle);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the key came from a successful open and is closed once.
        unsafe { ffi::RegCloseKey(self.0) };
    }
}

/// Opens a key under `HKEY_USERS` with exactly the requested access.
fn open_key(path: &str, access: u32) -> io::Result<Key> {
    let path = ffi::wide(path);
    let mut key: ffi::Handle = std::ptr::null_mut();
    // SAFETY: `path` is a live null-terminated buffer and `key` outlives the call.
    ffi::status(unsafe {
        ffi::RegOpenKeyExW(ffi::HKEY_USERS, path.as_ptr(), 0, access, &mut key)
    })?;
    Ok(Key(key))
}

/// Opens a key for reading values and closes it again without reading any.
pub fn open_for_query(path: &str) -> io::Result<()> {
    open(path, ffi::KEY_QUERY_VALUE)
}

/// Opens a key and closes it again; the granted access is the observation.
fn open(path: &str, access: u32) -> io::Result<()> {
    open_key(path, access).map(|_| ())
}

/// Reads the driver's canary value from its disposable key.
fn read_canary(path: &str) -> io::Result<()> {
    let key = open_key(path, ffi::KEY_QUERY_VALUE)?;
    let name = ffi::wide("canary");
    let mut data = [0_u8; 128];
    let mut length = data.len() as u32;
    // SAFETY: every pointer refers to a live local and `length` holds the buffer size.
    ffi::status(unsafe {
        ffi::RegQueryValueExW(
            key.0,
            name.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            data.as_mut_ptr(),
            &mut length,
        )
    })
}
