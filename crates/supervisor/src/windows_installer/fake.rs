//! A host kept in memory, for the installer's platform-neutral tests. It
//! answers every question from its fields and changes them when a change is
//! applied, the way the Windows host changes the machine.
use super::access::ADMINISTRATORS;
use super::config_text;
use super::install::Host;
use super::model::{Desired, Recovery, ServiceSpec, StartType};
use super::observed::{KeySource, PathState, ServiceState};
use super::plan::Action;
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;

/// A machine in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeHost {
    /// Whether the session is elevated.
    pub elevated: bool,
    /// Every directory and the key file.
    pub paths: BTreeMap<String, PathState>,
    /// Text files by path.
    pub texts: BTreeMap<String, String>,
    /// The installed binary's digest.
    pub binary: Option<String>,
    /// Registered services by name.
    pub services: BTreeMap<String, ServiceState>,
    /// What this run's key file is like.
    pub key_source: KeySource,
    /// The privileges of each running process's token.
    pub tokens: BTreeMap<u32, Vec<String>>,
    /// Fail the change with this number (counting from 0), once.
    pub fail_at: Option<usize>,
    /// How many changes were applied.
    pub applied: usize,
}

impl FakeHost {
    /// A clean, elevated machine with a readable key file.
    pub fn clean() -> Self {
        Self {
            elevated: true,
            paths: BTreeMap::new(),
            texts: BTreeMap::new(),
            binary: None,
            services: BTreeMap::new(),
            key_source: KeySource::Readable { installed: false },
            tokens: BTreeMap::new(),
            fail_at: None,
            applied: 0,
        }
    }

    /// The registered service called `name`.
    pub fn spec(&mut self, name: &str) -> &mut ServiceSpec {
        &mut self
            .services
            .get_mut(name)
            .expect("a registered service")
            .spec
    }

    /// A service as Windows registers it before any setting is made.
    fn registered(want: &ServiceSpec) -> ServiceState {
        let spec = ServiceSpec {
            recovery: Recovery {
                reset_seconds: 0,
                actions: Vec::new(),
                command: String::new(),
            },
            preshutdown_ms: 180_000,
            privileges: Vec::new(),
            ..want.clone()
        };
        ServiceState {
            spec,
            process: None,
        }
    }

    /// Makes a directory or sets the access list of what is at `path`.
    fn secure(&mut self, desired: &Desired, path: &str) -> Result<()> {
        let owner = ADMINISTRATORS.to_owned();
        if let Some(directory) = desired.directory(path) {
            let access = directory.access.clone();
            self.paths
                .insert(path.to_owned(), PathState::Directory { owner, access });
            return Ok(());
        }
        let key = desired.key.as_ref().filter(|key| key.path == path);
        let access = key.context("no such path in the model")?.access.clone();
        self.paths
            .insert(path.to_owned(), PathState::File { owner, access });
        Ok(())
    }

    /// Changes one setting of a registered service to its desired value.
    fn set(&mut self, desired: &Desired, name: &str, action: &Action) -> Result<()> {
        let want = desired
            .service(name)
            .context("no such service in the model")?
            .clone();
        let spec = &mut self.services.get_mut(name).context("not registered")?.spec;
        match action {
            Action::SetBinPath { .. } => spec.bin_path = want.bin_path,
            Action::SetStartType { .. } => spec.start = want.start,
            Action::SetDependencies { .. } => spec.dependencies = want.dependencies,
            Action::SetAccount { .. } => spec.account = want.account,
            Action::SetRecovery { .. } => spec.recovery = want.recovery,
            Action::SetPreshutdown { .. } => spec.preshutdown_ms = want.preshutdown_ms,
            Action::SetPrivileges { .. } => spec.privileges = want.privileges,
            other => bail!("{other:?} is not a service setting"),
        }
        Ok(())
    }

    /// Registers a service or changes one of its settings.
    fn change_service(&mut self, desired: &Desired, name: &str, action: &Action) -> Result<()> {
        if !matches!(action, Action::CreateService { .. }) {
            return self.set(desired, name, action);
        }
        let want = desired.service(name).context("no such service")?;
        self.services
            .insert(name.to_owned(), Self::registered(want));
        Ok(())
    }

    /// Makes a change that is not to a service.
    fn change_files(&mut self, desired: &Desired, action: &Action) -> Result<()> {
        let manifest = || (desired.layout.manifest(), desired.manifest.clone());
        match action {
            Action::CreateDirectory { path } | Action::SetAccessList { path } => {
                return self.secure(desired, path);
            }
            Action::InstallBinary => self.binary = Some(desired.binary_sha256.clone()),
            Action::WriteManifest => drop(self.texts.insert(manifest().0, manifest().1)),
            Action::WriteConfig { .. } => return self.write_config(desired),
            Action::InstallKey => return self.install_key(desired),
            other => bail!("{other:?} is a service change"),
        }
        Ok(())
    }

    /// Writes the configuration as the Windows host does: merged.
    fn write_config(&mut self, desired: &Desired) -> Result<()> {
        let path = desired.layout.config();
        let text =
            config_text::merged(self.texts.get(&path).map(String::as_str), &desired.settings)?;
        self.texts.insert(path, text);
        Ok(())
    }

    /// Copies the key: the file appears with its access list.
    fn install_key(&mut self, desired: &Desired) -> Result<()> {
        let key = desired.key.as_ref().context("no key in the model")?;
        self.secure(desired, &key.path.clone())?;
        self.key_source = KeySource::Readable { installed: true };
        Ok(())
    }
}

impl Host for FakeHost {
    fn elevated(&self) -> Result<bool> {
        Ok(self.elevated)
    }

    fn path(&self, path: &str) -> Result<PathState> {
        Ok(self.paths.get(path).cloned().unwrap_or(PathState::Missing))
    }

    fn text(&self, path: &str) -> Result<Option<String>> {
        Ok(self.texts.get(path).cloned())
    }

    fn sha256(&self, _path: &str) -> Result<Option<String>> {
        Ok(self.binary.clone())
    }

    fn service(&self, name: &str) -> Result<Option<ServiceState>> {
        Ok(self.services.get(name).cloned())
    }

    fn key_source(&self, _source: &str, _installed: Option<&str>) -> KeySource {
        self.key_source.clone()
    }

    fn privileges(&self, process: u32) -> Result<Vec<String>> {
        self.tokens
            .get(&process)
            .cloned()
            .context("the token could not be opened")
    }

    fn apply(&mut self, desired: &Desired, action: &Action) -> Result<()> {
        if self.fail_at.take_if(|at| *at == self.applied).is_some() {
            bail!("the machine refused the change");
        }
        self.applied += 1;
        match action.service() {
            Some(name) => self.change_service(desired, name, action),
            None => self.change_files(desired, action),
        }
    }
}

/// A start type other than the one `seen` has.
pub fn other_start(seen: StartType) -> StartType {
    match seen {
        StartType::Auto => StartType::Demand,
        _ => StartType::Auto,
    }
}
