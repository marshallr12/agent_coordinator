//! Root's reads and writes below the implementer's directory. The role owns
//! `coordinator/`, `runs/` and everything in them, so every path is opened
//! with `files::open_beneath` from the root-owned `<state_dir>/impl`: a
//! symlink at any component is refused, never followed. Errors name paths,
//! never file contents.
use crate::push_helper::files::open_beneath;
use anyhow::{Context, Result, ensure};
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// The largest credential file read.
pub const MAX_CREDENTIALS: u64 = 64 * 1024;

/// Reads `relative` below `base`: it must be a regular, single-link file
/// owned by `owner` of at most `max` bytes. `O_NONBLOCK` keeps a FIFO from
/// blocking the open; the type check then refuses it.
pub fn read(base: &Path, relative: &Path, owner: u32, max: u64) -> Result<Vec<u8>> {
    let shown = base.join(relative);
    let flags = libc::O_RDONLY | libc::O_NONBLOCK;
    let file = open_beneath(base, relative, flags, 0)
        .with_context(|| format!("open {} without following symlinks", shown.display()))?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.nlink() == 1 && metadata.uid() == owner,
        "{} must be a regular, single-link file owned by the role",
        shown.display()
    );
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("read {}", shown.display()))?;
    ensure!(
        bytes.len() as u64 <= max,
        "{} is too large",
        shown.display()
    );
    Ok(bytes)
}

/// Creates `relative` below `base` (new, mode 0600), writes `contents` and
/// hands the file to `uid`:`gid`.
pub fn write(base: &Path, relative: &Path, contents: &[u8], uid: u32, gid: u32) -> Result<()> {
    let shown = base.join(relative);
    let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL;
    let mut file = open_beneath(base, relative, flags, 0o600)
        .with_context(|| format!("create {} without following symlinks", shown.display()))?;
    file.write_all(contents)?;
    std::os::unix::fs::fchown(&file, Some(uid), Some(gid))?;
    Ok(file.sync_all()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A base directory with `real/file.toml` holding `token`.
    fn base() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(dir.path()).unwrap();
        fs::create_dir(base.join("real")).unwrap();
        fs::write(base.join("real/file.toml"), "token").unwrap();
        (dir, base)
    }

    /// This process's user id, the owner of files the tests create.
    fn me() -> u32 {
        // SAFETY: getuid takes no arguments, cannot fail and touches no memory.
        unsafe { libc::getuid() }
    }

    #[test]
    fn reads_a_plain_file_within_its_bound() {
        let (_dir, base) = base();
        let bytes = read(&base, Path::new("real/file.toml"), me(), 64).unwrap();
        assert_eq!(bytes, b"token");
        assert!(read(&base, Path::new("real/file.toml"), me(), 2).is_err());
        assert!(read(&base, Path::new("real/file.toml"), me() + 1, 64).is_err());
    }

    #[test]
    fn refuses_symlinks_hard_links_and_fifos() {
        let (_dir, base) = base();
        std::os::unix::fs::symlink(base.join("real/file.toml"), base.join("link")).unwrap();
        std::os::unix::fs::symlink(base.join("real"), base.join("dir")).unwrap();
        fs::hard_link(base.join("real/file.toml"), base.join("hard")).unwrap();
        let fifo = std::ffi::CString::new(base.join("fifo").to_str().unwrap()).unwrap();
        // SAFETY: `fifo` is a NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        for name in ["link", "dir/file.toml", "hard", "fifo"] {
            let error = read(&base, Path::new(name), me(), 64).unwrap_err();
            assert!(!format!("{error:#}").contains("token"), "{name}");
        }
    }

    #[test]
    fn writes_refuse_symlinked_parents_and_existing_files() {
        let (_dir, base) = base();
        std::os::unix::fs::symlink(base.join("real"), base.join("dir")).unwrap();
        // SAFETY: getgid takes no arguments, cannot fail and touches no memory.
        let (uid, gid) = (me(), unsafe { libc::getgid() });
        assert!(write(&base, Path::new("dir/new"), b"x", uid, gid).is_err());
        assert!(!base.join("real/new").exists());
        assert!(write(&base, Path::new("real/file.toml"), b"x", uid, gid).is_err());
        write(&base, Path::new("real/new"), b"x", uid, gid).unwrap();
        assert_eq!(fs::read(base.join("real/new")).unwrap(), b"x");
    }
}
