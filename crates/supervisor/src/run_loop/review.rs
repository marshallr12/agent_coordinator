//! The reviewer verdict pipeline (autonomy plan §2.3 "Reviewers", M5; task
//! 6cf630c0). The supervisor claims the review `next` offers with the
//! reviewer principal, runs the reviewer launch, parses its final message,
//! refuses an approval without non-empty evidence for every acceptance
//! criterion, and posts the verdict with its `review_independence`. Any
//! failure after the claim releases the review, which queues it again.
use super::RunConfig;
use crate::profile::Harness;
use anyhow::{Context, Result, bail, ensure};
use coordinator_core::workflow::{ReviewFindingInput, ReviewInput};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use uuid::Uuid;

/// The reviewer role contract; [`render_prompt`] fills its placeholders.
pub const REVIEWER_CONTRACT: &str = include_str!("../../contracts/reviewer.md");
/// The independence every supervised review has (plan §2.3, decision U5): a
/// separate reviewer account in a fresh launch, context and session.
pub const INDEPENDENCE: &str = "distinct_launch";
/// The longest decision summary posted (the service accepts 16 KiB).
const MAX_SUMMARY_CHARS: usize = 12_000;
/// The most findings posted (the service accepts 100).
const MAX_FINDINGS: usize = 100;
/// The longest finding posted (the service accepts 8 KiB).
const MAX_FINDING_CHARS: usize = 4_000;

/// A review `next` offers (`claim_review`).
#[derive(Debug, Clone, PartialEq)]
pub struct Review {
    pub activity: String,
    pub subject: String,
    pub submission: String,
    pub title: String,
    pub project_policy_revision: u64,
    pub workflow_policy_revision: u64,
}

/// A claimed review: its attempt and what the reviewer judges.
#[derive(Debug, Clone, PartialEq)]
pub struct ReviewClaim {
    pub attempt: String,
    pub generation: u64,
    /// The coordinator session the claim, decision and launch belong to.
    pub session: Uuid,
    /// The subject task's acceptance criteria.
    pub criteria: Vec<String>,
    /// The immutable submission under review, as the service reports it.
    pub submission: Value,
}

/// The structured verdict a reviewer returns (`profile::review_schema`).
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Verdict {
    pub decision: String,
    #[serde(default)]
    pub summary: String,
    pub findings: Vec<String>,
    pub criteria_evidence: Vec<CriterionEvidence>,
    #[serde(default)]
    pub amendment_decision: Option<String>,
}

/// The reviewer's evidence for one acceptance criterion.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CriterionEvidence {
    pub criterion: String,
    pub evidence: String,
}

/// What the pipeline needs from the coordinator and the host.
pub trait ReviewDriver {
    /// `next` for the reviewer principal.
    fn next_review(&mut self) -> Result<Value>;
    /// Claims the review in a fresh reviewer-principal session.
    fn claim_review(&mut self, review: &Review) -> Result<ReviewClaim>;
    /// Runs the reviewer launch to its end and returns its final output:
    /// Codex's last-message file or Claude's event stream.
    fn run_reviewer(&mut self, review: &Review, claim: &ReviewClaim) -> Result<String>;
    /// Posts the decision with the reviewer principal.
    fn decide(&mut self, review: &Review, claim: &ReviewClaim, input: &ReviewInput) -> Result<()>;
    /// Releases the claimed review with a handoff; the service queues it again.
    fn release_review(&mut self, review: &Review, claim: &ReviewClaim, summary: &str)
    -> Result<()>;
    /// The failed-verdict counts this host keeps between polls.
    fn strikes(&mut self) -> &mut Strikes;
}

/// Consecutive failed verdicts per submission, kept in memory, so a broken
/// reviewer cannot burn launches on one submission forever.
#[derive(Debug, Default)]
pub struct Strikes(HashMap<String, u32>);

impl Strikes {
    /// The failed verdicts recorded for `submission` since its last success.
    pub fn count(&self, submission: &str) -> u32 {
        self.0.get(submission).copied().unwrap_or(0)
    }

    /// Counts one more failure for `submission`, or clears it on success.
    fn record(&mut self, submission: &str, failed: bool) {
        if failed {
            *self.0.entry(submission.to_owned()).or_default() += 1;
        } else {
            self.0.remove(submission);
        }
    }
}

/// What one review poll did.
#[derive(Debug, Clone, PartialEq)]
pub enum ReviewOutcome {
    Idle,
    Decided {
        activity: String,
        decision: String,
    },
    Requeued {
        activity: String,
        reason: String,
    },
    /// The submission's verdicts failed `failures` times in a row; it is no
    /// longer claimed until the loop restarts.
    Skipped {
        submission: String,
        failures: u32,
    },
    Failed(String),
}

