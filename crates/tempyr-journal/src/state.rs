//! Sticky publisher state and rotating event log.
//!
//! Two artifacts under `<git-common-dir>/tempyr/journals/`:
//!
//! - `state.json` — the **last successful push timestamp**, **last error**,
//!   and counters. Read by `tempyr journal status`/`doctor` and updated by
//!   the publisher on every operation. Single small file; rewritten atomically
//!   via temp-and-rename.
//! - `publisher.log` — append-only structured event log. One JSON object per
//!   line. Rotated when it exceeds `max_bytes` (default 5 MB) by renaming to
//!   `publisher.log.1`, dropping any prior `.1`.
//!
//! These are the only operational surfaces the publisher exposes outside its
//! own process, so callers (status/doctor commands, hook scripts) can inspect
//! health without IPC.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::Result;
use crate::path as jpath;

/// Sticky state persisted in `<journals>/state.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PublisherState {
    /// Last successful push to the remote, if any.
    pub last_push_ok_utc: Option<DateTime<Utc>>,
    /// Most recent error (any operation: commit, push, fetch, refspec config).
    pub last_error: Option<LastError>,
    /// Total commits made to refs/tempyr/journals/* since state was created.
    pub commits_total: u64,
    /// Total successful pushes since state was created.
    pub pushes_total: u64,
    /// Total push failures (transient or permanent).
    pub push_failures_total: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LastError {
    pub ts_utc: DateTime<Utc>,
    pub op: String, // e.g. "push", "commit", "fetch"
    pub message: String,
}

impl PublisherState {
    /// Load from `state.json` in the common dir. Returns the default state if
    /// the file doesn't exist (first run case).
    pub fn load(common_dir: &Path) -> Result<Self> {
        let path = jpath::publisher_state_path(common_dir);
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = std::fs::read(&path)?;
        let state: PublisherState = serde_json::from_slice(&bytes)?;
        Ok(state)
    }

    /// Atomically write to `state.json`. Writes to a sibling tempfile and
    /// renames into place; survives partial writes / crashes.
    pub fn save(&self, common_dir: &Path) -> Result<()> {
        let path = jpath::publisher_state_path(common_dir);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(self)?;
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn record_push_ok(&mut self, ts: DateTime<Utc>) {
        self.last_push_ok_utc = Some(ts);
        self.pushes_total += 1;
        self.last_error = None;
    }

    pub fn record_push_failure(&mut self, ts: DateTime<Utc>, op: &str, message: &str) {
        self.push_failures_total += 1;
        self.last_error = Some(LastError {
            ts_utc: ts,
            op: op.to_string(),
            message: message.to_string(),
        });
    }

    pub fn record_commit(&mut self) {
        self.commits_total += 1;
    }
}

/// Severity of a publisher log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// One structured line in `publisher.log`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogLine {
    pub ts: DateTime<Utc>,
    pub level: LogLevel,
    pub event: String,
    #[serde(skip_serializing_if = "serde_json::Map::is_empty", default)]
    pub fields: serde_json::Map<String, serde_json::Value>,
}

