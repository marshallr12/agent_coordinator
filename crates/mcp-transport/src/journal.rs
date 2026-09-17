//! One protected, exclusively locked journal per provisioned MCP identity.
use crate::private::{protect_directory, protect_file};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize)]
struct State {
    version: u32,
    binding: String,
    pending: Option<String>,
    #[serde(default)]
    last_completed: Option<String>,
    requests: BTreeMap<String, Value>,
}

pub struct Journal {
    directory: PathBuf,
    _lock: File,
    state: State,
}

pub fn binding(parts: &[&str]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update(part.len().to_le_bytes());
        hash.update(part.as_bytes());
    }
    hex::encode(hash.finalize())
}

fn ordinary(path: &Path, directory: bool) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        !meta.is_symlink() && meta.is_dir() == directory && (directory || meta.is_file()),
        "journal path is not an ordinary file or directory"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // A second hard link could expose or substitute journal state.
        ensure!(
            directory || meta.nlink() == 1,
            "journal file has multiple links"
        );
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(
            meta.file_attributes() & 0x400 == 0,
            "journal path is a reparse point"
        );
    }
    Ok(())
}

impl Journal {
    pub fn open(directory: &Path, identity: String) -> Result<Self> {
        ensure!(
            directory.is_absolute() && directory.parent().is_some(),
            "journal directory must be an absolute non-root path"
        );
        ensure!(
            !directory
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir)),
            "journal path must be normalized"
        );
        // Do not follow links in any ancestor. Require the parent to exist rather
        // than creating an uncontrolled chain with inherited permissions.
        let parent = directory.parent().context("missing journal parent")?;
        for ancestor in parent.ancestors() {
            ordinary(ancestor, true)?;
        }
        if !directory.exists() {
            match fs::create_dir(directory) {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(e) => return Err(e.into()),
            }
        }
        ordinary(directory, true)?;
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_str().context("invalid journal filename")?;
            ensure!(
                name == "journal.json" || name == "journal.lock" || name.starts_with(".journal-"),
                "journal directory contains unrelated data"
            );
            ordinary(&entry.path(), false)?;
        }
        protect_directory(directory)?;
        let lock_path = directory.join("journal.lock");
        if lock_path.exists() {
            ordinary(&lock_path, false)?;
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(&lock_path)?;
        protect_file(&lock_path)?;
        lock.try_lock()
            .context("another adapter owns this journal")?;
        let path = directory.join("journal.json");
        let state = if path.exists() {
            ordinary(&path, false)?;
            protect_file(&path)?;
            ensure!(
                fs::metadata(&path)?.len() <= 64 * 1024 * 1024,
                "journal is too large; retain it and provision a new session after resolving pending work"
            );
            let state: State = serde_json::from_slice(&fs::read(path)?)?;
            ensure!(
                state.version == 1 && state.binding == identity,
                "journal belongs to another endpoint or credential/session identity"
            );
            ensure!(
                state
                    .pending
                    .as_ref()
                    .is_none_or(|key| state.requests.contains_key(key)),
                "invalid pending journal reference"
            );
            state
        } else {
            State {
                version: 1,
                binding: identity,
                pending: None,
                last_completed: None,
                requests: BTreeMap::new(),
            }
        };
        let journal = Self {
            directory: directory.into(),
            _lock: lock,
            state,
        };
        // Verify durable storage before advertising capability, even for a new
        // empty session. Any failure prevents startup and mutation dispatch.
        journal.save()?;
        Ok(journal)
    }

    fn save(&self) -> Result<()> {
        let mut temp = tempfile::Builder::new()
            .prefix(".journal-")
            .tempfile_in(&self.directory)?;
        protect_file(temp.path())?;
        let bytes = serde_json::to_vec(&self.state)?;
        ensure!(
            bytes.len() <= 64 * 1024 * 1024,
            "journal capacity reached; retain state and resolve this session"
        );
        temp.write_all(&bytes)?;
        temp.as_file().sync_all()?;
        temp.persist(self.directory.join("journal.json"))
            .map_err(|e| e.error)?;
        #[cfg(unix)]
        File::open(&self.directory)?.sync_all()?;
        Ok(())
    }

    pub fn status(&self) -> Value {
        json!({"durable_mutation_journal":true,"version":1,"pending":self.state.pending.is_some(),"last_request_replayable":self.retry_request().is_some(),"pending_tool":self.pending().and_then(|p|p["name"].as_str().map(str::to_owned)),"authority_renewed":false})
    }

    pub fn pending(&self) -> Option<Value> {
        self.state
            .pending
            .as_ref()
            .and_then(|key| self.state.requests.get(key))
            .cloned()
    }

    pub fn prepare(&mut self, params: &mut Value) -> Result<()> {
        let arguments = params
            .get_mut("arguments")
            .and_then(Value::as_object_mut)
            .context("tool arguments must be an object")?;
        let key = arguments
            .get("idempotency_key")
            .and_then(Value::as_str)
            .context("invalid mutation key")?
            .to_owned();
        ensure!(
            (16..=128).contains(&key.len())
                && key
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b)),
            "invalid mutation key"
        );
        if let Some(pending) = &self.state.pending {
            ensure!(
                pending == &key,
                "pending mutation must be reconciled using coordinator_transport_retry before another write"
            );
        }
        let previous = self.state.clone();
        if let Some(saved) = self.state.requests.get(&key) {
            ensure!(
                saved == params,
                "mutation key cannot be reused with different arguments"
            );
        } else {
            self.state.requests.insert(key.clone(), params.clone());
        }
        self.state.pending = Some(key);
        if let Err(error) = self.save() {
            self.state = previous;
            return Err(error);
        }
        Ok(())
    }

    pub fn retry_request(&self) -> Option<Value> {
        self.pending().or_else(|| {
            self.state
                .last_completed
                .as_ref()
                .and_then(|key| self.state.requests.get(key))
                .cloned()
        })
    }

    pub fn complete(&mut self) -> Result<()> {
        let previous = self.state.clone();
        let pending = self.state.pending.take();
        if pending.is_none() {
            bail!("no pending mutation");
        }
        self.state.last_completed = pending;
        if let Err(error) = self.save() {
            self.state = previous;
            return Err(error);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restart_reuses_exact_arguments_and_excludes_other_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state");
        let mut j = Journal::open(&path, "identity".into()).unwrap();
        assert!(Journal::open(&path, "identity".into()).is_err());
        let mut call = json!({"name":"claim","arguments":{"idempotency_key":"test-key-123456789","body":{"task":"a"}}});
        j.prepare(&mut call).unwrap();
        drop(j);
        let mut j = Journal::open(&path, "identity".into()).unwrap();
        assert_eq!(j.pending(), Some(call.clone()));
        let mut resend = json!({"name":"claim","arguments":{"idempotency_key":"test-key-123456789","body":{"task":"a"}}});
        j.prepare(&mut resend).unwrap();
        assert_eq!(resend, call);
        let mut other = json!({"name":"claim","arguments":{"idempotency_key":"other-key-12345678","body":{"task":"b"}}});
        assert!(j.prepare(&mut other).is_err());
        j.complete().unwrap();
        call["arguments"]["body"]["task"] = json!("changed");
        assert!(j.prepare(&mut call).is_err());
        drop(j);
        assert!(Journal::open(&path, "another identity".into()).is_err());
    }
    #[test]
    fn failed_persistence_cannot_leave_an_unsaved_retry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state");
        let mut j = Journal::open(&path, "a".into()).unwrap();
        assert!(
            j.prepare(&mut json!({"name":"claim","arguments":{}}))
                .is_err()
        );
        fs::remove_file(path.join("journal.json")).unwrap();
        fs::create_dir(path.join("journal.json")).unwrap();
        assert!(
            j.prepare(
                &mut json!({"name":"claim","arguments":{"idempotency_key":"test-key-123456789"}})
            )
            .is_err()
        );
        assert!(j.pending().is_none());
        assert!(j.retry_request().is_none());
    }
    #[test]
    fn completed_request_remains_replayable_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state");
        let mut j = Journal::open(&path, "a".into()).unwrap();
        let mut call = json!({"name":"claim","arguments":{"idempotency_key":"test-key-123456789"}});
        j.prepare(&mut call).unwrap();
        j.complete().unwrap();
        drop(j);
        let mut j = Journal::open(&path, "a".into()).unwrap();
        assert!(j.pending().is_none());
        assert_eq!(j.retry_request(), Some(call.clone()));
        j.prepare(&mut call).unwrap();
        assert_eq!(j.pending(), Some(call));
    }
    #[test]
    fn rejects_unrelated_directory_and_corrupt_state() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("unrelated"), "keep").unwrap();
        assert!(Journal::open(dir.path(), "a".into()).is_err());
        let path = dir.path().join("state");
        drop(Journal::open(&path, "a".into()).unwrap());
        fs::write(path.join("journal.json"), "{").unwrap();
        assert!(Journal::open(&path, "a".into()).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn private_modes_and_link_rejection() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state");
        let mut j = Journal::open(&path, "a".into()).unwrap();
        j.prepare(
            &mut json!({"name":"claim","arguments":{"idempotency_key":"test-key-123456789"}}),
        )
        .unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(path.join("journal.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        symlink(&path, dir.path().join("link")).unwrap();
        assert!(Journal::open(&dir.path().join("link"), "a".into()).is_err());
    }
}