/// One review poll: claim the offered review, launch the reviewer, and post
/// its verdict, or release the review when the verdict cannot be posted.
/// After `settings.review_attempts` failed verdicts in a row a submission is
/// skipped instead.
pub fn review(driver: &mut dyn ReviewDriver, settings: &RunConfig) -> ReviewOutcome {
    let next = match driver.next_review() {
        Ok(next) => next,
        Err(error) => return ReviewOutcome::Failed(format!("reviewer next: {error:#}")),
    };
    let Some(review) = offered(&next) else {
        return ReviewOutcome::Idle;
    };
    let failures = driver.strikes().count(&review.submission);
    if failures >= settings.review_attempts {
        let submission = review.submission;
        return ReviewOutcome::Skipped {
            submission,
            failures,
        };
    }
    let outcome = claim_and_judge(driver, settings.harness, review.clone());
    let failed = matches!(outcome, ReviewOutcome::Requeued { .. });
    driver.strikes().record(&review.submission, failed);
    outcome
}

/// Claims `review`, then posts its verdict or releases it.
fn claim_and_judge(
    driver: &mut dyn ReviewDriver,
    harness: Harness,
    review: Review,
) -> ReviewOutcome {
    let claim = match driver.claim_review(&review) {
        Ok(claim) => claim,
        Err(error) => {
            return ReviewOutcome::Failed(format!("claim {}: {error:#}", review.activity));
        }
    };
    match judge(driver, harness, &review, &claim) {
        Ok(decision) => ReviewOutcome::Decided {
            activity: review.activity,
            decision,
        },
        Err(error) => requeue(driver, &review, &claim, &error),
    }
}

/// Runs the reviewer, checks its verdict and posts it; returns the decision.
fn judge(
    driver: &mut dyn ReviewDriver,
    harness: Harness,
    review: &Review,
    claim: &ReviewClaim,
) -> Result<String> {
    let output = driver.run_reviewer(review, claim)?;
    let verdict = final_verdict(harness, &output)?;
    verdict.check(claim)?;
    let input = verdict.input(claim, &review.submission)?;
    driver
        .decide(review, claim, &input)
        .context("post the decision")?;
    Ok(input.decision)
}

/// Releases a review whose verdict was not posted, so it is queued again.
fn requeue(
    driver: &mut dyn ReviewDriver,
    review: &Review,
    claim: &ReviewClaim,
    error: &anyhow::Error,
) -> ReviewOutcome {
    let reason = format!("{error:#}");
    let summary =
        format!("agentc-supervisor posted no verdict: {reason}. The review is queued again.");
    let summary: String = summary.chars().take(4000).collect();
    match driver.release_review(review, claim, &summary) {
        Ok(()) => ReviewOutcome::Requeued {
            activity: review.activity.clone(),
            reason,
        },
        Err(release) => ReviewOutcome::Failed(format!(
            "release {} after {reason}: {release:#}",
            review.activity
        )),
    }
}

/// The review to claim from a reviewer `next` response, if it offers one.
pub fn offered(next: &Value) -> Option<Review> {
    let action = next.get("action").filter(|a| a["kind"] == "claim_review")?;
    let body = &action["call"]["body"];
    let text = |value: &Value| value.as_str().map(str::to_owned);
    Some(Review {
        activity: text(&action["activity_id"])?,
        subject: text(&action["subject_task_id"])?,
        submission: text(&body["expected_submission_id"])?,
        title: text(&action["title"]).unwrap_or_default(),
        project_policy_revision: body["expected_project_policy_revision"].as_u64()?,
        workflow_policy_revision: body["expected_workflow_policy_revision"].as_u64()?,
    })
}

/// The verdict in a launch's final output. Codex's last-message file is the
/// JSON itself; Claude's event stream ends with a `result` event whose
/// `structured_output` (or, failing that, `result` text) holds it.
pub fn final_verdict(harness: Harness, output: &str) -> Result<Verdict> {
    let value = match harness {
        Harness::Codex => {
            serde_json::from_str(output.trim()).context("the final message is not JSON")?
        }
        Harness::Claude => claude_result(output)?,
    };
    serde_json::from_value(value).context("the final message is not a verdict")
}

/// The verdict object of the last `result` event in a Claude event stream.
fn claude_result(events: &str) -> Result<Value> {
    let result = (events.lines().rev())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|event| event["type"] == "result")
        .context("the launch wrote no result event")?;
    if let Some(output) = result.get("structured_output").filter(|v| v.is_object()) {
        return Ok(output.clone());
    }
    let text = result["result"]
        .as_str()
        .context("the result event holds no verdict")?;
    serde_json::from_str(text.trim()).context("the result text is not JSON")
}

impl Verdict {
    /// M5: an approval must carry non-empty evidence for every criterion it
    /// is judged on, and no entry may be empty.
    pub fn check(&self, claim: &ReviewClaim) -> Result<()> {
        if self.decision != "approve" {
            return Ok(());
        }
        let evidence = &self.criteria_evidence;
        ensure!(
            !evidence.is_empty(),
            "the approval has no criteria_evidence"
        );
        let empty = evidence
            .iter()
            .find(|e| blank(&e.criterion) || blank(&e.evidence));
        ensure!(
            empty.is_none(),
            "the approval has an empty criteria_evidence entry"
        );
        ensure!(
            !amended(claim) || self.amendment_decision.is_some(),
            "the approval does not decide the submission's ac_amendment"
        );
        for criterion in self.judged_criteria(claim) {
            ensure!(
                evidence.iter().any(|e| same(&e.criterion, &criterion)),
                "the approval has no evidence for the criterion {criterion:?}"
            );
        }
        Ok(())
    }

