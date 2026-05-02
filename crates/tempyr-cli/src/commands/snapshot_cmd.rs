//! Snapshot store management.
//!
//! The shared snapshot store at `<git-common-dir>/tempyr/snapshots/<key>/`
//! accumulates one ~1.8 MB SQLite index per unique graph state ever indexed
//! by any worktree. With no upper bound, it can balloon to gigabytes.
//!
//! `tempyr snapshot prune` enforces a Nix-style hybrid retention policy:
//!
//! 1. **Pinned set** — every snapshot key cited by a live worktree's
//!    `<shared_root>/worktrees/<wt>/snapshot-key.txt` cursor is permanent.
//!    Stale worktree dirs (no matching `.git/worktrees/<wt>/` private admin
//!    dir, when `git worktree list` is available) are excluded from the pin
//!    set so their old cursors don't keep dead snapshots alive forever.
//! 2. **Recent buffer** — after pinning, keep up to `--keep-recent` of the
//!    most-recently-modified snapshots, even if they are not pinned. This
//!    cushions branch-switching workflows so the immediate previous state is
//!    not evicted when a new snapshot lands.
//! 3. **Size cap** — among everything that is neither pinned nor in the
//!    recent buffer, evict in least-recently-modified order until the total
//!    snapshot-store size is under `--max-size`.
//!
//! Deletion is two-phase to dodge open-file races on Windows: each victim is
//! first renamed to a sibling `<shared_root>/snapshots/.gc-<key>-<pid>-<ts>/`
//! (invisible to fresh `current_index_path` lookups), then `remove_dir_all`
//! is attempted. If removal fails (most often because a long-running query
//! still holds the file open), the renamed dir is left for the next pass.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use serde::Serialize;
use tempyr_core::project::{self, CacheLayout};

use crate::config::ProjectContext;

/// Prefix marking a snapshot dir that has been atomically renamed for
/// deletion by [`run_prune`] but not yet `remove_dir_all`-ed. New readers
/// look up snapshots by exact key, so a renamed dir is invisible to them.
const GC_PREFIX: &str = ".gc-";

/// Default size cap for `tempyr snapshot prune --max-size`. At ~1.8 MB per
/// snapshot this fits ~280 snapshots, comfortably more than the typical
/// working set across worktrees on one repo. Also surfaced by `tempyr
/// doctor` (see [`SNAPSHOT_STORE_HINT_BYTES`]).
pub const DEFAULT_MAX_SIZE_BYTES: u64 = 500 * 1024 * 1024;

/// Default size of the "recent buffer" — the number of newest snapshots to
/// keep beyond the pinned set, so branch-switching doesn't immediately
/// evict the previous state.
pub const DEFAULT_KEEP_RECENT: usize = 20;

/// `tempyr doctor` switches the snapshot-store summary from "ok" to
/// "consider `tempyr snapshot prune`" when either threshold is exceeded.
/// Set well below [`DEFAULT_MAX_SIZE_BYTES`] so users see the hint before
/// they hit the cap.
pub const SNAPSHOT_STORE_HINT_DIRS: usize = 200;
pub const SNAPSHOT_STORE_HINT_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct PruneOptions {
    pub keep_recent: usize,
    pub max_size_bytes: u64,
}

impl Default for PruneOptions {
    fn default() -> Self {
        Self {
            keep_recent: DEFAULT_KEEP_RECENT,
            max_size_bytes: DEFAULT_MAX_SIZE_BYTES,
        }
    }
}

