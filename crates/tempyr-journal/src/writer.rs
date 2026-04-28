//! Append-only JSONL writer with cross-platform locking.
//!
//! Each `append` call:
//! 1. Validates the entry via `kind::validate_entry`.
//! 2. Opens the session's JSONL file with read+append+create.
//! 3. Acquires an exclusive blocking lock (`std::fs::File::lock`, Rust 1.89+).
//! 4. Writes one full line in a single `write_all` (no torn lines on failure).
//! 5. `sync_data()` for durability.
//! 6. Drops the file handle; the lock is released automatically.
//!
//! The Windows lock requirement is that the file be opened with `read(true)`
//! (in addition to whatever write mode you want); plain `append(true)` alone
//! produces an access-denied error when locking. See rust-lang/rust#54118.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::entry::{Confidence, Entry, Polarity, Severity};
use crate::kind::{Kind, validate_entry};
use crate::redact::default_redactor;
use crate::{JournalError, Result, Session, path as jpath};

/// Transport-independent draft of a journal entry. CLI args (`LogArgs`) and
/// MCP params (`JournalLogParams`) both translate to this so the
/// path-normalization, redaction, append, and finalize pipeline lives in one
/// place.
///
/// Most fields map 1:1 to [`Entry`]; the writer copies them verbatim. The two
/// exceptions are:
///
/// - [`files`](Self::files): each absolute path under `worktree_top` is
///   stripped to a repo-relative form (forward-slash); relative inputs only
///   get separator normalization. Out-of-worktree absolute paths are kept
///   as-is and may be blocked by the redactor.
/// - [`cwd`](Self::cwd): a raw [`PathBuf`] (typically `env::current_dir()`)
///   is converted to a repo-relative string. Returns `None` for the
///   worktree root or out-of-worktree paths.
#[derive(Debug, Clone)]
pub struct EntryDraft {
    pub kind: Kind,
    pub summary: String,
    pub detail: Option<String>,
    pub tags: Vec<String>,
    /// Raw file paths; normalized to repo-relative inside `write_entry`.
    pub files: Vec<String>,
    pub references: Vec<String>,
    /// Raw current directory; normalized to repo-relative inside `write_entry`.
    pub cwd: Option<PathBuf>,
    pub provisional: bool,
    pub confidence: Option<Confidence>,
    pub severity: Option<Severity>,
    pub alternatives: Vec<String>,
    pub chosen: Option<String>,
    pub rationale: Option<String>,
    pub reversible: Option<bool>,
    pub approach: Option<String>,
    pub failure_mode: Option<String>,
    pub next_to_try: Option<String>,
    pub polarity: Option<Polarity>,
    pub passed: Option<bool>,
    pub build_ok: Option<bool>,
    pub commit_sha: Option<String>,
    pub is_final: bool,
}

impl EntryDraft {
    /// Empty draft with only the required fields populated. Optional fields
    /// default to their zero values; per-kind structured fields stay `None`
    /// and are only required when the kind demands them.
    pub fn new(kind: Kind, summary: impl Into<String>) -> Self {
        Self {
            kind,
            summary: summary.into(),
            detail: None,
            tags: Vec::new(),
            files: Vec::new(),
            references: Vec::new(),
            cwd: None,
            provisional: false,
            confidence: None,
            severity: None,
            alternatives: Vec::new(),
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
        }
    }
}

/// Result of [`write_entry`]: the persisted entry and whether the session was
/// finalized as part of this call. Callers use the entry's id/kind to format
/// transport-specific output (CLI text, MCP JSON response).
#[derive(Debug, Clone)]
pub struct WriteOutcome {
    pub entry: Entry,
    pub finalized: bool,
}

/// Build an [`Entry`] from `draft`, normalize paths against `worktree_top`,
/// run the default redactor, append to `session`'s JSONL, and finalize the
/// session if `draft.is_final`. Returns the persisted entry plus a flag.
///
/// This is the shared write pipeline used by both the CLI (`tempyr journal
/// log`) and the MCP `journal_log` tool — the only thing each transport owns
/// is parsing input into the draft and formatting the response.
pub fn write_entry(
    session: &Session,
    worktree_top: &Path,
    draft: EntryDraft,
) -> Result<WriteOutcome> {
    let mut entry = Entry::for_session(draft.kind, draft.summary, session);
    entry.detail = draft.detail;
    entry.tags = draft.tags;
    let cwd = draft.cwd.as_deref();
    entry.files = draft
        .files
        .into_iter()
        .map(|p| jpath::resolve_file_path(&p, worktree_top, cwd))
        .collect();
    entry.references = draft.references;
    entry.cwd = cwd.and_then(|c| relative_cwd(c, worktree_top));
    entry.provisional = draft.provisional;
    entry.confidence = draft.confidence;
    entry.severity = draft.severity;
    entry.alternatives = draft.alternatives;
    entry.chosen = draft.chosen;
    entry.rationale = draft.rationale;
    entry.reversible = draft.reversible;
    entry.approach = draft.approach;
    entry.failure_mode = draft.failure_mode;
    entry.next_to_try = draft.next_to_try;
    entry.polarity = draft.polarity;
    entry.passed = draft.passed;
    entry.build_ok = draft.build_ok;
    entry.commit_sha = draft.commit_sha;
    entry.is_final = draft.is_final;

    default_redactor().enforce(&mut entry)?;
    // `append` is atomic: under the JSONL lock it checks the session isn't
    // already finalized, writes the line, and on `entry.is_final` writes
    // the `.ready` marker before releasing the lock — so the finalize step
    // can't be raced by a concurrent writer.
    let finalized = entry.is_final;
    append(session, &entry)?;
    Ok(WriteOutcome { entry, finalized })
}

