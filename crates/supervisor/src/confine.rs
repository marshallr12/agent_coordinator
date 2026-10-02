//! Per-launch state layout and conservative retention for cooperating supervisors.
//!
//! Locks serialize preparation, launch and pruning. A started run is never
//! reused; only a recorded terminal run is eligible for pruning. Losing the
//! supervisor or failing to observe exit leaves the state retained. This is not
//! a security boundary against the role uid: host OS write confinement and
//! enforcement of descendant process lifetime are separate requirements.
use crate::config::Config;
use crate::profile::{LaunchSpec, Role};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{ErrorKind, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const LOCK: &str = ".state-lock";
const PREPARED: &str = ".state-prepared";
const STARTED: &str = ".state-started";
const TERMINAL: &str = ".state-terminal.json";
const KEEP_TERMINAL: usize = 5;

/// Host-installed baseline; keep the installer in sync. Per-launch copies
/// remain role-writable until OS enforcement is applied at launch.
pub const CARGO_CONFIG_SEED: &[u8] = b"[net]\ngit-fetch-with-cli = false\n";

/// Shared layout contract for launch profiles and later host preflight checks.
pub struct StatePaths {
    pub root: PathBuf,
    pub home: PathBuf,
    pub cargo: PathBuf,
    pub coordinator: PathBuf,
}

impl StatePaths {
    pub fn new(run: &Path) -> Self {
        let root = run.join("state");
        Self {
            home: root.join("home"),
            cargo: root.join("cargo"),
            coordinator: root.join("coordinator"),
            root,
        }
    }

    pub fn directories(&self) -> Vec<PathBuf> {
        vec![
            self.root.clone(),
            self.home.clone(),
            self.cargo.clone(),
            self.coordinator.clone(),
            self.cargo.join("registry"),
            self.cargo.join("git"),
        ]
    }
}

/// Held from before preparation through the observed harness exit. The lock
/// file lives outside `state/`, so pruning never replaces the locked inode.
pub struct RunState {
    run: PathBuf,
    role: Role,
    _lock: File,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Terminal {
    role: String,
    session_id: String,
    finished_unix_nanos: u128,
}

impl RunState {
    pub fn prepare(spec: &LaunchSpec, config: &Config) -> Result<Self> {
        // Public `prepare` runs before launch preflight: reject escaping paths
        // before creating, pruning, or writing anything.
        managed_paths(spec, config, true)?;
        let seed = cargo_seed(config)?;
        // Callers may have already created the run to write its prompt. The
        // state subtree, rather than the caller's existing run, must be 0700.
        fs::create_dir_all(&spec.run)?;
        ensure!(
            fs::symlink_metadata(&spec.run)?.is_dir(),
            "run must not be a symlink"
        );
        let lock = open_lock(&spec.run, true)?;
        lock.try_lock().context("run state is already in use")?;
        for marker in [STARTED, TERMINAL] {
            ensure!(
                fs::symlink_metadata(spec.run.join(marker))
                    .is_err_and(|e| e.kind() == ErrorKind::NotFound),
                "run has already started or terminated; use a fresh run directory"
            );
        }
        let guard = Self {
            run: spec.run.clone(),
            role: spec.role,
            _lock: lock,
        };
        // Reclaim only known terminal state before allocating new caches. Disk
        // errors fail the launch; there is no shared-writable-cache fallback.
        prune_terminal(config, spec.role, &spec.run)?;
        guard.prepare_layout(&seed)?;
        Ok(guard)
    }

    fn prepare_layout(&self, seed: &[u8]) -> Result<()> {
        let state = StatePaths::new(&self.run);
        let prepared = self.run.join(PREPARED);
        if fs::symlink_metadata(&prepared).is_ok() {
            ensure!(
                read_regular(&prepared)? == self.role.slug().as_bytes(),
                "prepared run role differs"
            );
            for dir in state.directories() {
                check_private_dir(&dir)?;
            }
            ensure!(
                read_regular(&state.cargo.join("config.toml"))? == seed,
                "per-launch Cargo config differs from the protected seed"
            );
            return Ok(());
        }
        ensure!(
            fs::symlink_metadata(&state.root).is_err_and(|e| e.kind() == ErrorKind::NotFound),
            "unrecognized or partially prepared state; use a fresh run directory"
        );
        for dir in state.directories() {
            private_dir(&dir)?;
        }
        write_new(&state.cargo.join("config.toml"), seed)?;
        // Credentials are never copied into per-launch state.
        write_new(&prepared, self.role.slug().as_bytes())
    }

    /// Persist launch intent before spawning. A crash after this point never
    /// makes the run reusable or gives retention permission to remove it.
    pub fn started(&self, spec: &LaunchSpec) -> Result<()> {
        write_new(
            &self.run.join(STARTED),
            spec.session_id.to_string().as_bytes(),
        )
    }

    /// Call only after `wait` succeeds, or spawn definitively failed. Do not
    /// call on Drop: the harness could outlive an interrupted supervisor.
    pub fn terminal(&self, spec: &LaunchSpec) -> Result<()> {
        let record = Terminal {
            role: self.role.slug().into(),
            session_id: spec.session_id.to_string(),
            finished_unix_nanos: SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
        };
        write_new(&self.run.join(TERMINAL), &serde_json::to_vec(&record)?)
    }
}

/// Keep five most recently completed runs per role. Unknown, interrupted,
/// malformed, locked, current and symlinked runs are never candidates. Retain
/// prompts and outputs: only the launch's `state/` subtree is removed.
fn prune_terminal(config: &Config, role: Role, current: &Path) -> Result<()> {
    let runs = config.state_dir.join(role.slug()).join("runs");
    let entries = match fs::read_dir(&runs) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("list runs for state retention"),
    };
    let current = fs::canonicalize(current)?;
    let mut candidates = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() || fs::canonicalize(entry.path())? == current {
            continue;
        }
        if let Some(terminal) = terminal_record(&entry.path(), role) {
            candidates.push((terminal.finished_unix_nanos, entry.path()));
        }
    }
    candidates.sort_unstable_by(|left, right| right.cmp(left));
    for (completed, run) in candidates.into_iter().skip(KEEP_TERMINAL) {
        let lock = match open_lock(&run, false) {
            Ok(lock) => lock,
            // No lock file or an unreadable one is not evidence of inactivity.
            Err(_) => continue,
        };
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => continue,
            Err(TryLockError::Error(error)) => return Err(error).context("lock retained run"),
        }
        if !terminal_record(&run, role).is_some_and(|r| r.finished_unix_nanos == completed) {
            continue;
        }
        let state = StatePaths::new(&run).root;
        match fs::symlink_metadata(&state) {
            Ok(metadata) if metadata.is_dir() => {
                fs::remove_dir_all(&state).context("prune terminal run state")?;
            }
            Ok(_) => {} // Never follow a replacement symlink.
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("inspect terminal run state"),
        }
    }
    Ok(())
}

