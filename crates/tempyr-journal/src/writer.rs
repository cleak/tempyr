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
use std::path::Path;

use crate::Session;
use crate::entry::Entry;
use crate::kind::validate_entry;
use crate::{JournalError, Result};

/// Append one entry to its session's JSONL. Validates first; on failure
/// nothing is written.
pub fn append(session: &Session, entry: &Entry) -> Result<()> {
    validate_entry(entry)?;
    append_validated(&session.jsonl_path(), entry)
}

/// Append a pre-validated entry. Lower-level entry point for the publisher
/// and indexer; most callers want `append`.
pub fn append_validated(jsonl_path: &Path, entry: &Entry) -> Result<()> {
    let mut line = serde_json::to_vec(entry)?;
    line.push(b'\n');

    // `read(true)` is required for `File::lock` on Windows even though we
    // never read — see rust-lang/rust#54118.
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
