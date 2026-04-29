//! Single-entry lookups by ID.
//!
//! Phase 3a's read-side surface is intentionally narrow: just
//! `get_entry(id) -> Option<Entry>` and `count_entries() -> u64`. The
//! search/ranking layer comes in slice 3b on top of FTS5 + sqlite-vec.
//!
//! `get_entry` round-trips through the `entries.body_json` column, so
//! callers get the full Entry shape (including per-kind structured
//! fields like `chosen`/`rationale`/`approach`) without needing to
//! reconstruct from individual columns.

use rusqlite::{Connection, OptionalExtension, params};
use tempyr_journal::Entry;

use crate::Result;

/// Fetch one entry by ID. Returns `Ok(None)` if the row doesn't exist
/// (a future agent may legitimately query an ID it heard about that's
/// not on this machine yet — e.g., across-machine sync without a
/// fetch). Errors only on db access / JSON corruption.
pub fn get_entry(conn: &Connection, id: &str) -> Result<Option<Entry>> {
    let body_json: Option<String> = conn
        .query_row(
            "SELECT body_json FROM entries WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(json) = body_json else {
        return Ok(None);
    };
    let entry: Entry = serde_json::from_str(&json)?;
    Ok(Some(entry))
}

/// Diagnostic: total entries indexed across all sources. Cheap because
/// of the primary-key index on `entries.id`.
pub fn count_entries(conn: &Connection) -> Result<u64> {
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0))?;
    Ok(n as u64)
}
