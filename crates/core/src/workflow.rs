use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RequiredCheck {
    pub identity: String,
    pub version: String,
    pub environment: String,
    /// The repository paths that are this check's only inputs. Optional:
    /// absent or empty means the check declares none, and nothing about it
    /// changes. When declared, a candidate whose paths are byte-identical to
    /// the target's may land while the target itself fails the check (see
    /// the operator guide, "Checks that read only declared inputs"). Validate
    /// with [`validate_check_inputs`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<String>,
}

/// The most input paths one required check may declare.
pub const MAX_CHECK_INPUTS: usize = 20;
/// The longest declared input path in bytes.
pub const MAX_CHECK_INPUT_BYTES: usize = 300;

/// Refuses declared check inputs that are not plain repository file paths:
/// each must be relative, `/`-separated, free of `.` and `..` segments,
/// empty segments, backslashes, control characters and glob characters
/// (`*`, `?`, `[`, `]`, `{`, `}`; globs are not supported), and unique. An
/// empty list is valid and means no declaration.
pub fn validate_check_inputs(inputs: &[String]) -> Result<(), String> {
    if inputs.len() > MAX_CHECK_INPUTS {
        return Err(format!(
            "A check declares at most {MAX_CHECK_INPUTS} input paths."
        ));
    }
    for (index, path) in inputs.iter().enumerate() {
        validate_input_path(path)?;
        if inputs[..index].contains(path) {
            return Err(format!("Input path {path:?} is declared twice."));
        }
    }
    Ok(())
}

/// Checks one declared input path (see [`validate_check_inputs`]).
fn validate_input_path(path: &str) -> Result<(), String> {
    let bad = |why: &str| Err(format!("Input path {path:?} is not allowed: {why}."));
    if path.is_empty() || path.len() > MAX_CHECK_INPUT_BYTES {
        return bad("it must have 1 to 300 bytes");
    }
    if path.starts_with('/') || path.ends_with('/') {
        return bad("it must be a relative file path without a leading or trailing slash");
    }
    if path.chars().any(|c| c.is_control() || c == '\\') {
        return bad("it must not contain control characters or backslashes");
    }
    if path.chars().any(|c| "*?[]{}".contains(c)) {
        return bad("globs are not supported");
    }
    if path.split('/').any(|part| matches!(part, "" | "." | "..")) {
        return bad("it must not contain empty, . or .. segments");
    }
    Ok(())
}

#[cfg(test)]
mod input_tests {
    use super::*;

    fn paths(list: &[&str]) -> Vec<String> {
        list.iter().map(|p| (*p).to_owned()).collect()
    }

    #[test]
    fn plain_relative_paths_are_accepted_and_none_is_valid() {
        assert!(validate_check_inputs(&[]).is_ok());
        assert!(validate_check_inputs(&paths(&["Cargo.lock", ".cargo/audit.toml"])).is_ok());
    }

    #[test]
    fn globs_escapes_and_duplicates_are_refused() {
        let bad = [
            "",
            "/etc/passwd",
            "dir/",
            "../x",
            "a/./b",
            "a//b",
            "a\\b",
            "*.lock",
            "a/b?",
            "a[0]",
            "{a,b}",
            "a\nb",
        ];
        for path in bad {
            assert!(validate_check_inputs(&paths(&[path])).is_err(), "{path:?}");
        }
        assert!(validate_check_inputs(&paths(&["a", "a"])).is_err());
        let many: Vec<String> = (0..=MAX_CHECK_INPUTS).map(|i| format!("f{i}")).collect();
        assert!(validate_check_inputs(&many).is_err());
    }

    #[test]
    fn the_field_is_optional_and_omitted_when_empty() {
        let plain: RequiredCheck =
            serde_json::from_str(r#"{"identity":"a","version":"1","environment":"e"}"#).unwrap();
        assert!(plain.inputs.is_empty());
        assert!(!serde_json::to_string(&plain).unwrap().contains("inputs"));
        let declared: RequiredCheck = serde_json::from_str(
            r#"{"identity":"a","version":"1","environment":"e","inputs":["Cargo.lock"]}"#,
        )
        .unwrap();
        assert_eq!(declared.inputs, ["Cargo.lock"]);
    }
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

/// A contributor's proposed acceptance-criteria change, carried in a submission and
/// applied only when the reviewer explicitly accepts it.
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AcAmendmentInput {
    /// The task's current acceptance criteria, exactly.
    pub old: Vec<String>,
    /// The proposed acceptance criteria; submission evidence must cover these.
    pub new: Vec<String>,
    pub rationale: String,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ac_amendment: Option<AcAmendmentInput>,
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
    /// The independence a review claim asserts (`distinct_launch`,
    /// `distinct_host` or `distinct_vendor`). With `distinct_launch` the
    /// service also refuses a reviewer whose launch (the root of its
    /// `parent_session_id` chain) is shared with a recorded contributor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_independence: Option<String>,
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
    /// `accepted` or `rejected`; required when approving a submission that carries an
    /// `ac_amendment`. A rejected amendment makes the whole review changes_requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amendment_decision: Option<String>,
    /// How independent the reviewer was (`distinct_launch`, `distinct_host` or
    /// `distinct_vendor`), as the posting supervisor reports it; recorded on the decision.
    /// `distinct_launch` is enforced: the reviewer's launch must differ from every
    /// recorded contributor's launch for the subject and its landing range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_independence: Option<String>,
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
