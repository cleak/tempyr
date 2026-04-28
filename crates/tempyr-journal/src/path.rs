//! Git-aware path resolution for the journal subsystem.
//!
//! Worktrees share Git refs and the object database with the primary repo, but
//! their `.git` is a *file* pointing to `.git/worktrees/<wt>/` inside the
//! primary `.git`. We resolve every path through `git rev-parse --git-common-dir`
//! so journal storage lives in the shared location and survives worktree
//! pruning.
//!
//! Layout under the resolved common dir:
//!
//! ```text
//! <git-common-dir>/
//!   tempyr/
//!     journals/
//!       open/                      # in-flight session JSONL files
//!         <session-id>.jsonl
//!         <session-id>.meta.json
//!         <session-id>.ready       # marker: publisher may commit
//!       publisher.lock              # single-publisher coordination
//!       state.json                  # last push, last error, etc.
//!       publisher.log               # rotating file log
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{JournalError, Result};

/// Run `git rev-parse --git-common-dir` from `start` and return the absolute
/// path. Errors with `NotAGitRepo` if `start` is not inside a Git work tree.
pub fn git_common_dir(start: &Path) -> Result<PathBuf> {
    let raw = git_rev_parse(start, &["--git-common-dir"])?;
    let path = PathBuf::from(&raw);
    let absolute = if path.is_absolute() {
        path
    } else {
        start.join(path)
    };
    Ok(canonicalize_or_keep(&absolute))
}

/// Run `git rev-parse --show-toplevel` from `start` and return the absolute
/// path of the working tree root.
pub fn repo_toplevel(start: &Path) -> Result<PathBuf> {
    let raw = git_rev_parse(start, &["--show-toplevel"])?;
    let path = PathBuf::from(&raw);
    Ok(canonicalize_or_keep(&path))
}

/// Current branch name (`git rev-parse --abbrev-ref HEAD`). Returns `None` if
/// HEAD is detached.
pub fn current_branch(start: &Path) -> Result<Option<String>> {
    match git_rev_parse(start, &["--abbrev-ref", "HEAD"]) {
        Ok(s) if s == "HEAD" => Ok(None),
        Ok(s) => Ok(Some(s)),
        Err(e) => Err(e),
    }
}

/// Current HEAD SHA (`git rev-parse HEAD`). Returns `None` on a fresh repo
/// with no commits yet.
pub fn current_head(start: &Path) -> Result<Option<String>> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("HEAD")
        .current_dir(start)
        .output()?;
    if output.status.success() {
        let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if s.is_empty() { Ok(None) } else { Ok(Some(s)) }
    } else {
        // Empty repo: `git rev-parse HEAD` exits non-zero. Distinguish from
        // "not a repo" by checking the cause.
        let err = String::from_utf8_lossy(&output.stderr);
        if err.contains("unknown revision")
            || err.contains("ambiguous argument 'HEAD'")
            || err.contains("does not have any commits yet")
        {
            Ok(None)
        } else {
            Err(JournalError::NotAGitRepo(err.into_owned()))
        }
    }
}

/// 8 hex chars of blake3 over the normalized worktree path. On Windows the
/// path is lowercased before hashing because the FS is case-insensitive;
/// elsewhere case is preserved.
pub fn worktree_hash(worktree_top: &Path) -> String {
    let canonical = canonicalize_or_keep(worktree_top);
    let s = canonical.to_string_lossy();
    let normalized = normalize_for_hash(&s);
    let hex = blake3::hash(normalized.as_bytes()).to_hex();
    hex[..8].to_string()
}

/// Repo-relative form of `path` with forward slashes. Absolute paths under
/// `worktree_top` are stripped to the relative remainder; relative paths are
/// passed through (assumed already repo-local). Path separators are
/// normalized to `/` in either case so Windows backslashes don't survive into
/// the journal.
///
/// Used by `tempyr journal log --file <path>` and the journal_log MCP tool to
/// keep absolute repo paths from tripping the redactor's `user_home_path` rule
/// when the repo lives under `/Users/<name>/` or `C:\Users\<name>\`.
///
/// For relative inputs that should be resolved against a working directory
/// before normalization (e.g. `--file src/lib.rs` from a subdirectory), use
/// [`resolve_file_path`] instead.
pub fn repo_relative_path(path: &str, worktree_top: &Path) -> String {
    let p = Path::new(path);
    let body = if p.is_absolute() {
        let canon_p = canonicalize_or_keep(p);
        let canon_top = canonicalize_or_keep(worktree_top);
        canon_p
            .strip_prefix(&canon_top)
            .map(|rel| rel.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_string())
    } else {
        path.to_string()
    };
    body.replace('\\', "/")
}

