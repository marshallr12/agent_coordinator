//! Bounded task-history response types shared by service clients.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TaskHistoryItem {
    pub kind: String,
    pub relation: String,
    pub task_id: String,
    pub occurred_at: Option<String>,
    pub record: Value,
}

#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TaskHistoryPage {
    pub project_id: String,
    pub task_id: String,
    pub subject_task_id: String,
    pub kind: String,
    pub snapshot: String,
    pub items: Vec<TaskHistoryItem>,
    pub next_cursor: Option<String>,
}
