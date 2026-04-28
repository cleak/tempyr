//! Session ID generation, validation, and per-session lifecycle.
//!
//! A session is one continuous span of agent activity on a single worktree.
//! Each session has its own `<id>.jsonl` file in `<git-common-dir>/tempyr/journals/open/`
//! and a `<id>.meta.json` sidecar. When the session ends, a `<id>.ready` marker
//! signals that the publisher may commit it as a Git ref.
//!
//! Session ID format: `YYYYMMDD-<wt_hash>-HHMMSS`
//! - `YYYYMMDD` lets the publisher derive the ref date hierarchy
//! - `wt_hash` is 8 hex chars (matches `path::worktree_hash`)
//! - `HHMMSS` is the start time UTC, second-precision (collision risk per
//!   worktree per second is negligible; if needed, the writer falls back to a
//!   short random suffix)
//!
//! The strict format also defends against path injection via session_id.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::path as jpath;
use crate::{JournalError, Result, SCHEMA_VERSION};

/// Validated session identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    /// Build a session ID from a timestamp + worktree hash. The hash must be
    /// 8 lowercase hex chars; otherwise an `InvalidEntry` error is returned.
    pub fn new(ts: DateTime<Utc>, worktree_hash: &str) -> Result<Self> {
        if !is_valid_hash(worktree_hash) {
            return Err(JournalError::InvalidEntry(format!(
                "worktree_hash must be 8 lowercase hex chars, got {worktree_hash:?}"
            )));
        }
        let s = format!(
            "{}-{}-{}",
            ts.format("%Y%m%d"),
            worktree_hash,
            ts.format("%H%M%S")
        );
        Ok(SessionId(s))
    }

    /// Parse and validate an existing session ID string. Rejects path
    /// traversal, control characters, and any deviation from the expected
    /// `YYYYMMDD-hex8-HHMMSS` shape.
    pub fn parse(s: &str) -> Result<Self> {
        if !is_valid_session_id(s) {
            return Err(JournalError::InvalidEntry(format!(
                "invalid session id: {s:?} (expected YYYYMMDD-hex8-HHMMSS)"
            )));
        }
        Ok(SessionId(s.to_string()))
    }

    /// Raw string form.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `YYYYMMDD` prefix.
    pub fn date_part(&self) -> &str {
        &self.0[..8]
    }

    /// Year (`YYYY`).
    pub fn year(&self) -> &str {
        &self.0[..4]
    }

    /// Month (`MM`).
    pub fn month(&self) -> &str {
        &self.0[4..6]
    }

    /// Day (`DD`).
    pub fn day(&self) -> &str {
        &self.0[6..8]
    }

    /// Worktree hash component.
    pub fn worktree_hash(&self) -> &str {
        &self.0[9..17]
    }

    /// Start time `HHMMSS`.
    pub fn time_part(&self) -> &str {
        &self.0[18..]
    }

    /// Ref path under `refs/tempyr/journals/archive/`.
    pub fn archive_ref_path(&self) -> String {
        format!(
            "refs/tempyr/journals/archive/{}/{}/{}/{}",
            self.year(),
            self.month(),
            self.day(),
            self.0
        )
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Per-session metadata persisted as `<session>.meta.json` next to the JSONL.
///
/// Captures one-time facts about the session that we don't want to repeat on
/// every entry. Indexer can read this once and join across all entries from
/// the same session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    /// Schema version (matches the JSONL `v` field).
    #[serde(rename = "v")]
    pub schema_version: u32,
    pub session_id: SessionId,
    pub created_utc: DateTime<Utc>,
    pub agent: String,
    /// 8-char blake3 prefix over the canonicalized (and Windows-lowercased)
    /// worktree path. Stable across machines for the same logical worktree.
    pub worktree_hash: String,
    /// Worktree top-level directory at session-open time. Canonicalized but
    /// preserves case (the lowercasing is internal to `worktree_hash`).
    pub repo_root: PathBuf,
    pub branch: Option<String>,
    pub head: Option<String>,
}

