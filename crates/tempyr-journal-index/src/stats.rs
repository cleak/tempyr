//! Aggregate statistics over the journal index.
//!
//! Powers `tempyr journal stats` and the `journal_stats` MCP tool.
//! All numbers come from cheap SQL aggregates over the existing
//! schema — no temporary tables, no joins beyond what's already
//! indexed. Output is "debugging gold" in the sense that the
//! interesting signals here usually surface usage anomalies:
//!
//! - A **low dead-end rate** (decision-to-dead-end ratio leaning
//!   heavily toward decisions) usually means agents aren't logging
//!   failures, which is the journal's highest-value content.
//! - A **high provisional ratio** with no committing (no `is_final`
//!   entries closing sessions) suggests sessions are accumulating
//!   without the publisher seeing them.
//! - A **flat activity histogram** during a known active period
//!   means the agent isn't reaching the journal at all — usually a
//!   misconfigured hook or environment.
//!
//! Probes degrade gracefully: missing tables surface as zeros, not
//! errors. The stats command is a diagnostic; never let it fail in
//! a way that masks the data the user came to see.

use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, types::Value};
use serde::Serialize;

use crate::Result;

/// Hard bounds on caller-supplied knobs. CLI uses these for clap
/// `value_parser` ranges; MCP uses them for clamping. Centralized
/// here so both transports enforce the same bounds and a future
/// tweak only needs one place.
pub const MIN_SINCE_DAYS: u32 = 0;
pub const MAX_SINCE_DAYS: u32 = 36_500; // ~100 years; well past any real project.
pub const MIN_TOP_LIST: u32 = 1;
pub const MAX_TOP_LIST: u32 = 1_000;
pub const MIN_ACTIVITY_WINDOW_DAYS: u32 = 1;
pub const MAX_ACTIVITY_WINDOW_DAYS: u32 = 365;

/// Caller-supplied knobs for one stats query.
#[derive(Debug, Clone)]
pub struct StatsOptions {
    /// Filter to entries newer than this many days (inclusive lower
    /// bound on `entry.ts`). `None` covers all of history.
    pub since_days: Option<u32>,
    /// Cap on the per-tag entries in [`StatsReport::top_tags`].
    pub top_tags: usize,
    /// Cap on the per-file entries in [`StatsReport::top_files`].
    pub top_files: usize,
    /// Number of days of activity-histogram buckets to return,
    /// counting back from today. 30 is a reasonable default.
    pub activity_window_days: u32,
}