/// Default rotation threshold (5 MB).
pub const DEFAULT_MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// Append a single structured line to `publisher.log`. Rotates first if the
/// file is at or over `max_bytes`. Failures here are non-fatal for callers —
/// the publisher should never crash because logging failed — so the function
/// returns the underlying error and lets the caller decide.
pub fn append_log(
    common_dir: &Path,
    level: LogLevel,
    event: &str,
    fields: serde_json::Map<String, serde_json::Value>,
    max_bytes: u64,
) -> Result<()> {
    let line = LogLine {
        ts: Utc::now(),
        level,
        event: event.to_string(),
        fields,
    };
    let path = jpath::publisher_log_path(common_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    rotate_if_needed(&path, max_bytes)?;

    let mut bytes = serde_json::to_vec(&line)?;
    bytes.push(b'\n');

    use std::fs::OpenOptions;
    use std::io::Write;
    let mut f = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(&path)?;
    f.lock().map_err(|e| crate::JournalError::Lock(e.to_string()))?;
    f.write_all(&bytes)?;
    f.sync_data()?;
    Ok(())
}

/// Convenience: log without extra fields.
pub fn log(common_dir: &Path, level: LogLevel, event: &str) -> Result<()> {
    append_log(common_dir, level, event, serde_json::Map::new(), DEFAULT_MAX_LOG_BYTES)
}

fn rotate_if_needed(path: &Path, max_bytes: u64) -> Result<()> {
    let size = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(_) => return Ok(()), // doesn't exist yet
    };
    if size < max_bytes {
        return Ok(());
    }
    let rotated = path.with_extension("log.1");
    // If rotated already exists, it gets dropped (we keep only one history).
    let _ = std::fs::remove_file(&rotated);
    std::fs::rename(path, &rotated)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    #[test]
    fn state_default_is_load_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let state = PublisherState::load(dir.path()).unwrap();
        assert!(state.last_push_ok_utc.is_none());
        assert!(state.last_error.is_none());
        assert_eq!(state.commits_total, 0);
    }

    #[test]
    fn state_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let common = dir.path();
        std::fs::create_dir_all(jpath::journals_root(common)).unwrap();

        let mut state = PublisherState::default();
        state.record_commit();
        state.record_commit();
        state.record_push_ok(Utc.with_ymd_and_hms(2026, 4, 27, 10, 0, 0).unwrap());
        state.record_push_failure(
            Utc.with_ymd_and_hms(2026, 4, 27, 11, 0, 0).unwrap(),
            "push",
            "non-fast-forward",
        );
        state.save(common).unwrap();

        let loaded = PublisherState::load(common).unwrap();
        assert_eq!(loaded.commits_total, 2);
        assert_eq!(loaded.pushes_total, 1);
        assert_eq!(loaded.push_failures_total, 1);
        assert!(loaded.last_error.is_some());
        assert_eq!(loaded.last_error.as_ref().unwrap().op, "push");
    }

    #[test]
    fn state_save_uses_temp_rename() {
        // After save, no .tmp file should be left behind.
        let dir = tempfile::tempdir().unwrap();
        let common = dir.path();
        std::fs::create_dir_all(jpath::journals_root(common)).unwrap();

        let state = PublisherState::default();
        state.save(common).unwrap();

        let tmp = jpath::publisher_state_path(common).with_extension("json.tmp");
        assert!(!tmp.exists());
    }

    #[test]
    fn record_push_ok_clears_last_error() {
        let mut state = PublisherState::default();
        state.record_push_failure(Utc::now(), "push", "auth failed");
        assert!(state.last_error.is_some());
        state.record_push_ok(Utc::now());
        assert!(state.last_error.is_none());
    }

    #[test]
    fn log_appends_line() {
        let dir = tempfile::tempdir().unwrap();
        let common = dir.path();
        std::fs::create_dir_all(jpath::journals_root(common)).unwrap();

        log(common, LogLevel::Info, "publisher_started").unwrap();
        log(common, LogLevel::Warn, "push_retry").unwrap();

        let path = jpath::publisher_log_path(common);
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let l0: LogLine = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(l0.level, LogLevel::Info);
        assert_eq!(l0.event, "publisher_started");
    }

    #[test]
    fn log_with_fields() {
        let dir = tempfile::tempdir().unwrap();
        let common = dir.path();
        std::fs::create_dir_all(jpath::journals_root(common)).unwrap();

        let mut fields = serde_json::Map::new();
        fields.insert("ref".into(), json!("refs/tempyr/journals/archive/2026/04/27/x"));
        fields.insert("retry_count".into(), json!(2));
        append_log(common, LogLevel::Error, "push_failed", fields, DEFAULT_MAX_LOG_BYTES).unwrap();

        let text = std::fs::read_to_string(jpath::publisher_log_path(common)).unwrap();
        let line: LogLine = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(line.event, "push_failed");
        assert_eq!(line.fields["retry_count"], 2);
    }

    #[test]
    fn log_rotates_at_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let common = dir.path();
        std::fs::create_dir_all(jpath::journals_root(common)).unwrap();

        let max = 256u64; // tiny threshold for the test
        // Write enough lines to exceed the threshold.
        for i in 0..10 {
            let mut fields = serde_json::Map::new();
            fields.insert("i".into(), json!(i));
            fields.insert("padding".into(), json!("xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"));
            append_log(common, LogLevel::Info, "tick", fields, max).unwrap();
        }

        let live = jpath::publisher_log_path(common);
        let rotated = live.with_extension("log.1");
        assert!(live.exists());
        assert!(rotated.exists(), "expected rotated log at {}", rotated.display());
    }

    #[test]
    fn log_rotation_keeps_only_one_history() {
        let dir = tempfile::tempdir().unwrap();
        let common = dir.path();
        std::fs::create_dir_all(jpath::journals_root(common)).unwrap();

        let max = 128u64;
        for i in 0..30 {
            let mut fields = serde_json::Map::new();
            fields.insert("i".into(), json!(i));
            fields.insert("padding".into(), json!("xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"));
            append_log(common, LogLevel::Info, "tick", fields, max).unwrap();
        }

        // Should have publisher.log + publisher.log.1, no .2 etc.
        let dir_iter = std::fs::read_dir(jpath::journals_root(common)).unwrap();
        let log_files: Vec<_> = dir_iter
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("publisher.log")
            })
            .collect();
        assert!(log_files.len() <= 2, "got {} log files", log_files.len());
    }
}
