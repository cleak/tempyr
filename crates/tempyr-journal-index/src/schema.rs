//! SQLite schema for the journal index.
//!
//! Phase 3a is structural-only: entries + junction tables (tags, files,
//! refs) + sessions + indexer_state. FTS5 and sqlite-vec land in 3b
//! and will add virtual tables alongside this set without disrupting it.
//!
//! All tables are created `IF NOT EXISTS`, so [`apply`] is idempotent
//! and safe to call on every open. The schema version sits in a tiny
//! `schema_meta` table; future migrations check it and bump.

use rusqlite::{Connection, params};

use crate::Result;

/// Bump when the schema changes in a way that needs a migration.
pub const SCHEMA_VERSION: u32 = 1;

/// Open the index db at `path`, creating its parent directory and
/// applying the schema if needed. Pragmas are set for durability +
/// concurrent-reader friendliness:
///
/// - `journal_mode=WAL` — readers don't block writers and vice versa
/// - `synchronous=NORMAL` — single fsync per commit (durable enough
///   for a derivable cache; `FULL` would be overkill)
/// - `foreign_keys=ON` — junction tables reference `entries(id)` and
///   should cascade on entry deletion
pub fn open(path: &std::path::Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    apply(&conn)?;
    Ok(conn)
}

/// Apply (idempotently) the current schema. Records the schema version
/// in `schema_meta` for future migrations.
pub fn apply(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS schema_meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS entries (
            id            TEXT PRIMARY KEY,
            session_id    TEXT NOT NULL,
            ts            TEXT NOT NULL,
            agent         TEXT NOT NULL,
            kind          TEXT NOT NULL,
            summary       TEXT NOT NULL,
            detail        TEXT,
            body_hash     BLOB NOT NULL,
            body_json     TEXT NOT NULL,
            branch        TEXT,
            head          TEXT,
            cwd           TEXT,
            provisional   INTEGER NOT NULL DEFAULT 0,
            confidence    TEXT,
            severity      TEXT,
            is_final      INTEGER NOT NULL DEFAULT 0,
            source        TEXT NOT NULL CHECK (source IN ('open', 'archive'))
        );
        CREATE INDEX IF NOT EXISTS entries_session ON entries(session_id);
        CREATE INDEX IF NOT EXISTS entries_kind_ts ON entries(kind, ts DESC);

        CREATE TABLE IF NOT EXISTS entry_tags (
            entry_id TEXT NOT NULL REFERENCES entries(id) ON DELETE CASCADE,
            tag      TEXT NOT NULL,
            PRIMARY KEY (entry_id, tag)
        );
        CREATE INDEX IF NOT EXISTS entry_tags_tag ON entry_tags(tag);

        CREATE TABLE IF NOT EXISTS entry_files (
            entry_id TEXT NOT NULL REFERENCES entries(id) ON DELETE CASCADE,
            path     TEXT NOT NULL,
            PRIMARY KEY (entry_id, path)
        );
        CREATE INDEX IF NOT EXISTS entry_files_path ON entry_files(path);

        CREATE TABLE IF NOT EXISTS entry_refs (
            entry_id TEXT NOT NULL REFERENCES entries(id) ON DELETE CASCADE,
            node_id  TEXT NOT NULL,
            PRIMARY KEY (entry_id, node_id)
        );
        CREATE INDEX IF NOT EXISTS entry_refs_node ON entry_refs(node_id);

        CREATE TABLE IF NOT EXISTS sessions (
            session_id          TEXT PRIMARY KEY,
            agent               TEXT,
            branch              TEXT,
            head                TEXT,
            worktree_hash       TEXT,
            repo_root           TEXT,
            created_utc         TEXT,
            archived_ref        TEXT,
            archived_commit_sha TEXT
        );

        CREATE TABLE IF NOT EXISTS indexer_state (
            source_kind TEXT NOT NULL CHECK (source_kind IN ('open', 'archive')),
            source_key  TEXT NOT NULL,
            last_offset INTEGER,
            last_sha    TEXT,
            ts          TEXT NOT NULL,
            PRIMARY KEY (source_kind, source_key)
        );
        "#,
    )?;
    conn.execute(
        "INSERT OR REPLACE INTO schema_meta(key, value) VALUES ('schema_version', ?1)",
        params![SCHEMA_VERSION.to_string()],
    )?;
    Ok(())
}

/// Drop every entry-related row inside a single transaction. Used by
/// `tempyr journal index --rebuild` (without `--force`) for the
/// multi-session-safe reset path: any concurrent reader gets a brief
/// `SQLITE_BUSY` rather than the file-vanished surprise that
/// `remove_file` would inflict on Windows.
pub fn truncate(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction()?;
    // Order matters only because of FK cascades; with foreign_keys=ON
    // deleting `entries` cascades to junction tables. We still
    // explicit-truncate `indexer_state` and `sessions` since they're
    // not FK-linked to entries.
    tx.execute_batch(
        r#"
        DELETE FROM entries;
        DELETE FROM sessions;
        DELETE FROM indexer_state;
        "#,
    )?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_creates_db_and_applies_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/index.db");
        let conn = open(&path).unwrap();
        let v: String = conn
            .query_row(
                "SELECT value FROM schema_meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION.to_string());
    }

    #[test]
    fn apply_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.db");
        let conn = open(&path).unwrap();
        // Second apply on the same connection — must not error.
        apply(&conn).unwrap();
    }

    #[test]
    fn truncate_clears_entries_and_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.db");
        let mut conn = open(&path).unwrap();
        conn.execute(
            "INSERT INTO entries(id, session_id, ts, agent, kind, summary, body_hash, body_json, source) \
             VALUES ('j-x','sess','2026-04-28T00:00:00Z','claude','plan','dummy summary that is long enough to satisfy the field semantics', X'00','{}','open')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO indexer_state(source_kind, source_key, last_offset, ts) VALUES ('open','foo.jsonl',42,'2026-04-28T00:00:00Z')",
            [],
        )
        .unwrap();
        truncate(&mut conn).unwrap();
        let n_entries: i64 = conn
            .query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0))
            .unwrap();
        let n_state: i64 = conn
            .query_row("SELECT COUNT(*) FROM indexer_state", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n_entries, 0);
        assert_eq!(n_state, 0);
    }
}
