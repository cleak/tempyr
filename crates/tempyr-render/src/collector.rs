use tempyr_core::graph::Graph;
use tempyr_core::node::Node;
use tempyr_core::temporal::{TemporalFilter, filter_edges, is_node_visible};

use crate::template::SectionDef;

/// Collected data for a single rendered section.
#[derive(Debug, Clone)]
pub struct SectionData {
    pub heading: String,
    pub items: Vec<SectionItem>,
    pub is_root_section: bool,
}

/// A single item in a rendered section (one node's contribution).
#[derive(Debug, Clone)]
pub struct SectionItem {
    pub node_id: String,
    pub title: String,
    pub node_type: String,
    pub fields: Vec<(String, String)>,
    pub body: Option<String>,
    pub sub_items: Vec<SectionItem>,
    pub internal_edges: Vec<(String, String, String)>, // (from_id, to_id, edge_type)
}

/// Collect data for a section by traversing the graph from the root node.
pub fn collect_section(
    graph: &Graph,
    root: &Node,
    section: &SectionDef,
    filter: &TemporalFilter,
) -> SectionData {
    let heading = section.heading.clone();

    // Root body sections
    if section.source.as_deref() == Some("root") {
        return collect_root_section(root, section);
    }

    // Semantic search placeholder (not yet implemented)
    if section.source.as_deref() == Some("semantic_search") {
        return SectionData {
            heading,
            items: Vec::new(),
            is_root_section: false,
        };
    }

    // Traversal-based sections
    if let Some(edge_type) = &section.traverse {
        return collect_traverse_section(graph, root, section, edge_type, filter);
    }

    // Fallback: empty section
    SectionData {
        heading,
        items: Vec::new(),
        is_root_section: false,
    }
}

/// Collect a section sourced from the root node itself.
fn collect_root_section(root: &Node, section: &SectionDef) -> SectionData {
    let body = if let Some(body_section_name) = &section.body_section {
        extract_body_section(&root.body, body_section_name)
    } else {
        Some(root.body.clone())
    };

    let fields = collect_fields(root, section);

    let item = SectionItem {
        node_id: root.id().to_string(),
        title: root.title().to_string(),
        node_type: root.node_type().to_string(),
        fields,
        body,
        sub_items: Vec::new(),
        internal_edges: Vec::new(),
    };

    SectionData {
        heading: section.heading.clone(),
        items: vec![item],
        is_root_section: true,
    }
}

/// Collect a section by traversing edges from the root.
fn collect_traverse_section(
    graph: &Graph,
    root: &Node,
    section: &SectionDef,
    edge_type: &str,
    temporal_filter: &TemporalFilter,
) -> SectionData {
    let target_type = section.target_type.as_deref();
    let include_body = section.include_body.unwrap_or(false);

    // Filter root's edges by temporal validity
    let visible_edges = filter_edges(root.edges(), temporal_filter);

    let mut items = Vec::new();

    for edge in visible_edges {
        if edge.edge_type != edge_type {
            continue;
        }

        let Some(target_node) = graph.get_node(&edge.target) else {
            continue;
        };

        // Filter by target type if specified
        if let Some(tt) = target_type
            && target_node.node_type() != tt
        {
            continue;
        }

        // Filter by node visibility
        if !is_node_visible(target_node, temporal_filter) {
            continue;
        }

        // Apply status filter if specified
        if let Some(filter_map) = &section.filter
            && let Some(allowed_statuses) = filter_map.get("status")
            && let Some(status) = target_node.status()
            && !allowed_statuses.contains(&status.to_string())
        {
            continue;
        }

        let body = if include_body {
            Some(target_node.body.clone())
        } else {
            None
        };

        let fields = collect_fields(target_node, section);

        // Sub-traversal (one more hop)
        let sub_items = if let (Some(sub_edge), Some(sub_type)) =
            (&section.sub_traverse, &section.sub_target_type)
        {
            collect_sub_items(graph, target_node, sub_edge, sub_type, temporal_filter)
        } else {
            Vec::new()
        };

        // Internal edges between items in this section
        let internal_edges = if section.show_internal_edges.unwrap_or(false) {
            collect_internal_edges(target_node, section)
        } else {
            Vec::new()
        };

        items.push(SectionItem {
            node_id: target_node.id().to_string(),
            title: target_node.title().to_string(),
            node_type: target_node.node_type().to_string(),
            fields,
            body,
            sub_items,
            internal_edges,
        });
    }

    SectionData {
        heading: section.heading.clone(),
        items,
        is_root_section: false,
    }
}

