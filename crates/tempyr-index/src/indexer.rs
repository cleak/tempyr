use std::path::Path;

use rusqlite::Connection;
use serde::Serialize;

use tempyr_core::graph::Graph;
use tempyr_core::node::Node;

use crate::Result;

/// Layout version of the structural tables, stamped into `PRAGMA user_version`
/// by [`Index::rebuild`]. Bump it whenever the tables or the per-node row
/// derivation change; [`Index::incremental_update`] falls back to a full
/// rebuild for any index stamped with a different version.
///
/// History: `0` = unversioned (no `index_hash` column); `1` = `index_hash`.
pub const INDEX_SCHEMA_VERSION: i32 = 1;

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

/// The column values one node contributes to the index: its `nodes` row and
/// the `edges` rows it owns. Built once per node and used both for the INSERTs
/// and for the `index_hash` fingerprint, so the fingerprint covers exactly
/// what is stored and cannot drift from it.
#[derive(Serialize)]
struct NodeRow<'a> {
    id: &'a str,
    node_type: &'a str,
    status: Option<&'a str>,
    owner: Option<&'a str>,
    title: &'a str,
    body_text: &'a str,
    file_path: String,
    created_at: Option<String>,
    updated_at: Option<String>,
    tags: Option<String>,
    content_hash: &'a str,
    edges: Vec<EdgeRow<'a>>,
}

#[derive(Serialize)]
struct EdgeRow<'a> {
    target_id: &'a str,
    edge_type: &'a str,
    valid_from: Option<String>,
    valid_until: Option<String>,
    annotation: Option<&'a str>,
}

impl<'a> NodeRow<'a> {
    fn from_node(node: &'a Node) -> Self {
        Self {
            id: node.id(),
            node_type: node.node_type(),
            status: node.status(),
            owner: node.frontmatter.owner.as_deref(),
            title: node.title(),
            body_text: &node.body,
            file_path: node.file_path.to_string_lossy().to_string(),
            created_at: node.frontmatter.created.map(|c| c.to_rfc3339()),
            updated_at: node.frontmatter.updated.map(|u| u.to_rfc3339()),
            tags: node
                .frontmatter
                .tags
                .as_ref()
                .map(|t| serde_json::to_string(t).unwrap_or_default()),
            content_hash: &node.content_hash,
            edges: node
                .edges()
                .iter()
                .map(|edge| EdgeRow {
                    target_id: &edge.target,
                    edge_type: &edge.edge_type,
                    valid_from: edge.valid_from.map(|d| d.to_string()),
                    valid_until: edge.valid_until.map(|d| d.to_string()),
                    annotation: edge.annotation.as_deref(),
                })
                .collect(),
        }
    }

    /// Fingerprint of every value this node writes to the index.
    ///
    /// Unlike `Node::content_hash` (body only, the embedding cache key), this
    /// changes on frontmatter-only edits such as a status change or an added
    /// edge, which is what incremental updates need to detect.
    fn index_hash(&self) -> String {
        // JSON is an unambiguous encoding of the field sequence, so distinct
        // rows cannot collide through concatenation.
        let encoded = serde_json::to_vec(self).expect("NodeRow serialization is infallible");
        blake3::hash(&encoded).to_hex().to_string()
    }
}

/// Compute the index fingerprint for a node (see `NodeRow::index_hash`).
pub(crate) fn node_index_hash(node: &Node) -> String {
    NodeRow::from_node(node).index_hash()
}

impl Index {
    /// Create a new index database at the given path.
    ///
    /// The structural tables only match [`INDEX_SCHEMA_VERSION`] once
    /// [`Index::rebuild`] has run; call it before querying a new index.
    pub fn create(path: &Path) -> Result<Self> {
        Self::initialize(Connection::open(path)?)
    }

    /// Create an in-memory index (for testing).
    pub fn create_in_memory() -> Result<Self> {
        Self::initialize(Connection::open_in_memory()?)
    }

