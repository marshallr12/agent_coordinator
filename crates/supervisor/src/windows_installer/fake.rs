//! A host kept in memory, for the installer's platform-neutral tests. It
//! answers every question from its fields and changes them when a change is
//! applied, the way the Windows host changes the machine.
use super::access::{ADMINISTRATORS, AccessList, FULL, SYSTEM};
use super::config_text;
use super::control::{RunState, Services, start_line};
use super::install::{Host, Machine};
use super::model::{Desired, Layout, Recovery, Role, ServiceSpec, StartType};
use super::observed::{KeySource, PathState, ProfileState, ServiceState};
use super::plan::Action;
use super::release::Releases;
use anyhow::{Context, Result, bail, ensure};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration;

/// A machine in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeHost {
    /// Whether the session is elevated.
    pub elevated: bool,
    /// Every directory and the key file.
    pub paths: BTreeMap<String, PathState>,
    /// Text files by path.
    pub texts: BTreeMap<String, String>,
    /// Binary files by path: each one's digest.
    pub digests: BTreeMap<String, String>,
    /// The services' logs by path, shared with [`FakeServices`], which
    /// writes a start line when it starts a service.
    pub logs: Rc<RefCell<BTreeMap<String, String>>>,
    /// The profile of each account that has one.
    pub profiles: BTreeMap<String, ProfileState>,
    /// Paths whose removal fails, as a file nobody may delete would.
    pub stuck: Vec<String>,
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
            digests: BTreeMap::new(),
            logs: Rc::default(),
            profiles: BTreeMap::new(),
            stuck: Vec::new(),
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
            Action::InstallBinary => {
                let sha256 = desired.binary_sha256.clone();
                self.digests.insert(desired.layout.binary(), sha256);
            }
            Action::WriteManifest => drop(self.texts.insert(manifest().0, manifest().1)),
            Action::RecordRelease => {
                let path = desired.layout.releases();
                let existing = self.texts.get(&path).map(String::as_str);
                let text = Releases::with_current(existing, desired.release())?;
                self.texts.insert(path, text);
            }
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

    fn sha256(&self, path: &str) -> Result<Option<String>> {
        Ok(self.digests.get(path).cloned())
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

/// Whether `path` is `root` or lies below it.
fn below(path: &str, root: &str) -> bool {
    path == root || path.starts_with(&format!(r"{root}\"))
}

impl FakeHost {
    /// The text of a log or text file at `path`.
    fn content(&self, path: &str) -> Option<String> {
        let logged = self.logs.borrow().get(path).cloned();
        logged.or_else(|| self.texts.get(path).cloned())
    }

    /// Every path the machine holds.
    fn every_path(&self) -> Vec<String> {
        let logs: Vec<String> = self.logs.borrow().keys().cloned().collect();
        let files = self.texts.keys().chain(self.digests.keys()).cloned();
        self.paths
            .keys()
            .cloned()
            .chain(files)
            .chain(logs)
            .collect()
    }

    /// Removes `path` and everything below it, except what is stuck and
    /// the directories above what is stuck.
    fn forget(&mut self, path: &str, keep: &[String]) {
        let gone = |p: &String| below(p, path) && !keep.iter().any(|k| below(k, p));
        self.paths.retain(|p, _| !gone(p));
        self.texts.retain(|p, _| !gone(p));
        self.digests.retain(|p, _| !gone(p));
        self.logs.borrow_mut().retain(|p, _| !gone(p));
    }
}

impl Machine for FakeHost {
    fn copy(&mut self, from: &str, to: &str) -> Result<()> {
        ensure!(
            !self.stuck.iter().any(|s| s == to),
            "{to} cannot be written"
        );
        if let Some(digest) = self.digests.get(from).cloned() {
            self.digests.insert(to.to_owned(), digest);
            return Ok(());
        }
        let text = self
            .content(from)
            .with_context(|| format!("{from} does not exist"))?;
        self.texts.insert(to.to_owned(), text);
        Ok(())
    }

    fn write(&mut self, path: &str, text: &str) -> Result<()> {
        self.texts.insert(path.to_owned(), text.to_owned());
        Ok(())
    }

    fn remove(&mut self, path: &str) -> Result<()> {
        self.texts.remove(path);
        self.digests.remove(path);
        Ok(())
    }

    fn make_directory(&mut self, path: &str) -> Result<()> {
        let access = AccessList::directory(&[(SYSTEM, FULL), (ADMINISTRATORS, FULL)]);
        let owner = ADMINISTRATORS.to_owned();
        let directory = PathState::Directory { owner, access };
        self.paths.entry(path.to_owned()).or_insert(directory);
        Ok(())
    }

    fn entries(&self, path: &str) -> Result<Vec<String>> {
        let prefix = format!(r"{path}\");
        let mut names: Vec<String> = self
            .every_path()
            .iter()
            .filter_map(|p| p.strip_prefix(&prefix))
            .map(|rest| rest.split('\\').next().unwrap_or(rest).to_owned())
            .collect();
        names.sort();
        names.dedup();
        Ok(names)
    }

    fn length(&self, path: &str) -> Result<u64> {
        Ok(self.content(path).map_or(0, |text| text.len() as u64))
    }

    fn read_after(&self, path: &str, offset: u64) -> Result<Option<String>> {
        let text = self.content(path);
        Ok(text.map(|t| t.get(offset as usize..).unwrap_or_default().to_owned()))
    }

    fn remove_tree(&mut self, path: &str) -> Result<Vec<String>> {
        let stuck: Vec<String> = self
            .stuck
            .iter()
            .filter(|s| below(s, path))
            .cloned()
            .collect();
        self.forget(path, &stuck);
        Ok(stuck
            .iter()
            .map(|s| format!("{s}: access is denied"))
            .collect())
    }

    fn delete_service(&mut self, name: &str) -> Result<()> {
        self.services.remove(name);
        Ok(())
    }

    fn profile(&self, sid: &str) -> Result<ProfileState> {
        Ok(self
            .profiles
            .get(sid)
            .copied()
            .unwrap_or(ProfileState::Absent))
    }

    fn delete_profile(&mut self, sid: &str) -> Result<()> {
        let state = self.profile(sid)?;
        ensure!(
            state == ProfileState::Unloaded,
            "the profile of {sid} is {state:?}"
        );
        self.profiles.remove(sid);
        Ok(())
    }
}

/// The Service Control Manager in memory: each service's state, and a log
/// line written at each start, as a service of ours writes one.
#[derive(Debug)]
pub struct FakeServices {
    /// Where the logs are.
    pub layout: Layout,
    /// Each registered service's state.
    pub states: BTreeMap<String, RunState>,
    /// The logs, shared with the [`FakeHost`].
    pub logs: Rc<RefCell<BTreeMap<String, String>>>,
    /// Every start and stop asked for, in order.
    pub calls: Vec<String>,
    /// How many of the next starts run without logging their start, as a
    /// release that does not come up would.
    pub silent_starts: usize,
    /// Services that stop with the runner's code for launches whose end
    /// Windows did not confirm.
    pub unclean: Vec<String>,
}

impl FakeServices {
    /// The services of `host`, each stopped, logging to `host`'s logs.
    pub fn of(host: &FakeHost, layout: &Layout) -> Self {
        let stopped = RunState::Stopped {
            exit: 0,
            specific: 0,
        };
        Self {
            layout: layout.clone(),
            states: host.services.keys().map(|n| (n.clone(), stopped)).collect(),
            logs: host.logs.clone(),
            calls: Vec::new(),
            silent_starts: 0,
            unclean: Vec::new(),
        }
    }

    /// The names of the services that run.
    pub fn running(&self) -> Vec<String> {
        let running = self.states.iter().filter(|(_, s)| **s == RunState::Running);
        running.map(|(n, _)| n.clone()).collect()
    }

    /// Appends `role`'s start line to its log.
    fn log_start(&mut self, role: Role) {
        let path = self.layout.log(role);
        let mut logs = self.logs.borrow_mut();
        let log = logs.entry(path).or_default();
        // Only the runner's lines begin with a time.
        let time = if role == Role::Runner {
            "2026-11-24T03:00:00Z "
        } else {
            ""
        };
        log.push_str(&format!("{time}{}\n", start_line(role)));
    }
}

impl Services for FakeServices {
    fn state(&mut self, name: &str) -> Result<RunState> {
        Ok(self.states.get(name).copied().unwrap_or(RunState::Missing))
    }

    fn start(&mut self, name: &str) -> Result<()> {
        self.calls.push(format!("start {name}"));
        ensure!(self.states.contains_key(name), "{name} is not registered");
        self.states.insert(name.to_owned(), RunState::Running);
        let role = Role::ALL
            .into_iter()
            .find(|r| self.layout.service(*r) == name);
        match self.silent_starts {
            0 => self.log_start(role.context("not one of the four services")?),
            _ => self.silent_starts -= 1,
        }
        Ok(())
    }

    fn stop(&mut self, name: &str) -> Result<()> {
        self.calls.push(format!("stop {name}"));
        let unclean = self.unclean.iter().any(|n| n == name);
        let (exit, specific) = if unclean { (1066, 1) } else { (0, 0) };
        self.states
            .insert(name.to_owned(), RunState::Stopped { exit, specific });
        Ok(())
    }

    fn pause(&mut self, _duration: Duration) {}
}

/// A start type other than the one `seen` has.
pub fn other_start(seen: StartType) -> StartType {
    match seen {
        StartType::Auto => StartType::Demand,
        _ => StartType::Auto,
    }
}
