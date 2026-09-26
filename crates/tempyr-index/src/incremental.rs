use std::collections::HashMap;

use crate::Result;
use crate::indexer::{INDEX_SCHEMA_VERSION, Index, IndexStats, node_index_hash};
use tempyr_core::graph::Graph;

impl Index {
    /// Incremental update: re-index only the nodes whose indexed values
    /// changed, and drop nodes that left the graph.
    ///
    /// Change detection compares each node's `index_hash`, which covers the
    /// frontmatter and edges as well as the body, so the result always matches
    /// [`Index::rebuild`] for the same graph. An index stamped with a different
    /// [`INDEX_SCHEMA_VERSION`] is rebuilt instead.
    ///
    /// Runs as one transaction: on error the index is left unchanged.
    pub fn incremental_update(&self, graph: &Graph) -> Result<IndexStats> {
        if self.schema_version()? != INDEX_SCHEMA_VERSION {
            return self.rebuild(graph);
        }

        let tx = self.conn.unchecked_transaction()?;

        let indexed: HashMap<String, Option<String>> = {
            let mut stmt = self.conn.prepare("SELECT id, index_hash FROM nodes")?;
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<std::result::Result<_, _>>()?
        };

        for indexed_id in indexed.keys() {
            if !graph.nodes.contains_key(indexed_id) {
                self.remove_node(indexed_id)?;
            }
        }

        for node in graph.nodes.values() {
            match indexed.get(node.id()) {
                Some(Some(existing)) if *existing == node_index_hash(node) => {}
                Some(_) => {
                    self.remove_node(node.id())?;
                    self.insert_node(node)?;
                }
                None => self.insert_node(node)?,
            }
        }

        tx.commit()?;

        self.stats()
    }

    /// Remove a node, its FTS entry, and the edges it owns from the index.
    ///
    /// Edge rows belong to their source node (they come from its frontmatter),
    /// so edges from other nodes that point at this one are kept, exactly as a
    /// full rebuild would keep them.
    pub fn remove_node(&self, node_id: &str) -> Result<()> {
        // Remove FTS entry first (need the rowid)
        let rowid_result: std::result::Result<i64, _> =
            self.conn
                .query_row("SELECT rowid FROM nodes WHERE id = ?1", [node_id], |row| {
                    row.get(0)
                });

        if let Ok(rowid) = rowid_result {
            self.conn.execute(
                "INSERT INTO nodes_fts(nodes_fts, rowid, id, title, body_text, tags) VALUES('delete', ?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    rowid,
                    node_id,
                    self.get_title(node_id)?.unwrap_or_default(),
                    self.get_body_text(node_id)?.unwrap_or_default(),
                    self.get_tags(node_id)?.unwrap_or_default(),
                ],
            )?;
        }

        self.conn
            .execute("DELETE FROM edges WHERE source_id = ?1", [node_id])?;
        self.conn
            .execute("DELETE FROM nodes WHERE id = ?1", [node_id])?;

