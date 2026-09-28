//! Where check results and branch rules come from, and how failed jobs are
//! rerun. The loop is written against [`ChecksSource`]; production uses
//! GitHub Actions (job-level check runs of every attempt, p4-design finding
//! 5) and staging/tests use a JSON file.
use crate::flake::{Outcome, outcome};
use crate::github::{GithubApp, RepoId};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;

/// GitHub Actions' App id; check runs from any other App are ignored.
pub const ACTIONS_APP_ID: i64 = 15368;

/// A required check as the result roster names it.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct RosterCheck {
    pub identity: String,
    pub check_name: String,
    pub workflow_path: String,
}

/// One job-level run of a check on a commit.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct CheckRun {
    pub check_name: String,
    pub run_id: i64,
    pub run_attempt: i64,
    pub app_id: i64,
    pub head_sha: String,
    pub workflow_path: String,
    /// `None` while the run is queued or in progress.
    pub conclusion: Option<String>,
}

/// Source of check runs and branch rules for one forge.
pub trait ChecksSource {
    /// Runs of every attempt on `sha` for the wanted checks (others may be
    /// omitted); a rerun attempt is a separate run with the same `run_id`.
    fn check_runs(
        &self,
        repo: Option<&RepoId>,
        sha: &str,
        wanted: &[RosterCheck],
    ) -> impl Future<Output = Result<Vec<CheckRun>>> + Send;

    /// Rule types active on `branch` (e.g. `non_fast_forward`).
    fn branch_rules(
        &self,
        repo: Option<&RepoId>,
        branch: &str,
    ) -> impl Future<Output = Result<Vec<String>>> + Send;

    /// Starts a new attempt of workflow run `run_id` that reruns its failed jobs.
    fn rerun_failed_jobs(
        &self,
        repo: Option<&RepoId>,
        run_id: i64,
    ) -> impl Future<Output = Result<()>> + Send;
}

/// GitHub Actions through the integrator's App installation.
pub struct GithubChecks<'a> {
    pub app: &'a GithubApp,
}

impl ChecksSource for GithubChecks<'_> {
    async fn check_runs(
        &self,
        repo: Option<&RepoId>,
        sha: &str,
        _wanted: &[RosterCheck],
    ) -> Result<Vec<CheckRun>> {
        let repo = repo.context("GitHub checks need a github.com repository URL")?;
        let path = format!(
            "/repos/{}/{}/actions/runs?head_sha={sha}",
            repo.owner, repo.name
        );
        let mut out = Vec::new();
        for run in self.paged(&path, "workflow_runs").await? {
            out.extend(self.run_jobs(repo, &run, sha).await?);
        }
        Ok(out)
    }

    async fn branch_rules(&self, repo: Option<&RepoId>, branch: &str) -> Result<Vec<String>> {
        let repo = repo.context("GitHub rules need a github.com repository URL")?;
        let path = format!(
            "/repos/{}/{}/rules/branches/{branch}",
            repo.owner, repo.name
        );
        let rules = self.app.get(&path).await?;
        let types = rules.as_array().into_iter().flatten();
        Ok(types
            .filter_map(|rule| rule["type"].as_str().map(str::to_owned))
            .collect())
    }

    /// `POST …/actions/runs/{run_id}/rerun-failed-jobs`; the App needs the
    /// `actions: write` permission for it.
    async fn rerun_failed_jobs(&self, repo: Option<&RepoId>, run_id: i64) -> Result<()> {
        let repo = repo.context("GitHub reruns need a github.com repository URL")?;
        let path = format!(
            "/repos/{}/{}/actions/runs/{run_id}/rerun-failed-jobs",
            repo.owner, repo.name
        );
        self.app.post(&path).await.map(|_| ())
    }
}

impl GithubChecks<'_> {
    /// Every entry of the list `key` across the pages of `path` (which
    /// already has a query string), until a page comes back short.
    async fn paged(&self, path: &str, key: &str) -> Result<Vec<Value>> {
        let mut all = Vec::new();
        for page in 1..=MAX_PAGES {
            let reply = self
                .app
                .get(&format!("{path}&per_page={PER_PAGE}&page={page}"))
                .await?;
            let entries = reply[key].as_array().cloned().unwrap_or_default();
            let short = entries.len() < PER_PAGE;
            all.extend(entries);
            if short {
                break;
            }
        }
        Ok(all)
    }

    /// The jobs of every attempt of one workflow run, as check runs.
    async fn run_jobs(&self, repo: &RepoId, run: &Value, sha: &str) -> Result<Vec<CheckRun>> {
        let (Some(run_id), Some(workflow_path)) = (run["id"].as_i64(), run["path"].as_str()) else {
            return Ok(Vec::new());
        };
        let path = format!(
            "/repos/{}/{}/actions/runs/{run_id}/jobs?filter=all",
            repo.owner, repo.name
        );
        let jobs = self.paged(&path, "jobs").await?;
        Ok(jobs
            .iter()
            .filter_map(|job| job_run(job, run_id, workflow_path, sha))
            .collect())
    }
}

