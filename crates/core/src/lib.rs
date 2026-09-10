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

pub const INSTRUCTION_VERSION: &str = "7";
pub const REQUIRED_SECTION: &str = "coordination-v7";

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
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UnblockInput {
    pub expected_revision: i64,
    pub reason: String,
}