impl Default for StatsOptions {
    fn default() -> Self {
        Self {
            since_days: None,
            top_tags: 20,
            top_files: 20,
            activity_window_days: 30,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StatsReport {
    pub total_entries: u64,
    pub total_sessions: u64,
    pub total_agents: u64,
    /// Entries where `provisional = true`.
    pub provisional_entries: u64,
    /// Entries closing a session (`is_final = true`).
    pub final_entries: u64,
    /// Per-kind counts, ordered by count descending. Kinds with zero
    /// matching entries are omitted.
    pub kind_distribution: Vec<KindCount>,
    /// Sessions per `agent` field, ordered by session count descending.
    pub sessions_per_agent: Vec<AgentCount>,
    /// Top tags by reference count, capped at `opts.top_tags`.
    pub top_tags: Vec<TagCount>,
    /// Top file paths by reference count, capped at `opts.top_files`.
    pub top_files: Vec<FileCount>,
    /// Per-day entry counts for the last `opts.activity_window_days`,
    /// keyed by `YYYY-MM-DD` (UTC). Days with zero entries are
    /// included so the histogram has a stable shape.
    pub activity_per_day: Vec<DayCount>,
    /// Convenience signal: dead-end count divided by
    /// (decision + dead_end). Returns `None` when the denominator
    /// is zero. A reading near 0 means agents are choosing not to
    /// log failures relative to decisions — the spec's main
    /// motivating concern.
    pub dead_end_ratio: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct KindCount {
    pub kind: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentCount {
    pub agent: String,
    pub session_count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TagCount {
    pub tag: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileCount {
    pub path: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DayCount {
    /// `YYYY-MM-DD` (UTC).
    pub date: String,
    pub count: u64,
}

/// Compute every section of [`StatsReport`] in one pass. Never fails
/// silently — a SQL error from any sub-query bubbles up as an
/// `Err`. Callers (CLI / MCP) downgrade this to a clean error
/// message; the stats command is a diagnostic, so missing data
/// matters more than empty sections.
pub fn compute_stats(conn: &Connection, opts: &StatsOptions) -> Result<StatsReport> {
    // Build a `WHERE` fragment for the `since_days` filter. Each
    // sub-query uses it where applicable; queries that join through
    // entry_tags / entry_files JOIN back to entries.ts so the same
    // filter applies.
    let now = Utc::now();
    let since_filter = opts.since_days.map(|d| {
        let cutoff = now - Duration::try_days(i64::from(d)).unwrap_or(Duration::zero());
        cutoff.to_rfc3339()
    });

    let total_entries = count_entries(conn, since_filter.as_deref())?;
    let total_sessions = count_distinct(conn, "session_id", since_filter.as_deref())?;
    let total_agents = count_distinct(conn, "agent", since_filter.as_deref())?;
    let provisional_entries = count_with_clause(conn, "provisional = 1", since_filter.as_deref())?;
    let final_entries = count_with_clause(conn, "is_final = 1", since_filter.as_deref())?;

    let kind_distribution = kind_distribution(conn, since_filter.as_deref())?;
    let sessions_per_agent = sessions_per_agent(conn, since_filter.as_deref())?;
    let top_tags = top_tags(conn, opts.top_tags, since_filter.as_deref())?;
    let top_files = top_files(conn, opts.top_files, since_filter.as_deref())?;
    let activity_per_day = activity_per_day(conn, opts.activity_window_days, now)?;

    let dead_end_ratio = compute_dead_end_ratio(&kind_distribution);

    Ok(StatsReport {
        total_entries,
        total_sessions,
        total_agents,
        provisional_entries,
        final_entries,
        kind_distribution,
        sessions_per_agent,
        top_tags,
        top_files,
        activity_per_day,
        dead_end_ratio,
    })
}

fn count_entries(conn: &Connection, since: Option<&str>) -> Result<u64> {
    let (sql, bind) = with_since("SELECT COUNT(*) FROM entries", since);
    let n: i64 = conn.query_row(&sql, rusqlite::params_from_iter(bind.iter()), |r| r.get(0))?;
    Ok(n as u64)
}

fn count_distinct(conn: &Connection, column: &str, since: Option<&str>) -> Result<u64> {
    let base = format!("SELECT COUNT(DISTINCT {column}) FROM entries");
    let (sql, bind) = with_since(&base, since);
    let n: i64 = conn.query_row(&sql, rusqlite::params_from_iter(bind.iter()), |r| r.get(0))?;
    Ok(n as u64)
}

fn count_with_clause(conn: &Connection, clause: &str, since: Option<&str>) -> Result<u64> {
    let base = format!("SELECT COUNT(*) FROM entries WHERE {clause}");
    let (sql, bind) = append_since(&base, since);
    let n: i64 = conn.query_row(&sql, rusqlite::params_from_iter(bind.iter()), |r| r.get(0))?;
    Ok(n as u64)
}

fn kind_distribution(conn: &Connection, since: Option<&str>) -> Result<Vec<KindCount>> {
    let base = "SELECT kind, COUNT(*) AS c FROM entries";
    let (mut sql, bind) = with_since(base, since);
    sql.push_str(" GROUP BY kind ORDER BY c DESC");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(bind.iter()), |r| {
            Ok(KindCount {
                kind: r.get::<_, String>(0)?,
                count: r.get::<_, i64>(1)? as u64,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn sessions_per_agent(conn: &Connection, since: Option<&str>) -> Result<Vec<AgentCount>> {
    let base = "SELECT agent, COUNT(DISTINCT session_id) AS c FROM entries";
    let (mut sql, bind) = with_since(base, since);
    sql.push_str(" GROUP BY agent ORDER BY c DESC");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(bind.iter()), |r| {
            Ok(AgentCount {
                agent: r.get::<_, String>(0)?,
                session_count: r.get::<_, i64>(1)? as u64,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn top_tags(conn: &Connection, limit: usize, since: Option<&str>) -> Result<Vec<TagCount>> {
    // Two query shapes: a JOIN through entries when there's a
    // `since` filter (so we can compare on `entries.ts`), and a
    // direct read of `entry_tags` otherwise. The simpler shape
    // skips one b-tree lookup per row when the filter isn't needed.
    let (mut sql, mut bind): (String, Vec<Value>) = if let Some(s) = since {
        (
            "SELECT t.tag, COUNT(*) AS c FROM entry_tags t \
             JOIN entries e ON e.id = t.entry_id WHERE e.ts >= ?1 \
             GROUP BY t.tag ORDER BY c DESC LIMIT ?"
                .to_string(),
            vec![Value::Text(s.to_string())],
        )
    } else {
        (
            "SELECT tag, COUNT(*) AS c FROM entry_tags \
             GROUP BY tag ORDER BY c DESC LIMIT ?"
                .to_string(),
            Vec::new(),
        )
    };
    // Fix up the trailing `LIMIT ?` to use the next param index.
    let limit_idx = bind.len() + 1;
    sql = sql.replace("LIMIT ?", &format!("LIMIT ?{limit_idx}"));
    bind.push(Value::Integer(i64::try_from(limit).unwrap_or(i64::MAX)));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(bind.iter()), |r| {
            Ok(TagCount {
                tag: r.get::<_, String>(0)?,
                count: r.get::<_, i64>(1)? as u64,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn top_files(conn: &Connection, limit: usize, since: Option<&str>) -> Result<Vec<FileCount>> {
    let (mut sql, mut bind): (String, Vec<Value>) = if let Some(s) = since {
        (
            "SELECT f.path, COUNT(*) AS c FROM entry_files f \
             JOIN entries e ON e.id = f.entry_id WHERE e.ts >= ?1 \
             GROUP BY f.path ORDER BY c DESC LIMIT ?"
                .to_string(),
            vec![Value::Text(s.to_string())],
        )
    } else {
        (
            "SELECT path, COUNT(*) AS c FROM entry_files \
             GROUP BY path ORDER BY c DESC LIMIT ?"
                .to_string(),
            Vec::new(),
        )
    };
    let limit_idx = bind.len() + 1;
    sql = sql.replace("LIMIT ?", &format!("LIMIT ?{limit_idx}"));
    bind.push(Value::Integer(i64::try_from(limit).unwrap_or(i64::MAX)));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(bind.iter()), |r| {
            Ok(FileCount {
                path: r.get::<_, String>(0)?,
                count: r.get::<_, i64>(1)? as u64,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Activity histogram: per-day entry counts for the last `window`
/// days, keyed by `YYYY-MM-DD`. Days with zero entries are kept in
/// the output (with `count = 0`) so the histogram has a stable
/// shape — easier to render and to spot gaps.
/// Activity histogram. Returns `window + 1` daily buckets covering
/// the cutoff day through today (inclusive on both ends): the SQL
/// cutoff is `now - window days` and matches `ts >=` inclusively,
/// so the bucket loop must extend to the cutoff day too — otherwise
/// an entry written exactly `window` days ago would land in the
/// SQL result with no Rust bucket to receive it and silently drop
/// from the rendered timeline.
fn activity_per_day(conn: &Connection, window: u32, now: DateTime<Utc>) -> Result<Vec<DayCount>> {
    if window == 0 {
        return Ok(Vec::new());
    }
    let cutoff = now - Duration::try_days(i64::from(window)).unwrap_or(Duration::zero());
    // SQLite's `substr(ts, 1, 10)` peels off the YYYY-MM-DD prefix
    // from the RFC3339 timestamps we store. That keeps the bucket
    // computation in SQL — no need to pull every entry into Rust.
    let mut stmt = conn.prepare(
        "SELECT substr(ts, 1, 10) AS day, COUNT(*) AS c \
         FROM entries WHERE ts >= ?1 GROUP BY day",
    )?;
    let mut by_day: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let rows = stmt.query_map([cutoff.to_rfc3339()], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
    })?;
    for row in rows {
        let (day, c) = row?;
        by_day.insert(day, c);
    }
    // Fill in zeros for any missing day in the window. `0..=window`
    // (inclusive) so the cutoff day gets a bucket; see the function
    // doc for why.
    let mut out = Vec::with_capacity(window as usize + 1);
    for i in 0..=window {
        let day = (now - Duration::try_days(i64::from(i)).unwrap_or(Duration::zero()))
            .format("%Y-%m-%d")
            .to_string();
        let count = by_day.get(&day).copied().unwrap_or(0);
        out.push(DayCount { date: day, count });
    }
    // Reverse so the oldest day is first — reads left-to-right as
    // a timeline.
    out.reverse();
    Ok(out)
}

fn compute_dead_end_ratio(kinds: &[KindCount]) -> Option<f64> {
    let mut decisions = 0u64;
    let mut dead_ends = 0u64;
    for k in kinds {
        match k.kind.as_str() {
            "decision" => decisions = k.count,
            "dead_end" => dead_ends = k.count,
            _ => {}
        }
    }
    let total = decisions + dead_ends;
    if total == 0 {
        None
    } else {
        Some(dead_ends as f64 / total as f64)
    }
}

/// Append `WHERE ts >= ?1` to a base query when a `since` filter is
/// set. Returns `(sql, bind)` ready to feed to `params_from_iter`.
/// Used by queries that hit `entries` directly; queries that join
/// through `entry_tags` / `entry_files` build their own SQL because
/// the table alias differs.
fn with_since(base: &str, since: Option<&str>) -> (String, Vec<Value>) {
    match since {
        Some(s) => (
            format!("{base} WHERE ts >= ?1"),
            vec![Value::Text(s.to_string())],
        ),
        None => (base.to_string(), Vec::new()),
    }
}

/// Like [`with_since`] but appends `AND ts >= ?N` (rather than
/// `WHERE`) — for queries that already have a `WHERE` clause.
fn append_since(base: &str, since: Option<&str>) -> (String, Vec<Value>) {
    match since {
        Some(s) => (
            format!("{base} AND ts >= ?1"),
            vec![Value::Text(s.to_string())],
        ),
        None => (base.to_string(), Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::refresh_index;
    use crate::schema;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use tempyr_journal::{EntryDraft, Kind, Session, write_entry};

    fn fresh_repo() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let outer = tempfile::tempdir().unwrap();
        let repo = outer.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [
            ["init", "--quiet", "--initial-branch=main"].as_slice(),
            ["config", "user.name", "tempyr-test"].as_slice(),
            ["config", "user.email", "tempyr-test@example.com"].as_slice(),
        ] {
            let out = Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .expect("spawn git");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let common = repo.join(".git");
        (outer, repo, common)
    }

    fn write_one(common: &Path, repo: &Path, kind: Kind, summary: &str, files: Vec<String>) {
        let session = Session::open_or_resume(common, repo, "claude").unwrap();
        let mut draft = EntryDraft::new(kind, format!("{summary} — long enough for the validator"));
        if matches!(kind, Kind::Decision) {
            draft.detail = Some("x".repeat(60));
            draft.chosen = Some("a".to_string());
            draft.rationale = Some("a is reversible".to_string());
            draft.reversible = Some(true);
        }
        if matches!(kind, Kind::DeadEnd) {
            draft.detail = Some("x".repeat(60));
            draft.approach = Some("tried a".to_string());
            draft.failure_mode = Some("hit x".to_string());
        }
        draft.files = files;
        write_entry(&session, repo, draft).unwrap();
    }

    #[test]
    fn empty_index_yields_zero_totals() {
        let (outer, repo, common) = fresh_repo();
        // Force schema creation without writing entries.
        refresh_index(&common, &repo).unwrap();
        let conn = schema::open(&crate::index_db_path(&common)).unwrap();
        let report = compute_stats(&conn, &StatsOptions::default()).unwrap();
        assert_eq!(report.total_entries, 0);
        assert_eq!(report.total_sessions, 0);
        assert_eq!(report.kind_distribution.len(), 0);
        assert_eq!(report.dead_end_ratio, None);
        drop(outer);
    }

    #[test]
    fn kind_distribution_counts_match() {
        let (outer, repo, common) = fresh_repo();
        write_one(&common, &repo, Kind::Finding, "first finding", vec![]);
        write_one(&common, &repo, Kind::Finding, "second finding", vec![]);
        write_one(&common, &repo, Kind::Decision, "a decision", vec![]);
        write_one(&common, &repo, Kind::DeadEnd, "a dead end", vec![]);
        refresh_index(&common, &repo).unwrap();

        let conn = schema::open(&crate::index_db_path(&common)).unwrap();
        let report = compute_stats(&conn, &StatsOptions::default()).unwrap();
        assert_eq!(report.total_entries, 4);
        let by_kind: std::collections::HashMap<&str, u64> = report
            .kind_distribution
            .iter()
            .map(|k| (k.kind.as_str(), k.count))
            .collect();
        assert_eq!(by_kind.get("finding").copied(), Some(2));
        assert_eq!(by_kind.get("decision").copied(), Some(1));
        assert_eq!(by_kind.get("dead_end").copied(), Some(1));
        // 1 dead_end / (1 decision + 1 dead_end) = 0.5
        assert_eq!(report.dead_end_ratio, Some(0.5));
        drop(outer);
    }

    #[test]
    fn top_files_orders_by_reference_count() {
        let (outer, repo, common) = fresh_repo();
        std::fs::write(repo.join("hot.rs"), "").unwrap();
        std::fs::write(repo.join("cold.rs"), "").unwrap();

        write_one(
            &common,
            &repo,
            Kind::Finding,
            "first ref to hot.rs",
            vec!["hot.rs".to_string()],
        );
        write_one(
            &common,
            &repo,
            Kind::Finding,
            "second ref to hot.rs",
            vec!["hot.rs".to_string()],
        );
        write_one(
            &common,
            &repo,
            Kind::Finding,
            "ref to cold.rs",
            vec!["cold.rs".to_string()],
        );
        refresh_index(&common, &repo).unwrap();

        let conn = schema::open(&crate::index_db_path(&common)).unwrap();
        let report = compute_stats(&conn, &StatsOptions::default()).unwrap();
        assert_eq!(report.top_files.len(), 2);
        assert_eq!(report.top_files[0].path, "hot.rs");
        assert_eq!(report.top_files[0].count, 2);
        assert_eq!(report.top_files[1].path, "cold.rs");
        assert_eq!(report.top_files[1].count, 1);
        drop(outer);
    }

    #[test]
    fn dead_end_ratio_returns_none_with_no_decisions_or_dead_ends() {
        let (outer, repo, common) = fresh_repo();
        write_one(&common, &repo, Kind::Finding, "just a finding", vec![]);
        write_one(&common, &repo, Kind::Plan, "just a plan", vec![]);
        refresh_index(&common, &repo).unwrap();
        let conn = schema::open(&crate::index_db_path(&common)).unwrap();
        let report = compute_stats(&conn, &StatsOptions::default()).unwrap();
        assert_eq!(report.dead_end_ratio, None);
        drop(outer);
    }

    #[test]
    fn activity_histogram_includes_zero_days() {
        let (outer, repo, common) = fresh_repo();
        write_one(&common, &repo, Kind::Finding, "today's entry", vec![]);
        refresh_index(&common, &repo).unwrap();
        let conn = schema::open(&crate::index_db_path(&common)).unwrap();
        let opts = StatsOptions {
            activity_window_days: 7,
            ..Default::default()
        };
        let report = compute_stats(&conn, &opts).unwrap();
        // 7-day window emits 8 buckets (cutoff day through today,
        // inclusive on both ends) — see activity_per_day doc.
        assert_eq!(report.activity_per_day.len(), 8);
        // Last bucket (today, in chronological order) holds the
        // single entry we wrote.
        let total: u64 = report.activity_per_day.iter().map(|d| d.count).sum();
        assert_eq!(total, 1);
        drop(outer);
    }

    #[test]
    fn since_days_filter_excludes_old_entries() {
        // Backdate an entry's `ts` to two years ago in the index
        // DB, then run compute_stats with a 30-day window — the
        // entry must NOT count toward `total_entries`. Without
        // this, the test would only verify the inclusion path
        // (which works whether the WHERE clause runs or not).
        let (outer, repo, common) = fresh_repo();
        write_one(&common, &repo, Kind::Finding, "an old entry", vec![]);
        refresh_index(&common, &repo).unwrap();

        // Sanity-check: with no filter the entry IS counted.
        {
            let conn = schema::open(&crate::index_db_path(&common)).unwrap();
            let report = compute_stats(&conn, &StatsOptions::default()).unwrap();
            assert_eq!(report.total_entries, 1);
        }

        // Backdate every entry to two years ago by writing the
        // timestamp directly. Going through normal write paths
        // would either reject the past timestamp or land it as
        // "now", neither of which exercises this filter.
        {
            let conn = schema::open(&crate::index_db_path(&common)).unwrap();
            let two_years_ago = (Utc::now() - chrono::Duration::days(730)).to_rfc3339();
            conn.execute("UPDATE entries SET ts = ?1", [&two_years_ago])
                .unwrap();
        }

        // 30-day window: backdated entry excluded, so total = 0.
        let conn = schema::open(&crate::index_db_path(&common)).unwrap();
        let opts = StatsOptions {
            since_days: Some(30),
            ..Default::default()
        };
        let report = compute_stats(&conn, &opts).unwrap();
        assert_eq!(report.total_entries, 0);
        // 1000-day window: backdated entry included again, proving
        // the cutoff math is what's gating the result.
        let opts = StatsOptions {
            since_days: Some(1000),
            ..Default::default()
        };
        let report = compute_stats(&conn, &opts).unwrap();
        assert_eq!(report.total_entries, 1);
        drop(outer);
    }
}
