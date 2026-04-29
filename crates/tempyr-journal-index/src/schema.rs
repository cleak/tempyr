//! SQLite schema for the journal index.
//!
//! Phase 3a is structural-only: entries + junction tables (tags, files,
//! refs) + sessions + indexer_state. FTS5 and sqlite-vec land in 3b
//! and will add virtual tables alongside this set without disrupting it.
//!
//! All tables are created `IF NOT EXISTS`, so [`apply`] is idempotent
//! and safe to call on every open. The schema version sits in a tiny
//! `schema_meta` table; future migrations check it and bump.

use std::sync::Once;

use rusqlite::{Connection, TransactionBehavior, params};

use crate::Result;

/// Embedding model + dimension for slice 3b2. Locked here so the
/// schema, indexer, and search modules all agree. If we swap
/// models later, bump `SCHEMA_VERSION` and add a migration that
/// invalidates the embedding cache (or filters by model).
pub const EMBED_MODEL_NAME: &str = "all-MiniLM-L6-v2";
pub const EMBED_DIM: usize = 384;

/// Register the sqlite-vec extension as a SQLite auto-extension so
/// every `Connection::open` afterward has `vec_version()`, `vec_f32()`,
/// and `vec0` virtual tables available. Once-only registration keeps
/// us from race-loading the extension across multiple opens.
fn register_sqlite_vec_once() {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        // SAFETY: sqlite_vec::sqlite3_vec_init is the standard
        // SQLite extension entry point. `sqlite3_auto_extension`
        // registers it globally for all subsequent connections in
        // this process.
        unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(
                    *mut rusqlite::ffi::sqlite3,
                    *mut *mut std::ffi::c_char,
                    *const rusqlite::ffi::sqlite3_api_routines,
                ) -> std::ffi::c_int,
            >(sqlite_vec::sqlite3_vec_init)));
        }
    });
}

/// Bump when the schema changes in a way that needs a migration.
///
/// History:
/// - **1** (slice 3a): structural tables only — `entries`,
///   junction tables, `sessions`, `indexer_state`.
/// - **2** (slice 3b1): added `entries_fts` (FTS5 virtual table)
///   and `AFTER INSERT/DELETE` triggers on `entries`. Open of an
///   older db rebuilds the FTS5 contents from `entries` so
///   existing 3a installs upgrade seamlessly.
/// - **3** (slice 3b2): added `entry_embeddings` (sqlite-vec
///   virtual table) for semantic search. Embeddings are populated
///   incrementally by the indexer, filtered to high-value kinds
///   (decision/finding/dead_end/outcome).
pub const SCHEMA_VERSION: u32 = 3;

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
    // Register sqlite-vec as an auto-extension before opening any
    // connection. Idempotent — only the first call actually does the
    // work.
    register_sqlite_vec_once();
    let mut conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    apply(&conn)?;
    // Slice 3b1 schema-bump migration: if `entries_fts` is empty but
    // `entries` already has rows (3a-era db opened by 3b1+ code),
    // rebuild the FTS5 contents from `entries`. One-time O(n) scan;
    // subsequent inserts feed via the AFTER INSERT trigger.
    rebuild_fts_if_needed(&mut conn)?;
    Ok(conn)
}

/// Populate `entries_fts` from `entries` once on the 3a → 3b1
/// upgrade path. We can't probe FTS5 directly to detect "is the
/// index populated?": with `content='entries'`, `COUNT(*) FROM
/// entries_fts` queries the linked entries table, not the FTS5
/// index. Instead, we track the migration via a flag in
/// `schema_meta` — set once, read cheaply on every open.
const FTS_REBUILT_KEY: &str = "fts5_rebuilt_at_v2";

