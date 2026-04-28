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
    pub fn open(common_dir: &Path, worktree_top: &Path, agent: &str) -> Result<Self> {
        Self::open_at(common_dir, worktree_top, agent, Utc::now())
    }

    /// Internal constructor that takes an explicit timestamp (for tests).
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

        // Write metadata sidecar atomically. Use create_new so we don't
        // overwrite a same-id session (collision is virtually impossible
        // given second-precision ts + worktree_hash, but be safe).
        let meta_path = jpath::session_meta_path(common_dir, session_id.as_str());
        let json = serde_json::to_string_pretty(&meta)?;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&meta_path)
        {
            Ok(mut f) => {
                use std::io::Write;
                f.write_all(json.as_bytes())?;
                f.write_all(b"\n")?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Same-id collision: replace our in-memory meta with the
                // persisted one so the returned Session matches disk. The
                // existing session's agent/branch/head win.
                let bytes = std::fs::read(&meta_path)?;
                meta = serde_json::from_slice(&bytes)?;
            }
            Err(e) => return Err(e.into()),
        }

        Ok(Session {
            common_dir: common_dir.to_path_buf(),
            meta,
        })
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
    fn open_twice_with_same_ts_does_not_clobber_meta() {
        // Same worktree + same ts -> same session id. Second open must not
        // overwrite disk, and the returned in-memory meta must reflect the
        // persisted state (not the second caller's args).
        let common = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        let s1 = Session::open_at(common.path(), worktree.path(), "claude", fixed_ts()).unwrap();
        let original_bytes = std::fs::read(s1.meta_path()).unwrap();
        let s2 = Session::open_at(
            common.path(),
            worktree.path(),
            "different-agent",
            fixed_ts(),
        )
        .unwrap();
        let after_bytes = std::fs::read(s1.meta_path()).unwrap();
        assert_eq!(original_bytes, after_bytes);
        // In-memory meta of s2 must come from disk (s1's "claude"), not its
        // own caller-supplied "different-agent".
        assert_eq!(s2.meta().agent, "claude");
        assert_eq!(s2.id(), s1.id());
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
