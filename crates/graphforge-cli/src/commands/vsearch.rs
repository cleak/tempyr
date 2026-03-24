use crate::config::ProjectContext;
use graphforge_index::indexer::Index;

pub fn run(
    ctx: &ProjectContext,
    query: &str,
    max_results: usize,
    node_type: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let index_path = ctx.index_path();
    if !index_path.exists() {
        anyhow::bail!("Index not found. Run `graphforge index rebuild` first.");
    }

    let index = Index::open(&index_path)?;

    // Check if embeddings exist
    let emb_count = index.embedding_count()?;
    if emb_count == 0 {
        anyhow::bail!(
            "No embeddings found. Vector search requires embeddings to be generated.\n\
             Embeddings are populated when Claude Code uses the MCP server's graph_context tool \
             with an embedding API configured in .graphforge/config.toml."
        );
    }

    // For CLI without an embedding API, we can't embed the query.
    // This command works when embeddings + query embedding are available via MCP.
    // For now, fall back to FTS search with a note.
    println!("Note: CLI vector search requires a query embedding. Falling back to FTS search.");
    println!("Use the MCP server (graph_vsearch tool) for true semantic search.\n");

    let results = index.search_fts_filtered(query, node_type, max_results)?;

    if json {
        let json_results: Vec<_> = results.iter().map(|r| {
            serde_json::json!({
                "node_id": r.node_id,
                "title": r.title,
                "node_type": r.node_type,
                "score": r.score,
            })
        }).collect();
        println!("{}", serde_json::to_string_pretty(&json_results)?);
    } else {
        if results.is_empty() {
            println!("No results for: {query}");
            return Ok(());
        }
        for result in &results {
            println!("{} ({}) - {}", result.node_id, result.node_type, result.title);
        }
    }

    Ok(())
}
