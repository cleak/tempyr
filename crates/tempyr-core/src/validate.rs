use crate::graph::Graph;
use crate::node::Node;
use crate::schema::Schema;

/// A validation issue found in the graph.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidationIssue {
    pub severity: Severity,
    pub kind: ValidationKind,
    pub node_id: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ValidationKind {
    DanglingEdge,
    MissingReverseEdge,
    MissingRequiredField,
    InvalidStatus,
    InvalidEdgeType,
    InvalidNodeType,
}

impl std::fmt::Display for ValidationIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let severity = match self.severity {
            Severity::Error => "ERROR",
            Severity::Warning => "WARN",
        };
        write!(f, "[{severity}] {}: {}", self.node_id, self.message)
    }
}

/// Validate the entire graph, returning all issues found.
pub fn validate_graph(graph: &Graph) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();

    for node in graph.nodes.values() {
        issues.extend(validate_node(node, &graph.schema));
        issues.extend(validate_edges(node, graph));
    }

    issues.extend(validate_bidirectional_edges(graph));

    issues
}

/// Validate a single node against the schema.
pub fn validate_node(node: &Node, schema: &Schema) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();

    // Check node type exists in schema
    let node_def = match schema.node_types.get(node.node_type()) {
        Some(def) => def,
        None => {
            issues.push(ValidationIssue {
                severity: Severity::Error,
                kind: ValidationKind::InvalidNodeType,
                node_id: node.id().to_string(),
                message: format!("Unknown node type: '{}'", node.node_type()),
            });
            return issues;
        }
    };

    // Check required fields
    for field in &node_def.required_fields {
        let has_field = match field.as_str() {
            "status" => node.frontmatter.status.is_some(),
            "owner" => node.frontmatter.owner.is_some(),
            _ => true, // unknown required fields are not checked (future extensibility)
        };
        if !has_field {
            issues.push(ValidationIssue {
                severity: Severity::Error,
                kind: ValidationKind::MissingRequiredField,
                node_id: node.id().to_string(),
                message: format!("Missing required field: '{field}'"),
            });
        }
    }

    // Check status is valid
    if let Some(status) = node.status()
        && !node_def.allowed_statuses.is_empty()
        && !node_def.allowed_statuses.contains(&status.to_string())
    {
        issues.push(ValidationIssue {
            severity: Severity::Error,
            kind: ValidationKind::InvalidStatus,
            node_id: node.id().to_string(),
            message: format!(
                "Invalid status '{}'. Allowed: {:?}",
                status, node_def.allowed_statuses
            ),
        });
    }

    issues
}

/// Validate edges of a node: check edge types are valid per schema.
fn validate_edges(node: &Node, graph: &Graph) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();

    for edge in node.edges() {
        // Check target node exists
        if graph.get_node(&edge.target).is_none() {
            issues.push(ValidationIssue {
                severity: Severity::Error,
                kind: ValidationKind::DanglingEdge,
                node_id: node.id().to_string(),
                message: format!(
                    "Edge targets non-existent node '{}' (type: {})",
                    edge.target, edge.edge_type
                ),
            });
            continue;
        }

        // Check edge type is allowed
        let target_node = graph.get_node(&edge.target).unwrap();
        if graph
            .schema
            .validate_edge(node.node_type(), &edge.edge_type, target_node.node_type())
            .is_err()
        {
            issues.push(ValidationIssue {
                severity: Severity::Error,
                kind: ValidationKind::InvalidEdgeType,
                node_id: node.id().to_string(),
                message: format!(
                    "Edge type '{}' not allowed from '{}' ({}) to '{}' ({})",
                    edge.edge_type,
                    node.id(),
                    node.node_type(),
                    edge.target,
                    target_node.node_type()
                ),
            });
        }
    }

    issues
}

