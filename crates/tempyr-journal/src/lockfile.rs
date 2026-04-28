//! Single-publisher coordination via `<journals>/publisher.lock`.
//!
//! At most one publisher runs at a time per repo. A second invocation —
//! whether from `tempyr journal flush` or an in-process ticker — sees the
//! lock is held and exits cleanly without waiting. We use a non-blocking
//! `try_lock` so a `flush` from CI never wedges on the auto-publisher in
//! the user's IDE.
//!
//! Holding the lock = holding an open file handle. Drop releases the OS
//! lock; if the process dies, the OS reclaims the handle and the next
//! caller can take over. No stale-PID detection needed.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::path as jpath;
use crate::{JournalError, Result};

/// Held publisher lock. Drops the OS lock when this value drops.
///
/// The `_file` field is intentionally unused at the type level — the file
/// handle's lifetime *is* the lock's lifetime.
pub struct PublisherLock {
    _file: File,
    path: PathBuf,
}

impl PublisherLock {
    /// Try to acquire the publisher lock. Returns `Ok(Some(lock))` on
    /// success, `Ok(None)` if another process already holds it, or `Err`
    /// if the lockfile couldn't be opened (permissions, missing parent).
    ///
    /// Creates the lockfile (and journal directory layout) if missing.
    /// Stamps the current PID into the file as a diagnostic so `tempyr
    /// journal status` can show "currently publishing as pid N" — the PID
    /// is informational only; we never trust it for liveness decisions.
    pub fn try_acquire(common_dir: &Path) -> Result<Option<Self>> {
        jpath::ensure_layout(common_dir)?;
        let path = jpath::publisher_lock_path(common_dir);

        // `read(true)` is required for `File::try_lock` on Windows even
        // though we never read — same constraint as the JSONL writer.
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;

        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(JournalError::Lock(e.to_string()));
            }
        }

        // Lock held: stamp our PID for diagnostics. Best-effort — failure
        // here doesn't invalidate the lock.
        let _ = stamp_pid(&file);

        Ok(Some(PublisherLock { _file: file, path }))
    }

    /// Path to the lockfile (for diagnostics / error messages).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Probe whether the publisher lock is currently held by *some* process
    /// (us or another). Implemented as `try_acquire` + immediate drop:
    /// `Ok(None)` from try_acquire means contended → held; `Ok(Some(_))`
    /// means we got it → not held by anyone else, so we drop and report
    /// "not held". An error during the probe (permission, etc.) is reported
    /// as `Ok(None)` upward so `status` callers don't crash on edge cases.
    pub fn is_held(common_dir: &Path) -> bool {
        match Self::try_acquire(common_dir) {
            Ok(Some(lock)) => {
                drop(lock);
                false
            }
            Ok(None) => true,
            Err(_) => false,
        }
    }

    /// Best-effort: read the PID stamped into the lockfile. Returns
    /// `None` if the file doesn't exist, can't be read (e.g. exclusively
    /// locked on Windows), or doesn't contain a parseable u32. Use only
    /// for diagnostics — never as proof of liveness.
    pub fn stamped_pid(common_dir: &Path) -> Option<u32> {
        let path = jpath::publisher_lock_path(common_dir);
        let bytes = std::fs::read(&path).ok()?;
        let text = std::str::from_utf8(&bytes).ok()?;
        text.trim().parse::<u32>().ok()
    }
}

fn stamp_pid(mut file: &File) -> std::io::Result<()> {
    use std::io::{Seek, SeekFrom};
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    writeln!(file, "{}", std::process::id())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_acquire_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let lock = PublisherLock::try_acquire(dir.path()).unwrap();
        assert!(lock.is_some());
    }

    #[test]
    fn second_acquire_returns_none_while_first_held() {
        let dir = tempfile::tempdir().unwrap();
        let first = PublisherLock::try_acquire(dir.path()).unwrap().unwrap();
        let second = PublisherLock::try_acquire(dir.path()).unwrap();
        assert!(
            second.is_none(),
            "second acquire should not succeed while first holds the lock"
        );
        drop(first);
    }

    #[test]
    fn release_lets_next_caller_acquire() {
        let dir = tempfile::tempdir().unwrap();
        {
            let _lock = PublisherLock::try_acquire(dir.path()).unwrap().unwrap();
        }
        // After drop, a new caller should be able to acquire.
        let next = PublisherLock::try_acquire(dir.path()).unwrap();
        assert!(next.is_some());
    }

    #[test]
    fn pid_is_stamped() {
        // On Windows the exclusive lock prevents *reading* the file from a
        // second handle, so we drop the lock before inspecting content.
        // This verifies the diagnostic stamp happened, not the lock.
        let dir = tempfile::tempdir().unwrap();
        let lock_path = jpath::publisher_lock_path(dir.path());
        {
            let _lock = PublisherLock::try_acquire(dir.path()).unwrap().unwrap();
        }
        let bytes = std::fs::read(&lock_path).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        let pid: u32 = text.trim().parse().expect("lockfile should contain a pid");
        assert_eq!(pid, std::process::id());
    }

    #[test]
    fn is_held_reports_true_while_other_holds_lock() {
        let dir = tempfile::tempdir().unwrap();
        let _held = PublisherLock::try_acquire(dir.path()).unwrap().unwrap();
        assert!(PublisherLock::is_held(dir.path()));
    }

    #[test]
    fn is_held_reports_false_when_unheld() {
        let dir = tempfile::tempdir().unwrap();
        // No prior holder → probe gets the lock and immediately drops.
        assert!(!PublisherLock::is_held(dir.path()));
        // After the probe drops the lock, a caller can still acquire.
        assert!(PublisherLock::try_acquire(dir.path()).unwrap().is_some());
    }

    #[test]
    fn stamped_pid_returns_pid_after_acquire_and_release() {
        let dir = tempfile::tempdir().unwrap();
        {
            let _lock = PublisherLock::try_acquire(dir.path()).unwrap().unwrap();
        }
        // After drop, the PID should still be readable from the file.
        let pid = PublisherLock::stamped_pid(dir.path()).expect("pid should be readable");
        assert_eq!(pid, std::process::id());
    }

    #[test]
    fn stamped_pid_returns_none_when_lockfile_missing() {
        let dir = tempfile::tempdir().unwrap();
        // No journal layout exists; stamped_pid should not error and
        // should return None.
        assert!(PublisherLock::stamped_pid(dir.path()).is_none());
    }
}
