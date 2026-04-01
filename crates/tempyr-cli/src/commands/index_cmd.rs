use crate::config::ProjectContext;
use tempyr_core::graph::Graph;
use tempyr_index::embeddings::{self, EmbeddingConfig, EmbeddingStore};
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
    let config = load_embedding_config(ctx);
    let store_path =
        ctx.embedding_store_path(&config.provider, config.model.as_deref(), config.dimensions);
    let legacy_embedding_count = index.embedding_count().unwrap_or(0);
    let (embedding_count, shared_embedding_count, shared_embedding_error) = if store_path.exists() {
        match EmbeddingStore::open_or_create(&store_path) {
            Ok(store) => (
                store.count_embeddings_for_index(&index, None).ok(),
                store.count().ok(),
                None,
            ),
            Err(err) => (None, None, Some(err.to_string())),
        }
    } else {
        (None, None, None)
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "node_count": stats.node_count,
                "edge_count": stats.edge_count,
                "fts_entries": stats.fts_entries,
                "embedding_count": embedding_count.unwrap_or(legacy_embedding_count),
                "legacy_embedding_count": legacy_embedding_count,
                "shared_embedding_count": shared_embedding_count,
                "shared_embedding_error": shared_embedding_error,
                "nodes_by_type": stats.nodes_by_type,
            }))?
        );
    } else {
        println!("Index statistics:");
        println!("  Nodes: {}", stats.node_count);
        println!("  Edges: {}", stats.edge_count);
        println!("  FTS entries: {}", stats.fts_entries);
        println!(
            "  Embeddings (current snapshot): {}",
            embedding_count.unwrap_or(legacy_embedding_count)
        );
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
    }

    Ok(())
}

/// Try to embed graph nodes. Returns error (not fatal) if no API key is available.
fn try_embed(graph: &Graph, ctx: &ProjectContext) -> anyhow::Result<embeddings::EmbedStats> {
    let config = load_embedding_config(ctx);
    let provider = embeddings::create_provider(&config)?;
    let store_path =
        ctx.embedding_store_path(&config.provider, config.model.as_deref(), config.dimensions);
    let store = EmbeddingStore::open_or_create(&store_path)?;

    let rt = tokio::runtime::Runtime::new()?;
    let stats = rt.block_on(embeddings::embed_graph(&store, graph, provider.as_ref()))?;
    Ok(stats)
}

/// Load embedding config from .tempyr/config.toml, falling back to defaults.
fn load_embedding_config(ctx: &ProjectContext) -> EmbeddingConfig {
    let config_path = ctx.tempyr_dir.join("config.toml");
    if let Ok(content) = std::fs::read_to_string(&config_path)
        && let Ok(table) = content.parse::<toml::Table>()
        && let Some(emb) = table.get("embedding")
        && let Ok(config) = emb.clone().try_into::<EmbeddingConfig>()
    {
        return config;
    }
    EmbeddingConfig::default()
}