/// Converts one Actions job into a check run; `None` for malformed entries.
fn job_run(job: &Value, run_id: i64, workflow_path: &str, sha: &str) -> Option<CheckRun> {
    let completed = job["status"].as_str() == Some("completed");
    Some(CheckRun {
        check_name: job["name"].as_str()?.to_owned(),
        run_id,
        run_attempt: job["run_attempt"].as_i64().unwrap_or(1),
        app_id: ACTIONS_APP_ID,
        head_sha: job["head_sha"].as_str().unwrap_or(sha).to_owned(),
        workflow_path: workflow_path.split('@').next()?.to_owned(),
        conclusion: completed
            .then(|| job["conclusion"].as_str().map(str::to_owned))
            .flatten(),
    })
}

/// Workflow runs or jobs requested per GitHub page.
const PER_PAGE: usize = 100;
/// Pages read per listing at most (`MAX_PAGES` × `PER_PAGE` entries).
const MAX_PAGES: usize = 100;

/// File-backed checks for staging and tests. The file is re-read on every
/// call so a soak script can change outcomes while the integrator runs:
/// `{"rules": {"main": ["non_fast_forward"]}, "runs": {"<sha>": [CheckRun]},
///   "scripts": {"<sha>": {"<check name>": ["failure", "success"]}},
///   "reruns": {"<run id>": 1}, "default_conclusion": "success"}`.
/// Explicit `runs` win. Otherwise every scripted check, and with a default
/// conclusion every other wanted check, is a job of one synthetic run per
/// SHA (its id derives from the SHA) that models `rerun-failed-jobs`: each
/// rerun adds an attempt in which a job whose latest attempt passed is
/// copied with its conclusion and every other job takes its next script
/// entry (`null` = in progress; the default conclusion never runs out). The
/// attempt appears only once every rerun job has an entry for it. A rerun
/// of a run whose jobs all passed, or any rerun while `rerun_error` is set,
/// is refused; an accepted rerun is recorded in the file.
pub struct FakeChecks {
    pub path: PathBuf,
}

/// The fake checks file's contents.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct FakeFile {
    pub rules: BTreeMap<String, Vec<String>>,
    pub runs: BTreeMap<String, Vec<CheckRun>>,
    /// Per-attempt conclusions per SHA and check name, oldest first.
    pub scripts: BTreeMap<String, BTreeMap<String, Vec<Option<String>>>>,
    /// Reruns requested per synthetic run id.
    pub reruns: BTreeMap<i64, i64>,
    /// When set, every rerun request fails with this message.
    pub rerun_error: Option<String>,
    pub default_conclusion: Option<String>,
}

/// One job of a synthetic run while its attempts are simulated.
struct FakeJob {
    name: String,
    conclusions: Vec<Option<String>>,
    used: usize,
}

impl FakeChecks {
    /// The current file contents; a missing file means "no rules, no runs".
    fn load(&self) -> Result<FakeFile> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("parse {}", self.path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(FakeFile::default()),
            Err(error) => Err(error).with_context(|| format!("read {}", self.path.display())),
        }
    }

    /// Replaces the file's contents.
    fn store(&self, file: &FakeFile) -> Result<()> {
        let text = serde_json::to_string_pretty(file)?;
        std::fs::write(&self.path, text).with_context(|| format!("write {}", self.path.display()))
    }
}

impl ChecksSource for FakeChecks {
    async fn check_runs(
        &self,
        _repo: Option<&RepoId>,
        sha: &str,
        wanted: &[RosterCheck],
    ) -> Result<Vec<CheckRun>> {
        let file = self.load()?;
        let mut runs = file.runs.get(sha).cloned().unwrap_or_default();
        let names = wanted.iter().map(|check| check.check_name.clone());
        let jobs = file.simulate(sha, names.collect());
        for check in wanted {
            if runs.iter().any(|run| run.check_name == check.check_name) {
                continue;
            }
            let job = jobs.iter().find(|job| job.name == check.check_name);
            runs.extend(job.into_iter().flat_map(|job| job.runs(check, sha)));
        }
        Ok(runs)
    }

