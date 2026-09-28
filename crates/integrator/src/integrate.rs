//! One integration cycle per project (p4-design §3 steps 1–5 and 7; flake
//! attribution and `unreviewed_landing` tip-move reports are not built yet).
//! The freeze reports and the privilege decision are in `gates.rs`.
//! Everything durable lives in the service (results, receipts, authority);
//! local intents and worktrees are disposable and are rebuilt from the
//! service's pinned result, whose R must reproduce exactly.
//! Checks, pushing and observations are in `publish.rs`.
use crate::checks::ChecksSource;
use crate::config::Config;
use crate::git;
use crate::github::RepoId;
use crate::roster::{self, Roster};
use crate::service::{NewResult, Queue, QueueItem, ResultRecord, Service};
use crate::state::{LoopState, target_key};
use anyhow::{Context, Result};
use coordinator_local::git_workflow::{self, IntegrationIntentSummary, PrepareIntegration};
use std::path::PathBuf;

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
    /// Runs one cycle for `project`: the first queue item that is not
    /// blocked is taken as far as it can go. Blocked items are logged to
    /// stderr and skipped so they cannot stall the rest of the queue.
    pub async fn cycle(&mut self, project: &str) -> Result<Step> {
        let queue: Queue = match self.service.queue(project).await? {
            Ok(queue) => queue,
            Err(refusal) => return Ok(Step::Refused(refusal.code)),
        };
        let mut last = Step::Idle;
        for item in &queue.items {
            last = self.item_cycle(project, &queue, item.clone()).await?;
            if !matches!(last, Step::Blocked(_)) {
                return Ok(last);
            }
            eprintln!(
                "agentc-integrator: {project}/{}: {last:?}",
                item.submission_id
            );
        }
        Ok(last)
    }

    /// Watchdog, target observation, held authority, then integration.
    async fn item_cycle(&mut self, project: &str, queue: &Queue, item: QueueItem) -> Result<Step> {
        let repo = RepoId::from_url(&item.repository_url);
        let missing = self
            .missing_rules(repo.as_ref(), &item.target_branch)
            .await?;
        if !missing.is_empty() {
            return self.ruleset_frozen(project, &item, &missing).await;
        }
        let target = target_key(&item.repository_url, &item.target_branch);
        self.state.end_ruleset_episode(&target)?;
        let job = self.open_job(project, item, repo)?;
        if let Some(reason) = self
            .state
            .check_tip(&job.target_key(), &job.mirror, &job.x)?
        {
            return self.rewrite_frozen(&job, reason).await;
        }
        if let Some(held) = held_elsewhere(&job) {
            let nonce = held.authority_expires_at.clone().unwrap_or_default();
            return self.observe_and_close(&job, &held, &nonce).await;
        }
        self.integrate(&job, queue).await
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

    /// Mirrors the repository, observes X and fetches X and C.
    fn open_job(&self, project: &str, item: QueueItem, repo: Option<RepoId>) -> Result<Job> {
        let mirror = git::mirror_dir(&self.config.state_dir, &item.repository_url);
        git::ensure_mirror(&mirror, &item.repository_url)?;
        let target_ref = format!("refs/heads/{}", item.target_branch);
        let x = git::ls_remote(&mirror, &item.repository_url, &target_ref)?
            .with_context(|| format!("target {target_ref} does not exist"))?;
        let candidate = match &item.candidate_ref {
            Some(reference) => format!("+{reference}:{reference}"),
            None => item.candidate_revision.clone(),
        };
        git::fetch(&mirror, &[format!("+{target_ref}:{target_ref}"), candidate])?;
        Ok(Job {
            project: project.into(),
            item,
            repo,
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
    async fn pin_result(&self, job: &Job, roster: &Roster) -> Result<Result<ResultRecord, Step>> {
        let c = &job.item.candidate_revision;
        if !git::merges_cleanly(&job.mirror, &job.x, c)? {
            let evidence = format!("candidate {c} conflicts with target tip {}", job.x);
            return self.revise(job, "conflict", &evidence).await.map(Err);
        }
        let computed = self.compute_result(job, roster)?;
        if let Some(existing) = job.item.results.iter().find(|r| r.t0 == job.x) {
            return Ok(reproduced(existing, &computed));
        }
        let reply = self.service.record_result(&job.project, &computed).await?;
        Ok(reply.map_err(|refusal| Step::Refused(refusal.code)))
    }

    /// Computes R in a linked worktree through `git_workflow` (deterministic;
    /// idempotent while the intent file exists).
    fn compute_result(&self, job: &Job, roster: &Roster) -> Result<NewResult> {
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

    /// Sends the subject back to its implementer and drops local state.
    pub(crate) async fn revise(&self, job: &Job, reason: &str, evidence: &str) -> Result<Step> {
        let reply = self
            .service
            .revise(&job.project, &job.item.submission_id, reason, evidence)
            .await?;
        self.discard_local(job)?;
        Ok(match reply {
            Ok(_) => Step::Revised(reason.into()),
            Err(refusal) => Step::Refused(refusal.code),
        })
    }
}

/// A result of this submission for another tip that still holds push
/// authority: it must be observed before anything else can proceed.
fn held_elsewhere(job: &Job) -> Option<ResultRecord> {
    let held = |r: &&ResultRecord| r.authority_expires_at.is_some() && r.t0 != job.x;
    job.item.results.iter().find(held).cloned()
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
