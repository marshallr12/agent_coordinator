//! Files `launch-root` opens, as root, below directories an agent account
//! can write: the helper's log in `$RUN` and the implementer's coordinator
//! credentials. Each path is walked one component at a time without
//! following a symlink, so renaming an entry cannot redirect root elsewhere.
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::ffi::CString;
use std::fs::File;
use std::io::{self, ErrorKind, Read};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};

/// The largest credential file read.
const MAX_CREDENTIALS: u64 = 64 * 1024;

/// Opens `relative` below `base` with `flags`, refusing a symlink at every
/// component. `base` itself must be canonical and free of symlinks.
pub fn open_beneath(
    base: &Path,
    relative: &Path,
    flags: libc::c_int,
    mode: u32,
) -> io::Result<File> {
    crate::confine::path_without_symlinks(base, false).map_err(io::Error::other)?;
    let mut directory = open_at(None, base, libc::O_PATH | libc::O_DIRECTORY, 0)?;
    let names: Vec<_> = relative.components().collect();
    let Some((last, parents)) = names.split_last() else {
        return Err(io::Error::from(ErrorKind::InvalidInput));
    };
    for name in parents {
        directory = open_at(
            Some(&directory),
            name_of(*name)?,
            libc::O_PATH | libc::O_DIRECTORY,
            0,
        )?;
    }
    open_at(Some(&directory), name_of(*last)?, flags, mode)
}

/// One plain path component, refusing `..`, `.` and roots.
fn name_of(component: Component<'_>) -> io::Result<&Path> {
    match component {
        Component::Normal(name) => Ok(Path::new(name)),
        _ => Err(io::Error::from(ErrorKind::InvalidInput)),
    }
}

/// `openat(2)` relative to `directory` (or the working directory) with
/// `O_NOFOLLOW | O_CLOEXEC` added to `flags`.
fn open_at(
    directory: Option<&File>,
    name: &Path,
    flags: libc::c_int,
    mode: u32,
) -> io::Result<File> {
    let name = CString::new(name.as_os_str().as_bytes())?;
    let at = directory.map_or(libc::AT_FDCWD, AsRawFd::as_raw_fd);
    let flags = flags | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: `name` is NUL-terminated and `at` is a live descriptor or
    // AT_FDCWD; on success the returned descriptor is owned by nobody else.
    let descriptor = unsafe { libc::openat(at, name.as_ptr(), flags, mode as libc::c_uint) };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned a fresh descriptor this File now owns.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

/// Creates the helper's log `relative` below `base`: new, mode 0600, owned
/// by this (root) account, so the agent account can neither read it nor
/// point it at another file.
pub fn helper_log(base: &Path, relative: &Path) -> Result<File> {
    let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL;
    open_beneath(base, relative, flags, 0o600).with_context(|| {
        format!(
            "create the push helper log {}",
            base.join(relative).display()
        )
    })
}

/// The SHA-256 digests (lowercase hex) of every token in the credential
/// files `relative` below `base` that exist, deduplicated and sorted. A file
/// must be a regular file owned by `owner`. Token bytes and parse errors,
/// which may quote them, are never reported.
pub fn credential_digests(base: &Path, files: &[&Path], owner: u32) -> Result<Vec<String>> {
    let mut digests = Vec::new();
    for relative in files {
        let Some(text) = read_credentials(base, relative, owner)? else {
            continue;
        };
        digests.extend(tokens(&text, relative)?.iter().map(digest));
    }
    digests.sort();
    digests.dedup();
    Ok(digests)
}

/// The credential file's text, or `None` when it does not exist.
fn read_credentials(base: &Path, relative: &Path, owner: u32) -> Result<Option<String>> {
    let shown = base.join(relative);
    let file = match open_beneath(base, relative, libc::O_RDONLY | libc::O_NONBLOCK, 0) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("open {}", shown.display())),
    };
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.uid() == owner,
        "{} must be a regular file owned by the implementer",
        shown.display()
    );
    let mut text = String::new();
    file.take(MAX_CREDENTIALS + 1)
        .read_to_string(&mut text)
        .with_context(|| format!("read {}", shown.display()))?;
    ensure!(
        text.len() as u64 <= MAX_CREDENTIALS,
        "{} is too large",
        shown.display()
    );
    Ok(Some(text))
}

/// The `credentials.toml` shape `agent-coordinator` reads.
#[derive(Deserialize)]
struct Store {
    #[serde(default)]
    credentials: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    token: String,
}

/// The non-empty tokens in a credential file's text.
fn tokens(text: &str, shown: &Path) -> Result<Vec<String>> {
    let Ok(store) = toml::from_str::<Store>(text) else {
        bail!("{} is not a valid credential file", shown.display());
    };
    let tokens = store.credentials.into_iter().map(|entry| entry.token);
    Ok(tokens.filter(|token| !token.is_empty()).collect())
}

/// Lowercase hex SHA-256 of `token`, as `agent-coordinator` computes it.
fn digest(token: &String) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}
