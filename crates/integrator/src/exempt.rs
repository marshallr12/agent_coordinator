//! What a failing target must not stall (P6). Two parts, both driven by the
//! same required-check facts the flake attribution uses:
//!
//! * The pass-through. A reproduced failure of a check on R that also fails
//!   on the target tip X is exempt, so R still proceeds, when the service
//!   roster declares the check's input paths (`inputs`) and every declared
//!   path has the same blob (or is absent) in R and in X, and X's failing
//!   result is fresh: it started after R's first failure of the check (an
//!   older failure is rerun on X, like an older pass, and the fresh result
//!   decides). A candidate that changes an input must pass the check; a check
//!   without declared inputs, a target whose result is missing or stale, or
//!   any other failing check on R is handled as before. The integrator files
//!   the target's `target_failing` report (which opens the fix task at the
//!   service) and records the exemption; push authority verifies the same
//!   facts again from the service's records.
//! * The resolution. Each cycle, an open `target_failing` report whose check
//!   passed on the target tip in a run that started after the report was
//!   created is reported to the service, which resolves it with a note
//!   naming the passing run.
use crate::attribution::fix_target_report;
use crate::checks::ChecksSource;
use crate::flake::{Blame, CheckAttempts, OnTarget, on_target, started_after};
use crate::git;
use crate::integrate::{Integrator, Job};
use crate::roster;
use crate::service::{
    InputBlobs, NewExemption, OpenReport, Queue, ResultRecord, Target, TargetPassed,
    is_service_failure,
};
use crate::watch::Watched;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

/// Whether a reproduced failure that also fails on X is exempt.
pub(crate) enum Exempt {
    /// The exemption is recorded: R proceeds as far as this check goes.
    Yes,
    /// X's failure is older than R's and was rerun; a later cycle decides.
    Wait,
    /// Not exempt: the subject is handled as for any failure on the target.
    No,
}

impl<C: ChecksSource> Integrator<C> {
    /// Decides whether `check`'s reproduced failure on R, which fails on X
    /// as `x` says, is exempt, and records the exemption when it is.
    pub(crate) async fn exempt_failure(
        &mut self,
        job: &Job,
        result: &ResultRecord,
        check: &CheckAttempts,
        x: &OnTarget,
    ) -> Result<Exempt> {
        let OnTarget::Failed(failed) = x else {
            return Ok(Exempt::No);
        };
        let Some(inputs) = identical_inputs(job, result, &check.check.inputs)? else {
            return Ok(Exempt::No);
        };
        if !started_after(failed, check.first_failure_started()) {
            let rerun = self.rerun_target(job, result, failed).await?;
            return Ok(if rerun == Blame::Wait {
                Exempt::Wait
            } else {
                Exempt::No
            });
        }
        self.attest_exemption(job, result, (check, x), inputs).await
    }

    /// Files the target's `target_failing` report and records the exemption
    /// that names it; a refusal by the service leaves the failure unexempt.
    async fn attest_exemption(
        &self,
        job: &Job,
        result: &ResultRecord,
        (check, x): (&CheckAttempts, &OnTarget),
        inputs: Vec<InputBlobs>,
    ) -> Result<Exempt> {
        let report = fix_target_report(job, result, check, x, "target_failing");
        let report_id = match self.service.report(&job.project, &report).await? {
            Ok(record) => record.id,
            Err(refusal) => return Ok(self.not_exempt(job, "report", &refusal.code)),
        };
        let exemption = NewExemption {
            result_id: result.id.clone(),
            identity: check.check.identity.clone(),
            check_name: check.check.check_name.clone(),
            r: result.r.clone(),
            x: job.x.clone(),
            report_id: report_id.clone(),
            inputs,
        };
        match self
            .service
            .record_exemption(&job.project, &exemption)
            .await?
        {
            Ok(_) => {
                eprintln!(
                    "agentc-integrator: {}: {} failed on R {} and on target {} with declared inputs \
                     unchanged: exempt (report {report_id})",
                    job.project, check.check.check_name, result.r, job.x
                );
                Ok(Exempt::Yes)
            }
            Err(refusal) => Ok(self.not_exempt(job, "exemption", &refusal.code)),
        }
    }

    /// Logs that the service refused `what` and returns [`Exempt::No`].
    fn not_exempt(&self, job: &Job, what: &str, code: &str) -> Exempt {
        eprintln!(
            "agentc-integrator: {}: {what} refused ({code}); the failure is not exempt",
            job.project
        );
        Exempt::No
    }

