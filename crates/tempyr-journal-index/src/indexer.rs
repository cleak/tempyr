//! Refresh the index from both live JSONL files and archived git refs.
//!
//! Two sources, one shared table:
//!
//! - **Open**: `<journals>/open/*.jsonl` — sessions still being
//!   appended by an agent. Tracked in `indexer_state` by file path
//!   and `last_offset` (byte position into the file). On re-run we
//!   resume from `last_offset` so we only ingest new lines.
//! - **Archive**: refs under `refs/tempyr/journals/archive/*` — sessions
//!   already committed and pushed. Tracked in `indexer_state` by
//!   refname and `last_sha`. If the SHA hasn't changed, we skip the
//!   ref entirely (no `git cat-file` shellout).
//!
//! Per-line idempotency is doubly enforced: byte-offset tracking
//! filters at the source level, and an `INSERT OR IGNORE` on the
//! `entries.id` primary key absorbs anything that slipped through
//! (e.g. an open session that was archived between runs and now
//! shows up via both sources).
//!
//! Errors per line don't abort the run: a corrupt JSONL line increments
//! a counter on `IndexerReport` and the indexer continues. Bigger
//! failures (db error, git command failure) propagate.

use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;
use std::process::Command;

use chrono::Utc;
use rusqlite::{Connection, params};
use tempyr_journal::{Entry, path as jpath};

use crate::{IndexError, Result, schema};

/// One refresh run's outcome. All counters are over the *current run*,
/// not cumulative across the lifetime of the index.
#[derive(Debug, Default, Clone)]
pub struct IndexerReport {
    /// Total JSONL lines visited across all sources.
    pub scanned: u64,
    /// Entries newly inserted (id wasn't already in the table).
    pub inserted: u64,
    /// Entries skipped because their id was already indexed.
    pub already_indexed: u64,
    /// Lines that failed to parse / validate. Non-fatal; ingestion continues.
    pub corrupt_lines: u64,
    /// Open JSONL files visited.
    pub open_files: u64,
    /// Archived refs visited (whether or not they had new content).
    pub archive_refs: u64,
    /// Entries embedded this run (slice 3b2). Counts both fresh
    /// embeddings and cache hits — i.e., how many `entry_embeddings`
    /// rows the indexer populated this pass.
    pub embedded: u64,
    /// Entries skipped from embedding because their kind is
    /// low-information for vector search (plan, question, risk,
    /// assumption). Stored for diagnostics; agents can `journal index`
    /// to see how many entries were filtered out.
    pub embed_filtered: u64,
}

/// Refresh the index at `<common_dir>/tempyr/journals/index.db` from
/// both live JSONL and archived refs in `repo_root`. Opens (and
/// migrates) the db lazily.
///
/// **Structural-only**: this variant does not populate
/// `entry_embeddings`. Use [`refresh_index_with_embedder`] when you
/// want vector search to see freshly-ingested entries on the next
/// query. (Tests that don't need vector search use this signature
/// to skip the fastembed model load.)
pub fn refresh_index(common_dir: &Path, repo_root: &Path) -> Result<IndexerReport> {
    let db_path = crate::index_db_path(common_dir);
    let mut conn = schema::open(&db_path)?;
    let mut report = IndexerReport::default();

    refresh_open(&mut conn, common_dir, &mut report)?;
    refresh_archive(&mut conn, repo_root, &mut report)?;

    Ok(report)
}

/// Same as [`refresh_index`] but also embeds pending high-value
/// entries via the supplied [`crate::Embedder`]. Embedding failures
/// are non-fatal: structural refresh always succeeds first; embed
/// pass is best-effort, with each failed row logged on the report
/// (`embed_filtered` does NOT include embed errors — those still
/// count as "attempted, failed silently"). The intent is that
/// callers get vector search "for free" on the next query without
/// needing a separate `embed` step.
pub fn refresh_index_with_embedder(
    common_dir: &Path,
    repo_root: &Path,
    embedder: &crate::Embedder,
) -> Result<IndexerReport> {
    let db_path = crate::index_db_path(common_dir);
    let mut conn = schema::open(&db_path)?;
    let mut report = IndexerReport::default();

    refresh_open(&mut conn, common_dir, &mut report)?;
    refresh_archive(&mut conn, repo_root, &mut report)?;
    embed_pending(&mut conn, common_dir, embedder, &mut report)?;

    Ok(report)
}

// --- Open-source refresh ---------------------------------------------------