    fn initialize(conn: Connection) -> Result<Self> {
        // Disable FK enforcement — the index is a derived artifact,
        // and edges may reference nodes not yet inserted during rebuild.
        conn.execute_batch("PRAGMA foreign_keys = OFF;")?;
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

    /// Layout version stamped on this index (see [`INDEX_SCHEMA_VERSION`]).
    pub fn schema_version(&self) -> Result<i32> {
        Ok(self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?)
    }

    /// Create the structural tables if they do not exist yet.
    fn create_tables(&self) -> Result<()> {
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
                content_hash TEXT NOT NULL,
                -- Fingerprint of every value the node contributes to the
                -- index (NodeRow::index_hash). NULL means unknown, which
                -- incremental_update treats as changed.
                index_hash  TEXT
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

    /// Drop and recreate the structural tables at the current layout and stamp
    /// [`INDEX_SCHEMA_VERSION`]. The embedding cache table is left untouched.
    fn reset_tables(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            DROP TABLE IF EXISTS nodes_fts;
            DROP TABLE IF EXISTS edges;
            DROP TABLE IF EXISTS nodes;
            ",
        )?;
        self.create_tables()?;
        self.conn
            .pragma_update(None, "user_version", INDEX_SCHEMA_VERSION)?;
        Ok(())
    }

    /// Full rebuild: drop all data and reindex from the graph.
    ///
    /// Runs as one transaction: on error the index is left unchanged, and the
    /// rebuild costs a single journal flush instead of one per row.
    pub fn rebuild(&self, graph: &Graph) -> Result<IndexStats> {
        let tx = self.conn.unchecked_transaction()?;
        self.reset_tables()?;
        for node in graph.nodes.values() {
            self.insert_node(node)?;
        }
        tx.commit()?;

        self.stats()
    }

    /// Insert a single node and the edges it owns into the index.
    ///
    /// The node must not be indexed yet (see `remove_node`); a duplicate id
    /// fails on the `nodes` primary key.
    pub(crate) fn insert_node(&self, node: &Node) -> Result<()> {
        let row = NodeRow::from_node(node);
        let index_hash = row.index_hash();

        self.conn
            .prepare_cached(
                "INSERT INTO nodes (id, node_type, status, owner, title, body_text, file_path, created_at, updated_at, tags, content_hash, index_hash)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            )?
            .execute(rusqlite::params![
                row.id,
                row.node_type,
                row.status,
                row.owner,
                row.title,
                row.body_text,
                row.file_path,
                row.created_at,
                row.updated_at,
                row.tags,
                row.content_hash,
                index_hash,
            ])?;
        let rowid = self.conn.last_insert_rowid();

        self.conn
            .prepare_cached(
                "INSERT INTO nodes_fts(rowid, id, title, body_text, tags) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?
            .execute(rusqlite::params![
                rowid,
                row.id,
                row.title,
                row.body_text,
                row.tags.as_deref().unwrap_or(""),
            ])?;

        let mut insert_edge = self.conn.prepare_cached(
            "INSERT OR IGNORE INTO edges (source_id, target_id, edge_type, valid_from, valid_until, annotation)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for edge in &row.edges {
            insert_edge.execute(rusqlite::params![
                row.id,
                edge.target_id,
                edge.edge_type,
                edge.valid_from,
                edge.valid_until,
                edge.annotation,
            ])?;
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

        let fts_entries: usize =
            self.conn
                .query_row("SELECT COUNT(*) FROM nodes_fts", [], |row| row.get(0))?;

        let mut stmt = self.conn.prepare(
            "SELECT node_type, COUNT(*) FROM nodes GROUP BY node_type ORDER BY node_type",
        )?;
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

    /// List node IDs and content hashes for vector-store backed operations.
    pub fn node_ids_and_content_hashes(
        &self,
        node_type_filter: Option<&str>,
    ) -> Result<Vec<(String, String)>> {
        let sql = if node_type_filter.is_some() {
            "SELECT id, content_hash FROM nodes WHERE node_type = ?1"
        } else {
            "SELECT id, content_hash FROM nodes"
        };

        let mut stmt = self.conn.prepare(sql)?;
        let rows = if let Some(node_type) = node_type_filter {
            stmt.query_map([node_type], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        } else {
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };

        Ok(rows)
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

    fn make_test_graph() -> Graph {
        let mut graph = Graph::new(make_schema());

        let feat = "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: alice\ntags: [replay, test]\nedges:\n  - target: epic-a\n    type: child_of\n---\n# Feature A\n\nThis feature handles session replay.\n";
        let epic = "---\nid: epic-a\ntype: epic\nstatus: active\nowner: alice\nedges:\n  - target: feat-a\n    type: parent_of\n---\n# Epic A\n\nThe observability epic.\n";
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

    #[test]
    fn test_node_ids_and_content_hashes() {
        let graph = make_test_graph();
        let index = Index::create_in_memory().unwrap();
        index.rebuild(&graph).unwrap();

        let mut all_rows = index.node_ids_and_content_hashes(None).unwrap();
        all_rows.sort();

        let mut expected_all = vec![
            (
                "epic-a".to_string(),
                graph.get_node("epic-a").unwrap().content_hash.clone(),
            ),
            (
                "feat-a".to_string(),
                graph.get_node("feat-a").unwrap().content_hash.clone(),
            ),
            (
                "task-a".to_string(),
                graph.get_node("task-a").unwrap().content_hash.clone(),
            ),
        ];
        expected_all.sort();
        assert_eq!(all_rows, expected_all);

        let feature_rows = index.node_ids_and_content_hashes(Some("feature")).unwrap();
        assert_eq!(
            feature_rows,
            vec![(
                "feat-a".to_string(),
                graph.get_node("feat-a").unwrap().content_hash.clone(),
            )]
        );
    }

    #[test]
    fn test_rebuild_commits_once() {
        let graph = make_test_graph();
        let index = Index::create_in_memory().unwrap();
        let commits = crate::test_support::commit_counter(&index.conn);

        index.rebuild(&graph).unwrap();

        assert_eq!(commits.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn test_rebuild_stamps_schema_version() {
        let index = Index::create_in_memory().unwrap();
        assert_eq!(index.schema_version().unwrap(), 0);

        index.rebuild(&make_test_graph()).unwrap();

        assert_eq!(index.schema_version().unwrap(), INDEX_SCHEMA_VERSION);
    }

    #[test]
    fn test_rebuild_migrates_unversioned_index() {
        // Layout written before `index_hash` existed (user_version 0).
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE nodes (
                id TEXT PRIMARY KEY, node_type TEXT NOT NULL, status TEXT, owner TEXT,
                title TEXT, body_text TEXT, file_path TEXT NOT NULL, created_at TEXT,
                updated_at TEXT, tags TEXT, content_hash TEXT NOT NULL
            );
            CREATE TABLE edges (
                source_id TEXT NOT NULL, target_id TEXT NOT NULL, edge_type TEXT NOT NULL,
                valid_from TEXT, valid_until TEXT, annotation TEXT,
                PRIMARY KEY (source_id, target_id, edge_type)
            );
            CREATE VIRTUAL TABLE nodes_fts USING fts5(
                id, title, body_text, tags, content='nodes', content_rowid='rowid'
            );
            INSERT INTO nodes (id, node_type, file_path, content_hash)
                VALUES ('gone', 'task', 'gone.md', 'h');
            ",
        )
        .unwrap();
        let index = Index { conn };

        let stats = index.rebuild(&make_test_graph()).unwrap();

        assert_eq!(stats.node_count, 3);
        assert_eq!(stats.edge_count, 2);
        assert_eq!(index.schema_version().unwrap(), INDEX_SCHEMA_VERSION);
        let missing_hashes: usize = index
            .conn
            .query_row(
                "SELECT COUNT(*) FROM nodes WHERE index_hash IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(missing_hashes, 0);
    }

    #[test]
    fn test_rebuild_keeps_embedding_cache() {
        let index = Index::create_in_memory().unwrap();
        index.store_embedding("feat-a", "h", &[1.0]).unwrap();

        index.rebuild(&make_test_graph()).unwrap();

        assert!(index.has_valid_embedding("feat-a", "h").unwrap());
    }

    #[test]
    fn test_index_hash_tracks_every_indexed_field() {
        let base = "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: alice\ntags: [replay]\nedges:\n  - target: epic-a\n    type: child_of\n---\n# Feature A\n\nBody.\n";
        let hash_of = |content: &str| {
            let node = parse_node(content, PathBuf::from("feat.md")).unwrap();
            (node_index_hash(&node), node.content_hash)
        };
        let (base_hash, base_content_hash) = hash_of(base);

        assert_eq!(hash_of(base).0, base_hash, "hash must be deterministic");

        let frontmatter_only_edits = [
            ("status", base.replace("status: draft", "status: active")),
            ("owner", base.replace("owner: alice", "owner: bob")),
            ("tags", base.replace("[replay]", "[replay, extra]")),
            (
                "edge target",
                base.replace("target: epic-a", "target: epic-b"),
            ),
            (
                "edge type",
                base.replace("type: child_of", "type: relates_to"),
            ),
            (
                "edge annotation",
                base.replace("type: child_of\n", "type: child_of\n    annotation: why\n"),
            ),
            (
                "edge validity",
                base.replace(
                    "type: child_of\n",
                    "type: child_of\n    valid_from: 2026-01-01\n",
                ),
            ),
            (
                "added edge",
                base.replace(
                    "---\n# Feature",
                    "  - target: task-a\n    type: parent_of\n---\n# Feature",
                ),
            ),
            (
                "updated timestamp",
                base.replace(
                    "owner: alice\n",
                    "owner: alice\nupdated: 2026-01-01T00:00:00Z\n",
                ),
            ),
        ];
        for (label, edited) in frontmatter_only_edits {
            let (hash, content_hash) = hash_of(&edited);
            assert_eq!(
                content_hash, base_content_hash,
                "{label}: edit must not touch the body"
            );
            assert_ne!(hash, base_hash, "{label}: index hash must change");
        }

        let (body_hash, _) = hash_of(&base.replace("Body.", "Other body."));
        assert_ne!(body_hash, base_hash, "body edit must change the index hash");
    }
}
