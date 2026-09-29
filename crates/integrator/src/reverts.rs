//! The integrator's side of revert tasks (planning plan-final §2.4a "M6";
//! the service side is `crates/server/src/integrator_reverts.rs`).
//!
//! A revert in the queue's `reverts` is computed mechanically on the
//! target's current tip X (`revert.rs`). A clean revert is pushed to a
//! create-only candidate ref named from the revert task and X, then recorded
//! as the revert's candidate with `t0` = X; from then on it is an ordinary
//! queue item (results, checks, flake attribution, the privilege gate, push
//! authority, publish, observation). A revert that conflicts on X, whose R
//! is not in X's history, or that would change nothing on X is reported
//! `not-mechanical` with reason `conflict` (never an empty candidate); a
//! service refusal of either post skips the revert for the cycle. A revert
//! item whose checks reproduce a failure that passes on X is reported
//! `not-mechanical` with reason `check_failed` instead of being revised.
//! Either report turns the revert into ordinary implementation work on the
//! service.
//!
//! This module also refuses to re-land reverted commits: a no-op result
//! (R == X) whose candidate commit or landing range meets a landing a revert
//! undid (`items[].reverted`), or that `results` refuses with
//! `candidate_reverted_in_history`, is revised `reverted_in_history` instead
//! of being pinned.
use crate::checks::ChecksSource;
use crate::git;
use crate::integrate::{Integrator, Job, Step};
use crate::revert::{self, Mechanical, RevertSpec};
use crate::service::{Cite, NewReport, NewResult, QueueItem, Refusal, RevertCandidate, RevertItem};
use anyhow::Result;
use serde_json::json;

/// The refusal a candidate gets when another one is recorded for its tip.
const CANDIDATE_CONFLICT: &str = "revert_candidate_conflict";
/// The `results` refusal of a no-op that re-lands reverted commits.
const REVERTED_IN_HISTORY: &str = "candidate_reverted_in_history";

impl<C: ChecksSource> Integrator<C> {
    /// Computes the mechanical revert of `revert` on tip `x` and records the
    /// candidate, or reports it not mechanical. A Git failure or a service
    /// refusal skips the revert this cycle (`Blocked`), so a revert the
    /// service keeps refusing cannot starve the reverts and items after it.
    pub(crate) async fn revert_cycle(
        &self,
        project: &str,
        revert: &RevertItem,
        x: String,
    ) -> Result<Step> {
        Ok(match self.revert_step(project, revert, &x).await? {
            Step::Refused(code) => blocked(revert, code),
            step => step,
        })
    }

    /// The step of one revert: its candidate recorded, its not-mechanical
    /// report, or why it is skipped.
    async fn revert_step(&self, project: &str, revert: &RevertItem, x: &str) -> Result<Step> {
        let Some(url) = revert.repository_url.as_deref() else {
            return Ok(blocked(revert, "no repository"));
        };
        let outcome = match self.compute_revert(revert, url, x) {
            Ok(Mechanical::Clean { commit, tree }) => {
                let candidate = candidate(revert, x, commit, tree);
                return self
                    .record_candidate(project, revert, url, &candidate)
                    .await;
            }
            Ok(outcome) => outcome,
            Err(error) => return Ok(blocked(revert, format!("{error:#}"))),
        };
        let evidence = unmechanical_evidence(revert, x, &outcome);
        let report = (x, "conflict", evidence.as_str());
        self.not_mechanical(project, &revert.id, report).await
    }

    /// Computes the revert in a disposable worktree of `url`'s mirror.
    fn compute_revert(&self, revert: &RevertItem, url: &str, x: &str) -> Result<Mechanical> {
        let mirror = git::mirror_dir(&self.config.state_dir, url);
        let key = format!("revert-{}-{}", revert.id, &x[..12]);
        let dir = self.config.state_dir.join("worktrees").join(&key);
        revert::compute(
            &mirror,
            &dir,
            &format!("revert/{key}"),
            &revert_spec(revert, x),
        )
    }

