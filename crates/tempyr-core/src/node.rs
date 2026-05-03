use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::edge::EdgeEntry;
use crate::{Result, TempyrError};

/// The YAML frontmatter of a graph node file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeFrontmatter {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<EdgeEntry>,
}

/// A fully parsed graph node: frontmatter + markdown body + metadata.
#[derive(Debug, Clone)]
pub struct Node {
    pub frontmatter: NodeFrontmatter,
    pub body: String,
    pub file_path: PathBuf,
    pub content_hash: String,
}

impl Node {
    pub fn id(&self) -> &str {
        &self.frontmatter.id
    }

    pub fn node_type(&self) -> &str {
        &self.frontmatter.node_type
    }

    pub fn status(&self) -> Option<&str> {
        self.frontmatter.status.as_deref()
    }

    pub fn title(&self) -> &str {
        // Extract title from first H1 in body, or fall back to id
        self.body
            .lines()
            .find(|line| line.starts_with("# "))
            .map(|line| line.trim_start_matches("# ").trim())
            .unwrap_or(&self.frontmatter.id)
    }

    pub fn edges(&self) -> &[EdgeEntry] {
        &self.frontmatter.edges
    }
}

/// Parse a node from its file content (YAML frontmatter + markdown body).
pub fn parse_node(content: &str, file_path: PathBuf) -> Result<Node> {
    let (frontmatter_str, body) = split_frontmatter(content)?;

    let frontmatter: NodeFrontmatter =
        serde_yaml::from_str(frontmatter_str).map_err(|e| TempyrError::Yaml(e.to_string()))?;

    let content_hash = blake3::hash(body.as_bytes()).to_hex().to_string();

    Ok(Node {
        frontmatter,
        body: body.to_string(),
        file_path,
        content_hash,
    })
}

/// Serialize a node back to its file format (YAML frontmatter + markdown body).
pub fn serialize_node(node: &Node) -> Result<String> {
    let yaml =
        serde_yaml::to_string(&node.frontmatter).map_err(|e| TempyrError::Yaml(e.to_string()))?;

    Ok(format!("---\n{}---\n{}", yaml, node.body))
}

