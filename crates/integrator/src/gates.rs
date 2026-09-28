//! Findings the integrator reports to the service (p4-design §3 steps 1, 2
//! and 4): the watchdog's `ruleset_missing` and the tip monitor's
//! `target_rewritten` freezes, and the `privilege_gate` decision (the tip
//! monitor's `unreviewed_landing` is built in `landing.rs`). Each
//! report carries a dedupe key, so repeating it every cycle stores one row.
//! Freezes stay local and unconditional; their reports are best effort. The
//! privilege gate is the one report the loop waits on: R is pushed only
//! after a human resolves its report with `allow`.
use crate::checks::ChecksSource;
use crate::integrate::{Integrator, Job, Step};
use crate::privilege::{self, Finding};
use crate::service::{NewReport, ResultRecord, Target};
use anyhow::Result;
use serde_json::json;
use sha2::{Digest, Sha256};

impl<C: ChecksSource> Integrator<C> {
    /// Watchdog freeze: reports the missing rules once per target, freeze
    /// episode and rule set, then freezes the target. The episode lasts
    /// until the rules are back, so a later loss of the same rules is a new
    /// report even after a human resolved the earlier one.
    pub(crate) async fn ruleset_frozen(
        &mut self,
        project: &str,
        target: &Target,
        missing: &[String],
    ) -> Result<Step> {
        let key = target.key();
        let details = json!({"repository_url": target.repository_url,
            "target_branch": target.target_branch, "missing_rules": missing});
        let episode = self.state.ruleset_episode(&key)?;
        let dedupe = format!("{}:{episode}:{}", short_digest(&key), missing.join(","));
        self.report_best_effort(project, target_report("ruleset_missing", dedupe, details))
            .await;
        Ok(Step::Frozen(format!(
            "ruleset_missing: {}",
            missing.join(", ")
        )))
    }

    /// Tip-monitor freeze: reports the rewrite once per target and recorded
    /// freeze (which names both tips), then stays frozen.
    pub(crate) async fn rewrite_frozen(
        &self,
        project: &str,
        target: &Target,
        reason: String,
    ) -> Step {
        let key = target.key();
        let recorded = self.state.freeze_reason(&key).unwrap_or(&reason);
        let details = json!({"repository_url": target.repository_url,
            "target_branch": target.target_branch, "reason": recorded});
        let dedupe = format!("{}:{}", short_digest(&key), short_digest(recorded));
        let report = target_report("target_rewritten", dedupe, details);
        self.report_best_effort(project, report).await;
        Step::Frozen(reason)
    }

    /// Posts a report whose outcome does not steer the loop; a failure is
    /// logged. True when the service stored or refused it (a refusal is
    /// final); false on any other failure (transport, or a non-refusal
    /// error status such as 400 or 500), so a caller can retry next cycle.
    pub(crate) async fn report_best_effort(&self, project: &str, report: NewReport) -> bool {
        let outcome = self.service.report(project, &report).await;
        let (failure, settled) = match outcome {
            Ok(Ok(_)) => return true,
            Ok(Err(refusal)) => (refusal.code, true),
            Err(error) => (format!("{error:#}"), false),
        };
        eprintln!(
            "agentc-integrator: {project}: {} report: {failure}",
            report.kind
        );
        settled
    }

    /// Step 4: `None` when R may proceed (it changes no gated path, or
    /// a human allowed this exact R); otherwise the step that skips it.
    pub(crate) async fn privilege_gate(
        &self,
        job: &Job,
        result: &ResultRecord,
    ) -> Result<Option<Step>> {
        let required = roster_paths(result);
        let found = privilege::findings(&job.mirror, &job.x, &result.r, &required)?;
        if found.is_empty() {
            return Ok(None);
        }
        let report = gate_report(job, result, &found);
        Ok(match self.service.report(&job.project, &report).await? {
            Ok(record) if record.allowed => None,
            Ok(record) if record.resolved_at.is_some() => Some(Step::Blocked(format!(
                "privilege_gate: a human denied R {} (report {})",
                result.r, record.id
            ))),
            Ok(record) => Some(Step::Blocked(format!(
                "privilege_gate: R {} awaits a human decision (report {})",
                result.r, record.id
            ))),
            Err(refusal) => Some(Step::Refused(refusal.code)),
        })
    }
}

/// A report about a target rather than one subject.
pub(crate) fn target_report(
    kind: &'static str,
    dedupe_key: String,
    details: serde_json::Value,
) -> NewReport {
    NewReport {
        kind,
        dedupe_key,
        task_id: None,
        submission_id: None,
        result_id: None,
        details,
    }
}

/// The `privilege_gate` report for one result, keyed by the result and its
/// R so the decision binds to exactly that commit.
fn gate_report(job: &Job, result: &ResultRecord, found: &[Finding]) -> NewReport {
    NewReport {
        kind: "privilege_gate",
        dedupe_key: format!("{}:{}", result.id, result.r),
        task_id: Some(job.item.subject_task_id.clone()),
        submission_id: Some(result.submission_id.clone()),
        result_id: Some(result.id.clone()),
        details: json!({"r": result.r, "t0": result.t0, "workflows": found}),
    }
}

/// The workflow paths of the result roster's required checks.
fn roster_paths(result: &ResultRecord) -> Vec<String> {
    let checks = result.roster["required_checks"].as_array();
    let paths = checks.into_iter().flatten();
    paths
        .filter_map(|check| check["workflow_path"].as_str().map(str::to_owned))
        .collect()
}

/// A short, stable digest for dedupe keys built from long or free text.
pub(crate) fn short_digest(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))[..16].to_owned()
}
