use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::{JournalEvent, StoredJob};

pub(crate) struct JobLock {
    file: File,
}

impl Drop for JobLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

pub(crate) fn paths(state_file: &Path) -> Result<JobPaths> {
    let directory = state_file
        .parent()
        .context("job state file must have a parent directory")?
        .to_path_buf();
    let stem = state_file
        .file_stem()
        .and_then(|value| value.to_str())
        .context("job state file must have a Unicode file stem")?;
    Ok(JobPaths {
        directory: directory.clone(),
        state: state_file.to_path_buf(),
        lock: directory.join(format!("{stem}.lock")),
        journal: directory.join(format!("{stem}.journal.jsonl")),
        stdout: directory.join("stdout.log"),
        stderr: directory.join("stderr.log"),
    })
}

pub(crate) struct JobPaths {
    pub directory: PathBuf,
    pub state: PathBuf,
    pub lock: PathBuf,
    pub journal: PathBuf,
    pub stdout: PathBuf,
    pub stderr: PathBuf,
}

pub(crate) fn prepare_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("create {}", path.display()))?;
    protect_directory(path)?;
    Ok(())
}

pub(crate) fn lock(paths: &JobPaths, blocking: bool) -> Result<Option<JobLock>> {
    prepare_directory(&paths.directory)?;
    let file = protected_open(&paths.lock, false)?;
    if blocking {
        file.lock().context("lock local job state")?;
        Ok(Some(JobLock { file }))
    } else {
        match file.try_lock() {
            Ok(()) => Ok(Some(JobLock { file })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => Err(error).context("lock local job state"),
        }
    }
}

pub(crate) fn load(paths: &JobPaths) -> Result<StoredJob> {
    let bytes = fs::read(&paths.state)
        .with_context(|| format!("read local job state {}", paths.state.display()))?;
    serde_json::from_slice(&bytes).context("decode local job state")
}

pub(crate) fn save(paths: &JobPaths, state: &StoredJob) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(state).context("encode local job state")?;
    let temporary = paths.directory.join(format!(
        ".state-{}-{}-{}.tmp",
        std::process::id(),
        state.revision,
        uuid::Uuid::new_v4()
    ));
    let mut file = protected_create_new(&temporary)?;
    if let Err(error) = (|| -> io::Result<()> {
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, &paths.state)?;
        sync_directory(&paths.directory)?;
        Ok(())
    })() {
        let _ = fs::remove_file(&temporary);
        return Err(error).context("durably save local job state");
    }
    Ok(())
}

pub(crate) fn append_event(paths: &JobPaths, event: &JournalEvent) -> Result<()> {
    let mut file = protected_open(&paths.journal, false)?;
    let mut existing = Vec::new();
    file.read_to_end(&mut existing)
        .context("inspect local job journal tail")?;
    if !existing.is_empty() && !existing.ends_with(b"\n") {
        let valid_length = existing
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |index| index + 1);
        file.set_len(valid_length as u64)
            .context("repair interrupted local job journal tail")?;
    }
    file.seek(SeekFrom::End(0))
        .context("seek local job journal")?;
    serde_json::to_writer(&mut file, event).context("encode local job journal entry")?;
    file.write_all(b"\n").context("append local job journal")?;
    file.sync_all().context("sync local job journal")?;
    Ok(())
}

pub(crate) fn events(paths: &JobPaths) -> Result<Vec<JournalEvent>> {
    let file = match File::open(&paths.journal) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).context("open local job journal"),
    };
    let mut result = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line.context("read local job journal")?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str(&line) {
            Ok(value) => result.push(value),
            Err(_) => break,
        }
    }
    Ok(result)
}

pub(crate) fn protected_log(path: &Path) -> Result<File> {
    protected_open(path, true)
}

pub(crate) fn reset_log(path: &Path) -> Result<()> {
    protected_open(path, false)?
        .set_len(0)
        .with_context(|| format!("reset protected log {}", path.display()))
}

fn protected_open(path: &Path, append: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).append(append);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .with_context(|| format!("open protected file {}", path.display()))?;
    protect_file(path)?;
    Ok(file)
}

fn protected_create_new(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .with_context(|| format!("create protected file {}", path.display()))
}

#[cfg(unix)]
fn protect_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("protect directory {}", path.display()))
}

#[cfg(unix)]
fn protect_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("protect file {}", path.display()))
}

#[cfg(windows)]
fn protect_directory(_path: &Path) -> Result<()> {
    // The CLI places this directory beneath its user-private state directory.
    // New files inherit that directory's user-only DACL.
    Ok(())
}

#[cfg(windows)]
fn protect_file(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn protect_directory(_path: &Path) -> Result<()> {
    bail!("protected local job storage is unsupported on this platform")
}

#[cfg(not(any(unix, windows)))]
fn protect_file(_path: &Path) -> Result<()> {
    bail!("protected local job storage is unsupported on this platform")
}

#[cfg(unix)]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(any(unix, windows)))]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}
