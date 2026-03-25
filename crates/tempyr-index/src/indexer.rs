use std::path::Path;

use rusqlite::Connection;

use tempyr_core::graph::Graph;
use tempyr_core::node::Node;

use crate::Result;

/// Statistics about the index.
#[derive(Debug, Clone, Default)]
pub struct IndexStats {
    pub node_count: usize,
    pub edge_count: usize,
    pub fts_entries: usize,
    pub nodes_by_type: Vec<(String, usize)>,
}

/// The SQLite index wrapping a database connection.
pub struct Index {
    pub(crate) conn: Connection,
}

impl Index {
    /// Create a new index database at the given path.
    pub fn create(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        let index = Self { conn };
        index.create_tables()?;
        index.create_embedding_tables()?;
        Ok(index)
    }

    /// Create an in-memory index (for testing).
    pub fn create_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let index = Self { conn };
        index.create_tables()?;
        index.create_embedding_tables()?;
        Ok(index)
    }

    /// Open an existing index database.
    pub fn open(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Err(crate::IndexError::General(format!(
                "Index database not found: {}",
                path.display()
            )));
        }
        let conn = Connection::open(path)?;
        Ok(Self { conn })
    }

    /// Create the database tables.
    fn create_tables(&self) -> Result<()> {
        // Disable FK enforcement — the index is a derived artifact,
        // and edges may reference nodes not yet inserted during rebuild.
        self.conn.execute_batch("PRAGMA foreign_keys = OFF;")?;

        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS nodes (
                id          TEXT PRIMARY KEY,
                node_type   TEXT NOT NULL,
                status      TEXT,
                owner       TEXT,
                title       TEXT,
                body_text   TEXT,
                file_path   TEXT NOT NULL,
                created_at  TEXT,
                updated_at  TEXT,
                tags        TEXT,
                content_hash TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS edges (
                source_id   TEXT NOT NULL,
                target_id   TEXT NOT NULL,
                edge_type   TEXT NOT NULL,
                valid_from  TEXT,
                valid_until TEXT,
                annotation  TEXT,
                PRIMARY KEY (source_id, target_id, edge_type)
            );

            CREATE INDEX IF NOT EXISTS idx_edges_target ON edges(target_id);
            CREATE INDEX IF NOT EXISTS idx_edges_type ON edges(edge_type);
            CREATE INDEX IF NOT EXISTS idx_nodes_type ON nodes(node_type);
            CREATE INDEX IF NOT EXISTS idx_nodes_status ON nodes(status);

            CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts USING fts5(
                id,
                title,
                body_text,
                tags,
                content='nodes',
                content_rowid='rowid',
                tokenize='porter unicode61'
            );
            ",
        )?;
        Ok(())
    }

    /// Full rebuild: drop all data and reindex from the graph.
    pub fn rebuild(&self, graph: &Graph) -> Result<IndexStats> {
        self.conn.execute_batch(
            "
            DELETE FROM edges;
            DELETE FROM nodes_fts;
            DELETE FROM nodes;
            ",
        )?;

        for node in graph.nodes.values() {
            self.insert_node(node)?;
        }

        self.stats()
    }

    /// Insert a single node and its edges into the index.
    pub(crate) fn insert_node(&self, node: &Node) -> Result<()> {
        let title = node.title().to_string();
        let tags_json = node
            .frontmatter
            .tags
            .as_ref()
            .map(|t| serde_json::to_string(t).unwrap_or_default());

        self.conn.execute(
            "INSERT OR REPLACE INTO nodes (id, node_type, status, owner, title, body_text, file_path, created_at, updated_at, tags, content_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                node.id(),
                node.node_type(),
                node.status(),
                node.frontmatter.owner.as_deref(),
                title,
                node.body,
                node.file_path.to_string_lossy().to_string(),
                node.frontmatter.created.map(|c| c.to_rfc3339()),
                node.frontmatter.updated.map(|u| u.to_rfc3339()),
                tags_json,
                node.content_hash,
            ],
        )?;

        // Insert into FTS
        // Get the rowid of the just-inserted node
        let rowid: i64 = self.conn.query_row(
            "SELECT rowid FROM nodes WHERE id = ?1",
            [node.id()],
            |row| row.get(0),
        )?;

        self.conn.execute(
            "INSERT INTO nodes_fts(rowid, id, title, body_text, tags) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                rowid,
                node.id(),
                title,
                node.body,
                tags_json.as_deref().unwrap_or(""),
            ],
        )?;

        // Insert edges
        for edge in node.edges() {
            self.conn.execute(
                "INSERT OR IGNORE INTO edges (source_id, target_id, edge_type, valid_from, valid_until, annotation)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    node.id(),
                    edge.target,
                    edge.edge_type,
                    edge.valid_from.map(|d| d.to_string()),
                    edge.valid_until.map(|d| d.to_string()),
                    edge.annotation,
                ],
            )?;
        }

        Ok(())
    }

    /// Get index statistics.
    pub fn stats(&self) -> Result<IndexStats> {
        let node_count: usize = self
            .conn
            .query_row("SELECT COUNT(*) FROM nodes", [], |row| row.get(0))?;

        let edge_count: usize = self
            .conn
            .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))?;

        let fts_entries: usize = self.conn.query_row(
            "SELECT COUNT(*) FROM nodes_fts",
            [],
            |row| row.get(0),
        )?;

        let mut stmt = self
            .conn
            .prepare("SELECT node_type, COUNT(*) FROM nodes GROUP BY node_type ORDER BY node_type")?;
        let nodes_by_type = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, usize>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(IndexStats {
            node_count,
            edge_count,
            fts_entries,
            nodes_by_type,
        })
    }

    /// Get the content hash of a node in the index.
    pub fn get_content_hash(&self, node_id: &str) -> Result<Option<String>> {
        let result = self.conn.query_row(
            "SELECT content_hash FROM nodes WHERE id = ?1",
            [node_id],
            |row| row.get(0),
        );

        match result {
            Ok(hash) => Ok(Some(hash)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Get the body text of a node from the index.
    pub fn get_body_text(&self, node_id: &str) -> Result<Option<String>> {
        let result = self.conn.query_row(
            "SELECT body_text FROM nodes WHERE id = ?1",
            [node_id],
            |row| row.get(0),
        );

        match result {
            Ok(body) => Ok(Some(body)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Get the updated_at timestamp of a node from the index.
    pub fn get_updated_at(&self, node_id: &str) -> Result<Option<String>> {
        let result = self.conn.query_row(
            "SELECT updated_at FROM nodes WHERE id = ?1",
            [node_id],
            |row| row.get(0),
        );

        match result {
            Ok(ts) => Ok(ts),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Get the node type for a node from the index.
    pub fn get_node_type(&self, node_id: &str) -> Result<Option<String>> {
        let result = self.conn.query_row(
            "SELECT node_type FROM nodes WHERE id = ?1",
            [node_id],
            |row| row.get(0),
        );

        match result {
            Ok(nt) => Ok(Some(nt)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

// serde_json is used for tags serialization
use serde_json as _;

#[cfg(test)]
mod tests {
    use super::*;
    use tempyr_core::graph::Graph;
    use tempyr_core::node::parse_node;
    use tempyr_core::schema::Schema;
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

    fn make_test_graph() -> Graph {
        let mut graph = Graph::new(make_schema());

        let feat = "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: caleb\ntags: [replay, test]\nedges:\n  - target: epic-a\n    type: child_of\n---\n# Feature A\n\nThis feature handles session replay.\n";
        let epic = "---\nid: epic-a\ntype: epic\nstatus: active\nowner: caleb\nedges:\n  - target: feat-a\n    type: parent_of\n---\n# Epic A\n\nThe observability epic.\n";
        let task = "---\nid: task-a\ntype: task\nstatus: backlog\n---\n# Task A\n\nImplement ingestion pipeline.\n";

        graph.add_node(parse_node(feat, PathBuf::from("feat.md")).unwrap());
        graph.add_node(parse_node(epic, PathBuf::from("epic.md")).unwrap());
        graph.add_node(parse_node(task, PathBuf::from("task.md")).unwrap());

        graph
    }

    #[test]
    fn test_create_index() {
        let index = Index::create_in_memory().unwrap();
        let stats = index.stats().unwrap();
        assert_eq!(stats.node_count, 0);
        assert_eq!(stats.edge_count, 0);
    }

    #[test]
    fn test_rebuild_from_graph() {
        let graph = make_test_graph();
        let index = Index::create_in_memory().unwrap();
        let stats = index.rebuild(&graph).unwrap();

        assert_eq!(stats.node_count, 3);
        assert_eq!(stats.edge_count, 2); // child_of + parent_of
        assert_eq!(stats.fts_entries, 3);
    }

    #[test]
    fn test_stats_by_type() {
        let graph = make_test_graph();
        let index = Index::create_in_memory().unwrap();
        let stats = index.rebuild(&graph).unwrap();

        let type_map: std::collections::HashMap<_, _> = stats.nodes_by_type.into_iter().collect();
        assert_eq!(type_map.get("feature"), Some(&1));
        assert_eq!(type_map.get("epic"), Some(&1));
        assert_eq!(type_map.get("task"), Some(&1));
    }

    #[test]
    fn test_content_hash_lookup() {
        let graph = make_test_graph();
        let index = Index::create_in_memory().unwrap();
        index.rebuild(&graph).unwrap();

        let hash = index.get_content_hash("feat-a").unwrap();
        assert!(hash.is_some());
        assert!(index.get_content_hash("nonexistent").unwrap().is_none());
    }
}
