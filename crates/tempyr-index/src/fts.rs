use crate::Result;
use crate::indexer::Index;

/// A search result from the FTS5 index.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub node_id: String,
    pub title: String,
    pub node_type: String,
    pub status: Option<String>,
    pub score: f64,
    pub snippet: String,
}

/// A node returned from a metadata-only query (no FTS ranking).
#[derive(Debug, Clone)]
pub struct ListResult {
    pub node_id: String,
    pub title: String,
    pub node_type: String,
    pub status: Option<String>,
    pub owner: Option<String>,
}

/// Filters for metadata-based queries.
#[derive(Debug, Clone, Default)]
pub struct MetadataFilter<'a> {
    pub node_type: Option<&'a str>,
    pub status: Option<&'a str>,
    pub owner: Option<&'a str>,
}

impl MetadataFilter<'_> {
    /// Build SQL WHERE conditions and positional params from the filter fields.
    /// `table_prefix` is prepended to column names (e.g. "nodes" → "nodes.status = ?N").
    /// Pass "" for no prefix. `start_idx` is the first `?N` placeholder number.
    /// Returns (conditions, boxed params, next available placeholder index).
    fn build_conditions(
        &self,
        table_prefix: &str,
        start_idx: u32,
    ) -> (Vec<String>, Vec<Box<dyn rusqlite::types::ToSql>>, u32) {
        let mut conditions = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let mut idx = start_idx;

        let prefix = if table_prefix.is_empty() {
            String::new()
        } else {
            format!("{table_prefix}.")
        };

        for (col, val) in [
            ("node_type", self.node_type),
            ("status", self.status),
            ("owner", self.owner),
        ] {
            if let Some(v) = val {
                conditions.push(format!("{prefix}{col} = ?{idx}"));
                params.push(Box::new(v.to_string()));
                idx += 1;
            }
        }

        (conditions, params, idx)
    }
}

impl Index {
    /// Full-text search using FTS5 with BM25 scoring.
    pub fn search_fts(&self, query: &str, max_results: usize) -> Result<Vec<SearchResult>> {
        self.search_fts_filtered(query, None, max_results)
    }

    /// Full-text search with optional node type filter.
    pub fn search_fts_filtered(
        &self,
        query: &str,
        node_type: Option<&str>,
        max_results: usize,
    ) -> Result<Vec<SearchResult>> {
        let filter = MetadataFilter {
            node_type,
            ..Default::default()
        };
        self.search_fts_with_metadata(query, &filter, max_results)
    }

    /// Full-text search with metadata filters (status, owner, node_type).
    pub fn search_fts_with_metadata(
        &self,
        query: &str,
        filter: &MetadataFilter,
        max_results: usize,
    ) -> Result<Vec<SearchResult>> {
        let safe_query = sanitize_fts_query(query);
        if safe_query.is_empty() {
            return Ok(Vec::new());
        }

        let mut conditions = vec!["nodes_fts MATCH ?1".to_string()];
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(safe_query)];
        let (filter_conds, filter_params, next_idx) = filter.build_conditions("nodes", 2);
        conditions.extend(filter_conds);
        params.extend(filter_params);

        let where_clause = conditions.join(" AND ");
        params.push(Box::new(max_results as i64));

        let sql = format!(
            "SELECT nodes_fts.id, nodes.title, nodes.node_type, nodes.status,
                    rank AS score,
                    snippet(nodes_fts, 2, '>>>', '<<<', '...', 32) AS snippet
             FROM nodes_fts
             JOIN nodes ON nodes.id = nodes_fts.id
             WHERE {where_clause}
             ORDER BY rank
             LIMIT ?{next_idx}"
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let results = stmt.query_map(param_refs.as_slice(), map_search_row)?;

        let mut output = Vec::new();
        for result in results {
            output.push(result?);
        }
        Ok(output)
    }

    /// Query nodes by metadata only (no full-text search). Returns nodes
    /// matching all provided filters, ordered by updated_at descending.
    pub fn query_by_metadata(
        &self,
        filter: &MetadataFilter,
        max_results: usize,
    ) -> Result<Vec<ListResult>> {
        let (conditions, mut params, next_idx) = filter.build_conditions("", 1);

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        params.push(Box::new(max_results as i64));

        let sql = format!(
            "SELECT id, title, node_type, status, owner
             FROM nodes
             {where_clause}
             ORDER BY updated_at DESC, id ASC
             LIMIT ?{next_idx}"
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let results = stmt.query_map(param_refs.as_slice(), |row| {
            Ok(ListResult {
                node_id: row.get(0)?,
                title: row.get(1)?,
                node_type: row.get(2)?,
                status: row.get(3)?,
                owner: row.get(4)?,
            })
        })?;

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
        status: row.get(3)?,
        score: row.get(4)?,
        snippet: row.get(5)?,
    })
}

