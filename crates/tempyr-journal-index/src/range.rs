//! Range queries: filter journal entries to those whose HEAD-at-
//! write-time fell inside a `git A..B` rev range.
//!
//! Pairs naturally with `git log A..B` workflows. The CLI shells out
//! to `git rev-list <expr>` to expand the range expression into a
//! concrete list of SHAs, then this module does the SQL filter +
//! token-budget fill so the output shape matches `journal search`.
//!
//! `entries.head` carries the HEAD SHA at the moment the entry was
//! written (snapshotted into [`tempyr_journal::SessionMeta`] when the
//! session opened), so rows with `head IN (rev-list)` are exactly
//! "entries written while one of the in-range commits was checked
//! out". Rows with a NULL `head` (sessions opened in detached state
//! or before HEAD was resolvable) just don't match — that's the
//! intended behavior.

use chrono::Utc;
use rusqlite::{Connection, types::Value};
use tempyr_journal::{Entry, Kind};

use crate::Result;
use crate::search::{
    DEFAULT_TOKEN_BUDGET, ScoreBreakdown, SearchHit, apply_token_budget, kind_boost,
    normalize_for_dedup, recency_boost,
};

/// Maximum number of commit SHAs `range_query` will use in its
/// `IN (...)` filter before truncating. SQLite's default parameter
/// cap is 999 and the query also binds kinds + limit, so 900 leaves
/// safe headroom. Exposed publicly so CLI / MCP callers can
/// pre-validate via `git rev-list --count` before doing the heavy
/// lifting and return a clean error rather than relying on the
/// library's silent-truncation guard.
pub const MAX_RANGE_COMMITS: usize = 900;

/// Caller-supplied knobs for one range query. Mirrors [`SearchOptions`]
/// where it makes sense (kinds, limit, token_budget) and drops the
/// query-string concepts (FTS, vector, rerank) that don't apply.
///
/// [`SearchOptions`]: crate::search::SearchOptions
#[derive(Debug, Clone)]
pub struct RangeOptions {
    /// SHAs of commits in the range. The caller (CLI / MCP) is
    /// responsible for expanding `A..B`-style range expressions via
    /// `git rev-list`. Empty list returns no hits.
    pub commits: Vec<String>,
    /// Optional kind filter (matches any of). Empty = no filter.
    pub kinds: Vec<Kind>,
    /// Hard limit on returned hits. Default 50 (range views are
    /// usually wider than ad-hoc searches).
    pub limit: usize,
    /// Token budget for the response. Detail bodies are truncated
    /// to fit; hits whose summary alone wouldn't fit are dropped.
    pub token_budget: usize,
    /// Per-hit score breakdown in the response. For range queries
    /// the only signal is recency + kind boost — bm25/vector/rrf
    /// stay 0 so the breakdown shape matches `search` for any
    /// downstream consumer that switches between the two.
    pub explain: bool,
}

impl Default for RangeOptions {
    fn default() -> Self {
        Self {
            commits: Vec::new(),
            kinds: Vec::new(),
            limit: 50,
            token_budget: DEFAULT_TOKEN_BUDGET,
            explain: false,
        }
    }
}

