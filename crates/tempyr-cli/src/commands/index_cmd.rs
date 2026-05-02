use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use crate::commands::snapshot_cmd::{self, PruneOptions};
use crate::config::ProjectContext;
use tempyr_core::graph::Graph;
use tempyr_core::project::SnapshotBuildLock;
use tempyr_index::embeddings::{self, EmbeddingStore};
use tempyr_index::indexer::Index;

/// How long to wait for a competing builder to finish before giving up.
/// Used both for the "wait for the other process to publish the snapshot"
/// poll and the "block on the build lock" retry. Builds are typically a
/// few seconds; the budget is generous to cover cold caches.
const BUILD_LOCK_WAIT: Duration = Duration::from_secs(60);

/// Polling interval while waiting for another process to publish a snapshot
/// or release the build lock.
const BUILD_LOCK_POLL: Duration = Duration::from_millis(100);

/// Outcome of negotiating with concurrent rebuilds for the current snapshot.
#[cfg_attr(test, derive(Debug))]
enum RebuildSlot {
    /// Caller holds the build lock and must rebuild the snapshot from scratch.
    /// Held until the caller drops the value at end of scope.
    Build(#[allow(dead_code)] SnapshotBuildLock),
    /// The shared snapshot exists; caller should seed-and-report rather than
    /// rebuild. The bool records whether another process built it during this
    /// invocation (true) or whether it was already there at entry (false).
    UseExisting { built_by_other: bool },
}

pub fn run_rebuild(
    ctx: &ProjectContext,
    json: bool,
    skip_embeddings: bool,
    force: bool,
) -> anyhow::Result<()> {
    let graph = Graph::load_from_directory(&ctx.graph_dir, ctx.schema.clone())?;
    let (snapshot_key, _) = ctx.ensure_active_index_seeded()?;
    let shared = ctx.shared_snapshot_index_path()?;

    let slot = negotiate_rebuild_slot(ctx, &shared, force, BUILD_LOCK_WAIT)?;
    if let RebuildSlot::UseExisting { built_by_other } = slot {
        return seed_and_report(
            ctx,
            &graph,
            &snapshot_key,
            json,
            skip_embeddings,
            built_by_other,
        );
    }

    rebuild_from_scratch(ctx, &graph, &snapshot_key, json, skip_embeddings)?;

    // Best-effort snapshot store maintenance. Failures here do not break the
    // rebuild — pruning is a hygiene step.
    let _ = snapshot_cmd::run_prune(
        ctx,
        &PruneOptions::default(),
        false,
        snapshot_cmd::PruneOutput::Silent,
    );

    Ok(())
}

/// Decide whether this caller should rebuild the snapshot or reuse an
/// existing one, while serializing against any concurrent builder.
///
/// Returns:
/// - [`RebuildSlot::UseExisting`] when `force = false` and either (a) the
///   shared snapshot is already on disk at entry, or (b) it appears while
///   we wait for another process to publish.
/// - [`RebuildSlot::Build`] holding the per-key build lock, when this
///   caller must rebuild. Drop the value to release.
///
/// `wait_budget` bounds **both** the wait-for-publish poll and the
/// block-acquire retry, so the worst-case total wait is roughly
/// `2 * wait_budget`. Tests can pass a small budget to exercise the
/// blocking path without sleeping for the production default.
fn negotiate_rebuild_slot(
    ctx: &ProjectContext,
    shared: &Path,
    force: bool,
    wait_budget: Duration,
) -> anyhow::Result<RebuildSlot> {
    if !force && shared.exists() {
        return Ok(RebuildSlot::UseExisting {
            built_by_other: false,
        });
    }

    if let Some(lock) = ctx.try_acquire_snapshot_build_lock()? {
        if !force && shared.exists() {
            // Another process won the race and published while we were
            // taking the lock. Release and use what they built.
            drop(lock);
            return Ok(RebuildSlot::UseExisting {
                built_by_other: true,
            });
        }
        return Ok(RebuildSlot::Build(lock));
    }

    // Lock is held by another rebuild. Wait for them to publish.
    wait_for_snapshot(shared, wait_budget);
    if !force && shared.exists() {
        return Ok(RebuildSlot::UseExisting {
            built_by_other: true,
        });
    }

    // Other builder didn't publish in time. Block on the lock so the work
    // is still serialized — better to wait than to double-build.
    let lock = acquire_build_lock_blocking(ctx, wait_budget)?;
    if !force && shared.exists() {
        drop(lock);
        return Ok(RebuildSlot::UseExisting {
            built_by_other: true,
        });
    }
    Ok(RebuildSlot::Build(lock))
}

fn rebuild_from_scratch(
    ctx: &ProjectContext,
    graph: &Graph,
    snapshot_key: &str,
    json: bool,
    skip_embeddings: bool,
) -> anyhow::Result<()> {
    let (_key, index_path) = ctx.ensure_active_index_seeded()?;

    if index_path.exists() {
        std::fs::remove_file(&index_path)?;
    }
    if let Some(parent) = index_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let index = Index::create(&index_path)?;
    let stats = index.rebuild(graph)?;

    let embed_result = maybe_embed(graph, ctx, skip_embeddings);
    ctx.write_active_snapshot_key(snapshot_key)?;
    ctx.publish_active_snapshot(snapshot_key)?;

    if json {
        let mut result = serde_json::json!({
            "node_count": stats.node_count,
            "edge_count": stats.edge_count,
            "fts_entries": stats.fts_entries,
            "nodes_by_type": stats.nodes_by_type,
            "source": "rebuilt",
        });
        if let Ok(ref es) = embed_result {
            result["embeddings"] = serde_json::json!({
                "embedded": es.embedded,
                "cached": es.skipped,
                "dimensions": es.dimensions,
            });
        }
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!(
            "Index rebuilt: {} nodes, {} edges, {} FTS entries",
            stats.node_count, stats.edge_count, stats.fts_entries
        );
        for (node_type, count) in &stats.nodes_by_type {
            println!("  {node_type}: {count}");
        }
        render_embedding_message(&embed_result);
    }

    Ok(())
}

fn wait_for_snapshot(path: &Path, max: Duration) {
    let deadline = Instant::now() + max;
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        thread::sleep(BUILD_LOCK_POLL);
    }
}

