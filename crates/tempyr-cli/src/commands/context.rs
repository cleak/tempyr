use crate::commands::semantic::SemanticSearchRuntime;
use crate::config::ProjectContext;
use tempyr_core::graph::Graph;
use tempyr_index::hybrid::RetrievalConfig;

pub fn run(
    ctx: &ProjectContext,
    query: &str,
    root: Option<&str>,
    budget: usize,
    json: bool,
) -> anyhow::Result<()> {
    let graph = Graph::load_from_directory(&ctx.graph_dir, ctx.schema.clone())?;
    let config = RetrievalConfig {
        token_budget: budget,
        ..RetrievalConfig::standard()
    };

    let mut semantic_search = SemanticSearchRuntime::new(ctx)?;
    let results = semantic_search.hybrid_retrieve(&graph, query, root, config)?;

    if json {
        let json_results: Vec<_> = results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "node_id": r.node_id,
                    "combined_score": r.combined_score,
                    "structural_score": r.structural_score,
                    "bm25_score": r.bm25_score,
                    "vector_score": r.vector_score,
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
        let structural = result
            .structural_score
            .map(|s| format!(" struct={s:.2}"))
            .unwrap_or_default();
        let bm25 = result
            .bm25_score
            .map(|s| format!(" bm25={s:.2}"))
            .unwrap_or_default();
        let vector = result
            .vector_score
            .map(|s| format!(" vec={s:.2}"))
            .unwrap_or_default();
        println!(
            "{} (score={:.3}{structural}{bm25}{vector})",
            result.node_id, result.combined_score
        );
    }

    Ok(())
}
