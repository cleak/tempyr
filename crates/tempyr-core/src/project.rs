//! Project root resolution with redirect support.
//!
//! Walks up from the current directory looking for `.tempyr/` or `.tempyr-redirect`.
//! A `.tempyr-redirect` file contains a path (relative or absolute) pointing to
//! the real tempyr project root. This lets you run tempyr commands from a working
//! project that stores its knowledge graph in a separate repository.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use blake3::Hasher;
use walkdir::WalkDir;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitDirs {
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheLayout {
    pub shared_root: PathBuf,
    pub worktree_root: PathBuf,
}

impl CacheLayout {
    pub fn active_index_path(&self) -> PathBuf {
        self.worktree_root.join("index.db")
    }

    pub fn active_snapshot_path(&self) -> PathBuf {
        self.worktree_root.join("snapshot-key.txt")
    }

    pub fn snapshot_index_path(&self, snapshot_key: &str) -> PathBuf {
        self.shared_root
            .join("snapshots")
            .join(snapshot_key)
            .join("index.db")
    }

    pub fn embeddings_dir(&self) -> PathBuf {
        self.shared_root.join("embeddings")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexLayout {
    pub cache: CacheLayout,
    pub snapshot_key: String,
    pub legacy_index_path: PathBuf,
}

impl IndexLayout {
    pub fn resolve(root: &Path, graph_dir: &Path, tempyr_dir: &Path) -> io::Result<Self> {
        Ok(Self {
            cache: cache_layout(root, tempyr_dir),
            snapshot_key: graph_snapshot_key(graph_dir, tempyr_dir)?,
            legacy_index_path: tempyr_dir.join("index.db"),
        })
    }

    pub fn active_index_path(&self) -> PathBuf {
        self.cache.active_index_path()
    }

    pub fn active_snapshot_path(&self) -> PathBuf {
        self.cache.active_snapshot_path()
    }

    pub fn shared_snapshot_index_path(&self) -> PathBuf {
        self.cache.snapshot_index_path(&self.snapshot_key)
    }

    pub fn current_index_path(&self) -> Option<PathBuf> {
        let shared = self.shared_snapshot_index_path();
        if shared.exists() {
            return Some(shared);
        }

        let active = self.active_index_path();
        if active.exists()
            && self.active_snapshot_key().as_deref() == Some(self.snapshot_key.as_str())
        {
            return Some(active);
        }

        if self.legacy_index_path.exists() {
            return Some(self.legacy_index_path.clone());
        }

        None
    }

    pub fn ensure_active_index_seeded(&self) -> io::Result<PathBuf> {
        let active = self.active_index_path();
        let snapshot_matches =
            self.active_snapshot_key().as_deref() == Some(self.snapshot_key.as_str());
        let needs_seed = !active.exists() || !snapshot_matches;

        if !needs_seed {
            return Ok(active);
        }

        if active.exists() {
            fs::remove_file(&active)?;
        }

        if let Some(parent) = active.parent() {
            fs::create_dir_all(parent)?;
        }

        let shared = self.shared_snapshot_index_path();
        if shared.exists() {
            fs::copy(&shared, &active)?;
            self.write_active_snapshot_key()?;
        } else if self.legacy_index_path.exists() {
            fs::copy(&self.legacy_index_path, &active)?;
        }

        Ok(active)
    }

    pub fn write_active_snapshot_key(&self) -> io::Result<()> {
        let path = self.active_snapshot_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, &self.snapshot_key)
    }

    pub fn publish_active_snapshot(&self) -> io::Result<PathBuf> {
        let active = self.active_index_path();
        let shared = self.shared_snapshot_index_path();
        if shared.exists() {
            return Ok(shared);
        }

        let parent = shared
            .parent()
            .ok_or_else(|| io::Error::other("Invalid shared snapshot path"))?;
        fs::create_dir_all(parent)?;

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let tmp = shared.with_extension(format!("db.tmp.{}.{}", std::process::id(), nonce));
        fs::copy(&active, &tmp)?;
        match fs::rename(&tmp, &shared) {
            Ok(()) => Ok(shared),
            Err(_) if shared.exists() => {
                let _ = fs::remove_file(&tmp);
                Ok(shared)
            }
            Err(err) => {
                let _ = fs::remove_file(&tmp);
                Err(err)
            }
        }
    }

    fn active_snapshot_key(&self) -> Option<String> {
        fs::read_to_string(self.active_snapshot_path())
            .ok()
            .map(|s| s.trim().to_string())
    }
}

/// Walk up the directory tree to find a tempyr project root.
///
/// Checks each directory for:
/// 1. `.tempyr-redirect` - a file whose first non-empty line is a path to the real project root
/// 2. `.tempyr/` - a directory indicating this is the project root
///
/// Redirect paths are resolved relative to the directory containing the redirect file.
/// Only one level of redirect is followed (no chaining).
pub fn find_project_root() -> Option<PathBuf> {
    find_project_root_from(std::env::current_dir().ok()?)
}

/// Same as [`find_project_root`] but starting from a given directory.
pub fn find_project_root_from(start: PathBuf) -> Option<PathBuf> {
    let mut dir = start;
    loop {
        // Check for redirect file first
        let redirect_path = dir.join(".tempyr-redirect");
        if redirect_path.is_file()
            && let Some(target) = read_redirect(&redirect_path, &dir)
            && target.join(".tempyr").is_dir()
        {
            return Some(target);
        }

        // Check for direct .tempyr/ directory
        if dir.join(".tempyr").is_dir() {
            return Some(dir);
        }

        if !dir.pop() {
            return None;
        }
    }
}

/// Read a `.tempyr-redirect` file and resolve the path it contains.
fn read_redirect(file: &Path, base_dir: &Path) -> Option<PathBuf> {
    let contents = std::fs::read_to_string(file).ok()?;
    let target = contents.lines().find(|l| !l.trim().is_empty())?.trim();

    if target.is_empty() {
        return None;
    }

    let path = PathBuf::from(target);
    let resolved = if path.is_absolute() {
        path
    } else {
        base_dir.join(path)
    };

    // Canonicalize to clean up ../ segments
    std::fs::canonicalize(resolved).ok()
}

/// Resolve the Git administration directories for the given worktree root.
///
/// For a normal checkout, both fields will point at the repo's `.git/` directory.
/// For a linked worktree, `git_dir` points at the worktree-private admin dir and
/// `common_dir` points at the shared repository admin dir.
pub fn resolve_git_dirs(root: &Path) -> Option<GitDirs> {
    let dot_git = root.join(".git");
    let git_dir = if dot_git.is_dir() {
        fs::canonicalize(dot_git).ok()?
    } else if dot_git.is_file() {
        let raw = fs::read_to_string(&dot_git).ok()?;
        let target = raw
            .lines()
            .find_map(|line| line.strip_prefix("gitdir:").map(str::trim))?;
        let path = PathBuf::from(target);
        let resolved = if path.is_absolute() {
            path
        } else {
            root.join(path)
        };
        fs::canonicalize(resolved).ok()?
    } else {
        return None;
    };

    let common_dir = read_commondir(&git_dir).unwrap_or_else(|| git_dir.clone());
    Some(GitDirs {
        git_dir,
        common_dir,
    })
}

pub fn cache_layout(root: &Path, tempyr_dir: &Path) -> CacheLayout {
    if let Some(git_dirs) = resolve_git_dirs(root) {
        let worktree_id = short_path_hash(&git_dirs.git_dir);
        let shared_root = git_dirs.common_dir.join("tempyr");
        let worktree_root = shared_root.join("worktrees").join(worktree_id);
        CacheLayout {
            shared_root,
            worktree_root,
        }
    } else {
        let shared_root = tempyr_dir.join("cache");
        let worktree_root = shared_root.join("worktrees").join("default");
        CacheLayout {
            shared_root,
            worktree_root,
        }
    }
}

/// Hash the current graph snapshot so identical worktrees can share immutable indices.
///
/// The hash includes:
/// - all markdown files under `graph/`
/// - `.tempyr/schema.toml`
/// - a format/version marker for cache invalidation
pub fn graph_snapshot_key(graph_dir: &Path, tempyr_dir: &Path) -> io::Result<String> {
    let mut files = Vec::new();

    if graph_dir.exists() {
        for entry in WalkDir::new(graph_dir)
            .min_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|ext| ext == "md") {
                let rel = path.strip_prefix(graph_dir).unwrap_or(path).to_path_buf();
                files.push((
                    format!("graph/{}", rel.to_string_lossy().replace('\\', "/")),
                    path.to_path_buf(),
                ));
            }
        }
    }

    let schema_path = tempyr_dir.join("schema.toml");
    if schema_path.is_file() {
        files.push(("schema.toml".to_string(), schema_path));
    }

    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut hasher = Hasher::new();
    hasher.update(b"tempyr-snapshot-v1\0");

    for (label, path) in files {
        hasher.update(label.as_bytes());
        hasher.update(b"\0");
        hasher.update(&fs::read(path)?);
        hasher.update(b"\0");
    }

    let hex = hasher.finalize().to_hex().to_string();
    Ok(hex[..16].to_string())
}

