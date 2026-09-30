//! One integration cycle per project (p4-design §3). The
//! per-target watchdog and tip monitor are in `watch.rs` (the tip monitor's
//! `unreviewed_landing` check in `landing.rs`), the freeze reports and the
//! privilege decision in `gates.rs`, flake attribution in `attribution.rs`.
//! Everything durable lives in the service (results, receipts, authority);
//! local intents and worktrees are disposable and are rebuilt from the
//! service's pinned result, whose R must reproduce exactly.
//! Checks, pushing and observations are in `publish.rs`. Reverts awaiting
//! a mechanical candidate are worked before ordinary items (they are always
//! admitted and usually urgent); their computation, and the refusal of a
//! no-op that re-lands reverted commits, are in `reverts.rs`.
use crate::checks::ChecksSource;
use crate::config::Config;
use crate::git;
use crate::github::RepoId;
use crate::reverts::reverted_commits;
use crate::roster::{self, Roster};
use crate::service::{
    Cite, NewResult, Queue, QueueItem, ResultRecord, Service, is_service_failure,
};
use crate::state::{LoopState, Published, target_key};
use anyhow::{Context, Result};
use coordinator_local::git_workflow::{self, IntegrationIntentSummary, PrepareIntegration};
use std::path::{Path, PathBuf};

/// What one cycle did; logged, and asserted by tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Idle,
    Frozen(String),
    Blocked(String),
    Refused(String),
    Revised(String),
    ChecksPending,
    ReturnedToReview,
    Observed(String),
    /// The service now holds a mechanical candidate for this revert task.
    RevertCandidate(String),
    /// The revert is now ordinary implementation work (not mechanical), for
    /// this reason.
    NotMechanical(String),
    /// Working this subject or watching this target failed with an error
    /// that is not a [`crate::service::ServiceFailure`]; it is skipped this
    /// cycle and retried on the next.
    Failed(String),
}

/// The integrator's long-lived parts.
pub struct Integrator<C: ChecksSource> {
    pub config: Config,
    pub service: Service,
    pub checks: C,
    pub state: LoopState,
}

/// One queue item being integrated against target tip X.
pub(crate) struct Job {
    pub project: String,
    pub item: QueueItem,
    pub repo: Option<RepoId>,
    pub mirror: PathBuf,
    pub x: String,
}

impl Job {
    /// Short key naming this (submission, X) pair on disk.
    fn key(&self) -> String {
        format!("{}-{}", self.item.submission_id, &self.x[..12])
    }

    /// The target branch's full ref name.
    pub fn target_ref(&self) -> String {
        format!("refs/heads/{}", self.item.target_branch)
    }

    /// The key the target's last tip and freeze are stored under.
    pub fn target_key(&self) -> String {
        target_key(&self.item.repository_url, &self.item.target_branch)
    }
}

impl<C: ChecksSource> Integrator<C> {
    /// Runs one cycle for `project`: every target is watched first, then
    /// the first revert or queue item that is not skipped is taken as far as
    /// it can go (reverts first; one of either per cycle). Work on a held
    /// target (frozen, its tip move not yet reported, or its watch failed) is
    /// skipped; blocked, refused and failed work is logged to stderr and
    /// skipped so it cannot stall the rest. A [`crate::service::ServiceFailure`]
    /// aborts the cycle.
    pub async fn cycle(&mut self, project: &str) -> Result<Step> {
        let queue: Queue = match self.service.queue(project).await? {
            Ok(queue) => queue,
            Err(refusal) => return Ok(Step::Refused(refusal.code)),
        };
        let watched = self.watch_targets(project, &queue).await?;
        let mut last = watched.first_held().unwrap_or(Step::Idle);
        for revert in &queue.reverts {
            let Some(x) = revert.target().and_then(|t| watched.tip(&t.key())) else {
                continue;
            };
            last = self
                .revert_cycle(project, revert, x)
                .await
                .or_else(failed)?;
            if !skipped(project, &revert.id, &last) {
                return Ok(last);
            }
        }
        for item in &queue.items {
            let Some(x) = watched.tip(&item.target().key()) else {
                continue;
            };
            let outcome = self.item_cycle(project, &queue, item.clone(), x).await;
            last = outcome.or_else(failed)?;
            if !skipped(project, &item.submission_id, &last) {
                return Ok(last);
            }
        }
        Ok(last)
    }

    /// Held authority first, then integration against the watched tip X.
    async fn item_cycle(
        &mut self,
        project: &str,
        queue: &Queue,
        item: QueueItem,
        x: String,
    ) -> Result<Step> {
        let job = self.open_job(project, item, x)?;
        if let Some(held) = held_elsewhere(&job) {
            let nonce = held.authority_expires_at.clone().unwrap_or_default();
            return self.observe_and_close(&job, &held, &nonce).await;
        }
        self.integrate(&job, queue).await
    }