    /// Pushes the candidate to its create-only ref and records it. A
    /// different candidate already recorded for this tip skips the revert
    /// until X moves and the revert is computed again.
    async fn record_candidate(
        &self,
        project: &str,
        revert: &RevertItem,
        url: &str,
        candidate: &RevertCandidate,
    ) -> Result<Step> {
        let mirror = git::mirror_dir(&self.config.state_dir, url);
        let (commit, reference) = (&candidate.candidate_commit, &candidate.candidate_ref);
        if let Err(error) = git::push_create_only(&mirror, url, commit, reference) {
            return Ok(blocked(revert, format!("{error:#}")));
        }
        let reply = self
            .service
            .revert_candidate(project, &revert.id, candidate);
        Ok(match reply.await? {
            Ok(_) => Step::RevertCandidate(revert.id.clone()),
            Err(refusal) if refusal.code == CANDIDATE_CONFLICT => Step::Blocked(format!(
                "{CANDIDATE_CONFLICT}: revert {} on {}",
                revert.id, candidate.t0
            )),
            Err(refusal) => Step::Refused(refusal.code),
        })
    }

    /// Posts `not-mechanical` for revert task `id` with (tip, reason,
    /// evidence).
    async fn not_mechanical(
        &self,
        project: &str,
        id: &str,
        report: (&str, &str, &str),
    ) -> Result<Step> {
        let reply = self.service.not_mechanical(project, id, report).await?;
        Ok(match reply {
            Ok(_) => Step::NotMechanical(report.1.into()),
            Err(refusal) => Step::Refused(refusal.code),
        })
    }

    /// A revert item's checks reproduced a failure that passes on X: the
    /// revert is reported not mechanical (`check_failed`) instead of being
    /// revised, and local state is dropped.
    pub(crate) async fn revert_check_failed(
        &self,
        job: &Job,
        revert_id: &str,
        evidence: &str,
    ) -> Result<Step> {
        self.discard_local(job)?;
        let report = (job.x.as_str(), "check_failed", evidence);
        self.not_mechanical(&job.project, revert_id, report).await
    }

    /// Handles a `results` refusal: `candidate_reverted_in_history` is
    /// revised with the commits it names; any other stays a refusal.
    pub(crate) async fn result_refused(&self, job: &Job, refusal: Refusal) -> Result<Step> {
        if refusal.code != REVERTED_IN_HISTORY {
            return Ok(Step::Refused(refusal.code));
        }
        let commits = refusal.details["commits"].as_array().into_iter().flatten();
        let commits: Vec<String> = commits
            .filter_map(|sha| sha.as_str().map(str::to_owned))
            .collect();
        self.revise_reverted(job, &commits).await
    }

    /// Sends a candidate that re-lands reverted `commits` back to its author
    /// (`reverted_in_history`) and drops local state. The service refusing
    /// that (`not_reverted_in_history`) blocks the subject and is reported.
    pub(crate) async fn revise_reverted(&self, job: &Job, commits: &[String]) -> Result<Step> {
        let evidence = format!(
            "candidate {} re-lands commits a recorded revert undid: {}",
            job.item.candidate_revision,
            commits.join(", ")
        );
        let submission = &job.item.submission_id;
        let reason = ("reverted_in_history", evidence.as_str());
        let reply = self
            .service
            .revise(&job.project, submission, reason, Cite::default());
        let reply = reply.await?;
        self.discard_local(job)?;
        match reply {
            Ok(_) => Ok(Step::Revised(reason.0.into())),
            Err(refusal) if refusal.code == "not_reverted_in_history" => {
                Ok(self.history_disputed(job, commits).await)
            }
            Err(refusal) => Ok(Step::Refused(refusal.code)),
        }
    }

    /// The service found no reverted landing where this integrator did:
    /// reports it (best effort) and skips the subject.
    async fn history_disputed(&self, job: &Job, commits: &[String]) -> Step {
        let reason = format!(
            "not_reverted_in_history: the service finds no reverted landing in {} for {}",
            commits.join(", "),
            job.item.submission_id
        );
        eprintln!("agentc-integrator: {}: {reason}", job.project);
        let report = NewReport {
            kind: "fix_target",
            dedupe_key: format!(
                "{}:{}:not_reverted_in_history",
                job.item.submission_id, job.x
            ),
            task_id: Some(job.item.subject_task_id.clone()),
            submission_id: Some(job.item.submission_id.clone()),
            result_id: None,
            details: json!({"verdict": "not_reverted_in_history", "x": job.x,
                "candidate": job.item.candidate_revision, "commits": commits,
                "blocks_subject": true}),
        };
        self.report_best_effort(&job.project, report).await;
        Step::Blocked(reason)
    }
}

/// The step that skips `revert` this cycle, and why.
fn blocked(revert: &RevertItem, why: impl std::fmt::Display) -> Step {
    Step::Blocked(format!("revert {}: {why}", revert.id))
}

