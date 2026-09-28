//! Where check results and branch rules come from. The loop is written
//! against [`ChecksSource`]; production uses GitHub Actions (job-level check
//! runs, p4-design finding 5) and staging/tests use a JSON file.
use crate::github::{GithubApp, RepoId};
use anyhow::{Context, Result};
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
    /// Latest-attempt runs on `sha` for the wanted checks (others may be omitted).
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
            "/repos/{}/{}/actions/runs?head_sha={sha}&per_page=100",
            repo.owner, repo.name
        );
        let runs = self.app.get(&path).await?;
        let mut out = Vec::new();
        for run in runs["workflow_runs"].as_array().into_iter().flatten() {
            out.extend(self.run_jobs(repo, run, sha).await?);
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
}

impl GithubChecks<'_> {
    /// The latest attempt's jobs of one workflow run, as check runs.
    async fn run_jobs(&self, repo: &RepoId, run: &Value, sha: &str) -> Result<Vec<CheckRun>> {
        let (Some(run_id), Some(workflow_path)) = (run["id"].as_i64(), run["path"].as_str()) else {
            return Ok(Vec::new());
        };
        let path = format!(
            "/repos/{}/{}/actions/runs/{run_id}/jobs?filter=latest&per_page=100",
            repo.owner, repo.name
        );
        let jobs = self.app.get(&path).await?;
        let jobs = jobs["jobs"].as_array().cloned().unwrap_or_default();
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

/// File-backed checks for staging and tests. The file is re-read on every
/// call so a soak script can change outcomes while the integrator runs:
/// `{"rules": {"main": ["non_fast_forward"]}, "runs": {"<sha>": [CheckRun]},
///   "default_conclusion": "success"}`. With a default conclusion, every
/// wanted check without an explicit run gets a synthetic completed run.
pub struct FakeChecks {
    pub path: PathBuf,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct FakeFile {
    pub rules: BTreeMap<String, Vec<String>>,
    pub runs: BTreeMap<String, Vec<CheckRun>>,
    pub default_conclusion: Option<String>,
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
        let Some(conclusion) = file.default_conclusion else {
            return Ok(runs);
        };
        for check in wanted {
            if !runs.iter().any(|run| run.check_name == check.check_name) {
                runs.push(synthetic_run(check, sha, &conclusion));
            }
        }
        Ok(runs)
    }

    async fn branch_rules(&self, _repo: Option<&RepoId>, branch: &str) -> Result<Vec<String>> {
        Ok(self.load()?.rules.get(branch).cloned().unwrap_or_default())
    }
}

/// A completed Actions-style run with a stable id derived from the SHA.
fn synthetic_run(check: &RosterCheck, sha: &str, conclusion: &str) -> CheckRun {
    let run_id = i64::from_str_radix(&sha[..8.min(sha.len())], 16)
        .unwrap_or(1)
        .max(1);
    CheckRun {
        check_name: check.check_name.clone(),
        run_id,
        run_attempt: 1,
        app_id: ACTIONS_APP_ID,
        head_sha: sha.to_owned(),
        workflow_path: check.workflow_path.clone(),
        conclusion: Some(conclusion.to_owned()),
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
