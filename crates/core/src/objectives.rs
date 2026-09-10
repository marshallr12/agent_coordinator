//! Objective grouping requests shared by the service and native clients.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ObjectiveChildInput {
    pub task_id: String,
    pub required: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectiveInput {
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub acceptance_criteria: Vec<String>,
    #[serde(default = "normal_priority")]
    pub priority: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<ObjectiveChildInput>,
    #[serde(default)]
    pub planned: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectiveChildrenInput {
    pub expected_revision: i64,
    pub children: Vec<ObjectiveChildInput>,
}

fn normal_priority() -> i64 {
    2
}
