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

/// RRF fusion constant per the spec. `1 / (k + rank)` — with `k=60`
/// a top-1 hit contributes `~0.0164`, top-10 contributes `~0.0143`.
const RRF_K: f64 = 60.0;

/// Multiplier that lifts RRF scores into the same scale as the
/// recency + kind boosts (~[0, 0.5] each). Empirical: a top-1 hit
/// in both BM25 and vector contributes `2 * (1/61) * SCALE = ~1.0`
/// — large enough to dominate against single-source matches without
/// drowning out recency/kind tie-breakers.
const RRF_SCALE: f64 = 30.0;

/// Caller-supplied knobs for one search.
#[derive(Debug, Clone)]
pub struct SearchOptions {
    /// FTS5 query string. Passed through verbatim — supports `"phrase"`,
    /// `term1 OR term2`, `prefix*`, etc. (FTS5 syntax).
    pub query: String,
    /// Pre-embedded query vector for hybrid retrieval. When `Some`,
    /// the search blends BM25 and vector-cosine ranks via RRF (slice
    /// 3b2). When `None`, behavior matches slice 3b1: pure BM25 +
    /// recency + kind boost. The CLI/MCP layers embed the query
    /// string at call time and populate this; tests can pass a
    /// hand-crafted vector to drive the fusion deterministically.
    pub query_vector: Option<Vec<f32>>,
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
            query_vector: None,
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
///
/// In **BM25-only mode** (no `query_vector`): `bm25` carries the
/// negated FTS5 BM25 score directly; `vector` and `rrf` are 0.
///
/// In **hybrid mode** (`query_vector = Some(...)`): `bm25` and
/// `vector` carry the RRF-scaled per-side contributions; `rrf` is
/// their sum (handy summary line). `total = rrf + recency + kind`.
#[derive(Debug, Clone, Serialize)]
pub struct ScoreBreakdown {
    pub bm25: f64,
    /// 0 in BM25-only mode; RRF-scaled vector contribution otherwise.
    pub vector: f64,
    /// 0 in BM25-only mode; sum of `bm25` + `vector` after RRF
    /// scaling otherwise. Provided as a single number for callers
    /// that just want "how strong is the fused-rank signal" without
    /// adding two fields.
    pub rrf: f64,
    pub recency: f64,
    pub kind: f64,
    pub total: f64,
}

/// One row of raw query output before fusion. Carries enough to
/// reconstruct the entry and to rank within a single side.
struct RawHit {
    rowid: i64,
    body_json: String,
    ts: String,
    /// FTS5 returns this only on the BM25 path; `f64::NAN` when the
    /// row came from the vector-only side.
    bm25_raw: f64,
}

/// Run a hybrid BM25 + (optional) vector search and return ranked hits.
///
/// Pipeline:
///
/// 1. **BM25**: FTS5 MATCH against `query`, filtered by `kinds` /
///    `since_days`. Produces a ranked list with FTS5's bm25() score.
/// 2. **Vector** (if `query_vector` is set): sqlite-vec cosine
///    similarity against `entry_embeddings`, same filters applied.
///    Produces a ranked list by distance.
/// 3. **Fusion**: each candidate gets `rrf = 1/(k+rank_bm25) +
///    1/(k+rank_vec)` with k=60. Single-source candidates get one
///    side at 0. The RRF score is scaled to be comparable to the
///    recency + kind boost magnitudes.
/// 4. **Boosts**: add `recency_boost(ts)` and `kind_boost(kind)` per
///    the existing 3b1 logic.
/// 5. **Sort** descending by total.
/// 6. **Dedup** by `(blake3(summary_normalized), kind)`.
/// 7. **Token-budget greedy fill**: detail truncated to fit; hits
///    whose summary alone won't fit are skipped (continue, not
///    break).
/// 8. **Limit**.
///
/// In BM25-only mode (`query_vector = None`) the score reduces to
/// the slice-3b1 expression `bm25_norm + recency + kind` — same
/// ranking, byte-for-byte for a deterministic seed. The presence
/// of a `query_vector` switches into RRF mode; the `bm25` field of
/// `ScoreBreakdown` then carries the RRF-scaled BM25 contribution
/// instead of the raw bm25_norm.
pub fn search(conn: &Connection, opts: &SearchOptions) -> Result<Vec<SearchHit>> {
    let trimmed = opts.query.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    // Pull headroom for dedup + token-budget truncation. 4× cap is
    // empirical (matches 3b1). saturating_mul + try_from keep us
    // robust against pathological `limit` from external callers.
    let pull_usize = opts.limit.max(1).saturating_mul(4).max(40);
    let pull_i64 = i64::try_from(pull_usize).unwrap_or(i64::MAX);

    // BM25 side — always runs.
    let bm25_hits = run_bm25_query(conn, opts, trimmed, pull_i64)?;
    // Vector side — only when caller passed a query vector.
    let vec_hits = if let Some(qv) = opts.query_vector.as_deref() {
        run_vector_query(conn, opts, qv, pull_i64)?
    } else {
        Vec::new()
    };
    let hybrid_mode = opts.query_vector.is_some();

    // Build a unified candidate map: rowid → raw entry data + ranks
    // from each side. We keep BM25's body_json/ts copy when both
    // sides have a row (arbitrary — they're identical).
    use std::collections::HashMap;
    struct Candidate {
        body_json: String,
        ts: String,
        bm25_rank: Option<usize>,
        bm25_raw: Option<f64>,
        vec_rank: Option<usize>,
    }
    let mut by_rowid: HashMap<i64, Candidate> = HashMap::new();
    for (rank0, h) in bm25_hits.into_iter().enumerate() {
        let rank = rank0 + 1;
        by_rowid
            .entry(h.rowid)
            .and_modify(|c| {
                c.bm25_rank = Some(rank);
                c.bm25_raw = Some(h.bm25_raw);
            })
            .or_insert(Candidate {
                body_json: h.body_json,
                ts: h.ts,
                bm25_rank: Some(rank),
                bm25_raw: Some(h.bm25_raw),
                vec_rank: None,
            });
    }
    for (rank0, h) in vec_hits.into_iter().enumerate() {
        let rank = rank0 + 1;
        by_rowid
            .entry(h.rowid)
            .and_modify(|c| c.vec_rank = Some(rank))
            .or_insert(Candidate {
                body_json: h.body_json,
                ts: h.ts,
                bm25_rank: None,
                bm25_raw: None,
                vec_rank: Some(rank),
            });
    }

    // Internal carrier so we can sort with stable tie-breakers (the
    // public `SearchHit` only exposes the entry + score, but the
    // sort needs ranks + rowid for determinism).
    struct ScoredCandidate {
        hit: SearchHit,
        rowid: i64,
        bm25_rank: Option<usize>,
        vec_rank: Option<usize>,
    }

    // Score each candidate.
    let now = Utc::now();
    let mut scored: Vec<ScoredCandidate> = Vec::new();
    for (rowid, cand) in by_rowid {
        let entry: Entry = serde_json::from_str(&cand.body_json)?;
        let kind = entry.kind;
        let recency = recency_boost(&cand.ts, now);
        let kindb = kind_boost(kind);

        let (bm25_score, vector_score, rrf_score, total) = if hybrid_mode {
            // RRF: `1 / (k + rank)` per side, scaled.
            let rrf_bm25 = cand
                .bm25_rank
                .map(|r| RRF_SCALE / (RRF_K + r as f64))
                .unwrap_or(0.0);
            let rrf_vec = cand
                .vec_rank
                .map(|r| RRF_SCALE / (RRF_K + r as f64))
                .unwrap_or(0.0);
            let rrf = rrf_bm25 + rrf_vec;
            let total = rrf + recency + kindb;
            (rrf_bm25, rrf_vec, rrf, total)
        } else {
            // BM25-only: preserve the 3b1 score expression
            // exactly. bm25_norm = -bm25(...). vector and rrf
            // remain 0 in the breakdown.
            let bm25_norm = -cand.bm25_raw.unwrap_or(0.0);
            let total = bm25_norm + recency + kindb;
            (bm25_norm, 0.0, 0.0, total)
        };

        let explain = opts.explain.then_some(ScoreBreakdown {
            bm25: bm25_score,
            vector: vector_score,
            rrf: rrf_score,
            recency,
            kind: kindb,
            total,
        });

        scored.push(ScoredCandidate {
            hit: SearchHit {
                entry,
                score: total,
                explain,
            },
            rowid,
            bm25_rank: cand.bm25_rank,
            vec_rank: cand.vec_rank,
        });
    }

    // Sort descending by total, with stable tie-breakers so two
    // candidates that score equal (rare but possible — e.g. two
    // dead-ends with identical recency, both same-source rank) don't
    // shuffle between runs. Tie-break order:
    //   1. score desc (the actual signal)
    //   2. bm25_rank asc — better BM25 wins
    //   3. vec_rank asc  — better vector wins
    //   4. rowid asc     — final fallback, stable across HashMap order
    scored.sort_by(|a, b| {
        b.hit
            .score
            .partial_cmp(&a.hit.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a.bm25_rank
                    .unwrap_or(usize::MAX)
                    .cmp(&b.bm25_rank.unwrap_or(usize::MAX))
            })
            .then_with(|| {
                a.vec_rank
                    .unwrap_or(usize::MAX)
                    .cmp(&b.vec_rank.unwrap_or(usize::MAX))
            })
            .then_with(|| a.rowid.cmp(&b.rowid))
    });

