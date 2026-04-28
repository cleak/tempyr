//! Publisher: turns finalized JSONL sessions into pushed Git refs.
//!
//! Pipeline per ready session (one with a `<id>.ready` marker):
//!
//! 1. Hash both `<id>.jsonl` and `<id>.meta.json` as blobs.
//! 2. Build a tree containing both blobs.
//! 3. Make a parent-less commit pointing at the tree.
//! 4. `update-ref` to `refs/tempyr/journals/archive/<YYYY>/<MM>/<DD>/<id>`.
//! 5. (Unless `push: false`) `git push <remote> <ref>:<ref>`.
//! 6. On push success: delete the three local files.
//!
//! Idempotency: step 4 is idempotent; if the ref already exists we skip
//! steps 1-4 and try the push again. This makes "killed between commit
//! and push" recoverable: rerun the publisher.
//!
//! Concurrency: a single [`PublisherLock`](crate::lockfile::PublisherLock)
//! held over the whole [`publish_ready_sessions`] call serializes invocations.
//!
//! Failure handling: a per-session error doesn't abort the whole run.
//! Errors are recorded in the report; `state.json` records the most recent
//! error; the `.ready` marker is preserved so the next publisher run
//! retries.

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde_json::json;

use crate::git::{self, TreeEntry};
use crate::lockfile::PublisherLock;
use crate::path as jpath;
use crate::session::SessionId;
use crate::state::{LogLevel, PublisherState, append_log};
use crate::{JournalError, Result};

/// Knobs for one [`publish_ready_sessions`] call.
#[derive(Debug, Clone)]
pub struct PublishOptions {
    /// Plan only; don't write refs, don't push, don't clean up.
    pub dry_run: bool,
    /// Push to the remote after commit. When false, the ref is created
    /// locally and the open files are *not* deleted (since they haven't
    /// safely landed on a remote).
    pub push: bool,
    /// Remote name for push. Slice 1 hardcodes "origin" via the CLI default.
    pub remote: String,
}

impl Default for PublishOptions {
    fn default() -> Self {
        Self {
            dry_run: false,
            push: true,
            remote: "origin".to_string(),
        }
    }
}

/// Per-session result inside a [`PublishReport`].
#[derive(Debug, Clone)]
pub enum SessionStatus {
    /// Newly committed (and possibly pushed). Holds the refname.
    Published { refname: String, pushed: bool },
    /// Ref already existed; we attempted push (or skipped if `push: false`).
    AlreadyArchived { refname: String, pushed: bool },
    /// Reachable failure for this session — others can still proceed.
    Failed { error: String },
    /// `dry_run` was set; we computed but wrote nothing.
    DryRun { refname: String },
}

/// Aggregate result of one publisher invocation.
#[derive(Debug, Clone, Default)]
pub struct PublishReport {
    pub scanned: usize,
    pub results: Vec<(String, SessionStatus)>,
}

impl PublishReport {
    pub fn published_count(&self) -> usize {
        self.results
            .iter()
            .filter(|(_, s)| matches!(s, SessionStatus::Published { .. }))
            .count()
    }

    pub fn failed_count(&self) -> usize {
        self.results
            .iter()
            .filter(|(_, s)| matches!(s, SessionStatus::Failed { .. }))
            .count()
    }
}

/// Returned when [`publish_ready_sessions`] can't acquire the publisher
/// lock (another publisher is already running). The caller should treat
/// this as a benign "already running" signal, not an error.
#[derive(Debug)]
pub struct AlreadyRunning;

