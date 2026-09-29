//! The integrator's only local state besides mirrors and intents: the last
//! target tip seen per target (repository URL + branch, see [`target_key`]),
//! sticky freezes, ruleset-missing episodes, the results granted push
//! authority to this integrator per target (so the tip monitor can tell its
//! own landings from out-of-band ones, and a conflict revise can cite the
//! landing that moved the target) and the check reruns requested
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

/// Published results remembered per target; the tip monitor only needs the
/// ones landed since its previous observation.
const MAX_PUBLISHED: usize = 64;

/// A result granted push authority to this integrator: R, its tip T0 and
/// the service's result id. `t0` is `None` for an entry stored in
/// `state.json` as a bare R string; only R itself then counts as the
/// integrator's. `result_id` is `None` for an entry stored without one; such
/// an entry is never cited as the landing that moved the target.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(from = "PublishedEntry")]
pub struct Published {
    pub r: String,
    pub t0: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_id: Option<String>,
}

/// The stored forms of [`Published`]: an object, or a bare R.
#[derive(Deserialize)]
#[serde(untagged)]
enum PublishedEntry {
    Pair {
        r: String,
        t0: Option<String>,
        #[serde(default)]
        result_id: Option<String>,
    },
    Bare(String),
}

/// Reads either stored form.
impl From<PublishedEntry> for Published {
    fn from(entry: PublishedEntry) -> Self {
        match entry {
            PublishedEntry::Pair { r, t0, result_id } => Self { r, t0, result_id },
            PublishedEntry::Bare(r) => Self {
                r,
                t0: None,
                result_id: None,
            },
        }
    }
}

