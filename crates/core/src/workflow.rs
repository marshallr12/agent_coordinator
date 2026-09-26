use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RequiredCheck {
    pub identity: String,
    pub version: String,
    pub environment: String,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowPolicyInput {
    pub expected_revision: i64,
    /// Omit for normal URL-derived setup; explicit values are administrator alias configuration.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub canonical_repository_key: String,
    pub required_checks: Vec<RequiredCheck>,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptanceEvidence {
    pub criterion: String,
    pub evidence: String,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lessons: Vec<crate::knowledge::SubmissionLessonInput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_ids: Vec<String>,
    pub repository: Option<String>,
    pub base_revision: Option<String>,
    pub candidate_revision: Option<String>,
    pub candidate_tree: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_remote: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_ref: Option<String>,
}

#[cfg(test)]
mod compatibility_tests {
    use super::SubmissionInput;
    use serde_json::json;

    #[test]
    fn pre_knowledge_submission_keeps_its_receipt_fingerprint_shape() {
        // A client may retry this persisted request after the service upgrades.
        let old = json!({"generation":1,"task_revision":1,"project_policy_revision":1,
            "workflow_policy_revision":0,"kind":"general","summary":"Result",
            "acceptance_evidence":[],"handoff":"Next steps","repository":null,
            "base_revision":null,"candidate_revision":null,"candidate_tree":null});
        let request: SubmissionInput = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(serde_json::to_value(request).unwrap(), old);
    }
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityClaimInput {
    pub expected_submission_id: String,
    pub expected_project_policy_revision: i64,
    pub expected_workflow_policy_revision: i64,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewFindingInput {
    pub severity: String,
    pub remedy: String,
    #[serde(default)]
    pub evidence: String,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
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

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationAuthorizationInput {
    pub submission_id: String,
    pub expected_project_policy_revision: i64,
    pub expected_workflow_policy_revision: i64,
    pub summary: String,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
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

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
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

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationReconciliationInput {
    pub submission_id: String,
    pub disposition: String,
    pub observed_target_revision: String,
    pub observed_target_tree: String,
    pub evidence: String,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentPublicationReconciliationInput {
    pub attempt_id: String,
    pub generation: i64,
    pub submission_id: String,
    pub disposition: String,
    pub canonical_repository_key: String,
    pub target_branch: String,
    pub observed_target_revision: String,
    pub observed_target_tree: String,
    pub observed_at: i64,
    pub local_journal_verified: bool,
    pub publisher_stopped: bool,
    pub evidence: String,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FinalizeIntegrationInput {
    pub generation: i64,
    pub submission_id: String,
    pub observed_target_revision: String,
    pub observed_target_tree: String,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReopenSubmissionInput {
    pub submission_id: String,
    pub reason: String,
    /// Required when an agent revises: `conflict`, `check_failed`, `candidate_missing`,
    /// `requirements_changed` or `author_withdraw`. Humans may omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    /// Evidence for the reason code, such as conflicting paths or the failing check receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityReleaseInput {
    pub generation: i64,
    pub summary: String,
    #[serde(default)]
    pub blocked: bool,
}
