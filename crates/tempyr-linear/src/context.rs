use serde_json::{Value, json};
use tempyr_core::graph::Graph;
use tempyr_core::node::Node;
use tempyr_core::schema::Schema;

use crate::mapping::{body_summary, node_title};

/// Metadata for a Linear attachment linking to a graph node.
pub struct AttachmentInput {
    pub title: String,
    pub subtitle: String,
    pub url: String,
    pub metadata: Value,
}

/// Build a rich markdown description for a Linear issue from a graph node.
///
/// Includes the node body plus collapsible sections for parent context,
/// decisions, constraints, blocking items, components, and MCP breadcrumbs.
pub fn build_issue_description(node: &Node, graph: &Graph, _schema: &Schema) -> String {
    let mut sections: Vec<String> = Vec::new();

    // Node body (the primary content)
    sections.push(node.body.clone());

    // Parent context (child_of edges → epics and features)
    let parent_section = build_parent_section(node, graph);
    if !parent_section.is_empty() {
        sections.push(format!("+++ Parent Context\n{parent_section}\n+++"));
    }

    // Decisions & constraints
    let decisions_section = build_related_section(
        node,
        graph,
        &["depends_on", "constrained_by", "decision_for"],
        &["decision", "constraint"],
        "Decisions & Constraints",
    );
    if !decisions_section.is_empty() {
        sections.push(decisions_section);
    }

    // Blocking items
    let blocking_section = build_blocking_section(node, graph);
    if !blocking_section.is_empty() {
        sections.push(format!("+++ Blocking Items\n{blocking_section}\n+++"));
    }

    // Components & APIs
    let components_section = build_related_section(
        node,
        graph,
        &["uses", "exposes"],
        &["component", "api_surface"],
        "Components",
    );
    if !components_section.is_empty() {
        sections.push(components_section);
    }

    // Risks
    let risks_section = build_related_section(node, graph, &["has_risk"], &["risk"], "Risks");
    if !risks_section.is_empty() {
        sections.push(risks_section);
    }

    // Tempyr footer with MCP breadcrumbs
    let title = node_title(node);
    let footer = format!(
        "---\n\n\
         **Tempyr**\n\
         - Node: `{}` | Type: {} | Status: {}\n\
         - File: `{}`\n\
         - MCP: `graph_get_node \"{}\"` | `graph_traverse \"{}\"` | `graph_context \"{}\"`",
        node.id(),
        node.node_type(),
        node.status().unwrap_or("—"),
        node.file_path.display(),
        node.id(),
        node.id(),
        title,
    );
    sections.push(footer);

    sections.join("\n\n---\n\n")
}

/// Build attachment metadata for each relevant context node.
pub fn build_attachments(node: &Node, graph: &Graph, _schema: &Schema) -> Vec<AttachmentInput> {
    let mut attachments = Vec::new();

    for edge in node.edges() {
        let Some(target) = graph.get_node(&edge.target) else {
            continue;
        };

        // Only create attachments for context-rich node types
        let target_type = target.node_type();
        if !matches!(
            target_type,
            "decision"
                | "constraint"
                | "persona"
                | "metric"
                | "risk"
                | "component"
                | "api_surface"
                | "open_question"
        ) {
            continue;
        }

        let title = node_title(target);
        let summary = body_summary(&target.body, 100);
        let subtitle = format!(
            "{} ({}) — {}",
            target_type,
            target.status().unwrap_or("—"),
            summary
        );

        attachments.push(AttachmentInput {
            title,
            subtitle,
            url: format!("tempyr://{}", target.id()),
            metadata: json!({
                "nodeId": target.id(),
                "nodeType": target_type,
                "edgeType": edge.edge_type,
                "status": target.status().unwrap_or("—"),
            }),
        });
    }

    attachments
}

// ─── Section builders ──────────────────────────────────

fn build_parent_section(node: &Node, graph: &Graph) -> String {
    let mut lines = Vec::new();

    for edge in node.edges() {
        if edge.edge_type != "child_of" {
            continue;
        }
        let Some(parent) = graph.get_node(&edge.target) else {
            continue;
        };

        let parent_type = parent.node_type();
        let label = match parent_type {
            "epic" => "Epic",
            "feature" => "Feature",
            "task" => "Parent Task",
            _ => parent_type,
        };

        let title = node_title(parent);
        let summary = body_summary(&parent.body, 200);
        let status = parent.status().unwrap_or("—");

        lines.push(format!("**{label}**: {title} ({status})"));
        if !summary.is_empty() {
            lines.push(format!("> {summary}"));
        }
        lines.push(String::new());
    }

    lines.join("\n").trim().to_string()
}