    /// Fetches C into the target's mirror, which the watch already created
    /// and brought up to X.
    pub(crate) fn open_job(&self, project: &str, item: QueueItem, x: String) -> Result<Job> {
        let mirror = git::mirror_dir(&self.config.state_dir, &item.repository_url);
        let candidate = match &item.candidate_ref {
            Some(reference) => format!("+{reference}:{reference}"),
            None => item.candidate_revision.clone(),
        };
        git::fetch(&mirror, &[candidate])?;
        Ok(Job {
            project: project.into(),
            repo: RepoId::from_url(&item.repository_url),
            item,
            mirror,
            x,
        })
    }

    /// Steps 3 and 5–7 for one job.
    async fn integrate(&mut self, job: &Job, queue: &Queue) -> Result<Step> {
        let roster =
            match roster::at_target(&job.mirror, &job.x, &self.config.roster_path, &queue.roster) {
                Ok(roster) => roster,
                Err(error) => return Ok(Step::Blocked(format!("roster: {error:#}"))),
            };
        let result = match self.pin_result(job, &roster).await? {
            Ok(result) => result,
            Err(step) => return Ok(step),
        };
        if result.r == job.x {
            return self.observe_and_close(job, &result, "").await;
        }
        if let Some(skip) = self.privilege_gate(job, &result).await? {
            return Ok(skip);
        }
        self.check_and_publish(job, &result, &roster).await
    }

    /// The pinned result for (S, X). R is always (re)computed locally, so
    /// the intent and worktree exist; a reused service result must match it.
    /// A no-op that re-lands reverted commits is revised instead of pinned.
    async fn pin_result(&self, job: &Job, roster: &Roster) -> Result<Result<ResultRecord, Step>> {
        let c = &job.item.candidate_revision;
        if !git::merges_cleanly(&job.mirror, &job.x, c)? {
            return self.revise_conflict(job).await.map(Err);
        }
        let computed = self.compute_result(job, roster)?;
        let relanded = reverted_commits(&job.item, &computed);
        if !relanded.is_empty() {
            return self.revise_reverted(job, &relanded).await.map(Err);
        }
        if let Some(existing) = job.item.results.iter().find(|r| r.t0 == job.x) {
            return Ok(reproduced(existing, &computed));
        }
        match self.service.record_result(&job.project, &computed).await? {
            Ok(result) => Ok(Ok(result)),
            Err(refusal) => self.result_refused(job, refusal).await.map(Err),
        }
    }

    /// Revises a candidate that conflicts with X, citing the result this
    /// integrator published on the target since the candidate's reviewed
    /// base, when there is one (see [`landing_result`]).
    async fn revise_conflict(&self, job: &Job) -> Result<Step> {
        let c = &job.item.candidate_revision;
        let evidence = format!("candidate {c} conflicts with target tip {}", job.x);
        let published = self.state.published(&job.target_key());
        let moved_by = landing_result(&job.mirror, published, &job.x, &job.item.reviewed_base);
        let cite = Cite {
            moved_by_result_id: moved_by,
            ..Cite::default()
        };
        self.revise(job, ("conflict", &evidence), cite).await
    }

    /// Computes R in a linked worktree through `git_workflow` (deterministic;
    /// idempotent while the intent file exists).
    pub(crate) fn compute_result(&self, job: &Job, roster: &Roster) -> Result<NewResult> {
        let summary = self.prepare(job)?;
        Ok(NewResult {
            submission_id: job.item.submission_id.clone(),
            landing_range: git::landing_range(&job.mirror, &job.x, &summary.candidate)?,
            r: summary.result.context("integration produced no result")?,
            r_tree: summary
                .result_tree
                .context("integration produced no result tree")?,
            t0: summary.expected_target,
            t0_tree: summary.expected_target_tree,
            c: summary.candidate,
            roster: roster.json.clone(),
        })
    }

    /// Prepares (or reloads) the job's integration intent in its worktree.
    fn prepare(&self, job: &Job) -> Result<IntegrationIntentSummary> {
        let (worktree, branch, intent) = self.job_paths(job);
        git::ensure_worktree(&job.mirror, &worktree, &branch, &job.x)?;
        git_workflow::prepare_integration(PrepareIntegration {
            state_file: &intent,
            checkout: &worktree,
            configured_remote: &job.item.repository_url,
            target_branch: &job.item.target_branch,
            expected_target: &job.x,
            candidate_base: &job.item.reviewed_base,
            candidate: &job.item.candidate_revision,
        })
    }

    /// Worktree dir, its branch and the intent file for a job.
    pub(crate) fn job_paths(&self, job: &Job) -> (PathBuf, String, PathBuf) {
        let key = job.key();
        let state = &self.config.state_dir;
        (
            state.join("worktrees").join(&key),
            format!("integration/{key}"),
            state.join("intents").join(format!("{key}.json")),
        )
    }

    /// Drops the job's worktree and intent; the next cycle rebuilds both
    /// from the service's result (R is deterministic).
    pub(crate) fn discard_local(&self, job: &Job) -> Result<()> {
        let (worktree, branch, intent) = self.job_paths(job);
        git::remove_worktree(&job.mirror, &worktree, &branch)?;
        for suffix in ["json", "lock", "journal.jsonl"] {
            let _ = std::fs::remove_file(intent.with_extension(suffix));
        }
        Ok(())
    }