/// Sanitize a user query for FTS5 by wrapping individual terms in quotes.
fn sanitize_fts_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|term| {
            // Strip any FTS5 operators for safety
            let clean: String = term
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
                .collect();
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
    use std::path::{Path, PathBuf};
    use tempyr_core::graph::Graph;
    use tempyr_core::node::parse_node;
    use tempyr_core::schema::Schema;

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
        let tasks_only = index
            .search_fts_filtered("pipeline", Some("task"), 10)
            .unwrap();

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

    #[test]
    fn test_fts_with_status_filter() {
        let index = build_test_index();
        let filter = MetadataFilter {
            status: Some("backlog"),
            ..Default::default()
        };
        let results = index
            .search_fts_with_metadata("pipeline", &filter, 10)
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].node_id, "task-ingestion");
        assert_eq!(results[0].status.as_deref(), Some("backlog"));
    }

    #[test]
    fn test_fts_with_owner_filter() {
        let index = build_test_index();
        let filter = MetadataFilter {
            owner: Some("caleb"),
            ..Default::default()
        };
        let results = index
            .search_fts_with_metadata("replay", &filter, 10)
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].node_id, "feat-replay");
    }

    #[test]
    fn test_fts_with_combined_filters() {
        let index = build_test_index();
        // type=feature + status=draft should find feat-replay
        let filter = MetadataFilter {
            node_type: Some("feature"),
            status: Some("draft"),
            ..Default::default()
        };
        let results = index
            .search_fts_with_metadata("replay", &filter, 10)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].node_id, "feat-replay");

        // type=task + status=draft should find nothing
        let filter2 = MetadataFilter {
            node_type: Some("task"),
            status: Some("draft"),
            ..Default::default()
        };
        let results2 = index
            .search_fts_with_metadata("replay", &filter2, 10)
            .unwrap();
        assert!(results2.is_empty());
    }

    #[test]
    fn test_query_by_metadata_all() {
        let index = build_test_index();
        let filter = MetadataFilter::default();
        let results = index.query_by_metadata(&filter, 100).unwrap();
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_query_by_metadata_status() {
        let index = build_test_index();
        let filter = MetadataFilter {
            status: Some("backlog"),
            ..Default::default()
        };
        let results = index.query_by_metadata(&filter, 100).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].node_id, "task-ingestion");
        assert_eq!(results[0].status.as_deref(), Some("backlog"));
    }

    #[test]
    fn test_query_by_metadata_type() {
        let index = build_test_index();
        let filter = MetadataFilter {
            node_type: Some("task"),
            ..Default::default()
        };
        let results = index.query_by_metadata(&filter, 100).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].node_type, "task");
    }

    #[test]
    fn test_query_by_metadata_owner() {
        let index = build_test_index();
        let filter = MetadataFilter {
            owner: Some("caleb"),
            ..Default::default()
        };
        let results = index.query_by_metadata(&filter, 100).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].node_id, "feat-replay");
        assert_eq!(results[0].owner.as_deref(), Some("caleb"));
    }

    #[test]
    fn test_query_by_metadata_no_match() {
        let index = build_test_index();
        let filter = MetadataFilter {
            status: Some("archived"),
            ..Default::default()
        };
        let results = index.query_by_metadata(&filter, 100).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_query_by_metadata_combined() {
        let index = build_test_index();
        let filter = MetadataFilter {
            node_type: Some("feature"),
            owner: Some("caleb"),
            ..Default::default()
        };
        let results = index.query_by_metadata(&filter, 100).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].node_id, "feat-replay");
    }

    #[test]
    fn test_search_result_includes_status() {
        let index = build_test_index();
        let results = index.search_fts("replay", 10).unwrap();
        let feat = results.iter().find(|r| r.node_id == "feat-replay").unwrap();
        assert_eq!(feat.status.as_deref(), Some("draft"));
    }
}
