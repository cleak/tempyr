//! Persistent cache of journal entry embeddings, keyed by content hash.
//!
//! Stored in `<git-common-dir>/tempyr/journals/embeddings.db` —
//! deliberately separate from `index.db` for two reasons:
//!
//! 1. **`index --rebuild` doesn't blow away embeddings.** Rebuilding
//!    the structural index is cheap; re-embedding hundreds of
//!    entries costs CPU. The cache survives.
//! 2. **Cross-worktree sharing is straightforward.** Both dbs live
//!    in the shared `<git-common-dir>`, but a future change that
//!    moves the structural index (e.g. per-worktree) wouldn't
//!    drag the expensive cache with it.
//!
//! The cache is content-addressable: lookup is by `body_hash`
//! (blake3 of the entry's canonical JSON body, populated by the 3a
//! indexer). Two entries with byte-identical bodies share an
//! embedding row, even across sessions.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use crate::Result;
use crate::schema::EMBED_DIM;

/// Path to the embedding cache db given a git common dir.
pub fn cache_db_path(common_dir: &Path) -> PathBuf {
    common_dir
        .join("tempyr")
        .join("journals")
        .join("embeddings.db")
}

/// Open (or create) the cache db at `path`. Schema is small and fixed;
/// no migration logic needed yet — the model name is stored on every
/// row so a future model swap can either coexist or invalidate.
pub fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    apply(&conn)?;
    Ok(conn)
}

/// Apply (idempotently) the cache schema.
pub fn apply(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS embeddings (
            body_hash   BLOB NOT NULL,
            model       TEXT NOT NULL,
            dim         INTEGER NOT NULL,
            vec         BLOB NOT NULL,
            created_utc TEXT NOT NULL,
            PRIMARY KEY (body_hash, model)
        );
        "#,
    )?;
    Ok(())
}

/// Look up a cached embedding by `(body_hash, model)`. Returns the raw
/// bytes (little-endian f32 layout — see [`crate::embed::vec_to_bytes`]).
/// `None` on miss.
pub fn get(conn: &Connection, body_hash: &[u8], model: &str) -> Result<Option<Vec<u8>>> {
    let row: Option<(Vec<u8>, i64)> = conn
        .query_row(
            "SELECT vec, dim FROM embeddings WHERE body_hash = ?1 AND model = ?2",
            params![body_hash, model],
            |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()?;
    let Some((vec, dim)) = row else {
        return Ok(None);
    };
    if dim as usize != EMBED_DIM {
        // A row from a different model snuck through; treat as miss.
        // Strict equality on `dim` keeps us from blending vectors of
        // different shapes into vec0.
        return Ok(None);
    }
    Ok(Some(vec))
}

/// Upsert a `(body_hash, model)` row with the given embedding bytes.
/// Idempotent — a re-embed of the same content overwrites the
/// existing row's `created_utc` but produces an identical `vec`.
pub fn put(
    conn: &Connection,
    body_hash: &[u8],
    model: &str,
    dim: usize,
    vec_bytes: &[u8],
) -> Result<()> {
    conn.execute(
        r#"
        INSERT INTO embeddings(body_hash, model, dim, vec, created_utc)
        VALUES (?1, ?2, ?3, ?4, ?5)
        ON CONFLICT(body_hash, model) DO UPDATE SET
            vec         = excluded.vec,
            dim         = excluded.dim,
            created_utc = excluded.created_utc
        "#,
        params![
            body_hash,
            model,
            dim as i64,
            vec_bytes,
            chrono::Utc::now().to_rfc3339()
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_creates_db_and_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/embeddings.db");
        let conn = open(&path).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM embeddings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn put_and_get_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("embeddings.db")).unwrap();
        let hash = blake3::hash(b"some entry body").as_bytes().to_vec();
        let bytes = vec![1u8; 384 * 4];
        put(&conn, &hash, "all-MiniLM-L6-v2", 384, &bytes).unwrap();
        let got = get(&conn, &hash, "all-MiniLM-L6-v2").unwrap().unwrap();
        assert_eq!(got, bytes);
    }

    #[test]
    fn get_returns_none_on_miss() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("embeddings.db")).unwrap();
        let hash = blake3::hash(b"never-stored").as_bytes().to_vec();
        assert!(get(&conn, &hash, "all-MiniLM-L6-v2").unwrap().is_none());
    }

    #[test]
    fn put_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("embeddings.db")).unwrap();
        let hash = blake3::hash(b"content").as_bytes().to_vec();
        let bytes = vec![2u8; 384 * 4];
        put(&conn, &hash, "model", 384, &bytes).unwrap();
        put(&conn, &hash, "model", 384, &bytes).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM embeddings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn get_treats_dim_mismatch_as_miss() {
        // A row with a different dim shouldn't poison vec0 — return
        // None so the caller re-embeds with the current model.
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("embeddings.db")).unwrap();
        let hash = blake3::hash(b"x").as_bytes().to_vec();
        // Force a wrong-dim row directly (bypassing put which always
        // takes dim).
        conn.execute(
            "INSERT INTO embeddings(body_hash, model, dim, vec, created_utc) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                hash,
                "model",
                768i64,
                vec![0u8; 768 * 4],
                "2026-04-29T00:00:00Z"
            ],
        )
        .unwrap();
        assert!(get(&conn, &hash, "model").unwrap().is_none());
    }
}
