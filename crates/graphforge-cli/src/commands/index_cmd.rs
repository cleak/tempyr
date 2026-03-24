use crate::config::ProjectContext;
use graphforge_core::graph::Graph;
use graphforge_index::indexer::Index;

pub fn run_rebuild(ctx: &ProjectContext, json: bool) -> anyhow::Result<()> {
    let graph = Graph::load_from_directory(&ctx.graph_dir, ctx.schema.clone())?;
    let index_path = ctx.index_path();

    // Remove existing index
    if index_path.exists() {
        std::fs::remove_file(&index_path)?;
    }

    let index = Index::create(&index_path)?;
    let stats = index.rebuild(&graph)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
            "node_count": stats.node_count,
            "edge_count": stats.edge_count,
            "fts_entries": stats.fts_entries,
            "nodes_by_type": stats.nodes_by_type,
        }))?);
    } else {
        println!("Index rebuilt: {} nodes, {} edges, {} FTS entries",
            stats.node_count, stats.edge_count, stats.fts_entries);
        for (node_type, count) in &stats.nodes_by_type {
            println!("  {node_type}: {count}");
        }
    }

    Ok(())
}

pub fn run_update(ctx: &ProjectContext, json: bool) -> anyhow::Result<()> {
    let graph = Graph::load_from_directory(&ctx.graph_dir, ctx.schema.clone())?;
    let index_path = ctx.index_path();

    if !index_path.exists() {
        // No existing index — do a full rebuild
        return run_rebuild(ctx, json);
    }

    let index = Index::open(&index_path)?;
    let stats = index.incremental_update(&graph)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
            "node_count": stats.node_count,
            "edge_count": stats.edge_count,
            "fts_entries": stats.fts_entries,
        }))?);
    } else {
        println!("Index updated: {} nodes, {} edges", stats.node_count, stats.edge_count);
    }

    Ok(())
}

pub fn run_stats(ctx: &ProjectContext, json: bool) -> anyhow::Result<()> {
    let index_path = ctx.index_path();
    if !index_path.exists() {
        anyhow::bail!("Index not found. Run `graphforge index rebuild` first.");
    }

    let index = Index::open(&index_path)?;
    let stats = index.stats()?;

    if json {
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
            "node_count": stats.node_count,
            "edge_count": stats.edge_count,
            "fts_entries": stats.fts_entries,
            "nodes_by_type": stats.nodes_by_type,
        }))?);
    } else {
        println!("Index statistics:");
        println!("  Nodes: {}", stats.node_count);
        println!("  Edges: {}", stats.edge_count);
        println!("  FTS entries: {}", stats.fts_entries);
        for (node_type, count) in &stats.nodes_by_type {
            println!("  {node_type}: {count}");
        }
    }

    Ok(())
}
