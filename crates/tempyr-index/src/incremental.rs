use crate::Result;
use crate::indexer::{Index, IndexStats};
use tempyr_core::graph::Graph;

impl Index {
    /// Incremental update: only re-index nodes whose content hash has changed.
    pub fn incremental_update(&self, graph: &Graph) -> Result<IndexStats> {
        // Track which node IDs exist in the current graph
        let graph_ids: std::collections::HashSet<&str> =
            graph.nodes.keys().map(|s| s.as_str()).collect();

        // Find nodes in the index that are no longer in the graph
        let mut stmt = self.conn.prepare("SELECT id FROM nodes")?;
        let indexed_ids: Vec<String> = stmt
            .query_map([], |row| row.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        for indexed_id in &indexed_ids {
            if !graph_ids.contains(indexed_id.as_str()) {
                self.remove_node(indexed_id)?;
            }
        }

        // Add or update nodes from the graph
        for node in graph.nodes.values() {
            match self.get_content_hash(node.id())? {
                Some(existing_hash) if existing_hash == node.content_hash => {
                    // Content hasn't changed — still need to check frontmatter
                    // For now, skip entirely (optimization: could check updated_at)
                }
                Some(_) => {
                    // Content changed, re-index
                    self.remove_node(node.id())?;
                    self.insert_node(node)?;
                }
                None => {
                    // New node
                    self.insert_node(node)?;
                }
            }
        }

        self.stats()
    }

    /// Remove a node and its edges from the index.
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

        self.conn.execute(
            "DELETE FROM edges WHERE source_id = ?1 OR target_id = ?1",
            [node_id],
        )?;
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
}