/// Handle to an in-flight session. Owns the session ID, common dir, and
/// metadata; does not hold any open file handles. The writer module uses
/// these accessors to locate the session's JSONL and acquire locks per
/// append.
#[derive(Debug, Clone)]
pub struct Session {
    common_dir: PathBuf,
    meta: SessionMeta,
}

impl Session {
    /// Open a fresh session for the given worktree. Creates the journal
    /// directory layout if missing and writes the metadata sidecar. Does
    /// not write any journal entries; that's the writer's job.
    ///
    /// Retries briefly if a different agent already opened a session at
    /// `(worktree, current_second)` — same wall-clock second means same
    /// session id, but two agents must not share a session. Spinning until
    /// the clock advances yields a fresh id.
    pub fn open(common_dir: &Path, worktree_top: &Path, agent: &str) -> Result<Self> {
        let mut last_err: Option<JournalError> = None;
        for _ in 0..30 {
            match Self::open_at(common_dir, worktree_top, agent, Utc::now()) {
                Ok(s) => return Ok(s),
                Err(JournalError::AgentMismatch { .. }) => {
                    std::thread::sleep(Duration::from_millis(60));
                    last_err = None;
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_err.unwrap_or_else(|| JournalError::AgentMismatch {
            existing: "<unknown>".into(),
            requested: agent.to_string(),
        }))
    }

    /// Open a session at an explicit timestamp. Returns
    /// `JournalError::AgentMismatch` if the session id derived from
    /// `(worktree, ts)` already exists on disk for a different agent —
    /// callers using `open()` retry with a fresh timestamp; tests use this
    /// strict form directly.
    pub fn open_at(
        common_dir: &Path,
        worktree_top: &Path,
        agent: &str,
        ts: DateTime<Utc>,
    ) -> Result<Self> {
        jpath::ensure_layout(common_dir)?;

        let wt_hash = jpath::worktree_hash(worktree_top);
        let session_id = SessionId::new(ts, &wt_hash)?;

        let branch = jpath::current_branch(worktree_top).ok().flatten();
        let head = jpath::current_head(worktree_top).ok().flatten();

        let mut meta = SessionMeta {
            schema_version: SCHEMA_VERSION,
            session_id: session_id.clone(),
            created_utc: ts,
            agent: agent.to_string(),
            worktree_hash: wt_hash,
            repo_root: worktree_top.to_path_buf(),
            branch,
            head,
        };

        let meta_path = jpath::session_meta_path(common_dir, session_id.as_str());
        let json = serde_json::to_string_pretty(&meta)?;

        // Atomic-write pattern: write fully to a unique temp, fsync, then
        // hard-link into place. hard_link fails atomically with AlreadyExists
        // if the meta is already there (rename would silently replace it,
        // which we never want). Either we win and our content is the on-disk
        // truth, or we lose and read the persisted meta — never a torn read.
        let tmp_path = unique_meta_tmp_path(&meta_path);
        write_meta_tmp(&tmp_path, json.as_bytes())?;
        let won_race = match std::fs::hard_link(&tmp_path, &meta_path) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(e) => {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(e.into());
            }
        };
        let _ = std::fs::remove_file(&tmp_path);

        if !won_race {
            // Lost the race: read the persisted meta. If the winning agent
            // matches us, reuse the session (same agent, same second is
            // fine — and same-id reuse is the common case). If it differs,
            // surface AgentMismatch so `open()` can retry with a fresh ts;
            // we never want to write entries under another agent's name.
            let existing = read_meta_with_retry(&meta_path)?;
            if existing.agent != agent {
                return Err(JournalError::AgentMismatch {
                    existing: existing.agent,
                    requested: agent.to_string(),
                });
            }
            meta = existing;
        }

        Ok(Session {
            common_dir: common_dir.to_path_buf(),
            meta,
        })
    }

    /// Open a fresh session, or reuse an active (non-finalized) one for this
    /// `(worktree, agent)` pair if one is already on disk. Prevents per-CLI
    /// invocation session sprawl: multiple `tempyr journal log` calls during
    /// the same chunk of agent activity now group into one session.
    pub fn open_or_resume(common_dir: &Path, worktree_top: &Path, agent: &str) -> Result<Self> {
        if let Some(session) = Self::find_active(common_dir, worktree_top, agent)? {
            return Ok(session);
        }
        Self::open(common_dir, worktree_top, agent)
    }

    /// Find the newest non-finalized session in `common_dir` whose worktree
    /// hash matches `worktree_top` and whose meta.agent matches `agent`.
    /// "Non-finalized" means no `<id>.ready` marker exists. Returns `None` if
    /// no candidate is found.
    pub fn find_active(
        common_dir: &Path,
        worktree_top: &Path,
        agent: &str,
    ) -> Result<Option<Self>> {
        let wt_hash = jpath::worktree_hash(worktree_top);
        let open_dir = jpath::open_dir(common_dir);
        let read_dir = match std::fs::read_dir(&open_dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };

        let mut candidates: Vec<SessionId> = Vec::new();
        for entry in read_dir {
            let entry = entry?;
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            let Some(id_str) = name.strip_suffix(".meta.json") else {
                continue;
            };
            let Ok(id) = SessionId::parse(id_str) else {
                continue;
            };
            if id.worktree_hash() != wt_hash {
                continue;
            }
            // Skip finalized sessions; the publisher owns those.
            if jpath::session_ready_marker(common_dir, id.as_str()).exists() {
                continue;
            }
            candidates.push(id);
        }
        // Newest-first by lexicographic order (== chronological for our format).
        candidates.sort_by(|a, b| b.as_str().cmp(a.as_str()));

        for id in candidates {
            let Some(session) = Self::resume(common_dir, &id)? else {
                continue;
            };
            if session.meta().agent == agent {
                return Ok(Some(session));
            }
        }
        Ok(None)
    }

    /// Resume an existing session by ID. Returns `Ok(None)` if there's no
    /// open session with that ID. Errors if the on-disk meta is for a
    /// different session id (corruption or tampering).
    pub fn resume(common_dir: &Path, session_id: &SessionId) -> Result<Option<Self>> {
        let meta_path = jpath::session_meta_path(common_dir, session_id.as_str());
        let bytes = match std::fs::read(&meta_path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let meta: SessionMeta = serde_json::from_slice(&bytes)?;
        if &meta.session_id != session_id {
            return Err(JournalError::InvalidEntry(format!(
                "session meta mismatch: expected {session_id}, found {}",
                meta.session_id
            )));
        }
        Ok(Some(Session {
            common_dir: common_dir.to_path_buf(),
            meta,
        }))
    }

    pub fn id(&self) -> &SessionId {
        &self.meta.session_id
    }

    pub fn meta(&self) -> &SessionMeta {
        &self.meta
    }

    pub fn common_dir(&self) -> &Path {
        &self.common_dir
    }

    /// Path to the session's append-only JSONL file.
    pub fn jsonl_path(&self) -> PathBuf {
        jpath::session_jsonl_path(&self.common_dir, self.meta.session_id.as_str())
    }

    /// Path to the session's metadata sidecar.
    pub fn meta_path(&self) -> PathBuf {
        jpath::session_meta_path(&self.common_dir, self.meta.session_id.as_str())
    }

    /// Path to the `.ready` marker.
    pub fn ready_marker_path(&self) -> PathBuf {
        jpath::session_ready_marker(&self.common_dir, self.meta.session_id.as_str())
    }

    /// True if the session has been finalized (the publisher may now commit it).
    pub fn is_ready(&self) -> bool {
        self.ready_marker_path().exists()
    }

    /// Mark the session as ready for the publisher. Idempotent.
    pub fn finalize(&self) -> Result<()> {
        let path = self.ready_marker_path();
        // Touch — create empty file if missing.
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        Ok(())
    }
}

// ---- Meta-sidecar atomic write helpers ----

fn write_meta_tmp(tmp_path: &Path, content: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp_path)?;
    f.write_all(content)?;
    f.write_all(b"\n")?;
    f.sync_data()?;
    Ok(())
}

fn unique_meta_tmp_path(meta_path: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let stem = meta_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    meta_path.with_file_name(format!("{stem}.tmp.{pid}.{n}"))
}

/// Read+parse the meta sidecar, retrying briefly to absorb the microsecond
/// window between a winning writer hard-linking and the link being visible.
fn read_meta_with_retry(path: &Path) -> Result<SessionMeta> {
    let mut last_err: Option<JournalError> = None;
    for attempt in 0..10 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(10));
        }
        match std::fs::read(path) {
            Ok(bytes) if bytes.is_empty() => {
                last_err = Some(JournalError::InvalidEntry(
                    "empty session meta sidecar".into(),
                ));
            }
            Ok(bytes) => match serde_json::from_slice::<SessionMeta>(&bytes) {
                Ok(meta) => return Ok(meta),
                Err(e) => last_err = Some(e.into()),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // hard_link succeeded then the file vanished — extraordinary,
                // but retry rather than treat as fatal.
                last_err = Some(e.into());
            }
            Err(e) => return Err(e.into()),
        }
    }
    Err(last_err.unwrap_or_else(|| {
        JournalError::InvalidEntry("session meta unreadable after retries".into())
    }))
}

