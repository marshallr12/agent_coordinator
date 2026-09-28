//! Steps 5–7 of an integration: R on its result branch, one poll of the
//! required checks per cycle (so the queue GET heartbeat never stalls),
//! receipts, push authority, the lease-guarded push and the observation
//! that always follows a grant — even when the push step fails — because
//! the service keeps authority outstanding until an observation ends it.
use crate::checks::{ACTIONS_APP_ID, CheckRun, ChecksSource, RosterCheck};
use crate::git;
use crate::integrate::{Integrator, Job, Step};
use crate::roster::Roster;
use crate::service::{Ancestry, Observation, Receipt, Reply, ResultRecord};
use anyhow::{Context, Result, bail, ensure};
use coordinator_local::git_workflow::{
    self, FreshPublicationAuthority, PublicationAuthorizationContext,
};
use serde_json::Value;
use std::time::{Duration, Instant};

/// Seconds of push authority kept in reserve for the push itself.
const AUTHORITY_MARGIN_SECONDS: u64 = 30;
/// The service's authority lifetime; a local budget never exceeds it.
const AUTHORITY_TTL_SECONDS: u64 = 600;

impl<C: ChecksSource> Integrator<C> {
    /// Pushes R for checks, gates on them, then publishes.
    pub(crate) async fn check_and_publish(
        &mut self,
        job: &Job,
        result: &ResultRecord,
        roster: &Roster,
    ) -> Result<Step> {
        let branch = format!(
            "refs/heads/{}{}",
            self.config.result_branch_prefix, result.id
        );
        git::push_create_only(&job.mirror, &job.item.repository_url, &result.r, &branch)?;
        if let Some(step) = self.checks_gate(job, result, &roster.checks).await? {
            return Ok(step);
        }
        self.publish(job, result).await
    }

    /// Polls the roster's checks on R once and posts receipts; `None` when
    /// all passed. A failure revises (S4 adds rerun-based flake attribution).
    async fn checks_gate(
        &self,
        job: &Job,
        result: &ResultRecord,
        wanted: &[RosterCheck],
    ) -> Result<Option<Step>> {
        let runs = self
            .checks
            .check_runs(job.repo.as_ref(), &result.r, wanted)
            .await?;
        let Some(runs) = completed_runs(&runs, wanted, &result.r) else {
            return Ok(Some(Step::ChecksPending));
        };
        for run in &runs {
            self.post_receipt(job, result, run).await?;
        }
        match failed_checks(&runs) {
            None => Ok(None),
            Some(names) => {
                let evidence = format!("required checks failed on {}: {names}", result.r);
                self.revise(job, "check_failed", &evidence).await.map(Some)
            }
        }
    }

    /// Posts one receipt, bound to the workflow blob the run used (in R).
    async fn post_receipt(&self, job: &Job, result: &ResultRecord, run: &CheckRun) -> Result<()> {
        let blob = git::blob_at(&job.mirror, &result.r, &run.workflow_path)?
            .with_context(|| format!("workflow {} missing in R", run.workflow_path))?;
        let receipt = Receipt {
            result_id: result.id.clone(),
            check_name: run.check_name.clone(),
            run_id: run.run_id,
            run_attempt: run.run_attempt,
            app_id: run.app_id,
            head_sha: run.head_sha.clone(),
            workflow_path: run.workflow_path.clone(),
            workflow_blob: blob,
            conclusion: run.conclusion.clone().unwrap_or_default(),
        };
        if let Err(refusal) = self.service.record_receipt(&job.project, &receipt).await? {
            bail!("receipt refused: {}", refusal.code);
        }
        Ok(())
    }

    /// Step 7: authority, then the push attempt, then — whatever happened —
    /// the observation that ends the authority.
    async fn publish(&mut self, job: &Job, result: &ResultRecord) -> Result<Step> {
        let sent = Instant::now();
        let grant = match self
            .service
            .push_authority(&job.project, &result.id)
            .await?
        {
            Ok(grant) => grant,
            Err(refusal) => return Ok(Step::Refused(refusal.code)),
        };
        if grant["granted"] != true {
            self.discard_local(job)?;
            return Ok(Step::ReturnedToReview);
        }
        if let Err(error) = self.try_push(job, result, &grant, sent).await {
            eprintln!(
                "agentc-integrator: push of {} not completed: {error:#}",
                result.r
            );
        }
        let nonce = grant["expires_at"].as_str().unwrap_or_default().to_owned();
        self.observe_and_close(job, result, &nonce).await
    }