/// Repo-relative form of `cwd` under `worktree_top`. Returns `None` for the
/// worktree root (avoid noisy `cwd: "."`) and for out-of-worktree paths
/// (avoid leaking absolute home-dir paths into journals).
fn relative_cwd(cwd: &Path, worktree_top: &Path) -> Option<String> {
    if cwd == worktree_top {
        return None;
    }
    cwd.strip_prefix(worktree_top)
        .ok()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
}

/// Append one entry atomically with respect to session finalization.
/// Validates the entry, locks the JSONL, refuses if the session was already
/// finalized between caller's open and now, writes one full line, fsyncs,
/// and — if `entry.is_final` — drops the `.ready` marker before releasing
/// the lock. Holding the lock across all three steps prevents a concurrent
/// writer from slipping an append in between this entry and the marker, or
/// from writing to a session the publisher has already taken ownership of.
pub fn append(session: &Session, entry: &Entry) -> Result<()> {
    validate_entry(entry)?;

    let jsonl_path = session.jsonl_path();
    // `read(true)` is required for `File::lock` on Windows even though we
    // never read — see rust-lang/rust#54118.
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(&jsonl_path)?;
    file.lock().map_err(|e| JournalError::Lock(e.to_string()))?;

    // Atomicity boundary opens here. From this point until `file` is dropped
    // (releasing the lock) no other process can append, finalize, or commit.
    if session.is_ready() {
        return Err(JournalError::InvalidEntry(format!(
            "session {} is finalized; refuse to append",
            session.id()
        )));
    }

    let mut line = serde_json::to_vec(entry)?;
    line.push(b'\n');
    file.write_all(&line)?;
    file.sync_data()?;

    if entry.is_final {
        session.finalize()?;
    }
    Ok(())
}