/// Collect sub-items by following one more hop from a node.
fn collect_sub_items(
    graph: &Graph,
    node: &Node,
    edge_type: &str,
    target_type: &str,
    temporal_filter: &TemporalFilter,
) -> Vec<SectionItem> {
    let visible_edges = filter_edges(node.edges(), temporal_filter);
    let mut items = Vec::new();

    for edge in visible_edges {
        if edge.edge_type != edge_type {
            continue;
        }
        let Some(target) = graph.get_node(&edge.target) else {
            continue;
        };
        if target.node_type() != target_type {
            continue;
        }
        if !is_node_visible(target, temporal_filter) {
            continue;
        }

        items.push(SectionItem {
            node_id: target.id().to_string(),
            title: target.title().to_string(),
            node_type: target.node_type().to_string(),
            fields: Vec::new(),
            body: Some(target.body.clone()),
            sub_items: Vec::new(),
            internal_edges: Vec::new(),
        });
    }

    items
}

/// Collect internal edges between items (e.g., blocked_by between tasks).
fn collect_internal_edges(node: &Node, section: &SectionDef) -> Vec<(String, String, String)> {
    let Some(edge_types) = &section.internal_edge_types else {
        return Vec::new();
    };

    let mut edges = Vec::new();
    for edge in node.edges() {
        if edge_types.contains(&edge.edge_type) {
            edges.push((
                node.id().to_string(),
                edge.target.clone(),
                edge.edge_type.clone(),
            ));
        }
    }
    edges
}

/// Extract field values from a node for display.
fn collect_fields(node: &Node, section: &SectionDef) -> Vec<(String, String)> {
    let Some(fields) = &section.include_fields else {
        return Vec::new();
    };

    let mut result = Vec::new();
    for field in fields {
        let value = match field.as_str() {
            "status" => node.status().map(String::from),
            "owner" => node.frontmatter.owner.clone(),
            "created" => node
                .frontmatter
                .created
                .map(|c| c.format("%Y-%m-%d").to_string()),
            "updated" => node
                .frontmatter
                .updated
                .map(|u| u.format("%Y-%m-%d").to_string()),
            _ => None,
        };
        if let Some(v) = value {
            result.push((field.clone(), v));
        }
    }
    result
}

