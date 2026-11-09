//! Named pipes and events: the authorized broker route and the host's own objects.

use crate::ffi;
use crate::report::Report;
use crate::spec::Spec;
use std::fs::OpenOptions;
use std::io::{self, Read, Write};

/// Records the authorized broker exchange, then attacks on host pipes and events.
pub fn run(spec: &Spec, report: &mut Report) -> Result<(), String> {
    let forged = spec.require("forged_launch")?;
    let reply = broker_exchange(spec.require("broker_pipe")?, forged);
    report.value("broker.reply", reply.as_deref().unwrap_or("none"));
    report.control("own.broker_route", reply.map(|_| ()));
    report.attack("host.pipe_connect", connect(spec.require("host_pipe")?));
    let event = spec.require("host_event")?;
    report.attack(
        "host.event_open_local",
        open_event(&format!("Local\\{event}")),
    );
    report.attack(
        "host.event_open_global",
        open_event(&format!("Global\\{event}")),
    );
    Ok(())
}

/// Sends a request that claims another launch's name and returns the broker's reply.
///
/// The broker must answer with the identity the OS reports, not the claimed one.
fn broker_exchange(pipe: &str, forged_launch: &str) -> io::Result<String> {
    let mut client = OpenOptions::new().read(true).write(true).open(pipe)?;
    client.write_all(format!("launch={forged_launch}\n").as_bytes())?;
    let mut buffer = [0_u8; 256];
    let length = client.read(&mut buffer)?;
    Ok(String::from_utf8_lossy(&buffer[..length])
        .trim()
        .to_string())
}

/// Opens a named pipe as a client.
pub fn connect(pipe: &str) -> io::Result<()> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(pipe)
        .map(|_| ())
}

/// Opens a named event.
pub fn open_event(name: &str) -> io::Result<()> {
    ffi::open_event(name).map(|_| ())
}