    /// Resolves every open `target_failing` report whose check now passes on
    /// its target's tip in a run that started after the report was created.
    /// A failure to read or report one report is logged and skipped; a
    /// failure to reach the service aborts the cycle.
    pub(crate) async fn resolve_fixed_targets(
        &mut self,
        project: &str,
        queue: &Queue,
        watched: &Watched,
    ) -> Result<()> {
        let reports = match self.service.open_target_reports(project).await? {
            Ok(reports) => reports,
            Err(refusal) => {
                eprintln!(
                    "agentc-integrator: {project}: open reports refused: {}",
                    refusal.code
                );
                return Ok(());
            }
        };
        let failing = reports
            .iter()
            .filter(|r| r.details["verdict"] == "target_failing");
        for report in failing {
            if let Err(error) = self.report_fixed(project, queue, watched, report).await {
                if is_service_failure(&error) {
                    return Err(error);
                }
                eprintln!(
                    "agentc-integrator: {project}: report {}: {error:#}",
                    report.id
                );
            }
        }
        Ok(())
    }

    /// Tells the service that `report`'s check passed on the target's tip,
    /// when it did in a run that started after the report was created.
    async fn report_fixed(
        &self,
        project: &str,
        queue: &Queue,
        watched: &Watched,
        report: &OpenReport,
    ) -> Result<()> {
        let text = |key: &str| report.details[key].as_str().unwrap_or_default().to_owned();
        let target = Target {
            repository_url: text("repository_url"),
            target_branch: text("target_branch"),
        };
        let Some(x) = watched.tip(&target.key()) else {
            return Ok(());
        };
        let mirror = git::mirror_dir(&self.config.state_dir, &target.repository_url);
        let roster = roster::at_target(&mirror, &x, &self.config.roster_path, &queue.roster)?;
        let identity = text("identity");
        let Some(check) = roster.checks.iter().find(|c| c.identity == identity) else {
            return Ok(());
        };
        let created = DateTime::parse_from_rfc3339(&report.created_at)
            .context("report created_at")?
            .with_timezone(&Utc);
        let repo = crate::github::RepoId::from_url(&target.repository_url);
        let wanted = std::slice::from_ref(check);
        let runs = self.checks.check_runs(repo.as_ref(), &x, wanted).await?;
        let OnTarget::Passed(run) = on_target(&runs, check, &x, Some(created)) else {
            return Ok(());
        };
        let passed = TargetPassed {
            x,
            run_id: run.run_id,
            run_attempt: run.run_attempt,
            conclusion: run.conclusion.clone().unwrap_or_default(),
            started_at: run.started_at.clone().unwrap_or_default(),
        };
        match self
            .service
            .target_passed(project, &report.id, &passed)
            .await?
        {
            Ok(_) => eprintln!(
                "agentc-integrator: {project}: {} passes on target {}: report {} resolved",
                check.check_name, passed.x, report.id
            ),
            Err(refusal) => eprintln!(
                "agentc-integrator: {project}: report {} not resolved: {}",
                report.id, refusal.code
            ),
        }
        Ok(())
    }
}

/// The declared inputs with their blob ids when every one is identical in R
/// and X (the same blob, or absent from both); `None` when the check
/// declares no inputs or any input differs. A revision the mirror cannot
/// read is an error, never "absent from both".
fn identical_inputs(
    job: &Job,
    result: &ResultRecord,
    declared: &[String],
) -> Result<Option<Vec<InputBlobs>>> {
    if declared.is_empty() {
        return Ok(None);
    }
    for rev in [&result.r, &job.x] {
        git::text(
            &job.mirror,
            ["rev-parse", "--verify", &format!("{rev}^{{tree}}")],
        )
        .with_context(|| format!("revision {rev} is not in the mirror"))?;
    }
    let mut compared = Vec::with_capacity(declared.len());
    for path in declared {
        let r_blob = git::blob_at(&job.mirror, &result.r, path)?;
        let x_blob = git::blob_at(&job.mirror, &job.x, path)?;
        if r_blob != x_blob {
            return Ok(None);
        }
        compared.push(InputBlobs {
            path: path.clone(),
            r_blob,
            x_blob,
        });
    }
    Ok(Some(compared))
}
