//! The Windows host the installer works on (phase 7, step 7a): the
//! [`Host`] of [`crate::windows_installer`], answered by the Service
//! Control Manager and the file system. Every decision is the neutral
//! core's; this module reads what Windows says and makes one change when
//! told to.
//!
//! It registers services and never starts, stops or deletes one. It makes
//! and secures directories and never removes one. It runs no program.
pub mod access;
pub mod files;
pub mod services;
pub mod tokens;

use super::reader;
use crate::windows_installer::Host;
use crate::windows_installer::config_text;
use crate::windows_installer::model::{Desired, ServiceSpec};
use crate::windows_installer::observed::{KeySource, PathState, ServiceState};
use crate::windows_installer::plan::Action;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// This machine. `binary` is the file `install` copies into the program
/// directory: the running executable.
#[derive(Debug)]
pub struct WindowsHost {
    binary: PathBuf,
}

impl WindowsHost {
    /// This machine, installing the running executable.
    pub fn new() -> Result<Self> {
        let binary = std::env::current_exe().context("find the running executable")?;
        Ok(Self { binary })
    }

    /// The SHA-256, in hex, of the binary `install` copies.
    pub fn binary_sha256(&self) -> Result<String> {
        let path = self.binary.display().to_string();
        files::sha256(&path)?.with_context(|| format!("{path} does not exist"))
    }

    /// Makes the directory at `path` with its access list.
    fn create_directory(desired: &Desired, path: &str) -> Result<()> {
        let spec = desired
            .directory(path)
            .context("no such directory in the model")?;
        reader::create_directory(Path::new(path), &spec.access.creation_sddl()?)
    }

    /// Sets the access list of the modeled directory or key at `path`.
    fn secure(desired: &Desired, path: &str) -> Result<()> {
        if let Some(directory) = desired.directory(path) {
            return access::set(path, &directory.access);
        }
        let key = desired.key.as_ref().filter(|key| key.path == path);
        access::set(path, &key.context("no such path in the model")?.access)
    }

    /// Writes the configuration: the owner's entries with the managed ones
    /// set as wanted.
    fn write_config(desired: &Desired) -> Result<()> {
        let path = desired.layout.config();
        let found = files::read_text(&path)?;
        let text = config_text::merged(found.as_deref(), &desired.settings)?;
        files::write_text(&path, &text)
    }

    /// Copies this run's key file into place under the key's access list.
    fn install_key(desired: &Desired) -> Result<()> {
        let key = desired.key.as_ref().context("no key in the model")?;
        let source = key.source.as_deref().context("no key file was given")?;
        files::install_key(source, &key.path, &key.access.creation_sddl()?)
    }

    /// Makes one change to a registered service.
    fn set_service(spec: &ServiceSpec, action: &Action) -> Result<()> {
        match action {
            Action::CreateService { .. } => services::create(spec),
            Action::SetBinPath { .. } => services::set_bin_path(spec),
            Action::SetStartType { .. } => services::set_start_type(spec),
            Action::SetDependencies { .. } => services::set_dependencies(spec),
            Action::SetAccount { .. } => services::set_account(spec),
            Action::SetRecovery { .. } => services::set_recovery(spec),
            Action::SetPreshutdown { .. } => services::set_preshutdown(spec),
            Action::SetPrivileges { .. } => services::set_privileges(spec),
            other => bail!("{other:?} is not a service change"),
        }
    }
}

impl Host for WindowsHost {
    fn elevated(&self) -> Result<bool> {
        tokens::elevated()
    }

    fn path(&self, path: &str) -> Result<PathState> {
        access::observe(path)
    }

    fn text(&self, path: &str) -> Result<Option<String>> {
        files::read_text(path)
    }

    fn sha256(&self, path: &str) -> Result<Option<String>> {
        files::sha256(path)
    }

    fn service(&self, name: &str) -> Result<Option<ServiceState>> {
        services::observe(name)
    }

    fn key_source(&self, source: &str, installed: Option<&str>) -> KeySource {
        files::key_source(source, installed)
    }

    fn privileges(&self, process: u32) -> Result<Vec<String>> {
        tokens::privileges(process)
    }

    fn apply(&mut self, desired: &Desired, action: &Action) -> Result<()> {
        let layout = &desired.layout;
        match action {
            Action::CreateDirectory { path } => Self::create_directory(desired, path),
            Action::SetAccessList { path } => Self::secure(desired, path),
            Action::InstallBinary => files::copy_binary(&self.binary, &layout.binary()),
            Action::WriteManifest => files::write_text(&layout.manifest(), &desired.manifest),
            Action::WriteConfig { .. } => Self::write_config(desired),
            Action::InstallKey => Self::install_key(desired),
            other => {
                let name = other.service().context("a change without a target")?;
                let spec = desired
                    .service(name)
                    .context("no such service in the model")?;
                Self::set_service(spec, other)
            }
        }
    }
}