/// Check that every edge has a matching reverse edge in the target node.
pub fn validate_bidirectional_edges(graph: &Graph) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();

    for node in graph.nodes.values() {
        for edge in node.edges() {
            // Skip if target doesn't exist (already caught by validate_edges)
            let Some(target_node) = graph.get_node(&edge.target) else {
                continue;
            };

            // Get the expected reverse edge type
            let Some(reverse_type) = graph.schema.reverse_edge_type(&edge.edge_type) else {
                continue; // unknown edge type, caught elsewhere
            };

            // Check that target has reverse edge pointing back to source
            let has_reverse = target_node
                .edges()
                .iter()
                .any(|e| e.target == node.id() && e.edge_type == reverse_type);

            if !has_reverse {
                issues.push(ValidationIssue {
                    severity: Severity::Error,
                    kind: ValidationKind::MissingReverseEdge,
                    node_id: node.id().to_string(),
                    message: format!(
                        "Edge {} -> {} ({}) is missing reverse edge {} -> {} ({})",
                        node.id(),
                        edge.target,
                        edge.edge_type,
                        edge.target,
                        node.id(),
                        reverse_type
                    ),
                });
            }
        }
    }

    issues
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Graph;
    use crate::node::parse_node;
    use crate::schema::Schema;
    use std::path::{Path, PathBuf};

    fn make_schema() -> Schema {
        let schema_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("schema/default-schema.toml");
        Schema::load(&schema_path).unwrap()
    }

    #[test]
    fn test_validate_clean_graph() {
        let mut graph = Graph::new(make_schema());

        let epic = "---\nid: epic-a\ntype: epic\nstatus: draft\nowner: caleb\nedges:\n  - target: feat-a\n    type: parent_of\n---\n# Epic A\n";
        let feat = "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: caleb\nedges:\n  - target: epic-a\n    type: child_of\n---\n# Feat A\n";

        graph.add_node(parse_node(epic, PathBuf::from("epic.md")).unwrap());
        graph.add_node(parse_node(feat, PathBuf::from("feat.md")).unwrap());

        let issues = validate_graph(&graph);
        let errors: Vec<_> = issues
            .iter()
            .filter(|i| i.severity == Severity::Error)
            .collect();
        assert!(errors.is_empty(), "Expected no errors, got: {errors:?}");
    }

    #[test]
    fn test_validate_dangling_edge() {
        let mut graph = Graph::new(make_schema());

        let node = "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: caleb\nedges:\n  - target: nonexistent-node\n    type: depends_on\n---\n# Feat A\n";
        graph.add_node(parse_node(node, PathBuf::from("feat.md")).unwrap());

        let issues = validate_graph(&graph);
        assert!(
            issues
                .iter()
                .any(|i| i.kind == ValidationKind::DanglingEdge)
        );
    }

    #[test]
    fn test_validate_missing_reverse_edge() {
        let mut graph = Graph::new(make_schema());

        // feat-a has child_of -> epic-a, but epic-a does NOT have parent_of -> feat-a
        let feat = "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: caleb\nedges:\n  - target: epic-a\n    type: child_of\n---\n# Feat A\n";
        let epic = "---\nid: epic-a\ntype: epic\nstatus: draft\nowner: caleb\n---\n# Epic A\n";

        graph.add_node(parse_node(feat, PathBuf::from("feat.md")).unwrap());
        graph.add_node(parse_node(epic, PathBuf::from("epic.md")).unwrap());

        let issues = validate_graph(&graph);
        assert!(
            issues
                .iter()
                .any(|i| i.kind == ValidationKind::MissingReverseEdge)
        );
    }

    #[test]
    fn test_validate_missing_required_field() {
        let mut graph = Graph::new(make_schema());

        // Feature requires status and owner, missing owner
        let node = "---\nid: feat-a\ntype: feature\nstatus: draft\n---\n# Feat A\n";
        graph.add_node(parse_node(node, PathBuf::from("feat.md")).unwrap());

        let issues = validate_graph(&graph);
        assert!(issues.iter().any(|i| {
            i.kind == ValidationKind::MissingRequiredField && i.message.contains("owner")
        }));
    }

    #[test]
    fn test_validate_invalid_status() {
        let mut graph = Graph::new(make_schema());

        let node = "---\nid: feat-a\ntype: feature\nstatus: banana\nowner: caleb\n---\n# Feat\n";
        graph.add_node(parse_node(node, PathBuf::from("feat.md")).unwrap());

        let issues = validate_graph(&graph);
        assert!(
            issues
                .iter()
                .any(|i| i.kind == ValidationKind::InvalidStatus)
        );
    }

    #[test]
    fn test_validate_invalid_node_type() {
        let mut graph = Graph::new(make_schema());

        let node = "---\nid: thing-a\ntype: made_up_type\n---\n# Thing\n";
        graph.add_node(parse_node(node, PathBuf::from("thing.md")).unwrap());

        let issues = validate_graph(&graph);
        assert!(
            issues
                .iter()
                .any(|i| i.kind == ValidationKind::InvalidNodeType)
        );
    }

    #[test]
    fn test_validate_persona_no_status_required() {
        let mut graph = Graph::new(make_schema());

        // Persona has no required fields
        let node = "---\nid: persona-dev\ntype: persona\n---\n# Developer\n";
        graph.add_node(parse_node(node, PathBuf::from("p.md")).unwrap());

        let issues = validate_graph(&graph);
        let errors: Vec<_> = issues
            .iter()
            .filter(|i| i.severity == Severity::Error)
            .collect();
        assert!(errors.is_empty());
    }
}
