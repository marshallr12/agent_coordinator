//! Windows path attacks: junctions, symbolic links, hard links and streams.
//!
//! Two things are tested. From inside, a link the launch plants must not reach
//! a host file. From outside, the driver's handle-based reader must refuse the
//! objects planted under `collect`, so a host broker cannot be tricked into
//! following a worker-controlled link with host rights.

use super::files;
use super::process::system_tool;
use crate::report::Report;
use crate::spec::Spec;
use std::fs;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

/// Records link and stream attacks, then plants material for the host-side reader.
pub fn run(spec: &Spec, work: &Path, report: &mut Report) -> Result<(), String> {
    let host_dir = Path::new(spec.require("host_dir")?);
    let canary = Path::new(spec.require("host_file")?);
    let name = canary.file_name().ok_or("host_file has no name")?;
    report.value(
        "path.junction_create",
        &created(junction(&work.join("jx"), host_dir)),
    );
    report.attack(
        "path.junction_read",
        files::read(&work.join("jx").join(name)),
    );
    let link = work.join("sl.txt");
    report.value(
        "path.symlink_create",
        &created(std::os::windows::fs::symlink_file(canary, &link)),
    );
    report.attack("path.symlink_read", files::read(&link));
    report.attack(
        "path.hardlink_host",
        fs::hard_link(canary, work.join("hl.txt")),
    );
    let tool = Path::new(spec.require("tool_file")?);
    report.attack(
        "path.hardlink_tool",
        fs::hard_link(tool, work.join("tool-link.bin")),
    );
    report.attack("path.stream_host", fs::write(stream(canary), b"x"));
    report.value("path.planted", &plant(&work.join("collect"), host_dir));
    Ok(())
}

/// Creates a directory junction with `mklink /J`, which needs no privilege.
fn junction(link: &Path, target: &Path) -> io::Result<()> {
    let status = Command::new(system_tool("cmd.exe"))
        .args(["/d", "/c", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    match status.code() {
        Some(0) => Ok(()),
        code => Err(io::Error::from_raw_os_error(code.unwrap_or(-1))),
    }
}

/// The alternate data stream `probe` of a file.
fn stream(path: &Path) -> std::ffi::OsString {
    let mut name = path.as_os_str().to_os_string();
    name.push(":probe");
    name
}

/// Describes whether planting a link worked.
fn created(result: io::Result<()>) -> String {
    match result {
        Ok(()) => "created".to_string(),
        Err(error) => format!("refused:{}", crate::report::code(&error)),
    }
}

/// Plants a plain file, a hard-linked pair and a junction; returns what now exists.
fn plant(collect: &Path, host_dir: &Path) -> String {
    let mut planted = Vec::new();
    let attempts: [(&str, io::Result<()>); 3] = [
        (
            "plain",
            fs::create_dir_all(collect)
                .and_then(|()| fs::write(collect.join("plain.txt"), b"plain")),
        ),
        ("twins", twins(collect)),
        ("redirect", junction(&collect.join("redirect"), host_dir)),
    ];
    for (name, result) in attempts {
        if result.is_ok() {
            planted.push(name);
        }
    }
    planted.join(",")
}

/// Two names for one own file, so its link count is two.
fn twins(collect: &Path) -> io::Result<()> {
    fs::write(collect.join("twin-a.txt"), b"twin")?;
    fs::hard_link(collect.join("twin-a.txt"), collect.join("twin-b.txt"))
}