    async fn branch_rules(&self, _repo: Option<&RepoId>, branch: &str) -> Result<Vec<String>> {
        Ok(self.load()?.rules.get(branch).cloned().unwrap_or_default())
    }

    async fn rerun_failed_jobs(&self, _repo: Option<&RepoId>, run_id: i64) -> Result<()> {
        let mut file = self.load()?;
        if let Some(error) = &file.rerun_error {
            bail!("rerun refused: {error}");
        }
        ensure!(!file.all_passed(run_id), "run {run_id} has no failed jobs");
        *file.reruns.entry(run_id).or_default() += 1;
        self.store(&file)
    }
}

impl FakeFile {
    /// The jobs of the synthetic run on `sha`: the scripted checks and,
    /// with a default conclusion, `names`, each with its visible attempts.
    fn simulate(&self, sha: &str, names: Vec<String>) -> Vec<FakeJob> {
        let names = self.job_names(sha, names).into_iter();
        let mut jobs: Vec<FakeJob> = names.filter_map(|n| self.first(sha, n)).collect();
        let run_id = synthetic_run_id(sha);
        let reruns = self.reruns.get(&run_id).copied().unwrap_or(0);
        for _ in 0..reruns {
            if !self.advance(sha, &mut jobs) {
                break;
            }
        }
        jobs
    }

    /// The scripted checks on `sha` and, with a default conclusion, `names`.
    fn job_names(&self, sha: &str, mut names: Vec<String>) -> Vec<String> {
        let scripted = self.scripts.get(sha).into_iter().flat_map(|s| s.keys());
        names.retain(|_| self.default_conclusion.is_some());
        names.extend(scripted.cloned());
        names.sort();
        names.dedup();
        names
    }

    /// Adds the next attempt to every job; false (and no change) when some
    /// rerun job has no entry for it yet.
    fn advance(&self, sha: &str, jobs: &mut [FakeJob]) -> bool {
        let next: Option<Vec<(Option<String>, usize)>> =
            jobs.iter().map(|job| self.next(sha, job)).collect();
        let Some(next) = next else { return false };
        for (job, (conclusion, used)) in jobs.iter_mut().zip(next) {
            job.conclusions.push(conclusion);
            job.used += used;
        }
        true
    }

    /// Script entry `index` of check `name` on `sha`, or the default.
    fn entry(&self, sha: &str, name: &str, index: usize) -> Option<Option<String>> {
        match self.scripts.get(sha).and_then(|s| s.get(name)) {
            Some(script) => script.get(index).cloned(),
            None => self.default_conclusion.clone().map(Some),
        }
    }

    /// Job `name`'s first attempt, if it has one.
    fn first(&self, sha: &str, name: String) -> Option<FakeJob> {
        let conclusion = self.entry(sha, &name, 0)?;
        Some(FakeJob {
            name,
            conclusions: vec![conclusion],
            used: 1,
        })
    }

    /// A job's conclusion in the next attempt and the script entries it
    /// uses: a passed job is copied, any other takes its next entry.
    fn next(&self, sha: &str, job: &FakeJob) -> Option<(Option<String>, usize)> {
        match job.conclusions.last() {
            Some(Some(c)) if outcome(c) == Outcome::Pass => Some((Some(c.clone()), 0)),
            _ => self.entry(sha, &job.name, job.used).map(|entry| (entry, 1)),
        }
    }

    /// True when the scripted run `run_id` exists and all its jobs' latest
    /// attempts passed (GitHub then refuses `rerun-failed-jobs`).
    fn all_passed(&self, run_id: i64) -> bool {
        let Some(sha) = self
            .scripts
            .keys()
            .find(|sha| synthetic_run_id(sha) == run_id)
        else {
            return false;
        };
        let jobs = self.simulate(sha, Vec::new());
        let passed = |job: &FakeJob| {
            let last = job.conclusions.last().cloned().flatten();
            last.is_some_and(|c| outcome(&c) == Outcome::Pass)
        };
        !jobs.is_empty() && jobs.iter().all(passed)
    }
}

impl FakeJob {
    /// This job's attempts as check runs of `check` on `sha`.
    fn runs<'a>(
        &'a self,
        check: &'a RosterCheck,
        sha: &'a str,
    ) -> impl Iterator<Item = CheckRun> + 'a {
        let conclusions = self.conclusions.iter().cloned();
        (1..)
            .zip(conclusions)
            .map(move |(attempt, c)| synthetic_run(check, sha, attempt, c))
    }
}

