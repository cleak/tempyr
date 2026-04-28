//! Shell-out helpers for the publisher.
//!
//! Each helper invokes `git` as a subprocess with hardened settings:
//! - `GIT_TERMINAL_PROMPT=0` so a credential prompt can't wedge the publisher
//! - 30-second timeout, after which the child is killed
//! - stdin/stdout/stderr piped and drained in helper threads (a >64 KB pipe
//!   buffer can otherwise deadlock the child)
//! - `current_dir` set to the repo path; `GIT_DIR`/`GIT_WORK_TREE` cleared so
//!   we don't inherit a stray env from the parent
//!
//! Helpers cover the publisher's needs only: hash-object, mktree,
//! commit-tree, update-ref, ref existence, and push. They are intentionally
//! thin wrappers — no semantic interpretation of stderr beyond "exit
//! status was non-zero".

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::{JournalError, Result};

/// Default per-operation timeout. Fast local ops (mktree/commit-tree/etc)
/// finish in milliseconds; the long pole is `push` over the network.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Captured output of a single git invocation.
#[derive(Debug, Clone)]
pub struct GitOutput {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

impl GitOutput {
    /// Return Ok if the process exited 0; otherwise an error containing
    /// the trimmed stderr (or stdout fallback).
    pub fn ok_or_err(self, op: &str) -> Result<Self> {
        if self.status.success() {
            return Ok(self);
        }
        let msg = if !self.stderr.trim().is_empty() {
            self.stderr.trim().to_string()
        } else if !self.stdout.trim().is_empty() {
            self.stdout.trim().to_string()
        } else {
            format!("git exited with {}", self.status)
        };
        Err(JournalError::Git(format!("{op}: {msg}")))
    }
}

/// Run `git <args>` in `repo`, optionally feeding `stdin`, with a
/// per-process `timeout`. Captures stdout and stderr.
pub fn run(
    repo: &Path,
    args: &[&str],
    stdin: Option<&[u8]>,
    timeout: Duration,
) -> Result<GitOutput> {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(repo)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });

    let mut child = cmd
        .spawn()
        .map_err(|e| JournalError::Git(format!("spawn git: {e}")))?;

    if let Some(bytes) = stdin
        && let Some(mut sin) = child.stdin.take()
    {
        sin.write_all(bytes)
            .map_err(|e| JournalError::Git(format!("write stdin: {e}")))?;
        // Drop closes the pipe so git sees EOF.
    }

    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let out_h = thread::spawn(move || drain(stdout));
    let err_h = thread::spawn(move || drain(stderr));

    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(s) = child
            .try_wait()
            .map_err(|e| JournalError::Git(format!("wait: {e}")))?
        {
            break s;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            // Drain after kill so the threads exit cleanly.
            let _ = out_h.join();
            let _ = err_h.join();
            return Err(JournalError::Git(format!(
                "git {} timed out after {}s",
                args.join(" "),
                timeout.as_secs()
            )));
        }
        thread::sleep(Duration::from_millis(20));
    };

    let stdout = out_h.join().unwrap_or_default();
    let stderr = err_h.join().unwrap_or_default();
    Ok(GitOutput {
        status,
        stdout,
        stderr,
    })
}

fn drain<R: Read>(mut r: R) -> String {
    let mut buf = String::new();
    let _ = r.read_to_string(&mut buf);
    buf
}

/// `git hash-object -w --stdin` — write `bytes` as a blob to the object
/// database and return its SHA-1.
pub fn hash_object_blob(repo: &Path, bytes: &[u8]) -> Result<String> {
    let out = run(
        repo,
        &["hash-object", "-w", "--stdin"],
        Some(bytes),
        DEFAULT_TIMEOUT,
    )?
    .ok_or_err("hash-object")?;
    Ok(out.stdout.trim().to_string())
}

/// One entry in a tree built via `git mktree`.
#[derive(Debug, Clone)]
pub struct TreeEntry<'a> {
    /// Octal mode, e.g. `100644` for a regular file blob.
    pub mode: &'a str,
    /// `blob` or `tree`.
    pub kind: &'a str,
    /// Object SHA.
    pub sha: &'a str,
    /// Filename within the tree.
    pub name: &'a str,
}

