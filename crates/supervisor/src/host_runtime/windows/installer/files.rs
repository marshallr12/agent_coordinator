//! The installer's files: the binary, the configuration, the manifest and
//! the push App's key. Each is written beside its place and renamed into
//! it, so a file is never seen half written. A link is refused, never
//! followed. Nothing here prints, logs or returns a key's bytes; an error
//! names a path and what Windows said about it.
use super::super::sys::{Descriptor, last_error};
use super::super::text::wide;
use super::access::is_link;
use crate::windows_installer::observed::KeySource;
use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::io::FromRawHandle;
use std::path::Path;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{GENERIC_WRITE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL};

/// The most a text file of the installer's may hold.
const TEXT_LIMIT: u64 = 1024 * 1024;
/// The most a key file may hold; a PEM RSA key is a few thousand bytes.
const KEY_LIMIT: u64 = 64 * 1024;

/// Opens the plain file at `path`; `None` when nothing is there. A link,
/// a directory and a file over `limit` bytes are refused.
fn plain(path: &str, limit: u64) -> Result<Option<File>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("inspect {path}")),
    };
    ensure!(!is_link(&metadata), "{path} is a link or reparse point");
    ensure!(metadata.is_file(), "{path} is not a file");
    ensure!(
        metadata.len() <= limit,
        "{path} is larger than {limit} bytes"
    );
    File::open(path)
        .map(Some)
        .with_context(|| format!("open {path}"))
}

/// The text of the file at `path`, when there is one.
pub fn read_text(path: &str) -> Result<Option<String>> {
    let Some(mut file) = plain(path, TEXT_LIMIT)? else {
        return Ok(None);
    };
    let mut text = String::new();
    file.read_to_string(&mut text)
        .with_context(|| format!("read {path}"))?;
    Ok(Some(text))
}

/// The SHA-256, in hex, of the file at `path`, when there is one.
pub fn sha256(path: &str) -> Result<Option<String>> {
    let Some(mut file) = plain(path, u64::MAX)? else {
        return Ok(None);
    };
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).with_context(|| format!("read {path}"))?;
    Ok(Some(hex::encode(hasher.finalize())))
}

/// The path a file is written at before it is renamed to `path`.
fn staged(path: &str) -> String {
    format!("{path}.new")
}

/// Removes what an interrupted run left at `path`, if anything.
fn discard(path: &str) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(error).with_context(|| format!("remove {path}"))
        }
        _ => Ok(()),
    }
}

/// Flushes the file at `path` to disk, so a rename that follows never puts
/// a file in place whose bytes a power loss could take back.
fn sync(path: &str) -> Result<()> {
    let file = std::fs::OpenOptions::new().write(true).open(path);
    let file = file.with_context(|| format!("open {path} to flush it"))?;
    file.sync_all().with_context(|| format!("flush {path}"))
}

/// Puts the synced file `staged` in place at `path`.
fn put_in_place(staged: &str, path: &str) -> Result<()> {
    sync(staged)?;
    std::fs::rename(staged, path).with_context(|| format!("put {path} in place"))
}

/// Writes `text` as the file at `path`, synced and whole or not at all.
pub fn write_text(path: &str, text: &str) -> Result<()> {
    let staged = staged(path);
    std::fs::write(&staged, text).with_context(|| format!("write {staged}"))?;
    put_in_place(&staged, path)
}

/// Copies the binary at `source` to `path`, synced and whole or not at all.
pub fn copy_binary(source: &Path, path: &str) -> Result<()> {
    let staged = staged(path);
    let copied = std::fs::copy(source, &staged);
    copied.with_context(|| format!("copy {} to {staged}", source.display()))?;
    put_in_place(&staged, path)
}

/// Copies the plain file `from` to `to`, synced and whole or not at all
/// (phase 7, step 7b). A link at either end is refused.
pub fn copy_file(from: &str, to: &str) -> Result<()> {
    plain(from, u64::MAX)?.with_context(|| format!("{from} does not exist"))?;
    plain(to, u64::MAX)?;
    copy_binary(Path::new(from), to)
}