/// How the target tip moved since the last recorded observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TipMove {
    /// The target is frozen (now or earlier); the reason names both tips.
    Frozen(String),
    /// The tip is the recorded one.
    Unchanged,
    /// The tip descends from the recorded one (`None` on first sight).
    Forward(Option<String>),
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
    /// Results granted push authority, per target key, oldest first.
    #[serde(default)]
    published: BTreeMap<String, Vec<Published>>,
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

    /// Classifies the move of `target` to tip `x` without recording it:
    /// freezes the target when `x` does not descend from the recorded tip.
    /// The caller records `x` with [`LoopState::record_tip`] once the move
    /// is dealt with.
    pub fn tip_move(&mut self, target: &str, mirror: &Path, x: &str) -> Result<TipMove> {
        if let Some(reason) = self.frozen.get(target) {
            let hint = format!("(remove it from {} to resume)", self.path.display());
            return Ok(TipMove::Frozen(format!("{reason} {hint}")));
        }
        let Some(previous) = self.tips.get(target).cloned() else {
            return Ok(TipMove::Forward(None));
        };
        if previous == x {
            return Ok(TipMove::Unchanged);
        }
        if git::is_ancestor(mirror, &previous, x)? {
            return Ok(TipMove::Forward(Some(previous)));
        }
        let reason = format!("target_rewritten: {previous} is not an ancestor of {x}");
        self.freeze(target, &reason)
            .map(|()| TipMove::Frozen(reason))
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

    /// Remembers that this integrator may publish `entry` on `target`
    /// (recorded before the push, so a crash after it cannot make the
    /// landing look out-of-band); keeps the latest [`MAX_PUBLISHED`]. When
    /// the save fails the entry is forgotten again, so a retry saves it
    /// before any push instead of finding it already known in memory.
    pub fn record_published(&mut self, target: &str, entry: Published) -> Result<()> {
        let previous = self.published.get(target).cloned();
        let list = self.published.entry(target.into()).or_default();
        if list.iter().any(|known| known.r == entry.r) {
            return Ok(());
        }
        list.push(entry);
        let excess = list.len().saturating_sub(MAX_PUBLISHED);
        list.drain(..excess);
        self.save()
            .inspect_err(|_| self.restore_published(target, previous))
    }

    /// Puts `target`'s published list back to `previous` (absent when `None`).
    fn restore_published(&mut self, target: &str, previous: Option<Vec<Published>>) {
        match previous {
            Some(list) => self.published.insert(target.into(), list),
            None => self.published.remove(target),
        };
    }

    /// The results recorded by [`LoopState::record_published`] for `target`.
    pub fn published(&self, target: &str) -> &[Published] {
        self.published.get(target).map_or(&[], Vec::as_slice)
    }

    /// Remembers the latest observed tip.
    pub fn record_tip(&mut self, target: &str, tip: &str) -> Result<()> {
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

    /// Records `x` as the tip of `target` when it is a forward move.
    fn observe(state: &mut LoopState, mirror: &Path, x: &str) -> TipMove {
        let moved = state.tip_move("p", mirror, x).unwrap();
        if matches!(moved, TipMove::Forward(_)) {
            state.record_tip("p", x).unwrap();
        }
        moved
    }

    #[test]
    fn forward_moves_pass_and_rewrites_freeze_stickily() {
        let remote = remote();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut state = LoopState::load(&path).unwrap();
        let base = git(&remote.source, &["rev-parse", "HEAD"]);
        let next = commit(&remote.source, "n.txt", "n\n");
        assert_eq!(
            observe(&mut state, &remote.source, &base),
            TipMove::Forward(None)
        );
        assert_eq!(
            observe(&mut state, &remote.source, &base),
            TipMove::Unchanged
        );
        let forward = TipMove::Forward(Some(base.clone()));
        assert_eq!(observe(&mut state, &remote.source, &next), forward);
        let rewritten = observe(&mut state, &remote.source, &base);
        assert!(
            matches!(&rewritten, TipMove::Frozen(r) if r.starts_with("target_rewritten")),
            "{rewritten:?}"
        );
        assert!(state.freeze_reason("p").unwrap().ends_with(&base));
        let other = dir.path().join("other.json");
        let mut fresh = LoopState::load(&other).unwrap();
        fresh.tips.insert("p".into(), "ab".repeat(20));
        assert!(
            fresh.tip_move("p", &remote.source, &base).is_err(),
            "missing object is not a rewrite"
        );
        assert!(fresh.frozen.is_empty());
        let mut reloaded = LoopState::load(&path).unwrap();
        let frozen = reloaded.tip_move("p", &remote.source, &next).unwrap();
        assert!(matches!(frozen, TipMove::Frozen(_)), "{frozen:?}");
    }

    #[test]
    fn a_pending_forward_move_is_not_recorded() {
        let remote = remote();
        let dir = tempfile::tempdir().unwrap();
        let mut state = LoopState::load(&dir.path().join("state.json")).unwrap();
        let base = git(&remote.source, &["rev-parse", "HEAD"]);
        observe(&mut state, &remote.source, &base);
        let next = commit(&remote.source, "n.txt", "n\n");
        let forward = TipMove::Forward(Some(base));
        assert_eq!(state.tip_move("p", &remote.source, &next).unwrap(), forward);
        assert_eq!(state.tip_move("p", &remote.source, &next).unwrap(), forward);
    }

    /// A published entry for R `r` on tip `x`, with result id `id-<r>`.
    fn entry(r: &str) -> Published {
        Published {
            r: r.into(),
            t0: Some("x".into()),
            result_id: Some(format!("id-{r}")),
        }
    }

    #[test]
    fn published_results_persist_and_stay_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut state = LoopState::load(&path).unwrap();
        for n in 0..=MAX_PUBLISHED {
            state
                .record_published("t", entry(&format!("r{n}")))
                .unwrap();
        }
        state.record_published("t", entry("r1")).unwrap();
        let reloaded = LoopState::load(&path).unwrap();
        let published = reloaded.published("t");
        assert_eq!(published.len(), MAX_PUBLISHED);
        assert_eq!(published[0].r, "r1", "the oldest is dropped");
        let last = published.last().unwrap();
        assert_eq!(last.r, format!("r{MAX_PUBLISHED}"));
        assert_eq!(last.t0.as_deref(), Some("x"));
        assert_eq!(last.result_id, Some(format!("id-r{MAX_PUBLISHED}")));
        assert!(reloaded.published("other").is_empty());
    }

    #[test]
    fn a_published_entry_whose_save_failed_is_not_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut state = LoopState::load(&path).unwrap();
        std::fs::create_dir(path.with_extension("json.tmp")).unwrap();
        assert!(state.record_published("t", entry("r")).is_err());
        assert!(state.published("t").is_empty(), "rolled back in memory");
        assert!(
            state.record_published("t", entry("r")).is_err(),
            "retry saves again"
        );
        std::fs::remove_dir(path.with_extension("json.tmp")).unwrap();
        state.record_published("t", entry("r")).unwrap();
        assert_eq!(LoopState::load(&path).unwrap().published("t").len(), 1);
    }

    #[test]
    fn bare_published_entries_load_as_r_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let text = r#"{"published": {"t": ["r0", {"r": "r1", "t0": "x"}]}}"#;
        std::fs::write(&path, text).unwrap();
        let state = LoopState::load(&path).unwrap();
        let bare = Published {
            r: "r0".into(),
            t0: None,
            result_id: None,
        };
        assert_eq!(state.published("t")[0], bare);
        assert_eq!(state.published("t")[1].t0.as_deref(), Some("x"));
        assert_eq!(state.published("t")[1].result_id, None, "no id stored");
    }
}
