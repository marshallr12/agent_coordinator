//! Workload and adversarial fixture for `windows-containment-probe.py`.
//!
//! The probe builds and tests this crate inside a Windows launch boundary, then
//! runs it as several concurrent launches. Each launch first creates resources
//! of its own, then attacks the host and the other launches, and writes one
//! `key=value` row per observation. The fixture records facts only: the driver
//! holds the expected value for every row and fails closed on anything missing.
//!
//! It is a synthetic stand-in, never a provider harness, and it reads no real
//! credential: every protected target is a canary the driver created.

mod report;
mod spec;

#[cfg(windows)]
mod checks;
#[cfg(windows)]
mod ffi;
#[cfg(windows)]
mod modes;
#[cfg(windows)]
mod victim;

/// Runs the mode named by the first argument and exits with its status.
#[cfg(windows)]
fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(modes::dispatch(&arguments));
}

/// The fixture exercises Windows boundaries; elsewhere only its pure tests are useful.
#[cfg(not(windows))]
fn main() {
    eprintln!("this fixture runs on native Windows only");
    std::process::exit(2);
}
