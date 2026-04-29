//! BM25-based search over journal entries with recency + kind boosts.
//!
//! Phase 3b1: pure FTS5 BM25, no embeddings. The vector blend
//! (slice 3b2) will RRF-fuse this ranking with cosine similarity from
//! sqlite-vec, but the query API stays identical — callers won't see
//! the upgrade.
//!
//! Score for each candidate row:
//!
//! ```text
//! score = bm25_norm + recency_boost(ts) + kind_boost(kind)
//! ```
//!
//! `bm25_norm` is `-bm25(...)` (FTS5 returns negative values; smaller =
//! better, so we negate to put "better" on the upper end of the
//! number line). `recency_boost` is exponential decay with a 14-day
//! half-life. `kind_boost` is a fixed table that ranks decisions and
//! dead-ends above plans/questions because those are the high-value
//! kinds for "what do past sessions know about X" queries.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, types::Value};
use serde::Serialize;
use tempyr_journal::{Entry, Kind};

use crate::Result;

/// Default token budget for one search response. Detail bodies are
/// truncated greedily to fit. Roughly 4× the size of the agent's
/// typical follow-up so a search reply is meaningful but doesn't
/// crowd the context.
pub const DEFAULT_TOKEN_BUDGET: usize = 4000;

/// Rough character-to-token ratio for English-ish text. Used only for
/// the budget heuristic; we'd never claim accuracy here.
const CHARS_PER_TOKEN: usize = 4;

/// 14-day half-life on recency. `R = 0.5` so a same-day entry gets a
/// +0.5 nudge that decays to +0.25 after two weeks.
const RECENCY_WEIGHT: f64 = 0.5;
const RECENCY_HALF_LIFE_DAYS: f64 = 14.0;

/// Caller-supplied knobs for one search.
#[derive(Debug, Clone)]
pub struct SearchOptions {
    /// FTS5 query string. Passed through verbatim — supports `"phrase"`,
    /// `term1 OR term2`, `prefix*`, etc. (FTS5 syntax).
    pub query: String,
    /// Optional kind filter (matches any of). Empty = no filter.
    pub kinds: Vec<Kind>,
    /// Hard limit on returned hits.
    pub limit: usize,
    /// Filter to entries newer than this many days. None = no filter.
    pub since_days: Option<u32>,
    /// Token budget. Detail bodies are truncated to fit.
    pub token_budget: usize,
    /// Per-hit score breakdown in the response.
    pub explain: bool,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            query: String::new(),
            kinds: Vec::new(),
            limit: 10,
            since_days: None,
            token_budget: DEFAULT_TOKEN_BUDGET,
            explain: false,
        }
    }
}

/// One hit in a search response. `entry` is the full Entry shape —
/// callers don't need a separate `journal_get` round-trip.
#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub entry: Entry,
    pub score: f64,
    /// Populated only when `SearchOptions::explain == true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explain: Option<ScoreBreakdown>,
}

/// Per-component score breakdown for `--explain` mode. All components
/// sum to `total`; `total` matches `SearchHit::score`.
#[derive(Debug, Clone, Serialize)]
pub struct ScoreBreakdown {
    pub bm25: f64,
    pub recency: f64,
    pub kind: f64,
    pub total: f64,
}