    // Unwrap the sort carrier; downstream stages operate on
    // SearchHit only.
    let mut scored: Vec<SearchHit> = scored.into_iter().map(|s| s.hit).collect();
    // Suppress unused-mut warning if the next stage doesn't mutate.
    let _ = &mut scored;

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

/// Run the BM25 side of the hybrid pipeline. Returns rows ordered
/// best→worst (smaller bm25 score = better), so the caller can use
/// position as the rank for RRF fusion.
fn run_bm25_query(
    conn: &Connection,
    opts: &SearchOptions,
    query: &str,
    pull: i64,
) -> Result<Vec<RawHit>> {
    let mut sql = String::from(
        r#"
        SELECT
            e.rowid,
            e.body_json,
            bm25(entries_fts) AS bm25,
            e.ts AS ts
        FROM entries_fts
        JOIN entries e ON e.rowid = entries_fts.rowid
        WHERE entries_fts MATCH ?1
        "#,
    );
    let mut bind: Vec<Value> = vec![Value::Text(query.to_string())];
    push_filters(&mut sql, &mut bind, opts)?;
    sql.push_str(" ORDER BY bm25 ASC LIMIT ?");
    sql.push_str(&format!("{}", bind.len() + 1));
    bind.push(Value::Integer(pull));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(bind.iter()), |r| {
        Ok(RawHit {
            rowid: r.get(0)?,
            body_json: r.get(1)?,
            bm25_raw: r.get(2)?,
            ts: r.get(3)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Run the vector side via sqlite-vec cosine distance. The query
/// vector is bound as a little-endian f32 BLOB. Returns rows ordered
/// best→worst (smaller distance = better).
///
/// **Filters push down into vec0** via a `rowid IN (subquery)`
/// constraint. Without that, vec0's KNN returns the top-K nearest
/// neighbors over *all* embedded entries; a post-hoc filter could
/// drop them all and leave us with no hits when filtered-set
/// matches exist further down the distance ranking. The
/// `rowid IN (SELECT rowid FROM entries WHERE …)` shape lets
/// sqlite-vec consider only allowed rows during the KNN scan.
///
/// vec0 also requires `LIMIT N` (or `k = ?`) in the same query
/// block as `MATCH`; it errors with "A LIMIT or 'k = ?' constraint
/// is required on vec0 knn queries" if we tried to wrap the MATCH
/// in a CTE and apply LIMIT outside.
fn run_vector_query(
    conn: &Connection,
    opts: &SearchOptions,
    query_vector: &[f32],
    pull: i64,
) -> Result<Vec<RawHit>> {
    let qbytes = crate::embed::vec_to_bytes(query_vector);
    // Bind layout:
    //   ?1                        → query vec blob
    //   ?2                        → vec0 explicit `k` value
    //   ?3..                      → filter binds (kinds + since_days)
    //                                inside the rowid-IN subquery
    let mut bind: Vec<Value> = vec![Value::Blob(qbytes), Value::Integer(pull)];

    // vec0 wants `MATCH` + LIMIT/k *in the same query block*; once
    // we wrap it in a CTE or join with WHERE clauses on other
    // columns, the SQLite planner stops pushing the LIMIT down and
    // vec0 errors with "A LIMIT or 'k = ?' constraint is required".
    // Using vec0's explicit `k = ?` predicate keeps the constraint
    // visible regardless of how the rest of the query is shaped.
    //
    // Filters push into vec0 via `rowid IN (subquery)` so KNN
    // considers only allowed rows up front — without that, vec0's
    // top-K ignores filters and we'd lose hits when the user
    // narrows by kind / since.
    let mut sql = String::from(
        r#"
        SELECT
            entry_embeddings.rowid,
            e.body_json,
            entry_embeddings.distance,
            e.ts
        FROM entry_embeddings
        JOIN entries e ON e.rowid = entry_embeddings.rowid
        WHERE entry_embeddings.embedding MATCH ?1
          AND k = ?2
          AND entry_embeddings.rowid IN (
              SELECT e.rowid FROM entries e WHERE 1 = 1
        "#,
    );
    push_filters(&mut sql, &mut bind, opts)?;
    sql.push_str(") ORDER BY entry_embeddings.distance");

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(bind.iter()), |r| {
        // RawHit shape repurposed for the vector side: bm25_raw
        // becomes distance. The caller doesn't read bm25_raw on
        // the vector side except in BM25-only mode (where vec_hits
        // is empty), so the field reuse is harmless.
        Ok(RawHit {
            rowid: r.get(0)?,
            body_json: r.get(1)?,
            bm25_raw: r.get::<_, f64>(2)?,
            ts: r.get(3)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Append `kinds` and `since_days` filters to a SQL fragment, binding
/// values into `bind`. Shared between BM25 and vector queries so
/// filtering semantics stay consistent.
fn push_filters(sql: &mut String, bind: &mut Vec<Value>, opts: &SearchOptions) -> Result<()> {
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
    if let Some(days) = opts.since_days {
        let duration = chrono::Duration::try_days(i64::from(days)).ok_or_else(|| {
            crate::IndexError::InvalidEntry(format!(
                "since_days {days} too large to express as a Duration"
            ))
        })?;
        let cutoff = Utc::now() - duration;
        sql.push_str(&format!(" AND e.ts >= ?{}", bind.len() + 1));
        bind.push(Value::Text(cutoff.to_rfc3339()));
    }
    Ok(())
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

    // --- Slice 3b2: vector + RRF tests ----------------------------

    /// Shared embedder for hybrid-mode tests. Same OnceLock pattern
    /// as `embed::tests::shared_embedder` — the model load is the
    /// dominant cost so we share across all tests in this run.
    fn shared_embedder_for_search() -> &'static crate::Embedder {
        use std::sync::OnceLock;
        static EMB: OnceLock<crate::Embedder> = OnceLock::new();
        EMB.get_or_init(|| {
            crate::Embedder::new().expect("fastembed model should load for search tests")
        })
    }

    #[test]
    #[ignore = "downloads/loads the BGE-small ONNX model; run with --ignored"]
    fn vector_path_finds_semantically_related_entry() {
        // BM25 alone misses this: the query mentions "credentials"
        // but the seeded entry says "auth tokens" — keyword overlap
        // is zero (since 'token' isn't 'credentials'). Vector
        // semantics should still surface it.
        let (outer, repo, common) = fresh_repo();
        let embedder = shared_embedder_for_search();

        write_test_entry(
            &common,
            &repo,
            Utc::now(),
            Kind::DeadEnd,
            "auth tokens leaked through middleware logger output",
            Some(
                "the middleware was passing the bearer header straight into a structured log call which got captured by the agent's redaction layer way too late",
            ),
        );
        crate::indexer::refresh_index_with_embedder(&common, &repo, embedder).unwrap();
        let conn = schema::open(&crate::index_db_path(&common)).unwrap();

        // BM25-only with a different vocabulary: should miss.
        let bm25_only = search(
            &conn,
            &SearchOptions {
                query: "credentials".to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            bm25_only.is_empty(),
            "BM25 alone shouldn't match a query without keyword overlap"
        );

        // Hybrid mode: same query string, but with the vector. Should
        // surface the semantically-related entry via the vector side.
        let qv = embedder.embed_one("credentials handling").unwrap();
        let hybrid = search(
            &conn,
            &SearchOptions {
                query: "credentials handling".to_string(),
                query_vector: Some(qv),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            !hybrid.is_empty(),
            "vector semantics should find the related entry"
        );
        assert_eq!(hybrid[0].entry.kind, Kind::DeadEnd);
        drop(outer);
    }

    #[test]
    #[ignore = "downloads/loads the BGE-small ONNX model; run with --ignored"]
    fn explain_includes_vector_and_rrf_in_hybrid_mode() {
        let (outer, repo, common) = fresh_repo();
        let embedder = shared_embedder_for_search();
        write_test_entry(
            &common,
            &repo,
            Utc::now(),
            Kind::Decision,
            "use postgres for primary storage of user accounts and sessions",
            Some(
                "weighed sqlite vs postgres; postgres wins on concurrent writes and we already operate one in production so the ops cost is zero",
            ),
        );
        crate::indexer::refresh_index_with_embedder(&common, &repo, embedder).unwrap();
        let conn = schema::open(&crate::index_db_path(&common)).unwrap();

        let qv = embedder.embed_one("primary database choice").unwrap();
        let hits = search(
            &conn,
            &SearchOptions {
                query: "postgres".to_string(),
                query_vector: Some(qv),
                explain: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!hits.is_empty());
        let b = hits[0].explain.as_ref().expect("explain populated");
        // bm25 and vector should both be > 0 since the entry hits
        // both sides; rrf is their sum.
        assert!(b.bm25 > 0.0, "bm25 RRF contribution should be > 0");
        assert!(b.vector > 0.0, "vector RRF contribution should be > 0");
        assert!((b.bm25 + b.vector - b.rrf).abs() < 1e-9);
        assert!((b.rrf + b.recency + b.kind - b.total).abs() < 1e-9);
        drop(outer);
    }

    #[test]
    fn explain_zero_for_vector_components_in_bm25_only_mode() {
        // In BM25-only mode (no query_vector), the breakdown's
        // `vector` and `rrf` fields should be exactly 0 — preserving
        // the slice-3b1 contract for callers that don't supply a
        // vector.
        let (_o, _r, _c, conn) = seed_and_open(Utc::now());
        let opts = SearchOptions {
            query: "auth".to_string(),
            explain: true,
            ..Default::default()
        };
        let hits = search(&conn, &opts).unwrap();
        let b = hits[0].explain.as_ref().expect("explain populated");
        assert_eq!(b.vector, 0.0);
        assert_eq!(b.rrf, 0.0);
    }

    #[test]
    #[ignore = "downloads/loads the BGE-small ONNX model; run with --ignored"]
    fn embed_pending_filters_low_value_kinds() {
        // Plans, questions, risks, and assumptions should NOT get
        // embeddings — they're filtered out by `is_embeddable_kind`.
        // Decisions, dead-ends, findings, and outcomes should.
        let (outer, repo, common) = fresh_repo();
        let embedder = shared_embedder_for_search();
        let now = Utc::now();

        write_test_entry(
            &common,
            &repo,
            now,
            Kind::Plan,
            "plan: investigate caching strategy for the API responses",
            None,
        );
        write_test_entry(
            &common,
            &repo,
            now + chrono::Duration::seconds(1),
            Kind::Decision,
            "decision: use redis for response caching with a 5m TTL",
            None,
        );
        let report = crate::indexer::refresh_index_with_embedder(&common, &repo, embedder).unwrap();
        // 1 entry embedded (the decision), 1 filtered (the plan).
        assert_eq!(report.embedded, 1);
        assert_eq!(report.embed_filtered, 1);

        // Direct check: the entry_embeddings vec0 table has one row.
        let conn = schema::open(&crate::index_db_path(&common)).unwrap();
        let n_emb: i64 = conn
            .query_row("SELECT COUNT(*) FROM entry_embeddings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n_emb, 1);
        drop(outer);
    }

    #[test]
    #[ignore = "downloads/loads the BGE-small ONNX model; run with --ignored"]
    fn embedding_cache_survives_index_rebuild() {
        // Slice 3b2's separate `embeddings.db` should mean a
        // structural rebuild of `index.db` does NOT trigger
        // re-embedding of cached content. We verify by:
        // 1. Refreshing once with the embedder (writes cache + vec0).
        // 2. Truncating the index db (clears entry_embeddings + entries).
        // 3. Refreshing again with the embedder.
        // 4. The second refresh should report 1 `embedded` (cache hit
        //    copies the bytes back into vec0) but NOT call fastembed
        //    a second time. Indirectly we verify by expecting the
        //    fastembed model lookup latency... actually we just
        //    check that step 3 succeeds and entry_embeddings has
        //    the row again (which only works if the cache hit path
        //    fires — without the cache the embedder would still be
        //    invoked, also producing a row, so this test is a bit
        //    weak as a behavior check). Stronger: verify the cache
        //    db has the row, which is the contract the user cares
        //    about ("don't re-embed on rebuild").
        let (outer, repo, common) = fresh_repo();
        let embedder = shared_embedder_for_search();
        write_test_entry(
            &common,
            &repo,
            Utc::now(),
            Kind::DeadEnd,
            "tried adding sqlite-vec via static link but build flags leaked",
            Some(
                "the build script's CFLAGS exported into downstream crates and broke the windows MSVC compile of an unrelated dependency",
            ),
        );
        crate::indexer::refresh_index_with_embedder(&common, &repo, embedder).unwrap();

        // Cache should now have a row for our content.
        let cache_path = crate::embed_cache::cache_db_path(&common);
        let cache = crate::embed_cache::open(&cache_path).unwrap();
        let n_cache: i64 = cache
            .query_row("SELECT COUNT(*) FROM embeddings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n_cache, 1, "embedding cache should have one row");

        // Truncate the index db and refresh again. Cache rows
        // remain (separate db); the second refresh should
        // re-populate entry_embeddings from the cache.
        let mut conn = schema::open(&crate::index_db_path(&common)).unwrap();
        schema::truncate(&mut conn).unwrap();
        drop(conn);

        let r2 = crate::indexer::refresh_index_with_embedder(&common, &repo, embedder).unwrap();
        assert_eq!(r2.embedded, 1, "cache hit should still count as embedded");

        // Cache row count unchanged.
        let n_cache_2: i64 = cache
            .query_row("SELECT COUNT(*) FROM embeddings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n_cache_2, 1);
        drop(outer);
    }
}
