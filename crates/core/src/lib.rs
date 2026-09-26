//! Vendor-neutral request types shared by the coordination server and clients.
use serde::{Deserialize, Serialize};
pub mod history;
pub use history::*;
pub mod objectives;
pub use objectives::*;
pub mod knowledge;
pub use knowledge::*;
pub mod imports;
pub use imports::*;
pub mod artifacts;
pub use artifacts::*;
pub mod workflow;
pub use workflow::*;

pub const INSTRUCTION_VERSION: &str = "9";
pub const REQUIRED_SECTION: &str = "coordination-v8";

/// Immutable source and target identity embedded in every workspace binary.
pub const BUILD_SOURCE_COMMIT: &str = env!("COORDINATOR_SOURCE_COMMIT");
pub const BUILD_SOURCE_REPOSITORY: &str = env!("COORDINATOR_SOURCE_REPOSITORY");
pub const BUILD_TARGET_OS: &str = env!("COORDINATOR_TARGET_OS");
pub const BUILD_TARGET_ARCH: &str = env!("COORDINATOR_TARGET_ARCH");
pub const BUILD_DIRTY: &str = env!("COORDINATOR_BUILD_DIRTY");
pub const CLIENT_PROTOCOL_VERSIONS: &[&str] = &["v1"];
pub const CLIENT_CAPABILITIES: &[&str] = &["durable_candidate_submission_fields"];
pub const CLIENT_SUPPORTED_TARGETS: &[&str] = &["linux-x86_64", "linux-aarch64", "windows-x86_64"];

pub fn build_identity() -> serde_json::Value {
    serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "source_commit": BUILD_SOURCE_COMMIT,
        "source_repository": BUILD_SOURCE_REPOSITORY,
        "target_os": BUILD_TARGET_OS,
        "target_arch": BUILD_TARGET_ARCH,
        "dirty": BUILD_DIRTY == "true",
    })
}

pub fn client_compatibility() -> serde_json::Value {
    serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "supported_protocol_versions": CLIENT_PROTOCOL_VERSIONS,
        "capabilities": CLIENT_CAPABILITIES,
        "build": build_identity(),
    })
}

pub fn service_client_compatibility() -> serde_json::Value {
    serde_json::json!({
        "required_protocol_versions": CLIENT_PROTOCOL_VERSIONS,
        "required_capabilities": CLIENT_CAPABILITIES,
        "compatible_client": {
            "version": env!("CARGO_PKG_VERSION"),
            "source_commit": BUILD_SOURCE_COMMIT,
            "source_repository": BUILD_SOURCE_REPOSITORY,
            "supported_targets": CLIENT_SUPPORTED_TARGETS,
            "verified_packages": [],
            "provenance": "Build the exact named source commit. No checksummed package is registered for this deployment."
        }
    })
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectInput {
    pub name: String,
    pub repository_url: String,
    pub target_branch: String,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskInput {
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub acceptance_criteria: Vec<String>,
    #[serde(default = "code")]
    pub kind: String,
    #[serde(default = "normal")]
    pub priority: i64,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub planned: bool,
}
fn code() -> String {
    "code".into()
}
fn normal() -> i64 {
    2
}
fn work() -> String {
    "work".into()
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimInput {
    pub task_id: Option<String>,
    pub expected_task_revision: Option<i64>,
    #[serde(default = "work")]
    pub mode: String,
    pub policy_revision: i64,
    pub instruction_version: String,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Acknowledgment {
    pub project_id: String,
    pub policy_revision: i64,
    pub instruction_version: String,
    pub sections: Vec<String>,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RenewInput {
    pub generation: i64,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointInput {
    pub generation: i64,
    pub summary: String,
    #[serde(default)]
    pub current_action: String,
    #[serde(default)]
    pub next_step: String,
    #[serde(default)]
    pub blockers: Vec<String>,
    /// Register delegated helpers before they contribute to this task.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contributor_session_ids: Vec<String>,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseInput {
    pub generation: i64,
    pub summary: String,
    #[serde(default)]
    pub blocked: bool,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryInput {
    pub generation: i64,
    pub disposition: String,
    pub summary: String,
    pub saved_work_checked: bool,
    pub running_jobs_checked: bool,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckoutInput {
    pub generation: i64,
    pub workstation_id: String,
    pub identity: String,
    pub path: String,
    pub branch: String,
    pub base_revision: String,
    pub clean: bool,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyInput {
    pub expected_revision: i64,
    pub review_mode: String,
    pub recovery_mode: String,
    pub lease_seconds: i64,
    pub rules: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provenance: String,
    pub agent_rule_editing: bool,
    pub automatic_integration: bool,
    /// Omission preserves the current policy for older clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_subagent_reviews: Option<bool>,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskEdit {
    pub expected_revision: i64,
    pub title: String,
    pub description: String,
    pub acceptance_criteria: Vec<String>,
    pub priority: i64,
    pub depends_on: Vec<String>,
    pub planned: bool,
}

/// Human-managed authority for an agent principal or role to edit task definitions.
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDefinitionGrantInput {
    pub target_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_principal_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_role: Option<String>,
}

/// Revision-checked revocation of a human-managed task-definition grant.
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDefinitionGrantRevoke {
    pub expected_revision: i64,
}

pub fn timestamp(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or(chrono::DateTime::UNIX_EPOCH)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Stable harness registration; resume proof is supplied only in an HTTP header.
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionInput {
    pub session_id: String,
    pub workstation_id: String,
    pub harness: String,
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent: Option<SubagentInput>,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SubagentInput {
    pub project_id: String,
    /// Stable name reused across this subagent's sessions.
    pub name: String,
    pub parent_session_id: String,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UnblockInput {
    pub expected_revision: i64,
    pub reason: String,
}