/// Run a BM25 search over `entries_fts` and return ranked hits.
///
/// Pipeline:
///
/// 1. FTS5 MATCH against the user query, joined to `entries`,
///    filtered by `kinds` and `since_days` if set.
/// 2. Compute the composite score per row.
/// 3. Sort descending by score.
/// 4. Dedup by `(blake3(summary_normalized), kind)` — distinct
///    `summary` text but same kind+content (rare; can happen when a
///    session reuses a phrasing) collapses to one hit.
/// 5. Greedy fill within `token_budget`: detail truncated to remaining
///    budget; if even the summary doesn't fit, the hit is dropped.
/// 6. Hard `limit` after the above.
pub fn search(conn: &Connection, opts: &SearchOptions) -> Result<Vec<SearchHit>> {
    let trimmed = opts.query.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    // Build the SQL with optional kind / since filters. Parameter
    // count is variadic (one per kind) so we use a Vec<Box<dyn ToSql>>.
    let mut sql = String::from(
        r#"
        SELECT
            e.body_json,
            bm25(entries_fts) AS bm25,
            e.ts AS ts,
            e.kind AS kind
        FROM entries_fts
        JOIN entries e ON e.rowid = entries_fts.rowid
        WHERE entries_fts MATCH ?1
        "#,
    );

    let mut bind: Vec<Value> = Vec::new();
    bind.push(Value::Text(trimmed.to_string()));

    // Kind filter — varies arity, so build the placeholders.
    if !opts.kinds.is_empty() {
        sql.push_str(" AND e.kind IN (");
        for (i, k) in opts.kinds.iter().enumerate() {
            if i > 0 {
                sql.push(',');
            }
            // 1-based positional placeholders. bind index = current
            // bind.len() + 1.
            sql.push_str(&format!("?{}", bind.len() + 1));
            bind.push(Value::Text(k.as_str().to_string()));
        }
        sql.push(')');
    }

    // Recency filter as a SQL string (chrono's now-days-ago). We
    // don't try to do this in SQL with date/time funcs because the
    // ts column is RFC3339 text — a string compare against a cutoff
    // is enough.
    if let Some(days) = opts.since_days {
        // `Duration::days` is soft-deprecated in chrono 0.4.32+ —
        // it panics on overflow. `try_days` returns Option and lets
        // us reject pathological inputs cleanly. `days` is u32 so it
        // always fits in i64 and (well under chrono's ~9.2e15-day
        // ceiling) always succeeds, but using the non-panicking API
        // pins the contract: a malicious / misconfigured caller
        // can't crash the server with a giant `since_days`.
        let duration = chrono::Duration::try_days(i64::from(days)).ok_or_else(|| {
            crate::IndexError::InvalidEntry(format!(
                "since_days {days} too large to express as a Duration"
            ))
        })?;
        let cutoff = Utc::now() - duration;
        sql.push_str(&format!(" AND e.ts >= ?{}", bind.len() + 1));
        bind.push(Value::Text(cutoff.to_rfc3339()));
    }

    sql.push_str(" ORDER BY bm25 ASC LIMIT ?");
    sql.push_str(&format!("{}", bind.len() + 1));
    // Pull more than `limit` so dedup + token-budget truncation has
    // headroom. 4× cap is empirical — small enough to not blow up
    // for a wide query, large enough to cover most dedup churn.
    //
    // `saturating_mul` and the `i64::try_from` fallback keep us
    // robust against pathological `limit` inputs from external
    // callers (the MCP API exposes `limit: Option<usize>`):
    // `usize::MAX * 4` would otherwise panic in debug or wrap to
    // garbage in release.
    let pull_usize = opts.limit.max(1).saturating_mul(4).max(40);
    let pull_i64 = i64::try_from(pull_usize).unwrap_or(i64::MAX);
    bind.push(Value::Integer(pull_i64));

    let now = Utc::now();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(bind.iter()), |r| {
        let body_json: String = r.get(0)?;
        let bm25: f64 = r.get(1)?;
        let ts: String = r.get(2)?;
        let kind_s: String = r.get(3)?;
        Ok((body_json, bm25, ts, kind_s))
    })?;

    let mut scored: Vec<SearchHit> = Vec::new();
    for row in rows {
        let (body_json, bm25, ts_s, kind_s) = row?;
        let entry: Entry = serde_json::from_str(&body_json)?;
        let kind = entry.kind;

        // FTS5 bm25() returns a negative number; smaller (more
        // negative) = better match. Negate so the additive ranking
        // puts higher = better.
        let bm25_norm = -bm25;

        let recency = recency_boost(&ts_s, now);
        let kindb = kind_boost(kind);
        // `kind_s` is the snake_case form pulled from SQL just to
        // double-check round-trip; assert it matches what we got out
        // of the body_json (cheap sanity).
        debug_assert_eq!(kind.as_str(), kind_s);
        let total = bm25_norm + recency + kindb;

        let explain = opts.explain.then_some(ScoreBreakdown {
            bm25: bm25_norm,
            recency,
            kind: kindb,
            total,
        });

        scored.push(SearchHit {
            entry,
            score: total,
            explain,
        });
    }

    // Sort descending. NaN shouldn't appear (all components are
    // finite), but treat any oddity as "lowest" via partial_cmp.
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Dedup by (summary normalized, kind).
    let mut seen: HashSet<(blake3::Hash, Kind)> = HashSet::new();
    let mut deduped: Vec<SearchHit> = Vec::with_capacity(scored.len());
    for hit in scored {
        let key = (
            blake3::hash(normalize_for_dedup(&hit.entry.summary).as_bytes()),
            hit.entry.kind,
        );
        if seen.insert(key) {
            deduped.push(hit);
        }
    }

    // Token-budget greedy fill: truncate detail to fit, drop hits
    // that can't fit even their summary.
    let truncated = apply_token_budget(deduped, opts.token_budget);

    // Hard limit.
    Ok(truncated.into_iter().take(opts.limit).collect())
}