/// Makes a plain directory at `path`, which inherits its parent's access
/// list. One already there is kept; a link or a file there is refused.
pub fn make_directory(path: &str) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure!(!is_link(&metadata), "{path} is a link or reparse point");
            ensure!(metadata.is_dir(), "{path} is not a directory");
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(path).with_context(|| format!("make {path}"))
        }
        Err(error) => Err(error).with_context(|| format!("inspect {path}")),
    }
}

/// The names of what is directly in the directory at `path`; none when
/// nothing is there. A link is refused rather than listed through.
pub fn entries(path: &str) -> Result<Vec<String>> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("inspect {path}")),
        Ok(metadata) => ensure!(!is_link(&metadata), "{path} is a link or reparse point"),
    }
    let mut names = Vec::new();
    for entry in std::fs::read_dir(path).with_context(|| format!("list {path}"))? {
        let entry = entry.with_context(|| format!("list {path}"))?;
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    Ok(names)
}

/// How many bytes the plain file at `path` holds; 0 when there is none.
pub fn length(path: &str) -> Result<u64> {
    let Some(file) = plain(path, u64::MAX)? else {
        return Ok(0);
    };
    Ok(file
        .metadata()
        .with_context(|| format!("inspect {path}"))?
        .len())
}

/// The text of the plain file at `path` after its first `offset` bytes;
/// `None` when there is no such file. A service's log is read while the
/// service writes it, so the read shares the file.
pub fn read_after(path: &str, offset: u64) -> Result<Option<String>> {
    use std::io::{Seek, SeekFrom};
    let Some(mut file) = plain(path, u64::MAX)? else {
        return Ok(None);
    };
    file.seek(SeekFrom::Start(offset))
        .with_context(|| format!("read {path}"))?;
    let mut bytes = Vec::new();
    file.take(TEXT_LIMIT)
        .read_to_end(&mut bytes)
        .with_context(|| format!("read {path}"))?;
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

/// Removes the tree at `path` and returns each path that could not be
/// removed, with why. A link or junction anywhere in it is removed itself,
/// never what it points to. A read-only file loses that attribute first;
/// no ownership is ever taken, so a file only its owner may delete stays
/// and is reported.
pub fn remove_tree(path: &str) -> Result<Vec<String>> {
    let mut left = Vec::new();
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => left.push(format!("{path}: {error}")),
        Ok(metadata) => remove_entry(Path::new(path), &metadata, &mut left),
    }
    Ok(left)
}

/// Removes one entry of a tree, below it first when it is a directory.
fn remove_entry(path: &Path, metadata: &std::fs::Metadata, left: &mut Vec<String>) {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY;
    let directory = metadata.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0;
    if directory && !is_link(metadata) {
        remove_children(path, left);
    } else if metadata.permissions().readonly() {
        let mut writable = metadata.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        writable.set_readonly(false);
        let _ = std::fs::set_permissions(path, writable);
    }
    // A directory link is removed as a directory, which removes the link.
    let removed = match directory {
        true => std::fs::remove_dir(path),
        false => std::fs::remove_file(path),
    };
    if let Err(error) = removed {
        left.push(format!("{}: {error}", path.display()));
    }
}

/// Removes everything in the plain directory at `path`.
fn remove_children(path: &Path, left: &mut Vec<String>) {
    let listed = match std::fs::read_dir(path) {
        Ok(listed) => listed,
        Err(error) => return left.push(format!("{}: {error}", path.display())),
    };
    for entry in listed {
        let child = entry
            .map(|e| e.path())
            .and_then(|p| Ok((p.symlink_metadata()?, p)));
        match child {
            Ok((metadata, child)) => remove_entry(&child, &metadata, left),
            Err(error) => left.push(format!("{}: {error}", path.display())),
        }
    }
}