/// Like [`repo_relative_path`] but joins relative inputs onto `cwd` first.
/// `--file src/lib.rs` invoked from `<repo>/crates/foo/` should record as
/// `crates/foo/src/lib.rs`, not `src/lib.rs` (which a reader would interpret
/// relative to the repo root).
///
/// When `cwd` is `None`, falls back to [`repo_relative_path`]'s pass-through
/// behavior for relative inputs.
pub fn resolve_file_path(path: &str, worktree_top: &Path, cwd: Option<&Path>) -> String {
    let p = Path::new(path);
    if !p.is_absolute()
        && let Some(base) = cwd
    {
        let abs = base.join(p);
        return repo_relative_path(&abs.to_string_lossy(), worktree_top);
    }
    repo_relative_path(path, worktree_top)
}

/// Tempyr's directory under the git common dir: `<common>/tempyr/`.
pub fn tempyr_dir(common_dir: &Path) -> PathBuf {
    common_dir.join("tempyr")
}

/// Journals root: `<common>/tempyr/journals/`.
pub fn journals_root(common_dir: &Path) -> PathBuf {
    tempyr_dir(common_dir).join("journals")
}

/// Open-sessions directory: `<common>/tempyr/journals/open/`.
pub fn open_dir(common_dir: &Path) -> PathBuf {
    journals_root(common_dir).join("open")
}

/// Path to a specific session's JSONL file (in `open/`).
pub fn session_jsonl_path(common_dir: &Path, session_id: &str) -> PathBuf {
    open_dir(common_dir).join(format!("{session_id}.jsonl"))
}

/// Path to a session's metadata sidecar.
pub fn session_meta_path(common_dir: &Path, session_id: &str) -> PathBuf {
    open_dir(common_dir).join(format!("{session_id}.meta.json"))
}

/// Marker file that signals "ready to publish" for a session.
pub fn session_ready_marker(common_dir: &Path, session_id: &str) -> PathBuf {
    open_dir(common_dir).join(format!("{session_id}.ready"))
}

/// Single-publisher lockfile.
pub fn publisher_lock_path(common_dir: &Path) -> PathBuf {
    journals_root(common_dir).join("publisher.lock")
}

/// Sticky publisher state (last push, last error).
pub fn publisher_state_path(common_dir: &Path) -> PathBuf {
    journals_root(common_dir).join("state.json")
}

/// Rotating publisher log file.
pub fn publisher_log_path(common_dir: &Path) -> PathBuf {
    journals_root(common_dir).join("publisher.log")
}

/// Create the journal directory layout if it doesn't already exist. Idempotent.
pub fn ensure_layout(common_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(open_dir(common_dir))?;
    Ok(())
}

// ---- Helpers ----

fn git_rev_parse(start: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("rev-parse")
        .args(args)
        .current_dir(start)
        .output()
        .map_err(|e| JournalError::Git(format!("failed to invoke git: {e}")))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr).into_owned();
        return Err(JournalError::NotAGitRepo(err.trim().to_string()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn canonicalize_or_keep(p: &Path) -> PathBuf {
    match p.canonicalize() {
        Ok(c) => strip_unc(c),
        Err(_) => p.to_path_buf(),
    }
}

/// On Windows, `Path::canonicalize` returns paths prefixed with `\\?\`.
/// Strip that to keep paths interoperable with shelled-out git invocations,
/// which don't always handle the long-path prefix.
#[cfg(target_os = "windows")]
fn strip_unc(path: PathBuf) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path
    }
}

#[cfg(not(target_os = "windows"))]
fn strip_unc(path: PathBuf) -> PathBuf {
    path
}

#[cfg(target_os = "windows")]
fn normalize_for_hash(s: &str) -> String {
    s.to_ascii_lowercase()
}