/// Extract a named section (## Heading) from a markdown body.
pub fn extract_body_section(body: &str, section_name: &str) -> Option<String> {
    let heading_prefix = format!("## {section_name}");
    let mut in_section = false;
    let mut lines = Vec::new();

    for line in body.lines() {
        if line.starts_with(&heading_prefix) {
            in_section = true;
            continue;
        }
        if in_section && line.starts_with("## ") {
            break;
        }
        if in_section {
            lines.push(line);
        }
    }

    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n").trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use tempyr_core::graph::Graph;
    use tempyr_core::node::parse_node;
    use tempyr_core::schema::Schema;
    use tempyr_core::temporal::TemporalFilter;

    fn make_schema() -> Schema {
        let schema_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("schema/default-schema.toml");
        Schema::load(&schema_path).unwrap()
    }

    fn build_test_graph() -> Graph {
        let mut graph = Graph::new(make_schema());

        let feat = r#"---
id: feat-replay
type: feature
status: draft
owner: alice
created: 2026-03-20T14:30:00Z
updated: 2026-03-23T09:15:00Z
edges:
  - target: decision-storage
    type: depends_on
  - target: persona-eng
    type: serves
  - target: task-ingestion
    type: decomposes_to
---
# Session Replay

## Problem

Engineers need to see what happened during a session.

## Solution

A recording agent captures DOM snapshots.
"#;
        let persona = "---\nid: persona-eng\ntype: persona\nedges:\n  - target: feat-replay\n    type: served_by\n---\n# Platform Engineer\n\nDebug funnel issues.\n";
        let decision = "---\nid: decision-storage\ntype: decision\nstatus: decided\nedges:\n  - target: feat-replay\n    type: decision_for\n---\n# Storage Backend\n\nUse ClickHouse for event storage.\n";
        let decision_old = "---\nid: decision-old\ntype: decision\nstatus: superseded\n---\n# Old Decision\n\nThis was superseded.\n";
        let task = "---\nid: task-ingestion\ntype: task\nstatus: backlog\nedges:\n  - target: feat-replay\n    type: child_of\n---\n# Build Ingestion\n\nImplement ingestion pipeline.\n";

        graph.add_node(parse_node(feat, PathBuf::from("f.md")).unwrap());
        graph.add_node(parse_node(persona, PathBuf::from("p.md")).unwrap());
        graph.add_node(parse_node(decision, PathBuf::from("d.md")).unwrap());
        graph.add_node(parse_node(decision_old, PathBuf::from("do.md")).unwrap());
        graph.add_node(parse_node(task, PathBuf::from("t.md")).unwrap());

        graph
    }

    #[test]
    fn test_collect_root_section() {
        let graph = build_test_graph();
        let root = graph.get_node("feat-replay").unwrap();
        let section = SectionDef {
            heading: "Overview".to_string(),
            source: Some("root".to_string()),
            body_section: None,
            traverse: None,
            target_type: None,
            include_body: None,
            include_fields: Some(vec!["status".to_string(), "owner".to_string()]),
            filter: None,
            sub_traverse: None,
            sub_target_type: None,
            show_internal_edges: None,
            internal_edge_types: None,
            max_results: None,
            min_similarity: None,
            query_from: None,
        };

        let data = collect_section(&graph, root, &section, &TemporalFilter::current());
        assert!(data.is_root_section);
        assert_eq!(data.items.len(), 1);
        assert!(
            data.items[0]
                .body
                .as_ref()
                .unwrap()
                .contains("Session Replay")
        );
        assert!(
            data.items[0]
                .fields
                .iter()
                .any(|(k, v)| k == "status" && v == "draft")
        );
    }

    #[test]
    fn test_collect_root_body_section() {
        let graph = build_test_graph();
        let root = graph.get_node("feat-replay").unwrap();
        let section = SectionDef {
            heading: "Problem".to_string(),
            source: Some("root".to_string()),
            body_section: Some("Problem".to_string()),
            traverse: None,
            target_type: None,
            include_body: None,
            include_fields: None,
            filter: None,
            sub_traverse: None,
            sub_target_type: None,
            show_internal_edges: None,
            internal_edge_types: None,
            max_results: None,
            min_similarity: None,
            query_from: None,
        };

        let data = collect_section(&graph, root, &section, &TemporalFilter::current());
        let body = data.items[0].body.as_ref().unwrap();
        assert!(body.contains("Engineers need to see"));
        assert!(!body.contains("## Solution")); // should stop at next heading
    }

    #[test]
    fn test_collect_traverse_section() {
        let graph = build_test_graph();
        let root = graph.get_node("feat-replay").unwrap();
        let section = SectionDef {
            heading: "Target Users".to_string(),
            source: None,
            body_section: None,
            traverse: Some("serves".to_string()),
            target_type: Some("persona".to_string()),
            include_body: Some(true),
            include_fields: None,
            filter: None,
            sub_traverse: None,
            sub_target_type: None,
            show_internal_edges: None,
            internal_edge_types: None,
            max_results: None,
            min_similarity: None,
            query_from: None,
        };

        let data = collect_section(&graph, root, &section, &TemporalFilter::current());
        assert_eq!(data.items.len(), 1);
        assert_eq!(data.items[0].title, "Platform Engineer");
        assert!(data.items[0].body.is_some());
    }

    #[test]
    fn test_collect_with_status_filter() {
        let graph = build_test_graph();
        let root = graph.get_node("feat-replay").unwrap();

        // This filter would exclude superseded decisions
        let section = SectionDef {
            heading: "Decisions".to_string(),
            source: None,
            body_section: None,
            traverse: Some("depends_on".to_string()),
            target_type: Some("decision".to_string()),
            include_body: Some(true),
            include_fields: None,
            filter: Some({
                let mut m = std::collections::HashMap::new();
                m.insert(
                    "status".to_string(),
                    vec!["decided".to_string(), "discussing".to_string()],
                );
                m
            }),
            sub_traverse: None,
            sub_target_type: None,
            show_internal_edges: None,
            internal_edge_types: None,
            max_results: None,
            min_similarity: None,
            query_from: None,
        };

        let data = collect_section(&graph, root, &section, &TemporalFilter::current());
        assert_eq!(data.items.len(), 1);
        assert_eq!(data.items[0].node_id, "decision-storage");
    }

    #[test]
    fn test_extract_body_section_basic() {
        let body = "# Title\n\n## Problem\n\nThis is the problem.\n\n## Solution\n\nThis is the solution.\n";
        let extracted = extract_body_section(body, "Problem").unwrap();
        assert_eq!(extracted, "This is the problem.");
    }

    #[test]
    fn test_extract_body_section_not_found() {
        let body = "# Title\n\n## Something\n\nContent.\n";
        assert!(extract_body_section(body, "Missing").is_none());
    }

    #[test]
    fn test_semantic_search_placeholder() {
        let graph = build_test_graph();
        let root = graph.get_node("feat-replay").unwrap();
        let section = SectionDef {
            heading: "Insights".to_string(),
            source: Some("semantic_search".to_string()),
            body_section: None,
            traverse: None,
            target_type: None,
            include_body: None,
            include_fields: None,
            filter: None,
            sub_traverse: None,
            sub_target_type: None,
            show_internal_edges: None,
            internal_edge_types: None,
            max_results: Some(5),
            min_similarity: Some(0.7),
            query_from: Some("root".to_string()),
        };

        let data = collect_section(&graph, root, &section, &TemporalFilter::current());
        assert!(data.items.is_empty()); // placeholder returns empty
    }
}
