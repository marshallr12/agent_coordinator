//! The per-launch helper directory under `<state_dir>/push`. Only the
//! supervisor's own (root) account writes that directory, so no path root
//! creates or removes there can be renamed or replaced by an agent account.
//!
//! `<state_dir>/push/<launch id>/` (mode 0711) holds `.lock`, held for the
//! whole launch; `sock/` (helper user, implementer group, mode 2750), where
//! the helper publishes `push.sock` and its 0660 socket inherits the
//! implementer group; and `work/` (helper user, mode 0700), the helper's
//! home and the parent of its private repository.
use anyhow::{Context, Result, bail, ensure};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::ErrorKind;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// The published socket's file name.
pub const SOCKET_NAME: &str = "push.sock";

/// The longest published socket path. The helper first binds
/// `<directory>/.ap-XXXXXXXX/s`, up to 14 bytes longer than any
/// `<directory>/<name>`, and a Unix socket path holds at most 107 bytes.
pub const MAX_SOCKET_PATH: usize = 93;

const LOCK: &str = ".lock";
const STAGING_PREFIX: &str = ".new-";
const SWEEP_LOCK: &str = ".sweep";

/// Refuses a socket path the helper could not bind: relative, or longer than
/// [`MAX_SOCKET_PATH`] bytes.
pub fn check_socket_path(path: &Path) -> Result<()> {
    ensure!(path.is_absolute(), "the socket path must be absolute");
    ensure!(
        path.as_os_str().len() <= MAX_SOCKET_PATH,
        "the socket path is longer than {MAX_SOCKET_PATH} bytes, so the helper could not bind it"
    );
    Ok(())
}

/// Who owns what in a launch directory.
pub struct Owners {
    /// The account running `launch-root`; it owns the directory itself.
    pub supervisor: u32,
    /// The helper's user and primary group.
    pub helper: (u32, u32),
    /// The implementer's primary group, the socket directory's group.
    pub implementer_group: u32,
}

/// One launch's helper directory, locked until it is removed.
pub struct LaunchDir {
    path: PathBuf,
    _lock: File,
}

impl LaunchDir {
    /// The socket the helper publishes and the launch connects to.
    pub fn socket(&self) -> PathBuf {
        self.path.join("sock").join(SOCKET_NAME)
    }

    /// The helper's home directory.
    pub fn work_home(&self) -> PathBuf {
        self.path.join("work")
    }

    /// The helper's private bare repository, which it creates.
    pub fn work_dir(&self) -> PathBuf {
        self.work_home().join("repo")
    }

    /// Creates `<root>/<id>` after sweeping stale launch directories. Sweeping
    /// and building happen under the root's `.sweep` lock; the tree is built
    /// under a `.new-` name with its own lock already held, then renamed into
    /// place, so a sweep never sees an unlocked live directory, and any `.new-`
    /// directory a sweep finds belongs to no running build.
    pub fn create(root: &Path, id: &str, owners: &Owners) -> Result<Self> {
        let path = root.join(id);
        check_socket_path(&path.join("sock").join(SOCKET_NAME))?;
        prepare_root(root, owners.supervisor)?;
        let _sweeping = sweep_lock(root)?;
        sweep_stale(root, owners.supervisor)?;
        ensure!(
            absent(&path)?,
            "launch directory {} already exists",
            path.display()
        );
        let staging = root.join(format!("{STAGING_PREFIX}{id}"));
        let lock = build(&staging, owners).inspect_err(|_| {
            let _ = fs::remove_dir_all(&staging);
        })?;
        fs::rename(&staging, &path).context("move the launch directory into place")?;
        Ok(Self { path, _lock: lock })
    }

    /// Removes the whole directory; the helper must have exited.
    pub fn remove(self) -> Result<()> {
        fs::remove_dir_all(&self.path).with_context(|| format!("remove {}", self.path.display()))
    }
}

/// Builds the directory tree at `staging` and returns its held lock.
fn build(staging: &Path, owners: &Owners) -> Result<File> {
    new_dir(staging, 0o711)?;
    let lock = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(staging.join(LOCK))
        .context("create the launch lock")?;
    lock.lock().context("lock the launch directory")?;
    let (helper_uid, helper_gid) = owners.helper;
    owned_dir(
        &staging.join("sock"),
        helper_uid,
        owners.implementer_group,
        0o2750,
    )?;
    owned_dir(&staging.join("work"), helper_uid, helper_gid, 0o700)?;
    Ok(lock)
}

