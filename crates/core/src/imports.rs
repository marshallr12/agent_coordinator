//! Service-authoritative Markdown import and export request types.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImportSource {
    pub context: String,
    pub git_revision: String,
    pub observed_at: String,
    pub branch: String,
    pub environment: String,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MarkdownChunk {
    pub path: String,
    pub markdown: String,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HistoricalMapping {
    pub path: String,
    pub section_identity: String,
    pub title: String,
    pub disposition: String,
    pub evidence: String,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImportPreviewInput {
    pub source: ImportSource,
    pub chunks: Vec<MarkdownChunk>,
    #[serde(default)]
    pub historical_mappings: Vec<HistoricalMapping>,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyImportInput {
    pub preview_digest: String,
    pub expected_project_event_revision: i64,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ImportItem {
    pub stable_identity: String,
    pub source_path: String,
    pub section_identity: String,
    pub title: String,
    pub record_kind: String,
    pub disposition: String,
    pub source_line: usize,
    pub evidence: String,
    pub prior_record_revision: Option<i64>,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ImportConflict {
    pub code: String,
    pub source_path: String,
    pub section_identity: String,
    pub message: String,
    pub blocking: bool,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct UnresolvedMarkdownLink {
    pub source_path: String,
    pub source_line: usize,
    pub target: String,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ImportPreview {
    pub id: String,
    pub digest: String,
    pub project_id: String,
    pub project_event_revision: i64,
    pub source: ImportSource,
    pub items: Vec<ImportItem>,
    pub conflicts: Vec<ImportConflict>,
    pub unresolved_links: Vec<UnresolvedMarkdownLink>,
    pub created_at: String,
    pub applied_at: Option<String>,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExportRecord {
    pub sort_key: String,
    pub kind: String,
    pub id: String,
    pub title: String,
    pub status: String,
    pub body: String,
    pub provenance: Value,
}
