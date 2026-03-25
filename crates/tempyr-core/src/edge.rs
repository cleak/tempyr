use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// A single edge entry as stored in a node's YAML frontmatter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EdgeEntry {
    pub target: String,
    #[serde(rename = "type")]
    pub edge_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_from: Option<NaiveDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<NaiveDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotation: Option<String>,
}

/// A fully resolved edge between two nodes (used in graph operations, not file storage).
#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    pub source_id: String,
    pub target_id: String,
    pub edge_type: String,
    pub valid_from: Option<NaiveDate>,
    pub valid_until: Option<NaiveDate>,
    pub annotation: Option<String>,
}

impl EdgeEntry {
    pub fn new(target: &str, edge_type: &str) -> Self {
        Self {
            target: target.to_string(),
            edge_type: edge_type.to_string(),
            valid_from: None,
            valid_until: None,
            annotation: None,
        }
    }
}