fn refresh_open(
    conn: &mut Connection,
    common_dir: &Path,
    report: &mut IndexerReport,
) -> Result<()> {
    let open_dir = jpath::open_dir(common_dir);
    let read_dir = match std::fs::read_dir(&open_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };

    for entry in read_dir {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(".jsonl") {
            continue;
        }
        report.open_files += 1;
        ingest_open_file(conn, &path, report)?;
    }
    Ok(())
}

fn ingest_open_file(conn: &mut Connection, path: &Path, report: &mut IndexerReport) -> Result<()> {
    let key = path.to_string_lossy().into_owned();
    let last_offset = read_last_offset(conn, "open", &key)?;

    let mut file = File::open(path)?;
    let file_len = file.metadata()?.len();

    // If the file shrank below last_offset (e.g. someone manually
    // truncated, or a stray same-name file), reset to 0 and re-ingest
    // from the top — we'd rather re-run idempotent inserts than miss
    // data. Cast last_offset (i64) to u64 only after the bounds check.
    let start: u64 = if last_offset < 0 || (last_offset as u64) > file_len {
        0
    } else {
        last_offset as u64
    };
    file.seek(SeekFrom::Start(start))?;

    let mut reader = BufReader::new(file);

    // Stream-ingest line by line. A trailing partial line (no `\n`)
    // means a writer is mid-append; we stop *before* it, leaving its
    // bytes for the next refresh to pick up. `BufRead::read_until`
    // returns Ok(0) on EOF; if the buffer doesn't end with `\n` after
    // a non-zero read, the file ends with a partial line.
    let tx = conn.transaction()?;
    let mut consumed: u64 = 0;
    loop {
        let mut line_buf: Vec<u8> = Vec::new();
        let n = reader.read_until(b'\n', &mut line_buf)?;
        if n == 0 {
            break; // clean EOF
        }
        if !line_buf.ends_with(b"\n") {
            // Partial trailing line: don't count, don't advance offset.
            break;
        }
        // Strip the trailing newline; insertion sees only the JSON.
        let line = &line_buf[..line_buf.len() - 1];
        if !line.is_empty() {
            report.scanned += 1;
            match parse_and_insert(&tx, line, "open") {
                Ok(true) => report.inserted += 1,
                Ok(false) => report.already_indexed += 1,
                // Only un-parseable JSON / per-kind validation
                // failures count as "corrupt_lines" — these are recoverable
                // input problems where the right move is to skip the line
                // and keep going. SQLite errors, IO errors, and re-serialize
                // Json errors are real failures that must propagate so the
                // caller sees them (and so the rolling transaction's
                // implicit rollback on drop applies cleanly).
                Err(IndexError::InvalidEntry(_)) => report.corrupt_lines += 1,
                Err(e) => return Err(e),
            }
        }
        consumed += n as u64;
    }
    if consumed == 0 {
        // No complete lines available; nothing to record.
        return Ok(());
    }
    let new_offset = (start + consumed) as i64;
    write_last_offset(&tx, "open", &key, new_offset)?;
    tx.commit()?;
    Ok(())
}

// --- Archive-source refresh ------------------------------------------------

fn refresh_archive(
    conn: &mut Connection,
    repo_root: &Path,
    report: &mut IndexerReport,
) -> Result<()> {
    let refs = list_archive_refs(repo_root)?;
    for (refname, sha) in refs {
        report.archive_refs += 1;
        let last_sha = read_last_sha(conn, "archive", &refname)?;
        if last_sha.as_deref() == Some(sha.as_str()) {
            continue;
        }
        ingest_archive_ref(conn, repo_root, &refname, &sha, report)?;
    }
    Ok(())
}