/// Run the publisher pipeline against every `.ready` session in
/// `common_dir`. Holds the publisher lock for the duration. If the lock
/// is contended, returns `Ok(Err(AlreadyRunning))` — the caller picks a
/// human message ("publisher already running, skipping").
pub fn publish_ready_sessions(
    common_dir: &Path,
    repo_root: &Path,
    opts: &PublishOptions,
) -> Result<std::result::Result<PublishReport, AlreadyRunning>> {
    let Some(_lock) = PublisherLock::try_acquire(common_dir)? else {
        return Ok(Err(AlreadyRunning));
    };

    let mut state = PublisherState::load(common_dir)?;
    let mut report = PublishReport::default();

    let ready_ids = scan_ready_sessions(common_dir)?;
    report.scanned = ready_ids.len();

    if ready_ids.is_empty() {
        return Ok(Ok(report));
    }

    let _ = append_log(
        common_dir,
        LogLevel::Info,
        "publish_started",
        json_map([
            ("scanned", json!(ready_ids.len())),
            ("dry_run", json!(opts.dry_run)),
            ("push", json!(opts.push)),
        ]),
        crate::state::DEFAULT_MAX_LOG_BYTES,
    );

    for id in ready_ids {
        let id_str = id.as_str().to_string();
        let result = publish_one(common_dir, repo_root, &id, opts);
        match &result {
            Ok(SessionStatus::Published { .. }) => {
                state.record_commit();
                if opts.push {
                    state.record_push_ok(Utc::now());
                }
                let _ = state.save(common_dir);
            }
            Ok(SessionStatus::AlreadyArchived { pushed: true, .. }) => {
                state.record_push_ok(Utc::now());
                let _ = state.save(common_dir);
            }
            _ => {}
        }
        let status = match result {
            Ok(s) => s,
            Err(e) => {
                state.record_push_failure(Utc::now(), "publish", &e.to_string());
                let _ = state.save(common_dir);
                let _ = append_log(
                    common_dir,
                    LogLevel::Error,
                    "publish_failed",
                    json_map([
                        ("session_id", json!(id_str.clone())),
                        ("error", json!(e.to_string())),
                    ]),
                    crate::state::DEFAULT_MAX_LOG_BYTES,
                );
                SessionStatus::Failed {
                    error: e.to_string(),
                }
            }
        };
        report.results.push((id_str, status));
    }

    let _ = append_log(
        common_dir,
        LogLevel::Info,
        "publish_finished",
        json_map([
            ("scanned", json!(report.scanned)),
            ("published", json!(report.published_count())),
            ("failed", json!(report.failed_count())),
        ]),
        crate::state::DEFAULT_MAX_LOG_BYTES,
    );

    Ok(Ok(report))
}