fn acquire_build_lock_blocking(
    ctx: &ProjectContext,
    wait_budget: Duration,
) -> anyhow::Result<SnapshotBuildLock> {
    let deadline = Instant::now() + wait_budget;
    loop {
        if let Some(lock) = ctx.try_acquire_snapshot_build_lock()? {
            return Ok(lock);
        }
        if Instant::now() >= deadline {
            anyhow::bail!(
                "Could not acquire snapshot build lock within {wait_budget:?}; another rebuild is still running."
            );
        }
        thread::sleep(BUILD_LOCK_POLL);
    }
}

fn seed_and_report(
    ctx: &ProjectContext,
    graph: &Graph,
    snapshot_key: &str,
    json: bool,
    skip_embeddings: bool,
    waited_for_concurrent_builder: bool,
) -> anyhow::Result<()> {
    let index_path = ctx.queryable_index_path()?;
    let index = Index::open(&index_path)?;
    let stats = index.stats()?;
    let embed_result = maybe_embed(graph, ctx, skip_embeddings);
    ctx.write_active_snapshot_key(snapshot_key)?;

    let source = if waited_for_concurrent_builder {
        "snapshot_built_by_other_process"
    } else {
        "existing_snapshot"
    };

    if json {
        let mut result = serde_json::json!({
            "node_count": stats.node_count,
            "edge_count": stats.edge_count,
            "fts_entries": stats.fts_entries,
            "nodes_by_type": stats.nodes_by_type,
            "source": source,
            "snapshot_key": snapshot_key,
        });
        if let Ok(ref es) = embed_result {
            result["embeddings"] = serde_json::json!({
                "embedded": es.embedded,
                "cached": es.skipped,
                "dimensions": es.dimensions,
            });
        }
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        let prefix = if waited_for_concurrent_builder {
            "Index reused after concurrent build"
        } else {
            "Index reused from existing snapshot"
        };
        println!(
            "{prefix}: {} nodes, {} edges, {} FTS entries (snapshot {snapshot_key})",
            stats.node_count, stats.edge_count, stats.fts_entries
        );
        for (node_type, count) in &stats.nodes_by_type {
            println!("  {node_type}: {count}");
        }
        render_embedding_message(&embed_result);
    }

    Ok(())
}