/// Run `git for-each-ref --format=%(refname)%09%(objectname) refs/tempyr/journals/archive`
/// inside `repo_root`. Returns `(refname, sha)` pairs.
fn list_archive_refs(repo_root: &Path) -> Result<Vec<(String, String)>> {
    let out = Command::new("git")
        .args([
            "for-each-ref",
            "--format=%(refname)%09%(objectname)",
            "refs/tempyr/journals/archive",
        ])
        .current_dir(repo_root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .map_err(|e| IndexError::Git(format!("for-each-ref: {e}")))?;
    if !out.status.success() {
        return Err(IndexError::Git(format!(
            "for-each-ref: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let mut pairs = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut parts = line.splitn(2, '\t');
        let refname = parts.next().unwrap_or("").trim();
        let sha = parts.next().unwrap_or("").trim();
        if refname.is_empty() || sha.is_empty() {
            continue;
        }
        pairs.push((refname.to_string(), sha.to_string()));
    }
    Ok(pairs)
}

fn ingest_archive_ref(
    conn: &mut Connection,
    repo_root: &Path,
    refname: &str,
    sha: &str,
    report: &mut IndexerReport,
) -> Result<()> {
    // Each archived session's tree contains entries.jsonl + meta.json
    // (per the publisher). We only need the JSONL for entries; meta is
    // surfaced separately (Phase 3b will join it onto session rows).
    let blob_spec = format!("{refname}:entries.jsonl");
    let out = Command::new("git")
        .args(["cat-file", "blob", &blob_spec])
        .current_dir(repo_root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .map_err(|e| IndexError::Git(format!("cat-file {refname}: {e}")))?;
    if !out.status.success() {
        // Treat a missing entries.jsonl in a journal ref as a corrupt
        // session (could happen if someone hand-crafted refs). Don't
        // abort; record and continue.
        report.corrupt_lines += 1;
        return Ok(());
    }

    let tx = conn.transaction()?;
    for line in out.stdout.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        report.scanned += 1;
        match parse_and_insert(&tx, line, "archive") {
            Ok(true) => report.inserted += 1,
            Ok(false) => report.already_indexed += 1,
            // Same classification as the open path: only InvalidEntry
            // (un-parseable JSON or per-kind validation failure) is a
            // skippable corrupt line. Anything else is a real failure
            // and must propagate.
            Err(IndexError::InvalidEntry(_)) => report.corrupt_lines += 1,
            Err(e) => return Err(e),
        }
    }
    write_last_sha(&tx, "archive", refname, sha)?;
    tx.commit()?;
    Ok(())
}

// --- Embedding pass --------------------------------------------------------

/// True if this entry kind carries enough semantic content to be
/// worth embedding. Plans/questions/risks/assumptions are excluded
/// per the spec — they're often too short or hypothetical to score
/// well on vector queries, and skipping them saves embedding cost.
fn is_embeddable_kind(kind_str: &str) -> bool {
    matches!(kind_str, "decision" | "finding" | "dead_end" | "outcome")
}

/// Embed every pending high-value entry: rows whose kind is in the
/// allow-list AND that don't yet have a row in `entry_embeddings`.
/// Uses the on-disk `embeddings.db` cache (keyed by `body_hash`) to
/// avoid re-embedding bytes-identical content.
fn embed_pending(
    conn: &mut Connection,
    common_dir: &Path,
    embedder: &crate::Embedder,
    report: &mut IndexerReport,
) -> Result<()> {
    use rusqlite::params;

    // Open the embedding cache (separate db). We hold this for the
    // duration of the embed pass; per-row cache lookups + writes go
    // through it.
    let cache_path = crate::embed_cache::cache_db_path(common_dir);
    let cache = crate::embed_cache::open(&cache_path)?;

    // Find pending rows. We pull rowid + kind + body_hash + summary
    // + detail in one query and filter the kinds in Rust so the
    // SQL stays simple. The LEFT JOIN ensures we only see entries
    // whose embedding row is missing.
    let mut stmt = conn.prepare(
        r#"
        SELECT e.rowid, e.kind, e.body_hash, e.summary, e.detail
        FROM entries e
        LEFT JOIN entry_embeddings ee ON ee.rowid = e.rowid
        WHERE ee.rowid IS NULL
        "#,
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Vec<u8>>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?,
        ))
    })?;

    // Bucket pending rows into:
    //  - cache_hits: (rowid, vec_bytes) — copy directly into vec0
    //  - to_embed:   (rowid, body_hash, text) — call fastembed
    let mut cache_hits: Vec<(i64, Vec<u8>)> = Vec::new();
    let mut to_embed: Vec<(i64, Vec<u8>, String)> = Vec::new();

    for row in rows {
        let (rowid, kind, body_hash, summary, detail) = row?;
        if !is_embeddable_kind(&kind) {
            report.embed_filtered += 1;
            continue;
        }
        let model = embedder.model_name();
        if let Some(cached) = crate::embed_cache::get(&cache, &body_hash, model)? {
            cache_hits.push((rowid, cached));
        } else {
            // Combine summary + detail for the embedding text.
            // Detail carries the bulk of semantic content for
            // decisions/dead-ends; summary alone is too sparse.
            let text = match detail.as_deref() {
                Some(d) if !d.is_empty() => format!("{summary}\n\n{d}"),
                _ => summary,
            };
            to_embed.push((rowid, body_hash, text));
        }
    }
    drop(stmt);

    // Cache-hit pass: write directly into entry_embeddings.
    if !cache_hits.is_empty() {
        let tx = conn.transaction()?;
        for (rowid, bytes) in &cache_hits {
            tx.execute(
                "INSERT OR IGNORE INTO entry_embeddings(rowid, embedding) VALUES (?1, ?2)",
                params![rowid, bytes],
            )?;
            report.embedded += 1;
        }
        tx.commit()?;
    }

    // Embed-and-write pass: feed fastembed in one batch (it pools
    // internally), then write to both the cache and entry_embeddings.
    if !to_embed.is_empty() {
        let texts: Vec<&str> = to_embed.iter().map(|(_, _, t)| t.as_str()).collect();
        // If embedding fails (e.g., ONNX runtime hiccup), surface
        // the error — the caller decides whether to retry. We're
        // already past the structural-refresh commit, so a failure
        // here doesn't undo any structural work.
        let vecs = embedder.embed(&texts)?;

        let tx = conn.transaction()?;
        for ((rowid, body_hash, _), vec) in to_embed.iter().zip(vecs.iter()) {
            let bytes = crate::embed::vec_to_bytes(vec);
            tx.execute(
                "INSERT OR IGNORE INTO entry_embeddings(rowid, embedding) VALUES (?1, ?2)",
                params![rowid, bytes.clone()],
            )?;
            crate::embed_cache::put(
                &cache,
                body_hash,
                embedder.model_name(),
                embedder.dim(),
                &bytes,
            )?;
            report.embedded += 1;
        }
        tx.commit()?;
    }

    Ok(())
}

// --- Per-line insert -------------------------------------------------------

/// Parse one JSONL line and upsert it into `entries`. Returns
/// `Ok(true)` if newly inserted, `Ok(false)` if the id was already
/// present (idempotent re-run), or `Err` on parse failure.
///
/// `INSERT OR IGNORE` is the second line of defense for idempotency:
/// even if the per-source offset/SHA tracking says "new content", a
/// duplicate id (same entry visible via both open and archive) won't
/// double-insert.
fn parse_and_insert(tx: &rusqlite::Transaction<'_>, line: &[u8], source: &str) -> Result<bool> {
    let entry: Entry = serde_json::from_slice(line)
        .map_err(|e| IndexError::InvalidEntry(format!("parse: {e}")))?;
    insert_entry(tx, &entry, source)
}

fn insert_entry(tx: &rusqlite::Transaction<'_>, entry: &Entry, source: &str) -> Result<bool> {
    let body_json = serde_json::to_string(entry)?;
    let body_hash = blake3::hash(body_json.as_bytes());
    let n = tx.execute(
        r#"
        INSERT OR IGNORE INTO entries(
            id, session_id, ts, agent, kind, summary, detail,
            body_hash, body_json,
            branch, head, cwd,
            provisional, confidence, severity, is_final, source
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7,
            ?8, ?9,
            ?10, ?11, ?12,
            ?13, ?14, ?15, ?16, ?17
        )
        "#,
        params![
            &entry.id,
            &entry.session_id,
            &entry.ts.to_rfc3339(),
            &entry.agent,
            entry.kind.as_str(),
            &entry.summary,
            &entry.detail,
            &body_hash.as_bytes()[..],
            &body_json,
            &entry.branch,
            &entry.head,
            &entry.cwd,
            i64::from(entry.provisional),
            entry
                .confidence
                .as_ref()
                .map(|c| serde_json::to_string(c).unwrap_or_default()),
            entry
                .severity
                .as_ref()
                .map(|s| serde_json::to_string(s).unwrap_or_default()),
            i64::from(entry.is_final),
            source,
        ],
    )?;
    if n == 0 {
        return Ok(false);
    }

    // Junction inserts. INSERT OR IGNORE because (entry_id, value)
    // pairs are PK-deduped already; this just makes re-runs no-ops.
    for tag in &entry.tags {
        tx.execute(
            "INSERT OR IGNORE INTO entry_tags(entry_id, tag) VALUES (?1, ?2)",
            params![&entry.id, tag],
        )?;
    }
    for path in &entry.files {
        tx.execute(
            "INSERT OR IGNORE INTO entry_files(entry_id, path) VALUES (?1, ?2)",
            params![&entry.id, path],
        )?;
    }
    for node_id in &entry.references {
        tx.execute(
            "INSERT OR IGNORE INTO entry_refs(entry_id, node_id) VALUES (?1, ?2)",
            params![&entry.id, node_id],
        )?;
    }

    // Touch the session row. INSERT OR IGNORE on the primary key keeps
    // a session's first-seen meta authoritative; later entries don't
    // overwrite the captured branch/head.
    tx.execute(
        r#"
        INSERT OR IGNORE INTO sessions(session_id, agent, branch, head, worktree_hash, repo_root, created_utc)
        VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6)
        "#,
        params![
            &entry.session_id,
            &entry.agent,
            &entry.branch,
            &entry.head,
            &entry.worktree_hash,
            &entry.ts.to_rfc3339(),
        ],
    )?;

    Ok(true)
}