    /// Sends the subject back to its implementer with `(reason, evidence)`
    /// and what the revise cites, and drops local state.
    pub(crate) async fn revise(
        &self,
        job: &Job,
        (reason, evidence): (&str, &str),
        cite: Cite<'_>,
    ) -> Result<Step> {
        let submission = &job.item.submission_id;
        let reply = self
            .service
            .revise(&job.project, submission, (reason, evidence), cite)
            .await?;
        self.discard_local(job)?;
        Ok(match reply {
            Ok(_) => Step::Revised(reason.into()),
            Err(refusal) => Step::Refused(refusal.code),
        })
    }
}

/// Logs `step` when it is `Blocked`, `Refused` or `Failed`, which skips
/// `subject` for this cycle; true when it did.
fn skipped(project: &str, subject: &str, step: &Step) -> bool {
    let skip = matches!(step, Step::Blocked(_) | Step::Refused(_) | Step::Failed(_));
    if skip {
        log_skip(project, subject, step);
    }
    skip
}

/// Logs to stderr that `subject` (an item, revert or target) is skipped
/// this cycle, and the step that skips it.
pub(crate) fn log_skip(project: &str, subject: &str, step: &Step) {
    eprintln!("agentc-integrator: {project}/{subject}: {step:?}");
}

/// Turns the error of one subject or target into `Step::Failed`, which skips
/// it this cycle; a [`crate::service::ServiceFailure`] stays an error and
/// aborts the cycle.
pub(crate) fn failed(error: anyhow::Error) -> Result<Step> {
    if is_service_failure(&error) {
        return Err(error);
    }
    Ok(Step::Failed(format!("{error:#}")))
}

/// A result of this submission for another tip that still holds push
/// authority: it must be observed before anything else can proceed.
fn held_elsewhere(job: &Job) -> Option<ResultRecord> {
    let held = |r: &&ResultRecord| r.authority_expires_at.is_some() && r.t0 != job.x;
    job.item.results.iter().find(held).cloned()
}

/// The id of the newest result this integrator published on the target that
/// landed since the candidate's reviewed `base`: its R is an ancestor of X
/// and not of `base`. Published results land on a fast-forward-only target,
/// so the qualifying Rs form one line and the newest descends from the rest.
/// Entries without a result id, or that Git cannot place, never qualify.
pub(crate) fn landing_result<'a>(
    mirror: &Path,
    published: &'a [Published],
    x: &str,
    base: &str,
) -> Option<&'a str> {
    let ancestor = |a: &str, d: &str| git::is_ancestor(mirror, a, d).ok();
    let landed = |p: &&Published| {
        p.result_id.is_some()
            && ancestor(&p.r, x) == Some(true)
            && ancestor(&p.r, base) == Some(false)
    };
    let mut newest: Option<&Published> = None;
    for entry in published.iter().filter(landed) {
        if newest.is_none_or(|n| ancestor(&n.r, &entry.r) == Some(true)) {
            newest = Some(entry);
        }
    }
    newest.and_then(|p| p.result_id.as_deref())
}

/// The service's result when the local computation reproduced its R.
fn reproduced(existing: &ResultRecord, computed: &NewResult) -> Result<ResultRecord, Step> {
    if existing.r == computed.r && existing.c == computed.c {
        return Ok(existing.clone());
    }
    Err(Step::Blocked(format!(
        "result {} pins R {} but this host computes {}",
        existing.id, existing.r, computed.r
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::testing::{commit, git, remote};

    /// A published entry for R `r` with result id `id`.
    fn published(r: &str, id: Option<&str>) -> Published {
        Published {
            r: r.into(),
            t0: None,
            result_id: id.map(Into::into),
        }
    }

    #[test]
    fn the_newest_result_landed_since_the_base_is_cited() {
        let remote = remote();
        let dir = &remote.source;
        let base = git(dir, &["rev-parse", "HEAD"]);
        let first = commit(dir, "a.txt", "a\n");
        let second = commit(dir, "b.txt", "b\n");
        let unnamed = commit(dir, "c.txt", "c\n");
        let x = commit(dir, "d.txt", "d\n");
        git(dir, &["checkout", "--quiet", "-b", "side", &base]);
        let unlanded = commit(dir, "e.txt", "e\n");
        let list = [
            published(&second, Some("b")),
            published(&first, Some("a")),
            published(&unnamed, None),
            published(&unlanded, Some("u")),
        ];
        let newest = landing_result(dir, &list, &x, &base);
        assert_eq!(newest, Some("b"), "newest by ancestry, not list order");
        assert_eq!(landing_result(dir, &list[1..], &x, &base), Some("a"));
        let none = landing_result(dir, &list, &x, &second);
        assert_eq!(none, None, "results in the base did not move the target");
        let missing = [published(&"ab".repeat(20), Some("m"))];
        assert_eq!(landing_result(dir, &missing, &x, &base), None);
    }
}
