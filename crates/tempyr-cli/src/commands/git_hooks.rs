use anyhow::Context;
use std::fs;
use std::path::{Path, PathBuf};

use super::managed::WriteOutcome;

const TEMPYR_VERSION: &str = env!("CARGO_PKG_VERSION");
const MANAGED_START: &str = "# >>> tempyr managed index warmup >>>";
const MANAGED_END: &str = "# <<< tempyr managed index warmup <<<";
const SHEBANG: &str = "#!/bin/sh\n";

struct GitHookDef {
    name: &'static str,
    description: &'static str,
}

const GIT_HOOKS: &[GitHookDef] = &[
    GitHookDef {
        name: "post-checkout",
        description: "warm index after checkout and new worktree creation",
    },
    GitHookDef {
        name: "post-merge",
        description: "refresh index after merges",
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

    let managed_block = render_managed_block();

    let mut reports = Vec::new();
    for def in GIT_HOOKS {
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

    let managed_block = render_managed_block();

    let mut results = Vec::new();
    for def in GIT_HOOKS {
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
    let git_dirs = tempyr_core::project::resolve_git_dirs(root)?;
    Some(git_dirs.common_dir.join("hooks"))
}

fn hook_status(path: &Path, managed_block: &str) -> anyhow::Result<HookStatus> {
    if !path.is_file() {
        return Ok(HookStatus::Missing);
    }

    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read hook {}", path.display()))?;
    if content.contains(managed_block) {
        if !hook_is_executable(path)? {
            return Ok(HookStatus::Stale);
        }
        return Ok(HookStatus::UpToDate);
    }
    Ok(HookStatus::Stale)
}

fn merge_hook_content(existing: Option<&str>, managed_block: &str) -> (String, WriteOutcome) {
    match existing {
        None => (format!("{SHEBANG}{managed_block}"), WriteOutcome::Created),
        Some(content) if content.contains(managed_block) => {
            (content.to_string(), WriteOutcome::Unchanged)
        }
        Some(content) => {
            if let Some((start, end)) = managed_block_range(content) {
                let mut merged = String::with_capacity(content.len() + managed_block.len());
                merged.push_str(&content[..start]);
                merged.push_str(managed_block);
                merged.push_str(&content[end..]);
                return (merged, WriteOutcome::Updated);
            }

            let mut merged = content.to_string();
            if !merged.ends_with('\n') {
                merged.push('\n');
            }
            if !merged.ends_with("\n\n") {
                merged.push('\n');
            }
            merged.push_str(managed_block);
            (merged, WriteOutcome::Merged)
        }
    }
}

fn managed_block_range(content: &str) -> Option<(usize, usize)> {
    let start = content.find(MANAGED_START)?;
    let end_marker = content[start..].find(MANAGED_END)?;
    let end = start + end_marker + MANAGED_END.len();
    let end = if content[end..].starts_with("\r\n") {
        end + 2
    } else if content[end..].starts_with('\n') {
        end + 1
    } else {
        end
    };
    Some((start, end))
}

fn render_managed_block() -> String {
    format!(
        "{MANAGED_START}\n# tempyr version: {TEMPYR_VERSION}\nTEMPYR_BIN=\"${{TEMPYR_BIN:-}}\"\n\
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
if [ ! -d .tempyr ] && [ ! -f .tempyr-redirect ]; then\n  exit 0\nfi\n\
run_tempyr index update --json --skip-embeddings >/dev/null 2>&1 || true\n\
{MANAGED_END}\n"
    )
}

#[cfg(unix)]
fn ensure_hook_executable(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let metadata =
        fs::metadata(path).with_context(|| format!("Failed to stat hook {}", path.display()))?;
    let mut perms = metadata.permissions();
    perms.set_mode(perms.mode() | 0o755);
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
    Ok(metadata.permissions().mode() & 0o111 != 0)
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
        let managed = render_managed_block();

        let (content, outcome) = merge_hook_content(None, &managed);

        assert_eq!(outcome, WriteOutcome::Created);
        assert!(content.starts_with(SHEBANG));
        assert!(content.contains(MANAGED_START));
    }

    #[test]
    fn merge_hook_content_appends_to_existing_user_hook() {
        let managed = render_managed_block();
        let existing = "#!/bin/sh\necho user-hook\n";

        let (content, outcome) = merge_hook_content(Some(existing), &managed);

        assert_eq!(outcome, WriteOutcome::Merged);
        assert!(content.contains("echo user-hook"));
        assert!(content.contains(MANAGED_START));
    }

    #[test]
    fn merge_hook_content_replaces_stale_managed_block() {
        let stale = format!("{MANAGED_START}\nold\n{MANAGED_END}\n");
        let existing = format!("#!/bin/sh\n{stale}echo after\n");
        let managed = render_managed_block();

        let (content, outcome) = merge_hook_content(Some(&existing), &managed);

        assert_eq!(outcome, WriteOutcome::Updated);
        assert!(content.contains("echo after"));
        assert!(content.contains("TEMPYR_BIN="));
        assert!(!content.contains("\nold\n"));
    }

    #[test]
    fn hook_status_treats_existing_user_hook_as_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("post-checkout");
        fs::write(&path, "#!/bin/sh\necho user-hook\n").unwrap();

        let managed = render_managed_block();

        assert_eq!(hook_status(&path, &managed).unwrap(), HookStatus::Stale);
    }

    #[cfg(unix)]
    #[test]
    fn hook_status_treats_non_executable_managed_hook_as_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("post-checkout");
        let managed = render_managed_block();
        fs::write(&path, &managed).unwrap();

        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o644);
        fs::set_permissions(&path, perms).unwrap();

        assert_eq!(hook_status(&path, &managed).unwrap(), HookStatus::Stale);
    }

    #[test]
    fn managed_block_is_worktree_agnostic() {
        let managed = render_managed_block();

        assert!(managed.contains("TEMPYR_BIN=\"${TEMPYR_BIN:-}\""));
        assert!(managed.contains("./target/debug/tempyr"));
        assert!(!managed.contains("/tmp/tempyr"));
    }
}