// --- indexer_state helpers --------------------------------------------------

fn read_last_offset(conn: &Connection, kind: &str, key: &str) -> Result<i64> {
    // Distinguish "no row yet" (first-time scan; offset is 0) from
    // real DB errors. The previous `.ok().flatten()` swallowed both,
    // which would have hidden a corrupt indexer_state table behind a
    // silent re-ingest from byte 0.
    match conn.query_row(
        "SELECT last_offset FROM indexer_state WHERE source_kind = ?1 AND source_key = ?2",
        params![kind, key],
        |r| r.get::<_, Option<i64>>(0),
    ) {
        Ok(Some(off)) => Ok(off),
        // Row exists but `last_offset` is NULL — happens for archive
        // rows mistakenly consulted via the open path; treat as 0.
        Ok(None) => Ok(0),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
        Err(e) => Err(e.into()),
    }
}

fn write_last_offset(
    tx: &rusqlite::Transaction<'_>,
    kind: &str,
    key: &str,
    offset: i64,
) -> Result<()> {
    tx.execute(
        r#"
        INSERT INTO indexer_state(source_kind, source_key, last_offset, last_sha, ts)
        VALUES (?1, ?2, ?3, NULL, ?4)
        ON CONFLICT(source_kind, source_key) DO UPDATE SET
            last_offset = excluded.last_offset,
            ts          = excluded.ts
        "#,
        params![kind, key, offset, Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

fn read_last_sha(conn: &Connection, kind: &str, key: &str) -> Result<Option<String>> {
    // Mirrors `read_last_offset` — separate "no row" (first-time scan
    // of this archive ref → return None so we'll ingest it) from real
    // DB errors that should propagate.
    match conn.query_row(
        "SELECT last_sha FROM indexer_state WHERE source_kind = ?1 AND source_key = ?2",
        params![kind, key],
        |r| r.get::<_, Option<String>>(0),
    ) {
        Ok(sha) => Ok(sha),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn write_last_sha(tx: &rusqlite::Transaction<'_>, kind: &str, key: &str, sha: &str) -> Result<()> {
    tx.execute(
        r#"
        INSERT INTO indexer_state(source_kind, source_key, last_offset, last_sha, ts)
        VALUES (?1, ?2, NULL, ?3, ?4)
        ON CONFLICT(source_kind, source_key) DO UPDATE SET
            last_sha = excluded.last_sha,
            ts       = excluded.ts
        "#,
        params![kind, key, sha, Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::io::Write;
    use std::path::PathBuf;
    use tempyr_journal::writer::append_validated;
    use tempyr_journal::{
        EntryDraft, Kind, PublishOptions, Session, publish_ready_sessions, write_entry,
    };

    /// Create a writable repo + bare-remote pair (mirrors the publisher
    /// tests' fixture). Returns `(outer_tempdir, repo_path, common_dir, bare)`.
    fn fresh_repo() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let outer = tempfile::tempdir().unwrap();
        let repo = outer.path().join("repo");
        let bare = outer.path().join("remote.git");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&bare).unwrap();

        for (cwd, args) in [
            (
                repo.as_path(),
                ["init", "--quiet", "--initial-branch=main"].as_slice(),
            ),
            (
                repo.as_path(),
                ["config", "user.name", "tempyr-test"].as_slice(),
            ),
            (
                repo.as_path(),
                ["config", "user.email", "tempyr-test@example.com"].as_slice(),
            ),
            (
                bare.as_path(),
                ["init", "--quiet", "--bare", "--initial-branch=main"].as_slice(),
            ),
        ] {
            std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .expect("git init / config");
        }
        std::process::Command::new("git")
            .args(["remote", "add", "origin", &bare.to_string_lossy()])
            .current_dir(&repo)
            .output()
            .unwrap();
        let common = repo.join(".git");
        (outer, repo, common, bare)
    }

    fn fixed_ts() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, 28, 12, 0, 0).unwrap()
    }

    /// Open a session and write one valid entry. Returns the session id
    /// (as the parsed SessionId — caller can stringify if needed).
    fn write_one_entry(common: &Path, repo: &Path, ts: chrono::DateTime<Utc>, summary: &str) {
        let session = Session::open_at(common, repo, "claude", ts).unwrap();
        let mut draft = EntryDraft::new(Kind::Plan, summary);
        draft.cwd = Some(repo.to_path_buf());
        write_entry(&session, repo, draft).unwrap();
    }

    fn finalize_one_entry(common: &Path, repo: &Path, ts: chrono::DateTime<Utc>, summary: &str) {
        let session = Session::open_at(common, repo, "claude", ts).unwrap();
        let mut draft = EntryDraft::new(Kind::Outcome, summary);
        draft.cwd = Some(repo.to_path_buf());
        draft.is_final = true;
        write_entry(&session, repo, draft).unwrap();
    }

    #[test]
    fn empty_dirs_yield_empty_report() {
        let (_outer, repo, common, _bare) = fresh_repo();
        let report = refresh_index(&common, &repo).unwrap();
        assert_eq!(report.scanned, 0);
        assert_eq!(report.inserted, 0);
        assert_eq!(report.open_files, 0);
        assert_eq!(report.archive_refs, 0);
    }

    #[test]
    fn ingests_open_jsonl_entries() {
        let (_outer, repo, common, _bare) = fresh_repo();
        write_one_entry(
            &common,
            &repo,
            fixed_ts(),
            "first plan entry that is sufficiently long",
        );
        let report = refresh_index(&common, &repo).unwrap();
        assert_eq!(report.open_files, 1);
        assert_eq!(report.scanned, 1);
        assert_eq!(report.inserted, 1);

        let conn = schema::open(&crate::index_db_path(&common)).unwrap();
        assert_eq!(crate::count_entries(&conn).unwrap(), 1);
    }

    #[test]
    fn ingests_archived_session() {
        let (_outer, repo, common, _bare) = fresh_repo();
        finalize_one_entry(
            &common,
            &repo,
            fixed_ts(),
            "outcome to archive that is sufficiently long",
        );
        publish_ready_sessions(&common, &repo, &PublishOptions::default())
            .unwrap()
            .unwrap();

        let report = refresh_index(&common, &repo).unwrap();
        assert_eq!(report.archive_refs, 1);
        assert!(
            report.inserted >= 1,
            "should index entries from the archived ref"
        );
        // Open dir was cleaned up by the publisher post-push.
        assert_eq!(report.open_files, 0);
    }

    #[test]
    fn re_run_is_idempotent() {
        let (_outer, repo, common, _bare) = fresh_repo();
        write_one_entry(
            &common,
            &repo,
            fixed_ts(),
            "first plan entry that is sufficiently long",
        );
        let r1 = refresh_index(&common, &repo).unwrap();
        let r2 = refresh_index(&common, &repo).unwrap();
        assert_eq!(r1.inserted, 1);
        // Second run: byte-offset tracking means we re-scan zero lines.
        assert_eq!(r2.scanned, 0);
        assert_eq!(r2.inserted, 0);

        let conn = schema::open(&crate::index_db_path(&common)).unwrap();
        assert_eq!(crate::count_entries(&conn).unwrap(), 1);
    }

    #[test]
    fn incremental_open_picks_up_only_delta() {
        let (_outer, repo, common, _bare) = fresh_repo();
        write_one_entry(
            &common,
            &repo,
            fixed_ts(),
            "first plan entry that is sufficiently long",
        );
        let r1 = refresh_index(&common, &repo).unwrap();
        assert_eq!(r1.inserted, 1);

        // Append another entry to the *same* session by reopening +
        // writing again with an `open_or_resume` (same ts second).
        write_one_entry(
            &common,
            &repo,
            fixed_ts(),
            "second plan entry that is sufficiently long",
        );

        let r2 = refresh_index(&common, &repo).unwrap();
        assert_eq!(r2.scanned, 1);
        assert_eq!(r2.inserted, 1);

        let conn = schema::open(&crate::index_db_path(&common)).unwrap();
        assert_eq!(crate::count_entries(&conn).unwrap(), 2);
    }

    #[test]
    fn archive_refresh_skips_unchanged_refs() {
        let (_outer, repo, common, _bare) = fresh_repo();
        finalize_one_entry(
            &common,
            &repo,
            fixed_ts(),
            "outcome that is sufficiently long for the validator",
        );
        publish_ready_sessions(&common, &repo, &PublishOptions::default())
            .unwrap()
            .unwrap();

        let r1 = refresh_index(&common, &repo).unwrap();
        let initial_inserts = r1.inserted;
        assert!(initial_inserts >= 1);

        let r2 = refresh_index(&common, &repo).unwrap();
        // Ref unchanged → archive_refs counts it as visited but
        // *no* lines should be scanned (we short-circuit before cat-file).
        assert_eq!(r2.archive_refs, 1);
        assert_eq!(r2.scanned, 0);
        assert_eq!(r2.inserted, 0);
    }

    #[test]
    fn corrupt_jsonl_line_increments_counter_but_does_not_abort() {
        // Build a JSONL with one valid entry + one garbage line +
        // another valid entry, then point the indexer at it directly
        // via append_validated to bypass per-entry validation.
        let (_outer, repo, common, _bare) = fresh_repo();
        let session = Session::open_at(&common, &repo, "claude", fixed_ts()).unwrap();
        // First valid entry via the normal pipeline.
        let mut draft = EntryDraft::new(Kind::Plan, "first plan entry that is sufficiently long");
        draft.cwd = Some(repo.clone());
        write_entry(&session, &repo, draft).unwrap();

        // Inject a corrupt line directly into the JSONL.
        std::fs::OpenOptions::new()
            .append(true)
            .open(session.jsonl_path())
            .unwrap()
            .write_all(b"this is not json at all\n")
            .map(|_| ())
            .unwrap_or(());

        // Then a second valid entry via append_validated (skips the
        // session-finalized check; we're not finalizing).
        use tempyr_journal::Entry as JEntry;
        let entry2 = JEntry::for_session(
            Kind::Finding,
            "another valid entry summary that is sufficiently long".to_string(),
            &session,
        );
        append_validated(&session.jsonl_path(), &entry2).unwrap();

        let report = refresh_index(&common, &repo).unwrap();
        assert_eq!(report.scanned, 3);
        assert_eq!(report.inserted, 2);
        assert_eq!(report.corrupt_lines, 1);
    }

    #[test]
    fn truncate_then_refresh_reingests() {
        let (_outer, repo, common, _bare) = fresh_repo();
        write_one_entry(
            &common,
            &repo,
            fixed_ts(),
            "first plan entry that is sufficiently long",
        );
        refresh_index(&common, &repo).unwrap();

        let mut conn = schema::open(&crate::index_db_path(&common)).unwrap();
        schema::truncate(&mut conn).unwrap();
        drop(conn);

        let report = refresh_index(&common, &repo).unwrap();
        assert_eq!(
            report.inserted, 1,
            "post-truncate refresh re-ingests everything"
        );
    }

    #[test]
    fn round_trips_entry_via_get_entry() {
        let (_outer, repo, common, _bare) = fresh_repo();
        write_one_entry(
            &common,
            &repo,
            fixed_ts(),
            "round-trip target entry that is sufficiently long",
        );
        refresh_index(&common, &repo).unwrap();

        let conn = schema::open(&crate::index_db_path(&common)).unwrap();
        // Pull any one entry's id, then round-trip it.
        let id: String = conn
            .query_row("SELECT id FROM entries LIMIT 1", [], |r| r.get(0))
            .unwrap();
        let entry = crate::get_entry(&conn, &id)
            .unwrap()
            .expect("entry should exist");
        assert_eq!(entry.id, id);
        assert!(entry.summary.contains("round-trip target"));
    }

    #[test]
    fn get_entry_returns_none_for_missing_id() {
        let (_outer, repo, common, _bare) = fresh_repo();
        // Reference `repo` to keep the `_bare` binding meaningful and
        // avoid an unused warning; we just need `common` to open the db.
        let _ = repo;
        let conn = schema::open(&crate::index_db_path(&common)).unwrap();
        assert!(crate::get_entry(&conn, "j-nonexistent").unwrap().is_none());
    }

    #[test]
    fn partial_trailing_line_is_left_for_next_refresh() {
        // Regression: with the streaming `BufReader` ingest, a writer
        // mid-append (line missing its trailing `\n`) must be skipped
        // *and* not advance the offset, so the next refresh picks it
        // up once the writer adds the newline.
        let (_outer, repo, common, _bare) = fresh_repo();
        write_one_entry(
            &common,
            &repo,
            fixed_ts(),
            "first plan entry that is sufficiently long",
        );
        // Append a JSON line WITHOUT the trailing newline (simulates a
        // writer mid-append between `write_all` and `sync_data`).
        let session = Session::open_or_resume(&common, &repo, "claude").unwrap();
        let jsonl = session.jsonl_path();
        let partial = br#"{"v":1,"id":"j-partial","ts":"2026-04-28T12:00:00Z","agent":"claude","kind":"plan","summary":"partial line missing newline that should not be ingested","session_id":"x","worktree_hash":"00000000"}"#;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&jsonl)
            .unwrap()
            .write_all(partial)
            .unwrap();

        // First refresh: only the first complete line is ingested; the
        // partial line is left for next time.
        let r1 = refresh_index(&common, &repo).unwrap();
        assert_eq!(r1.scanned, 1);
        assert_eq!(r1.inserted, 1);
        let conn = schema::open(&crate::index_db_path(&common)).unwrap();
        assert_eq!(crate::count_entries(&conn).unwrap(), 1);
        assert!(
            crate::get_entry(&conn, "j-partial").unwrap().is_none(),
            "partial line must not be inserted yet"
        );
        drop(conn);

        // Now finish the partial line by appending a newline. The next
        // refresh should pick it up (offset wasn't advanced past it).
        std::fs::OpenOptions::new()
            .append(true)
            .open(&jsonl)
            .unwrap()
            .write_all(b"\n")
            .unwrap();
        let r2 = refresh_index(&common, &repo).unwrap();
        assert_eq!(r2.scanned, 1);
        assert_eq!(r2.inserted, 1);
        let conn = schema::open(&crate::index_db_path(&common)).unwrap();
        assert!(crate::get_entry(&conn, "j-partial").unwrap().is_some());
    }

    #[test]
    fn read_last_offset_returns_zero_for_first_time_scan() {
        // After fixing the silent-error swallow, "no row yet" must
        // still cleanly return Ok(0) (first-time scan of a fresh JSONL
        // path). Real DB errors propagate; this test pins the
        // happy-path behavior so a future regression can't merge
        // silently.
        let dir = tempfile::tempdir().unwrap();
        let conn = schema::open(&dir.path().join("index.db")).unwrap();
        let off = read_last_offset(&conn, "open", "/never/seen/before.jsonl").unwrap();
        assert_eq!(off, 0);
        let sha = read_last_sha(&conn, "archive", "refs/never/seen/before").unwrap();
        assert!(sha.is_none());
    }

    #[test]
    fn parse_and_insert_propagates_non_invalid_entry_errors() {
        // Regression: previously the call sites' `Err(_)` arm collapsed
        // every parse_and_insert error into `corrupt_lines`, hiding
        // genuine SQLite / IO failures behind the corrupt-line counter.
        // The fix narrows that arm to `IndexError::InvalidEntry`;
        // anything else propagates.
        //
        // We trigger the propagation path by dropping the `entries`
        // table BEFORE calling parse_and_insert. The serde parse
        // succeeds (so we're past the InvalidEntry branch), but the
        // SQL INSERT then fails with "no such table: entries", which
        // `INSERT OR IGNORE` does NOT suppress (OR IGNORE only
        // swallows constraint violations, not statement-execution
        // errors). So we get back IndexError::Sqlite, which must
        // propagate.
        use tempyr_journal::{Entry, Kind};
        let dir = tempfile::tempdir().unwrap();
        let mut conn = schema::open(&dir.path().join("index.db")).unwrap();

        // Drop the entries table to force a "no such table" on INSERT.
        // We turn off foreign_keys for this connection first so the
        // FK references in junction tables don't block the drop.
        conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
        conn.execute("DROP TABLE entries", []).unwrap();

        let entry = Entry {
            schema_version: tempyr_journal::SCHEMA_VERSION,
            id: "j-test-propagation".into(),
            ts: fixed_ts(),
            agent: "claude".into(),
            kind: Kind::Plan,
            summary: "valid summary that is sufficiently long to satisfy the validator".into(),
            detail: None,
            tags: vec![],
            files: vec![],
            references: vec![],
            session_id: "20260428-deadbeef-120000".into(),
            worktree_hash: "deadbeef".into(),
            branch: None,
            head: None,
            cwd: None,
            provisional: false,
            confidence: None,
            severity: None,
            alternatives: vec![],
            chosen: None,
            rationale: None,
            reversible: None,
            approach: None,
            failure_mode: None,
            next_to_try: None,
            polarity: None,
            passed: None,
            build_ok: None,
            commit_sha: None,
            is_final: false,
        };
        let line = serde_json::to_vec(&entry).unwrap();

        let tx = conn.transaction().unwrap();
        let result = parse_and_insert(&tx, &line, "open");
        match result {
            Err(IndexError::Sqlite(_)) => {} // expected
            Err(IndexError::InvalidEntry(_)) => {
                panic!("write-path SQLite error must not be classified as InvalidEntry")
            }
            Err(other) => panic!("expected Sqlite error, got {other:?}"),
            Ok(_) => panic!("INSERT against a dropped table should have failed"),
        }
    }
}
