use std::collections::HashMap;
use std::path::Path;

use walkdir::WalkDir;

use crate::Result;
use crate::edge::EdgeEntry;
use crate::node::{Node, parse_node};
use crate::schema::Schema;

/// In-memory representation of the full knowledge graph.
#[derive(Debug)]
pub struct Graph {
    pub nodes: HashMap<String, Node>,
    pub schema: Schema,
}

impl Graph {
    /// Create an empty graph with the given schema.
    pub fn new(schema: Schema) -> Self {
        Self {
            nodes: HashMap::new(),
            schema,
        }
    }

    /// Load a graph from a directory of markdown node files.
    pub fn load_from_directory(graph_dir: &Path, schema: Schema) -> Result<Self> {
        let mut graph = Self::new(schema);

        if !graph_dir.exists() {
            return Ok(graph);
        }

        for entry in WalkDir::new(graph_dir)
            .min_depth(2) // skip the graph/ dir itself and type dirs
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "md") {
                let content = std::fs::read_to_string(path)?;
                match parse_node(&content, path.to_path_buf()) {
                    Ok(node) => {
                        graph.nodes.insert(node.id().to_string(), node);
                    }
                    Err(e) => {
                        eprintln!("Warning: skipping {}: {e}", path.display());
                    }
                }
            }
        }

        Ok(graph)
    }

    /// Get a node by ID.
    pub fn get_node(&self, id: &str) -> Option<&Node> {
        self.nodes.get(id)
    }

    /// Get all nodes of a given type.
    pub fn nodes_of_type(&self, node_type: &str) -> Vec<&Node> {
        self.nodes
            .values()
            .filter(|n| n.node_type() == node_type)
            .collect()
    }

    /// Get outgoing edges from a node.
    pub fn outgoing_edges(&self, node_id: &str) -> Vec<&EdgeEntry> {
        self.nodes
            .get(node_id)
            .map(|n| n.edges().iter().collect())
            .unwrap_or_default()
    }

    /// Get incoming edges to a node (source_node_id, edge_entry).
    pub fn incoming_edges(&self, node_id: &str) -> Vec<(&str, &EdgeEntry)> {
        let mut result = Vec::new();
        for node in self.nodes.values() {
            for edge in node.edges() {
                if edge.target == node_id {
                    result.push((node.id(), edge));
                }
            }
        }
        result
    }

    /// Get neighbor node IDs reachable via outgoing edges, optionally filtered by edge type.
    pub fn neighbors(&self, node_id: &str, edge_type: Option<&str>) -> Vec<&str> {
        self.outgoing_edges(node_id)
            .into_iter()
            .filter(|e| edge_type.is_none() || Some(e.edge_type.as_str()) == edge_type)
            .filter_map(|e| {
                if self.nodes.contains_key(&e.target) {
                    Some(e.target.as_str())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Add a node to the graph (does not write to disk).
    pub fn add_node(&mut self, node: Node) {
        self.nodes.insert(node.id().to_string(), node);
    }

    /// Remove a node from the graph (does not modify disk).
    pub fn remove_node(&mut self, id: &str) -> Option<Node> {
        self.nodes.remove(id)
    }

    /// Get the number of nodes in the graph.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Get the total number of edges across all nodes.
    pub fn edge_count(&self) -> usize {
        self.nodes.values().map(|n| n.edges().len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::parse_node;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn make_test_schema() -> Schema {
        let schema_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("schema/default-schema.toml");
        Schema::load(&schema_path).unwrap()
    }

    fn make_feature_node(id: &str, edges: &str) -> Node {
        let content = format!(
            "---\nid: {id}\ntype: feature\nstatus: draft\nowner: caleb\nedges:\n{edges}---\n# {id}\n\nBody of {id}.\n"
        );
        parse_node(&content, PathBuf::from(format!("graph/features/{id}.md"))).unwrap()
    }

    #[test]
    fn test_graph_load_directory() {
        let tmp = TempDir::new().unwrap();
        let graph_dir = tmp.path().join("graph");
        let features_dir = graph_dir.join("features");
        std::fs::create_dir_all(&features_dir).unwrap();

        std::fs::write(
            features_dir.join("feat-a.md"),
            "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: test\n---\n# Feat A\n",
        )
        .unwrap();
        std::fs::write(
            features_dir.join("feat-b.md"),
            "---\nid: feat-b\ntype: feature\nstatus: active\nowner: test\n---\n# Feat B\n",
        )
        .unwrap();

        let schema = make_test_schema();
        let graph = Graph::load_from_directory(&graph_dir, schema).unwrap();

        assert_eq!(graph.node_count(), 2);
        assert!(graph.get_node("feat-a").is_some());
        assert!(graph.get_node("feat-b").is_some());
    }

    #[test]
    fn test_graph_neighbors() {
        let schema = make_test_schema();
        let mut graph = Graph::new(schema);

        let node_a = make_feature_node(
            "feat-a",
            "  - target: feat-b\n    type: depends_on\n  - target: feat-c\n    type: depends_on\n",
        );
        let node_b = make_feature_node("feat-b", "");
        let node_c = make_feature_node("feat-c", "");

        graph.add_node(node_a);
        graph.add_node(node_b);
        graph.add_node(node_c);

        let neighbors = graph.neighbors("feat-a", None);
        assert_eq!(neighbors.len(), 2);
        assert!(neighbors.contains(&"feat-b"));
        assert!(neighbors.contains(&"feat-c"));

        let filtered = graph.neighbors("feat-a", Some("depends_on"));
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn test_graph_incoming_edges() {
        let schema = make_test_schema();
        let mut graph = Graph::new(schema);

        let node_a = make_feature_node("feat-a", "  - target: feat-b\n    type: depends_on\n");
        let node_b = make_feature_node("feat-b", "");

        graph.add_node(node_a);
        graph.add_node(node_b);

        let incoming = graph.incoming_edges("feat-b");
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0].0, "feat-a");
        assert_eq!(incoming[0].1.edge_type, "depends_on");
    }

    #[test]
    fn test_graph_nodes_of_type() {
        let schema = make_test_schema();
        let mut graph = Graph::new(schema);

        graph.add_node(make_feature_node("feat-a", ""));
        graph.add_node(make_feature_node("feat-b", ""));

        let task_content = "---\nid: task-a\ntype: task\nstatus: backlog\n---\n# Task A\n";
        graph.add_node(parse_node(task_content, PathBuf::from("t.md")).unwrap());

        assert_eq!(graph.nodes_of_type("feature").len(), 2);
        assert_eq!(graph.nodes_of_type("task").len(), 1);
        assert_eq!(graph.nodes_of_type("decision").len(), 0);
    }
}
