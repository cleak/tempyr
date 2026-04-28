//! Helpers used by `tempyr init` to configure the journal subsystem.
//!
//! Three pieces:
//!
//! 1. [`detect_visibility`] — best-effort check whether `origin` points
//!    at a public GitHub repo. Used to default `[journal] enabled` to
//!    `false` (with a warning) when journals would be world-readable.
//! 2. [`configure_auto_fetch_refspec`] — runs `git config --add
//!    remote.<remote>.fetch +refs/tempyr/journals/*:refs/tempyr/journals/*`
//!    so a regular `git fetch <remote>` also pulls journal refs.
//! 3. [`render_journal_config_block`] — produces the `[journal]` TOML
//!    block to append to `.tempyr/config.toml`.
//!
//! All operations are non-fatal at the init layer: we surface their
//! outcome in the init summary but never abort the whole project setup.

use std::path::Path;
use std::process::Command;

/// Outcome of a public/private repo check. We err toward "private" only
/// when we have evidence; anything else is undetermined and we warn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Private,
    Undetermined,
}

/// Try to determine the visibility of `<repo_root>`'s `origin` remote.
/// Uses `gh` CLI as the primary signal — it's already installed for
/// most users coding with Claude/Cursor and authenticated against the
/// host that owns the repo.
///
/// Returns [`Visibility::Undetermined`] if:
/// - the repo has no `origin`
/// - `origin` doesn't parse as a GitHub URL
/// - `gh` is missing or returned a non-zero exit / unrecognized output
///
/// We deliberately don't fall back to unauthenticated GitHub API calls
/// here; that would pull `reqwest` into the CLI for one-shot use. If a
/// real user reports this is too narrow we can revisit.
pub fn detect_visibility(repo_root: &Path) -> Visibility {
    let Some(url) = git_remote_url(repo_root, "origin") else {
        return Visibility::Undetermined;
    };
    let Some((owner, repo)) = parse_owner_repo_from_url(&url) else {
        return Visibility::Undetermined;
    };
    let slug = format!("{owner}/{repo}");
    let output = Command::new("gh")
        .args([
            "repo",
            "view",
            &slug,
            "--json",
            "visibility",
            "-q",
            ".visibility",
        ])
        .output();
    let Ok(output) = output else {
        return Visibility::Undetermined;
    };
    if !output.status.success() {
        return Visibility::Undetermined;
    }
    match String::from_utf8_lossy(&output.stdout)
        .trim()
        .to_uppercase()
        .as_str()
    {
        "PUBLIC" => Visibility::Public,
        // GitHub uses "INTERNAL" for org-internal repos. They aren't
        // world-readable, so treat as private.
        "PRIVATE" | "INTERNAL" => Visibility::Private,
        _ => Visibility::Undetermined,
    }
}

/// Read `git remote get-url <remote>`. None on error.
fn git_remote_url(repo_root: &Path, remote: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["remote", "get-url", remote])
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if url.is_empty() { None } else { Some(url) }
}

