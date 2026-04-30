use anyhow::Context;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::managed::WriteOutcome;

const TEMPYR_VERSION: &str = env!("CARGO_PKG_VERSION");
const MANAGED_START: &str = "# >>> tempyr managed hook >>>";
const MANAGED_END: &str = "# <<< tempyr managed hook <<<";
/// Legacy marker pair. Earlier versions of this file rendered every
/// managed hook with `tempyr managed index warmup` even when the
/// hook didn't do index warmup; the rename to a generic "hook"
/// marker happened when pre-commit landed (lint v2 backlog #8). We
/// keep the legacy pair recognized here so an upgrade replaces the
/// old block in place rather than appending a new one alongside it
/// (which would leave the user with two managed sections in their
/// hook). All *writes* use the current markers.
const LEGACY_MANAGED_START: &str = "# >>> tempyr managed index warmup >>>";
const LEGACY_MANAGED_END: &str = "# <<< tempyr managed index warmup <<<";
const SHEBANG: &str = "#!/bin/sh\n";

struct GitHookDef {
    name: &'static str,
    description: &'static str,
    /// Hook-specific body that runs *inside* the managed block,
    /// after the `run_tempyr` shell helper is defined and the
    /// `.tempyr` / `.tempyr-redirect` guard succeeds. Should invoke
    /// `run_tempyr <subcommand>` and handle its own redirection.
    /// Examples below — index-warmup hooks redirect both streams to
    /// /dev/null, while the lint hook lets stderr flow so the user
    /// sees warnings.
    body: &'static str,
}

/// Index-warmup hooks (post-checkout, post-merge) silence both
/// streams and never block the git operation. The user already sees
/// the underlying git output; we don't want to add tempyr noise
/// after every checkout.
const BODY_INDEX_WARMUP: &str =
    "  run_tempyr index update --json --skip-embeddings >/dev/null 2>&1 || true";

/// Pre-commit lint runs `tempyr journal lint` and lets stderr through
/// so the user actually sees stale-task warnings; stdout is silenced
/// (the JSON form is reserved for explicit `--json` invocations).
/// The trailing `|| true` keeps the hook from blocking the commit
/// even if the lint logic itself errors — the user can still commit;
/// they just lose the warning for this run.
const BODY_JOURNAL_LINT: &str = "  run_tempyr journal lint >/dev/null || true";

