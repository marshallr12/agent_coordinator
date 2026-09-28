//! The pure part of flake attribution (p4-design §3 step 6; plan-final
//! "Reproducible flake attribution"): how a check-run conclusion counts,
//! what a check's attempts on R decide, and whom a reproduced failure is
//! attributed to given the same check on the target tip X. The loop that
//! acts on these verdicts is in `attribution.rs`.
use crate::checks::{ACTIONS_APP_ID, CheckRun, RosterCheck};
use std::collections::BTreeMap;

/// Reruns requested per (result, run) before a check that keeps ending
/// without a decision blocks its subject.
pub(crate) const MAX_RERUNS: u32 = 2;

/// How one completed conclusion counts, as for a GitHub required check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// `success`, `skipped` or `neutral`.
    Pass,
    /// `failure` or `timed_out`.
    Fail,
    /// `cancelled`, `stale`, `action_required` or anything else.
    NoResult,
}

/// Classifies a completed check-run conclusion.
pub(crate) fn outcome(conclusion: &str) -> Outcome {
    match conclusion {
        "success" | "skipped" | "neutral" => Outcome::Pass,
        "failure" | "timed_out" => Outcome::Fail,
        _ => Outcome::NoResult,
    }
}

/// The outcome of a run (`NoResult` for one still running).
pub(crate) fn run_outcome(run: &CheckRun) -> Outcome {
    run.conclusion.as_deref().map_or(Outcome::NoResult, outcome)
}

/// Every completed attempt of one roster check on a commit, oldest first.
#[derive(Debug, Clone)]
pub(crate) struct CheckAttempts {
    pub check: RosterCheck,
    pub attempts: Vec<CheckRun>,
}

impl CheckAttempts {
    /// The deciding run: the latest attempt of the latest run.
    pub(crate) fn deciding(&self) -> &CheckRun {
        self.attempts
            .last()
            .expect("a check has at least one attempt")
    }

    /// Each attempt's outcome, oldest first.
    pub(crate) fn outcomes(&self) -> Vec<Outcome> {
        self.attempts.iter().map(run_outcome).collect()
    }

    /// The attempts that failed.
    pub(crate) fn failures(&self) -> impl Iterator<Item = &CheckRun> {
        let attempts = self.attempts.iter();
        attempts.filter(|run| run_outcome(run) == Outcome::Fail)
    }
}

/// What one check's attempts on R say; the latest attempt decides, as it
/// does for push authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The latest attempt passed and no attempt failed.
    Passed,
    /// The latest attempt passed after an earlier failure.
    Flaky,
    /// Another attempt decides: the latest failed once, or had no result.
    Rerun,
    /// The latest attempt failed and at least two attempts failed.
    Reproduced,
    /// Reruns are used up and the latest attempt still decides nothing.
    Undecided,
}

/// The verdict over one check's attempt outcomes on R, oldest first, given
/// the reruns already requested for its deciding run.
pub(crate) fn verdict(outcomes: &[Outcome], reruns: u32) -> Verdict {
    let failures = outcomes.iter().filter(|o| **o == Outcome::Fail).count();
    let rerun = if reruns < MAX_RERUNS {
        Verdict::Rerun
    } else {
        Verdict::Undecided
    };
    match outcomes.last() {
        Some(Outcome::Pass) if failures == 0 => Verdict::Passed,
        Some(Outcome::Pass) => Verdict::Flaky,
        Some(Outcome::Fail) if failures >= 2 => Verdict::Reproduced,
        _ => rerun,
    }
}

/// The same check on the target tip X.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OnTarget {
    /// Its latest attempt with a pass or fail result passed.
    Passed(CheckRun),
    /// Its latest attempt with a pass or fail result failed.
    Failed(CheckRun),
    /// No attempt has a result yet, but one is still running.
    Running,
    /// No attempt exists, or none ended with a result.
    Unknown,
}

/// Who a reproduced failure on R is attributed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Blame {
    /// The check passed on X: the candidate broke it.
    Author,
    /// The check fails on X as well.
    Target,
    /// X's run has not finished; a later cycle decides.
    Wait,
    /// X has no result for the check.
    Unverified,
}

/// Attributes a reproduced failure from the same check on X.
pub(crate) fn blame(on_target: &OnTarget) -> Blame {
    match on_target {
        OnTarget::Passed(_) => Blame::Author,
        OnTarget::Failed(_) => Blame::Target,
        OnTarget::Running => Blame::Wait,
        OnTarget::Unknown => Blame::Unverified,
    }
}

/// Whether `run` is an Actions run of `check` (same job name and workflow
/// file) on `sha`.
fn is_run_of(run: &CheckRun, check: &RosterCheck, sha: &str) -> bool {
    run.check_name == check.check_name
        && run.workflow_path == check.workflow_path
        && run.head_sha == sha
        && run.app_id == ACTIONS_APP_ID
}

/// The runs of `check` on `sha`, oldest first, one per (run, attempt).
fn runs_of(runs: &[CheckRun], check: &RosterCheck, sha: &str) -> Vec<CheckRun> {
    let mut mine: Vec<CheckRun> = runs
        .iter()
        .filter(|run| is_run_of(run, check, sha))
        .cloned()
        .collect();
    mine.sort_by_key(|run| (run.run_id, run.run_attempt));
    mine.dedup_by_key(|run| (run.run_id, run.run_attempt));
    mine
}

