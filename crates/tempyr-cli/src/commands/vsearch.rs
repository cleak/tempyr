use crate::commands::semantic::SemanticSearchRuntime;
use crate::config::ProjectContext;
use tempyr_core::graph::Graph;

pub fn run(
    ctx: &ProjectContext,
    query: &str,
    max_results: usize,
    node_type: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let graph = Graph::load_from_directory(&ctx.graph_dir, ctx.schema.clone())?;
    let mut semantic_search = SemanticSearchRuntime::new(ctx)?;
    let results = semantic_search.vector_search(&graph, query, max_results, node_type, None)?;

    if json {
        let json_results: Vec<_> = results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "node_id": r.node_id,
                    "similarity": r.similarity,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json_results)?);
    } else {
        if results.is_empty() {
            println!("No results for: {query}");
            return Ok(());
        }
        for result in &results {
            println!("{} (similarity: {:.3})", result.node_id, result.similarity);
        }
    }

    Ok(())
}