/// Map of `kind -> additive boost`. Decisions and dead-ends are the
/// queries we expect agents to issue most often ("did anyone try
/// approach X?"), so we tilt the ranking toward them.
fn kind_boost(kind: Kind) -> f64 {
    match kind {
        Kind::Decision | Kind::DeadEnd => 0.5,
        Kind::Finding => 0.3,
        Kind::Outcome => 0.2,
        Kind::Plan | Kind::Question | Kind::Risk => 0.0,
        Kind::Assumption => -0.1,
    }
}

/// Exponential decay with a 14-day half-life. Same-day → `+RECENCY_WEIGHT`;
/// 14d ago → `+RECENCY_WEIGHT/2`; 28d ago → `+RECENCY_WEIGHT/4`.
fn recency_boost(ts_rfc3339: &str, now: DateTime<Utc>) -> f64 {
    let Ok(ts) = DateTime::parse_from_rfc3339(ts_rfc3339) else {
        return 0.0;
    };
    let age_days = (now - ts.with_timezone(&Utc)).num_seconds() as f64 / 86_400.0;
    if age_days < 0.0 {
        // Future-dated entry (clock skew across machines). Treat as fresh.
        return RECENCY_WEIGHT;
    }
    RECENCY_WEIGHT * (-(age_days / RECENCY_HALF_LIFE_DAYS) * std::f64::consts::LN_2).exp()
}

