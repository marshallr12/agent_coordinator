use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RequiredCheck {
    pub identity: String,
    pub version: String,
    pub environment: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowPolicyInput {
    pub expected_revision: i64,
    pub canonical_repository_key: String,
    pub required_checks: Vec<RequiredCheck>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptanceEvidence {
    pub criterion: String,
    pub evidence: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SubmissionInput {
    pub generation: i64,
    pub task_revision: i64,
    pub project_policy_revision: i64,
    pub workflow_policy_revision: i64,
    pub kind: String,
    pub summary: String,
    pub acceptance_evidence: Vec<AcceptanceEvidence>,
    pub handoff: String,
    pub repository: Option<String>,
    pub base_revision: Option<String>,
    pub candidate_revision: Option<String>,
    pub candidate_tree: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityClaimInput {
    pub expected_submission_id: String,
    pub expected_project_policy_revision: i64,
    pub expected_workflow_policy_revision: i64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewFindingInput {
    pub severity: String,
    pub remedy: String,
    #[serde(default)]
    pub evidence: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewInput {
    pub generation: i64,
    pub submission_id: String,
    pub decision: String,
    pub summary: String,
    #[serde(default)]
    pub findings: Vec<ReviewFindingInput>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationAuthorizationInput {
    pub submission_id: String,
    pub expected_project_policy_revision: i64,
    pub expected_workflow_policy_revision: i64,
    pub summary: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationIntentInput {
    pub generation: i64,
    pub submission_id: String,
    pub observed_target_revision: String,
    pub observed_target_tree: String,
    pub result_revision: String,
    pub result_tree: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationResultInput {
    pub generation: i64,
    pub submission_id: String,
    pub publication_state: String,
    pub observed_target_revision: String,
    pub result_revision: String,
    pub result_tree: String,
    pub check_job_ids: Vec<String>,
    pub summary: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationReconciliationInput {
    pub submission_id: String,
    pub disposition: String,
    pub observed_target_revision: String,
    pub observed_target_tree: String,
    pub evidence: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FinalizeIntegrationInput {
    pub generation: i64,
    pub submission_id: String,
    pub observed_target_revision: String,
    pub observed_target_tree: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReopenSubmissionInput {
    pub submission_id: String,
    pub reason: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityReleaseInput {
    pub generation: i64,
    pub summary: String,
    #[serde(default)]
    pub blocked: bool,
}
