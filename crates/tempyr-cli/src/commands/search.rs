use crate::config::ProjectContext;
use tempyr_index::fts::MetadataFilter;
use tempyr_index::indexer::Index;

pub fn run(
    ctx: &ProjectContext,
    query: &str,
    max_results: usize,
    node_type: Option<&str>,
    status: Option<&str>,
    owner: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let index_path = ctx.current_index_path()?;
    let index = Index::open(&index_path)?;
    let filter = MetadataFilter {
        node_type,
        status,
        owner,
    };
    let results = index.search_fts_with_metadata(query, &filter, max_results)?;

    if json {
        let json_results: Vec<_> = results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "node_id": r.node_id,
                    "title": r.title,
                    "node_type": r.node_type,
                    "status": r.status,
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
        let status_str = result.status.as_deref().unwrap_or("-");
        println!(
            "{} ({}, {}) - {}",
            result.node_id, result.node_type, status_str, result.title
        );
        if !result.snippet.is_empty() {
            println!("  {}", result.snippet);
        }
    }

    Ok(())
}
