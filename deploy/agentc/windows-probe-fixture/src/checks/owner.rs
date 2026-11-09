//! Attacks on a protected owner who is a different account from the launch.
//!
//! In role-account mode the launch runs as a role account and the owner's
//! canaries belong to someone else. These rows exist only when the spec names
//! an owner directory; the single-account mode has no separate owner.

use super::{files, ipc, registry};
use crate::ffi;
use crate::report::Report;
use crate::spec::Spec;
use std::path::Path;

/// Records every attack on the owner's files, process, pipe and events.
pub fn run(spec: &Spec, report: &mut Report) -> Result<(), String> {
    let Some(directory) = spec.get("owner_dir") else {
        return Ok(());
    };
    owner_files(spec, Path::new(directory), report)?;
    owner_process(spec, report)?;
    report.attack(
        "owner.pipe_connect",
        ipc::connect(spec.require("owner_pipe")?),
    );
    let event = spec.require("owner_event")?;
    report.attack(
        "owner.event_open_local",
        ipc::open_event(&format!("Local\\{event}")),
    );
    report.attack(
        "owner.event_open_global",
        ipc::open_event(&format!("Global\\{event}")),
    );
    let identity = spec.require("identity_environment_key")?;
    report.attack(
        "identity.registry_environment_open",
        registry::open_for_query(identity),
    );
    Ok(())
}

/// Attacks the owner's private directory, its canary and its credential stand-in.
fn owner_files(spec: &Spec, directory: &Path, report: &mut Report) -> Result<(), String> {
    let canary = Path::new(spec.require("owner_file")?);
    report.attack("owner.file_read", files::read(canary));
    report.attack("owner.file_write", files::open_for_write(canary));
    report.attack("owner.dir_list", files::list(directory));
    let credential = Path::new(spec.require("owner_credential")?);
    report.attack("owner.credential_read", files::read(credential));
    Ok(())
}

/// Attacks the owner's process.
fn owner_process(spec: &Spec, report: &mut Report) -> Result<(), String> {
    let pid: u32 = spec
        .require("owner_pid")?
        .parse()
        .map_err(|_| "owner_pid is not a number".to_string())?;
    let vm = ffi::open_process(ffi::PROCESS_VM_ALL, pid).map(|_| ());
    report.attack("owner.process_open_vm", vm);
    let query = ffi::open_process(ffi::PROCESS_QUERY_LIMITED, pid).map(|_| ());
    report.attack("owner.process_open_query", query);
    Ok(())
}