/// The bytes of the key file at `path`: a plain, non-empty file of at most
/// [`KEY_LIMIT`] bytes.
fn key_bytes(path: &str) -> Result<Vec<u8>> {
    let mut file = plain(path, KEY_LIMIT)?.with_context(|| format!("{path} does not exist"))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .with_context(|| format!("read {path}"))?;
    ensure!(!bytes.is_empty(), "{path} is empty");
    Ok(bytes)
}

/// Whether the key file `source` can be used, and whether the key at
/// `installed` already holds the same bytes.
pub fn key_source(source: &str, installed: Option<&str>) -> KeySource {
    let bytes = match key_bytes(source) {
        Ok(bytes) => bytes,
        Err(error) => return KeySource::Unreadable(format!("{error:#}")),
    };
    let same = |path: &str| key_bytes(path).is_ok_and(|have| have == bytes);
    KeySource::Readable {
        installed: installed.is_some_and(same),
    }
}

/// Creates the file at `path`, which must not exist, with exactly the
/// access list `sddl` describes: it never exists with what its directory
/// would have handed down.
fn create_protected(path: &str, sddl: &str) -> Result<File> {
    let (name, descriptor) = (wide(path)?, Descriptor::parse(sddl)?);
    let attributes = descriptor.attributes(false);
    // SAFETY: the name is NUL-terminated; `attributes` and the descriptor
    // it borrows outlive the call; no sharing and no template file.
    #[rustfmt::skip]
    let raw = unsafe {
        CreateFileW(name.as_ptr(), GENERIC_WRITE, 0, &attributes, CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL, null_mut())
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(last_error(&format!("creating {path}")));
    }
    // SAFETY: `raw` is a live file handle that nothing else owns now.
    Ok(unsafe { File::from_raw_handle(raw) })
}

/// Copies the key file `source` to `path` under the access list `sddl`.
/// The copy is made beside its place with that list from its first byte
/// and then renamed, so the key is never readable by anyone else and a
/// replaced key is replaced whole. The source is only read.
pub fn install_key(source: &str, path: &str, sddl: &str) -> Result<()> {
    let bytes = key_bytes(source)?;
    let staged = staged(path);
    discard(&staged)?;
    let mut file = create_protected(&staged, sddl)?;
    let written = file.write_all(&bytes).and_then(|()| file.sync_all());
    drop(file);
    if written.is_err() {
        discard(&staged)?;
        bail!("writing {staged} failed");
    }
    std::fs::rename(&staged, path).with_context(|| format!("put {path} in place"))
}

#[cfg(test)]
mod tests {
    use super::super::access;
    use super::*;
    use crate::windows_installer::access::{ADMINISTRATORS, AccessList, FULL, READ, SYSTEM};
    use crate::windows_installer::observed::PathState;

    /// A service SID nothing on this machine has.
    const SERVICE: &str = "S-1-5-80-1-2-3-4-5";
    /// A synthetic key: no real key has this text. The installer never
    /// parses a key, so it need not look like one.
    const KEY: &str = "synthetic key file of step 7a\nc3ludGhldGljLWtleS03YQ==\nend of key\n";

    /// `name` inside `dir`, as text.
    fn inside(dir: &tempfile::TempDir, name: &str) -> String {
        dir.path().join(name).display().to_string()
    }

    /// The key's list, with this account added so the test can read back.
    fn key_access() -> AccessList {
        let me = super::super::super::token::current_user_sid().unwrap();
        AccessList::file(&[
            (SYSTEM, FULL),
            (ADMINISTRATORS, FULL),
            (SERVICE, READ),
            (&me, FULL),
        ])
    }