/// Outcome of one [`run_prune`] invocation.
///
/// All three "kept" counts sum to the number of snapshots remaining on
/// disk after the prune. Each snapshot is counted in exactly one bucket:
///
/// - `kept_pinned`: cited by a live worktree's `snapshot-key.txt` cursor.
/// - `kept_buffer`: not pinned, but among the most-recent `keep_recent` by mtime.
/// - `kept_under_cap`: neither pinned nor in the buffer, but kept because
///   evicting them wasn't necessary to fit under `max_size_bytes`.
///
/// `total_bytes_after_estimate` is the sum of sizes for all three kept
/// buckets — it's the on-disk size we expect after eviction completes.
#[derive(Debug, Serialize)]
pub struct PruneReport {
    pub kept_pinned: usize,
    pub kept_buffer: usize,
    pub kept_under_cap: usize,
    pub evicted: Vec<EvictedEntry>,
    /// Snapshots whose `rename` to a `.gc-*` stub or `remove_dir_all`
    /// failed (most often a long-running reader still holds the SQLite
    /// file open on Windows). The stub is left in place; the next prune
    /// pass sweeps it up.
    pub failures: Vec<EvictionFailure>,
    pub total_bytes_before: u64,
    pub total_bytes_after_estimate: u64,
}

#[derive(Debug, Serialize)]
pub struct EvictedEntry {
    pub snapshot_key: String,
    pub bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct EvictionFailure {
    pub snapshot_key: String,
    pub message: String,
}

#[derive(Debug)]
struct SnapshotEntry {
    snapshot_key: String,
    path: PathBuf,
    bytes: u64,
    modified_secs: u64,
}

/// Parse `--max-size` strings like `200`, `200K`, `500M`, `2G`.
pub fn parse_size(input: &str) -> anyhow::Result<u64> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        anyhow::bail!("size value is empty");
    }
    let bytes = trimmed.as_bytes();
    let last = bytes[bytes.len() - 1];
    let (digits, multiplier): (&str, u64) = match last {
        b'k' | b'K' => (&trimmed[..trimmed.len() - 1], 1024),
        b'm' | b'M' => (&trimmed[..trimmed.len() - 1], 1024 * 1024),
        b'g' | b'G' => (&trimmed[..trimmed.len() - 1], 1024 * 1024 * 1024),
        b'0'..=b'9' => (trimmed, 1),
        _ => anyhow::bail!("unrecognized size suffix in {input:?}; use plain bytes or K/M/G"),
    };
    let n: u64 = digits
        .parse()
        .with_context(|| format!("could not parse size value {input:?}"))?;
    Ok(n.saturating_mul(multiplier))
}

/// Output format for `tempyr snapshot prune`. Auto-prune callers
/// (rebuild/update tails) use [`PruneOutput::Silent`]; the CLI passes
/// `Human` or `Json` based on the global `--json` flag.
#[derive(Debug, Clone, Copy)]
pub enum PruneOutput {
    Silent,
    Human,
    Json,
}

pub fn run_prune(
    ctx: &ProjectContext,
    opts: &PruneOptions,
    dry_run: bool,
    output: PruneOutput,
) -> anyhow::Result<PruneReport> {
    let cache = ctx.cache_layout();
    let snapshots_root = cache.snapshots_root();

    let report = if snapshots_root.exists() {
        plan_and_evict(&snapshots_root, cache, opts, dry_run)?
    } else {
        PruneReport {
            kept_pinned: 0,
            kept_buffer: 0,
            kept_under_cap: 0,
            evicted: Vec::new(),
            failures: Vec::new(),
            total_bytes_before: 0,
            total_bytes_after_estimate: 0,
        }
    };

    render_prune_output(&report, dry_run, output)?;
    Ok(report)
}

