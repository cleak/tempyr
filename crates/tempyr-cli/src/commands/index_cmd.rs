use std::path::Path;

use crate::config::ProjectContext;
use tempyr_core::graph::Graph;
use tempyr_index::embeddings::{self, EmbeddingStore};
use tempyr_index::indexer::Index;

pub fn run_rebuild(ctx: &ProjectContext, json: bool) -> anyhow::Result<()> {
    let graph = Graph::load_from_directory(&ctx.graph_dir, ctx.schema.clone())?;
    let (snapshot_key, index_path) = ctx.ensure_active_index_seeded()?;

    // Remove existing index
    if index_path.exists() {
        std::fs::remove_file(&index_path)?;
    }
    if let Some(parent) = index_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let index = Index::create(&index_path)?;
    let stats = index.rebuild(&graph)?;

    // Try to generate embeddings
    let embed_result = try_embed(&graph, ctx);
    ctx.write_active_snapshot_key(&snapshot_key)?;
    ctx.publish_active_snapshot(&snapshot_key)?;

    if json {
        let mut result = serde_json::json!({
            "node_count": stats.node_count,
            "edge_count": stats.edge_count,
            "fts_entries": stats.fts_entries,
            "nodes_by_type": stats.nodes_by_type,
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
        match embed_result {
            Ok(es) => println!("{es}"),
            Err(e) => println!("Embeddings skipped: {e}"),
        }
    }

    Ok(())
}

pub fn run_update(ctx: &ProjectContext, json: bool) -> anyhow::Result<()> {
    let graph = Graph::load_from_directory(&ctx.graph_dir, ctx.schema.clone())?;
    let (snapshot_key, index_path) = ctx.ensure_active_index_seeded()?;

    if !index_path.exists() {
        return run_rebuild(ctx, json);
    }

    let index = Index::open(&index_path)?;
    let stats = index.incremental_update(&graph)?;

    // Try to generate embeddings for new/changed nodes
    let embed_result = try_embed(&graph, ctx);
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
        match embed_result {
            Ok(es) => println!("{es}"),
            Err(e) => println!("Embeddings skipped: {e}"),
        }
    }

    Ok(())
}

pub fn run_stats(ctx: &ProjectContext, json: bool) -> anyhow::Result<()> {
    let index_path = ctx.current_index_path()?;

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
