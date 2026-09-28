//! Step 6 of an integration (p4-design §3; plan-final "Reproducible flake
//! attribution"), acting on the verdicts of `flake.rs`. The latest attempt
//! of each required check on R decides, as it does for push authority:
//! a failure or an attempt without a result is rerun (only failed and
//! cancelled jobs rerun), a pass after a failure is `flaky` and R proceeds,
//! and a failure is reproduced once the deciding attempt and at least one
//! other attempt failed. A reproduced failure goes back to the author only
//! when the same check passed on the target tip X; when it fails on X too
//! (`fix_target`, verdict `target_failing`) or X has no result for it
//! (`fix_target`, verdict `target_unverified`) the subject is skipped until
//! X moves or X's check passes, and while X's run is still going the gate
//! waits. A check that [`MAX_RERUNS`] reruns leave undecided, or a rerun
//! request the checks source refuses, skips the subject with a `flaky`
//! report.
//! A cycle polls R's checks once (and X's only for a reproduced failure)
//! and never waits for a rerun: a requested rerun keeps the gate at
//! `ChecksPending` until a newer attempt of its run appears and completes.
use crate::checks::{CheckRun, ChecksSource, RosterCheck};
use crate::flake::{
    Blame, CheckAttempts, MAX_RERUNS, OnTarget, Verdict, blame, latest_attempts, on_target, verdict,
};
use crate::gates::{short_digest, target_report};
use crate::integrate::{Integrator, Job, Step};
use crate::service::{NewReport, ResultRecord};
use anyhow::Result;
use serde_json::{Value, json};

/// Checks paired with their verdicts.
type Judged<'a> = Vec<(&'a CheckAttempts, Verdict)>;

impl<C: ChecksSource> Integrator<C> {
    /// True while a rerun requested for `result` has not produced a newer
    /// attempt of its run yet.
    pub(crate) fn rerun_in_flight(&self, result: &ResultRecord, checks: &[CheckAttempts]) -> bool {
        let runs = checks.iter().flat_map(|check| &check.attempts);
        latest_attempts(runs).into_iter().any(|(run_id, attempt)| {
            let rerun = self.state.rerun_of(&result.id, run_id);
            rerun.count > 0 && rerun.attempt >= attempt
        })
    }

    /// Judges every check once all latest attempts completed: reruns what
    /// needs another attempt, reports flaky and undecided checks, and
    /// attributes reproduced failures. `None` lets R proceed.
    pub(crate) async fn attribute(
        &mut self,
        job: &Job,
        result: &ResultRecord,
        checks: &[CheckAttempts],
    ) -> Result<Option<Step>> {
        let judged = self.judge(result, checks);
        let rerun = pick(&judged, Verdict::Rerun);
        if !rerun.is_empty() {
            return self.rerun(job, result, &rerun).await.map(Some);
        }
        self.report_flaky(job, result, &pick(&judged, Verdict::Flaky), "flaky")
            .await;
        let undecided = pick(&judged, Verdict::Undecided);
        self.report_flaky(job, result, &undecided, "no_result")
            .await;
        let reproduced = pick(&judged, Verdict::Reproduced);
        if !reproduced.is_empty() {
            return self
                .attribute_reproduced(job, result, &reproduced)
                .await
                .map(Some);
        }
        Ok((!undecided.is_empty()).then(|| Step::Blocked(undecided_reason(result, &undecided))))
    }