/// Append a pre-validated entry to a JSONL path. Lower-level escape hatch
/// for the publisher/indexer that bypasses session-state checks and the
/// finalize coupling. Most callers want [`append`].
pub fn append_validated(jsonl_path: &Path, entry: &Entry) -> Result<()> {
    let mut line = serde_json::to_vec(entry)?;
    line.push(b'\n');

    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(jsonl_path)?;

    file.lock().map_err(|e| JournalError::Lock(e.to_string()))?;
    file.write_all(&line)?;
    file.sync_data()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Kind;
    use crate::entry::SCHEMA_VERSION;
    use chrono::{TimeZone, Utc};

    fn fixed_ts() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, 27, 12, 34, 56).unwrap()
    }

    fn entry_for(kind: Kind, summary: &str) -> Entry {
        Entry {
            schema_version: SCHEMA_VERSION,
            id: Entry::new_id(),
            ts: Utc::now(),
            agent: "claude".into(),
            kind,
            summary: summary.into(),
            detail: None,
            tags: vec![],
            files: vec![],
            references: vec![],
            session_id: "20260427-abcd1234-123456".into(),
            worktree_hash: "abcd1234".into(),
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
        }
    }

    fn open_test_session(common: &Path) -> Session {
        let worktree = tempfile::tempdir().unwrap();
        let session = Session::open_at(common, worktree.path(), "claude", fixed_ts()).unwrap();
        // Keep the worktree dir alive by leaking — test cleanup happens via
        // common's TempDir. We only need the path snapshot for hashing.
        std::mem::forget(worktree);
        session
    }

    fn entries_in(path: &Path) -> Vec<Entry> {
        let bytes = std::fs::read(path).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        text.lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).expect("parseable"))
            .collect()
    }

    #[test]
    fn append_writes_one_line_with_trailing_newline() {
        let common = tempfile::tempdir().unwrap();
        let session = open_test_session(common.path());

        let entry = entry_for(Kind::Finding, "first entry that is long enough to validate");
        append(&session, &entry).unwrap();

        let bytes = std::fs::read(session.jsonl_path()).unwrap();
        assert!(bytes.ends_with(b"\n"));
        assert_eq!(bytes.iter().filter(|b| **b == b'\n').count(), 1);
    }

    #[test]
    fn multiple_appends_one_per_line() {
        let common = tempfile::tempdir().unwrap();
        let session = open_test_session(common.path());

        for i in 0..5 {
            let e = entry_for(
                Kind::Plan,
                &format!("entry number {i} that is sufficiently long"),
            );
            append(&session, &e).unwrap();
        }

        let parsed = entries_in(&session.jsonl_path());
        assert_eq!(parsed.len(), 5);
    }

    #[test]
    fn validation_failure_does_not_write() {
        let common = tempfile::tempdir().unwrap();
        let session = open_test_session(common.path());

        let mut bad = entry_for(Kind::Finding, "too short");
        bad.summary = "x".into(); // below 20-char minimum
        let err = append(&session, &bad).unwrap_err();
        assert!(matches!(err, JournalError::InvalidEntry(_)));

        // No JSONL was created (writer aborted before opening).
        assert!(!session.jsonl_path().exists());
    }

    #[test]
    fn first_append_creates_jsonl() {
        let common = tempfile::tempdir().unwrap();
        let session = open_test_session(common.path());

        assert!(!session.jsonl_path().exists());
        let entry = entry_for(Kind::Plan, "first entry that is long enough to validate");
        append(&session, &entry).unwrap();
        assert!(session.jsonl_path().exists());
    }

    #[test]
    fn unicode_summary_round_trips() {
        let common = tempfile::tempdir().unwrap();
        let session = open_test_session(common.path());

        let entry = entry_for(Kind::Finding, "café — résumé naïve emoji 🦀 long enough");
        append(&session, &entry).unwrap();

        let parsed = entries_in(&session.jsonl_path());
        assert_eq!(parsed.len(), 1);
        assert_eq!(
            parsed[0].summary,
            "café — résumé naïve emoji 🦀 long enough"
        );
    }

    #[test]
    fn embedded_newlines_dont_break_jsonl() {
        let common = tempfile::tempdir().unwrap();
        let session = open_test_session(common.path());

        let mut entry = entry_for(
            Kind::Finding,
            "summary with \nembedded\n newlines that's long enough",
        );
        entry.detail = Some("detail\nwith\nnewlines\tand\ttabs\rand\rcontrol\0bytes".into());
        append(&session, &entry).unwrap();

        let parsed = entries_in(&session.jsonl_path());
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].summary, entry.summary);
        assert_eq!(parsed[0].detail, entry.detail);
    }

    #[test]
    fn large_detail_writes_atomically() {
        let common = tempfile::tempdir().unwrap();
        let session = open_test_session(common.path());

        // 100 KB detail — well above any plausible single-write atomicity
        // boundary on Windows or Linux.
        let big = "a".repeat(100_000);
        let mut entry = entry_for(Kind::Finding, "big entry with a long-enough summary text");
        entry.detail = Some(big.clone());
        append(&session, &entry).unwrap();

        let parsed = entries_in(&session.jsonl_path());
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].detail.as_deref(), Some(big.as_str()));
    }

    #[test]
    fn concurrent_writers_no_torn_lines() {
        // Many threads append simultaneously; every line must parse cleanly,
        // and the final count must equal total appends.
        let common = tempfile::tempdir().unwrap();
        let session = open_test_session(common.path());

        const THREADS: usize = 8;
        const PER_THREAD: usize = 32;

        let session_arc = std::sync::Arc::new(session);
        let mut handles = Vec::new();
        for t in 0..THREADS {
            let s = session_arc.clone();
            handles.push(std::thread::spawn(move || {
                for i in 0..PER_THREAD {
                    let e = entry_for(
                        Kind::Plan,
                        &format!("thread {t} entry {i} long enough text padding goes here"),
                    );
                    append(&s, &e).unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let parsed = entries_in(&session_arc.jsonl_path());
        assert_eq!(parsed.len(), THREADS * PER_THREAD);
    }

    #[test]
    fn write_entry_normalizes_files_and_finalizes() {
        let common = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        let session =
            Session::open_at(common.path(), worktree.path(), "claude", fixed_ts()).unwrap();

        let cwd_subdir = worktree.path().join("crates").join("foo");
        std::fs::create_dir_all(&cwd_subdir).unwrap();
        std::fs::write(cwd_subdir.join("bar.rs"), "").unwrap();
        std::fs::write(cwd_subdir.join("baz.rs"), "").unwrap();

        let abs_file = cwd_subdir.join("bar.rs");

        let mut draft = EntryDraft::new(
            Kind::Outcome,
            "shared write_entry pipeline normalizes & finalizes",
        );
        // CLI scenario: user is in <worktree>/crates/foo and passes:
        //   --file <abs to bar.rs>     (absolute)
        //   --file baz.rs              (cwd-relative: should resolve)
        //   --file ..\foo\baz.rs       (Windows-style cwd-relative)
        draft.files = vec![
            abs_file.to_string_lossy().into_owned(),
            "baz.rs".into(),
            r"..\foo\baz.rs".into(),
        ];
        draft.cwd = Some(cwd_subdir);
        draft.passed = Some(true);
        draft.is_final = true;

        let outcome = write_entry(&session, worktree.path(), draft).unwrap();

        // All three inputs land as forward-slash, repo-relative paths.
        assert_eq!(outcome.entry.files.len(), 3);
        assert_eq!(outcome.entry.files[0], "crates/foo/bar.rs");
        assert_eq!(outcome.entry.files[1], "crates/foo/baz.rs");
        assert_eq!(outcome.entry.files[2], "crates/foo/baz.rs");
        // cwd is the repo-relative subdir.
        assert_eq!(outcome.entry.cwd.as_deref(), Some("crates/foo"));
        // Finalize ran: .ready marker present and outcome.finalized true.
        assert!(outcome.finalized);
        assert!(session.is_ready());
    }

    #[test]
    fn append_refuses_after_session_finalized() {
        let common = tempfile::tempdir().unwrap();
        let session = open_test_session(common.path());

        // First entry, then a final one — both should succeed and the
        // second should atomically write the .ready marker.
        let plan = entry_for(Kind::Plan, "first entry that is sufficiently long");
        let mut last = entry_for(Kind::Outcome, "final entry that is sufficiently long");
        last.is_final = true;

        append(&session, &plan).unwrap();
        append(&session, &last).unwrap();
        assert!(
            session.is_ready(),
            ".ready marker should be set after final"
        );

        // Any subsequent append (e.g., a stale CLI from another shell that
        // resumed before the .ready was written) must be refused.
        let stale = entry_for(Kind::Finding, "post-final straggler should be refused");
        let err = append(&session, &stale).unwrap_err();
        match err {
            JournalError::InvalidEntry(msg) => {
                assert!(msg.contains("finalized"), "got: {msg}");
            }
            other => panic!("expected InvalidEntry, got {other:?}"),
        }

        // The straggler must NOT be in the JSONL.
        let parsed = entries_in(&session.jsonl_path());
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn append_validated_writes_past_finalized_session() {
        // The lower-level escape hatch (used by the publisher) should keep
        // working even when .ready exists — it skips session-state checks.
        let common = tempfile::tempdir().unwrap();
        let session = open_test_session(common.path());
        session.finalize().unwrap();

        let entry = entry_for(Kind::Plan, "publisher-style write past .ready");
        append_validated(&session.jsonl_path(), &entry).unwrap();
        assert_eq!(entries_in(&session.jsonl_path()).len(), 1);
    }

    #[test]
    fn write_entry_skips_finalize_when_not_final() {
        let common = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        let session =
            Session::open_at(common.path(), worktree.path(), "claude", fixed_ts()).unwrap();
        let draft = EntryDraft::new(Kind::Finding, "non-final entry should not write .ready");
        let outcome = write_entry(&session, worktree.path(), draft).unwrap();
        assert!(!outcome.finalized);
        assert!(!session.is_ready());
    }

    #[test]
    fn write_entry_blocks_redacted_input() {
        let common = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        let session =
            Session::open_at(common.path(), worktree.path(), "claude", fixed_ts()).unwrap();
        let mut draft = EntryDraft::new(
            Kind::Finding,
            "found ghp_abcdefghijklmnopqrstuvwxyz0123456789AB by accident",
        );
        // Force a redactor block via a real-looking GitHub PAT in summary.
        draft.is_final = false;
        let err = write_entry(&session, worktree.path(), draft).unwrap_err();
        assert!(matches!(err, JournalError::Redacted { .. }));
        // Block before append: nothing was written.
        assert!(!session.jsonl_path().exists());
    }

    #[test]
    fn append_validated_skips_validation() {
        // Lower-level entry point (used by the publisher and tests) should
        // accept entries the high-level path would reject.
        let common = tempfile::tempdir().unwrap();
        let session = open_test_session(common.path());

        let mut bad = entry_for(Kind::Finding, "x"); // too short summary
        bad.summary = "x".into();
        append_validated(&session.jsonl_path(), &bad).unwrap();

        let bytes = std::fs::read(session.jsonl_path()).unwrap();
        assert!(bytes.ends_with(b"\n"));
    }
}