fn plan_and_evict(
    snapshots_root: &Path,
    cache: &CacheLayout,
    opts: &PruneOptions,
    dry_run: bool,
) -> anyhow::Result<PruneReport> {
    let mut entries = enumerate_snapshots(snapshots_root)?;
    entries.sort_by_key(|e| std::cmp::Reverse(e.modified_secs));

    let pinned = collect_pin_set(cache);

    // Pass 1: keep all pinned, plus the most-recent `keep_recent` of the rest.
    let mut kept: HashSet<String> = HashSet::new();
    let mut kept_pinned = 0;
    let mut kept_buffer = 0;
    for entry in &entries {
        if pinned.contains(&entry.snapshot_key) && kept.insert(entry.snapshot_key.clone()) {
            kept_pinned += 1;
        }
    }
    for entry in &entries {
        if kept_buffer >= opts.keep_recent {
            break;
        }
        if kept.insert(entry.snapshot_key.clone()) {
            kept_buffer += 1;
        }
    }

    // Pass 2: among unkept, walk newest-first and keep each whose size fits
    // under the remaining cap headroom. Older unkept entries that can't fit
    // are evicted. This is a soft cap, not a hard LRU: a single huge
    // snapshot doesn't kick out smaller older ones that do fit.
    let total_bytes_before: u64 = entries.iter().map(|e| e.bytes).sum();
    let kept_bytes: u64 = entries
        .iter()
        .filter(|e| kept.contains(&e.snapshot_key))
        .map(|e| e.bytes)
        .sum();
    let mut running = kept_bytes;
    let mut kept_under_cap = 0;
    let mut evict: Vec<&SnapshotEntry> = Vec::new();
    let mut unkept: Vec<&SnapshotEntry> = entries
        .iter()
        .filter(|e| !kept.contains(&e.snapshot_key))
        .collect();
    unkept.sort_by_key(|e| std::cmp::Reverse(e.modified_secs));
    for entry in &unkept {
        if running + entry.bytes <= opts.max_size_bytes {
            running += entry.bytes;
            kept_under_cap += 1;
        } else {
            evict.push(*entry);
        }
    }

    let mut report = PruneReport {
        kept_pinned,
        kept_buffer,
        kept_under_cap,
        evicted: Vec::new(),
        failures: Vec::new(),
        total_bytes_before,
        total_bytes_after_estimate: running,
    };

    for entry in &evict {
        if dry_run {
            report.evicted.push(EvictedEntry {
                snapshot_key: entry.snapshot_key.clone(),
                bytes: entry.bytes,
            });
            continue;
        }
        match two_phase_remove(&entry.path) {
            Ok(()) => report.evicted.push(EvictedEntry {
                snapshot_key: entry.snapshot_key.clone(),
                bytes: entry.bytes,
            }),
            Err(err) => report.failures.push(EvictionFailure {
                snapshot_key: entry.snapshot_key.clone(),
                message: err.to_string(),
            }),
        }
    }

    // Sweep `.gc-*` stubs that prior runs left behind (e.g. EBUSY on Windows).
    if !dry_run {
        let _ = sweep_orphaned_gc_dirs(snapshots_root);
    }

    Ok(report)
}

fn render_prune_output(
    report: &PruneReport,
    dry_run: bool,
    output: PruneOutput,
) -> anyhow::Result<()> {
    match output {
        PruneOutput::Silent => Ok(()),
        PruneOutput::Json => {
            println!("{}", serde_json::to_string_pretty(report)?);
            Ok(())
        }
        PruneOutput::Human => {
            let verb = if dry_run { "would evict" } else { "evicted" };
            let total_kept = report.kept_pinned + report.kept_buffer + report.kept_under_cap;
            println!(
                "Snapshot prune{}: {verb} {} snapshots, kept {total_kept} ({} pinned, {} buffer, {} under cap)",
                if dry_run { " (dry run)" } else { "" },
                report.evicted.len(),
                report.kept_pinned,
                report.kept_buffer,
                report.kept_under_cap,
            );
            if !report.failures.is_empty() {
                println!("  {} eviction(s) failed:", report.failures.len());
                for failure in &report.failures {
                    println!("    {}: {}", failure.snapshot_key, failure.message);
                }
            }
            Ok(())
        }
    }
}