    /// The criteria the approval answers for: the amended ones when it
    /// accepts the submission's `ac_amendment`, else the task's own.
    fn judged_criteria(&self, claim: &ReviewClaim) -> Vec<String> {
        let amended = &claim.submission["ac_amendment"]["new"];
        match (self.amendment_decision.as_deref(), amended.as_array()) {
            (Some("accepted"), Some(new)) => strings(new),
            _ => claim.criteria.clone(),
        }
    }

    /// The service's review input: findings block an approval only when
    /// changes are requested, the evidence is kept in the summary, and the
    /// review's independence is recorded.
    pub fn input(&self, claim: &ReviewClaim, submission: &str) -> Result<ReviewInput> {
        let (decision, severity) = match self.decision.as_str() {
            "approve" => ("approved", "advisory"),
            "request_changes" => ("changes_requested", "required"),
            other => bail!("unknown decision {other:?}"),
        };
        let findings = (self
            .findings
            .iter()
            .filter(|f| !blank(f))
            .take(MAX_FINDINGS))
        .map(|remedy| ReviewFindingInput {
            severity: severity.into(),
            remedy: remedy.chars().take(MAX_FINDING_CHARS).collect(),
            evidence: String::new(),
        });
        Ok(ReviewInput {
            generation: i64::try_from(claim.generation)?,
            submission_id: submission.into(),
            decision: decision.into(),
            summary: self.summary_text(),
            findings: findings.collect(),
            amendment_decision: self.amendment(claim),
            review_independence: Some(INDEPENDENCE.into()),
        })
    }

    /// The `amendment_decision` the service accepts: none without an
    /// `ac_amendment`, and never `accepted` with requested changes. Requested
    /// changes leave the amendment undecided (the service refuses `accepted`
    /// there); a rejection is kept, since it requests changes either way.
    fn amendment(&self, claim: &ReviewClaim) -> Option<String> {
        let decision = self.amendment_decision.as_deref();
        match (amended(claim), self.decision.as_str(), decision) {
            (false, _, _) | (true, "request_changes", Some("accepted")) => None,
            _ => self.amendment_decision.clone(),
        }
    }

    /// The reviewer's summary followed by its per-criterion evidence.
    fn summary_text(&self) -> String {
        let mut text = match self.summary.trim() {
            "" => "Supervised review.".to_owned(),
            summary => summary.to_owned(),
        };
        if !self.criteria_evidence.is_empty() {
            text.push_str("\n\nCriteria evidence:");
        }
        for entry in &self.criteria_evidence {
            text.push_str(&format!(
                "\n- {}: {}",
                entry.criterion.trim(),
                entry.evidence.trim()
            ));
        }
        text.chars().take(MAX_SUMMARY_CHARS).collect()
    }
}

/// The reviewer prompt: the contract with the review filled in, the
/// submission as JSON data whose `<` are escaped (so the data holds no tag),
/// then the base revision's instruction files as in the implementer prompt.
pub fn render_prompt(
    project: &str,
    review: &Review,
    claim: &ReviewClaim,
    files: &[(String, String)],
) -> String {
    let revision = |name: &str| claim.submission[name].as_str().unwrap_or("HEAD").to_owned();
    let mut prompt = REVIEWER_CONTRACT
        .replace("{{project}}", project)
        .replace(
            "{{task_title}}",
            &super::defuse(&review.title.replace(['\n', '\r'], " ")),
        )
        .replace("{{base}}", &revision("base_revision"))
        .replace("{{candidate}}", &revision("candidate_revision"));
    let data =
        serde_json::json!({"acceptance_criteria": claim.criteria, "submission": claim.submission});
    let data = serde_json::to_string_pretty(&data)
        .unwrap_or_default()
        .replace('<', "\\u003c");
    prompt.push_str(&format!("\n<review-data>\n{data}\n</review-data>\n"));
    prompt.push_str(&super::instruction_blocks(files));
    prompt
}

/// Whether the claimed submission carries an `ac_amendment`.
fn amended(claim: &ReviewClaim) -> bool {
    claim.submission["ac_amendment"].is_object()
}

/// The strings in a JSON array.
pub fn strings(values: &[Value]) -> Vec<String> {
    values
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}

/// True when `text` holds nothing but whitespace.
fn blank(text: &str) -> bool {
    text.trim().is_empty()
}

/// Whether two criteria are the same text, ignoring whitespace runs.
fn same(a: &str, b: &str) -> bool {
    a.split_whitespace().eq(b.split_whitespace())
}

#[cfg(test)]
mod tests;
