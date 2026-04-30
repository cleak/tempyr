//! Path-scoped blame queries: surface every journal entry whose
//! `files` field references a given path.
//!
//! Pairs naturally with `git blame`: that command shows the *who*
//! and *when* of each line; `tempyr journal blame <file>` shows the
//! *why* — every decision, dead-end, and finding the agent recorded
//! while working on this file. Particularly useful when the file
//! accumulated several dead-ends before its current shape; those
//! are the highest-signal entries for a future agent picking up
//! the same code.
//!
//! Storage: `entry_files(entry_id, path)` is already populated by
//! the indexer (one row per file each entry references). `path` is
//! indexed, so the query is a simple JOIN. No schema change needed.
//!
//! Path normalization: the indexer stores paths exactly as
//! `Entry.files` carries them — repo-relative, forward-slash. The
//! CLI / MCP layer is responsible for normalizing user input to
//! that form before calling [`blame_query`]; this module accepts
//! `path` as a literal SQL bind value and matches verbatim.

use chrono::Utc;
use rusqlite::{Connection, types::Value};
use tempyr_journal::{Entry, Kind};

use crate::Result;
use crate::search::{
    DEFAULT_TOKEN_BUDGET, ScoreBreakdown, SearchHit, apply_token_budget, kind_boost,
    normalize_for_dedup, recency_boost,
};

/// Caller-supplied knobs for one blame query. Mirrors [`RangeOptions`]
/// where it makes sense (kinds, limit, token_budget, explain) and
/// swaps the commit-list filter for a single normalized path.
///
/// [`RangeOptions`]: crate::range::RangeOptions
#[derive(Debug, Clone)]
pub struct BlameOptions {
    /// Repo-relative, forward-slash path to filter by. Must already
    /// be normalized — callers (CLI / MCP) use
    /// [`tempyr_journal::path::resolve_file_path`] to convert
    /// user-supplied input into this form.
    pub path: String,
    /// Optional kind filter (matches any of). Empty = no filter.
    pub kinds: Vec<Kind>,
    /// Hard limit on returned hits. Default 50.
    pub limit: usize,
    /// Token budget for the response. Detail bodies are truncated
    /// to fit; hits whose summary alone wouldn't fit are dropped.
    pub token_budget: usize,
    /// Per-hit score breakdown in the response. Only recency + kind
    /// signals exist for blame queries (no query string), so the
    /// other components stay 0 in the breakdown — same shape as a
    /// range result.
    pub explain: bool,
}

impl Default for BlameOptions {
    fn default() -> Self {
        Self {
            path: String::new(),
            kinds: Vec::new(),
            limit: 50,
            token_budget: DEFAULT_TOKEN_BUDGET,
            explain: false,
        }
    }
}