pub fn run_list(ctx: &ProjectContext, json: bool) -> anyhow::Result<()> {
    let cache = ctx.cache_layout();
    let snapshots_root = cache.snapshots_root();
    let entries = if snapshots_root.exists() {
        enumerate_snapshots(&snapshots_root)?
    } else {
        Vec::new()
    };
    let pinned = collect_pin_set(cache);

    if json {
        #[derive(Serialize)]
        struct Row<'a> {
            snapshot_key: &'a str,
            bytes: u64,
            modified_secs: u64,
            pinned: bool,
        }
        let rows: Vec<Row> = entries
            .iter()
            .map(|e| Row {
                snapshot_key: &e.snapshot_key,
                bytes: e.bytes,
                modified_secs: e.modified_secs,
                pinned: pinned.contains(&e.snapshot_key),
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        println!("Snapshots in {}:", snapshots_root.display());
        for entry in &entries {
            let marker = if pinned.contains(&entry.snapshot_key) {
                "PINNED"
            } else {
                "      "
            };
            println!(
                "  {marker}  {}  {:>10} bytes  mtime={}",
                entry.snapshot_key, entry.bytes, entry.modified_secs
            );
        }
        println!(
            "Total: {} snapshots, {} pinned",
            entries.len(),
            entries
                .iter()
                .filter(|e| pinned.contains(&e.snapshot_key))
                .count()
        );
    }
    Ok(())
}

fn enumerate_snapshots(snapshots_root: &Path) -> io::Result<Vec<SnapshotEntry>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(snapshots_root)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if !file_type.is_dir() {
            continue;
        }
        let name = match entry.file_name().into_string() {
            Ok(name) => name,
            Err(_) => continue,
        };
        // Skip the `.locks` coordination dir and any partial-prune `.gc-*`
        // stubs. `is_snapshot_key` accepts only the 16-hex-char names.
        if !project::is_snapshot_key(&name) {
            continue;
        }
        let path = entry.path();
        let bytes = directory_size(&path).unwrap_or(0);
        let modified_secs = directory_modified_secs(&path).unwrap_or(0);
        out.push(SnapshotEntry {
            snapshot_key: name,
            path,
            bytes,
            modified_secs,
        });
    }
    Ok(out)
}

/// Build the set of snapshot keys that no eviction may touch.
///
/// Each per-worktree `<shared_root>/worktrees/<wt-id>/snapshot-key.txt`
/// is a GC root pinning whatever snapshot key it contains. When `git
/// worktree list` works against the cache's owning repo we additionally
/// drop cursors whose `wt-id` no longer corresponds to a live worktree —
/// those are dangling pointers from worktrees the user deleted. When
/// git is unavailable we trust every cursor on disk: better to retain
/// too many snapshots than to silently evict one a worktree is still
/// pointing at.
fn collect_pin_set(cache: &CacheLayout) -> HashSet<String> {
    let mut pinned = HashSet::new();
    let worktrees_root = cache.worktrees_root();
    let live_wt_ids = live_worktree_ids(cache);
    let Ok(read) = fs::read_dir(&worktrees_root) else {
        return pinned;
    };
    for entry in read.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let cursor_path = entry.path().join("snapshot-key.txt");
        let Ok(raw) = fs::read_to_string(&cursor_path) else {
            continue;
        };
        let key = raw.trim().to_string();
        if !project::is_snapshot_key(&key) {
            continue;
        }

        // When we have a live worktree list, drop cursors whose worktree-id
        // is not present — those are dangling. Without git we keep all of
        // them.
        if let Some(ref live) = live_wt_ids {
            let wt_id = entry.file_name().to_string_lossy().to_string();
            if !live.contains(&wt_id) {
                continue;
            }
        }
        pinned.insert(key);
    }
    pinned
}