fn build_related_section(
    node: &Node,
    graph: &Graph,
    edge_types: &[&str],
    target_types: &[&str],
    heading: &str,
) -> String {
    let mut items = Vec::new();

    for edge in node.edges() {
        if !edge_types.contains(&edge.edge_type.as_str()) {
            continue;
        }
        let Some(target) = graph.get_node(&edge.target) else {
            continue;
        };
        if !target_types.contains(&target.node_type()) {
            continue;
        }

        let title = node_title(target);
        let status = target.status().unwrap_or("—");
        let summary = body_summary(&target.body, 120);

        if summary.is_empty() {
            items.push(format!("- **{title}** ({status})"));
        } else {
            items.push(format!("- **{title}** ({status}): {summary}"));
        }
    }

    if items.is_empty() {
        return String::new();
    }

    format!("+++ {heading}\n{}\n+++", items.join("\n"))
}

fn build_blocking_section(node: &Node, graph: &Graph) -> String {
    let mut items = Vec::new();

    for edge in node.edges() {
        if edge.edge_type != "blocked_by" {
            continue;
        }
        let Some(blocker) = graph.get_node(&edge.target) else {
            items.push(format!(
                "- **{}** (unresolved — node not found)",
                edge.target
            ));
            continue;
        };

        let title = node_title(blocker);
        let status = blocker.status().unwrap_or("—");
        let summary = body_summary(&blocker.body, 100);

        let marker = match blocker.node_type() {
            "open_question" => format!("[{status}]"),
            "decision" => match status {
                "decided" => "[resolved]".to_string(),
                _ => format!("[{status}]"),
            },
            _ => format!("[{status}]"),
        };

        if summary.is_empty() {
            items.push(format!("- {marker} **{title}**"));
        } else {
            items.push(format!("- {marker} **{title}** — {summary}"));
        }
    }

    items.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempyr_core::edge::EdgeEntry;
    use tempyr_core::node::{Node, NodeFrontmatter};
    use tempyr_core::schema::Schema;

    fn make_node(
        id: &str,
        node_type: &str,
        status: &str,
        body: &str,
        edges: Vec<EdgeEntry>,
    ) -> Node {
        Node {
            frontmatter: NodeFrontmatter {
                id: id.to_string(),
                node_type: node_type.to_string(),
                status: Some(status.to_string()),
                created: None,
                updated: None,
                owner: None,
                tags: None,
                edges,
            },
            body: body.to_string(),
            file_path: PathBuf::from(format!("graph/{node_type}s/{id}.md")),
            content_hash: "test".to_string(),
        }
    }

    #[test]
    fn test_build_issue_description_includes_footer() {
        let schema_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("schema/default-schema.toml");
        let schema = Schema::load(&schema_path).unwrap();
        let node = make_node(
            "task-build-auth",
            "task",
            "backlog",
            "# Build Auth\n\nImplement OAuth2 login.",
            vec![],
        );
        let graph = Graph::new(schema.clone());

        let desc = build_issue_description(&node, &graph, &schema);
        assert!(desc.contains("# Build Auth"));
        assert!(desc.contains("graph_get_node \"task-build-auth\""));
        assert!(desc.contains("**Tempyr**"));
    }

    #[test]
    fn test_build_attachments_filters_context_types() {
        let schema_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("schema/default-schema.toml");
        let schema = Schema::load(&schema_path).unwrap();

        let task = make_node(
            "task-build-auth",
            "task",
            "backlog",
            "# Build Auth",
            vec![
                EdgeEntry::new("decision-storage", "depends_on"),
                EdgeEntry::new("feat-replay", "child_of"),
            ],
        );

        let mut graph = Graph::new(schema.clone());
        graph.add_node(task.clone());
        graph.add_node(make_node(
            "decision-storage",
            "decision",
            "decided",
            "# Storage Decision\n\nUse S3.",
            vec![],
        ));
        graph.add_node(make_node(
            "feat-replay",
            "feature",
            "active",
            "# Replay",
            vec![],
        ));

        let attachments = build_attachments(&task, &graph, &schema);
        // Should include decision but not feature (feature is not a "context" type)
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].url, "tempyr://decision-storage");
    }
}