    /// Pushes R with a lease on X if the grant names this R and X, time is
    /// left, and the tip is still X. `git_workflow` re-observes around it.
    async fn try_push(
        &self,
        job: &Job,
        result: &ResultRecord,
        grant: &Value,
        sent: Instant,
    ) -> Result<()> {
        ensure!(
            grant["r"] == result.r.as_str() && grant["t0"] == result.t0.as_str(),
            "grant names another result"
        );
        let authority = FreshPublicationAuthority::valid_for(authority_budget(grant, sent)?)?;
        let tip = git::ls_remote(&job.mirror, &job.item.repository_url, &job.target_ref())?;
        ensure!(
            tip.as_deref() == Some(job.x.as_str()),
            "target moved before the push"
        );
        let (_, _, intent) = self.job_paths(job);
        let authorize = authorize_only(result, authority);
        let outcome =
            git_workflow::publish_prepared(&intent, &job.item.repository_url, authorize).await?;
        eprintln!(
            "agentc-integrator: publication of {}: {outcome:?}",
            result.r
        );
        Ok(())
    }

    /// Observes the target, attests it, drops local state (every disposition
    /// either finishes the subject or is rebuilt from the service next
    /// cycle), and checks the observed tip for a rewrite.
    pub(crate) async fn observe_and_close(
        &mut self,
        job: &Job,
        result: &ResultRecord,
        nonce: &str,
    ) -> Result<Step> {
        let (tip, ancestry) = self.ancestry(job, result)?;
        let data = match self.attest(job, result, (&tip, ancestry), nonce).await? {
            Ok(data) => data,
            Err(refusal) => return Ok(Step::Refused(refusal.code)),
        };
        self.discard_local(job)?;
        if let Some(reason) = self.state.check_tip(&job.target_key(), &job.mirror, &tip)? {
            return Ok(Step::Frozen(reason));
        }
        Ok(Step::Observed(
            data["disposition"].as_str().unwrap_or("unknown").to_owned(),
        ))
    }

    /// Posts the observation of `tip` for `result`.
    async fn attest(
        &self,
        job: &Job,
        result: &ResultRecord,
        (tip, ancestry): (&str, Ancestry),
        nonce: &str,
    ) -> Result<Reply<Value>> {
        let evidence = format!("ls-remote {} = {tip}", job.target_ref());
        let observation = Observation {
            result_id: &result.id,
            tip,
            ancestry,
            evidence: &evidence,
            nonce,
        };
        self.service.observe(&job.project, &observation).await
    }

    /// The current tip (fetched locally) and its ancestry relative to R.
    fn ancestry(&self, job: &Job, result: &ResultRecord) -> Result<(String, Ancestry)> {
        let tip = git::ls_remote(&job.mirror, &job.item.repository_url, &job.target_ref())?
            .context("target branch disappeared")?;
        if tip == result.r {
            return Ok((tip, Ancestry::Contained));
        }
        if tip == result.t0 {
            return Ok((tip, Ancestry::EqualT0));
        }
        let target_ref = job.target_ref();
        git::fetch(&job.mirror, &[format!("+{target_ref}:{target_ref}")])?;
        let ancestry = match git::is_ancestor(&job.mirror, &result.r, &tip)? {
            true => Ancestry::Contained,
            false => Ancestry::Moved,
        };
        Ok((tip, ancestry))
    }
}

/// The names of failed checks, joined, or `None` when all succeeded.
fn failed_checks(runs: &[CheckRun]) -> Option<String> {
    let failed: Vec<&str> = runs
        .iter()
        .filter(|run| run.conclusion.as_deref() != Some("success"))
        .map(|run| run.check_name.as_str())
        .collect();
    (!failed.is_empty()).then(|| failed.join(", "))
}

/// The `publish_prepared` callback: hands over the local authority budget
/// only when the intent's R and T0 are the authorized result's.
fn authorize_only(
    result: &ResultRecord,
    authority: FreshPublicationAuthority,
) -> impl FnOnce(PublicationAuthorizationContext) -> std::future::Ready<Result<FreshPublicationAuthority>>
{
    let (r, t0) = (result.r.clone(), result.t0.clone());
    move |ctx| {
        let same = ctx.result == r && ctx.expected_target == t0;
        std::future::ready(if same {
            Ok(authority)
        } else {
            Err(anyhow::anyhow!("local intent is not the authorized result"))
        })
    }
}

