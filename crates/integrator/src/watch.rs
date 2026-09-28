//! The per-target part of a cycle (p4-design §3 steps 1–2), run for every
//! target of the queue whether or not an item is queued for it: the ruleset
//! watchdog, then the tip monitor. Each target is observed with one
//! `ls-remote` and one fetch of its branch into the mirror per cycle; the
//! items integrate against the tip observed here.
use crate::checks::ChecksSource;
use crate::git;
use crate::github::RepoId;
use crate::integrate::{Integrator, Step};
use crate::service::{Queue, Target};
use crate::state::TipMove;
use anyhow::{Context, Result};
use std::path::PathBuf;

/// Each watched target's key with its tip X, or the step that holds its
/// items this cycle (a freeze, or a tip move not yet classified).
pub(crate) struct Watched(Vec<(String, Result<String, Step>)>);

impl Watched {
    /// The step of the first held target, if any.
    pub fn first_held(&self) -> Option<Step> {
        self.0.iter().find_map(|(_, status)| status.clone().err())
    }

    /// The observed tip of the target stored under `key`; `None` when that
    /// target is held or was not watched.
    pub fn tip(&self, key: &str) -> Option<String> {
        let status = self.0.iter().find(|(watched, _)| watched == key);
        status.and_then(|(_, status)| status.clone().ok())
    }
}

impl<C: ChecksSource> Integrator<C> {
    /// Runs the watchdog and the tip monitor on every target of `queue`.
    pub(crate) async fn watch_targets(&mut self, project: &str, queue: &Queue) -> Result<Watched> {
        let mut watched = Vec::new();
        for target in queue.all_targets() {
            let status = self.watch_target(project, &target).await?;
            watched.push((target.key(), status));
        }
        Ok(Watched(watched))
    }

    /// Watchdog, then tip monitor. A missing rule freezes the target, but
    /// its tip is still observed so no move goes unclassified meanwhile.
    async fn watch_target(
        &mut self,
        project: &str,
        target: &Target,
    ) -> Result<Result<String, Step>> {
        let rules = self.watchdog(project, target).await?;
        let tip = self.monitor_tip(project, target).await?;
        Ok(match rules {
            Some(frozen) => Err(frozen),
            None => tip,
        })
    }

    /// Step 1: the freeze step when a required rule is missing; otherwise
    /// ends any open ruleset-missing episode of the target.
    async fn watchdog(&mut self, project: &str, target: &Target) -> Result<Option<Step>> {
        let repo = RepoId::from_url(&target.repository_url);
        let missing = self
            .missing_rules(repo.as_ref(), &target.target_branch)
            .await?;
        if missing.is_empty() {
            self.state.end_ruleset_episode(&target.key())?;
            return Ok(None);
        }
        self.ruleset_frozen(project, target, &missing)
            .await
            .map(Some)
    }

    /// The required rule types missing from the target branch, in
    /// configuration order; empty when the branch carries them all.
    async fn missing_rules(&self, repo: Option<&RepoId>, branch: &str) -> Result<Vec<String>> {
        let active = self.checks.branch_rules(repo, branch).await?;
        let required = self.config.required_rules.iter();
        Ok(required
            .filter(|rule| !active.contains(rule))
            .cloned()
            .collect())
    }

    /// Step 2: observes X; a rewrite freezes, a forward move is classified
    /// (`landing.rs`) and X becomes the recorded tip once the move settled.
    /// An unsettled move holds the target's items, so nothing is integrated
    /// on a tip whose landing is not yet reported.
    async fn monitor_tip(
        &mut self,
        project: &str,
        target: &Target,
    ) -> Result<Result<String, Step>> {
        let (mirror, x) = self.observe_target(target)?;
        let key = target.key();
        let previous = match self.state.tip_move(&key, &mirror, &x)? {
            TipMove::Frozen(reason) => {
                return Ok(Err(self.rewrite_frozen(project, target, reason).await));
            }
            TipMove::Unchanged => return Ok(Ok(x)),
            TipMove::Forward(previous) => previous,
        };
        let settled = match previous {
            Some(p) => {
                self.classify_move(project, &mirror, target, (&p, &x))
                    .await?
            }
            None => true,
        };
        if !settled {
            let branch = &target.target_branch;
            let held = format!("tip_move_unsettled: {branch} moved to {x}, report pending");
            return Ok(Err(Step::Blocked(held)));
        }
        self.state.record_tip(&key, &x)?;
        Ok(Ok(x))
    }

    /// Mirrors the repository, reads X and fetches the target branch.
    fn observe_target(&self, target: &Target) -> Result<(PathBuf, String)> {
        let url = &target.repository_url;
        let mirror = git::mirror_dir(&self.config.state_dir, url);
        git::ensure_mirror(&mirror, url)?;
        let target_ref = format!("refs/heads/{}", target.target_branch);
        let x = git::ls_remote(&mirror, url, &target_ref)?
            .with_context(|| format!("target {target_ref} does not exist"))?;
        git::fetch(&mirror, &[format!("+{target_ref}:{target_ref}")])?;
        Ok((mirror, x))
    }
}