/// Run a range query: return the entries whose `head` matches one of
/// `opts.commits`, ranked by recency + kind boost, then deduped and
/// token-budget-truncated. Returns the hits in the same `SearchHit`
/// shape as `search` so consumers can render either uniformly.
pub fn range_query(conn: &Connection, opts: &RangeOptions) -> Result<Vec<SearchHit>> {
    if opts.commits.is_empty() {
        return Ok(Vec::new());
    }

    // Build the IN-list. SQLite's default parameter cap is 999;
    // that's well above any realistic `git rev-list A..B` output for
    // a single-feature branch, but truncate defensively rather than
    // failing the query mid-flight on a pathological caller. Callers
    // are expected to pre-validate via `git rev-list --count` and
    // surface a clean error to the user; this guard is the last
    // resort, so we emit a stderr warning when it fires (warn-once
    // per process so it doesn't spam if the caller skips
    // pre-validation in a tight loop).
    let commits: &[String] = if opts.commits.len() > MAX_RANGE_COMMITS {
        warn_truncation_once(opts.commits.len());
        &opts.commits[..MAX_RANGE_COMMITS]
    } else {
        &opts.commits
    };

    let mut sql = String::from(
        r#"
        SELECT e.rowid, e.body_json, e.ts
        FROM entries e
        WHERE e.head IN (
        "#,
    );
    let mut bind: Vec<Value> = Vec::with_capacity(commits.len() + opts.kinds.len() + 1);
    for (i, sha) in commits.iter().enumerate() {
        if i > 0 {
            sql.push(',');
        }
        sql.push_str(&format!("?{}", bind.len() + 1));
        bind.push(Value::Text(sha.clone()));
    }
    sql.push(')');

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

    // Order by ts DESC: most recent first, which is what a reviewer
    // browsing "what reasoning produced this branch" usually wants.
    sql.push_str(" ORDER BY e.ts DESC");

    // Pull headroom for dedup + token-budget. 4× cap matches search.
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

    // Score by recency + kind boost only (no query string, so no
    // BM25/vector signals available). bm25/vector/rrf stay 0 in
    // the breakdown so consumers see the empty-signal fields and
    // know they're looking at a range result, not a search result.
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

    // Dedup on (summary, kind) — same rule as `search`. Two
    // adjacent commits referencing the same finding shouldn't both
    // surface in the digest.
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

/// One-shot stderr warning when `range_query` had to truncate the
/// commit list. Mirrors the warn-once pattern used by the embedder
/// and reranker fallback paths. Callers should normally pre-validate
/// the count and never trip this; the warning surfaces test-level
/// or scripted-misuse cases where they didn't.
fn warn_truncation_once(actual: usize) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
        eprintln!(
            "warning: tempyr journal range commit list truncated from {actual} to {MAX_RANGE_COMMITS}; \
             pre-validate with `git rev-list --count` to avoid silent truncation"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::refresh_index;
    use crate::schema;
    use chrono::DateTime;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use tempyr_journal::{EntryDraft, Session, write_entry};

    /// Bootstrap a real git repo in a temp dir. `head` SHAs need a
    /// real `.git/` to make sense; we set up two commits so the
    /// tests can exercise multi-SHA range queries.
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

    fn git_commit(repo: &Path, file: &str, message: &str) -> String {
        let path = repo.join(file);
        std::fs::write(&path, message).unwrap();
        Command::new("git")
            .args(["add", file])
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-q", "-m", message])
            .current_dir(repo)
            .output()
            .unwrap();
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo)
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// Open a session whose `head` field reflects the *current*
    /// repo HEAD, write one entry, return the entry id and the
    /// captured head SHA.
    fn write_entry_at_head(common: &Path, repo: &Path, summary: &str) -> (String, String) {
        let session = Session::open_or_resume(common, repo, "claude").unwrap();
        let head = session.meta().head.clone().expect("session captured HEAD");
        let draft = EntryDraft::new(
            tempyr_journal::Kind::Finding,
            format!("{summary} — long enough for the validator"),
        );
        let outcome = write_entry(&session, repo, draft).unwrap();
        (outcome.entry.id, head)
    }

    /// Force-finalize the active session so a *next* `open_or_resume`
    /// produces a brand-new session. Lets each commit get a session
    /// with its own head SHA in the per-test fixtures.
    fn finalize_active(common: &Path, repo: &Path) {
        let session = Session::find_active(common, repo, "claude")
            .unwrap()
            .expect("session should be active");
        session.finalize().unwrap();
    }

    /// Sleep just long enough for the next session id (which is
    /// second-precision) to differ from the previous one. Without
    /// this, two sessions opened in the same second collide and
    /// `open_or_resume` returns the prior one.
    fn tick_session_clock() {
        std::thread::sleep(std::time::Duration::from_millis(1100));
    }

    #[test]
    fn range_query_filters_by_head_sha_set() {
        let (outer, repo, common) = fresh_repo();

        // Three commits, each with one entry whose head = that commit.
        let sha1 = git_commit(&repo, "a.txt", "first commit message");
        let (id1, head1) = write_entry_at_head(&common, &repo, "first entry");
        assert_eq!(head1, sha1);
        finalize_active(&common, &repo);
        tick_session_clock();

        let sha2 = git_commit(&repo, "b.txt", "second commit message");
        let (id2, head2) = write_entry_at_head(&common, &repo, "second entry");
        assert_eq!(head2, sha2);
        finalize_active(&common, &repo);
        tick_session_clock();

        let sha3 = git_commit(&repo, "c.txt", "third commit message");
        let (id3, head3) = write_entry_at_head(&common, &repo, "third entry");
        assert_eq!(head3, sha3);

        // Index everything. Open sessions count too — the indexer
        // walks `<journals>/open/` even before publish.
        refresh_index(&common, &repo).unwrap();

        let db_path = crate::index_db_path(&common);
        let conn = schema::open(&db_path).unwrap();

        // Query commits {sha1, sha3}: should match id1 and id3, not id2.
        let opts = RangeOptions {
            commits: vec![sha1.clone(), sha3.clone()],
            ..Default::default()
        };
        let hits = range_query(&conn, &opts).unwrap();
        let ids: std::collections::HashSet<&str> =
            hits.iter().map(|h| h.entry.id.as_str()).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(id1.as_str()));
        assert!(ids.contains(id3.as_str()));
        assert!(!ids.contains(id2.as_str()));

        drop(outer);
    }

    #[test]
    fn range_query_with_empty_commits_returns_empty() {
        let (outer, repo, common) = fresh_repo();
        git_commit(&repo, "a.txt", "first commit message");
        write_entry_at_head(&common, &repo, "an entry");
        refresh_index(&common, &repo).unwrap();

        let conn = schema::open(&crate::index_db_path(&common)).unwrap();
        let opts = RangeOptions::default(); // commits = empty
        let hits = range_query(&conn, &opts).unwrap();
        assert!(hits.is_empty());
        drop(outer);
    }

    #[test]
    fn range_query_kind_filter_excludes_other_kinds() {
        let (outer, repo, common) = fresh_repo();
        let sha = git_commit(&repo, "a.txt", "first commit message");

        // Two entries at the same head: one finding, one plan.
        let session = Session::open_or_resume(&common, &repo, "claude").unwrap();
        let finding = EntryDraft::new(
            tempyr_journal::Kind::Finding,
            "auth flow finding entry that's long enough",
        );
        let plan = EntryDraft::new(
            tempyr_journal::Kind::Plan,
            "auth flow plan entry that's long enough",
        );
        write_entry(&session, &repo, finding).unwrap();
        write_entry(&session, &repo, plan).unwrap();
        refresh_index(&common, &repo).unwrap();

        let conn = schema::open(&crate::index_db_path(&common)).unwrap();
        let opts = RangeOptions {
            commits: vec![sha],
            kinds: vec![tempyr_journal::Kind::Finding],
            ..Default::default()
        };
        let hits = range_query(&conn, &opts).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entry.kind, tempyr_journal::Kind::Finding);
        drop(outer);
    }

    #[test]
    fn range_query_orders_by_ts_desc() {
        // Two entries against the same head, written ~1 second
        // apart; the newer one should come first.
        let (outer, repo, common) = fresh_repo();
        let sha = git_commit(&repo, "a.txt", "first commit message");

        let session = Session::open_or_resume(&common, &repo, "claude").unwrap();
        let early = EntryDraft::new(
            tempyr_journal::Kind::Finding,
            "earlier entry that has the right length",
        );
        let outcome_early = write_entry(&session, &repo, early).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(1100));
        let later = EntryDraft::new(
            tempyr_journal::Kind::Finding,
            "later entry that has the right length",
        );
        let outcome_later = write_entry(&session, &repo, later).unwrap();
        refresh_index(&common, &repo).unwrap();

        let conn = schema::open(&crate::index_db_path(&common)).unwrap();
        let opts = RangeOptions {
            commits: vec![sha],
            ..Default::default()
        };
        let hits = range_query(&conn, &opts).unwrap();
        assert_eq!(hits.len(), 2);
        // Newer first.
        assert_eq!(hits[0].entry.id, outcome_later.entry.id);
        assert_eq!(hits[1].entry.id, outcome_early.entry.id);
        let _ = DateTime::parse_from_rfc3339(&hits[0].entry.ts.to_rfc3339()).unwrap();
        drop(outer);
    }
}
