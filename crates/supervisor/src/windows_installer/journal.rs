//! The installer's journal: one JSON line before each change is applied and
//! one after, appended and flushed. A run that fails part-way therefore
//! leaves a record of what it applied, and the next run, which plans from
//! the host as it then is, completes the rest.
//!
//! The journal lives in the installer's own directory below the state root.
//! On a clean host that directory does not exist yet, so the first lines
//! wait in memory until the installer has made it.
use super::plan::Change;
use anyhow::{Context, Result};
use serde_json::json;
use std::io::Write;
use std::path::PathBuf;

/// Where one change stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// About to be applied.
    Applying,
    /// Applied.
    Applied,
    /// Failed; the line carries why.
    Failed,
}

impl Stage {
    /// The stage's name in a journal line.
    fn name(self) -> &'static str {
        match self {
            Self::Applying => "applying",
            Self::Applied => "applied",
            Self::Failed => "failed",
        }
    }
}

/// An append-only journal file.
#[derive(Debug)]
pub struct Journal {
    path: PathBuf,
    run: String,
    waiting: Vec<String>,
}

impl Journal {
    /// The journal at `path`, for a new run.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            run: uuid::Uuid::new_v4().to_string(),
            waiting: Vec::new(),
        }
    }

    /// Records that `change` reached `stage`, with `error` when it failed.
    pub fn record(&mut self, stage: Stage, change: &Change, error: Option<&str>) -> Result<()> {
        let at = chrono::Utc::now().to_rfc3339();
        let mut line = json!({"at": at, "run": self.run, "stage": stage.name(), "change": change});
        if let Some(error) = error {
            line["error"] = error.into();
        }
        self.waiting.push(line.to_string());
        self.flush()
    }

    /// Appends the waiting lines once the journal's directory exists.
    fn flush(&mut self) -> Result<()> {
        let home = self.path.parent().is_some_and(|dir| dir.is_dir());
        if !home || self.waiting.is_empty() {
            return Ok(());
        }
        let text = self.waiting.join("\n") + "\n";
        let mut options = std::fs::OpenOptions::new();
        let opened = options.create(true).append(true).open(&self.path);
        let mut file = opened.with_context(|| format!("open {}", self.path.display()))?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        self.waiting.clear();
        Ok(())
    }

    /// How many lines still wait for the journal's directory.
    pub fn waiting(&self) -> usize {
        self.waiting.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::windows_installer::plan::Action;

    /// A change to journal.
    fn change() -> Change {
        Change {
            action: Action::InstallBinary,
            detail: "missing".into(),
        }
    }

    #[test]
    fn lines_wait_until_the_directory_exists_and_then_all_land() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("installer");
        let mut journal = Journal::at(home.join("journal.jsonl"));
        journal.record(Stage::Applying, &change(), None).unwrap();
        assert_eq!(journal.waiting(), 1);
        std::fs::create_dir(&home).unwrap();
        journal.record(Stage::Applied, &change(), None).unwrap();
        assert_eq!(journal.waiting(), 0);
        let text = std::fs::read_to_string(home.join("journal.jsonl")).unwrap();
        let stages: Vec<&str> = text
            .lines()
            .map(|l| &l[l.find("stage").unwrap()..])
            .collect();
        assert!(stages[0].starts_with(r#"stage":"applying"#), "{text}");
        assert!(stages[1].starts_with(r#"stage":"applied"#), "{text}");
    }

    #[test]
    fn a_failure_is_recorded_with_its_reason_and_the_change() {
        let dir = tempfile::tempdir().unwrap();
        let mut journal = Journal::at(dir.path().join("journal.jsonl"));
        journal
            .record(Stage::Failed, &change(), Some("access denied"))
            .unwrap();
        let text = std::fs::read_to_string(dir.path().join("journal.jsonl")).unwrap();
        let line: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(line["stage"], "failed");
        assert_eq!(line["error"], "access denied");
        assert_eq!(line["change"]["action"], "install_binary");
    }
}