// ---- Validation helpers ----

fn is_valid_hash(s: &str) -> bool {
    s.len() == 8
        && s.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// `YYYYMMDD-hex8-HHMMSS` exactly. Strict by regex; defends against path
/// injection in session IDs that flow into filesystem and git ref paths.
fn is_valid_session_id(s: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^\d{8}-[0-9a-f]{8}-\d{6}$").unwrap());
    re.is_match(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn fixed_ts() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, 27, 12, 34, 56).unwrap()
    }

    #[test]
    fn session_id_format() {
        let id = SessionId::new(fixed_ts(), "abcd1234").unwrap();
        assert_eq!(id.as_str(), "20260427-abcd1234-123456");
        assert_eq!(id.date_part(), "20260427");
        assert_eq!(id.year(), "2026");
        assert_eq!(id.month(), "04");
        assert_eq!(id.day(), "27");
        assert_eq!(id.worktree_hash(), "abcd1234");
        assert_eq!(id.time_part(), "123456");
    }

    #[test]
    fn archive_ref_path_uses_date_hierarchy() {
        let id = SessionId::new(fixed_ts(), "abcd1234").unwrap();
        assert_eq!(
            id.archive_ref_path(),
            "refs/tempyr/journals/archive/2026/04/27/20260427-abcd1234-123456"
        );
    }

    #[test]
    fn parse_round_trips() {
        let raw = "20260427-abcd1234-123456";
        let id = SessionId::parse(raw).unwrap();
        assert_eq!(id.as_str(), raw);
    }

    #[test]
    fn parse_rejects_path_traversal() {
        for bad in [
            "../../../etc/passwd",
            "20260427/abcd1234/123456",
            "20260427-abcd1234-12345/",  // wrong char
            "20260427-abcd1234-12345.",  // wrong char
            "20260427-abcd123g-123456",  // non-hex char
            "20260427-ABCD1234-123456",  // uppercase rejected
            "2026042-abcd1234-123456",   // wrong date length
            "20260427-abcd1234-1234567", // wrong time length
            "20260427--abcd1234-123456", // double sep
            "",
            "20260427-abcd1234", // missing time
        ] {
            assert!(
                SessionId::parse(bad).is_err(),
                "should have rejected: {bad}"
            );
        }
    }

    #[test]
    fn new_rejects_bad_hash() {
        assert!(SessionId::new(fixed_ts(), "ABCD1234").is_err());
        assert!(SessionId::new(fixed_ts(), "abcd123").is_err()); // 7 chars
        assert!(SessionId::new(fixed_ts(), "abcd12345").is_err()); // 9 chars
        assert!(SessionId::new(fixed_ts(), "abcdxyzz").is_err()); // non-hex
    }

    #[test]
    fn open_creates_layout_and_meta() {
        let common = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();

        let session =
            Session::open_at(common.path(), worktree.path(), "claude", fixed_ts()).unwrap();

        // Layout exists.
        assert!(jpath::open_dir(common.path()).exists());

        // Meta file exists and round-trips.
        let meta_bytes = std::fs::read(session.meta_path()).unwrap();
        let parsed: SessionMeta = serde_json::from_slice(&meta_bytes).unwrap();
        assert_eq!(parsed.session_id, *session.id());
        assert_eq!(parsed.agent, "claude");
        assert_eq!(parsed.schema_version, SCHEMA_VERSION);
    }

    #[test]
    fn open_does_not_create_jsonl_yet() {
        // The writer creates the JSONL on first append; opening a session
        // must not pre-create it.
        let common = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        let session =
            Session::open_at(common.path(), worktree.path(), "claude", fixed_ts()).unwrap();
        assert!(!session.jsonl_path().exists());
    }

    #[test]
    fn finalize_creates_ready_marker() {
        let common = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        let session =
            Session::open_at(common.path(), worktree.path(), "claude", fixed_ts()).unwrap();
        assert!(!session.is_ready());
        session.finalize().unwrap();
        assert!(session.is_ready());
        // Idempotent.
        session.finalize().unwrap();
        assert!(session.is_ready());
    }

    #[test]
    fn resume_finds_existing_session() {
        let common = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        let session =
            Session::open_at(common.path(), worktree.path(), "claude", fixed_ts()).unwrap();
        let resumed = Session::resume(common.path(), session.id())
            .unwrap()
            .unwrap();
        assert_eq!(resumed.id(), session.id());
        assert_eq!(resumed.meta().agent, "claude");
    }

    #[test]
    fn resume_returns_none_for_missing() {
        let common = tempfile::tempdir().unwrap();
        let id = SessionId::parse("20260427-abcd1234-123456").unwrap();
        assert!(Session::resume(common.path(), &id).unwrap().is_none());
    }

    #[test]
    fn open_at_same_agent_reuses_persisted_meta() {
        // Same worktree + same ts + same agent → reuse the existing session
        // and don't clobber disk.
        let common = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        let s1 = Session::open_at(common.path(), worktree.path(), "claude", fixed_ts()).unwrap();
        let original_bytes = std::fs::read(s1.meta_path()).unwrap();
        let s2 = Session::open_at(common.path(), worktree.path(), "claude", fixed_ts()).unwrap();
        let after_bytes = std::fs::read(s1.meta_path()).unwrap();
        assert_eq!(original_bytes, after_bytes);
        assert_eq!(s2.id(), s1.id());
        assert_eq!(s2.meta().agent, "claude");
    }

    #[test]
    fn open_at_different_agent_errors_with_agent_mismatch() {
        // Two agents in the same wall-clock second on the same worktree
        // produce the same session id. The second caller must not silently
        // get the first agent's session — it gets AgentMismatch instead, so
        // `open()` (which retries on Now()) can advance the clock and try
        // again. open_at is the strict form.
        let common = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        let _claude =
            Session::open_at(common.path(), worktree.path(), "claude", fixed_ts()).unwrap();
        let err =
            Session::open_at(common.path(), worktree.path(), "codex", fixed_ts()).unwrap_err();
        match err {
            JournalError::AgentMismatch {
                existing,
                requested,
            } => {
                assert_eq!(existing, "claude");
                assert_eq!(requested, "codex");
            }
            other => panic!("expected AgentMismatch, got {other:?}"),
        }
    }

    #[test]
    fn open_advances_past_same_second_collision_with_other_agent() {
        // Live `open()` (uses Now()) should retry past an existing same-id
        // session belonging to a different agent. The two ids must differ.
        let common = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        // Seed: claude takes the current-second slot.
        let claude_now = Utc::now();
        let claude =
            Session::open_at(common.path(), worktree.path(), "claude", claude_now).unwrap();
        // codex calls live open() — this may retry-and-spin briefly until
        // the clock advances to a new second.
        let codex = Session::open(common.path(), worktree.path(), "codex").unwrap();
        assert_ne!(codex.id(), claude.id());
        assert_eq!(codex.meta().agent, "codex");
        assert_eq!(claude.meta().agent, "claude");
    }

    #[test]
    fn find_active_returns_none_for_empty_journals() {
        let common = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        let found = Session::find_active(common.path(), worktree.path(), "claude").unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn find_active_returns_existing_session_for_same_worktree_and_agent() {
        let common = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        let s = Session::open_at(common.path(), worktree.path(), "claude", fixed_ts()).unwrap();

        let found = Session::find_active(common.path(), worktree.path(), "claude")
            .unwrap()
            .unwrap();
        assert_eq!(found.id(), s.id());
    }

    #[test]
    fn find_active_filters_by_agent() {
        let common = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        let _claude =
            Session::open_at(common.path(), worktree.path(), "claude", fixed_ts()).unwrap();
        // Different agent => no match for this worktree.
        let found = Session::find_active(common.path(), worktree.path(), "codex").unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn find_active_skips_finalized_sessions() {
        let common = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        let s = Session::open_at(common.path(), worktree.path(), "claude", fixed_ts()).unwrap();
        s.finalize().unwrap();
        // .ready marker present => not active anymore.
        let found = Session::find_active(common.path(), worktree.path(), "claude").unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn find_active_returns_newest_when_multiple_open() {
        use chrono::TimeZone;
        let common = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        let earlier = Utc.with_ymd_and_hms(2026, 4, 27, 10, 0, 0).unwrap();
        let later = Utc.with_ymd_and_hms(2026, 4, 27, 12, 0, 0).unwrap();
        let _old = Session::open_at(common.path(), worktree.path(), "claude", earlier).unwrap();
        let new = Session::open_at(common.path(), worktree.path(), "claude", later).unwrap();
        let found = Session::find_active(common.path(), worktree.path(), "claude")
            .unwrap()
            .unwrap();
        assert_eq!(found.id(), new.id());
    }

    #[test]
    fn open_or_resume_reuses_active_session() {
        let common = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        let s1 = Session::open_at(common.path(), worktree.path(), "claude", fixed_ts()).unwrap();
        let s2 = Session::open_or_resume(common.path(), worktree.path(), "claude").unwrap();
        assert_eq!(s1.id(), s2.id());
    }

    #[test]
    fn open_or_resume_opens_fresh_when_none_active() {
        let common = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        let s = Session::open_or_resume(common.path(), worktree.path(), "claude").unwrap();
        assert!(s.meta_path().exists());
    }

    #[test]
    fn resume_rejects_mismatched_session_id() {
        // Build a session, then write its meta under a *different* session
        // id's path. resume() should refuse to return that mismatched meta.
        let common = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        let s = Session::open_at(common.path(), worktree.path(), "claude", fixed_ts()).unwrap();
        let bytes = std::fs::read(s.meta_path()).unwrap();

        let other_id = SessionId::parse("20260101-deadbeef-000000").unwrap();
        let other_path = jpath::session_meta_path(common.path(), other_id.as_str());
        std::fs::write(&other_path, bytes).unwrap();

        let err = Session::resume(common.path(), &other_id).unwrap_err();
        assert!(matches!(err, JournalError::InvalidEntry(msg) if msg.contains("mismatch")));
    }
}
