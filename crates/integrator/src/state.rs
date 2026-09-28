//! The integrator's only local state besides mirrors and intents: the last
//! target tip seen per target (repository URL + branch, see [`target_key`]),
//! sticky freezes, ruleset-missing episodes and the check reruns requested
//! (so a rerun the checks source has not started yet is not requested
//! again, and reruns per run stay capped). Only Git's "not an ancestor" verdict freezes; a Git failure
//! (missing object, lock) is an ordinary error and retries. A tip that stops
//! descending from the previous one means the target was rewritten (force
//! push); the target then stays frozen until a human deletes its entry from
//! `state.json`, because every published/not-published judgement assumes a
//! fast-forward-only target.
use crate::git;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The key a target's tip and freeze are stored under.
pub fn target_key(url: &str, branch: &str) -> String {
    format!("{url}#{branch}")
}

/// Last tips and freezes, persisted as `state.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct LoopState {
    #[serde(skip)]
    path: PathBuf,
    #[serde(default)]
    tips: BTreeMap<String, String>,
    #[serde(default)]
    frozen: BTreeMap<String, String>,
    /// Open ruleset-missing episodes: target key to episode name.
    #[serde(default)]
    ruleset_episodes: BTreeMap<String, String>,
    /// Episodes started so far; part of every episode name.
    #[serde(default)]
    episode_seq: u64,
    /// Reruns requested, keyed `<result id>:<run id>`.
    #[serde(default)]
    reruns: BTreeMap<String, RerunRecord>,
}

/// The reruns requested for one workflow run of one result.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct RerunRecord {
    /// The attempt the latest request reran.
    pub attempt: i64,
    /// Requests made so far.
    pub count: u32,
}

impl LoopState {
    /// Loads `path`, or starts empty when it does not exist.
    pub fn load(path: &Path) -> Result<Self> {
        let mut state: Self = match std::fs::read_to_string(path) {
            Ok(text) => {
                serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        state.path = path.to_path_buf();
        Ok(state)
    }

    /// Writes the state atomically (temp file + rename).
    fn save(&self) -> Result<()> {
        let parent = self.path.parent().context("state file has no parent")?;
        std::fs::create_dir_all(parent)?;
        let temp = self.path.with_extension("json.tmp");
        std::fs::write(&temp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&temp, &self.path).context("replace state file")
    }

    /// Freeze reason for `target`, freezing it now if `x` does not descend
    /// from the last recorded tip; otherwise records `x` and returns `None`.
    pub fn check_tip(&mut self, target: &str, mirror: &Path, x: &str) -> Result<Option<String>> {
        if let Some(reason) = self.frozen.get(target) {
            return Ok(Some(format!(
                "{reason} (remove it from {} to resume)",
                self.path.display()
            )));
        }
        match self.tips.get(target) {
            Some(previous) if previous != x && !git::is_ancestor(mirror, previous, x)? => {
                let reason = format!("target_rewritten: {previous} is not an ancestor of {x}");
                self.freeze(target, &reason).map(|()| Some(reason))
            }
            _ => self.record_tip(target, x).map(|()| None),
        }
    }

    /// The stored freeze reason of `target`, if it is frozen.
    pub fn freeze_reason(&self, target: &str) -> Option<&str> {
        self.frozen.get(target).map(String::as_str)
    }

    /// The open ruleset-missing episode of `target`, starting one if none is
    /// open. Its name joins the start time in milliseconds and a sequence
    /// number, so a later episode never reuses an earlier name.
    pub fn ruleset_episode(&mut self, target: &str) -> Result<String> {
        if let Some(open) = self.ruleset_episodes.get(target) {
            return Ok(open.clone());
        }
        self.episode_seq += 1;
        let millis = chrono::Utc::now().timestamp_millis();
        let name = format!("{millis}-{}", self.episode_seq);
        self.ruleset_episodes.insert(target.into(), name.clone());
        self.save().map(|()| name)
    }

    /// Ends the ruleset-missing episode of `target` (its rules are back).
    pub fn end_ruleset_episode(&mut self, target: &str) -> Result<()> {
        if self.ruleset_episodes.remove(target).is_none() {
            return Ok(());
        }
        self.save()
    }

    /// The reruns requested for `run_id` of `result` (zero when none).
    pub fn rerun_of(&self, result: &str, run_id: i64) -> RerunRecord {
        let key = format!("{result}:{run_id}");
        self.reruns.get(&key).copied().unwrap_or_default()
    }

    /// Records a request to rerun attempt `attempt` of `run_id` for `result`.
    pub fn record_rerun(&mut self, result: &str, run_id: i64, attempt: i64) -> Result<()> {
        let record = self.reruns.entry(format!("{result}:{run_id}")).or_default();
        record.attempt = attempt;
        record.count += 1;
        self.save()
    }

    /// Freezes `target` until a human clears the entry.
    fn freeze(&mut self, target: &str, reason: &str) -> Result<()> {
        self.frozen.insert(target.into(), reason.into());
        self.save()
    }

    /// Remembers the latest observed tip.
    fn record_tip(&mut self, target: &str, tip: &str) -> Result<()> {
        if self.tips.get(target).map(String::as_str) == Some(tip) {
            return Ok(());
        }
        self.tips.insert(target.into(), tip.into());
        self.save()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::testing::{commit, git, remote};

    #[test]
    fn ruleset_episodes_persist_until_ended_and_never_repeat() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut state = LoopState::load(&path).unwrap();
        let first = state.ruleset_episode("t").unwrap();
        let mut reloaded = LoopState::load(&path).unwrap();
        assert_eq!(reloaded.ruleset_episode("t").unwrap(), first);
        reloaded.end_ruleset_episode("t").unwrap();
        assert_ne!(reloaded.ruleset_episode("t").unwrap(), first);
    }

    #[test]
    fn requested_reruns_persist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut state = LoopState::load(&path).unwrap();
        assert_eq!(state.rerun_of("res1", 7), RerunRecord::default());
        state.record_rerun("res1", 7, 1).unwrap();
        state.record_rerun("res1", 7, 2).unwrap();
        let reloaded = LoopState::load(&path).unwrap();
        let record = RerunRecord {
            attempt: 2,
            count: 2,
        };
        assert_eq!(reloaded.rerun_of("res1", 7), record);
        assert_eq!(reloaded.rerun_of("res2", 7).count, 0);
    }

    #[test]
    fn forward_moves_pass_and_rewrites_freeze_stickily() {
        let remote = remote();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut state = LoopState::load(&path).unwrap();
        let base = git(&remote.source, &["rev-parse", "HEAD"]);
        let next = commit(&remote.source, "n.txt", "n\n");
        assert_eq!(state.check_tip("p", &remote.source, &base).unwrap(), None);
        assert_eq!(state.check_tip("p", &remote.source, &next).unwrap(), None);
        assert!(
            state
                .check_tip("p", &remote.source, &base)
                .unwrap()
                .unwrap()
                .starts_with("target_rewritten")
        );
        assert!(state.freeze_reason("p").unwrap().ends_with(&base));
        let other = dir.path().join("other.json");
        let mut fresh = LoopState::load(&other).unwrap();
        fresh.tips.insert("p".into(), "ab".repeat(20));
        assert!(
            fresh.check_tip("p", &remote.source, &base).is_err(),
            "missing object is not a rewrite"
        );
        assert!(fresh.frozen.is_empty());
        let mut reloaded = LoopState::load(&path).unwrap();
        assert!(
            reloaded
                .check_tip("p", &remote.source, &next)
                .unwrap()
                .is_some()
        );
    }
}