pub fn run_update(ctx: &ProjectContext, json: bool, skip_embeddings: bool) -> anyhow::Result<()> {
    let graph = Graph::load_from_directory(&ctx.graph_dir, ctx.schema.clone())?;
    let (snapshot_key, index_path) = ctx.ensure_active_index_seeded()?;

    if !index_path.exists() {
        return run_rebuild(ctx, json, skip_embeddings, false);
    }

    let index = Index::open(&index_path)?;
    let stats = index.incremental_update(&graph)?;

    // Try to generate embeddings for new/changed nodes
    let embed_result = maybe_embed(&graph, ctx, skip_embeddings);
    ctx.write_active_snapshot_key(&snapshot_key)?;
    ctx.publish_active_snapshot(&snapshot_key)?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "node_count": stats.node_count,
                "edge_count": stats.edge_count,
                "fts_entries": stats.fts_entries,
            }))?
        );
    } else {
        println!(
            "Index updated: {} nodes, {} edges",
            stats.node_count, stats.edge_count
        );
        render_embedding_message(&embed_result);
    }

    // Tier 3: best-effort snapshot store maintenance.
    let _ = snapshot_cmd::run_prune(
        ctx,
        &PruneOptions::default(),
        false,
        snapshot_cmd::PruneOutput::Silent,
    );

    Ok(())
}

pub fn run_stats(ctx: &ProjectContext, json: bool) -> anyhow::Result<()> {
    let index_path = ctx.queryable_index_path()?;

    let index = Index::open(&index_path)?;
    let stats = index.stats()?;
    let resolved = ctx.resolved_embedding_config()?;
    let store_path = ctx.embedding_store_path(
        &resolved.provider,
        resolved.model.as_deref(),
        Some(resolved.dimensions),
    );
    let legacy_embedding_count = index.embedding_count().unwrap_or(0);
    let (embedding_count, shared_embedding_count, shared_embedding_error) =
        shared_embedding_counts(&store_path, &index);
    let effective_embedding_count = match (&embedding_count, &shared_embedding_error) {
        (Some(count), _) => Some(*count),
        (None, Some(_)) => None,
        (None, None) => Some(legacy_embedding_count),
    };

    render_stats(
        stats,
        legacy_embedding_count,
        effective_embedding_count,
        shared_embedding_count,
        shared_embedding_error,
        json,
    )
}

/// Try to embed graph nodes. Returns error (not fatal) if no API key is available.
fn try_embed(graph: &Graph, ctx: &ProjectContext) -> anyhow::Result<embeddings::EmbedStats> {
    let resolved = ctx.resolved_embedding_config()?;
    let provider = embeddings::create_provider_from_resolved(&resolved)?;
    let store_path = ctx.embedding_store_path(
        &resolved.provider,
        resolved.model.as_deref(),
        Some(resolved.dimensions),
    );
    let store = EmbeddingStore::open_or_create(&store_path)?;

    let rt = tokio::runtime::Runtime::new()?;
    let stats = rt.block_on(embeddings::embed_graph(&store, graph, provider.as_ref()))?;
    Ok(stats)
}

fn maybe_embed(
    graph: &Graph,
    ctx: &ProjectContext,
    skip_embeddings: bool,
) -> anyhow::Result<embeddings::EmbedStats> {
    if skip_embeddings {
        anyhow::bail!("disabled via --skip-embeddings");
    }
    try_embed(graph, ctx)
}

