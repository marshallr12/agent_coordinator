//! The runner's durable record of each launch. An entry is written, flushed
//! and atomically put in place before the launch it describes is started,
//! and again once Windows has given the launch its identity. A runner that
//! finds an entry it did not write this run never starts that launch again:
//! it only judges whether the launch may still run ([`super::recovery`]).
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// How far a launch got, as last saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Its profile, directories and desktop exist; nothing was started.
    Prepared,
    /// A start was about to be attempted; whether it happened is unknown.
    Starting,
    /// Windows created the root process recorded here.
    Started,
    /// The launcher emptied the launch's job.
    Ended,
}

/// One launch as saved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// The launch's id; also the entry's file name.
    pub launch: String,
    pub stage: Stage,
    /// The AppContainer profile and its package SID.
    pub profile: String,
    pub package_sid: String,
    /// The launcher process that owns the launch's jobs.
    pub job_owner: String,
    /// The exact executable of the last start, once one was attempted.
    pub executable: Option<String>,
    /// What the caller says the launch is for: the exact source revision
    /// and the coordinator attempt and generation. Recorded, not checked.
    pub source: Option<String>,
    pub attempt: Option<String>,
    pub generation: Option<u64>,
    /// The root process of the last start and its creation time.
    pub pid: Option<u32>,
    pub created: Option<u64>,
    /// The launch's control and working directories, so the runner can
    /// remove them when the launch is cleaned up or reclaimed. Absent in
    /// entries written before they were recorded.
    #[serde(default)]
    pub control: Option<PathBuf>,
    #[serde(default)]
    pub work: Option<PathBuf>,
}

/// A directory of entries, one file per launch.
#[derive(Debug, Clone)]
pub struct Journal {
    directory: PathBuf,
}

impl Journal {
    /// The journal in `directory`, which is created if it is missing.
    pub fn open(directory: &Path) -> Result<Self> {
        fs::create_dir_all(directory).with_context(|| format!("create {}", directory.display()))?;
        Ok(Self {
            directory: directory.to_owned(),
        })
    }

    /// Where the entry of `launch` lives. The id becomes a file name, so
    /// only letters, digits, `-`, `_` and `.` are accepted.
    fn path(&self, launch: &str) -> Result<PathBuf> {
        let plain = !launch.is_empty()
            && launch.len() <= 96
            && !launch.starts_with('.')
            && (launch.chars()).all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
        ensure!(plain, "{launch:?} cannot name a launch");
        Ok(self.directory.join(format!("{launch}.json")))
    }

    /// Saves `entry`: a temporary file is written and flushed to disk, then
    /// renamed over the old entry, so a reader sees one or the other whole.
    pub fn save(&self, entry: &Entry) -> Result<()> {
        let path = self.path(&entry.launch)?;
        let temporary = path.with_extension("tmp");
        let mut file = fs::File::create(&temporary)
            .with_context(|| format!("create {}", temporary.display()))?;
        file.write_all(&serde_json::to_vec(entry)?)?;
        file.sync_all().context("flush the launch record")?;
        drop(file);
        fs::rename(&temporary, &path).with_context(|| format!("replace {}", path.display()))
    }

    /// The saved entry of `launch`, if there is a readable one.
    pub fn load(&self, launch: &str) -> Option<Entry> {
        let bytes = fs::read(self.path(launch).ok()?).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Every saved entry, in launch order. A file that cannot be read as an
    /// entry is reported by name: it is an unknown producer, not an absent one.
    pub fn load_all(&self) -> Result<(Vec<Entry>, Vec<String>)> {
        let (mut entries, mut unreadable) = (Vec::new(), Vec::new());
        for file in fs::read_dir(&self.directory)? {
            let path = file?.path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let parsed = fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok());
            match parsed {
                Some(entry) => entries.push(entry),
                None => unreadable.push(path.display().to_string()),
            }
        }
        entries.sort_by(|a: &Entry, b: &Entry| a.launch.cmp(&b.launch));
        Ok((entries, unreadable))
    }

    /// Removes the entry of `launch`, once nothing of the launch is left.
    pub fn remove(&self, launch: &str) -> Result<()> {
        let path = self.path(launch)?;
        match fs::remove_file(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                Err(error).with_context(|| format!("remove {}", path.display()))
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A prepared entry for `launch`.
    fn entry(launch: &str) -> Entry {
        Entry {
            launch: launch.to_owned(),
            stage: Stage::Prepared,
            profile: "agentc.test".into(),
            package_sid: "S-1-15-2-1".into(),
            job_owner: "runner:10:500".into(),
            executable: None,
            source: Some("6be041f".into()),
            attempt: Some("a1".into()),
            generation: Some(4),
            pid: None,
            created: None,
            control: None,
            work: Some(PathBuf::from("w")),
        }
    }

    #[test]
    fn an_entry_is_replaced_whole_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(&dir.path().join("launches")).unwrap();
        let mut saved = entry("impl-1");
        journal.save(&saved).unwrap();
        saved.stage = Stage::Started;
        (saved.pid, saved.created) = (Some(4242), Some(77));
        journal.save(&saved).unwrap();
        assert_eq!(journal.load("impl-1"), Some(saved.clone()));
        assert_eq!(journal.load_all().unwrap(), (vec![saved], vec![]));
        assert!(!dir.path().join("launches/impl-1.tmp").exists());
        journal.remove("impl-1").unwrap();
        journal.remove("impl-1").unwrap();
        assert_eq!(journal.load("impl-1"), None);
    }

    #[test]
    fn an_unreadable_entry_is_reported_not_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(dir.path()).unwrap();
        journal.save(&entry("b")).unwrap();
        journal.save(&entry("a")).unwrap();
        fs::write(dir.path().join("torn.json"), b"{\"launch\":").unwrap();
        fs::write(dir.path().join("notes.txt"), b"ignored").unwrap();
        let (entries, unreadable) = journal.load_all().unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.launch.as_str()).collect();
        assert_eq!(names, ["a", "b"]);
        assert_eq!(unreadable.len(), 1);
        assert!(unreadable[0].ends_with("torn.json"));
    }

    #[test]
    fn a_launch_id_cannot_leave_the_journal_directory() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(dir.path()).unwrap();
        for id in ["", "..", "a/b", "a\\b", ".hidden", "c:evil", "a b"] {
            assert!(journal.save(&entry(id)).is_err(), "{id:?}");
            assert_eq!(journal.load(id), None, "{id:?}");
        }
    }
}