    #[test]
    fn the_key_is_copied_with_its_list_and_the_source_is_left() {
        let dir = tempfile::tempdir().unwrap();
        let (source, key) = (inside(&dir, "source.pem"), inside(&dir, "push-app.pem"));
        std::fs::write(&source, KEY).unwrap();
        assert_eq!(
            key_source(&source, None),
            KeySource::Readable { installed: false }
        );
        install_key(&source, &key, &key_access().sddl().unwrap()).unwrap();
        assert_eq!(std::fs::read_to_string(&key).unwrap(), KEY);
        assert_eq!(std::fs::read_to_string(&source).unwrap(), KEY);
        let PathState::File { access: seen, .. } = access::observe(&key).unwrap() else {
            panic!("not a file");
        };
        assert_eq!(seen, key_access());
        assert_eq!(
            key_source(&source, Some(&key)),
            KeySource::Readable { installed: true }
        );
        assert!(!Path::new(&staged(&key)).exists());
    }

    #[test]
    fn another_key_replaces_the_installed_one_and_keeps_the_list() {
        let dir = tempfile::tempdir().unwrap();
        let (source, key) = (inside(&dir, "source.pem"), inside(&dir, "push-app.pem"));
        std::fs::write(&source, KEY).unwrap();
        install_key(&source, &key, &key_access().sddl().unwrap()).unwrap();
        std::fs::write(&source, KEY.replace("c3ludG", "b3RoZX")).unwrap();
        assert_eq!(
            key_source(&source, Some(&key)),
            KeySource::Readable { installed: false }
        );
        std::fs::write(staged(&key), "left by an interrupted run").unwrap();
        install_key(&source, &key, &key_access().sddl().unwrap()).unwrap();
        assert!(std::fs::read_to_string(&key).unwrap().contains("b3RoZX"));
        let PathState::File { access: seen, .. } = access::observe(&key).unwrap() else {
            panic!("not a file");
        };
        assert_eq!(seen, key_access());
    }

    #[test]
    fn a_key_file_that_cannot_be_used_says_why_without_its_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let missing = key_source(&inside(&dir, "none.pem"), None);
        assert!(
            matches!(&missing, KeySource::Unreadable(why) if why.ends_with("does not exist")),
            "{missing:?}"
        );
        let empty = inside(&dir, "empty.pem");
        std::fs::write(&empty, "").unwrap();
        assert!(
            matches!(key_source(&empty, None), KeySource::Unreadable(why) if why.ends_with("is empty"))
        );
        let large = inside(&dir, "large.pem");
        std::fs::write(&large, KEY.repeat(1000)).unwrap();
        let refused = key_source(&large, None);
        assert!(
            matches!(&refused, KeySource::Unreadable(why) if why.contains("larger than") && !why.contains("c3ludG")),
            "{refused:?}"
        );
        let directory = key_source(&dir.path().display().to_string(), None);
        assert!(
            matches!(&directory, KeySource::Unreadable(why) if why.ends_with("is not a file")),
            "{directory:?}"
        );
    }

    #[test]
    fn a_key_file_that_is_a_link_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (real, link) = (inside(&dir, "real.pem"), inside(&dir, "link.pem"));
        std::fs::write(&real, KEY).unwrap();
        if std::os::windows::fs::symlink_file(&real, &link).is_err() {
            eprintln!("skipped: this session may not create a symbolic link");
            return;
        }
        let refused = key_source(&link, None);
        assert!(
            matches!(&refused, KeySource::Unreadable(why) if why.contains("is a link")),
            "{refused:?}"
        );
    }

    #[test]
    fn text_is_written_whole_and_a_digest_is_of_the_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = inside(&dir, "supervisor.toml");
        assert_eq!(read_text(&path).unwrap(), None);
        assert_eq!(sha256(&path).unwrap(), None);
        write_text(&path, "abc").unwrap();
        assert_eq!(read_text(&path).unwrap().as_deref(), Some("abc"));
        let digest = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(sha256(&path).unwrap().as_deref(), Some(digest));
        let copy = inside(&dir, "copy.bin");
        copy_binary(Path::new(&path), &copy).unwrap();
        assert_eq!(sha256(&copy).unwrap().as_deref(), Some(digest));
    }
}
