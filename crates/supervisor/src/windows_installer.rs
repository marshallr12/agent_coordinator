//! The Windows host installer's core (phase 7, step 7a): what an installed
//! host looks like, what a host looks like now, and the changes that turn
//! one into the other.
//!
//! Nothing in this module calls Windows, so it is compiled and tested on
//! every platform. [`model`] builds the desired state from a [`Layout`]
//! and the owner's [`Inputs`]; [`observed`] holds what a [`Host`] reports;
//! [`plan`] refuses a host it must not adopt and otherwise lists the
//! changes; [`install`] applies them one at a time, each decided on a fresh
//! observation and each journaled. The Windows [`Host`] is
//! `host_runtime::windows::installer`.
//!
//! The installer never runs a program it is given, never prints key bytes
//! (no value here holds any), and never follows a link: a directory that is
//! a link, or that someone else owns, is refused before anything changes.
pub mod access;
pub mod config_text;
pub mod install;
pub mod journal;
pub mod model;
pub mod observed;
pub mod plan;
pub mod service_sid;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;

pub use install::{Host, Report, install, install_from, status, status_from};
pub use model::{Desired, Inputs, Layout, Overrides};