fn terminal_record(run: &Path, role: Role) -> Option<Terminal> {
    let record: Terminal = serde_json::from_slice(&read_regular(&run.join(TERMINAL)).ok()?).ok()?;
    let started = read_regular(&run.join(STARTED)).ok()?;
    (record.role == role.slug() && record.session_id.as_bytes() == started).then_some(record)
}

pub fn regular_file(path: &Path) -> Result<fs::Metadata> {
    path_without_symlinks(path, false)?;
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file(),
        "{} must be a regular file",
        path.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            metadata.nlink() == 1,
            "{} must not be hard-linked",
            path.display()
        );
    }
    Ok(metadata)
}

/// Executables run by the supervisor must not be replaceable through any
/// ancestor. This check happens before invoking even a version/probe command.
#[cfg(unix)]
pub fn protected_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let metadata = regular_file(path)?;
    ensure!(
        metadata.uid() == 0 && metadata.mode() & 0o6022 == 0 && metadata.mode() & 0o111 != 0,
        "{} must be root-owned, executable, non-setuid and not group/world-writable",
        path.display()
    );
    for parent in path.ancestors().skip(1) {
        let metadata = fs::symlink_metadata(parent)?;
        ensure!(
            metadata.is_dir() && metadata.uid() == 0 && metadata.mode() & 0o022 == 0,
            "executable parent {} must be protected by root ownership",
            parent.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn protected_executable(_path: &Path) -> Result<()> {
    anyhow::bail!("protected executables require Unix ownership checks")
}

pub fn read_regular(path: &Path) -> Result<Vec<u8>> {
    regular_file(path)?;
    Ok(fs::read(path)?)
}

fn open_lock(run: &Path, create: bool) -> Result<File> {
    let path = run.join(LOCK);
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            regular_file(&path)?;
        }
        Err(error) if create && error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(create)
        .truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

fn write_new(path: &Path, contents: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(contents)?;
    file.sync_all()?;
    #[cfg(unix)]
    File::open(path.parent().context("state file has no parent")?)?.sync_all()?;
    Ok(())
}

pub fn private_dir(path: &Path) -> Result<()> {
    path_without_symlinks(path, true)?;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    check_private_dir(path)
}

pub fn check_private_dir(path: &Path) -> Result<()> {
    path_without_symlinks(path, false)?;
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir(),
        "{} must be a directory, not a symlink",
        path.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            metadata.permissions().mode() & 0o7777 == 0o700,
            "{} must be mode 0700",
            path.display()
        );
    }
    Ok(())
}