/// Creates `path` with exactly `mode`, whatever the umask.
fn new_dir(path: &Path, mode: u32) -> Result<()> {
    fs::DirBuilder::new()
        .mode(0o700)
        .create(path)
        .with_context(|| format!("create {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .with_context(|| format!("set the mode of {}", path.display()))
}

/// Creates `path` owned by `uid`:`gid`. The owner changes before the mode,
/// so the setgid bit is set on the final owner.
fn owned_dir(path: &Path, uid: u32, gid: u32, mode: u32) -> Result<()> {
    new_dir(path, 0o700)?;
    std::os::unix::fs::chown(path, Some(uid), Some(gid))
        .with_context(|| format!("change the owner of {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .with_context(|| format!("set the mode of {}", path.display()))
}

/// Creates the push root (mode 0711) unless it exists, also when another
/// `launch-root` creates it at the same moment; either way it must be a real
/// directory owned by `owner` and closed to group and world writes.
pub(super) fn prepare_root(root: &Path, owner: u32) -> Result<()> {
    crate::confine::path_without_symlinks(root, true)?;
    match fs::DirBuilder::new().mode(0o700).create(root) {
        // Another `launch-root` may create it first; the checks below apply.
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error).with_context(|| format!("create {}", root.display())),
        Ok(()) => fs::set_permissions(root, fs::Permissions::from_mode(0o711))
            .with_context(|| format!("set the mode of {}", root.display()))?,
    }
    let metadata = fs::symlink_metadata(root)?;
    ensure!(
        metadata.is_dir() && metadata.uid() == owner && metadata.mode() & 0o022 == 0,
        "{} must be a directory owned by the supervisor and not group/world-writable",
        root.display()
    );
    Ok(())
}

/// True when nothing, not even a dangling symlink, is at `path`.
fn absent(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
}

/// Takes the push root's `.sweep` lock, waiting for any other `launch-root`
/// that is sweeping or building.
fn sweep_lock(root: &Path) -> Result<File> {
    let lock = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(root.join(SWEEP_LOCK))
        .context("open the sweep lock")?;
    lock.lock().context("take the sweep lock")?;
    Ok(lock)
}

/// Removes the launch directories a killed `launch-root` left behind, holding
/// the `.sweep` lock: real directories owned by `owner` that are either
/// abandoned `.new-` builds or launch directories whose lock nobody holds.
/// Symlinks, files, foreign-owned entries, other `.`-names and directories
/// without a lock are left untouched; an entry that vanishes meanwhile is
/// skipped.
pub fn sweep_stale(root: &Path, owner: u32) -> Result<()> {
    for entry in fs::read_dir(root).context("list launch directories")? {
        let path = entry?.path();
        if is_stale(&path, owner)? {
            match fs::remove_dir_all(&path) {
                Err(error) if error.kind() != ErrorKind::NotFound => {
                    return Err(error).with_context(|| format!("remove stale {}", path.display()));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// Whether `path` is an abandoned build or a launch directory no live
/// `launch-root` holds.
pub(super) fn is_stale(path: &Path, owner: u32) -> Result<bool> {
    let name = path.file_name().map(|name| name.as_encoded_bytes());
    let building = name.is_some_and(|name| name.starts_with(STAGING_PREFIX.as_bytes()));
    if !building && name.is_none_or(|name| name.starts_with(b".")) {
        return Ok(false);
    }
    let metadata = match fs::symlink_metadata(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        other => other?,
    };
    if !metadata.is_dir() || metadata.uid() != owner {
        return Ok(false);
    }
    Ok(building || unlocked(path)?)
}

/// Whether the launch directory's lock exists and nobody holds it.
fn unlocked(path: &Path) -> Result<bool> {
    let lock = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path.join(LOCK));
    let Ok(lock) = lock else {
        return Ok(false);
    };
    match lock.try_lock() {
        Ok(()) => Ok(true),
        Err(TryLockError::WouldBlock) => Ok(false),
        Err(TryLockError::Error(error)) => bail!("lock {}: {error}", path.display()),
    }
}