fn read_commondir(git_dir: &Path) -> Option<PathBuf> {
    let commondir_path = git_dir.join("commondir");
    if !commondir_path.is_file() {
        return None;
    }

    let raw = fs::read_to_string(commondir_path).ok()?;
    let target = raw.lines().find(|line| !line.trim().is_empty())?.trim();
    let path = PathBuf::from(target);
    let resolved = if path.is_absolute() {
        path
    } else {
        git_dir.join(path)
    };
    fs::canonicalize(resolved).ok()
}

fn short_path_hash(path: &Path) -> String {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut hasher = Hasher::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    let hex = hasher.finalize().to_hex().to_string();
    hex[..12].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn finds_direct_project_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        fs::create_dir(root.join(".tempyr")).unwrap();

        let found = find_project_root_from(root.clone());
        assert_eq!(found, Some(root));
    }

    #[test]
    fn finds_root_from_subdirectory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        fs::create_dir(root.join(".tempyr")).unwrap();
        let sub = root.join("src").join("deep");
        fs::create_dir_all(&sub).unwrap();

        let found = find_project_root_from(sub);
        assert_eq!(found, Some(root));
    }

    #[test]
    fn follows_redirect_file() {
        let tmp = tempfile::tempdir().unwrap();

        // Create the real project
        let real_root = tmp.path().join("knowledge-base");
        fs::create_dir_all(real_root.join(".tempyr")).unwrap();
        fs::create_dir(real_root.join("graph")).unwrap();

        // Create the working project with a redirect
        let work_root = tmp.path().join("main-project");
        fs::create_dir(&work_root).unwrap();
        fs::write(work_root.join(".tempyr-redirect"), "../knowledge-base\n").unwrap();

        let found = find_project_root_from(work_root);
        let expected = fs::canonicalize(&real_root).unwrap();
        assert_eq!(found, Some(expected));
    }

    #[test]
    fn redirect_to_missing_project_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();

        // Redirect points to a directory without .tempyr/
        let target = root.join("not-a-project");
        fs::create_dir(&target).unwrap();
        fs::write(root.join(".tempyr-redirect"), "not-a-project\n").unwrap();

        let found = find_project_root_from(root);
        assert_eq!(found, None);
    }

    #[test]
    fn redirect_with_absolute_path() {
        let tmp = tempfile::tempdir().unwrap();

        let real_root = tmp.path().join("kb");
        fs::create_dir_all(real_root.join(".tempyr")).unwrap();

        let work_root = tmp.path().join("app");
        fs::create_dir(&work_root).unwrap();
        fs::write(
            work_root.join(".tempyr-redirect"),
            real_root.to_str().unwrap(),
        )
        .unwrap();

        let found = find_project_root_from(work_root);
        let expected = fs::canonicalize(&real_root).unwrap();
        assert_eq!(found, Some(expected));
    }

    #[test]
    fn resolves_git_dirs_for_regular_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        fs::create_dir(root.join(".git")).unwrap();

        let dirs = resolve_git_dirs(&root).unwrap();
        assert_eq!(dirs.git_dir, fs::canonicalize(root.join(".git")).unwrap());
        assert_eq!(dirs.common_dir, dirs.git_dir);
    }

    #[test]
    fn resolves_git_dirs_for_linked_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let worktree = tmp.path().join("wt");
        let common = repo.join(".git");
        let private = common.join("worktrees").join("feature");

        fs::create_dir_all(&private).unwrap();
        fs::create_dir(&worktree).unwrap();
        fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", private.display()),
        )
        .unwrap();
        fs::write(private.join("commondir"), "../..\n").unwrap();

        let dirs = resolve_git_dirs(&worktree).unwrap();
        assert_eq!(dirs.git_dir, fs::canonicalize(&private).unwrap());
        assert_eq!(dirs.common_dir, fs::canonicalize(&common).unwrap());
    }

    #[test]
    fn snapshot_key_changes_when_graph_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let graph_dir = root.join("graph");
        let features_dir = graph_dir.join("features");
        let tempyr_dir = root.join(".tempyr");
        fs::create_dir_all(&features_dir).unwrap();
        fs::create_dir_all(&tempyr_dir).unwrap();
        fs::write(tempyr_dir.join("schema.toml"), "name = 'x'\n").unwrap();
        fs::write(features_dir.join("a.md"), "# A\n").unwrap();

        let first = graph_snapshot_key(&graph_dir, &tempyr_dir).unwrap();
        fs::write(features_dir.join("a.md"), "# A changed\n").unwrap();
        let second = graph_snapshot_key(&graph_dir, &tempyr_dir).unwrap();

        assert_ne!(first, second);
    }

    #[test]
    fn stale_active_index_is_reseeded_from_shared_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let graph_dir = root.join("graph");
        let features_dir = graph_dir.join("features");
        let tempyr_dir = root.join(".tempyr");
        fs::create_dir_all(&features_dir).unwrap();
        fs::create_dir_all(&tempyr_dir).unwrap();
        fs::write(tempyr_dir.join("schema.toml"), "name = 'x'\n").unwrap();
        fs::write(features_dir.join("a.md"), "# A\n").unwrap();

        let layout = IndexLayout::resolve(root, &graph_dir, &tempyr_dir).unwrap();
        let active = layout.active_index_path();
        let shared = layout.shared_snapshot_index_path();

        fs::create_dir_all(active.parent().unwrap()).unwrap();
        fs::create_dir_all(shared.parent().unwrap()).unwrap();
        fs::write(&active, "stale-active-index").unwrap();
        fs::write(layout.active_snapshot_path(), "outdated-snapshot").unwrap();
        fs::write(&shared, "shared-snapshot-index").unwrap();

        layout.ensure_active_index_seeded().unwrap();

        assert_eq!(
            fs::read_to_string(&active).unwrap(),
            "shared-snapshot-index"
        );
        assert_eq!(
            fs::read_to_string(layout.active_snapshot_path())
                .unwrap()
                .trim(),
            layout.snapshot_key
        );
    }
}
