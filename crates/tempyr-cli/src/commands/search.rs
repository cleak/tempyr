use crate::config::ProjectContext;
use tempyr_index::indexer::Index;

pub fn run(
    ctx: &ProjectContext,
    query: &str,
    max_results: usize,
    node_type: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let index_path = ctx.index_path();
    if !index_path.exists() {
        anyhow::bail!("Index not found. Run `tempyr index rebuild` first.");
    }

    let index = Index::open(&index_path)?;
    let results = index.search_fts_filtered(query, node_type, max_results)?;

    if json {
        let json_results: Vec<_> = results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "node_id": r.node_id,
                    "title": r.title,
                    "node_type": r.node_type,
                    "score": r.score,
                    "snippet": r.snippet,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json_results)?);
        return Ok(());
    }

    if results.is_empty() {
        println!("No results for: {query}");
        return Ok(());
    }

    for result in &results {
        println!("{} ({}) - {}", result.node_id, result.node_type, result.title);
        if !result.snippet.is_empty() {
            println!("  {}", result.snippet);
        }
    }

    Ok(())
}