fn render_embedding_message(embed_result: &anyhow::Result<embeddings::EmbedStats>) {
    match embed_result {
        Ok(es) => println!("{es}"),
        Err(e) => println!("Embeddings skipped: {e}"),
    }
}

fn shared_embedding_counts(
    store_path: &Path,
    index: &Index,
) -> (Option<usize>, Option<usize>, Option<String>) {
    if !store_path.exists() {
        return (None, None, None);
    }

    match EmbeddingStore::open_or_create(store_path) {
        Ok(store) => {
            let embedding_count = match store.count_embeddings_for_index(index, None) {
                Ok(count) => Some(count),
                Err(err) => return (None, None, Some(err.to_string())),
            };
            match store.count() {
                Ok(count) => (embedding_count, Some(count), None),
                Err(err) => (embedding_count, None, Some(err.to_string())),
            }
        }
        Err(err) => (None, None, Some(err.to_string())),
    }
}

fn render_stats(
    stats: tempyr_index::indexer::IndexStats,
    legacy_embedding_count: usize,
    embedding_count: Option<usize>,
    shared_embedding_count: Option<usize>,
    shared_embedding_error: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "node_count": stats.node_count,
                "edge_count": stats.edge_count,
                "fts_entries": stats.fts_entries,
                "embedding_count": embedding_count,
                "legacy_embedding_count": legacy_embedding_count,
                "shared_embedding_count": shared_embedding_count,
                "shared_embedding_error": shared_embedding_error,
                "nodes_by_type": stats.nodes_by_type,
            }))?
        );
        return Ok(());
    }

    println!("Index statistics:");
    println!("  Nodes: {}", stats.node_count);
    println!("  Edges: {}", stats.edge_count);
    println!("  FTS entries: {}", stats.fts_entries);
    match embedding_count {
        Some(count) => println!("  Embeddings (current snapshot): {count}"),
        None => println!(
            "  Embeddings (current snapshot): unavailable ({})",
            shared_embedding_error.as_deref().unwrap_or("unknown error")
        ),
    }
    println!("  Legacy index embeddings: {legacy_embedding_count}");
    match shared_embedding_count {
        Some(count) => println!("  Shared embedding cache entries: {count}"),
        None if shared_embedding_error.is_some() => println!(
            "  Shared embedding cache entries: unavailable ({})",
            shared_embedding_error.as_deref().unwrap_or("unknown error")
        ),
        None => println!("  Shared embedding cache entries: 0"),
    }
    for (node_type, count) in &stats.nodes_by_type {
        println!("  {node_type}: {count}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempyr_core::project as project_mod;

    fn make_ctx(tmp: &Path) -> ProjectContext {
        let tempyr_dir = tmp.join(".tempyr");
        fs::create_dir_all(&tempyr_dir).unwrap();
        fs::create_dir_all(tmp.join("graph")).unwrap();
        fs::write(tempyr_dir.join("schema.toml"), "name = 'x'\n").unwrap();
        let cache = project_mod::cache_layout(tmp, &tempyr_dir);
        ProjectContext {
            root: tmp.to_path_buf(),
            graph_dir: tmp.join("graph"),
            tempyr_dir,
            cache,
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

    fn touch_shared_snapshot(ctx: &ProjectContext) {
        let shared = ctx.shared_snapshot_index_path().unwrap();
        fs::create_dir_all(shared.parent().unwrap()).unwrap();
        fs::write(&shared, b"pretend index").unwrap();
    }

    #[test]
    fn slot_uses_existing_when_snapshot_present_and_not_forced() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = make_ctx(tmp.path());
        touch_shared_snapshot(&ctx);
        let shared = ctx.shared_snapshot_index_path().unwrap();

        let slot = negotiate_rebuild_slot(&ctx, &shared, false, Duration::from_millis(50)).unwrap();

        assert!(matches!(
            slot,
            RebuildSlot::UseExisting {
                built_by_other: false
            }
        ));
    }

    #[test]
    fn slot_takes_lock_to_build_when_snapshot_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = make_ctx(tmp.path());
        let shared = ctx.shared_snapshot_index_path().unwrap();

        let slot = negotiate_rebuild_slot(&ctx, &shared, false, Duration::from_millis(50)).unwrap();

        assert!(matches!(slot, RebuildSlot::Build(_)));
    }

    #[test]
    fn slot_forces_build_even_when_snapshot_present() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = make_ctx(tmp.path());
        touch_shared_snapshot(&ctx);
        let shared = ctx.shared_snapshot_index_path().unwrap();

        // force=true must bypass the short-circuit and take the build lock.
        let slot = negotiate_rebuild_slot(&ctx, &shared, true, Duration::from_millis(50)).unwrap();
        assert!(matches!(slot, RebuildSlot::Build(_)));
    }

    #[test]
    fn slot_blocking_acquire_succeeds_after_other_releases() {
        // Simulate: another process holds the build lock, never publishes a
        // snapshot, then releases. The waiter should escalate from "wait for
        // snapshot" → "block-acquire" → finally take the lock and rebuild.
        let tmp = tempfile::tempdir().unwrap();
        let ctx = make_ctx(tmp.path());
        let shared = ctx.shared_snapshot_index_path().unwrap();

        let other_lock = ctx.try_acquire_snapshot_build_lock().unwrap().unwrap();

        let cache = ctx.cache.clone();
        let snapshot_key = {
            let layout =
                project_mod::IndexLayout::resolve(&ctx.root, &ctx.graph_dir, &ctx.tempyr_dir)
                    .unwrap();
            layout.snapshot_key().unwrap()
        };

        let releaser = std::thread::spawn(move || {
            // Hold long enough that the waiter exhausts the snapshot poll
            // and enters the blocking-acquire loop, then release.
            std::thread::sleep(Duration::from_millis(60));
            drop(other_lock);
            // Keep `cache` and `snapshot_key` alive for the duration so the
            // lock-file path stays stable.
            let _ = (&cache, &snapshot_key);
        });

        // Total budget = 2 * wait_budget = 200ms. Release happens at ~60ms,
        // so the blocking-acquire phase should succeed well within that.
        let slot =
            negotiate_rebuild_slot(&ctx, &shared, false, Duration::from_millis(100)).unwrap();
        assert!(matches!(slot, RebuildSlot::Build(_)));
        releaser.join().unwrap();
    }

    #[test]
    fn slot_uses_snapshot_when_other_publishes_during_wait() {
        // Other process holds the lock, publishes the snapshot during the
        // wait, then releases. The waiter should short-circuit to UseExisting
        // without re-doing the work.
        let tmp = tempfile::tempdir().unwrap();
        let ctx = make_ctx(tmp.path());
        let shared = ctx.shared_snapshot_index_path().unwrap();
        let shared_for_thread = shared.clone();

        let other_lock = ctx.try_acquire_snapshot_build_lock().unwrap().unwrap();

        let publisher = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            fs::create_dir_all(shared_for_thread.parent().unwrap()).unwrap();
            fs::write(&shared_for_thread, b"published").unwrap();
            drop(other_lock);
        });

        let slot =
            negotiate_rebuild_slot(&ctx, &shared, false, Duration::from_millis(500)).unwrap();
        assert!(matches!(
            slot,
            RebuildSlot::UseExisting {
                built_by_other: true
            }
        ));
        publisher.join().unwrap();
    }

    #[test]
    fn slot_blocking_acquire_times_out_when_lock_never_releases() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = make_ctx(tmp.path());
        let shared = ctx.shared_snapshot_index_path().unwrap();

        let _held = ctx.try_acquire_snapshot_build_lock().unwrap().unwrap();

        // With no publisher and no release, total time bounded by 2*budget.
        // Use a small budget so the test is fast.
        let err =
            negotiate_rebuild_slot(&ctx, &shared, false, Duration::from_millis(60)).unwrap_err();
        assert!(
            err.to_string()
                .contains("Could not acquire snapshot build lock"),
            "unexpected error: {err}"
        );
    }
}