/// Parse a GitHub remote URL into `(owner, repo)`. Handles the three
/// common forms:
/// - `https://github.com/owner/repo` (with or without `.git`)
/// - `git@github.com:owner/repo` (SCP-style, with or without `.git`)
/// - `ssh://git@github.com/owner/repo`
///
/// Returns `None` for any other host or unparseable input.
pub fn parse_owner_repo_from_url(url: &str) -> Option<(String, String)> {
    let lower = url.to_ascii_lowercase();
    // Strip the host marker; whatever's left should be `<owner>/<repo>`.
    let after_host = if let Some(rest) = lower.strip_prefix("https://github.com/") {
        rest.to_string()
    } else if let Some(rest) = lower.strip_prefix("http://github.com/") {
        rest.to_string()
    } else if let Some(rest) = lower.strip_prefix("git@github.com:") {
        rest.to_string()
    } else if let Some(rest) = lower.strip_prefix("ssh://git@github.com/") {
        rest.to_string()
    } else {
        return None;
    };
    // Drop trailing `.git` and any trailing slash.
    let trimmed = after_host
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .trim_end_matches('/');
    let (owner, repo) = trimmed.split_once('/')?;
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

/// Add the journal-refs fetch refspec to `remote.<remote>.fetch` if it's
/// not already configured. Returns `Ok(true)` if a new entry was added,
/// `Ok(false)` if it was already present, or `Err` if `git config`
/// itself failed.
///
/// The refspec mirrors all journal refs from the remote so a plain
/// `git fetch <remote>` pulls another agent's pushed journals into the
/// local repo without the user needing to remember `tempyr journal
/// fetch`.
pub fn configure_auto_fetch_refspec(repo_root: &Path, remote: &str) -> std::io::Result<bool> {
    let refspec = "+refs/tempyr/journals/*:refs/tempyr/journals/*";
    let key = format!("remote.{remote}.fetch");

    // Check existing values — `git config --get-all <key>` lists every
    // configured fetch refspec, one per line.
    let existing = Command::new("git")
        .args(["config", "--get-all", &key])
        .current_dir(repo_root)
        .output()?;
    if existing.status.success() {
        let text = String::from_utf8_lossy(&existing.stdout);
        if text.lines().any(|line| line.trim() == refspec) {
            return Ok(false);
        }
    }

    let add = Command::new("git")
        .args(["config", "--add", &key, refspec])
        .current_dir(repo_root)
        .output()?;
    if !add.status.success() {
        return Err(std::io::Error::other(format!(
            "git config --add {key} failed: {}",
            String::from_utf8_lossy(&add.stderr).trim()
        )));
    }
    Ok(true)
}

/// Render the `[journal]` TOML block to append to a fresh
/// `.tempyr/config.toml`. Defaults match [`tempyr_journal::JournalConfig`];
/// only the `enabled` field varies based on the detected visibility.
pub fn render_journal_config_block(enabled: bool) -> String {
    format!(
        r#"
[journal]
# Capture agent reasoning (decisions, dead ends, findings) and push it
# to refs/tempyr/journals/* on the configured remote. Set to false to
# disable auto-publish; the `tempyr journal flush` CLI still works for
# one-off flushes.
enabled = {enabled}
remote = "origin"
tick_secs = 60                  # in-process publisher cadence inside `tempyr --mcp`
pack_refs_every_n_pushes = 50   # 0 disables `git pack-refs --all` after pushes
push_timeout_secs = 30
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_https_url() {
        assert_eq!(
            parse_owner_repo_from_url("https://github.com/cleak/tempyr"),
            Some(("cleak".to_string(), "tempyr".to_string()))
        );
    }

    #[test]
    fn parses_https_url_with_git_suffix() {
        assert_eq!(
            parse_owner_repo_from_url("https://github.com/cleak/tempyr.git"),
            Some(("cleak".to_string(), "tempyr".to_string()))
        );
    }

    #[test]
    fn parses_https_url_with_trailing_slash() {
        assert_eq!(
            parse_owner_repo_from_url("https://github.com/cleak/tempyr/"),
            Some(("cleak".to_string(), "tempyr".to_string()))
        );
    }

    #[test]
    fn parses_scp_style_url() {
        assert_eq!(
            parse_owner_repo_from_url("git@github.com:cleak/tempyr.git"),
            Some(("cleak".to_string(), "tempyr".to_string()))
        );
    }

    #[test]
    fn parses_ssh_url() {
        assert_eq!(
            parse_owner_repo_from_url("ssh://git@github.com/cleak/tempyr.git"),
            Some(("cleak".to_string(), "tempyr".to_string()))
        );
    }

    #[test]
    fn rejects_non_github_urls() {
        assert_eq!(parse_owner_repo_from_url("https://gitlab.com/x/y"), None);
        assert_eq!(parse_owner_repo_from_url("https://bitbucket.org/x/y"), None);
        assert_eq!(parse_owner_repo_from_url("not-a-url"), None);
    }

    #[test]
    fn rejects_malformed_paths() {
        // Missing repo segment.
        assert_eq!(parse_owner_repo_from_url("https://github.com/cleak"), None);
        // Extra segment (subgroup-style — GitHub doesn't support it).
        assert_eq!(parse_owner_repo_from_url("https://github.com/a/b/c"), None);
        // Empty owner.
        assert_eq!(parse_owner_repo_from_url("https://github.com//repo"), None);
    }

    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "t@t"])
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "t"])
            .current_dir(repo)
            .output()
            .unwrap();
        dir
    }

    #[test]
    fn configure_auto_fetch_refspec_adds_then_idempotent() {
        let dir = init_repo();
        let repo = dir.path();
        Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "https://github.com/example/example.git",
            ])
            .current_dir(repo)
            .output()
            .unwrap();

        let added = configure_auto_fetch_refspec(repo, "origin").unwrap();
        assert!(added, "first call should add the refspec");
        let added_again = configure_auto_fetch_refspec(repo, "origin").unwrap();
        assert!(
            !added_again,
            "second call should be a no-op (already present)"
        );

        let output = Command::new("git")
            .args(["config", "--get-all", "remote.origin.fetch"])
            .current_dir(repo)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        let count = text
            .lines()
            .filter(|l| l.trim() == "+refs/tempyr/journals/*:refs/tempyr/journals/*")
            .count();
        assert_eq!(count, 1, "refspec should appear exactly once");
    }

    #[test]
    fn detect_visibility_undetermined_for_no_origin() {
        let dir = init_repo();
        // No remote configured → no URL → Undetermined.
        let v = detect_visibility(dir.path());
        assert_eq!(v, Visibility::Undetermined);
    }

    #[test]
    fn detect_visibility_undetermined_for_non_github_origin() {
        let dir = init_repo();
        Command::new("git")
            .args(["remote", "add", "origin", "https://gitlab.com/a/b.git"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let v = detect_visibility(dir.path());
        assert_eq!(v, Visibility::Undetermined);
    }

    #[test]
    fn render_journal_config_block_round_trips_through_loader() {
        // The block we write at init time must parse back via the
        // journal config loader without errors.
        let block = render_journal_config_block(false);
        let cfg = tempyr_journal::JournalConfig::from_toml_str(&block).unwrap();
        assert!(!cfg.enabled);
        assert_eq!(cfg.remote, "origin");
        assert_eq!(cfg.tick_secs, 60);

        let block_on = render_journal_config_block(true);
        let cfg_on = tempyr_journal::JournalConfig::from_toml_str(&block_on).unwrap();
        assert!(cfg_on.enabled);
    }
}
