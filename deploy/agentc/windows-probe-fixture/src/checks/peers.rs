//! Attacks on the other concurrent launches.
//!
//! Every peer is alive and holds a private note, a pipe, an event and a broker
//! route while these run. The driver labels each row by role pair, so the same
//! attacks cover implementer-to-reviewer, reviewer-to-implementer and
//! same-role isolation.

use super::{files, ipc};
use crate::ffi;
use crate::report::Report;
use crate::spec::Spec;
use std::fs;
use std::path::Path;

/// Records every attack against every peer named in the spec.
pub fn run(spec: &Spec, report: &mut Report) -> Result<(), String> {
    for name in spec.peers() {
        let field = |field: &str| {
            spec.peer(name, field)
                .ok_or_else(|| format!("spec is missing peer.{name}.{field}"))
        };
        resources(name, Path::new(field("dir")?), report);
        processes(name, field("pid")?, report)?;
        report.attack(
            &format!("peer.{name}.pipe_connect"),
            ipc::connect(field("pipe")?),
        );
        report.attack(
            &format!("peer.{name}.event_open"),
            ipc::open_event(field("event")?),
        );
        report.attack(
            &format!("peer.{name}.broker_connect"),
            ipc::connect(field("broker_pipe")?),
        );
    }
    Ok(())
}

/// Attacks a peer's private working directory.
fn resources(name: &str, directory: &Path, report: &mut Report) {
    let note = directory.join(crate::victim::NOTE);
    report.attack(&format!("peer.{name}.file_read"), files::read(&note));
    report.attack(
        &format!("peer.{name}.file_write"),
        fs::write(directory.join("planted.txt"), b"x"),
    );
    report.attack(&format!("peer.{name}.dir_list"), files::list(directory));
}

/// Attacks a peer's process.
fn processes(name: &str, pid: &str, report: &mut Report) -> Result<(), String> {
    let pid: u32 = pid
        .parse()
        .map_err(|_| format!("peer.{name}.pid is not a number"))?;
    let vm = ffi::open_process(ffi::PROCESS_VM_ALL, pid).map(|_| ());
    report.attack(&format!("peer.{name}.process_open_vm"), vm);
    let query = ffi::open_process(ffi::PROCESS_QUERY_LIMITED, pid).map(|_| ());
    report.attack(&format!("peer.{name}.process_open_query"), query);
    Ok(())
}