/// A stable run id derived from the SHA, shared by all checks on it.
fn synthetic_run_id(sha: &str) -> i64 {
    i64::from_str_radix(&sha[..8.min(sha.len())], 16)
        .unwrap_or(1)
        .max(1)
}

/// One Actions-style attempt of `check` on `sha` (`None` = in progress).
fn synthetic_run(
    check: &RosterCheck,
    sha: &str,
    run_attempt: i64,
    conclusion: Option<String>,
) -> CheckRun {
    CheckRun {
        check_name: check.check_name.clone(),
        run_id: synthetic_run_id(sha),
        run_attempt,
        app_id: ACTIONS_APP_ID,
        head_sha: sha.to_owned(),
        workflow_path: check.workflow_path.clone(),
        conclusion,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn wanted() -> Vec<RosterCheck> {
        vec![RosterCheck {
            identity: "tests".into(),
            check_name: "Linux tests".into(),
            workflow_path: ".github/workflows/ci.yml".into(),
        }]
    }

    #[tokio::test]
    async fn fake_synthesises_default_runs_and_reads_rules() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fake.json");
        let body =
            json!({"rules": {"main": ["non_fast_forward"]}, "default_conclusion": "success"});
        std::fs::write(&path, body.to_string()).unwrap();
        let fake = FakeChecks { path };
        let runs = fake
            .check_runs(None, "abcdef0123", &wanted())
            .await
            .unwrap();
        assert_eq!(runs[0].conclusion.as_deref(), Some("success"));
        assert_eq!(runs[0].run_id, 0xabcdef01);
        assert_eq!(
            fake.branch_rules(None, "main").await.unwrap(),
            ["non_fast_forward"]
        );
    }

    /// "Linux tests" and "Lint" of `ci.yml`.
    fn two_jobs() -> Vec<RosterCheck> {
        let mut jobs = wanted();
        jobs.push(RosterCheck {
            identity: "lint".into(),
            check_name: "Lint".into(),
            workflow_path: ".github/workflows/ci.yml".into(),
        });
        jobs
    }

    /// (check name, attempt, conclusion) of every run the fake shows.
    async fn shown(fake: &FakeChecks) -> Vec<(String, i64, Option<String>)> {
        let runs = fake.check_runs(None, "abcdef0123", &two_jobs()).await;
        let runs = runs.unwrap().into_iter();
        runs.map(|run| (run.check_name, run.run_attempt, run.conclusion))
            .collect()
    }

    #[tokio::test]
    async fn fake_reruns_rerun_failed_jobs_and_copy_passed_ones() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fake.json");
        let script = json!({"abcdef0123": {"Linux tests": ["failure"], "Lint": ["success"]}});
        std::fs::write(&path, json!({"scripts": script}).to_string()).unwrap();
        let fake = FakeChecks { path: path.clone() };
        fake.rerun_failed_jobs(None, 0xabcdef01).await.unwrap();
        assert_eq!(shown(&fake).await.len(), 2, "no entry yet: no new attempt");
        let script =
            json!({"abcdef0123": {"Linux tests": ["failure", "success"], "Lint": ["success"]}});
        let mut file = fake.load().unwrap();
        file.scripts = serde_json::from_value(script).unwrap();
        fake.store(&file).unwrap();
        let success = Some("success".to_owned());
        let second: Vec<_> = shown(&fake)
            .await
            .into_iter()
            .filter(|r| r.1 == 2)
            .collect();
        assert_eq!(
            second,
            [
                ("Linux tests".into(), 2, success.clone()),
                ("Lint".into(), 2, success)
            ]
        );
        let refused = fake.rerun_failed_jobs(None, 0xabcdef01).await;
        assert!(refused.is_err(), "a run with no failed jobs is not rerun");
    }

    #[tokio::test]
    async fn missing_fake_file_has_no_rules_or_runs() {
        let fake = FakeChecks {
            path: PathBuf::from("/nonexistent/fake.json"),
        };
        assert!(
            fake.check_runs(None, "ab", &wanted())
                .await
                .unwrap()
                .is_empty()
        );
        assert!(fake.branch_rules(None, "main").await.unwrap().is_empty());
    }

    #[test]
    fn jobs_in_progress_have_no_conclusion() {
        let job = json!({"name": "Linux tests", "status": "in_progress", "conclusion": null, "run_attempt": 2});
        let run = job_run(&job, 7, ".github/workflows/ci.yml@refs/heads/x", "ab").unwrap();
        assert_eq!((run.conclusion, run.run_attempt), (None, 2));
        assert_eq!(run.workflow_path, ".github/workflows/ci.yml");
    }
}
