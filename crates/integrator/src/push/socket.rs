//! The helper's listening socket. It is bound and given its mode inside a
//! fresh private directory, then hard-linked to the requested path, so it is
//! never reachable with any other mode, whatever the umask, and an existing
//! path (a dangling symlink included) is refused rather than followed.
use anyhow::{Context, Result};
use std::fs::{self, DirBuilder, Permissions};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// The socket's mode: the helper's user and group may connect, others not.
pub const SOCKET_MODE: u32 = 0o660;

/// A listener on a socket file this helper bound; the file is removed on
/// drop if it is still the one bound.
pub struct BoundSocket {
    pub listener: UnixListener,
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl Drop for BoundSocket {
    /// Removes the socket unless something else has replaced it.
    fn drop(&mut self) {
        let ours = fs::symlink_metadata(&self.path)
            .is_ok_and(|meta| meta.dev() == self.device && meta.ino() == self.inode);
        if ours {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Binds a new socket at `path` with mode [`SOCKET_MODE`], failing if
/// anything is already there. The staging directory sits beside `path`, so
/// the staged name is a few bytes longer than `path` and must also fit the
/// Unix socket path limit.
pub fn bind(path: &Path) -> Result<BoundSocket> {
    let staging = Staging::create(path)?;
    let listener = UnixListener::bind(&staging.socket).context("bind the staged socket")?;
    fs::set_permissions(&staging.socket, Permissions::from_mode(SOCKET_MODE))
        .context("set the staged socket's mode")?;
    fs::hard_link(&staging.socket, path)
        .with_context(|| format!("publish socket {}", path.display()))?;
    let meta = fs::symlink_metadata(path).context("inspect the published socket")?;
    Ok(BoundSocket {
        listener,
        path: path.to_path_buf(),
        device: meta.dev(),
        inode: meta.ino(),
    })
}

/// A fresh mode-0700 directory beside the socket path, holding the staged
/// socket; both are removed on drop.
struct Staging {
    directory: PathBuf,
    socket: PathBuf,
}

impl Staging {
    /// Creates the directory `.ap-<random>` in `path`'s parent.
    fn create(path: &Path) -> Result<Self> {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let name = format!(".ap-{}", &Uuid::new_v4().simple().to_string()[..8]);
        let directory = parent.join(name);
        DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .context("create the socket staging directory")?;
        let socket = directory.join("s");
        Ok(Self { directory, socket })
    }
}

impl Drop for Staging {
    /// Removes the staged name and the directory; the published link keeps
    /// the socket.
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket);
        let _ = fs::remove_dir(&self.directory);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::FileTypeExt;

    /// The names in `directory`.
    fn names(directory: &Path) -> Vec<String> {
        let entries = fs::read_dir(directory).unwrap();
        entries
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect()
    }

    #[test]
    fn bind_publishes_one_socket_with_the_mode_and_removes_it_on_drop() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("push.sock");
        let socket = bind(&path).unwrap();
        let meta = fs::symlink_metadata(&path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o7777, SOCKET_MODE);
        assert!(meta.file_type().is_socket());
        assert_eq!(names(directory.path()), ["push.sock"]);
        assert!(std::os::unix::net::UnixStream::connect(&path).is_ok());
        assert!(bind(&path).is_err(), "bound over an existing socket");
        drop(socket);
        assert!(names(directory.path()).is_empty(), "socket not removed");
    }

    #[test]
    fn existing_files_and_dangling_symlinks_are_refused_untouched() {
        let directory = tempfile::tempdir().unwrap();
        let occupied = directory.path().join("occupied");
        fs::write(&occupied, "x").unwrap();
        assert!(bind(&occupied).is_err());
        let target = directory.path().join("target");
        let link = directory.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(bind(&link).is_err());
        assert!(!target.exists(), "followed the symlink");
        let mut left = names(directory.path());
        left.sort();
        assert_eq!(left, ["link", "occupied"]);
    }

    #[test]
    fn a_replaced_socket_path_is_left_alone() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("push.sock");
        let socket = bind(&path).unwrap();
        fs::remove_file(&path).unwrap();
        fs::write(&path, "someone else's").unwrap();
        drop(socket);
        assert!(path.exists(), "removed a file the helper did not bind");
    }
}
