//! Pre-cutover observation. This path calls only service/forge reads and
//! local Git computation; it never enters the live cycle or publication path.
use crate::{checks::ChecksSource, git, github::RepoId, integrate::Integrator, privilege, roster};
use anyhow::{Context, Result};
use serde_json::{Value, json};

impl<C: ChecksSource> Integrator<C> {
    /// One shadow pass. Missing rules are evidence rather than a reason to
    /// stop computing R: the full rulesets are installed after the shadow day.
    pub async fn shadow_cycle(&self, project: &str) -> Result<Vec<Value>> {
        let queue = self
            .service
            .shadow_queue(project)
            .await?
            .map_err(|r| anyhow::anyhow!("shadow queue refused: {}", r.code))?;
        let mut records = Vec::new();
        for target in queue.all_targets() {
            let mirror = git::mirror_dir(&self.config.state_dir, &target.repository_url);
            git::ensure_mirror(&mirror, &target.repository_url)?;
            let reference = format!("refs/heads/{}", target.target_branch);
            let x = git::ls_remote(&mirror, &target.repository_url, &reference)?
                .context("shadow target does not exist")?;
            git::fetch(&mirror, &[format!("+{reference}:{reference}")])?;
            let repo = RepoId::from_url(&target.repository_url);
            let active = self
                .checks
                .branch_rules(repo.as_ref(), &target.target_branch)
                .await?;
            let missing: Vec<_> = self
                .config
                .required_rules
                .iter()
                .filter(|r| !active.contains(r))
                .collect();
            let roster = roster::at_target(&mirror, &x, &self.config.roster_path, &queue.roster)?;
            let base_runs = self
                .checks
                .check_runs(repo.as_ref(), &x, &roster.checks)
                .await?;
            records.push(
                json!({"time": chrono::Utc::now().to_rfc3339(), "project": project,
                "mode": "shadow", "step": "Target", "target": target.repository_url,
                "branch": target.target_branch, "t0": x, "missing_rules": missing,
                "checks": base_runs, "pending_reverts": queue.reverts.len()}),
            );
            for item in queue.items.iter().filter(|i| i.target() == target) {
                let job = self.open_job(project, item.clone(), x.clone())?;
                let record = if !git::merges_cleanly(&mirror, &x, &item.candidate_revision)? {
                    json!({"step": "WouldReviseConflict", "submission": item.submission_id, "t0": x})
                } else {
                    let result = self.compute_result(&job, &roster)?;
                    let required = roster
                        .checks
                        .iter()
                        .map(|c| c.workflow_path.clone())
                        .collect::<Vec<_>>();
                    let gates = privilege::findings(&mirror, &x, &result.r, &required)?;
                    let runs = self
                        .checks
                        .check_runs(repo.as_ref(), &result.r, &roster.checks)
                        .await?;
                    json!({"step": "WouldPush", "result": result, "privilege_findings": gates,
                        "checks": runs, "authority_verified": false})
                };
                let mut record = record;
                record["time"] = json!(chrono::Utc::now().to_rfc3339());
                record["project"] = json!(project);
                record["mode"] = json!("shadow");
                records.push(record);
            }
        }
        if records.is_empty() {
            records.push(json!({"time": chrono::Utc::now().to_rfc3339(),
                "project": project, "mode": "shadow", "step": "Idle"}));
        }
        Ok(records)
    }
}
