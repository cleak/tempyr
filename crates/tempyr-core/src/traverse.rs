use std::collections::{HashSet, VecDeque};

use crate::graph::Graph;

/// A single node in a traversal result.
#[derive(Debug, Clone)]
pub struct TraversalResult {
    pub node_id: String,
    pub depth: usize,
    pub path: Vec<String>,
}

/// BFS traversal from a root node with optional edge type filter.
pub fn bfs(
    graph: &Graph,
    root_id: &str,
    max_depth: usize,
    edge_type_filter: Option<&str>,
) -> Vec<TraversalResult> {
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    let mut results = Vec::new();

    if graph.get_node(root_id).is_none() {
        return results;
    }

    visited.insert(root_id.to_string());
    queue.push_back(TraversalResult {
        node_id: root_id.to_string(),
        depth: 0,
        path: vec![root_id.to_string()],
    });

    while let Some(current) = queue.pop_front() {
        results.push(current.clone());

        if current.depth >= max_depth {
            continue;
        }

        let neighbors = graph.neighbors(&current.node_id, edge_type_filter);
        for neighbor_id in neighbors {
            if visited.insert(neighbor_id.to_string()) {
                let mut path = current.path.clone();
                path.push(neighbor_id.to_string());
                queue.push_back(TraversalResult {
                    node_id: neighbor_id.to_string(),
                    depth: current.depth + 1,
                    path,
                });
            }
        }
    }

    results
}

/// BFS traversal returning nodes with scores based on hop distance.
/// Scores: hop 0 = 1.0, hop 1 = 0.8, hop 2 = 0.5 (per spec section 3.6).
pub fn bfs_scored(graph: &Graph, root_id: &str, max_depth: usize) -> Vec<(String, f64)> {
    let results = bfs(graph, root_id, max_depth, None);
    results
        .into_iter()
        .map(|r| {
            let score = hop_score(r.depth);
            (r.node_id, score)
        })
        .collect()
}

/// Score for a given hop distance.
fn hop_score(depth: usize) -> f64 {
    match depth {
        0 => 1.0,
        1 => 0.8,
        2 => 0.5,
        _ => 0.3_f64.max(0.5 - 0.1 * (depth as f64 - 2.0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Graph;
    use crate::node::parse_node;
    use crate::schema::Schema;
    use std::path::{Path, PathBuf};

    fn make_test_schema() -> Schema {
        let schema_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("schema/default-schema.toml");
        Schema::load(&schema_path).unwrap()
    }

    fn add_feature(graph: &mut Graph, id: &str, edges_yaml: &str) {
        let content = format!(
            "---\nid: {id}\ntype: feature\nstatus: draft\nowner: test\nedges:\n{edges_yaml}---\n# {id}\n"
        );
        let node = parse_node(&content, PathBuf::from(format!("{id}.md"))).unwrap();
        graph.add_node(node);
    }

    #[test]
    fn test_bfs_basic() {
        // A -> B -> C (chain of 3)
        let mut graph = Graph::new(make_test_schema());
        add_feature(&mut graph, "a", "  - target: b\n    type: depends_on\n");
        add_feature(&mut graph, "b", "  - target: c\n    type: depends_on\n");
        add_feature(&mut graph, "c", "");

        let results = bfs(&graph, "a", 2, None);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].node_id, "a");
        assert_eq!(results[0].depth, 0);
        assert_eq!(results[1].node_id, "b");
        assert_eq!(results[1].depth, 1);
        assert_eq!(results[2].node_id, "c");
        assert_eq!(results[2].depth, 2);
    }

    #[test]
    fn test_bfs_with_edge_filter() {
        let mut graph = Graph::new(make_test_schema());
        add_feature(
            &mut graph,
            "a",
            "  - target: b\n    type: depends_on\n  - target: c\n    type: serves\n",
        );
        add_feature(&mut graph, "b", "");

        // Create a persona node for "c" since serves targets persona
        let persona = "---\nid: c\ntype: persona\n---\n# Persona C\n";
        graph.add_node(parse_node(persona, PathBuf::from("c.md")).unwrap());

        let results = bfs(&graph, "a", 2, Some("depends_on"));
        assert_eq!(results.len(), 2); // a and b, not c
        assert_eq!(results[1].node_id, "b");
    }

    #[test]
    fn test_bfs_max_depth() {
        let mut graph = Graph::new(make_test_schema());
        add_feature(&mut graph, "a", "  - target: b\n    type: depends_on\n");
        add_feature(&mut graph, "b", "  - target: c\n    type: depends_on\n");
        add_feature(&mut graph, "c", "");

        let results = bfs(&graph, "a", 1, None);
        assert_eq!(results.len(), 2); // only a and b
    }

    #[test]
    fn test_bfs_cycle_handling() {
        // A -> B -> A (cycle)
        let mut graph = Graph::new(make_test_schema());
        add_feature(&mut graph, "a", "  - target: b\n    type: depends_on\n");
        add_feature(&mut graph, "b", "  - target: a\n    type: depends_on\n");

        let results = bfs(&graph, "a", 10, None);
        assert_eq!(results.len(), 2); // visits each only once
    }

    #[test]
    fn test_bfs_nonexistent_root() {
        let graph = Graph::new(make_test_schema());
        let results = bfs(&graph, "nonexistent", 2, None);
        assert!(results.is_empty());
    }

    #[test]
    fn test_bfs_scored() {
        let mut graph = Graph::new(make_test_schema());
        add_feature(&mut graph, "a", "  - target: b\n    type: depends_on\n");
        add_feature(&mut graph, "b", "  - target: c\n    type: depends_on\n");
        add_feature(&mut graph, "c", "");

        let scored = bfs_scored(&graph, "a", 2);
        assert_eq!(scored.len(), 3);
        assert_eq!(scored[0].0, "a");
        assert!((scored[0].1 - 1.0).abs() < f64::EPSILON);
        assert_eq!(scored[1].0, "b");
        assert!((scored[1].1 - 0.8).abs() < f64::EPSILON);
        assert_eq!(scored[2].0, "c");
        assert!((scored[2].1 - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_bfs_path_tracking() {
        let mut graph = Graph::new(make_test_schema());
        add_feature(&mut graph, "a", "  - target: b\n    type: depends_on\n");
        add_feature(&mut graph, "b", "  - target: c\n    type: depends_on\n");
        add_feature(&mut graph, "c", "");

        let results = bfs(&graph, "a", 2, None);
        assert_eq!(results[2].path, vec!["a", "b", "c"]);
    }
}