/// Best-effort enumeration of live git worktree-ids for the repo that
/// owns this snapshot store. Each id is [`project::short_path_hash`] of
/// that worktree's private `.git` admin dir — the same hash
/// [`tempyr_core::project::IndexLayout`] uses to derive each worktree's
/// cache subdir.
///
/// We pass `--git-dir=<owning common dir>` rather than relying on the
/// process cwd, so a `cargo test` running from the Tempyr repo doesn't
/// accidentally treat the *test's* synthetic store as belonging to the
/// Tempyr worktree. Returns `None` when the cache isn't backed by a git
/// repo (non-git tempyr projects keep their cache under `.tempyr/cache/`)
/// or when git is unavailable; callers treat `None` as "trust all
/// on-disk cursors" so we never over-evict.
fn live_worktree_ids(cache: &CacheLayout) -> Option<HashSet<String>> {
    // For git-backed projects, `cache.shared_root` is `<common-dir>/tempyr`.
    // Bail out if the parent doesn't look like a git common dir (no `HEAD`
    // file, no `commondir` file pointing elsewhere).
    let common_dir = cache.shared_root.parent()?;
    if !common_dir.join("HEAD").is_file() && !common_dir.join("commondir").is_file() {
        return None;
    }
    let output = std::process::Command::new("git")
        .arg(format!("--git-dir={}", common_dir.display()))
        .args(["worktree", "list", "--porcelain"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut ids = HashSet::new();
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let Some(path) = line.strip_prefix("worktree ") else {
            continue;
        };
        if let Some(dirs) = project::resolve_git_dirs(&PathBuf::from(path)) {
            ids.insert(project::short_path_hash(&dirs.git_dir));
        }
    }
    Some(ids)
}

fn directory_size(path: &Path) -> io::Result<u64> {
    let mut total: u64 = 0;
    for entry in walkdir::WalkDir::new(path) {
        let Ok(entry) = entry else { continue };
        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if metadata.is_file() {
            total = total.saturating_add(metadata.len());
        }
    }
    Ok(total)
}

fn directory_modified_secs(path: &Path) -> io::Result<u64> {
    let metadata = fs::metadata(path)?;
    Ok(metadata
        .modified()
        .ok()
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0))
}

fn two_phase_remove(snapshot_dir: &Path) -> io::Result<()> {
    let parent = snapshot_dir
        .parent()
        .ok_or_else(|| io::Error::other("snapshot dir has no parent"))?;
    let name = snapshot_dir
        .file_name()
        .ok_or_else(|| io::Error::other("snapshot dir has no name"))?
        .to_string_lossy()
        .to_string();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let staged = parent.join(format!("{GC_PREFIX}{name}-{}-{nonce}", std::process::id()));

    fs::rename(snapshot_dir, &staged)?;
    // Try to remove the renamed dir. On Windows this can fail with EBUSY
    // if a long-running reader still has the index file open; in that
    // case the staged dir lingers and the next prune sweeps it up.
    fs::remove_dir_all(&staged)
}