/// Split file content into (frontmatter_yaml, body_markdown).
fn split_frontmatter(content: &str) -> Result<(&str, &str)> {
    let content = content.trim_start();

    if !content.starts_with("---") {
        return Err(TempyrError::Node(
            "File does not start with YAML frontmatter delimiter '---'".to_string(),
        ));
    }

    // Find the closing ---
    let after_first = &content[3..];
    let after_first = after_first.trim_start_matches(['\r', '\n']);

    let closing = after_first.find("\n---").ok_or_else(|| {
        TempyrError::Node("Missing closing YAML frontmatter delimiter '---'".to_string())
    })?;

    let frontmatter = &after_first[..closing];
    let rest = &after_first[closing + 4..]; // skip \n---
    // Skip the newline after ---
    let body = rest
        .strip_prefix('\n')
        .unwrap_or(rest.strip_prefix("\r\n").unwrap_or(rest));

    Ok((frontmatter, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use std::path::PathBuf;

    fn sample_node_content() -> &'static str {
        r#"---
id: feat-session-replay
type: feature
status: draft
created: 2026-03-20T14:30:00Z
updated: 2026-03-23T09:15:00Z
owner: alice
tags: [replay, observability, q2-2026]
edges:
  - target: epic-observability-v2
    type: child_of
  - target: persona-platform-eng
    type: serves
    valid_from: 2026-03-20
  - target: task-replay-ingestion
    type: decomposes_to
---
# Session Replay for Funnel Steps

## Problem

Platform engineers currently debug funnel drop-offs by reading logs.
"#
    }

    #[test]
    fn test_parse_node_basic() {
        let node = parse_node(
            sample_node_content(),
            PathBuf::from("graph/features/feat-session-replay.md"),
        )
        .unwrap();

        assert_eq!(node.id(), "feat-session-replay");
        assert_eq!(node.node_type(), "feature");
        assert_eq!(node.status(), Some("draft"));
        assert_eq!(node.frontmatter.owner.as_deref(), Some("alice"));
        assert_eq!(
            node.frontmatter.tags.as_ref().unwrap(),
            &["replay", "observability", "q2-2026"]
        );
        assert_eq!(node.frontmatter.edges.len(), 3);
        assert_eq!(node.frontmatter.edges[0].target, "epic-observability-v2");
        assert_eq!(node.frontmatter.edges[0].edge_type, "child_of");
        assert!(node.body.contains("# Session Replay"));
        assert!(node.body.contains("Platform engineers"));
    }

    #[test]
    fn test_parse_node_minimal() {
        let content = "---\nid: my-note\ntype: note\n---\nSome body text.\n";
        let node = parse_node(content, PathBuf::from("test.md")).unwrap();

        assert_eq!(node.id(), "my-note");
        assert_eq!(node.node_type(), "note");
        assert_eq!(node.status(), None);
        assert!(node.frontmatter.edges.is_empty());
        assert_eq!(node.body.trim(), "Some body text.");
    }

    #[test]
    fn test_parse_node_with_temporal_edges() {
        let node = parse_node(sample_node_content(), PathBuf::from("test.md")).unwrap();

        let serves_edge = &node.frontmatter.edges[1];
        assert_eq!(serves_edge.edge_type, "serves");
        assert_eq!(
            serves_edge.valid_from,
            Some(NaiveDate::from_ymd_opt(2026, 3, 20).unwrap())
        );
        assert_eq!(serves_edge.valid_until, None);
    }

    #[test]
    fn test_serialize_roundtrip() {
        let original = parse_node(sample_node_content(), PathBuf::from("test.md")).unwrap();
        let serialized = serialize_node(&original).unwrap();
        let reparsed = parse_node(&serialized, PathBuf::from("test.md")).unwrap();

        assert_eq!(original.frontmatter.id, reparsed.frontmatter.id);
        assert_eq!(
            original.frontmatter.node_type,
            reparsed.frontmatter.node_type
        );
        assert_eq!(original.frontmatter.status, reparsed.frontmatter.status);
        assert_eq!(
            original.frontmatter.edges.len(),
            reparsed.frontmatter.edges.len()
        );
        for (a, b) in original
            .frontmatter
            .edges
            .iter()
            .zip(reparsed.frontmatter.edges.iter())
        {
            assert_eq!(a.target, b.target);
            assert_eq!(a.edge_type, b.edge_type);
            assert_eq!(a.valid_from, b.valid_from);
        }
    }

    #[test]
    fn test_parse_invalid_yaml() {
        let content = "---\n[invalid yaml\n---\nbody\n";
        let result = parse_node(content, PathBuf::from("test.md"));
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_missing_frontmatter() {
        let content = "Just some text without frontmatter";
        let result = parse_node(content, PathBuf::from("test.md"));
        assert!(result.is_err());
    }

    #[test]
    fn test_content_hash_stability() {
        let node1 = parse_node(sample_node_content(), PathBuf::from("a.md")).unwrap();
        let node2 = parse_node(sample_node_content(), PathBuf::from("b.md")).unwrap();
        assert_eq!(node1.content_hash, node2.content_hash);
    }

    #[test]
    fn test_content_hash_ignores_frontmatter() {
        let content1 = "---\nid: node-a\ntype: note\nstatus: draft\n---\nSame body\n";
        let content2 = "---\nid: node-b\ntype: note\nstatus: active\n---\nSame body\n";

        let node1 = parse_node(content1, PathBuf::from("test.md")).unwrap();
        let node2 = parse_node(content2, PathBuf::from("test.md")).unwrap();

        assert_eq!(node1.content_hash, node2.content_hash);
    }

    #[test]
    fn test_title_extraction() {
        let node = parse_node(sample_node_content(), PathBuf::from("test.md")).unwrap();
        assert_eq!(node.title(), "Session Replay for Funnel Steps");
    }

    #[test]
    fn test_title_fallback_to_id() {
        let content = "---\nid: my-note\ntype: note\n---\nNo heading here.\n";
        let node = parse_node(content, PathBuf::from("test.md")).unwrap();
        assert_eq!(node.title(), "my-note");
    }
}
