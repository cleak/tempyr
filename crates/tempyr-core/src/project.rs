//! Project root resolution with redirect support.
//!
//! Walks up from the current directory looking for `.tempyr/` or `.tempyr-redirect`.
//! A `.tempyr-redirect` file contains a path (relative or absolute) pointing to
//! the real tempyr project root. This lets you run tempyr commands from a working
//! project that stores its knowledge graph in a separate repository.

use std::cell::RefCell;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use blake3::Hasher;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRoots {
    pub anchor_root: PathBuf,
    pub project_root: PathBuf,
}

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

const SNAPSHOT_KEY_CACHE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SnapshotMetadata {
    files: Vec<SnapshotMetadataEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SnapshotMetadataEntry {
    label: String,
    size: u64,
    modified_secs: u64,
    modified_nanos: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SnapshotKeyCacheEntry {
    version: u32,
    snapshot_key: String,
    metadata: SnapshotMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SnapshotInput {
    label: String,
    path: PathBuf,
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

#[derive(Debug)]
pub struct IndexLayout {
    pub cache: CacheLayout,
    snapshot_key: RefCell<Option<String>>,
    graph_dir: PathBuf,
    tempyr_dir: PathBuf,
    pub legacy_index_path: PathBuf,
}

struct StagedActiveIndex {
    active_path: PathBuf,
    staged_path: PathBuf,
}

impl StagedActiveIndex {
    fn path(&self) -> &Path {
        &self.staged_path
    }

    fn commit(self) -> io::Result<()> {
        if !self.staged_path.exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "Staged index was not created",
            ));
        }
        if let Some(parent) = self.active_path.parent() {
            fs::create_dir_all(parent)?;
        }
        if self.active_path.exists() {
            fs::remove_file(&self.active_path)?;
        }
        fs::rename(&self.staged_path, &self.active_path)?;
        Ok(())
    }
}

impl Drop for StagedActiveIndex {
    fn drop(&mut self) {
        cleanup_sqlite_artifacts(&self.staged_path);
    }
}

impl IndexLayout {
    pub fn resolve(root: &Path, graph_dir: &Path, tempyr_dir: &Path) -> io::Result<Self> {
        Ok(Self {
            cache: cache_layout(root, tempyr_dir),
            snapshot_key: RefCell::new(None),
            graph_dir: graph_dir.to_path_buf(),
            tempyr_dir: tempyr_dir.to_path_buf(),
            legacy_index_path: tempyr_dir.join("index.db"),
        })
    }

    pub fn snapshot_key(&self) -> io::Result<String> {
        if let Some(snapshot_key) = self.snapshot_key.borrow().clone() {
            return Ok(snapshot_key);
        }

        let inputs = collect_snapshot_inputs(&self.graph_dir, &self.tempyr_dir);
        let metadata = snapshot_metadata(&inputs)?;
        if let Some(cached) = read_snapshot_key_cache(&self.snapshot_key_cache_path())?
            && cached.version == SNAPSHOT_KEY_CACHE_VERSION
            && cached.metadata == metadata
        {
            *self.snapshot_key.borrow_mut() = Some(cached.snapshot_key.clone());
            return Ok(cached.snapshot_key);
        }

        let snapshot_key = snapshot_key_from_inputs(&inputs)?;
        let _ = write_snapshot_key_cache(
            &self.snapshot_key_cache_path(),
            &SnapshotKeyCacheEntry {
                version: SNAPSHOT_KEY_CACHE_VERSION,
                snapshot_key: snapshot_key.clone(),
                metadata,
            },
        );
        *self.snapshot_key.borrow_mut() = Some(snapshot_key.clone());
        Ok(snapshot_key)
    }

    pub fn set_snapshot_key(&self, snapshot_key: impl Into<String>) {
        *self.snapshot_key.borrow_mut() = Some(snapshot_key.into());
    }

    pub fn active_index_path(&self) -> PathBuf {
        self.cache.active_index_path()
    }

    pub fn active_snapshot_path(&self) -> PathBuf {
        self.cache.active_snapshot_path()
    }

    fn snapshot_key_cache_path(&self) -> PathBuf {
        self.cache.worktree_root.join("snapshot-key-cache.json")
    }

    pub fn shared_snapshot_index_path(&self) -> io::Result<PathBuf> {
        Ok(self.cache.snapshot_index_path(&self.snapshot_key()?))
    }

    pub fn current_index_path(&self) -> io::Result<Option<PathBuf>> {
        let snapshot_key = self.snapshot_key()?;
        let shared = self.cache.snapshot_index_path(&snapshot_key);
        if shared.exists() {
            return Ok(Some(shared));
        }

        let active = self.active_index_path();
        if active.exists() && self.active_snapshot_key().as_deref() == Some(snapshot_key.as_str()) {
            return Ok(Some(active));
        }

        if self.legacy_index_path.exists() {
            return Ok(Some(self.legacy_index_path.clone()));
        }

        Ok(None)
    }

    pub fn ensure_active_index_seeded(&self) -> io::Result<PathBuf> {
        let snapshot_key = self.snapshot_key()?;
        let active = self.active_index_path();
        let snapshot_matches = self.active_snapshot_key().as_deref() == Some(snapshot_key.as_str());
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

        let shared = self.cache.snapshot_index_path(&snapshot_key);
        if shared.exists() {
            fs::copy(&shared, &active)?;
            self.write_active_snapshot_key()?;
        } else if self.legacy_index_path.exists() {
            fs::copy(&self.legacy_index_path, &active)?;
        }

        Ok(active)
    }

    /// Run a staged index refresh and only replace the active index after the
    /// updater has finished successfully.
    pub fn update_active_index_atomically<F>(&self, updater: F) -> io::Result<()>
    where
        F: FnOnce(&Path) -> io::Result<()>,
    {
        let staged = self.stage_active_index()?;
        updater(staged.path())?;
        staged.commit()
    }

    pub fn write_active_snapshot_key(&self) -> io::Result<()> {
        let path = self.active_snapshot_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let snapshot_key = self.snapshot_key()?;
        fs::write(path, snapshot_key)
    }

    pub fn publish_active_snapshot(&self) -> io::Result<PathBuf> {
        let active = self.active_index_path();
        let shared = self.shared_snapshot_index_path()?;
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
        match fs::hard_link(&tmp, &shared) {
            Ok(()) => {
                let _ = fs::remove_file(&tmp);
                Ok(shared)
            }
            Err(err) => {
                if err.kind() == io::ErrorKind::AlreadyExists {
                    let _ = fs::remove_file(&tmp);
                    return Ok(shared);
                }
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

    fn stage_active_index(&self) -> io::Result<StagedActiveIndex> {
        let active = self.ensure_active_index_seeded()?;
        let staged = unique_sqlite_temp_path(&active, "staged");
        if let Some(parent) = staged.parent() {
            fs::create_dir_all(parent)?;
        }
        cleanup_sqlite_artifacts(&staged);
        if active.exists() {
            fs::copy(&active, &staged)?;
        }
        Ok(StagedActiveIndex {
            active_path: active,
            staged_path: staged,
        })
    }
}

fn unique_sqlite_temp_path(base: &Path, kind: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    base.with_extension(format!("db.{kind}.{}.{}", std::process::id(), nonce))
}

fn cleanup_sqlite_artifacts(path: &Path) {
    for candidate in sqlite_artifact_paths(path) {
        let _ = fs::remove_file(candidate);
    }
}

fn sqlite_artifact_paths(path: &Path) -> [PathBuf; 4] {
    [
        path.to_path_buf(),
        sqlite_auxiliary_path(path, "-journal"),
        sqlite_auxiliary_path(path, "-shm"),
        sqlite_auxiliary_path(path, "-wal"),
    ]
}

fn sqlite_auxiliary_path(path: &Path, suffix: &str) -> PathBuf {
    let mut raw = path.as_os_str().to_os_string();
    raw.push(suffix);
    PathBuf::from(raw)
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
    find_project_roots().map(|roots| roots.project_root)
}

/// Same as [`find_project_root`] but also returns the directory where the project marker
/// (`.tempyr/` or `.tempyr-redirect`) was discovered.
pub fn find_project_roots() -> Option<ProjectRoots> {
    find_project_roots_from(std::env::current_dir().ok()?)
}

/// Same as [`find_project_root`] but starting from a given directory.
pub fn find_project_root_from(start: PathBuf) -> Option<PathBuf> {
    find_project_roots_from(start).map(|roots| roots.project_root)
}

/// Same as [`find_project_roots`] but starting from a given directory.
pub fn find_project_roots_from(start: PathBuf) -> Option<ProjectRoots> {
    let mut dir = start;
    loop {
        // Check for redirect file first
        let redirect_path = dir.join(".tempyr-redirect");
        if redirect_path.is_file()
            && let Some(target) = read_redirect(&redirect_path, &dir)
            && target.join(".tempyr").is_dir()
        {
            return Some(ProjectRoots {
                anchor_root: dir.clone(),
                project_root: target,
            });
        }

        // Check for direct .tempyr/ directory
        if dir.join(".tempyr").is_dir() {
            return Some(ProjectRoots {
                anchor_root: dir.clone(),
                project_root: dir,
            });
        }

        if !dir.pop() {
            return None;
        }
    }
}

/// Load repo-local dotenv files for the current tempyr project without overwriting any
/// variables that are already present in the process environment.
///
/// Loading order preserves intuitive precedence:
/// 1. Existing process environment
/// 2. `.env.local` in the invocation project root
/// 3. `.env` in the invocation project root
/// 4. `.env.local` in the resolved tempyr root (for redirect setups)
/// 5. `.env` in the resolved tempyr root (for redirect setups)
pub fn load_project_env() -> io::Result<Vec<PathBuf>> {
    let Some(start) = std::env::current_dir().ok() else {
        return Ok(Vec::new());
    };
    load_project_env_from(start)
}

/// Same as [`load_project_env`] but starting from a specific directory.
pub fn load_project_env_from(start: PathBuf) -> io::Result<Vec<PathBuf>> {
    let Some(roots) = find_project_roots_from(start) else {
        return Ok(Vec::new());
    };

    let mut loaded = load_env_dir(&roots.anchor_root)?;
    if roots.project_root != roots.anchor_root {
        loaded.extend(load_env_dir(&roots.project_root)?);
    }
    Ok(loaded)
}

fn load_env_dir(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut loaded = Vec::new();
    for filename in [".env.local", ".env"] {
        let path = dir.join(filename);
        if !path.is_file() {
            continue;
        }

        // `from_path` preserves variables that are already set, so earlier files and the
        // existing process environment keep their precedence.
        dotenvy::from_path(&path)
            .map_err(|err| io::Error::other(format!("Failed to load {}: {err}", path.display())))?;
        loaded.push(path);
    }
    Ok(loaded)
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
    let inputs = collect_snapshot_inputs(graph_dir, tempyr_dir);
    snapshot_key_from_inputs(&inputs)
}

fn collect_snapshot_inputs(graph_dir: &Path, tempyr_dir: &Path) -> Vec<SnapshotInput> {
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
                files.push(SnapshotInput {
                    label: format!("graph/{}", rel.to_string_lossy().replace('\\', "/")),
                    path: path.to_path_buf(),
                });
            }
        }
    }

    let schema_path = tempyr_dir.join("schema.toml");
    if schema_path.is_file() {
        files.push(SnapshotInput {
            label: "schema.toml".to_string(),
            path: schema_path,
        });
    }

    files.sort_by(|a, b| a.label.cmp(&b.label));
    files
}

fn snapshot_key_from_inputs(inputs: &[SnapshotInput]) -> io::Result<String> {
    let mut hasher = Hasher::new();
    hasher.update(b"tempyr-snapshot-v1\0");

    for input in inputs {
        hasher.update(input.label.as_bytes());
        hasher.update(b"\0");
        hasher.update(&fs::read(&input.path)?);
        hasher.update(b"\0");
    }

    let hex = hasher.finalize().to_hex().to_string();
    Ok(hex[..16].to_string())
}

fn snapshot_metadata(inputs: &[SnapshotInput]) -> io::Result<SnapshotMetadata> {
    let mut files = Vec::with_capacity(inputs.len());
    for input in inputs {
        let metadata = fs::metadata(&input.path)?;
        let (modified_secs, modified_nanos) = modified_timestamp(&metadata);
        files.push(SnapshotMetadataEntry {
            label: input.label.clone(),
            size: metadata.len(),
            modified_secs,
            modified_nanos,
        });
    }
    Ok(SnapshotMetadata { files })
}

fn modified_timestamp(metadata: &fs::Metadata) -> (u64, u32) {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| (duration.as_secs(), duration.subsec_nanos()))
        .unwrap_or((0, 0))
}