/// Require absolute, canonical spelling and reject every symlink component.
/// A missing suffix is allowed only during preparation, after checking all
/// existing ancestors. This is a precondition check, not same-uid race fencing.
pub fn path_without_symlinks(path: &Path, allow_missing: bool) -> Result<()> {
    ensure!(path.is_absolute(), "{} must be absolute", path.display());
    let mut walked = PathBuf::new();
    for component in path.components() {
        ensure!(
            matches!(component, Component::RootDir | Component::Normal(_)),
            "{} must be canonical without dot components",
            path.display()
        );
        walked.push(component.as_os_str());
        match fs::symlink_metadata(&walked) {
            Ok(metadata) => ensure!(
                !metadata.is_symlink(),
                "{} must not contain a symlink",
                path.display()
            ),
            Err(error) if allow_missing && error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("inspect {}", walked.display()));
            }
        }
    }
    ensure!(
        walked.as_os_str() == path.as_os_str(),
        "{} must use canonical spelling",
        path.display()
    );
    Ok(())
}

pub fn managed_paths(spec: &LaunchSpec, config: &Config, allow_missing: bool) -> Result<()> {
    path_without_symlinks(&config.state_dir, allow_missing)?;
    for (path, directory) in [(&spec.run, "runs"), (&spec.clone, "clones")] {
        path_without_symlinks(path, allow_missing)?;
        let parent = config.state_dir.join(spec.role.slug()).join(directory);
        ensure!(
            path.starts_with(&parent) && path != &parent,
            "{} must be below {}",
            path.display(),
            parent.display()
        );
    }
    Ok(())
}

pub fn cargo_seed(config: &Config) -> Result<Vec<u8>> {
    let seed =
        read_regular(&config.cargo_config_seed).context("read required Cargo config seed")?;
    ensure!(
        seed == CARGO_CONFIG_SEED,
        "Cargo config seed differs from the required baseline"
    );
    Ok(seed)
}

