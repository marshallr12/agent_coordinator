//! Resources a launch owns while its peers attack it: a note, a pipe and an event.

use crate::ffi;
use crate::report::Report;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::Path;

/// File name of the private note inside a launch's working directory.
pub const NOTE: &str = "victim-note.txt";

/// The launch's own resources, kept alive until the launch exits.
pub struct Resources {
    note: io::Result<()>,
    pipe: io::Result<Vec<ffi::Owned>>,
    event: io::Result<ffi::Owned>,
    name: String,
}

impl Resources {
    /// Creates the note, three instances of the pipe and the event.
    pub fn create(name: &str, work: &Path) -> Self {
        Self {
            note: fs::write(work.join(NOTE), b"launch-private"),
            pipe: (0..3).map(|_| pipe_instance(&pipe_name(name))).collect(),
            event: event(&event_name(name)),
            name: name.to_string(),
        }
    }

    /// Records that each resource exists and that its owner can reach it.
    pub fn record(&self, report: &mut Report) {
        report.control("own.victim_note", copy(&self.note));
        report.control("own.pipe_server", copy(&self.pipe));
        report.control("own.pipe_connect", connect(&pipe_name(&self.name)));
        report.control("own.event_create", copy(&self.event));
        report.control(
            "own.event_open",
            ffi::open_event(&event_name(&self.name)).map(|_| ()),
        );
    }
}

/// The launch's pipe, inside the per-package `LOCAL` pipe namespace.
fn pipe_name(launch: &str) -> String {
    format!("\\\\.\\pipe\\LOCAL\\agentc-victim-{launch}")
}

/// The launch's event, inside the per-package named-object namespace.
fn event_name(launch: &str) -> String {
    format!("agentc-victim-{launch}-event")
}

/// Creates one listening instance of a byte-mode pipe that refuses remote clients.
fn pipe_instance(name: &str) -> io::Result<ffi::Owned> {
    const DUPLEX: u32 = 0x0000_0003;
    const BYTE_MODE_REJECT_REMOTE: u32 = 0x0000_0008;
    let name = ffi::wide(name);
    // SAFETY: `name` is a live null-terminated buffer; a null descriptor asks for
    // the creator's default security.
    ffi::owned(unsafe {
        ffi::CreateNamedPipeW(
            name.as_ptr(),
            DUPLEX,
            BYTE_MODE_REJECT_REMOTE,
            8,
            4096,
            4096,
            0,
            std::ptr::null(),
        )
    })
}

/// Creates the launch's named event.
fn event(name: &str) -> io::Result<ffi::Owned> {
    let name = ffi::wide(name);
    // SAFETY: `name` is a live null-terminated buffer; a null descriptor asks for
    // the creator's default security.
    ffi::owned(unsafe { ffi::CreateEventW(std::ptr::null(), 1, 0, name.as_ptr()) })
}

/// Positive control for the pipe-client code path: the owner can connect to its pipe.
fn connect(name: &str) -> io::Result<()> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(name)
        .map(|_| ())
}

/// Turns a stored result into a row, rebuilding the error it holds.
fn copy<T>(result: &io::Result<T>) -> io::Result<()> {
    match result {
        Ok(_) => Ok(()),
        Err(error) => Err(match error.raw_os_error() {
            Some(code) => io::Error::from_raw_os_error(code),
            None => io::Error::from(error.kind()),
        }),
    }
}