/// Run the v1 → v2 FTS5 rebuild atomically.
///
/// The whole sequence — read flag, count entries, INSERT INTO
/// entries_fts, INSERT OR REPLACE flag — runs inside `BEGIN IMMEDIATE`,
/// which acquires the SQLite write lock up front. With multiple
/// processes racing to open the same db, only one acquires the lock
/// and runs the rebuild; the others block on `BEGIN IMMEDIATE`, then
/// observe the flag set and skip. Without this, both processes could
/// rebuild concurrently — FTS5 rebuild is idempotent (so no
/// corruption) but the wasted O(n) scan compounds at scale.
fn rebuild_fts_if_needed(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    // Already rebuilt? Inside the IMMEDIATE transaction, this read
    // sees any prior commit before we proceed.
    let already: Option<String> = tx
        .query_row(
            "SELECT value FROM schema_meta WHERE key = ?1",
            params![FTS_REBUILT_KEY],
            |r| r.get(0),
        )
        .ok();
    if already.is_some() {
        return Ok(()); // tx drops, rolls back the (no-op) read.
    }

    // Not flagged yet. If entries is empty, there's nothing to rebuild
    // — but flag anyway so we don't redo this check on every open.
    let entries_count: i64 = tx.query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0))?;
    if entries_count > 0 {
        // FTS5's `rebuild` command re-derives the index from
        // `content=` (the linked `entries` table). Atomic and faster
        // than a manual INSERT-from-SELECT loop.
        tx.execute(
            "INSERT INTO entries_fts(entries_fts) VALUES ('rebuild')",
            [],
        )?;
    }
    tx.execute(
        "INSERT OR REPLACE INTO schema_meta(key, value) VALUES (?1, 'true')",
        params![FTS_REBUILT_KEY],
    )?;
    tx.commit()?;
    Ok(())
}

/// Apply (idempotently) the current schema. Records the schema version
/// in `schema_meta` for future migrations.
pub fn apply(conn: &Connection) -> Result<()> {
    // sqlite-vec needs the embedding dimension baked into the
    // CREATE VIRTUAL TABLE — `vec0(embedding float[N])`. We format
    // the SQL with the constant so the schema can't drift from
    // EMBED_DIM.
    let entry_embeddings_sql = format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS entry_embeddings USING vec0(\n\
         embedding float[{EMBED_DIM}]\n\
         );"
    );
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

        -- FTS5 virtual table mirroring entries.summary + entries.detail.
        -- `content='entries'` keeps content stored only in entries (no
        -- duplication); `content_rowid='rowid'` lets us address rows by
        -- the entries table's implicit rowid. Tokenizer: porter unicode61
        -- stems prose words and folds diacritics — fits agent-written
        -- summaries; code identifiers (`parse_and_insert`, `JournalConfig`)
        -- mostly survive Porter stemming intact.
        CREATE VIRTUAL TABLE IF NOT EXISTS entries_fts USING fts5(
            summary,
            detail,
            content='entries',
            content_rowid='rowid',
            tokenize='porter unicode61'
        );

        -- Keep entries_fts in sync. INSERT side: the indexer's
        -- INSERT OR IGNORE path produces real inserts whose AFTER
        -- INSERT trigger feeds FTS5; ignored rows (id collision) emit
        -- no trigger, which is what we want. DELETE side fires the
        -- contentless-table 'delete' command that tells FTS5 to
        -- forget the row.
        CREATE TRIGGER IF NOT EXISTS entries_ai AFTER INSERT ON entries BEGIN
            INSERT INTO entries_fts(rowid, summary, detail)
            VALUES (new.rowid, new.summary, COALESCE(new.detail, ''));
        END;
        CREATE TRIGGER IF NOT EXISTS entries_ad AFTER DELETE ON entries BEGIN
            INSERT INTO entries_fts(entries_fts, rowid, summary, detail)
            VALUES ('delete', old.rowid, old.summary, COALESCE(old.detail, ''));
        END;
        "#,
    )?;
    // sqlite-vec virtual table for semantic search. Lives alongside
    // entries; rows are inserted by the indexer's embed-on-refresh
    // path and joined back to entries via rowid.
    conn.execute_batch(&entry_embeddings_sql)?;
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
    // deleting `entries` cascades to junction tables, and the AFTER
    // DELETE trigger on entries cascades into entries_fts. We still
    // explicit-truncate `indexer_state`, `sessions`, and
    // `entry_embeddings` since they're not FK-linked to entries.
    // (vec0 virtual tables don't participate in standard cascade
    // semantics, so we drop their contents directly.)
    tx.execute_batch(
        r#"
        DELETE FROM entries;
        DELETE FROM sessions;
        DELETE FROM indexer_state;
        DELETE FROM entry_embeddings;
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