/// The completed attempts of every wanted check on `sha`, or `None` while
/// any check has no run yet or its latest attempt is still running.
pub(crate) fn attempts_by_check(
    runs: &[CheckRun],
    wanted: &[RosterCheck],
    sha: &str,
) -> Option<Vec<CheckAttempts>> {
    let completed = |check: &RosterCheck| {
        let mut attempts = runs_of(runs, check, sha);
        attempts.last()?.conclusion.as_ref()?;
        attempts.retain(|run| run.conclusion.is_some());
        Some(CheckAttempts {
            check: check.clone(),
            attempts,
        })
    };
    wanted.iter().map(completed).collect()
}

/// `check` on `sha`: its latest attempt with a pass or fail result, else
/// whether one is still running.
pub(crate) fn on_target(runs: &[CheckRun], check: &RosterCheck, sha: &str) -> OnTarget {
    let mine = runs_of(runs, check, sha);
    let decided = mine
        .iter()
        .rev()
        .find(|run| run.conclusion.is_some() && run_outcome(run) != Outcome::NoResult);
    match decided {
        Some(run) if run_outcome(run) == Outcome::Pass => OnTarget::Passed(run.clone()),
        Some(run) => OnTarget::Failed(run.clone()),
        None if mine.iter().any(|run| run.conclusion.is_none()) => OnTarget::Running,
        None => OnTarget::Unknown,
    }
}

/// The highest attempt seen per run id among `runs`.
pub(crate) fn latest_attempts<'a>(runs: impl Iterator<Item = &'a CheckRun>) -> BTreeMap<i64, i64> {
    let mut latest = BTreeMap::new();
    for run in runs {
        let attempt = latest.entry(run.run_id).or_insert(run.run_attempt);
        *attempt = run.run_attempt.max(*attempt);
    }
    latest
}

#[cfg(test)]
mod tests {
    use super::*;
    use Outcome::{Fail, NoResult, Pass};

    const CI: &str = ".github/workflows/ci.yml";

    /// A run of check `name` in `path` on `r`.
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
    fn conclusions_count_like_github_required_checks() {
        for pass in ["success", "skipped", "neutral"] {
            assert_eq!(outcome(pass), Pass, "{pass}");
        }
        for fail in ["failure", "timed_out"] {
            assert_eq!(outcome(fail), Fail, "{fail}");
        }
        for none in ["cancelled", "stale", "action_required", "startup_failure"] {
            assert_eq!(outcome(none), NoResult, "{none}");
        }
    }

    #[test]
    fn the_latest_attempt_decides_and_failures_reproduce_at_two() {
        assert_eq!(verdict(&[Pass], 0), Verdict::Passed);
        assert_eq!(verdict(&[NoResult, Pass], 1), Verdict::Passed);
        assert_eq!(verdict(&[Fail, Pass], 1), Verdict::Flaky);
        assert_eq!(verdict(&[Fail, Fail, Pass], 2), Verdict::Flaky);
        assert_eq!(verdict(&[Fail], 0), Verdict::Rerun);
        assert_eq!(verdict(&[Fail, Fail], 1), Verdict::Reproduced);
        assert_eq!(verdict(&[Fail, NoResult, Fail], 2), Verdict::Reproduced);
        assert_eq!(verdict(&[Fail, NoResult], 1), Verdict::Rerun);
        assert_eq!(verdict(&[NoResult, NoResult], 1), Verdict::Rerun);
        assert_eq!(
            verdict(&[NoResult, NoResult, NoResult], 2),
            Verdict::Undecided
        );
        assert_eq!(verdict(&[NoResult, NoResult, Fail], 2), Verdict::Undecided);
    }

    #[test]
    fn only_a_pass_on_the_target_blames_the_author() {
        let passed = run("a", CI, 1, 1, Some("success"));
        assert_eq!(blame(&OnTarget::Passed(passed.clone())), Blame::Author);
        assert_eq!(blame(&OnTarget::Failed(passed)), Blame::Target);
        assert_eq!(blame(&OnTarget::Running), Blame::Wait);
        assert_eq!(blame(&OnTarget::Unknown), Blame::Unverified);
    }

    #[test]
    fn the_target_skips_attempts_without_a_result() {
        let check = wanted("a");
        let failed = run("a", CI, 1, 1, Some("failure"));
        let cancelled = run("a", CI, 1, 2, Some("cancelled"));
        let running = run("a", CI, 1, 3, None);
        let runs = [failed.clone(), cancelled.clone(), running.clone()];
        assert_eq!(on_target(&runs, &check, "r"), OnTarget::Failed(failed));
        let runs = [cancelled.clone(), running];
        assert_eq!(on_target(&runs, &check, "r"), OnTarget::Running);
        assert_eq!(on_target(&[cancelled], &check, "r"), OnTarget::Unknown);
        assert_eq!(on_target(&[], &check, "r"), OnTarget::Unknown);
    }

    #[test]
    fn attempts_of_the_roster_workflow_are_ordered_and_the_latest_decides() {
        let runs = [
            run("a", CI, 1, 2, Some("success")),
            run("a", CI, 1, 1, Some("failure")),
            run("a", CI, 1, 1, Some("failure")),
        ];
        let done = attempts_by_check(&runs, &[wanted("a")], "r").unwrap();
        assert_eq!(done[0].outcomes(), [Fail, Pass]);
        assert_eq!(done[0].deciding().run_attempt, 2);
        let pending = [
            run("a", CI, 1, 1, Some("failure")),
            run("a", CI, 1, 2, None),
        ];
        assert!(attempts_by_check(&pending, &[wanted("a")], "r").is_none());
        let elsewhere = [run(
            "a",
            ".github/workflows/other.yml",
            9,
            1,
            Some("success"),
        )];
        assert!(attempts_by_check(&elsewhere, &[wanted("a")], "r").is_none());
        assert_eq!(on_target(&elsewhere, &wanted("a"), "r"), OnTarget::Unknown);
    }
}
