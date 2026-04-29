//! Derived SQLite index over journal entries.
//!
//! Phase 3a (this crate's initial scope): structural data plus ID lookup.
//! No FTS5, no sqlite-vec, no embeddings — those land in slice 3b. The
//! index lives at `<git-common-dir>/tempyr/journals/index.db`, derived
//! from two sources:
//!
//! 1. Live JSONL files in `<journals>/open/` (sessions still being
//!    written by an agent).
//! 2. Archived sessions on `refs/tempyr/journals/archive/<YYYY>/<MM>/<DD>/<id>`,
//!    extracted via `git cat-file blob <ref>:entries.jsonl`.
//!
//! Re-runs are idempotent. The indexer tracks per-source progress
//! (byte offsets for open files, ref SHAs for archived refs) so a
//! second invocation only ingests deltas.
//!
//! See `docs/journal-spec.md` §9 for the full Phase 3 design.

pub mod embed;
pub mod embed_cache;
pub mod indexer;
pub mod lookup;
pub mod schema;
pub mod search;

use std::path::{Path, PathBuf};

use thiserror::Error;

pub use embed::{Embedder, try_shared_embedder};
pub use indexer::{IndexerReport, refresh_index, refresh_index_with_embedder};
pub use lookup::{count_entries, get_entry};
pub use search::{ScoreBreakdown, SearchHit, SearchOptions, search};

/// Resolved location of `index.db` for a given git common dir.
pub fn index_db_path(common_dir: &Path) -> PathBuf {
    common_dir.join("tempyr").join("journals").join("index.db")
}

#[derive(Debug, Error)]
pub enum IndexError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("git command failed: {0}")]
    Git(String),

    #[error("journal subsystem error: {0}")]
    Journal(#[from] tempyr_journal::JournalError),

    #[error("invalid entry: {0}")]
    InvalidEntry(String),

    /// Failure in the embedding subsystem — model load (network /
    /// ONNX runtime / disk), inference (OOM / shape mismatch),
    /// or vector serialization. Distinct from [`InvalidEntry`]
    /// (which is data-validation) so callers can branch on
    /// "embeddings are unavailable" vs "this entry's content is
    /// malformed".
    #[error("embedding error: {0}")]
    Embed(String),
}

pub type Result<T> = std::result::Result<T, IndexError>;
