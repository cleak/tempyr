//! In-process publisher ticker for the long-running MCP server.
//!
//! When `tempyr --mcp` is hosting an agent, we want finalized sessions
//! to land on the remote without the user (or hooks) having to invoke
//! `tempyr journal flush` by hand. This module runs a small tokio task
//! that, every `interval`, calls
//! [`tempyr_journal::publish_ready_sessions`] for the project's git
//! common dir.
//!
//! Lifecycle:
//! - Spawned by `serve_stdio_with_project_root_fallback` after the
//!   project anchor has settled (so the repo root is stable).
//! - Joined to the [`ShutdownCoordinator`](crate::shutdown::ShutdownCoordinator)
//!   via its cancellation token: when shutdown fires, the loop breaks
//!   and runs **one final flush** before returning so finalized work
//!   doesn't sit on disk until the next agent invocation.
//! - Silently no-ops if the project root isn't a git repo (tempyr can
//!   be used in non-git directories; the journal subsystem doesn't
//!   apply there).
//!
//! Per-tick failures (no remote, auth issue, etc.) are absorbed: the
//! publisher records them in `state.json` + `publisher.log`, the
//! `.ready` markers stay put for retry, and the ticker continues.
//! We never want a transient git failure to bring down the agent.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tempyr_journal::{PublishOptions, path as jpath, publish_ready_sessions};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Default flush cadence when no override is provided. Conservative —
/// agent sessions are bursty (many entries in a few seconds), so we
/// don't need to be aggressive. The cost of a tick is one `read_dir`
/// + a no-op git check when there's nothing ready.
const DEFAULT_INTERVAL_SECS: u64 = 60;