/// Completed runs for every wanted check (same name and workflow file,
/// Actions app, latest run and attempt), or `None` while any is missing or
/// still running.
fn completed_runs(runs: &[CheckRun], wanted: &[RosterCheck], sha: &str) -> Option<Vec<CheckRun>> {
    let latest = |check: &RosterCheck| {
        runs.iter()
            .filter(|run| {
                run.check_name == check.check_name && run.workflow_path == check.workflow_path
            })
            .filter(|run| run.head_sha == sha && run.app_id == ACTIONS_APP_ID)
            .max_by_key(|run| (run.run_id, run.run_attempt))
            .filter(|run| run.conclusion.is_some())
            .cloned()
    };
    wanted.iter().map(latest).collect()
}

/// Local push budget: the grant's remaining lifetime (never more than the
/// service's TTL, so local clock skew cannot stretch it), measured from when
/// the request was sent, minus a margin.
fn authority_budget(grant: &Value, sent: Instant) -> Result<Duration> {
    let expires = grant["expires_at"]
        .as_str()
        .context("grant has no expires_at")?;
    let expires = chrono::DateTime::parse_from_rfc3339(expires)?;
    let left = (expires.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_seconds();
    let left = u64::try_from(left).unwrap_or(0).min(AUTHORITY_TTL_SECONDS);
    let budget = Duration::from_secs(left.saturating_sub(AUTHORITY_MARGIN_SECONDS));
    Ok(budget.saturating_sub(sent.elapsed()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CI: &str = ".github/workflows/ci.yml";

    /// A completed or pending run of check `name` in `path`.
    fn run(name: &str, path: &str, id: i64, attempt: i64, conclusion: Option<&str>) -> CheckRun {
        CheckRun {
            check_name: name.into(),
            run_id: id,
            run_attempt: attempt,
            app_id: ACTIONS_APP_ID,
            head_sha: "r".into(),
            workflow_path: path.into(),
            conclusion: conclusion.map(str::to_owned),
        }
    }

    /// A roster check named `name` in `ci.yml`.
    fn wanted(name: &str) -> RosterCheck {
        RosterCheck {
            identity: name.into(),
            check_name: name.into(),
            workflow_path: CI.into(),
        }
    }

    #[test]
    fn latest_attempt_of_the_roster_workflow_decides() {
        let runs = [
            run("a", CI, 1, 1, Some("failure")),
            run("a", CI, 1, 2, Some("success")),
        ];
        let done = completed_runs(&runs, &[wanted("a")], "r").unwrap();
        assert_eq!(done[0].conclusion.as_deref(), Some("success"));
        let pending = [
            run("a", CI, 1, 1, Some("failure")),
            run("a", CI, 1, 2, None),
        ];
        assert!(completed_runs(&pending, &[wanted("a")], "r").is_none());
        let elsewhere = [run(
            "a",
            ".github/workflows/other.yml",
            9,
            1,
            Some("success"),
        )];
        assert!(completed_runs(&elsewhere, &[wanted("a")], "r").is_none());
    }

    /// A context naming result `r` on target `t0`.
    fn context(r: &str, t0: &str) -> PublicationAuthorizationContext {
        PublicationAuthorizationContext {
            target_branch: "main".into(),
            expected_target: t0.into(),
            expected_target_tree: "t".into(),
            candidate: "c".into(),
            result: r.into(),
            result_tree: "rt".into(),
        }
    }

    #[tokio::test]
    async fn only_the_authorized_result_gets_the_budget() {
        let result: ResultRecord = serde_json::from_value(json!({"id": "1", "submission_id": "s",
            "t0": "x", "t0_tree": "t", "c": "c", "r": "r", "r_tree": "rt", "landing_range": [], "roster": {}}))
        .unwrap();
        let budget = || FreshPublicationAuthority::valid_for(Duration::from_secs(60)).unwrap();
        assert!(
            authorize_only(&result, budget())(context("r", "x"))
                .await
                .is_ok()
        );
        assert!(
            authorize_only(&result, budget())(context("other", "x"))
                .await
                .is_err()
        );
        assert!(
            authorize_only(&result, budget())(context("r", "moved"))
                .await
                .is_err()
        );
    }

    #[test]
    fn budget_is_capped_keeps_a_margin_and_never_goes_negative() {
        let later = (chrono::Utc::now() + chrono::Duration::seconds(900)).to_rfc3339();
        let budget = authority_budget(&json!({"expires_at": later}), Instant::now()).unwrap();
        assert!((565..=570).contains(&budget.as_secs()), "{budget:?}");
        let past = json!({"expires_at": "2000-01-01T00:00:00.000Z"});
        assert_eq!(
            authority_budget(&past, Instant::now()).unwrap(),
            Duration::ZERO
        );
    }
}
