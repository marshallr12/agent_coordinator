//! The pure part of flake attribution (p4-design §3 step 6; plan-final
//! "Reproducible flake attribution"): how a check-run conclusion counts,
//! what a check's attempts on R decide, and whom a reproduced failure is
//! attributed to given the same check on the target tip X, whose pass only
//! counts when it started after R's first failure (an older pass may predate
//! a change outside the commit). The loop that
//! acts on these verdicts is in `attribution.rs`.
use crate::checks::{ACTIONS_APP_ID, CheckRun, RosterCheck};
use chrono::{DateTime, Utc};
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

    /// When the earliest failed attempt started; `None` when no failure
    /// has a usable start time.
    pub(crate) fn first_failure_started(&self) -> Option<DateTime<Utc>> {
        self.failures()
            .filter_map(|run| run.started_at.as_deref().and_then(parse_time))
            .min()
    }

    /// The attempts that failed.
    pub(crate) fn failures(&self) -> impl Iterator<Item = &CheckRun> {
        let attempts = self.attempts.iter();
        attempts.filter(|run| run_outcome(run) == Outcome::Fail)
    }
}

/// An RFC 3339 time, or `None` when `text` is not one.
fn parse_time(text: &str) -> Option<DateTime<Utc>> {
    let time = DateTime::parse_from_rfc3339(text).ok()?;
    Some(time.with_timezone(&Utc))
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
    /// Its latest attempt with a pass or fail result passed, and started
    /// after R's first failure of the check.
    Passed(CheckRun),
    /// Its latest attempt with a pass or fail result passed, but started
    /// before R's first failure (or at an unknown time): data outside the
    /// commit may have changed since, so the pass proves nothing yet.
    /// `latest` is the newest attempt, which is no longer running.
    Stale { passed: CheckRun, latest: CheckRun },
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
    /// The check passed on X after R first failed: the candidate broke it.
    Author,
    /// The check passed on X only before R first failed: X needs a rerun.
    Refresh,
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
        OnTarget::Stale { .. } => Blame::Refresh,
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
/// whether one is still running. A pass counts only when it started after
/// `failed_since`, the start of R's first failure of the check; an older
/// pass is `Stale`, or `Running` while a newer attempt is going.
pub(crate) fn on_target(
    runs: &[CheckRun],
    check: &RosterCheck,
    sha: &str,
    failed_since: Option<DateTime<Utc>>,
) -> OnTarget {
    let mine = runs_of(runs, check, sha);
    let decided = mine
        .iter()
        .rev()
        .find(|run| run.conclusion.is_some() && run_outcome(run) != Outcome::NoResult);
    let fresh = |run: &CheckRun| {
        let started = run.started_at.as_deref().and_then(parse_time);
        started.zip(failed_since).is_some_and(|(s, f)| s > f)
    };
    match decided {
        Some(run) if run_outcome(run) == Outcome::Pass && fresh(run) => {
            OnTarget::Passed(run.clone())
        }
        Some(run) if run_outcome(run) == Outcome::Pass => match mine.last() {
            Some(latest) if latest.conclusion.is_some() => OnTarget::Stale {
                passed: run.clone(),
                latest: latest.clone(),
            },
            _ => OnTarget::Running,
        },
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
            job_id: None,
            started_at: None,
            completed_at: None,
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
        let stale = OnTarget::Stale {
            passed: run("a", CI, 1, 1, Some("success")),
            latest: run("a", CI, 1, 1, Some("success")),
        };
        assert_eq!(blame(&stale), Blame::Refresh);
        assert_eq!(blame(&OnTarget::Unknown), Blame::Unverified);
    }

    #[test]
    fn the_target_skips_attempts_without_a_result() {
        let check = wanted("a");
        let failed = run("a", CI, 1, 1, Some("failure"));
        let cancelled = run("a", CI, 1, 2, Some("cancelled"));
        let running = run("a", CI, 1, 3, None);
        let runs = [failed.clone(), cancelled.clone(), running.clone()];
        assert_eq!(
            on_target(&runs, &check, "r", None),
            OnTarget::Failed(failed)
        );
        let runs = [cancelled.clone(), running];
        assert_eq!(on_target(&runs, &check, "r", None), OnTarget::Running);
        let runs = [cancelled];
        assert_eq!(on_target(&runs, &check, "r", None), OnTarget::Unknown);
        assert_eq!(on_target(&[], &check, "r", None), OnTarget::Unknown);
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
        let target = on_target(&elsewhere, &wanted("a"), "r", None);
        assert_eq!(target, OnTarget::Unknown);
    }

    /// `run` started at `started` (RFC 3339).
    fn started(mut run: CheckRun, started: &str) -> CheckRun {
        run.started_at = Some(started.into());
        run
    }

    fn at(text: &str) -> Option<DateTime<Utc>> {
        parse_time(text)
    }

    #[test]
    fn a_pass_on_the_target_counts_only_when_it_started_after_the_failure() {
        let check = wanted("a");
        let since = at("2026-10-14T08:00:00Z");
        let old = started(run("a", CI, 1, 1, Some("success")), "2026-10-14T07:21:00Z");
        let target = on_target(std::slice::from_ref(&old), &check, "r", since);
        let stale = OnTarget::Stale {
            passed: old.clone(),
            latest: old.clone(),
        };
        assert_eq!(target, stale, "a pass before the failure is stale");
        let fresh = started(run("a", CI, 1, 2, Some("success")), "2026-10-14T09:00:00Z");
        let runs = [old.clone(), fresh.clone()];
        let target = on_target(&runs, &check, "r", since);
        assert_eq!(target, OnTarget::Passed(fresh), "the newer pass counts");
        let rerun = started(run("a", CI, 1, 2, None), "2026-10-14T09:00:00Z");
        let runs = [old.clone(), rerun];
        let target = on_target(&runs, &check, "r", since);
        assert_eq!(target, OnTarget::Running, "a rerun in progress is awaited");
        let failed = started(run("a", CI, 1, 2, Some("failure")), "2026-10-14T09:00:00Z");
        let runs = [old.clone(), failed.clone()];
        let target = on_target(&runs, &check, "r", since);
        assert_eq!(target, OnTarget::Failed(failed));
    }

    #[test]
    fn a_pass_without_a_usable_time_is_stale() {
        let check = wanted("a");
        let untimed = run("a", CI, 1, 1, Some("success"));
        let runs = std::slice::from_ref(&untimed);
        let since = at("2026-10-14T08:00:00Z");
        assert!(matches!(
            on_target(runs, &check, "r", since),
            OnTarget::Stale { .. }
        ));
        let timed = started(untimed, "2026-10-14T09:00:00Z");
        let runs = std::slice::from_ref(&timed);
        assert!(matches!(
            on_target(runs, &check, "r", None),
            OnTarget::Stale { .. }
        ));
        assert_eq!(parse_time("yesterday"), None);
    }

    #[test]
    fn the_first_failure_is_the_earliest_started_failed_attempt() {
        let attempts = vec![
            started(run("a", CI, 1, 2, Some("failure")), "2026-10-14T09:00:00Z"),
            started(run("a", CI, 1, 1, Some("failure")), "2026-10-14T08:00:00Z"),
            started(run("a", CI, 1, 3, Some("success")), "2026-10-14T07:00:00Z"),
        ];
        let check = CheckAttempts {
            check: wanted("a"),
            attempts,
        };
        assert_eq!(check.first_failure_started(), at("2026-10-14T08:00:00Z"));
    }
}
