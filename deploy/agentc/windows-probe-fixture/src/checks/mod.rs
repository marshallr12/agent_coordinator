//! Every observation one launch makes, grouped by the boundary it tests.

mod desktop;
mod files;
mod ipc;
mod network;
mod owner;
mod paths;
mod peers;
mod process;
mod registry;
pub mod scheduler;
pub mod token;

use crate::report::Report;
use crate::spec::Spec;
use std::path::Path;

/// Runs every check in a fixed order and appends its rows.
///
/// An error means the spec was unusable; the driver then sees missing rows and
/// fails closed.
pub fn run(spec: &Spec, work: &Path, report: &mut Report) -> Result<(), String> {
    token::run(report);
    report.value("env.names", &environment_names());
    files::run(spec, work, report)?;
    process::run(spec, report)?;
    scheduler::run(report);
    ipc::run(spec, report)?;
    registry::run(spec, report)?;
    owner::run(spec, report)?;
    network::run(spec, report);
    paths::run(spec, work, report)?;
    desktop::run(report);
    peers::run(spec, report)
}

/// The sorted names of every environment variable the launch received.
fn environment_names() -> String {
    let mut names: Vec<String> = std::env::vars_os()
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .collect();
    names.sort();
    names.join(",")
}