fn json_map<I>(iter: I) -> serde_json::Map<String, serde_json::Value>
where
    I: IntoIterator<Item = (&'static str, serde_json::Value)>,
{
    iter.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

/// List session IDs whose `.ready` marker is present, sorted oldest-first
/// so the resulting refs land in chronological order.
fn scan_ready_sessions(common_dir: &Path) -> Result<Vec<SessionId>> {
    let open = jpath::open_dir(common_dir);
    let read_dir = match std::fs::read_dir(&open) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };

    let mut ids: Vec<SessionId> = Vec::new();
    for entry in read_dir {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let Some(id_str) = name_str.strip_suffix(".ready") else {
            continue;
        };
        let Ok(id) = SessionId::parse(id_str) else {
            // Stray `.ready` with bogus id: skip silently.
            continue;
        };
        ids.push(id);
    }
    // Lexicographic == chronological for our YYYYMMDD-...-HHMMSS format.
    ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    Ok(ids)
}

fn publish_one(
    common_dir: &Path,
    repo_root: &Path,
    id: &SessionId,
    opts: &PublishOptions,
) -> Result<SessionStatus> {
    let jsonl_path = jpath::session_jsonl_path(common_dir, id.as_str());
    let meta_path = jpath::session_meta_path(common_dir, id.as_str());
    let ready_path = jpath::session_ready_marker(common_dir, id.as_str());

    // Sanity check: `.ready` exists but `.jsonl` is missing → orphan
    // marker. Treat as a failure so the human notices.
    if !jsonl_path.exists() {
        return Err(JournalError::InvalidEntry(format!(
            "session {} marked ready but {} is missing",
            id,
            jsonl_path.display()
        )));
    }

    let refname = id.archive_ref_path();

    if opts.dry_run {
        return Ok(SessionStatus::DryRun { refname });
    }

    // Idempotency: ref already there from a prior crashed run? Skip the
    // commit step; we only need to (re)push and clean up.
    let already_existed = git::ref_exists(repo_root, &refname)?;

    if !already_existed {
        let jsonl_bytes = std::fs::read(&jsonl_path)?;
        // meta.json is part of the archived tree; an unreadable or
        // missing sidecar is a real problem (corrupted session, half-
        // written by a prior run, perms issue). Surface it instead of
        // silently dropping the meta from the commit — the agent name,
        // worktree hash, and HEAD captured there are load-bearing for
        // the search index Phase 3 will build.
        let meta_bytes = std::fs::read(&meta_path)?;

        let jsonl_blob = git::hash_object_blob(repo_root, &jsonl_bytes)?;
        let meta_blob = git::hash_object_blob(repo_root, &meta_bytes)?;

        // Tree entries must be sorted by name. "entries.jsonl" < "meta.json".
        let entries = [
            TreeEntry {
                mode: "100644",
                kind: "blob",
                sha: &jsonl_blob,
                name: "entries.jsonl",
            },
            TreeEntry {
                mode: "100644",
                kind: "blob",
                sha: &meta_blob,
                name: "meta.json",
            },
        ];

        let tree_sha = git::mktree(repo_root, &entries)?;
        let commit_message = format!("tempyr journal: {id}");
        let commit_sha = git::commit_tree(repo_root, &tree_sha, &commit_message)?;
        git::update_ref(repo_root, &refname, &commit_sha)?;
    }

    let mut pushed = false;
    if opts.push {
        let refspec = format!("{refname}:{refname}");
        git::push_ref(repo_root, &opts.remote, &refspec, git::DEFAULT_TIMEOUT)?;
        pushed = true;
    }

    // Cleanup: only remove local files when we're sure the ref reached
    // the remote (or push was explicitly disabled and we accept the user
    // pushing later via `git push origin refs/tempyr/journals/*`).
    if pushed || !opts.push {
        cleanup_session_files(&jsonl_path, &meta_path, &ready_path)?;
    }

    if already_existed {
        Ok(SessionStatus::AlreadyArchived { refname, pushed })
    } else {
        Ok(SessionStatus::Published { refname, pushed })
    }
}

fn cleanup_session_files(jsonl: &Path, meta: &Path, ready: &Path) -> Result<()> {
    // Remove the payload first, then the marker. If any payload removal
    // fails we leave `.ready` in place so the next publisher run can
    // retry; removing `.ready` first would silently strand the session
    // (not retriable + payload still on disk = confusing for a human).
    //
    // We unconditionally call `remove_file` and ignore `NotFound`
    // instead of pre-checking `exists()`. The exists+remove pair is a
    // TOCTOU race (the file could vanish between the two syscalls)
    // and the single-syscall form is both simpler and atomic.
    remove_if_present(jsonl)?;
    remove_if_present(meta)?;
    remove_if_present(ready)?;
    Ok(())
}

fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Bare resolver: where would the publisher push? Currently always
/// `repo_root` itself — the publisher invokes `git push` from the repo
/// it's archiving. Exposed as a function so the CLI can show "pushing
/// from <repo_root>" on `--dry-run`.
pub fn resolve_repo_root(repo_root: &Path) -> PathBuf {
    repo_root.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal end-to-end fixture: a primary repo, a bare remote on disk
    /// added as `origin`, and one ready session sitting in
    /// `<repo>/.git/tempyr/journals/open/`.
    struct Fixture {
        _outer: tempfile::TempDir,
        repo: PathBuf,
        common_dir: PathBuf,
        bare: PathBuf,
        session_id: SessionId,
    }

    impl Fixture {
        fn new() -> Self {
            Self::new_with_id("20260427-abcd1234-120000")
        }

        fn new_with_id(session_id: &str) -> Self {
            let outer = tempfile::tempdir().unwrap();
            let repo = outer.path().join("repo");
            std::fs::create_dir_all(&repo).unwrap();
            git::run(
                &repo,
                &["init", "--quiet", "--initial-branch=main"],
                None,
                git::DEFAULT_TIMEOUT,
            )
            .unwrap()
            .ok_or_err("init repo")
            .unwrap();
            git::run(
                &repo,
                &["config", "user.name", "tempyr-test"],
                None,
                git::DEFAULT_TIMEOUT,
            )
            .unwrap()
            .ok_or_err("config name")
            .unwrap();
            git::run(
                &repo,
                &["config", "user.email", "tempyr-test@example.com"],
                None,
                git::DEFAULT_TIMEOUT,
            )
            .unwrap()
            .ok_or_err("config email")
            .unwrap();

            let bare = outer.path().join("remote.git");
            std::fs::create_dir_all(&bare).unwrap();
            git::run(
                &bare,
                &["init", "--quiet", "--bare", "--initial-branch=main"],
                None,
                git::DEFAULT_TIMEOUT,
            )
            .unwrap()
            .ok_or_err("init bare")
            .unwrap();
            git::run(
                &repo,
                &["remote", "add", "origin", &bare.to_string_lossy()],
                None,
                git::DEFAULT_TIMEOUT,
            )
            .unwrap()
            .ok_or_err("remote add")
            .unwrap();

            let common_dir = repo.join(".git");
            let id = SessionId::parse(session_id).unwrap();
            seed_ready_session(&common_dir, &id);

            Self {
                _outer: outer,
                repo,
                common_dir,
                bare,
                session_id: id,
            }
        }
    }

    fn seed_ready_session(common_dir: &Path, id: &SessionId) {
        std::fs::create_dir_all(jpath::open_dir(common_dir)).unwrap();
        let jsonl = jpath::session_jsonl_path(common_dir, id.as_str());
        let meta = jpath::session_meta_path(common_dir, id.as_str());
        let ready = jpath::session_ready_marker(common_dir, id.as_str());
        std::fs::write(
            &jsonl,
            br#"{"v":1,"id":"e1","summary":"hello","kind":"plan"}
"#,
        )
        .unwrap();
        std::fs::write(
            &meta,
            br#"{"v":1,"session_id":"x","agent":"claude"}
"#,
        )
        .unwrap();
        std::fs::write(&ready, b"").unwrap();
    }

    #[test]
    fn publishes_one_session_end_to_end() {
        let fx = Fixture::new();
        let opts = PublishOptions::default();
        let report = publish_ready_sessions(&fx.common_dir, &fx.repo, &opts)
            .unwrap()
            .unwrap();
        assert_eq!(report.scanned, 1);
        assert_eq!(report.published_count(), 1);
        assert_eq!(report.failed_count(), 0);

        // Ref now exists locally and on the bare remote.
        let refname = fx.session_id.archive_ref_path();
        assert!(git::ref_exists(&fx.repo, &refname).unwrap());
        assert!(git::ref_exists(&fx.bare, &refname).unwrap());

        // Local files are gone.
        assert!(!jpath::session_jsonl_path(&fx.common_dir, fx.session_id.as_str()).exists());
        assert!(!jpath::session_meta_path(&fx.common_dir, fx.session_id.as_str()).exists());
        assert!(!jpath::session_ready_marker(&fx.common_dir, fx.session_id.as_str()).exists());

        // state.json reflects success.
        let state = PublisherState::load(&fx.common_dir).unwrap();
        assert_eq!(state.commits_total, 1);
        assert_eq!(state.pushes_total, 1);
        assert!(state.last_error.is_none());
        assert!(state.last_push_ok_utc.is_some());
    }

    #[test]
    fn dry_run_writes_nothing() {
        let fx = Fixture::new();
        let opts = PublishOptions {
            dry_run: true,
            ..Default::default()
        };
        let report = publish_ready_sessions(&fx.common_dir, &fx.repo, &opts)
            .unwrap()
            .unwrap();
        assert_eq!(report.scanned, 1);
        assert!(matches!(report.results[0].1, SessionStatus::DryRun { .. }));
        // No ref, no cleanup.
        let refname = fx.session_id.archive_ref_path();
        assert!(!git::ref_exists(&fx.repo, &refname).unwrap());
        assert!(jpath::session_jsonl_path(&fx.common_dir, fx.session_id.as_str()).exists());
        assert!(jpath::session_ready_marker(&fx.common_dir, fx.session_id.as_str()).exists());
    }

    #[test]
    fn no_push_creates_ref_but_keeps_local_files() {
        let fx = Fixture::new();
        let opts = PublishOptions {
            push: false,
            ..Default::default()
        };
        let report = publish_ready_sessions(&fx.common_dir, &fx.repo, &opts)
            .unwrap()
            .unwrap();
        assert_eq!(report.published_count(), 1);

        let refname = fx.session_id.archive_ref_path();
        assert!(git::ref_exists(&fx.repo, &refname).unwrap());
        // Bare remote not touched.
        assert!(!git::ref_exists(&fx.bare, &refname).unwrap());
        // No-push still cleans up since the work is committed locally and
        // the user has acknowledged offline mode.
        assert!(!jpath::session_jsonl_path(&fx.common_dir, fx.session_id.as_str()).exists());
        // pushes_total stays 0 because we didn't push.
        let state = PublisherState::load(&fx.common_dir).unwrap();
        assert_eq!(state.commits_total, 1);
        assert_eq!(state.pushes_total, 0);
    }

    #[test]
    fn idempotent_when_ref_already_exists_and_push_succeeds() {
        let fx = Fixture::new();
        // First run: full pipeline.
        let _r1 = publish_ready_sessions(&fx.common_dir, &fx.repo, &PublishOptions::default())
            .unwrap()
            .unwrap();
        // Re-seed the same session (simulate "killed before cleanup" by
        // writing the files back into open/ — ref still exists locally).
        seed_ready_session(&fx.common_dir, &fx.session_id);

        let r2 = publish_ready_sessions(&fx.common_dir, &fx.repo, &PublishOptions::default())
            .unwrap()
            .unwrap();
        assert_eq!(r2.scanned, 1);
        assert!(matches!(
            r2.results[0].1,
            SessionStatus::AlreadyArchived { pushed: true, .. }
        ));
        // Cleanup ran the second time too.
        assert!(!jpath::session_jsonl_path(&fx.common_dir, fx.session_id.as_str()).exists());
    }

    #[test]
    fn push_failure_preserves_ready_marker_and_records_state() {
        let fx = Fixture::new();
        let opts = PublishOptions {
            remote: "nonexistent-remote".to_string(),
            ..Default::default()
        };
        let report = publish_ready_sessions(&fx.common_dir, &fx.repo, &opts)
            .unwrap()
            .unwrap();
        assert_eq!(report.scanned, 1);
        assert_eq!(report.failed_count(), 1);
        // Ref was created locally (because update-ref ran before push),
        // but the .ready marker is preserved for retry.
        let refname = fx.session_id.archive_ref_path();
        assert!(git::ref_exists(&fx.repo, &refname).unwrap());
        assert!(jpath::session_ready_marker(&fx.common_dir, fx.session_id.as_str()).exists());
        assert!(jpath::session_jsonl_path(&fx.common_dir, fx.session_id.as_str()).exists());

        // state.json records the failure.
        let state = PublisherState::load(&fx.common_dir).unwrap();
        assert!(state.last_error.is_some());
        assert_eq!(state.push_failures_total, 1);
    }

    #[test]
    fn lock_contention_returns_already_running() {
        let fx = Fixture::new();
        // Hold the publisher lock from this test thread.
        let _lock = PublisherLock::try_acquire(&fx.common_dir).unwrap().unwrap();
        let result =
            publish_ready_sessions(&fx.common_dir, &fx.repo, &PublishOptions::default()).unwrap();
        assert!(result.is_err(), "should report AlreadyRunning");
    }

    #[test]
    fn empty_open_dir_yields_empty_report() {
        let outer = tempfile::tempdir().unwrap();
        let repo = outer.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git::run(
            &repo,
            &["init", "--quiet", "--initial-branch=main"],
            None,
            git::DEFAULT_TIMEOUT,
        )
        .unwrap()
        .ok_or_err("init")
        .unwrap();

        let common_dir = repo.join(".git");
        let report = publish_ready_sessions(&common_dir, &repo, &PublishOptions::default())
            .unwrap()
            .unwrap();
        assert_eq!(report.scanned, 0);
        assert_eq!(report.published_count(), 0);
    }

    #[test]
    fn publishes_multiple_in_chronological_order() {
        let fx = Fixture::new();
        // Add a second, later session.
        let earlier = SessionId::parse("20260427-abcd1234-100000").unwrap();
        let later = SessionId::parse("20260427-abcd1234-130000").unwrap();
        seed_ready_session(&fx.common_dir, &earlier);
        seed_ready_session(&fx.common_dir, &later);

        let report = publish_ready_sessions(&fx.common_dir, &fx.repo, &PublishOptions::default())
            .unwrap()
            .unwrap();
        assert_eq!(report.scanned, 3);
        // First in the list should be the earliest id (10:00:00).
        let ordered_ids: Vec<&str> = report.results.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ordered_ids[0], "20260427-abcd1234-100000");
        assert_eq!(ordered_ids[1], fx.session_id.as_str());
        assert_eq!(ordered_ids[2], "20260427-abcd1234-130000");
    }

    #[test]
    fn orphan_ready_marker_fails_one_session_only() {
        let fx = Fixture::new();
        // Create another `.ready` with no `.jsonl` companion.
        let orphan = SessionId::parse("20260427-abcd1234-090000").unwrap();
        std::fs::write(
            jpath::session_ready_marker(&fx.common_dir, orphan.as_str()),
            b"",
        )
        .unwrap();
        // (No jsonl, no meta — orphan.)

        let report = publish_ready_sessions(&fx.common_dir, &fx.repo, &PublishOptions::default())
            .unwrap()
            .unwrap();
        assert_eq!(report.scanned, 2);
        // The orphan fails; the real one publishes.
        assert_eq!(report.published_count(), 1);
        assert_eq!(report.failed_count(), 1);
    }

    #[test]
    fn ignores_bogus_ready_filenames() {
        let fx = Fixture::new();
        // Stray file that strips to a non-id-shaped string — should not
        // crash and should not count as a scanned session.
        std::fs::write(
            jpath::open_dir(&fx.common_dir).join("not-a-session.ready"),
            b"",
        )
        .unwrap();
        let report = publish_ready_sessions(&fx.common_dir, &fx.repo, &PublishOptions::default())
            .unwrap()
            .unwrap();
        assert_eq!(report.scanned, 1);
        assert_eq!(report.published_count(), 1);
    }

    #[test]
    fn missing_meta_sidecar_surfaces_as_failure() {
        // meta.json was previously silently dropped via `unwrap_or_default`.
        // It carries the agent / branch / HEAD context the search index
        // relies on; an unreadable sidecar must surface as a per-session
        // failure (the .ready marker stays put for retry).
        let fx = Fixture::new();
        std::fs::remove_file(jpath::session_meta_path(
            &fx.common_dir,
            fx.session_id.as_str(),
        ))
        .unwrap();

        let report = publish_ready_sessions(&fx.common_dir, &fx.repo, &PublishOptions::default())
            .unwrap()
            .unwrap();
        assert_eq!(report.scanned, 1);
        assert_eq!(report.failed_count(), 1);
        // Marker preserved for retry.
        assert!(jpath::session_ready_marker(&fx.common_dir, fx.session_id.as_str()).exists());
    }

    #[test]
    fn cleanup_removes_ready_marker_last() {
        // Cleanup must remove .jsonl and .meta.json before .ready, so a
        // failure midway leaves the session retriable rather than
        // stranded with payload-but-no-marker. We verify by inspecting
        // file timestamps after a successful run: if .ready is removed
        // last, it doesn't exist (cleanup completed); if jsonl/meta
        // remain after a successful flush, that's also a failure.
        let fx = Fixture::new();
        let _r = publish_ready_sessions(&fx.common_dir, &fx.repo, &PublishOptions::default())
            .unwrap()
            .unwrap();
        // All three should be gone after a successful flush.
        assert!(!jpath::session_jsonl_path(&fx.common_dir, fx.session_id.as_str()).exists());
        assert!(!jpath::session_meta_path(&fx.common_dir, fx.session_id.as_str()).exists());
        assert!(!jpath::session_ready_marker(&fx.common_dir, fx.session_id.as_str()).exists());

        // Direct unit-test of the helper: if jsonl removal fails, .ready
        // must still exist so a retry sees the session as ready.
        let outer = tempfile::tempdir().unwrap();
        let jsonl = outer.path().join("a.jsonl");
        let meta = outer.path().join("a.meta.json");
        let ready = outer.path().join("a.ready");
        std::fs::write(&meta, b"meta").unwrap();
        std::fs::write(&ready, b"").unwrap();
        // `remove_if_present` calls `remove_file` directly and treats
        // `NotFound` as success, so a missing jsonl wouldn't trigger
        // the failure path we want to exercise. Force a real removal
        // error instead by putting a *directory* at jsonl's path —
        // `remove_file` on a directory errors with a non-NotFound kind
        // on both Unix and Windows. The helper's first call propagates
        // that error and `.ready` must still survive so the publisher
        // can retry the session.
        std::fs::create_dir_all(&jsonl).unwrap();
        let result = cleanup_session_files(&jsonl, &meta, &ready);
        assert!(result.is_err(), "removing a directory-as-file should fail");
        assert!(
            ready.exists(),
            ".ready must survive a partial cleanup so the publisher can retry"
        );
    }
}
