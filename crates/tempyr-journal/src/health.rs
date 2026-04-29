//! Journal-subsystem health probe.
//!
//! Inspects the on-disk state under `<git-common-dir>/tempyr/journals/`
//! and returns a structured snapshot for `tempyr doctor` /
//! `system_doctor` to render. Designed for diagnostic visibility into
//! the publisher pipeline:
//!
//! - **open / ready counts** — distinguishes sessions still being
//!   written from sessions queued for the next publisher pass. A
//!   ready session that's been sitting around for a long time
//!   usually means the publisher hasn't run.
//! - **publisher lock** — whether some process currently holds the
//!   single-publisher lock, plus the PID it stamped (informational —
//!   the lock's liveness is enforced by the OS, not the PID stamp).
//!
//! Probes degrade gracefully: a missing `<common>/tempyr/journals/`
//! directory just produces zero counts, and read errors surface as
//! `error` strings on the report rather than panics. The journal is
//! optional infrastructure — `doctor` should never fail because of
//! something that's only a debugging aid.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::lockfile::PublisherLock;
use crate::path as jpath;

/// Snapshot of journal subsystem state for diagnostic display.
#[derive(Debug, Default, Serialize)]
pub struct JournalHealthReport {
    /// `<git-common-dir>/tempyr/journals/`. Reported even when missing
    /// so the user can see where the journal *would* live.
    pub journals_dir: PathBuf,
    pub journals_dir_exists: bool,
    /// `<journals>/open/`.
    pub open_dir: PathBuf,
    pub open_dir_exists: bool,
    /// Number of `*.jsonl` files present in `open/` that don't have
    /// a corresponding `<id>.ready` marker — i.e. sessions still
    /// being appended to.
    pub open_session_count: usize,
    /// Number of sessions that have a `.ready` marker — finalized
    /// and waiting for the publisher to archive them as a git ref.
    pub ready_session_count: usize,
    /// Whether `<journals>/publisher.lock` is currently held by some
    /// process. Determined via `try_lock`-then-drop; a non-existent
    /// lockfile reports `false`.
    pub publisher_lock_held: bool,
    /// PID stamped inside the lockfile (informational only — the
    /// lock's authority comes from the OS handle, not this PID).
    /// `None` if the file is missing, locked, or not parseable.
    pub publisher_stamped_pid: Option<u32>,
    /// Probe failures (e.g. read of `open/` denied). Non-fatal —
    /// individual fields above may still be partially populated.
    pub errors: Vec<String>,
}

/// Build a [`JournalHealthReport`] for `common_dir`. Never fails —
/// errors during the probe are recorded on the report's `errors`
/// vector rather than propagated, since `doctor` is a diagnostic
/// command and should always emit *some* output.
pub fn build_journal_health(common_dir: &Path) -> JournalHealthReport {
    let journals_dir = jpath::journals_root(common_dir);
    let open_dir = jpath::open_dir(common_dir);
    let mut report = JournalHealthReport {
        journals_dir: journals_dir.clone(),
        journals_dir_exists: journals_dir.exists(),
        open_dir: open_dir.clone(),
        open_dir_exists: open_dir.exists(),
        ..Default::default()
    };

    if report.open_dir_exists {
        match count_sessions(&open_dir) {
            Ok((open, ready)) => {
                report.open_session_count = open;
                report.ready_session_count = ready;
            }
            Err(e) => {
                report.errors.push(format!("scan open/ failed: {e}"));
            }
        }
    }

    // Lock state is best-effort: any error during the probe surfaces
    // as `held = false` (matches `PublisherLock::is_held` semantics)
    // so the user doesn't see a confusing "lock-probe failed" line
    // for an empty repo with no journals dir yet.
    report.publisher_lock_held = PublisherLock::is_held(common_dir);
    report.publisher_stamped_pid = PublisherLock::stamped_pid(common_dir);

    report
}

/// Walk `open_dir` once and bucket entries:
/// - any `*.jsonl` whose paired `<id>.ready` exists counts as ready
/// - any `*.jsonl` without that marker counts as still-open
///
/// `.meta.json` and other companion files are ignored. A `.ready`
/// without a matching `.jsonl` is silently skipped — the publisher
/// would have already archived such a sessions's content, but this
/// counts the *current* on-disk state, not historical correctness.
fn count_sessions(open_dir: &Path) -> std::io::Result<(usize, usize)> {
    use std::collections::HashSet;
    let mut jsonls: Vec<String> = Vec::new();
    let mut readys: HashSet<String> = HashSet::new();
    for entry in std::fs::read_dir(open_dir)? {
        let entry = entry?;
        let name = match entry.file_name().to_str() {
            Some(n) => n.to_string(),
            None => continue,
        };
        if let Some(stem) = name.strip_suffix(".jsonl") {
            jsonls.push(stem.to_string());
        } else if let Some(stem) = name.strip_suffix(".ready") {
            readys.insert(stem.to_string());
        }
    }
    let ready = jsonls.iter().filter(|id| readys.contains(*id)).count();
    let open = jsonls.len() - ready;
    Ok((open, ready))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_journals_dir_is_clean_report() {
        let tmp = tempfile::tempdir().unwrap();
        let report = build_journal_health(tmp.path());
        assert!(!report.journals_dir_exists);
        assert!(!report.open_dir_exists);
        assert_eq!(report.open_session_count, 0);
        assert_eq!(report.ready_session_count, 0);
        assert!(!report.publisher_lock_held);
        assert!(report.errors.is_empty());
    }

    #[test]
    fn counts_open_and_ready_sessions() {
        let tmp = tempfile::tempdir().unwrap();
        let common = tmp.path();
        let open = jpath::open_dir(common);
        std::fs::create_dir_all(&open).unwrap();

        // Two sessions: one open, one ready.
        std::fs::write(open.join("session-a.jsonl"), b"{}\n").unwrap();
        std::fs::write(open.join("session-b.jsonl"), b"{}\n").unwrap();
        std::fs::write(open.join("session-b.ready"), b"").unwrap();

        let report = build_journal_health(common);
        assert!(report.open_dir_exists);
        assert_eq!(report.open_session_count, 1);
        assert_eq!(report.ready_session_count, 1);
        assert!(report.errors.is_empty());
    }

    #[test]
    fn ignores_unrelated_files() {
        let tmp = tempfile::tempdir().unwrap();
        let common = tmp.path();
        let open = jpath::open_dir(common);
        std::fs::create_dir_all(&open).unwrap();

        // .meta.json companions and a stray .ready with no jsonl
        // shouldn't get counted as sessions.
        std::fs::write(open.join("session-a.jsonl"), b"{}\n").unwrap();
        std::fs::write(open.join("session-a.meta.json"), b"{}").unwrap();
        std::fs::write(open.join("orphan.ready"), b"").unwrap();
        std::fs::write(open.join("README.md"), b"").unwrap();

        let report = build_journal_health(common);
        assert_eq!(report.open_session_count, 1);
        assert_eq!(report.ready_session_count, 0);
    }
}