fn read_snapshot_key_cache(path: &Path) -> io::Result<Option<SnapshotKeyCacheEntry>> {
    if !path.is_file() {
        return Ok(None);
    }

    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(_) => return Ok(None),
    };
    Ok(serde_json::from_str(&raw).ok())
}

fn write_snapshot_key_cache(path: &Path, cache: &SnapshotKeyCacheEntry) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let raw = serde_json::to_string(cache)
        .map_err(|err| io::Error::other(format!("Failed to serialize snapshot cache: {err}")))?;
    fs::write(path, raw)
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
    use std::sync::{LazyLock, Mutex};

    static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

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
    fn finds_anchor_and_project_root_for_redirect_file() {
        let tmp = tempfile::tempdir().unwrap();

        let real_root = tmp.path().join("knowledge-base");
        fs::create_dir_all(real_root.join(".tempyr")).unwrap();

        let work_root = tmp.path().join("main-project");
        fs::create_dir(&work_root).unwrap();
        fs::write(work_root.join(".tempyr-redirect"), "../knowledge-base\n").unwrap();

        let roots = find_project_roots_from(work_root.clone()).unwrap();

        assert_eq!(roots.anchor_root, work_root);
        assert_eq!(roots.project_root, fs::canonicalize(&real_root).unwrap());
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
    fn load_project_env_prefers_env_local_within_direct_project() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        fs::create_dir(root.join(".tempyr")).unwrap();

        let local_var = format!("TEMPYR_TEST_LOCAL_{}", rand::random::<u64>());
        let base_var = format!("TEMPYR_TEST_BASE_{}", rand::random::<u64>());
        fs::write(
            root.join(".env"),
            format!("{local_var}=from-dotenv\n{base_var}=from-dotenv\n"),
        )
        .unwrap();
        fs::write(
            root.join(".env.local"),
            format!("{local_var}=from-dotenv-local\n"),
        )
        .unwrap();

        let loaded = load_project_env_from(root).unwrap();

        assert_eq!(loaded.len(), 2);
        assert_eq!(std::env::var(&local_var).unwrap(), "from-dotenv-local");
        assert_eq!(std::env::var(&base_var).unwrap(), "from-dotenv");
    }

    #[test]
    fn load_project_env_finds_project_from_graph_dir_start() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        fs::create_dir(root.join(".tempyr")).unwrap();
        fs::create_dir(root.join("graph")).unwrap();

        let graph_var = format!("TEMPYR_TEST_GRAPH_DIR_{}", rand::random::<u64>());
        fs::write(root.join(".env"), format!("{graph_var}=from-dotenv\n")).unwrap();

        let loaded = load_project_env_from(root.join("graph")).unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(std::env::var(&graph_var).unwrap(), "from-dotenv");
    }

    #[test]
    fn load_project_env_uses_anchor_before_redirect_target() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();

        let real_root = tmp.path().join("knowledge-base");
        fs::create_dir_all(real_root.join(".tempyr")).unwrap();
        let work_root = tmp.path().join("main-project");
        fs::create_dir(&work_root).unwrap();
        fs::write(work_root.join(".tempyr-redirect"), "../knowledge-base\n").unwrap();

        let shared_var = format!("TEMPYR_TEST_SHARED_{}", rand::random::<u64>());
        let root_only_var = format!("TEMPYR_TEST_ROOT_ONLY_{}", rand::random::<u64>());
        fs::write(
            work_root.join(".env"),
            format!("{shared_var}=from-anchor\n"),
        )
        .unwrap();
        fs::write(
            real_root.join(".env"),
            format!("{shared_var}=from-redirect-target\n{root_only_var}=from-redirect-target\n"),
        )
        .unwrap();

        let loaded = load_project_env_from(work_root).unwrap();

        assert_eq!(loaded.len(), 2);
        assert_eq!(std::env::var(&shared_var).unwrap(), "from-anchor");
        assert_eq!(
            std::env::var(&root_only_var).unwrap(),
            "from-redirect-target"
        );
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
    fn snapshot_key_uses_metadata_cache_across_layout_instances() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let graph_dir = root.join("graph");
        let features_dir = graph_dir.join("features");
        let tempyr_dir = root.join(".tempyr");
        fs::create_dir_all(&features_dir).unwrap();
        fs::create_dir_all(&tempyr_dir).unwrap();
        fs::write(tempyr_dir.join("schema.toml"), "name = 'x'\n").unwrap();
        fs::write(features_dir.join("a.md"), "# A\n").unwrap();

        let metadata =
            snapshot_metadata(&collect_snapshot_inputs(&graph_dir, &tempyr_dir)).unwrap();
        let layout = IndexLayout::resolve(root, &graph_dir, &tempyr_dir).unwrap();
        write_snapshot_key_cache(
            &layout.snapshot_key_cache_path(),
            &SnapshotKeyCacheEntry {
                version: SNAPSHOT_KEY_CACHE_VERSION,
                snapshot_key: "cached-snapshot".to_string(),
                metadata,
            },
        )
        .unwrap();

        let fresh_layout = IndexLayout::resolve(root, &graph_dir, &tempyr_dir).unwrap();

        assert_eq!(fresh_layout.snapshot_key().unwrap(), "cached-snapshot");
    }

    #[test]
    fn snapshot_key_ignores_stale_metadata_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let graph_dir = root.join("graph");
        let features_dir = graph_dir.join("features");
        let tempyr_dir = root.join(".tempyr");
        fs::create_dir_all(&features_dir).unwrap();
        fs::create_dir_all(&tempyr_dir).unwrap();
        fs::write(tempyr_dir.join("schema.toml"), "name = 'x'\n").unwrap();
        fs::write(features_dir.join("a.md"), "# A\n").unwrap();

        let metadata =
            snapshot_metadata(&collect_snapshot_inputs(&graph_dir, &tempyr_dir)).unwrap();
        let layout = IndexLayout::resolve(root, &graph_dir, &tempyr_dir).unwrap();
        write_snapshot_key_cache(
            &layout.snapshot_key_cache_path(),
            &SnapshotKeyCacheEntry {
                version: SNAPSHOT_KEY_CACHE_VERSION,
                snapshot_key: "cached-snapshot".to_string(),
                metadata,
            },
        )
        .unwrap();

        fs::write(features_dir.join("a.md"), "# A changed more\n").unwrap();

        let fresh_layout = IndexLayout::resolve(root, &graph_dir, &tempyr_dir).unwrap();
        let actual = graph_snapshot_key(&graph_dir, &tempyr_dir).unwrap();

        assert_eq!(fresh_layout.snapshot_key().unwrap(), actual);
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
        let snapshot_key = layout.snapshot_key().unwrap();
        let active = layout.active_index_path();
        let shared = layout.shared_snapshot_index_path().unwrap();

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
            snapshot_key
        );
    }

    #[test]
    fn publish_active_snapshot_does_not_overwrite_existing_shared_snapshot() {
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
        let shared = layout.shared_snapshot_index_path().unwrap();

        fs::create_dir_all(active.parent().unwrap()).unwrap();
        fs::create_dir_all(shared.parent().unwrap()).unwrap();
        fs::write(&active, "active-index").unwrap();
        fs::write(&shared, "existing-shared-index").unwrap();

        let published = layout.publish_active_snapshot().unwrap();

        assert_eq!(published, shared);
        assert_eq!(fs::read_to_string(shared).unwrap(), "existing-shared-index");
    }

    #[test]
    fn atomic_active_index_update_promotes_staged_index() {
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
        fs::create_dir_all(active.parent().unwrap()).unwrap();
        fs::write(&active, "active-index").unwrap();
        fs::write(
            layout.active_snapshot_path(),
            layout.snapshot_key().unwrap(),
        )
        .unwrap();

        layout
            .update_active_index_atomically(|staged| {
                assert_eq!(fs::read_to_string(staged).unwrap(), "active-index");
                fs::write(staged, "refreshed-index")?;
                Ok(())
            })
            .unwrap();

        assert_eq!(fs::read_to_string(&active).unwrap(), "refreshed-index");
        assert!(staged_index_artifacts(&active).is_empty());
    }

    #[test]
    fn atomic_active_index_update_keeps_live_index_on_error() {
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
        fs::create_dir_all(active.parent().unwrap()).unwrap();
        fs::write(&active, "active-index").unwrap();
        fs::write(
            layout.active_snapshot_path(),
            layout.snapshot_key().unwrap(),
        )
        .unwrap();

        let err = layout
            .update_active_index_atomically(|staged| {
                fs::write(staged, "half-written-index")?;
                Err(io::Error::other("boom"))
            })
            .unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::Other);
        assert_eq!(fs::read_to_string(&active).unwrap(), "active-index");
        assert!(staged_index_artifacts(&active).is_empty());
    }

    fn staged_index_artifacts(active: &Path) -> Vec<PathBuf> {
        let prefix = format!("{}.", active.file_name().unwrap().to_string_lossy());
        fs::read_dir(active.parent().unwrap())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
                    && path != active
            })
            .collect()
    }
}