/// Lowercase + collapse whitespace for the dedup key. We don't strip
/// punctuation — different punctuation in two summaries usually
/// indicates a real semantic difference.
fn normalize_for_dedup(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn apply_token_budget(hits: Vec<SearchHit>, budget: usize) -> Vec<SearchHit> {
    let mut remaining = budget;
    let mut out: Vec<SearchHit> = Vec::with_capacity(hits.len());
    for mut hit in hits {
        let summary_cost = hit.entry.summary.chars().count() / CHARS_PER_TOKEN + 1;
        if summary_cost > remaining {
            // Skip this oversized hit and keep going — a slightly
            // smaller lower-ranked hit still deserves a chance to
            // fit. Strict rank order is a soft preference; "agent
            // gets some useful results within budget" wins.
            continue;
        }
        remaining -= summary_cost;

        // Truncate detail to fit the rest of the budget.
        if hit.entry.detail.is_some() {
            let detail_cost = hit
                .entry
                .detail
                .as_deref()
                .map(|s| s.chars().count() / CHARS_PER_TOKEN + 1)
                .unwrap_or(0);
            if detail_cost > remaining {
                let max_chars = remaining.saturating_sub(1) * CHARS_PER_TOKEN;
                if max_chars == 0 {
                    // Not enough budget left for any meaningful detail
                    // — even after the +1 token reserved for the
                    // ellipsis. Drop the detail entirely rather than
                    // emit a lone "…", which carries no information
                    // and just confuses readers.
                    hit.entry.detail = None;
                } else if let Some(detail) = hit.entry.detail.as_mut()
                    && max_chars < detail.chars().count()
                {
                    let truncated: String = detail.chars().take(max_chars).collect();
                    *detail = format!("{truncated}\u{2026}");
                }
                remaining = 0;
            } else {
                remaining -= detail_cost;
            }
        }
        out.push(hit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::refresh_index;
    use crate::schema;
    use chrono::TimeZone;
    use rusqlite::params;
    use std::path::PathBuf;
    use tempyr_journal::writer::append_validated;
    use tempyr_journal::{Entry, Kind, Session};

    /// Construct an `Entry` with an explicit `ts`, populated with the
    /// per-kind required fields for tests, and append it to the
    /// session's JSONL via `append_validated`.
    ///
    /// This bypasses `write_entry` deliberately. The production write
    /// path goes through `Entry::for_session` which hardcodes
    /// `ts = Utc::now()`; for recency / since-days tests we need the
    /// entry's own `ts` field to vary, not just the session id. For
    /// other tests (kind boost, dedup, token budget) the `ts` value
    /// doesn't matter — they pass `Utc::now()` and the helper picks
    /// up the production-equivalent timestamp.
    ///
    /// Per-kind fields (decision: chosen/rationale/reversible + 50+
    /// char detail; dead_end: approach/failure_mode + 50+ char detail;
    /// assumption: polarity) are filled from a small fixture so
    /// callers don't repeat them.
    fn write_test_entry(
        common: &std::path::Path,
        repo: &std::path::Path,
        ts: DateTime<Utc>,
        kind: Kind,
        summary: &str,
        detail: Option<&str>,
    ) {
        let session = Session::open_at(common, repo, "claude", ts).unwrap();
        let mut entry = Entry::for_session(kind, summary.to_string(), &session);
        entry.ts = ts;
        if let Some(d) = detail {
            entry.detail = Some(d.to_string());
        }
        apply_test_per_kind_fields(&mut entry, kind);
        append_validated(&session.jsonl_path(), &entry).unwrap();
    }

    /// Fill in the structured fields each kind requires at validate
    /// time. Detail strings are auto-padded to ≥50 chars where
    /// `validate_entry` requires it (decision, dead_end), so callers
    /// don't have to remember the threshold.
    fn apply_test_per_kind_fields(entry: &mut Entry, kind: Kind) {
        match kind {
            Kind::Decision => {
                entry
                    .detail
                    .get_or_insert_with(|| "x".repeat(60).to_string());
                entry.chosen = Some("a".to_string());
                entry.rationale = Some("a is more reversible".to_string());
                entry.reversible = Some(true);
            }
            Kind::DeadEnd => {
                entry
                    .detail
                    .get_or_insert_with(|| "x".repeat(60).to_string());
                entry.approach = Some("tried a".to_string());
                entry.failure_mode = Some("hit x".to_string());
            }
            Kind::Assumption => {
                entry.polarity = Some(tempyr_journal::Polarity::Positive);
            }
            _ => {}
        }
    }

    fn fresh_repo() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let outer = tempfile::tempdir().unwrap();
        let repo = outer.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [
            ["init", "--quiet", "--initial-branch=main"].as_slice(),
            ["config", "user.name", "tempyr-test"].as_slice(),
            ["config", "user.email", "tempyr-test@example.com"].as_slice(),
        ] {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap();
        }
        let common = repo.join(".git");
        (outer, repo, common)
    }

    fn seed_and_open(ts: DateTime<Utc>) -> (tempfile::TempDir, PathBuf, PathBuf, Connection) {
        let (outer, repo, common) = fresh_repo();
        write_test_entry(
            &common,
            &repo,
            ts,
            Kind::DeadEnd,
            "auth middleware token leak that needs investigation",
            None,
        );
        write_test_entry(
            &common,
            &repo,
            ts,
            Kind::Plan,
            "switch logging library completely to tracing crate",
            None,
        );
        refresh_index(&common, &repo).unwrap();
        let conn = schema::open(&crate::index_db_path(&common)).unwrap();
        (outer, repo, common, conn)
    }

    #[test]
    fn empty_query_yields_empty_results() {
        let (_o, _r, _c, conn) = seed_and_open(Utc::now());
        let opts = SearchOptions::default();
        assert!(search(&conn, &opts).unwrap().is_empty());
    }

    #[test]
    fn bm25_finds_matching_summary() {
        let (_o, _r, _c, conn) = seed_and_open(Utc::now());
        let opts = SearchOptions {
            query: "auth".to_string(),
            ..Default::default()
        };
        let hits = search(&conn, &opts).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].entry.summary.contains("auth"));
    }

    #[test]
    fn bm25_excludes_non_matching() {
        let (_o, _r, _c, conn) = seed_and_open(Utc::now());
        let opts = SearchOptions {
            query: "redis".to_string(),
            ..Default::default()
        };
        assert!(search(&conn, &opts).unwrap().is_empty());
    }

    #[test]
    fn kind_filter_excludes_other_kinds() {
        let (_o, _r, _c, conn) = seed_and_open(Utc::now());
        let opts = SearchOptions {
            // "the" stems via Porter; both seeded entries match it.
            query: "the OR auth OR logging OR library".to_string(),
            kinds: vec![Kind::DeadEnd],
            ..Default::default()
        };
        let hits = search(&conn, &opts).unwrap();
        for h in &hits {
            assert_eq!(h.entry.kind, Kind::DeadEnd);
        }
    }

    #[test]
    fn recency_boost_ranks_recent_higher() {
        // Two same-content entries, one fresh and one 90 days old —
        // the fresh one ranks first. We use `write_test_entry` so
        // each Entry's `ts` field reflects the intended date; the
        // standard `write_entry` path hardcodes ts = Utc::now().
        let (outer, repo, common) = fresh_repo();
        let fresh = Utc::now();
        let old = fresh - chrono::Duration::days(90);
        write_test_entry(
            &common,
            &repo,
            old,
            Kind::Plan,
            "investigation into authentication middleware behavior",
            None,
        );
        write_test_entry(
            &common,
            &repo,
            fresh,
            Kind::Plan,
            "investigation into authentication middleware behavior again",
            None,
        );
        refresh_index(&common, &repo).unwrap();
        let conn = schema::open(&crate::index_db_path(&common)).unwrap();
        let opts = SearchOptions {
            query: "authentication".to_string(),
            limit: 10,
            ..Default::default()
        };
        let hits = search(&conn, &opts).unwrap();
        assert!(hits.len() >= 2);
        // The first hit's ts is more recent.
        let first_ts = hits[0].entry.ts;
        let second_ts = hits[1].entry.ts;
        assert!(first_ts > second_ts);
        drop(outer);
    }

    #[test]
    fn kind_boost_lifts_decisions_and_dead_ends() {
        // A plan and a dead_end with similar BM25-relevant content;
        // the dead_end should rank first.
        let (outer, repo, common) = fresh_repo();
        let now = Utc::now();
        write_test_entry(
            &common,
            &repo,
            now,
            Kind::Plan,
            "investigate caching strategy for API responses again",
            None,
        );
        // Slightly later so they don't collide on session id.
        let later = now + chrono::Duration::seconds(2);
        write_test_entry(
            &common,
            &repo,
            later,
            Kind::DeadEnd,
            "investigate caching strategy for API responses again",
            None,
        );
        refresh_index(&common, &repo).unwrap();
        let conn = schema::open(&crate::index_db_path(&common)).unwrap();
        let opts = SearchOptions {
            query: "caching".to_string(),
            ..Default::default()
        };
        let hits = search(&conn, &opts).unwrap();
        assert!(!hits.is_empty());
        // Dedup folds them since summaries are byte-identical and we
        // normalize-then-hash. With dedup, the surviving hit should
        // be the dead_end (higher kind boost → ranked first → kept).
        assert_eq!(hits[0].entry.kind, Kind::DeadEnd);
        drop(outer);
    }

    #[test]
    fn dedups_identical_summary_and_kind() {
        let (outer, repo, common) = fresh_repo();
        let now = Utc::now();
        let later = now + chrono::Duration::seconds(2);
        write_test_entry(
            &common,
            &repo,
            now,
            Kind::Plan,
            "duplicate-prone summary string that appears twice",
            None,
        );
        write_test_entry(
            &common,
            &repo,
            later,
            Kind::Plan,
            "duplicate-prone summary string that appears twice",
            None,
        );
        refresh_index(&common, &repo).unwrap();
        let conn = schema::open(&crate::index_db_path(&common)).unwrap();
        let hits = search(
            &conn,
            &SearchOptions {
                query: "duplicate".to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            hits.len(),
            1,
            "dedup should collapse identical summary+kind"
        );
        drop(outer);
    }

    #[test]
    fn explain_populates_breakdown() {
        let (_o, _r, _c, conn) = seed_and_open(Utc::now());
        let opts = SearchOptions {
            query: "auth".to_string(),
            explain: true,
            ..Default::default()
        };
        let hits = search(&conn, &opts).unwrap();
        let h = &hits[0];
        let b = h.explain.as_ref().expect("explain should be populated");
        assert!((b.bm25 + b.recency + b.kind - b.total).abs() < 1e-9);
        assert!((b.total - h.score).abs() < 1e-9);
    }

    #[test]
    fn token_budget_truncates_detail() {
        let (outer, repo, common) = fresh_repo();
        let now = Utc::now();
        let big = "x".repeat(5000);
        write_test_entry(
            &common,
            &repo,
            now,
            Kind::Plan,
            "entry with very large detail that exceeds the budget",
            Some(&big),
        );
        refresh_index(&common, &repo).unwrap();
        let conn = schema::open(&crate::index_db_path(&common)).unwrap();
        let opts = SearchOptions {
            query: "exceeds".to_string(),
            token_budget: 100, // small, forces truncation
            ..Default::default()
        };
        let hits = search(&conn, &opts).unwrap();
        let detail = hits[0].entry.detail.as_deref().unwrap();
        assert!(
            detail.len() < big.len(),
            "detail should be truncated under tight budget"
        );
        assert!(
            detail.ends_with('\u{2026}'),
            "truncated detail ends with ellipsis"
        );
        drop(outer);
    }

    #[test]
    fn since_days_filter_excludes_old_entries() {
        let (outer, repo, common) = fresh_repo();
        let now = Utc::now();
        let old = now - chrono::Duration::days(30);
        write_test_entry(
            &common,
            &repo,
            old,
            Kind::Plan,
            "stale entry from a month ago that would otherwise match",
            None,
        );
        refresh_index(&common, &repo).unwrap();
        let conn = schema::open(&crate::index_db_path(&common)).unwrap();
        let opts = SearchOptions {
            query: "stale".to_string(),
            since_days: Some(7),
            ..Default::default()
        };
        let hits = search(&conn, &opts).unwrap();
        assert!(
            hits.is_empty(),
            "since_days=7 should exclude a 30-day-old entry"
        );
        drop(outer);
    }

    #[test]
    fn fts_stays_in_sync_via_triggers() {
        // Insert via the indexer (not direct SQL) and verify a search
        // immediately finds it — proving the AFTER INSERT trigger
        // fires and FTS5 sees the new content.
        let (outer, repo, common) = fresh_repo();
        write_test_entry(
            &common,
            &repo,
            Utc::now(),
            Kind::Finding,
            "triggers keep entries_fts in sync with entries automatically",
            None,
        );
        refresh_index(&common, &repo).unwrap();
        let conn = schema::open(&crate::index_db_path(&common)).unwrap();
        let hits = search(
            &conn,
            &SearchOptions {
                query: "triggers".to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        drop(outer);
    }

    #[test]
    fn fts_rebuild_on_3a_to_3b1_upgrade() {
        // Simulate a 3a-era db: insert rows directly into entries
        // with the AFTER INSERT trigger removed, so FTS5 doesn't get
        // populated on the way in. Then `open()` re-applies the
        // schema (recreates triggers via IF NOT EXISTS — but they're
        // gone so they DO get created) and `rebuild_fts_if_needed`
        // populates FTS5 from the existing entries.
        //
        // We verify by issuing a MATCH query: external-content FTS5
        // returns the linked rowid for COUNT(*), which is misleading
        // for "is FTS5 populated"; MATCH only returns rows the FTS5
        // index actually knows about.
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("index.db");
        {
            let conn = schema::open(&db_path).unwrap();
            // Simulate a real 3a-era db: triggers absent (they
            // didn't exist in v1), and the migration flag absent
            // (3a's schema didn't know about it).
            conn.execute_batch(
                r#"
                DROP TRIGGER IF EXISTS entries_ai;
                DROP TRIGGER IF EXISTS entries_ad;
                DELETE FROM schema_meta WHERE key = 'fts5_rebuilt_at_v2';
                "#,
            )
            .unwrap();
            // Direct insert; trigger doesn't fire.
            let entry_json = serde_json::json!({
                "v": 1,
                "id": "j-test-1",
                "ts": Utc.with_ymd_and_hms(2026, 4, 28, 12, 0, 0).unwrap().to_rfc3339(),
                "agent": "claude",
                "kind": "plan",
                "summary": "rebuild target entry from a 3a-era db simulation",
                "session_id": "20260428-deadbeef-120000",
                "worktree_hash": "deadbeef",
            });
            let body = serde_json::to_string(&entry_json).unwrap();
            conn.execute(
                "INSERT INTO entries(id, session_id, ts, agent, kind, summary, body_hash, body_json, source) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, X'00', ?7, 'open')",
                params![
                    entry_json["id"].as_str(),
                    entry_json["session_id"].as_str(),
                    entry_json["ts"].as_str(),
                    entry_json["agent"].as_str(),
                    entry_json["kind"].as_str(),
                    entry_json["summary"].as_str(),
                    body,
                ],
            )
            .unwrap();
            // FTS5 index is genuinely empty: a MATCH against the
            // word we'd expect to find returns 0 rows (proving the
            // trigger really didn't fire).
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM entries_fts WHERE entries_fts MATCH 'rebuild'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 0, "FTS5 should be empty before the rebuild");
        }

        // Re-open: the rebuild_fts_if_needed migration runs.
        let conn = schema::open(&db_path).unwrap();
        let hits = search(
            &conn,
            &SearchOptions {
                query: "rebuild".to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            hits.len(),
            1,
            "rebuilt FTS5 should let us find the 3a-era entry"
        );
    }

    #[test]
    fn token_budget_skips_oversized_hit_and_keeps_smaller_ones() {
        // Regression: with the previous `break`-on-oversize, a giant
        // first hit would lock out smaller lower-ranked hits. The new
        // `continue` should skip the giant one and still surface the
        // smaller siblings. We seed two entries — a huge-detail
        // monster and a tiny one — and use a budget that fits the
        // tiny one but not the huge one.
        let (outer, repo, common) = fresh_repo();
        let now = Utc::now();
        let huge = "x".repeat(20_000);
        // Big entry with detail far exceeding the budget. Use a
        // common search term ("budget") in both summaries so both
        // match the same query.
        write_test_entry(
            &common,
            &repo,
            now,
            Kind::Plan,
            // Long enough summary to also blow the cap (≈ 100 chars
            // base + padding makes summary alone ~25 tokens; budget
            // below is set to 10 so even the summary won't fit).
            "budget oversize regression hit huge entry with very long summary that exceeds the tight budget on its own",
            Some(&huge),
        );
        // Small entry that fits. Summary needs to be 20+ chars to
        // pass the writer's per-kind validator, so we pad the "tiny"
        // sibling with filler that still fits a 12-token budget
        // (≈ 48 chars).
        write_test_entry(
            &common,
            &repo,
            now + chrono::Duration::seconds(1),
            Kind::Plan,
            "budget tiny ok fits.",
            None,
        );
        refresh_index(&common, &repo).unwrap();
        let conn = schema::open(&crate::index_db_path(&common)).unwrap();
        let opts = SearchOptions {
            query: "budget".to_string(),
            limit: 10,
            // Tight budget: 6 tokens. The tiny summary is 19 chars
            // (≈ 5 tokens after the +1 rounding); the huge summary
            // is ≈ 30 tokens and won't fit.
            token_budget: 6,
            ..Default::default()
        };
        let hits = search(&conn, &opts).unwrap();
        // The huge hit was dropped; the tiny one survived.
        assert_eq!(
            hits.len(),
            1,
            "tiny hit should survive even though huge hit got skipped"
        );
        assert!(
            hits[0].entry.summary.contains("tiny"),
            "the surviving hit should be the small one"
        );
        drop(outer);
    }

    #[test]
    fn pull_does_not_overflow_for_huge_limit() {
        // Regression: `limit * 4` would panic on `usize::MAX` in
        // debug builds; saturating_mul should clamp it cleanly to
        // i64::MAX. We verify by issuing a search with a pathological
        // limit and confirming we still get ranked results without
        // panic.
        let (_o, _r, _c, conn) = seed_and_open(Utc::now());
        let opts = SearchOptions {
            query: "auth".to_string(),
            limit: usize::MAX,
            ..Default::default()
        };
        // The hard-`limit` cap at the end of `search` clamps to
        // usize::MAX (no-op), but the SQL LIMIT is bound to i64::MAX
        // via the saturating path. Just ensure we don't panic and
        // get the seeded match back.
        let hits = search(&conn, &opts).unwrap();
        assert!(!hits.is_empty());
    }

    #[test]
    fn truncation_drops_detail_instead_of_emitting_ellipsis_only() {
        // Regression: when the budget left after the summary is
        // exactly 1 token, `max_chars = (1 - 1) * 4 = 0`, and the
        // old truncation path produced a useless detail of just
        // "…". The fix sets detail to None instead.
        //
        // To get into that state we need a summary that consumes
        // most of the budget and a detail that's longer than what
        // 0 chars + ellipsis can accommodate. Use a 24-char summary
        // (≈ 7 tokens after the +1 rounding) + budget 7 → remaining
        // after summary = 0 → max_chars stays 0.
        let (outer, repo, common) = fresh_repo();
        write_test_entry(
            &common,
            &repo,
            Utc::now(),
            Kind::Plan,
            "exact-budget summary fits.",
            Some("body that won't fit because remaining tokens hit zero"),
        );
        refresh_index(&common, &repo).unwrap();
        let conn = schema::open(&crate::index_db_path(&common)).unwrap();
        let opts = SearchOptions {
            query: "summary".to_string(),
            // Just enough for the summary cost (≈ 7 tokens).
            token_budget: 7,
            ..Default::default()
        };
        let hits = search(&conn, &opts).unwrap();
        assert_eq!(hits.len(), 1);
        // The detail is dropped, not "…".
        assert!(
            hits[0].entry.detail.is_none(),
            "detail should be None when no budget remains, not '\u{2026}'"
        );
        drop(outer);
    }
}
