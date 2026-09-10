//! Shared knowledge and scoped decision request/response types.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeScope {
    #[serde(default)]
    pub task_ids: Vec<String>,
    #[serde(default)]
    pub components: Vec<String>,
    #[serde(default)]
    pub environments: Vec<String>,
    #[serde(default)]
    pub versions: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeProvenance {
    pub summary: String,
    pub source_uri: Option<String>,
    pub source_task_id: Option<String>,
    pub source_submission_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeInput {
    pub kind: String,
    pub title: String,
    pub body: String,
    pub status: String,
    pub scope: KnowledgeScope,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub applicability: String,
    pub provenance: KnowledgeProvenance,
    #[serde(default = "project_collection")]
    pub collection: String,
    #[serde(default)]
    pub share_across_projects: bool,
}

fn project_collection() -> String {
    "project".into()
}

/// The lesson shape accepted as part of an immutable submission transaction.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SubmissionLessonInput {
    pub kind: String,
    pub title: String,
    pub body: String,
    pub status: String,
    pub scope: KnowledgeScope,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub applicability: String,
    #[serde(default = "project_collection")]
    pub collection: String,
    #[serde(default)]
    pub share_across_projects: bool,
    #[serde(default)]
    pub provenance_summary: String,
    pub source_uri: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeEditInput {
    pub expected_revision: i64,
    pub title: String,
    pub body: String,
    pub status: String,
    pub scope: KnowledgeScope,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub applicability: String,
    pub provenance: KnowledgeProvenance,
    pub superseded_by_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeFeedbackInput {
    pub expected_revision: i64,
    pub useful: bool,
    #[serde(default)]
    pub comment: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionTaskInput {
    pub task_id: String,
    pub task_revision: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionInput {
    pub question: String,
    pub options: Vec<String>,
    pub rationale: String,
    pub required_actor: String,
    pub affected_tasks: Vec<DecisionTaskInput>,
    pub policy_revision: i64,
    #[serde(default)]
    pub environment: String,
    #[serde(default)]
    pub conditions: String,
    pub expires_at: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionAnswerInput {
    pub expected_generation: i64,
    pub disposition: String,
    pub answer: String,
    pub rationale: String,
    #[serde(default)]
    pub conditions_confirmed: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionReopenInput {
    pub expected_generation: i64,
    pub rationale: String,
    pub affected_tasks: Vec<DecisionTaskInput>,
    pub policy_revision: i64,
    #[serde(default)]
    pub environment: String,
    #[serde(default)]
    pub conditions: String,
    pub expires_at: Option<i64>,
}