/// Read the tick interval from the env var, falling back to the
/// [`DEFAULT_INTERVAL_SECS`]. Slice 3 will move this to `[journal]`
/// config; slice 2 keeps it as a low-friction override.
pub fn interval_from_env() -> Duration {
    std::env::var("TEMPYR_JOURNAL_TICK_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(DEFAULT_INTERVAL_SECS))
}

/// What the ticker resolved at startup. Returned for diagnostics so
/// the caller (the MCP entrypoint) can log whether the ticker actually
/// ran or skipped.
#[derive(Debug)]
pub enum SpawnOutcome {
    /// Background task spawned; holds its join handle.
    Running {
        handle: JoinHandle<()>,
        common_dir: PathBuf,
        repo_root: PathBuf,
        interval: Duration,
    },
    /// Project root isn't a git repo — nothing to ticker over.
    NotAGitRepo,
    /// Resolution failed for some other reason; we treat it as benign
    /// and skip rather than crashing the agent.
    Unavailable(String),
}

/// Spawn the ticker. `project_root` is the directory the MCP server
/// anchored to (typically the user's project). On cancellation the
/// loop breaks and one final flush runs before the task returns.
pub fn spawn(project_root: &Path, cancellation_token: CancellationToken) -> SpawnOutcome {
    spawn_with_interval(project_root, interval_from_env(), cancellation_token)
}

/// Same as [`spawn`] but with an explicit interval. Used by tests to
/// avoid racing on the env-var override (tests in the same process
/// share env state).
pub fn spawn_with_interval(
    project_root: &Path,
    interval: Duration,
    cancellation_token: CancellationToken,
) -> SpawnOutcome {
    let common_dir = match jpath::git_common_dir(project_root) {
        Ok(d) => d,
        Err(_) => return SpawnOutcome::NotAGitRepo,
    };
    let repo_root = match jpath::repo_toplevel(project_root) {
        Ok(d) => d,
        Err(e) => return SpawnOutcome::Unavailable(format!("repo top-level: {e}")),
    };

    let common_dir_for_task = common_dir.clone();
    let repo_root_for_task = repo_root.clone();
    let handle = tokio::spawn(async move {
        run_loop(
            common_dir_for_task,
            repo_root_for_task,
            interval,
            cancellation_token,
        )
        .await;
    });
    SpawnOutcome::Running {
        handle,
        common_dir,
        repo_root,
        interval,
    }
}

async fn run_loop(
    common_dir: PathBuf,
    repo_root: PathBuf,
    interval: Duration,
    cancellation_token: CancellationToken,
) {
    loop {
        tokio::select! {
            biased;
            _ = cancellation_token.cancelled() => break,
            _ = tokio::time::sleep(interval) => {
                run_one_flush(&common_dir, &repo_root).await;
            }
        }
    }

    // Shutdown: one final flush so .ready sessions don't strand on
    // disk until the next agent invocation. This runs even if the
    // last scheduled tick fired moments ago — empty case is cheap
    // (publisher returns Ok with scanned == 0).
    run_one_flush(&common_dir, &repo_root).await;
}

async fn run_one_flush(common_dir: &Path, repo_root: &Path) {
    let common_dir = common_dir.to_path_buf();
    let repo_root = repo_root.to_path_buf();
    // The publisher pipeline shells out to git, which is blocking.
    // Hand it to spawn_blocking so we don't stall the tokio runtime.
    let _ = tokio::task::spawn_blocking(move || {
        let opts = PublishOptions::default();
        // We don't surface the result here — failures are persisted in
        // state.json + publisher.log by the publisher itself, so the
        // status/logs CLIs and external readers can see them.
        let _ = publish_ready_sessions(&common_dir, &repo_root, &opts);
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Duration;
    use tempyr_journal::{PublisherState, SessionId, path as jpath};

    /// Serialize tests that mutate `TEMPYR_JOURNAL_TICK_SECS`. Without
    /// this, parallel tests would clobber each other's env state and
    /// the assertions would race.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn init_repo_with_remote() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
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
                .expect("git command");
        }
        std::process::Command::new("git")
            .args(["remote", "add", "origin", &bare.to_string_lossy()])
            .current_dir(&repo)
            .output()
            .expect("remote add");
        let common = repo.join(".git");
        (outer, repo, bare, common)
    }

    fn seed_ready_session(common_dir: &Path, id: &SessionId) {
        std::fs::create_dir_all(jpath::open_dir(common_dir)).unwrap();
        std::fs::write(
            jpath::session_jsonl_path(common_dir, id.as_str()),
            br#"{"v":1,"id":"e1","summary":"hi","kind":"plan"}
"#,
        )
        .unwrap();
        std::fs::write(
            jpath::session_meta_path(common_dir, id.as_str()),
            br#"{"v":1,"session_id":"x","agent":"claude"}
"#,
        )
        .unwrap();
        std::fs::write(jpath::session_ready_marker(common_dir, id.as_str()), b"").unwrap();
    }

    #[test]
    fn interval_env_override_applies() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: tests in the same process share env. Set + unset cleanly.
        unsafe {
            std::env::set_var("TEMPYR_JOURNAL_TICK_SECS", "5");
        }
        assert_eq!(interval_from_env(), Duration::from_secs(5));
        unsafe {
            std::env::remove_var("TEMPYR_JOURNAL_TICK_SECS");
        }
    }

    #[test]
    fn interval_env_invalid_falls_back_to_default() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            std::env::set_var("TEMPYR_JOURNAL_TICK_SECS", "not-a-number");
        }
        assert_eq!(
            interval_from_env(),
            Duration::from_secs(DEFAULT_INTERVAL_SECS)
        );
        unsafe {
            std::env::set_var("TEMPYR_JOURNAL_TICK_SECS", "0");
        }
        // Zero is treated as "unset" so the loop doesn't busy-spin.
        assert_eq!(
            interval_from_env(),
            Duration::from_secs(DEFAULT_INTERVAL_SECS)
        );
        unsafe {
            std::env::remove_var("TEMPYR_JOURNAL_TICK_SECS");
        }
    }

    #[tokio::test]
    async fn spawn_in_non_git_dir_returns_not_a_git_repo() {
        let dir = tempfile::tempdir().unwrap();
        let ct = CancellationToken::new();
        let outcome = spawn(dir.path(), ct);
        match outcome {
            SpawnOutcome::NotAGitRepo => {}
            other => panic!("expected NotAGitRepo, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn shutdown_runs_final_flush_against_pending_session() {
        // Whole point of slice 2: the user closes the agent, the .ready
        // session shouldn't sit on disk until next launch. The cancel
        // path should run one more flush before returning.
        let (_outer, repo, bare, common) = init_repo_with_remote();
        let id = SessionId::parse("20260428-deadbeef-120000").unwrap();
        seed_ready_session(&common, &id);

        // Long interval so the periodic tick *doesn't* fire — we want
        // to verify the final-flush behavior on cancellation, not a
        // periodic tick. Using spawn_with_interval (not spawn) avoids
        // sharing TEMPYR_JOURNAL_TICK_SECS with parallel tests.
        let ct = CancellationToken::new();
        let outcome = spawn_with_interval(&repo, Duration::from_secs(3600), ct.clone());
        let handle = match outcome {
            SpawnOutcome::Running { handle, .. } => handle,
            other => panic!("expected Running, got {other:?}"),
        };

        // Give the tokio task a moment to enter the select.
        tokio::time::sleep(Duration::from_millis(50)).await;
        ct.cancel();
        // The task should run one final flush, then return promptly.
        tokio::time::timeout(Duration::from_secs(10), handle)
            .await
            .expect("ticker task hung after cancellation")
            .expect("ticker task panicked");

        // Ref landed on the bare remote, open dir cleaned up.
        let refname = id.archive_ref_path();
        let bare_has_ref = std::process::Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", &refname])
            .current_dir(&bare)
            .status()
            .unwrap()
            .success();
        assert!(
            bare_has_ref,
            "bare remote should have the ref after final flush"
        );
        assert!(!jpath::session_jsonl_path(&common, id.as_str()).exists());

        // state.json reflects the publish.
        let state = PublisherState::load(&common).unwrap();
        assert_eq!(state.commits_total, 1);
        assert_eq!(state.pushes_total, 1);
    }

    #[tokio::test]
    async fn periodic_tick_publishes_session() {
        // Seed AFTER spawn to force the periodic tick (not the final
        // flush) to be what publishes.
        let (_outer, repo, bare, common) = init_repo_with_remote();

        let ct = CancellationToken::new();
        let outcome = spawn_with_interval(&repo, Duration::from_millis(150), ct.clone());
        let handle = match outcome {
            SpawnOutcome::Running { handle, .. } => handle,
            other => panic!("expected Running, got {other:?}"),
        };

        // Seed after spawn so the periodic tick (not final flush) is
        // what publishes. The cleanup signals success.
        let id = SessionId::parse("20260428-deadbeef-130000").unwrap();
        seed_ready_session(&common, &id);

        // Poll for cleanup (== publish completed) up to 10s. The
        // window is generous because the spawn_blocking pool plus git
        // process startup can be a couple hundred ms each on Windows.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if !jpath::session_ready_marker(&common, id.as_str()).exists() {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("periodic tick never published the seeded session");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        // Bare remote should have it.
        let refname = id.archive_ref_path();
        let bare_has_ref = std::process::Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", &refname])
            .current_dir(&bare)
            .status()
            .unwrap()
            .success();
        assert!(bare_has_ref);

        ct.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(10), handle).await;
    }
}