        Ok(())
    }

    fn get_title(&self, node_id: &str) -> Result<Option<String>> {
        let result =
            self.conn
                .query_row("SELECT title FROM nodes WHERE id = ?1", [node_id], |row| {
                    row.get(0)
                });
        match result {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn get_tags(&self, node_id: &str) -> Result<Option<String>> {
        let result =
            self.conn
                .query_row("SELECT tags FROM nodes WHERE id = ?1", [node_id], |row| {
                    row.get(0)
                });
        match result {
            Ok(v) => Ok(v),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn test_incremental_add_new() {
        let mut graph = Graph::new(make_schema());
        let feat = "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: alice\n---\n# A\n";
        graph.add_node(parse_node(feat, PathBuf::from("f.md")).unwrap());

        let index = Index::create_in_memory().unwrap();
        index.rebuild(&graph).unwrap();

        // Add a new node
        let task = "---\nid: task-b\ntype: task\nstatus: backlog\n---\n# B\n";
        graph.add_node(parse_node(task, PathBuf::from("t.md")).unwrap());

        let stats = index.incremental_update(&graph).unwrap();
        assert_eq!(stats.node_count, 2);
    }

    #[test]
    fn test_incremental_update_changed() {
        let mut graph = Graph::new(make_schema());
        let feat = "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: alice\n---\n# A\n\nOriginal body.\n";
        graph.add_node(parse_node(feat, PathBuf::from("f.md")).unwrap());

        let index = Index::create_in_memory().unwrap();
        index.rebuild(&graph).unwrap();

        // Update the body (changes content hash)
        let updated = "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: alice\n---\n# A\n\nUpdated body content.\n";
        graph.add_node(parse_node(updated, PathBuf::from("f.md")).unwrap());

        let stats = index.incremental_update(&graph).unwrap();
        assert_eq!(stats.node_count, 1);

        // Verify the body was updated
        let body = index.get_body_text("feat-a").unwrap().unwrap();
        assert!(body.contains("Updated body"));
    }

    #[test]
    fn test_incremental_remove_deleted() {
        let mut graph = Graph::new(make_schema());
        let feat = "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: alice\n---\n# A\n";
        let task = "---\nid: task-b\ntype: task\nstatus: backlog\n---\n# B\n";
        graph.add_node(parse_node(feat, PathBuf::from("f.md")).unwrap());
        graph.add_node(parse_node(task, PathBuf::from("t.md")).unwrap());

        let index = Index::create_in_memory().unwrap();
        index.rebuild(&graph).unwrap();
        assert_eq!(index.stats().unwrap().node_count, 2);

        // Remove one node
        graph.remove_node("task-b");

        let stats = index.incremental_update(&graph).unwrap();
        assert_eq!(stats.node_count, 1);
    }

    #[test]
    fn test_incremental_no_changes() {
        let mut graph = Graph::new(make_schema());
        let feat = "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: alice\n---\n# A\n";
        graph.add_node(parse_node(feat, PathBuf::from("f.md")).unwrap());

        let index = Index::create_in_memory().unwrap();
        index.rebuild(&graph).unwrap();

        // No changes
        let stats = index.incremental_update(&graph).unwrap();
        assert_eq!(stats.node_count, 1);
    }

    const FEAT: &str = "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: alice\nedges:\n  - target: epic-a\n    type: child_of\n---\n# Feature A\n\nSession replay capture.\n";
    const EPIC: &str = "---\nid: epic-a\ntype: epic\nstatus: active\nowner: alice\nedges:\n  - target: feat-a\n    type: parent_of\n---\n# Epic A\n\nObservability.\n";
    const TASK: &str =
        "---\nid: task-a\ntype: task\nstatus: backlog\n---\n# Task A\n\nIngestion pipeline.\n";

    /// Insert or replace a node, giving it a file path derived from its id.
    fn put(graph: &mut Graph, content: &str) {
        let mut node = parse_node(content, PathBuf::new()).unwrap();
        node.file_path = PathBuf::from(format!("{}.md", node.id()));
        graph.add_node(node);
    }

    fn base_graph() -> Graph {
        let mut graph = Graph::new(make_schema());
        for content in [FEAT, EPIC, TASK] {
            put(&mut graph, content);
        }
        graph
    }

    fn edges(index: &Index) -> Vec<(String, String, String)> {
        let mut stmt = index
            .conn
            .prepare("SELECT source_id, target_id, edge_type FROM edges ORDER BY 1, 2, 3")
            .unwrap();
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap()
    }

    fn edge(source: &str, target: &str, edge_type: &str) -> (String, String, String) {
        (source.into(), target.into(), edge_type.into())
    }

    type Rows = Vec<Vec<Option<String>>>;
    type Mutation = Box<dyn Fn(&mut Graph)>;

    /// Every stored `(nodes, edges)` value except SQLite rowids, in a stable
    /// order, after an FTS5 integrity check (which fails if `nodes_fts`
    /// disagrees with `nodes`).
    fn dump(index: &Index) -> (Rows, Rows) {
        index
            .conn
            .execute(
                "INSERT INTO nodes_fts(nodes_fts, rank) VALUES('integrity-check', 1)",
                [],
            )
            .expect("FTS index out of sync with nodes");
        let rows = |sql: &str, width: usize| -> Rows {
            let mut stmt = index.conn.prepare(sql).unwrap();
            stmt.query_map([], |row| {
                (0..width)
                    .map(|i| row.get(i))
                    .collect::<rusqlite::Result<_>>()
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
        };
        (
            rows(
                "SELECT id, node_type, status, owner, title, body_text, file_path, created_at, \
                 updated_at, tags, content_hash, index_hash FROM nodes ORDER BY id",
                12,
            ),
            rows(
                "SELECT source_id, target_id, edge_type, valid_from, valid_until, annotation \
                 FROM edges ORDER BY 1, 2, 3",
                6,
            ),
        )
    }

    fn rebuilt(graph: &Graph) -> Index {
        let index = Index::create_in_memory().unwrap();
        index.rebuild(graph).unwrap();
        index
    }

    /// Regression for #52: a frontmatter-only edit (as `add-edge` /
    /// `remove-edge` make) must reach an index seeded from the previous state.
    #[test]
    fn test_incremental_applies_edge_only_change() {
        let mut graph = base_graph();
        let index = rebuilt(&graph);

        put(
            &mut graph,
            &FEAT.replace(
                "  - target: epic-a\n    type: child_of\n",
                "  - target: task-a\n    type: parent_of\n",
            ),
        );
        index.incremental_update(&graph).unwrap();

        let edges = edges(&index);
        assert!(edges.contains(&edge("feat-a", "task-a", "parent_of")));
        assert!(!edges.contains(&edge("feat-a", "epic-a", "child_of")));
    }

    #[test]
    fn test_incremental_applies_status_change() {
        let mut graph = base_graph();
        let index = rebuilt(&graph);

        put(&mut graph, &TASK.replace("status: backlog", "status: done"));
        index.incremental_update(&graph).unwrap();

        let status: String = index
            .conn
            .query_row("SELECT status FROM nodes WHERE id = 'task-a'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(status, "done");
    }

    #[test]
    fn test_incremental_keeps_incoming_edges_of_reindexed_node() {
        let mut graph = base_graph();
        let index = rebuilt(&graph);

        // Only epic-a changes; feat-a's edge pointing at it is feat-a's row.
        put(&mut graph, &EPIC.replace("Observability.", "Tracing."));
        index.incremental_update(&graph).unwrap();

        assert!(edges(&index).contains(&edge("feat-a", "epic-a", "child_of")));
    }

    #[test]
    fn test_incremental_matches_rebuild_across_mutations() {
        let mut graph = base_graph();
        let index = rebuilt(&graph);

        let steps: Vec<Mutation> = vec![
            Box::new(|g| put(g, &FEAT.replace("Session replay", "Session recording"))),
            Box::new(|g| put(g, &TASK.replace("status: backlog", "status: in_progress"))),
            Box::new(|g| {
                put(
                    g,
                    &TASK.replace("---\n# Task", "tags: [ingest]\nowner: bob\n---\n# Task"),
                )
            }),
            Box::new(|g| {
                put(
                    g,
                    &EPIC.replace(
                        "    type: parent_of\n",
                        "    type: parent_of\n    annotation: scope\n  - target: task-a\n    type: parent_of\n",
                    ),
                )
            }),
            Box::new(|g| {
                g.remove_node("task-a");
            }),
            Box::new(|g| put(g, TASK)),
        ];

        for (step, mutate) in steps.iter().enumerate() {
            mutate(&mut graph);
            index.incremental_update(&graph).unwrap();
            assert_eq!(
                dump(&index),
                dump(&rebuilt(&graph)),
                "incremental index diverged from rebuild after step {step}"
            );
        }
    }

    #[test]
    fn test_incremental_updates_fts() {
        let mut graph = base_graph();
        let index = rebuilt(&graph);

        put(&mut graph, &FEAT.replace("Session replay", "Heatmap"));
        index.incremental_update(&graph).unwrap();

        let hits = |term: &str| -> i64 {
            index
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM nodes_fts WHERE nodes_fts MATCH ?1",
                    [term],
                    |row| row.get(0),
                )
                .unwrap()
        };
        assert_eq!(hits("heatmap"), 1);
        assert_eq!(hits("replay"), 0);
    }

    #[test]
    fn test_incremental_commits_once() {
        let mut graph = base_graph();
        let index = rebuilt(&graph);
        let commits = crate::test_support::commit_counter(&index.conn);

        put(&mut graph, &FEAT.replace("Session replay", "Heatmap"));
        put(&mut graph, &TASK.replace("status: backlog", "status: done"));
        graph.remove_node("epic-a");
        index.incremental_update(&graph).unwrap();

        assert_eq!(commits.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn test_incremental_rolls_back_on_error() {
        let mut graph = base_graph();
        let index = rebuilt(&graph);
        let before = dump(&index);
        index
            .conn
            .execute_batch(
                "CREATE TRIGGER reject_boom BEFORE INSERT ON edges WHEN NEW.target_id = 'boom'
                 BEGIN SELECT RAISE(ABORT, 'boom'); END;",
            )
            .unwrap();

        put(&mut graph, &FEAT.replace("Session replay", "Heatmap"));
        graph.remove_node("task-a");
        put(
            &mut graph,
            "---\nid: task-b\ntype: task\nedges:\n  - target: boom\n    type: relates_to\n---\n# B\n",
        );

        assert!(index.incremental_update(&graph).is_err());
        assert_eq!(dump(&index), before);
    }

    #[test]
    fn test_incremental_rebuilds_index_with_other_schema_version() {
        let mut graph = base_graph();
        let index = rebuilt(&graph);
        index.conn.pragma_update(None, "user_version", 0).unwrap();

        put(&mut graph, &TASK.replace("status: backlog", "status: done"));
        index.incremental_update(&graph).unwrap();

        assert_eq!(index.schema_version().unwrap(), INDEX_SCHEMA_VERSION);
        assert_eq!(dump(&index), dump(&rebuilt(&graph)));
    }

    #[test]
    fn test_incremental_reindexes_rows_without_index_hash() {
        let graph = base_graph();
        let index = rebuilt(&graph);
        index
            .conn
            .execute("UPDATE nodes SET index_hash = NULL", [])
            .unwrap();

        index.incremental_update(&graph).unwrap();

        assert_eq!(dump(&index), dump(&rebuilt(&graph)));
    }
}
