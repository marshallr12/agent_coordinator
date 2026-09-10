//! Requests for bounded, authenticated artifact storage.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactLinkInput {
    pub display_name: String,
    pub media_type: String,
    pub external_url: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub job_id: Option<String>,
    #[serde(default)]
    pub size_bytes: Option<i64>,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub retention_days: Option<i64>,
    #[serde(default)]
    pub pinned: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactUploadInput {
    pub filename: String,
    pub media_type: String,
    pub size_bytes: i64,
    pub sha256: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub job_id: Option<String>,
    #[serde(default)]
    pub retention_days: Option<i64>,
    #[serde(default)]
    pub pinned: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRetentionInput {
    pub pinned: bool,
    #[serde(default)]
    pub retention_days: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactDeleteInput {
    pub reason: String,
}
