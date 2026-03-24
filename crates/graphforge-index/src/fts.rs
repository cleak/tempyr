use crate::indexer::Index;
use crate::Result;

/// A search result from the FTS5 index.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub node_id: String,
    pub title: String,
    pub node_type: String,
    pub score: f64,
    pub snippet: String,
}

impl Index {
    /// Full-text search using FTS5 with BM25 scoring.
    pub fn search_fts(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<SearchResult>> {
        self.search_fts_filtered(query, None, max_results)
    }

    /// Full-text search with optional node type filter.
    pub fn search_fts_filtered(
        &self,
        query: &str,
        node_type: Option<&str>,
        max_results: usize,
    ) -> Result<Vec<SearchResult>> {
        // Escape the query for FTS5 (wrap terms in quotes for safety)
        let safe_query = sanitize_fts_query(query);
        if safe_query.is_empty() {
            return Ok(Vec::new());
        }

        let sql = if node_type.is_some() {
            "SELECT nodes_fts.id, nodes.title, nodes.node_type,
                    rank AS score,
                    snippet(nodes_fts, 2, '>>>', '<<<', '...', 32) AS snippet
             FROM nodes_fts
             JOIN nodes ON nodes.id = nodes_fts.id
             WHERE nodes_fts MATCH ?1 AND nodes.node_type = ?2
             ORDER BY rank
             LIMIT ?3"
        } else {
            "SELECT nodes_fts.id, nodes.title, nodes.node_type,
                    rank AS score,
                    snippet(nodes_fts, 2, '>>>', '<<<', '...', 32) AS snippet
             FROM nodes_fts
             JOIN nodes ON nodes.id = nodes_fts.id
             WHERE nodes_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2"
        };

        let mut stmt = self.conn.prepare(sql)?;

        let results = if let Some(nt) = node_type {
            stmt.query_map(
                rusqlite::params![safe_query, nt, max_results],
                map_search_row,
            )?
        } else {
            stmt.query_map(
                rusqlite::params![safe_query, max_results],
                map_search_row,
            )?
        };

        let mut output = Vec::new();
        for result in results {
            output.push(result?);
        }

        Ok(output)
    }
}

fn map_search_row(row: &rusqlite::Row) -> rusqlite::Result<SearchResult> {
    Ok(SearchResult {
        node_id: row.get(0)?,
        title: row.get(1)?,
        node_type: row.get(2)?,
        score: row.get(3)?,
        snippet: row.get(4)?,
    })
}

/// Sanitize a user query for FTS5 by wrapping individual terms in quotes.
fn sanitize_fts_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|term| {
            // Strip any FTS5 operators for safety
            let clean: String = term.chars().filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_').collect();
            if clean.is_empty() {
                String::new()
            } else {
                format!("\"{clean}\"")
            }
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" OR ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::Index;
    use graphforge_core::graph::Graph;
    use graphforge_core::node::parse_node;
    use graphforge_core::schema::Schema;
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

    fn build_test_index() -> Index {
        let mut graph = Graph::new(make_schema());

        let feat = "---\nid: feat-replay\ntype: feature\nstatus: draft\nowner: caleb\ntags: [replay, observability]\n---\n# Session Replay\n\nCapture and replay user sessions for debugging funnel drop-offs.\n";
        let decision = "---\nid: decision-storage\ntype: decision\nstatus: decided\n---\n# Storage Backend Decision\n\nWe decided to use ClickHouse for replay event storage due to high write throughput.\n";
        let task = "---\nid: task-ingestion\ntype: task\nstatus: backlog\n---\n# Build Ingestion Pipeline\n\nImplement the event ingestion pipeline for session replay data.\n";

        graph.add_node(parse_node(feat, PathBuf::from("f.md")).unwrap());
        graph.add_node(parse_node(decision, PathBuf::from("d.md")).unwrap());
        graph.add_node(parse_node(task, PathBuf::from("t.md")).unwrap());

        let index = Index::create_in_memory().unwrap();
        index.rebuild(&graph).unwrap();
        index
    }

    #[test]
    fn test_fts_basic_search() {
        let index = build_test_index();
        let results = index.search_fts("replay", 10).unwrap();

        assert!(!results.is_empty());
        // "replay" appears in feat-replay and task-ingestion
        let ids: Vec<_> = results.iter().map(|r| r.node_id.as_str()).collect();
        assert!(ids.contains(&"feat-replay"));
    }

    #[test]
    fn test_fts_no_results() {
        let index = build_test_index();
        let results = index.search_fts("xyznonexistent", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_fts_filtered_by_type() {
        let index = build_test_index();

        let all = index.search_fts("pipeline", 10).unwrap();
        let tasks_only = index.search_fts_filtered("pipeline", Some("task"), 10).unwrap();

        assert!(all.len() >= tasks_only.len());
        for r in &tasks_only {
            assert_eq!(r.node_type, "task");
        }
    }

    #[test]
    fn test_fts_title_scores_higher() {
        let index = build_test_index();
        // "storage" appears in decision title + body
        let results = index.search_fts("storage", 10).unwrap();
        assert!(!results.is_empty());
        // The decision about storage should be highly ranked
        assert_eq!(results[0].node_id, "decision-storage");
    }

    #[test]
    fn test_fts_empty_query() {
        let index = build_test_index();
        let results = index.search_fts("", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_sanitize_fts_query() {
        assert_eq!(sanitize_fts_query("hello world"), "\"hello\" OR \"world\"");
        assert_eq!(sanitize_fts_query("  "), "");
        assert_eq!(sanitize_fts_query("test-node"), "\"test-node\"");
    }
}