/// The `not-mechanical` evidence for a revert without a clean mechanical
/// candidate on `x` (reported with reason `conflict`: someone must decide
/// what undoing it means; the revert has no author to revise).
fn unmechanical_evidence(revert: &RevertItem, x: &str, outcome: &Mechanical) -> String {
    let r = &revert.target.r;
    match outcome {
        Mechanical::Conflict(paths) => format!(
            "the mechanical revert of {r} conflicts on target tip {x} in: {}",
            paths.join(", ")
        ),
        Mechanical::NotInHistory => format!("R {r} is not in the target history at {x}"),
        Mechanical::AlreadyUndone => format!("R {r} is already undone on {x}"),
        Mechanical::Clean { commit, .. } => format!("the revert of {r} on {x} is {commit}"),
    }
}

/// The revert of `revert`'s result on tip `x`.
fn revert_spec<'a>(revert: &'a RevertItem, x: &'a str) -> RevertSpec<'a> {
    RevertSpec {
        x,
        r: &revert.target.r,
        t0: &revert.target.t0,
        c: &revert.target.c,
        task_id: &revert.id,
    }
}

/// The candidate for a clean revert on `x`, with its create-only ref under
/// `refs/agent-coordinator/candidates/reverts/<task>/<x>`.
fn candidate(revert: &RevertItem, x: &str, commit: String, tree: String) -> RevertCandidate {
    RevertCandidate {
        t0: x.to_owned(),
        candidate_commit: commit,
        candidate_tree: tree,
        mechanical: true,
        candidate_ref: format!(
            "refs/agent-coordinator/candidates/reverts/{}/{x}",
            revert.id
        ),
    }
}

/// The commits of a no-op result (R == T0) — its candidate commit and
/// landing range — that lie in a landing a revert undid on the item's
/// target, sorted and each once; empty for any other result.
pub(crate) fn reverted_commits(item: &QueueItem, result: &NewResult) -> Vec<String> {
    if result.r != result.t0 {
        return Vec::new();
    }
    let same_target = |landing: &&crate::service::RevertedLanding| {
        landing
            .repository_url
            .as_deref()
            .is_none_or(|u| u == item.repository_url)
            && landing
                .target_branch
                .as_deref()
                .is_none_or(|b| b == item.target_branch)
    };
    let undone: Vec<&String> = item
        .reverted
        .iter()
        .filter(same_target)
        .flat_map(|landing| &landing.landing_range)
        .collect();
    let ours = result.landing_range.iter().chain([&result.c]);
    let mut hits: Vec<String> = ours.filter(|sha| undone.contains(sha)).cloned().collect();
    hits.sort();
    hits.dedup();
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    /// A queue item on `main` whose target had `landing` reverted, on
    /// `branch`.
    fn item(landing: &[&str], branch: &str) -> QueueItem {
        serde_json::from_value(json!({"subject_task_id": "t", "submission_id": "s",
            "title": "t", "priority": 1, "candidate_revision": "c", "candidate_ref": null,
            "reviewed_base": "b", "repository_url": "u", "target_branch": "main",
            "reverted": [{"revert_task_id": "rt", "result_id": "res", "landing_range": landing,
                "repository_url": "u", "target_branch": branch}]}))
        .unwrap()
    }

    /// A result with (r, t0), candidate `c` and landing range `range`.
    fn result(r: &str, t0: &str, c: &str, range: &[&str]) -> NewResult {
        NewResult {
            submission_id: "s".into(),
            t0: t0.into(),
            t0_tree: "tt".into(),
            c: c.into(),
            r: r.into(),
            r_tree: "rt".into(),
            landing_range: range.iter().map(|s| (*s).to_owned()).collect(),
            roster: Value::Null,
        }
    }

    #[test]
    fn only_a_no_op_on_the_same_target_meets_reverted_history() {
        let reverted = item(&["a", "c"], "main");
        let no_op = result("x", "x", "c", &[]);
        assert_eq!(reverted_commits(&reverted, &no_op), ["c"]);
        let merge = result("r", "x", "c", &["c"]);
        assert!(
            reverted_commits(&reverted, &merge).is_empty(),
            "not a no-op"
        );
        let other_branch = item(&["c"], "release");
        assert!(reverted_commits(&other_branch, &no_op).is_empty());
        let unrelated = item(&["a"], "main");
        assert!(reverted_commits(&unrelated, &no_op).is_empty());
    }
}