fn sweep_orphaned_gc_dirs(snapshots_root: &Path) -> io::Result<()> {
    for entry in fs::read_dir(snapshots_root)? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(GC_PREFIX) {
            continue;
        }
        let _ = fs::remove_dir_all(entry.path());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_size_accepts_plain_bytes() {
        assert_eq!(parse_size("1024").unwrap(), 1024);
    }

    #[test]
    fn parse_size_accepts_suffixes() {
        assert_eq!(parse_size("1K").unwrap(), 1024);
        assert_eq!(parse_size("2k").unwrap(), 2048);
        assert_eq!(parse_size("3M").unwrap(), 3 * 1024 * 1024);
        assert_eq!(parse_size("4g").unwrap(), 4 * 1024 * 1024 * 1024);
    }

    #[test]
    fn parse_size_rejects_unknown_suffix() {
        assert!(parse_size("1T").is_err());
        assert!(parse_size("1.5M").is_err());
        assert!(parse_size("").is_err());
    }

    use std::fs;

    /// Build a synthetic snapshot store under `tmp` with the given keys, each
    /// containing an `index.db` of `bytes_per_snapshot` bytes. Modified-time
    /// is back-dated so the first key is oldest and the last key is newest.
    fn populate_snapshot_store(
        tmp: &Path,
        keys: &[&str],
        bytes_per_snapshot: usize,
    ) -> CacheLayout {
        let cache = CacheLayout {
            shared_root: tmp.to_path_buf(),
            worktree_root: tmp.join("worktrees").join("default"),
        };
        let snapshots_root = cache.snapshots_root();
        fs::create_dir_all(&snapshots_root).unwrap();
        let payload = vec![0u8; bytes_per_snapshot];
        for (i, key) in keys.iter().enumerate() {
            let dir = snapshots_root.join(key);
            fs::create_dir_all(&dir).unwrap();
            let path = dir.join("index.db");
            fs::write(&path, &payload).unwrap();
            // Set mtime via filetime so test ordering is deterministic.
            let mtime = filetime_for_offset_secs((keys.len() - i) as i64 * 60);
            filetime::set_file_mtime(&path, mtime).unwrap();
            filetime::set_file_mtime(&dir, mtime).unwrap();
        }
        cache
    }

    fn write_worktree_cursor(cache: &CacheLayout, wt_id: &str, snapshot_key: &str) {
        let dir = cache.worktrees_root().join(wt_id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("snapshot-key.txt"), snapshot_key).unwrap();
    }

    fn filetime_for_offset_secs(secs_ago: i64) -> filetime::FileTime {
        let now = std::time::SystemTime::now();
        let target = if secs_ago >= 0 {
            now - std::time::Duration::from_secs(secs_ago as u64)
        } else {
            now + std::time::Duration::from_secs((-secs_ago) as u64)
        };
        filetime::FileTime::from_system_time(target)
    }

    fn make_ctx_for_cache(cache: &CacheLayout) -> ProjectContext {
        let root = cache.shared_root.clone();
        let tempyr_dir = root.join(".tempyr");
        std::fs::create_dir_all(&tempyr_dir).unwrap();
        ProjectContext {
            root: root.clone(),
            graph_dir: root.join("graph"),
            tempyr_dir,
            cache: cache.clone(),
            schema: tempyr_core::schema::Schema {
                meta: tempyr_core::schema::SchemaMeta {
                    version: "1".to_string(),
                    description: String::new(),
                },
                node_types: Default::default(),
                edge_types: Default::default(),
            },
        }
    }

    fn keys_on_disk(cache: &CacheLayout) -> Vec<String> {
        let mut keys: Vec<String> = fs::read_dir(cache.snapshots_root())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.len() == 16 && n.bytes().all(|b| b.is_ascii_hexdigit()))
            .collect();
        keys.sort();
        keys
    }

    #[test]
    fn prune_keeps_pinned_snapshots() {
        let tmp = tempfile::tempdir().unwrap();
        let keys = [
            "0000000000000001",
            "0000000000000002",
            "0000000000000003",
            "0000000000000004",
        ];
        let cache = populate_snapshot_store(tmp.path(), &keys, 8);
        write_worktree_cursor(&cache, "wt-a", "0000000000000001");
        let ctx = make_ctx_for_cache(&cache);

        let opts = PruneOptions {
            keep_recent: 0,
            max_size_bytes: 1, // force eviction of everything not pinned/buffered
        };
        let report = run_prune(&ctx, &opts, false, PruneOutput::Silent).unwrap();

        // The pinned key must survive even though it's the oldest.
        let remaining = keys_on_disk(&cache);
        assert!(remaining.contains(&"0000000000000001".to_string()));
        assert_eq!(report.kept_pinned, 1);
        // Everything else evicted.
        assert!(
            report
                .evicted
                .iter()
                .all(|e| e.snapshot_key != "0000000000000001")
        );
        assert_eq!(remaining.len(), 1);
    }

    #[test]
    fn prune_keeps_recent_buffer_above_pin_set() {
        let tmp = tempfile::tempdir().unwrap();
        let keys = [
            "1111111111111111", // oldest
            "2222222222222222",
            "3333333333333333",
            "4444444444444444",
            "5555555555555555", // newest
        ];
        let cache = populate_snapshot_store(tmp.path(), &keys, 8);
        // No worktree cursors → no pins.
        let ctx = make_ctx_for_cache(&cache);

        let opts = PruneOptions {
            keep_recent: 2,
            max_size_bytes: 1, // cap forces only buffer to survive
        };
        let report = run_prune(&ctx, &opts, false, PruneOutput::Silent).unwrap();

        assert_eq!(report.kept_pinned, 0);
        assert_eq!(report.kept_buffer, 2);
        // Only the 2 newest survive.
        let remaining = keys_on_disk(&cache);
        assert_eq!(
            remaining,
            vec![
                "4444444444444444".to_string(),
                "5555555555555555".to_string()
            ]
        );
    }

    #[test]
    fn prune_evicts_lru_under_size_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let keys = [
            "aaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbb",
            "cccccccccccccccc",
            "dddddddddddddddd",
        ];
        // 100 bytes per snapshot → 400 total.
        let cache = populate_snapshot_store(tmp.path(), &keys, 100);
        let ctx = make_ctx_for_cache(&cache);

        let opts = PruneOptions {
            keep_recent: 0,
            max_size_bytes: 250, // fits 2 snapshots, evict 2
        };
        let report = run_prune(&ctx, &opts, false, PruneOutput::Silent).unwrap();

        // The 2 newest survive (cccc + dddd).
        let remaining = keys_on_disk(&cache);
        assert_eq!(
            remaining,
            vec![
                "cccccccccccccccc".to_string(),
                "dddddddddddddddd".to_string()
            ]
        );
        assert_eq!(report.evicted.len(), 2);
        assert!(report.total_bytes_after_estimate <= opts.max_size_bytes);
    }

    #[test]
    fn prune_dry_run_does_not_delete() {
        let tmp = tempfile::tempdir().unwrap();
        let keys = ["1234567890abcdef", "abcdef1234567890"];
        let cache = populate_snapshot_store(tmp.path(), &keys, 50);
        let ctx = make_ctx_for_cache(&cache);

        let opts = PruneOptions {
            keep_recent: 0,
            max_size_bytes: 1,
        };
        let report = run_prune(&ctx, &opts, true, PruneOutput::Silent).unwrap();

        assert_eq!(report.evicted.len(), 2);
        assert_eq!(keys_on_disk(&cache).len(), 2, "dry run must not delete");
    }

    #[test]
    fn prune_skips_locks_dir_and_gc_stubs() {
        let tmp = tempfile::tempdir().unwrap();
        let keys = ["0123456789abcdef"];
        let cache = populate_snapshot_store(tmp.path(), &keys, 8);
        // Create a .locks dir and a .gc-* stub — these must be ignored.
        std::fs::create_dir_all(cache.snapshot_locks_dir()).unwrap();
        std::fs::create_dir_all(cache.snapshots_root().join(".gc-stale-12345")).unwrap();
        std::fs::write(
            cache
                .snapshots_root()
                .join(".gc-stale-12345")
                .join("index.db"),
            b"orphan",
        )
        .unwrap();

        let ctx = make_ctx_for_cache(&cache);
        let opts = PruneOptions {
            keep_recent: 100,
            max_size_bytes: u64::MAX,
        };
        let report = run_prune(&ctx, &opts, false, PruneOutput::Silent).unwrap();

        // Real snapshot survived.
        assert!(keys_on_disk(&cache).contains(&"0123456789abcdef".to_string()));
        // Orphan .gc-* dirs got swept.
        assert!(!cache.snapshots_root().join(".gc-stale-12345").exists());
        // .locks dir unaffected.
        assert!(cache.snapshot_locks_dir().exists());
        // Nothing real evicted.
        assert!(report.evicted.is_empty());
    }
}