/// `git mktree` — build a tree object from `entries` and return its SHA.
/// Caller is responsible for ensuring the entries are sorted by name (git
/// will reject an unsorted tree).
pub fn mktree(repo: &Path, entries: &[TreeEntry<'_>]) -> Result<String> {
    let mut input = String::new();
    for e in entries {
        // Git's tree-entry stdin format: `<mode> SP <type> SP <sha> TAB <name> LF`
        input.push_str(e.mode);
        input.push(' ');
        input.push_str(e.kind);
        input.push(' ');
        input.push_str(e.sha);
        input.push('\t');
        input.push_str(e.name);
        input.push('\n');
    }
    let out =
        run(repo, &["mktree"], Some(input.as_bytes()), DEFAULT_TIMEOUT)?.ok_or_err("mktree")?;
    Ok(out.stdout.trim().to_string())
}

/// `git commit-tree <tree> -m <message>` — create a parent-less commit
/// pointing at `tree`. Journal commits are orphans (no parent) so the
/// archive ref doesn't grow a chain that pulls extra objects.
pub fn commit_tree(repo: &Path, tree_sha: &str, message: &str) -> Result<String> {
    let out = run(
        repo,
        &["commit-tree", tree_sha, "-m", message],
        None,
        DEFAULT_TIMEOUT,
    )?
    .ok_or_err("commit-tree")?;
    Ok(out.stdout.trim().to_string())
}

/// `git update-ref <refname> <sha>` — point `refname` at `sha`. Idempotent
/// in the sense that it overwrites unconditionally, but pass `expected` to
/// fail on concurrent modification (we don't, in slice 1).
pub fn update_ref(repo: &Path, refname: &str, sha: &str) -> Result<()> {
    run(repo, &["update-ref", refname, sha], None, DEFAULT_TIMEOUT)?.ok_or_err("update-ref")?;
    Ok(())
}

/// True if `refname` resolves to an object in this repo.
pub fn ref_exists(repo: &Path, refname: &str) -> Result<bool> {
    let out = run(
        repo,
        &["rev-parse", "--verify", "--quiet", refname],
        None,
        DEFAULT_TIMEOUT,
    )?;
    // `rev-parse --verify --quiet`: exit 0 if exists, 1 if not. Anything
    // else is an actual error.
    if out.status.success() {
        return Ok(true);
    }
    let code = out.status.code().unwrap_or(-1);
    if code == 1 && out.stderr.trim().is_empty() {
        return Ok(false);
    }
    Err(JournalError::Git(format!(
        "rev-parse --verify {refname}: {}",
        if out.stderr.trim().is_empty() {
            format!("exit {code}")
        } else {
            out.stderr.trim().to_string()
        }
    )))
}

/// `git push <remote> <refspec>` — push a single ref. Pass `--quiet` to
/// keep stderr small. Returns the captured `GitOutput` so callers can log
/// stderr regardless of success/failure (push prints progress to stderr).
pub fn push_ref(repo: &Path, remote: &str, refspec: &str, timeout: Duration) -> Result<GitOutput> {
    run(repo, &["push", "--quiet", remote, refspec], None, timeout)?.ok_or_err("push")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Initialize a fresh repo at `path` with a single empty commit so HEAD
    /// resolves and a default branch exists. Returns the repo path.
    fn init_repo(path: &Path) -> PathBuf {
        run(
            path,
            &["init", "--quiet", "--initial-branch=main"],
            None,
            DEFAULT_TIMEOUT,
        )
        .unwrap()
        .ok_or_err("init")
        .unwrap();
        // Configure a local identity so commit-tree doesn't fail.
        run(
            path,
            &["config", "user.name", "tempyr-test"],
            None,
            DEFAULT_TIMEOUT,
        )
        .unwrap()
        .ok_or_err("config")
        .unwrap();
        run(
            path,
            &["config", "user.email", "tempyr-test@example.com"],
            None,
            DEFAULT_TIMEOUT,
        )
        .unwrap()
        .ok_or_err("config")
        .unwrap();
        path.to_path_buf()
    }

    #[test]
    fn hash_object_blob_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = init_repo(tmp.path());
        let sha = hash_object_blob(&repo, b"hello tempyr\n").unwrap();
        assert_eq!(sha.len(), 40);
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));

        // The blob should be retrievable via cat-file.
        let cat = run(&repo, &["cat-file", "-p", &sha], None, DEFAULT_TIMEOUT)
            .unwrap()
            .ok_or_err("cat-file")
            .unwrap();
        assert_eq!(cat.stdout, "hello tempyr\n");
    }

    #[test]
    fn mktree_builds_tree_from_blobs() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = init_repo(tmp.path());
        let blob_a = hash_object_blob(&repo, b"a\n").unwrap();
        let blob_b = hash_object_blob(&repo, b"b\n").unwrap();
        let entries = vec![
            TreeEntry {
                mode: "100644",
                kind: "blob",
                sha: &blob_a,
                name: "a.txt",
            },
            TreeEntry {
                mode: "100644",
                kind: "blob",
                sha: &blob_b,
                name: "b.txt",
            },
        ];
        let tree_sha = mktree(&repo, &entries).unwrap();
        assert_eq!(tree_sha.len(), 40);

        let listing = run(&repo, &["ls-tree", &tree_sha], None, DEFAULT_TIMEOUT)
            .unwrap()
            .ok_or_err("ls-tree")
            .unwrap();
        assert!(listing.stdout.contains("a.txt"));
        assert!(listing.stdout.contains("b.txt"));
    }

    #[test]
    fn commit_tree_creates_parentless_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = init_repo(tmp.path());
        let blob = hash_object_blob(&repo, b"x\n").unwrap();
        let tree = mktree(
            &repo,
            &[TreeEntry {
                mode: "100644",
                kind: "blob",
                sha: &blob,
                name: "x.txt",
            }],
        )
        .unwrap();
        let commit = commit_tree(&repo, &tree, "tempyr journal test").unwrap();
        assert_eq!(commit.len(), 40);

        let body = run(&repo, &["cat-file", "-p", &commit], None, DEFAULT_TIMEOUT)
            .unwrap()
            .ok_or_err("cat-file")
            .unwrap();
        assert!(body.stdout.contains(&format!("tree {tree}")));
        // No parent line: this is a journal-style orphan commit.
        assert!(!body.stdout.contains("parent "));
    }

    #[test]
    fn update_ref_and_ref_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = init_repo(tmp.path());
        let blob = hash_object_blob(&repo, b"x\n").unwrap();
        let tree = mktree(
            &repo,
            &[TreeEntry {
                mode: "100644",
                kind: "blob",
                sha: &blob,
                name: "x.txt",
            }],
        )
        .unwrap();
        let commit = commit_tree(&repo, &tree, "test").unwrap();

        let refname = "refs/tempyr/journals/archive/2026/04/27/test-session";
        assert!(!ref_exists(&repo, refname).unwrap());
        update_ref(&repo, refname, &commit).unwrap();
        assert!(ref_exists(&repo, refname).unwrap());
    }

    #[test]
    fn push_to_local_bare_remote() {
        let outer = tempfile::tempdir().unwrap();
        let bare = outer.path().join("remote.git");
        std::fs::create_dir_all(&bare).unwrap();
        run(
            &bare,
            &["init", "--quiet", "--bare", "--initial-branch=main"],
            None,
            DEFAULT_TIMEOUT,
        )
        .unwrap()
        .ok_or_err("init bare")
        .unwrap();

        let work = outer.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let repo = init_repo(&work);
        run(
            &repo,
            &["remote", "add", "origin", &bare.to_string_lossy()],
            None,
            DEFAULT_TIMEOUT,
        )
        .unwrap()
        .ok_or_err("remote add")
        .unwrap();

        let blob = hash_object_blob(&repo, b"payload\n").unwrap();
        let tree = mktree(
            &repo,
            &[TreeEntry {
                mode: "100644",
                kind: "blob",
                sha: &blob,
                name: "f.txt",
            }],
        )
        .unwrap();
        let commit = commit_tree(&repo, &tree, "test").unwrap();
        let refname = "refs/tempyr/journals/archive/2026/04/27/x";
        update_ref(&repo, refname, &commit).unwrap();

        let refspec = format!("{refname}:{refname}");
        push_ref(&repo, "origin", &refspec, DEFAULT_TIMEOUT).unwrap();

        // Bare remote should now have the ref.
        assert!(ref_exists(&bare, refname).unwrap());
    }

    #[test]
    fn push_failure_surfaces_stderr() {
        // No remote configured → push should fail with a useful message.
        let tmp = tempfile::tempdir().unwrap();
        let repo = init_repo(tmp.path());
        let err = push_ref(
            &repo,
            "nonexistent-remote",
            "refs/heads/main:refs/heads/main",
            DEFAULT_TIMEOUT,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("push"), "error should mention push op: {msg}");
    }

    #[test]
    fn timeout_kills_long_running_child() {
        // We can't easily make `git` itself hang, so smoke-test the timeout
        // path by giving git a very short deadline against a real op. With
        // a 1ms timeout, the spawn-and-wait loop must hit the deadline
        // (any op including process startup takes longer than 1ms on
        // typical CI hardware).
        let tmp = tempfile::tempdir().unwrap();
        let repo = init_repo(tmp.path());
        let err = run(&repo, &["fsck"], None, Duration::from_millis(1)).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("timed out"), "should report timeout: {msg}");
    }

    #[test]
    fn ref_exists_returns_false_for_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = init_repo(tmp.path());
        assert!(!ref_exists(&repo, "refs/tempyr/journals/archive/9999/12/31/no").unwrap());
    }
}