/// Never truncate an existing output: a symlink or hard link must not turn
/// preparation into an arbitrary write. Repeated preparation requires the
/// same generated bytes, otherwise the caller must choose a fresh run.
pub fn generated_file(path: &Path, contents: &[u8]) -> Result<()> {
    path_without_symlinks(path, true)?;
    match fs::symlink_metadata(path) {
        Ok(_) => ensure!(
            read_regular(path)? == contents,
            "{} differs from generated contents",
            path.display()
        ),
        Err(error) if error.kind() == ErrorKind::NotFound => write_new(path, contents)?,
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::prepare_run;
    use crate::profile::{self, Harness};
    use uuid::Uuid;

    fn config(root: &Path) -> Config {
        fs::write(root.join("cargo-config.toml"), CARGO_CONFIG_SEED).unwrap();
        Config {
            state_dir: root.join("roles"),
            cargo_config_seed: root.join("cargo-config.toml"),
            ..Config::default()
        }
    }

    fn spec(config: &Config, role: Role, name: &str) -> LaunchSpec {
        let base = config.state_dir.join(role.slug());
        LaunchSpec {
            role,
            harness: Harness::Claude,
            clone: base.join("clones").join(name),
            run: base.join("runs").join(name),
            model: "test".into(),
            effort: "low".into(),
            session_id: Uuid::new_v4(),
            project: None,
            task: None,
            push_socket: None,
        }
    }

    fn completed(config: &Config, role: Role, name: &str, finished: u128) -> LaunchSpec {
        let spec = spec(config, role, name);
        let state = RunState::prepare(&spec, config).unwrap();
        state.started(&spec).unwrap();
        state.terminal(&spec).unwrap();
        let record = Terminal {
            role: role.slug().into(),
            session_id: spec.session_id.to_string(),
            finished_unix_nanos: finished,
        };
        fs::write(
            spec.run.join(TERMINAL),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        fs::write(spec.run.join("last.md"), "keep this output").unwrap();
        spec
    }

    #[test]
    fn launches_have_private_homes_and_independent_seeded_cargo_caches() {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        let seed = b"[net]\ngit-fetch-with-cli = false\n";
        fs::write(&config.cargo_config_seed, seed).unwrap();
        for role in [Role::Implementer, Role::Reviewer] {
            let first = spec(&config, role, "first");
            let second = spec(&config, role, "second");
            let credentials = config.state_dir.join(role.slug()).join("claude-config");
            fs::create_dir_all(&credentials).unwrap();
            fs::write(credentials.join(".credentials.json"), "test sentinel").unwrap();
            prepare_run(&first, &config).unwrap();
            let first_state = StatePaths::new(&first.run);
            fs::write(first_state.home.join(".profile"), "first launch only").unwrap();
            fs::write(first_state.cargo.join("config.toml"), "changed by launch").unwrap();
            for cache in ["registry", "git"] {
                fs::write(
                    first_state.cargo.join(cache).join("poison"),
                    "first launch only",
                )
                .unwrap();
            }
            prepare_run(&second, &config).unwrap();
            let second_state = StatePaths::new(&second.run);
            let first_env = profile::command(&first, &config).env;
            let second_env = profile::command(&second, &config).env;
            for (name, expected) in [
                ("HOME", &second_state.home),
                ("CARGO_HOME", &second_state.cargo),
                ("AGENT_COORDINATOR_HOME", &second_state.coordinator),
            ] {
                let previous = first_env.iter().find(|(key, _)| key == name).unwrap();
                let current = second_env.iter().find(|(key, _)| key == name).unwrap();
                assert_ne!(previous.1, current.1);
                assert_eq!(current.1, expected.as_os_str());
            }
            for env in [&first_env, &second_env] {
                assert_eq!(
                    env.iter()
                        .find(|(key, _)| key == "CLAUDE_CONFIG_DIR")
                        .unwrap()
                        .1,
                    credentials.as_os_str()
                );
            }
            assert_eq!(
                fs::read(second_state.cargo.join("config.toml")).unwrap(),
                seed
            );
            assert_eq!(fs::read(&config.cargo_config_seed).unwrap(), seed);
            assert!(!second_state.home.join(".profile").exists());
            for cache in ["registry", "git"] {
                let path = second_state.cargo.join(cache);
                assert!(fs::symlink_metadata(&path).unwrap().is_dir());
                assert!(!path.join("poison").exists());
                assert_ne!(
                    fs::canonicalize(path).unwrap(),
                    fs::canonicalize(first_state.cargo.join(cache)).unwrap()
                );
            }
            for paths in [first_state, second_state] {
                for dir in paths.directories() {
                    check_private_dir(&dir).unwrap();
                    assert!(!dir.join(".credentials.json").exists());
                }
            }
            assert_eq!(
                fs::read(credentials.join(".credentials.json")).unwrap(),
                b"test sentinel"
            );
        }
    }

    #[test]
    fn missing_or_invalid_seed_fails_before_state_writes() {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        fs::remove_file(&config.cargo_config_seed).unwrap();
        let launch = spec(&config, Role::Implementer, "missing");
        assert!(prepare_run(&launch, &config).is_err());
        assert!(!launch.run.exists());
        fs::create_dir(&config.cargo_config_seed).unwrap();
        assert!(prepare_run(&launch, &config).is_err());
        fs::remove_dir(&config.cargo_config_seed).unwrap();
        fs::write(
            &config.cargo_config_seed,
            "[net]\ngit-fetch-with-cli=true\n",
        )
        .unwrap();
        assert!(prepare_run(&launch, &config).is_err());
        assert!(!launch.run.exists());
        assert!(!config.state_dir.join("impl/cargo").exists());
    }

    #[test]
    fn preparation_is_idempotent_until_started_and_excludes_concurrent_use() {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        let spec = spec(&config, Role::Implementer, "one");
        prepare_run(&spec, &config).unwrap();
        let guard = RunState::prepare(&spec, &config).unwrap();
        assert!(RunState::prepare(&spec, &config).is_err());
        guard.started(&spec).unwrap();
        assert!(guard.started(&spec).is_err());
        drop(guard); // Simulate supervisor loss; absence of the lock is insufficient.
        assert!(prepare_run(&spec, &config).is_err());
        assert!(StatePaths::new(&spec.run).root.exists());
        assert!(!spec.run.join(TERMINAL).exists());
    }

    #[test]
    fn retention_keeps_five_terminal_runs_and_preserves_active_unknown_and_other_roles() {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        let active = spec(&config, Role::Implementer, "active");
        let active_guard = RunState::prepare(&active, &config).unwrap();
        active_guard.started(&active).unwrap();
        let interrupted = spec(&config, Role::Implementer, "interrupted");
        let interrupted_guard = RunState::prepare(&interrupted, &config).unwrap();
        interrupted_guard.started(&interrupted).unwrap();
        drop(interrupted_guard);
        let unknown = spec(&config, Role::Implementer, "unknown");
        fs::create_dir_all(StatePaths::new(&unknown.run).root).unwrap();
        let reviewer = completed(&config, Role::Reviewer, "other-role", 1);
        let terminal: Vec<_> = (1..=6)
            .map(|i| completed(&config, Role::Implementer, &format!("done-{i}"), i))
            .collect();
        let current = spec(&config, Role::Implementer, "current");
        prepare_run(&current, &config).unwrap();
        assert!(!StatePaths::new(&terminal[0].run).root.exists());
        assert!(terminal[0].run.join("last.md").is_file());
        for kept in
            terminal[1..]
                .iter()
                .chain([&active, &interrupted, &unknown, &reviewer, &current])
        {
            assert!(
                StatePaths::new(&kept.run).root.exists(),
                "{}",
                kept.run.display()
            );
        }
        assert!(prepare_run(&terminal[0], &config).is_err());
    }

    #[test]
    fn retention_excludes_locked_and_current_even_with_terminal_records() {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        let locked = completed(&config, Role::Implementer, "locked", 1);
        let lock = open_lock(&locked.run, false).unwrap();
        lock.try_lock().unwrap();
        let current = completed(&config, Role::Implementer, "current", 2);
        let current_lock = open_lock(&current.run, false).unwrap();
        current_lock.try_lock().unwrap();
        // Simulate completion records already present while the lifecycle lock
        // is still held. Retention must never delete the locked/current state.
        for i in 3..=8 {
            completed(&config, Role::Implementer, &format!("done-{i}"), i);
        }
        drop(current_lock);
        prune_terminal(&config, Role::Implementer, &current.run).unwrap();
        assert!(StatePaths::new(&locked.run).root.exists());
        assert!(StatePaths::new(&current.run).root.exists());
        drop(lock);
        prune_terminal(&config, Role::Implementer, &current.run).unwrap();
        assert!(!StatePaths::new(&locked.run).root.exists());
    }

    #[test]
    fn concurrent_preparations_prune_expired_state_without_touching_active_runs() {
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        let active = spec(&config, Role::Implementer, "active");
        let active_guard = RunState::prepare(&active, &config).unwrap();
        active_guard.started(&active).unwrap();
        let terminal: Vec<_> = (1..=6)
            .map(|i| completed(&config, Role::Implementer, &format!("done-{i}"), i))
            .collect();
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let mut threads = Vec::new();
            for name in ["parallel-a", "parallel-b"] {
                let spec = spec(&config, Role::Implementer, name);
                let config = &config;
                let barrier = &barrier;
                threads.push(scope.spawn(move || {
                    barrier.wait();
                    prepare_run(&spec, config).unwrap();
                    assert!(StatePaths::new(&spec.run).root.is_dir());
                }));
            }
            for thread in threads {
                thread.join().unwrap();
            }
        });
        assert!(StatePaths::new(&active.run).root.is_dir());
        assert!(!StatePaths::new(&terminal[0].run).root.exists());
        for kept in &terminal[1..] {
            assert!(StatePaths::new(&kept.run).root.is_dir());
        }
    }

    #[cfg(unix)]
    #[test]
    fn retention_does_not_follow_symlinks_or_trust_malformed_terminal_records() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        let malformed = completed(&config, Role::Implementer, "malformed", 1);
        fs::write(malformed.run.join(TERMINAL), b"partial record").unwrap();
        let linked = completed(&config, Role::Implementer, "linked", 2);
        let outside = root.path().join("outside");
        fs::rename(StatePaths::new(&linked.run).root, &outside).unwrap();
        fs::write(outside.join("keep"), "untouched").unwrap();
        symlink(&outside, StatePaths::new(&linked.run).root).unwrap();
        symlink(&malformed.run, config.state_dir.join("impl/runs/alias")).unwrap();
        for i in 3..=8 {
            completed(&config, Role::Implementer, &format!("done-{i}"), i);
        }
        let current = spec(&config, Role::Implementer, "current");
        prepare_run(&current, &config).unwrap();
        assert!(StatePaths::new(&malformed.run).root.exists());
        assert!(outside.join("keep").exists());
        assert!(
            fs::symlink_metadata(StatePaths::new(&linked.run).root)
                .unwrap()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[test]
    fn preexisting_state_symlinks_and_nonprivate_directories_are_rejected() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        let spec = spec(&config, Role::Implementer, "one");
        prepare_run(&spec, &config).unwrap();
        let state = StatePaths::new(&spec.run);
        fs::set_permissions(&state.home, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(prepare_run(&spec, &config).is_err());
        fs::set_permissions(&state.home, fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_dir(state.cargo.join("registry")).unwrap();
        symlink(root.path(), state.cargo.join("registry")).unwrap();
        assert!(prepare_run(&spec, &config).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn public_preparation_rejects_paths_outside_managed_roots_and_symlink_ancestors() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        let original = spec(&config, Role::Implementer, "one");
        for path in [
            PathBuf::from("relative/run"),
            root.path().join("outside"),
            config.state_dir.join("rev/runs/wrong-role"),
            config.state_dir.join("impl/runs/../escaped"),
            config.state_dir.join("impl/runs/./dot"),
        ] {
            let launch = LaunchSpec {
                run: path,
                ..original.clone()
            };
            assert!(
                prepare_run(&launch, &config).is_err(),
                "{}",
                launch.run.display()
            );
        }
        let launch = LaunchSpec {
            clone: root.path().join("outside"),
            ..original.clone()
        };
        assert!(prepare_run(&launch, &config).is_err());
        assert!(!config.state_dir.exists());
        let outside = root.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::create_dir_all(config.state_dir.join("impl")).unwrap();
        symlink(&outside, config.state_dir.join("impl/runs")).unwrap();
        assert!(prepare_run(&original, &config).is_err());
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn preparation_never_overwrites_linked_generated_files_or_follows_temp_dirs() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        let sentinel = root.path().join("sentinel");
        fs::write(&sentinel, b"untouched").unwrap();
        for (name, file) in [
            ("settings", "role-settings.json"),
            ("schema", "result.schema.json"),
        ] {
            let launch = spec(&config, Role::Reviewer, name);
            fs::create_dir_all(&launch.run).unwrap();
            symlink(&sentinel, launch.run.join(file)).unwrap();
            assert!(prepare_run(&launch, &config).is_err());
            assert_eq!(fs::read(&sentinel).unwrap(), b"untouched");
        }
        let linked = spec(&config, Role::Implementer, "hardlink");
        fs::create_dir_all(&linked.run).unwrap();
        fs::hard_link(&sentinel, linked.run.join("role-settings.json")).unwrap();
        assert!(prepare_run(&linked, &config).is_err());
        assert_eq!(fs::read(&sentinel).unwrap(), b"untouched");
        let temporary = spec(&config, Role::Implementer, "temp");
        fs::create_dir_all(&temporary.run).unwrap();
        symlink(root.path(), temporary.run.join("tmp")).unwrap();
        assert!(prepare_run(&temporary, &config).is_err());
        assert_eq!(fs::read(&sentinel).unwrap(), b"untouched");
    }

    #[cfg(unix)]
    #[test]
    fn seed_links_and_modified_prepared_cargo_fail_closed() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path());
        let launch = spec(&config, Role::Implementer, "one");
        prepare_run(&launch, &config).unwrap();
        fs::write(
            StatePaths::new(&launch.run).cargo.join("config.toml"),
            b"changed",
        )
        .unwrap();
        assert!(prepare_run(&launch, &config).is_err());
        let target = root.path().join("seed-target");
        fs::rename(&config.cargo_config_seed, &target).unwrap();
        symlink(&target, &config.cargo_config_seed).unwrap();
        let fresh = spec(&config, Role::Implementer, "fresh");
        assert!(prepare_run(&fresh, &config).is_err());
        assert!(!fresh.run.exists());
    }
}
