//! File and directory access: the launch's own area, host canaries and tools.

use crate::report::Report;
use crate::spec::Spec;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::Path;

/// Records own-area controls, then every worker-to-host file attack.
pub fn run(spec: &Spec, work: &Path, report: &mut Report) -> Result<(), String> {
    report.control(
        "own.write_read",
        write_then_read(&work.join("own-note.txt")),
    );
    host_files(spec, report)?;
    tools(spec, report)?;
    for (name, path) in spec.group("alias") {
        report.attack(&format!("host.alias.{name}"), read(Path::new(path)));
    }
    Ok(())
}

/// Attacks the host-only directory, its canary and its credential stand-in.
fn host_files(spec: &Spec, report: &mut Report) -> Result<(), String> {
    let directory = Path::new(spec.require("host_dir")?);
    let canary = Path::new(spec.require("host_file")?);
    report.attack("host.file_read", read(canary));
    report.attack("host.file_write", open_for_write(canary));
    report.attack("host.file_delete", fs::remove_file(canary));
    report.attack("host.dir_list", list(directory));
    report.attack("host.dir_create", fs::create_dir(directory.join("planted")));
    report.attack(
        "host.credential_read",
        read(Path::new(spec.require("host_credential")?)),
    );
    report.attack(
        "host.profile_list",
        list(Path::new(spec.require("owner_profile")?)),
    );
    Ok(())
}

/// A read-only tool must be readable and must stay unwritable; a file granted only to
/// "all application packages" shows which kind of token the launch really holds.
fn tools(spec: &Spec, report: &mut Report) -> Result<(), String> {
    let tool = Path::new(spec.require("tool_file")?);
    report.control("own.tool_read", read(tool));
    report.attack("host.tool_write", open_for_write(tool));
    let shared = Path::new(spec.require("all_packages_file")?);
    report.attack("boundary.all_packages_read", read(shared));
    Ok(())
}

/// Writes a note and reads it back.
fn write_then_read(path: &Path) -> io::Result<()> {
    fs::write(path, b"launch-private")?;
    match fs::read(path)? == b"launch-private" {
        true => Ok(()),
        false => Err(io::Error::from_raw_os_error(13)),
    }
}

/// Opens a file and reads its first byte, so an open that defers the check still counts.
pub fn read(path: &Path) -> io::Result<()> {
    let mut byte = [0_u8; 1];
    File::open(path)?.read(&mut byte).map(|_| ())
}

/// Opens an existing file for writing without changing it.
pub fn open_for_write(path: &Path) -> io::Result<()> {
    OpenOptions::new().write(true).open(path).map(|_| ())
}

/// Lists a directory, forcing the first entry to be produced.
pub fn list(path: &Path) -> io::Result<()> {
    match fs::read_dir(path)?.next() {
        Some(entry) => entry.map(|_| ()),
        None => Ok(()),
    }
}
