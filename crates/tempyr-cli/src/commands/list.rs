use crate::config::ProjectContext;
use tempyr_index::fts::MetadataFilter;
use tempyr_index::indexer::Index;

pub fn run(
    ctx: &ProjectContext,
    node_type: Option<&str>,
    status: Option<&str>,
    owner: Option<&str>,
    max_results: usize,
    json: bool,
) -> anyhow::Result<()> {
    let index_path = ctx.queryable_index_path()?;
    let index = Index::open(&index_path)?;
    let filter = MetadataFilter {
        node_type,
        status,
        owner,
    };
    let results = index.query_by_metadata(&filter, max_results)?;

    if json {
        let json_results: Vec<_> = results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "node_id": r.node_id,
                    "title": r.title,
                    "node_type": r.node_type,
                    "status": r.status,
                    "owner": r.owner,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json_results)?);
        return Ok(());
    }

    if results.is_empty() {
        println!("No nodes match the given filters.");
        return Ok(());
    }

    for result in &results {
        let status_str = result.status.as_deref().unwrap_or("-");
        let owner_str = result.owner.as_deref().unwrap_or("-");
        println!(
            "{} ({}, {}, owner: {}) - {}",
            result.node_id, result.node_type, status_str, owner_str, result.title
        );
    }

    Ok(())
}