#[cfg(not(target_os = "windows"))]
fn normalize_for_hash(s: &str) -> String {
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn current_dir_is_a_git_repo() {
        let cwd = env::current_dir().unwrap();
        let common = git_common_dir(&cwd).unwrap();
        assert!(
            common.exists(),
            "common dir should exist: {}",
            common.display()
        );
        // Common dir contains HEAD (any git dir does)
        assert!(common.join("HEAD").exists() || common.join("packed-refs").exists());
    }

    #[test]
    fn worktree_common_dir_points_to_primary() {
        // The common dir should reflect whether we're in a primary checkout
        // or a linked worktree:
        //   primary:  <top>/.git is a directory; common_dir is under <top>
        //   worktree: <top>/.git is a file; common_dir is the primary's .git
        //             (outside the worktree's top-level)
        let cwd = env::current_dir().unwrap();
        let common = git_common_dir(&cwd).unwrap();
        let top = repo_toplevel(&cwd).unwrap();
        let dot_git = top.join(".git");

        if dot_git.is_file() {
            assert!(
                !common.starts_with(&top),
                "worktree common_dir {} should not be under worktree top {}",
                common.display(),
                top.display()
            );
        } else {
            assert!(
                common.starts_with(&top),
                "primary common_dir {} should be under repo top {}",
                common.display(),
                top.display()
            );
        }
    }

    #[test]
    fn worktree_hash_is_stable_and_short() {
        let cwd = env::current_dir().unwrap();
        let h1 = worktree_hash(&cwd);
        let h2 = worktree_hash(&cwd);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 8);
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn worktree_hash_differs_for_different_paths() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        assert_ne!(
            worktree_hash(a.path()),
            worktree_hash(b.path()),
            "different paths should produce different hashes"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn worktree_hash_case_insensitive_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        // Construct two PathBufs that point at the same dir but with
        // different case in any letters present.
        let lower = PathBuf::from(dir.path().to_string_lossy().to_lowercase());
        let upper = PathBuf::from(dir.path().to_string_lossy().to_uppercase());
        // Skip if the temp dir path has no letters (purely numeric prefix etc)
        if lower != upper {
            assert_eq!(worktree_hash(&lower), worktree_hash(&upper));
        }
    }

    #[test]
    fn non_git_directory_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let result = git_common_dir(dir.path());
        assert!(matches!(result, Err(JournalError::NotAGitRepo(_))));
    }

    #[test]
    fn ensure_layout_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let common = dir.path();
        ensure_layout(common).unwrap();
        ensure_layout(common).unwrap();
        assert!(open_dir(common).exists());
    }

    #[test]
    fn paths_compose_correctly() {
        let common = PathBuf::from("/tmp/repo/.git");
        assert_eq!(tempyr_dir(&common), PathBuf::from("/tmp/repo/.git/tempyr"));
        assert_eq!(
            journals_root(&common),
            PathBuf::from("/tmp/repo/.git/tempyr/journals")
        );
        assert_eq!(
            open_dir(&common),
            PathBuf::from("/tmp/repo/.git/tempyr/journals/open")
        );
        assert_eq!(
            session_jsonl_path(&common, "20260427-abc12345-120000"),
            PathBuf::from("/tmp/repo/.git/tempyr/journals/open/20260427-abc12345-120000.jsonl")
        );
        assert_eq!(
            publisher_lock_path(&common),
            PathBuf::from("/tmp/repo/.git/tempyr/journals/publisher.lock")
        );
    }

    #[test]
    fn repo_relative_path_strips_worktree_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("crates").join("foo");
        std::fs::create_dir_all(&nested).unwrap();
        let absolute = nested.join("bar.rs");
        std::fs::write(&absolute, "").unwrap();
        let normalized = repo_relative_path(&absolute.to_string_lossy(), dir.path());
        assert_eq!(normalized, "crates/foo/bar.rs");
    }

    #[test]
    fn repo_relative_path_passes_through_relative_input() {
        let dir = tempfile::tempdir().unwrap();
        // Forward-slash relative input round-trips unchanged.
        assert_eq!(
            repo_relative_path("crates/foo/bar.rs", dir.path()),
            "crates/foo/bar.rs"
        );
    }

    #[test]
    fn repo_relative_path_normalizes_backslashes_in_relative_input() {
        let dir = tempfile::tempdir().unwrap();
        // Windows-style relative input gets forward slashes too.
        assert_eq!(
            repo_relative_path(r"crates\foo\bar.rs", dir.path()),
            "crates/foo/bar.rs"
        );
    }

    #[test]
    fn repo_relative_path_keeps_outside_paths_but_normalizes_separators() {
        let inside = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let other = outside.path().join("elsewhere.rs");
        std::fs::write(&other, "").unwrap();
        let raw = other.to_string_lossy().to_string();
        // Out-of-worktree absolute path is preserved (we don't try to
        // synthesize a relative path); only separators get normalized.
        let normalized = repo_relative_path(&raw, inside.path());
        assert_eq!(normalized, raw.replace('\\', "/"));
    }

    #[test]
    fn resolve_file_path_joins_relative_against_cwd_then_normalizes() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("crates").join("foo");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("bar.rs"), "").unwrap();

        // CLI scenario: user is in <repo>/crates/foo and types --file bar.rs
        // The recorded path must be crates/foo/bar.rs, not bar.rs.
        let resolved = resolve_file_path("bar.rs", dir.path(), Some(&sub));
        assert_eq!(resolved, "crates/foo/bar.rs");
    }

    #[test]
    fn resolve_file_path_passes_absolute_through_repo_relative() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b.rs");
        std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
        std::fs::write(&nested, "").unwrap();
        // Absolute input: cwd is irrelevant.
        let resolved = resolve_file_path(&nested.to_string_lossy(), dir.path(), Some(dir.path()));
        assert_eq!(resolved, "a/b.rs");
    }

    #[test]
    fn resolve_file_path_with_no_cwd_falls_back_to_pass_through() {
        let dir = tempfile::tempdir().unwrap();
        // No cwd hint: relative input survives as-is (only separator normalized).
        assert_eq!(
            resolve_file_path(r"src\lib.rs", dir.path(), None),
            "src/lib.rs"
        );
    }

    #[test]
    fn current_branch_or_detached() {
        let cwd = env::current_dir().unwrap();
        // Either a branch name or None for detached HEAD; both are valid.
        let _ = current_branch(&cwd).unwrap();
    }

    #[test]
    fn current_head_returns_sha_in_real_repo() {
        let cwd = env::current_dir().unwrap();
        let head = current_head(&cwd).unwrap();
        let sha = head.expect("repo has commits");
        assert_eq!(sha.len(), 40);
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