    /// Each check's verdict, given the reruns requested for its deciding run.
    fn judge<'a>(&self, result: &ResultRecord, checks: &'a [CheckAttempts]) -> Judged<'a> {
        let judge = |check: &'a CheckAttempts| {
            let reruns = self.state.rerun_of(&result.id, check.deciding().run_id);
            (check, verdict(&check.outcomes(), reruns.count))
        };
        checks.iter().map(judge).collect()
    }

    /// Posts one `flaky` report per check with the given report verdict.
    async fn report_flaky(
        &self,
        job: &Job,
        result: &ResultRecord,
        checks: &[&CheckAttempts],
        verdict: &str,
    ) {
        for check in checks {
            let report = flaky_report(job, result, check, verdict);
            self.report_best_effort(&job.project, report).await;
        }
    }

    /// Reruns the failed jobs of each deciding run, once per run, and
    /// records each request; a refused request skips the subject instead.
    async fn rerun(
        &mut self,
        job: &Job,
        result: &ResultRecord,
        checks: &[&CheckAttempts],
    ) -> Result<Step> {
        let deciding = checks.iter().map(|check| check.deciding());
        for (run_id, attempt) in latest_attempts(deciding) {
            let request = self.checks.rerun_failed_jobs(job.repo.as_ref(), run_id);
            if let Err(error) = request.await {
                return Ok(self.rerun_refused(job, result, run_id, &error).await);
            }
            self.state.record_rerun(&result.id, run_id, attempt)?;
        }
        Ok(Step::ChecksPending)
    }

    /// Logs and reports a refused rerun request and skips the subject for
    /// this cycle; the next cycle asks again.
    async fn rerun_refused(
        &self,
        job: &Job,
        result: &ResultRecord,
        run_id: i64,
        error: &anyhow::Error,
    ) -> Step {
        let reason = format!("rerun_refused: run {run_id} of R {}: {error:#}", result.r);
        eprintln!("agentc-integrator: {}: {reason}", job.project);
        let report = NewReport {
            kind: "flaky",
            dedupe_key: format!("{}:{run_id}:rerun_refused", result.id),
            task_id: Some(job.item.subject_task_id.clone()),
            submission_id: Some(result.submission_id.clone()),
            result_id: Some(result.id.clone()),
            details: json!({"verdict": "rerun_refused", "r": result.r, "run_id": run_id,
                "error": format!("{error:#}"), "blocks_subject": true}),
        };
        self.report_best_effort(&job.project, report).await;
        Step::Blocked(reason)
    }

    /// Revises the author when any reproduced failure passed on X; waits
    /// while X's run is going; otherwise reports target problems and skips.
    async fn attribute_reproduced(
        &self,
        job: &Job,
        result: &ResultRecord,
        reproduced: &[&CheckAttempts],
    ) -> Result<Step> {
        let (mut author, mut target, mut waiting) = (Vec::new(), Vec::new(), false);
        for check in reproduced {
            let x = self.target_state(job, &check.check).await?;
            match blame(&x) {
                Blame::Author => author.push(author_evidence(job, result, check, &x)),
                Blame::Wait => waiting = true,
                blame => target.push(self.target_failure(job, result, check, blame).await),
            }
        }
        if !author.is_empty() {
            let evidence = author.join("; ");
            return self
                .revise(job, "check_failed", &evidence, Some(&result.id))
                .await;
        }
        Ok(match waiting {
            true => Step::ChecksPending,
            false => Step::Blocked(target.join("; ")),
        })
    }

    /// The same check on the target tip X (one poll of X).
    async fn target_state(&self, job: &Job, check: &RosterCheck) -> Result<OnTarget> {
        let wanted = std::slice::from_ref(check);
        let runs = self
            .checks
            .check_runs(job.repo.as_ref(), &job.x, wanted)
            .await?;
        Ok(on_target(&runs, check, &job.x))
    }

    /// Reports a reproduced failure that is not the author's and returns
    /// the reason the subject is skipped.
    async fn target_failure(
        &self,
        job: &Job,
        result: &ResultRecord,
        check: &CheckAttempts,
        blame: Blame,
    ) -> String {
        let (verdict, on_target) = target_verdict(blame);
        let report = fix_target_report(job, result, check, verdict);
        self.report_best_effort(&job.project, report).await;
        format!(
            "{verdict}: {} failed on R {} and {on_target} on target {}",
            check.check.check_name, result.r, job.x
        )
    }
}