/// Run a blame query: return every entry whose `files` includes
/// `opts.path`, ranked by recency + kind boost, then deduped and
/// token-budget-truncated. Returns the hits in the same `SearchHit`
/// shape as `search` and `range_query` so consumers can render any
/// of the three uniformly.
pub fn blame_query(conn: &Connection, opts: &BlameOptions) -> Result<Vec<SearchHit>> {
    if opts.path.is_empty() {
        return Ok(Vec::new());
    }

    let mut sql = String::from(
        r#"
        SELECT e.rowid, e.body_json, e.ts
        FROM entries e
        JOIN entry_files f ON f.entry_id = e.id
        WHERE f.path = ?1
        "#,
    );
    let mut bind: Vec<Value> = vec![Value::Text(opts.path.clone())];

    if !opts.kinds.is_empty() {
        sql.push_str(" AND e.kind IN (");
        for (i, k) in opts.kinds.iter().enumerate() {
            if i > 0 {
                sql.push(',');
            }
            sql.push_str(&format!("?{}", bind.len() + 1));
            bind.push(Value::Text(k.as_str().to_string()));
        }
        sql.push(')');
    }

    // Most-recent reasoning about the file first.
    sql.push_str(" ORDER BY e.ts DESC");

    // Pull headroom for dedup + token-budget. 4× cap matches
    // search/range.
    let pull_usize = opts.limit.max(1).saturating_mul(4).max(40);
    let pull_i64 = i64::try_from(pull_usize).unwrap_or(i64::MAX);
    sql.push_str(&format!(" LIMIT ?{}", bind.len() + 1));
    bind.push(Value::Integer(pull_i64));

    let mut stmt = conn.prepare(&sql)?;
    let raw: Vec<(i64, String, String)> = stmt
        .query_map(rusqlite::params_from_iter(bind.iter()), |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<std::result::Result<_, _>>()?;

    let now = Utc::now();
    let mut hits: Vec<SearchHit> = Vec::with_capacity(raw.len());
    for (_rowid, body_json, ts) in raw {
        let entry: Entry = serde_json::from_str(&body_json)?;
        let recency = recency_boost(&ts, now);
        let kindb = kind_boost(entry.kind);
        let total = recency + kindb;
        let explain = opts.explain.then_some(ScoreBreakdown {
            bm25: 0.0,
            vector: 0.0,
            rrf: 0.0,
            recency,
            kind: kindb,
            rerank: 0.0,
            reranked: false,
            total,
        });
        hits.push(SearchHit {
            entry,
            score: total,
            explain,
        });
    }

    // Dedup on (summary, kind) — same rule as search/range. Two
    // adjacent entries pointing at the same file with identical
    // text shouldn't both surface.
    use std::collections::HashSet;
    let mut seen: HashSet<(blake3::Hash, Kind)> = HashSet::new();
    let mut deduped: Vec<SearchHit> = Vec::with_capacity(hits.len());
    for hit in hits {
        let key = (
            blake3::hash(normalize_for_dedup(&hit.entry.summary).as_bytes()),
            hit.entry.kind,
        );
        if seen.insert(key) {
            deduped.push(hit);
        }
    }

    let truncated = apply_token_budget(deduped, opts.token_budget);
    Ok(truncated.into_iter().take(opts.limit).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::refresh_index;
    use crate::schema;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use tempyr_journal::{EntryDraft, Session, write_entry};

    fn fresh_repo() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let outer = tempfile::tempdir().unwrap();
        let repo = outer.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [
            ["init", "--quiet", "--initial-branch=main"].as_slice(),
            ["config", "user.name", "tempyr-test"].as_slice(),
            ["config", "user.email", "tempyr-test@example.com"].as_slice(),
        ] {
            Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap();
        }
        let common = repo.join(".git");
        (outer, repo, common)
    }

    /// Write one entry with the given file paths attached, return
    /// the entry id.
    fn write_entry_with_files(
        common: &Path,
        repo: &Path,
        kind: Kind,
        summary: &str,
        files: Vec<String>,
    ) -> String {
        let session = Session::open_or_resume(common, repo, "claude").unwrap();
        let mut draft = EntryDraft::new(kind, format!("{summary} — long enough for the validator"));
        draft.files = files;
        let outcome = write_entry(&session, repo, draft).unwrap();
        outcome.entry.id
    }

    #[test]
    fn blame_query_finds_entries_referencing_the_path() {
        let (outer, repo, common) = fresh_repo();
        // Make the file paths real so `resolve_file_path`'s
        // strip_prefix logic in the writer treats them as
        // worktree-rooted.
        std::fs::write(repo.join("auth.rs"), "fn auth() {}").unwrap();
        std::fs::write(repo.join("api.rs"), "fn api() {}").unwrap();

        let id_auth = write_entry_with_files(
            &common,
            &repo,
            Kind::Finding,
            "finding about the auth flow",
            vec!["auth.rs".to_string()],
        );
        let _id_api = write_entry_with_files(
            &common,
            &repo,
            Kind::Finding,
            "finding about the api shape",
            vec!["api.rs".to_string()],
        );

        refresh_index(&common, &repo).unwrap();

        let conn = schema::open(&crate::index_db_path(&common)).unwrap();
        let opts = BlameOptions {
            path: "auth.rs".to_string(),
            ..Default::default()
        };
        let hits = blame_query(&conn, &opts).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entry.id, id_auth);
        drop(outer);
    }

    #[test]
    fn blame_query_returns_empty_for_unknown_path() {
        let (outer, repo, common) = fresh_repo();
        std::fs::write(repo.join("real.rs"), "fn real() {}").unwrap();
        write_entry_with_files(
            &common,
            &repo,
            Kind::Finding,
            "an entry attached to real.rs",
            vec!["real.rs".to_string()],
        );
        refresh_index(&common, &repo).unwrap();

        let conn = schema::open(&crate::index_db_path(&common)).unwrap();
        let opts = BlameOptions {
            path: "nonexistent.rs".to_string(),
            ..Default::default()
        };
        let hits = blame_query(&conn, &opts).unwrap();
        assert!(hits.is_empty());
        drop(outer);
    }

    #[test]
    fn blame_query_kind_filter_excludes_other_kinds() {
        let (outer, repo, common) = fresh_repo();
        std::fs::write(repo.join("auth.rs"), "fn auth() {}").unwrap();

        // Two entries on the same file: one decision, one plan.
        let session = Session::open_or_resume(&common, &repo, "claude").unwrap();
        let mut decision_draft = EntryDraft::new(
            Kind::Decision,
            "decision about auth flow that's long enough",
        );
        decision_draft.files = vec!["auth.rs".to_string()];
        decision_draft.detail = Some("x".repeat(60));
        decision_draft.chosen = Some("a".to_string());
        decision_draft.rationale = Some("a is more reversible".to_string());
        decision_draft.reversible = Some(true);
        write_entry(&session, &repo, decision_draft).unwrap();

        let mut plan_draft = EntryDraft::new(Kind::Plan, "plan for auth flow that's long enough");
        plan_draft.files = vec!["auth.rs".to_string()];
        write_entry(&session, &repo, plan_draft).unwrap();
        refresh_index(&common, &repo).unwrap();

        let conn = schema::open(&crate::index_db_path(&common)).unwrap();
        let opts = BlameOptions {
            path: "auth.rs".to_string(),
            kinds: vec![Kind::Decision],
            ..Default::default()
        };
        let hits = blame_query(&conn, &opts).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entry.kind, Kind::Decision);
        drop(outer);
    }

    #[test]
    fn blame_query_orders_by_ts_desc() {
        let (outer, repo, common) = fresh_repo();
        std::fs::write(repo.join("auth.rs"), "fn auth() {}").unwrap();

        let session = Session::open_or_resume(&common, &repo, "claude").unwrap();
        let mut early = EntryDraft::new(Kind::Finding, "earlier entry that has the right length");
        early.files = vec!["auth.rs".to_string()];
        let outcome_early = write_entry(&session, &repo, early).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(1100));
        let mut later = EntryDraft::new(Kind::Finding, "later entry that has the right length");
        later.files = vec!["auth.rs".to_string()];
        let outcome_later = write_entry(&session, &repo, later).unwrap();
        refresh_index(&common, &repo).unwrap();

        let conn = schema::open(&crate::index_db_path(&common)).unwrap();
        let opts = BlameOptions {
            path: "auth.rs".to_string(),
            ..Default::default()
        };
        let hits = blame_query(&conn, &opts).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].entry.id, outcome_later.entry.id);
        assert_eq!(hits[1].entry.id, outcome_early.entry.id);
        drop(outer);
    }

    #[test]
    fn blame_query_empty_path_returns_empty() {
        let (outer, repo, common) = fresh_repo();
        std::fs::write(repo.join("auth.rs"), "fn auth() {}").unwrap();
        write_entry_with_files(
            &common,
            &repo,
            Kind::Finding,
            "an entry on auth.rs",
            vec!["auth.rs".to_string()],
        );
        refresh_index(&common, &repo).unwrap();

        let conn = schema::open(&crate::index_db_path(&common)).unwrap();
        let opts = BlameOptions::default(); // path = ""
        let hits = blame_query(&conn, &opts).unwrap();
        assert!(hits.is_empty());
        drop(outer);
    }
}