const GIT_HOOKS: &[GitHookDef] = &[
    GitHookDef {
        name: "post-checkout",
        description: "warm index after checkout and new worktree creation",
        body: BODY_INDEX_WARMUP,
    },
    GitHookDef {
        name: "post-merge",
        description: "refresh index after merges",
        body: BODY_INDEX_WARMUP,
    },
    GitHookDef {
        name: "pre-commit",
        description: "warn on in-progress tasks without journal coverage",
        body: BODY_JOURNAL_LINT,
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookStatus {
    UpToDate,
    Stale,
    Missing,
}

pub struct HookReport {
    pub name: &'static str,
    pub description: &'static str,
    pub status: HookStatus,
}

pub struct HookInstallResult {
    pub name: &'static str,
    pub description: &'static str,
    pub outcome: WriteOutcome,
}

pub fn check_all(root: &Path) -> anyhow::Result<Vec<HookReport>> {
    let Some(hooks_dir) = hooks_dir(root) else {
        return Ok(Vec::new());
    };

    let mut reports = Vec::new();
    for def in GIT_HOOKS {
        let managed_block = render_managed_block(def);
        let status = hook_status(&hooks_dir.join(def.name), &managed_block)?;
        reports.push(HookReport {
            name: def.name,
            description: def.description,
            status,
        });
    }

    Ok(reports)
}

pub fn install_all(root: &Path) -> anyhow::Result<Vec<HookInstallResult>> {
    let Some(hooks_dir) = hooks_dir(root) else {
        return Ok(Vec::new());
    };

    fs::create_dir_all(&hooks_dir)
        .with_context(|| format!("Failed to create hooks dir {}", hooks_dir.display()))?;

    let mut results = Vec::new();
    for def in GIT_HOOKS {
        let managed_block = render_managed_block(def);
        let path = hooks_dir.join(def.name);
        let existing = if path.is_file() {
            Some(
                fs::read_to_string(&path)
                    .with_context(|| format!("Failed to read hook {}", path.display()))?,
            )
        } else {
            None
        };
        let (content, outcome) = merge_hook_content(existing.as_deref(), &managed_block);

        if !matches!(outcome, WriteOutcome::Unchanged) {
            fs::write(&path, content)
                .with_context(|| format!("Failed to write hook {}", path.display()))?;
        }
        ensure_hook_executable(&path)?;

        results.push(HookInstallResult {
            name: def.name,
            description: def.description,
            outcome,
        });
    }

    Ok(results)
}

fn hooks_dir(root: &Path) -> Option<PathBuf> {
    resolve_hooks_dir_via_git(root).or_else(|| {
        let git_dirs = tempyr_core::project::resolve_git_dirs(root)?;
        Some(git_dirs.common_dir.join("hooks"))
    })
}

fn resolve_hooks_dir_via_git(root: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--git-path", "hooks"])
        .current_dir(root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    hooks_dir_from_git_output(root, &output.stdout)
}

fn hooks_dir_from_git_output(root: &Path, stdout: &[u8]) -> Option<PathBuf> {
    let path = std::str::from_utf8(stdout).ok()?.trim();
    if path.is_empty() {
        return None;
    }

    let path = PathBuf::from(path);
    if path.is_absolute() {
        Some(path)
    } else {
        Some(root.join(path))
    }
}

fn hook_status(path: &Path, managed_block: &str) -> anyhow::Result<HookStatus> {
    if !path.is_file() {
        return Ok(HookStatus::Missing);
    }

    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read hook {}", path.display()))?;
    let Some(status) = managed_block_status(&content, managed_block) else {
        return Ok(HookStatus::Stale);
    };

    if !status.is_current() {
        return Ok(HookStatus::Stale);
    }

    if !hook_is_executable(path)? {
        return Ok(HookStatus::Stale);
    }

    Ok(HookStatus::UpToDate)
}

fn merge_hook_content(existing: Option<&str>, managed_block: &str) -> (String, WriteOutcome) {
    match existing {
        None => (format!("{SHEBANG}{managed_block}"), WriteOutcome::Created),
        Some(content) => merge_existing_hook_content(content, managed_block),
    }
}

fn managed_block_range(content: &str) -> Option<(usize, usize)> {
    // Try the current markers first, then fall back to the legacy
    // pair so an upgrade replaces the old block in place. A future
    // marker rename can extend this list the same way.
    for (start_marker, end_marker_str) in [
        (MANAGED_START, MANAGED_END),
        (LEGACY_MANAGED_START, LEGACY_MANAGED_END),
    ] {
        let Some(start) = content.find(start_marker) else {
            continue;
        };
        let Some(end_offset) = content[start..].find(end_marker_str) else {
            continue;
        };
        let end = start + end_offset + end_marker_str.len();
        let end = if content[end..].starts_with("\r\n") {
            end + 2
        } else if content[end..].starts_with('\n') {
            end + 1
        } else {
            end
        };
        return Some((start, end));
    }
    None
}

fn merge_existing_hook_content(content: &str, managed_block: &str) -> (String, WriteOutcome) {
    if let Some(status) = managed_block_status(content, managed_block) {
        let (start, end) = status.range;

        if status.is_current() {
            return (content.to_string(), WriteOutcome::Unchanged);
        }

        if status.reachable && !status.has_additional_blocks {
            let mut merged = String::with_capacity(content.len() + managed_block.len());
            merged.push_str(&content[..start]);
            merged.push_str(managed_block);
            merged.push_str(&content[end..]);
            return (merged, WriteOutcome::Updated);
        }

        let base = strip_all_managed_blocks(content);
        return (
            insert_managed_block(&base, managed_block),
            WriteOutcome::Updated,
        );
    }

    (
        insert_managed_block(content, managed_block),
        WriteOutcome::Merged,
    )
}

fn strip_all_managed_blocks(content: &str) -> String {
    let mut stripped = String::with_capacity(content.len());
    let mut remaining = content;

    while let Some((start, end)) = managed_block_range(remaining) {
        stripped.push_str(&remaining[..start]);
        remaining = &remaining[end..];
    }

    stripped.push_str(remaining);
    stripped
}

fn insert_managed_block(content: &str, managed_block: &str) -> String {
    if let Some(offset) = terminal_control_offset(content) {
        let before = trim_trailing_line_endings(&content[..offset]);
        let after = &content[offset..];

        let mut merged =
            String::with_capacity(before.len() + managed_block.len() + after.len() + 2);
        if !before.is_empty() {
            merged.push_str(before);
            merged.push('\n');
            merged.push('\n');
        }
        merged.push_str(managed_block);
        merged.push_str(after);
        return merged;
    }

    let trimmed = trim_trailing_line_endings(content);
    let mut merged = String::with_capacity(trimmed.len() + managed_block.len() + 2);
    if !trimmed.is_empty() {
        merged.push_str(trimmed);
        merged.push('\n');
        merged.push('\n');
    }
    merged.push_str(managed_block);
    merged
}

fn trim_trailing_line_endings(content: &str) -> &str {
    content.trim_end_matches(['\r', '\n'])
}

fn managed_block_is_reachable(content: &str, block_start: usize) -> bool {
    terminal_control_offset(content).is_none_or(|offset| offset >= block_start)
}

struct ManagedBlockStatus {
    range: (usize, usize),
    is_exact_match: bool,
    has_additional_blocks: bool,
    reachable: bool,
}

impl ManagedBlockStatus {
    fn is_current(&self) -> bool {
        self.is_exact_match && !self.has_additional_blocks && self.reachable
    }
}

fn managed_block_status(content: &str, managed_block: &str) -> Option<ManagedBlockStatus> {
    let (start, end) = managed_block_range(content)?;
    let extracted = &content[start..end];
    // Detect *any* additional managed block — current or legacy
    // markers — past this one. Without the legacy check, an upgrade
    // from a hook that already has both markers would treat the
    // first as singular and miss the second.
    let has_additional_blocks =
        content[end..].contains(MANAGED_START) || content[end..].contains(LEGACY_MANAGED_START);

    Some(ManagedBlockStatus {
        range: (start, end),
        is_exact_match: extracted == managed_block,
        has_additional_blocks,
        reachable: managed_block_is_reachable(content, start),
    })
}

fn terminal_control_offset(content: &str) -> Option<usize> {
    let mut offset = 0;
    for line in content.split_inclusive('\n') {
        let line_content = line.trim_end_matches(['\r', '\n']);
        if is_terminal_control_line(line_content) {
            return Some(offset);
        }
        offset += line.len();
    }

    if offset < content.len() && is_terminal_control_line(&content[offset..]) {
        return Some(offset);
    }

    None
}

fn is_terminal_control_line(line: &str) -> bool {
    let line = line.trim_end();
    if line.is_empty() || line.starts_with('#') {
        return false;
    }

    if matches!(line.as_bytes().first(), Some(b' ' | b'\t')) {
        return false;
    }

    ["exit", "exec", "return"]
        .into_iter()
        .any(|keyword| matches_shell_keyword(line, keyword))
}

fn matches_shell_keyword(line: &str, keyword: &str) -> bool {
    let Some(rest) = line.strip_prefix(keyword) else {
        return false;
    };

    rest.is_empty()
        || matches!(
            rest.chars().next(),
            Some(' ' | '\t' | '\r' | '\n' | ';' | '#')
        )
}

fn render_managed_block(def: &GitHookDef) -> String {
    let body = def.body;
    let hook_name = def.name;
    format!(
        "{MANAGED_START}\n# tempyr version: {TEMPYR_VERSION}\n# hook: {hook_name}\nTEMPYR_BIN=\"${{TEMPYR_BIN:-}}\"\n\
run_tempyr() {{\n\
  if [ -n \"$TEMPYR_BIN\" ]; then\n\
    if [ -x \"$TEMPYR_BIN\" ]; then\n\
      \"$TEMPYR_BIN\" \"$@\"\n\
      return $?\n\
    fi\n\
    if command -v \"$TEMPYR_BIN\" >/dev/null 2>&1; then\n\
      \"$TEMPYR_BIN\" \"$@\"\n\
      return $?\n\
    fi\n\
  fi\n\
\n\
  for candidate in ./target/debug/tempyr ./target/debug/tempyr.exe ./target/release/tempyr ./target/release/tempyr.exe; do\n\
    if [ -x \"$candidate\" ]; then\n\
      \"$candidate\" \"$@\"\n\
      return $?\n\
    fi\n\
  done\n\
\n\
  if command -v tempyr >/dev/null 2>&1; then\n\
    tempyr \"$@\"\n\
    return $?\n\
  fi\n\
\n\
  return 127\n\
}}\n\
if [ -d .tempyr ] || [ -f .tempyr-redirect ]; then\n\
{body}\n\
fi\n\
{MANAGED_END}\n"
    )
}

#[cfg(unix)]
fn ensure_hook_executable(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let metadata =
        fs::metadata(path).with_context(|| format!("Failed to stat hook {}", path.display()))?;
    let mut perms = metadata.permissions();
    perms.set_mode(perms.mode() | 0o100);
    fs::set_permissions(path, perms)
        .with_context(|| format!("Failed to chmod hook {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn ensure_hook_executable(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn hook_is_executable(path: &Path) -> anyhow::Result<bool> {
    use std::os::unix::fs::PermissionsExt;

    let metadata =
        fs::metadata(path).with_context(|| format!("Failed to stat hook {}", path.display()))?;
    Ok(metadata.permissions().mode() & 0o100 != 0)
}

#[cfg(not(unix))]
fn hook_is_executable(_path: &Path) -> anyhow::Result<bool> {
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn merge_hook_content_creates_new_hook() {
        let managed = render_managed_block(&GIT_HOOKS[0]);

        let (content, outcome) = merge_hook_content(None, &managed);

        assert_eq!(outcome, WriteOutcome::Created);
        assert!(content.starts_with(SHEBANG));
        assert!(content.contains(MANAGED_START));
    }

    #[test]
    fn merge_hook_content_appends_to_existing_user_hook() {
        let managed = render_managed_block(&GIT_HOOKS[0]);
        let existing = "#!/bin/sh\necho user-hook\n";

        let (content, outcome) = merge_hook_content(Some(existing), &managed);

        assert_eq!(outcome, WriteOutcome::Merged);
        assert!(content.contains("echo user-hook"));
        assert!(content.contains(MANAGED_START));
    }

    #[test]
    fn merge_hook_content_inserts_before_terminal_control_flow() {
        let managed = render_managed_block(&GIT_HOOKS[0]);
        let existing = "#!/bin/sh\necho before\nexit 0\n";

        let (content, outcome) = merge_hook_content(Some(existing), &managed);

        assert_eq!(outcome, WriteOutcome::Merged);
        assert!(content.contains("echo before\n\n# >>> tempyr managed hook >>>"));
        assert!(content.contains(&format!("{MANAGED_END}\nexit 0\n")));
    }

    #[test]
    fn merge_hook_content_ignores_indented_control_flow() {
        let managed = render_managed_block(&GIT_HOOKS[0]);
        let existing = "#!/bin/sh\nif some_check; then\n  exit 0\nfi\n";

        let (content, outcome) = merge_hook_content(Some(existing), &managed);

        assert_eq!(outcome, WriteOutcome::Merged);
        assert!(content.contains("fi\n\n# >>> tempyr managed hook >>>"));
        assert!(!content.contains("then\n\n# >>> tempyr managed hook >>>"));
    }

    #[test]
    fn merge_hook_content_replaces_stale_managed_block() {
        let stale = format!("{MANAGED_START}\nold\n{MANAGED_END}\n");
        let existing = format!("#!/bin/sh\n{stale}echo after\n");
        let managed = render_managed_block(&GIT_HOOKS[0]);

        let (content, outcome) = merge_hook_content(Some(&existing), &managed);

        assert_eq!(outcome, WriteOutcome::Updated);
        assert!(content.contains("echo after"));
        assert!(content.contains("TEMPYR_BIN="));
        assert!(!content.contains("\nold\n"));
    }

    #[test]
    fn merge_hook_content_repositions_unreachable_managed_block() {
        let managed = render_managed_block(&GIT_HOOKS[0]);
        let existing = format!("#!/bin/sh\nexit 0\n\n{managed}");

        let (content, outcome) = merge_hook_content(Some(&existing), &managed);

        assert_eq!(outcome, WriteOutcome::Updated);
        assert_eq!(content.matches(MANAGED_START).count(), 1);
        assert!(content.contains(&format!("{MANAGED_END}\nexit 0\n")));
    }

    #[test]
    fn merge_hook_content_collapses_duplicate_managed_blocks() {
        let managed = render_managed_block(&GIT_HOOKS[0]);
        let existing = format!("#!/bin/sh\n{managed}\necho after\n{managed}");

        let (content, outcome) = merge_hook_content(Some(&existing), &managed);

        assert_eq!(outcome, WriteOutcome::Updated);
        assert_eq!(content.matches(MANAGED_START).count(), 1);
        assert!(content.contains("echo after"));
    }

    #[test]
    fn merge_hook_content_replaces_stale_first_block_when_later_block_is_current() {
        let managed = render_managed_block(&GIT_HOOKS[0]);
        let stale = format!("{MANAGED_START}\nold\n{MANAGED_END}\n");
        let existing = format!("#!/bin/sh\n{stale}\necho after\n{managed}");

        let (content, outcome) = merge_hook_content(Some(&existing), &managed);

        assert_eq!(outcome, WriteOutcome::Updated);
        assert_eq!(content.matches(MANAGED_START).count(), 1);
        assert!(content.contains("echo after"));
        assert!(!content.contains("\nold\n"));
    }

    #[test]
    fn hook_status_treats_existing_user_hook_as_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("post-checkout");
        fs::write(&path, "#!/bin/sh\necho user-hook\n").unwrap();

        let managed = render_managed_block(&GIT_HOOKS[0]);

        assert_eq!(hook_status(&path, &managed).unwrap(), HookStatus::Stale);
    }

    #[test]
    fn hook_status_ignores_indented_control_flow_before_managed_block() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("post-checkout");
        let managed = render_managed_block(&GIT_HOOKS[0]);
        let hook = format!("#!/bin/sh\nif some_check; then\n  exit 0\nfi\n\n{managed}");
        fs::write(&path, hook).unwrap();

        assert_eq!(hook_status(&path, &managed).unwrap(), HookStatus::UpToDate);
    }

    #[test]
    fn hook_status_treats_unreachable_managed_hook_as_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("post-checkout");
        let managed = render_managed_block(&GIT_HOOKS[0]);
        let hook = format!("#!/bin/sh\nexit 0\n\n{managed}");
        fs::write(&path, hook).unwrap();

        assert_eq!(hook_status(&path, &managed).unwrap(), HookStatus::Stale);
    }

    #[test]
    fn hook_status_treats_duplicate_managed_blocks_as_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("post-checkout");
        let managed = render_managed_block(&GIT_HOOKS[0]);
        let hook = format!("#!/bin/sh\n{managed}\n{managed}");
        fs::write(&path, hook).unwrap();

        assert_eq!(hook_status(&path, &managed).unwrap(), HookStatus::Stale);
    }

    #[cfg(unix)]
    #[test]
    fn hook_status_treats_non_executable_managed_hook_as_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("post-checkout");
        let managed = render_managed_block(&GIT_HOOKS[0]);
        fs::write(&path, &managed).unwrap();

        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o644);
        fs::set_permissions(&path, perms).unwrap();

        assert_eq!(hook_status(&path, &managed).unwrap(), HookStatus::Stale);
    }

    #[cfg(unix)]
    #[test]
    fn hook_status_requires_owner_execute_bit() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("post-checkout");
        let managed = render_managed_block(&GIT_HOOKS[0]);
        fs::write(&path, &managed).unwrap();

        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o055);
        fs::set_permissions(&path, perms).unwrap();

        assert_eq!(hook_status(&path, &managed).unwrap(), HookStatus::Stale);
        assert!(!hook_is_executable(&path).unwrap());
    }

    #[test]
    fn hooks_dir_from_git_output_resolves_relative_paths_against_root() {
        let root = Path::new("/repo/root");

        let path = hooks_dir_from_git_output(root, b".githooks\n").unwrap();

        assert_eq!(path, root.join(".githooks"));
    }

    #[test]
    fn hooks_dir_from_git_output_preserves_absolute_paths() {
        let root = Path::new("/repo/root");

        let path = hooks_dir_from_git_output(root, b"/shared/hooks\n").unwrap();

        assert_eq!(path, PathBuf::from("/shared/hooks"));
    }

    #[cfg(unix)]
    #[test]
    fn ensure_hook_executable_only_adds_owner_execute_bit() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("post-checkout");
        fs::write(&path, "#!/bin/sh\n").unwrap();

        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o600);
        fs::set_permissions(&path, perms).unwrap();

        ensure_hook_executable(&path).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn managed_block_is_worktree_agnostic() {
        let managed = render_managed_block(&GIT_HOOKS[0]);

        assert!(managed.contains("TEMPYR_BIN=\"${TEMPYR_BIN:-}\""));
        assert!(managed.contains("./target/debug/tempyr"));
        assert!(managed.contains("if [ -d .tempyr ] || [ -f .tempyr-redirect ]; then"));
        assert!(!managed.contains("exit 0"));
        assert!(!managed.contains("/tmp/tempyr"));
    }

    #[test]
    fn merge_hook_content_upgrades_legacy_marker_in_place() {
        // Earlier versions used `tempyr managed index warmup` markers.
        // An upgrade must REPLACE the legacy block with the current
        // one, NOT append a second managed section alongside it.
        let legacy_block = format!(
            "{LEGACY_MANAGED_START}\n# tempyr version: 0.1.0\nold body line\n{LEGACY_MANAGED_END}\n"
        );
        let existing = format!("#!/bin/sh\n{legacy_block}echo after\n");
        let managed = render_managed_block(&GIT_HOOKS[0]);

        let (content, outcome) = merge_hook_content(Some(&existing), &managed);

        assert_eq!(outcome, WriteOutcome::Updated);
        // Exactly one managed-block start marker (the current one),
        // and the legacy markers should be gone entirely.
        assert_eq!(content.matches(MANAGED_START).count(), 1);
        assert!(!content.contains(LEGACY_MANAGED_START));
        assert!(!content.contains(LEGACY_MANAGED_END));
        assert!(content.contains("echo after"));
    }

    #[test]
    fn managed_block_body_varies_per_hook() {
        // Per-hook body parameterization: the index-warmup hooks
        // (post-checkout, post-merge) call `index update` while the
        // pre-commit hook calls `journal lint`. Without parameterization
        // this test would fail because all hooks would render the same
        // body.
        let post_checkout = GIT_HOOKS
            .iter()
            .find(|h| h.name == "post-checkout")
            .expect("post-checkout hook def");
        let pre_commit = GIT_HOOKS
            .iter()
            .find(|h| h.name == "pre-commit")
            .expect("pre-commit hook def");

        let warmup = render_managed_block(post_checkout);
        let lint = render_managed_block(pre_commit);

        assert!(warmup.contains("run_tempyr index update"));
        assert!(!warmup.contains("run_tempyr journal lint"));
        assert!(lint.contains("run_tempyr journal lint"));
        assert!(!lint.contains("run_tempyr index update"));
        // Both render with hook-name annotation in the header so
        // the output is self-describing.
        assert!(warmup.contains("# hook: post-checkout"));
        assert!(lint.contains("# hook: pre-commit"));
    }
}