/// The checks with verdict `wanted`.
fn pick<'a>(judged: &Judged<'a>, wanted: Verdict) -> Vec<&'a CheckAttempts> {
    let matching = judged.iter().filter(|(_, v)| *v == wanted);
    matching.map(|(check, _)| *check).collect()
}

/// Why the subject is skipped when checks stay without a decision.
fn undecided_reason(result: &ResultRecord, checks: &[&CheckAttempts]) -> String {
    let names: Vec<&str> = checks
        .iter()
        .map(|check| check.check.check_name.as_str())
        .collect();
    format!(
        "no_result: {} on R {} after {MAX_RERUNS} reruns",
        names.join(", "),
        result.r
    )
}

/// The report verdict for a failure that is not the author's, and what the
/// check did on X.
fn target_verdict(blame: Blame) -> (&'static str, &'static str) {
    match blame {
        Blame::Unverified => ("target_unverified", "has no result"),
        _ => ("target_failing", "fails too"),
    }
}

/// The run id, attempt and conclusion of each run, for report details.
fn attempt_list<'a>(runs: impl Iterator<Item = &'a CheckRun>) -> Value {
    let list: Vec<Value> = runs
        .map(|run| {
            json!({"run_id": run.run_id, "run_attempt": run.run_attempt,
                "conclusion": run.conclusion})
        })
        .collect();
    Value::Array(list)
}

/// "run N attempt M" for evidence text.
fn run_label(run: &CheckRun) -> String {
    format!("run {} attempt {}", run.run_id, run.run_attempt)
}

/// The `check_failed` evidence for one check: the failed attempts on R that
/// reproduce it and the passing run on X.
fn author_evidence(
    job: &Job,
    result: &ResultRecord,
    check: &CheckAttempts,
    x: &OnTarget,
) -> String {
    let failed: Vec<String> = check.failures().map(run_label).collect();
    let passing = match x {
        OnTarget::Passed(run) => run_label(run),
        _ => String::new(),
    };
    format!(
        "{} failed on R {} ({}) and passed on target {} ({passing})",
        check.check.check_name,
        result.r,
        failed.join(", "),
        job.x
    )
}

/// A `flaky` report for one check of one result, with its attempts, keyed
/// by result, check and report verdict (`flaky` or `no_result`).
fn flaky_report(
    job: &Job,
    result: &ResultRecord,
    check: &CheckAttempts,
    verdict: &str,
) -> NewReport {
    let name = &check.check.check_name;
    NewReport {
        kind: "flaky",
        dedupe_key: format!("{}:{name}:{verdict}", result.id),
        task_id: Some(job.item.subject_task_id.clone()),
        submission_id: Some(result.submission_id.clone()),
        result_id: Some(result.id.clone()),
        details: json!({"verdict": verdict, "check_name": name,
            "identity": check.check.identity, "r": result.r,
            "attempts": attempt_list(check.attempts.iter()),
            "blocks_subject": verdict != "flaky"}),
    }
}

/// The `fix_target` report for one check on target tip X, keyed by target,
/// X, verdict and check so every subject blocked on it shares one report.
fn fix_target_report(
    job: &Job,
    result: &ResultRecord,
    check: &CheckAttempts,
    verdict: &str,
) -> NewReport {
    let target = job.target_key();
    let key = format!(
        "{}:{}:{verdict}:{}",
        short_digest(&target),
        job.x,
        check.check.check_name
    );
    let details = json!({"verdict": verdict, "repository_url": job.item.repository_url,
        "target_branch": job.item.target_branch, "x": job.x,
        "check_name": check.check.check_name, "identity": check.check.identity,
        "result_id": result.id, "r": result.r, "failed_on_r": attempt_list(check.failures()),
        "blocks_subject": true});
    target_report("fix_target", key, details)
}
