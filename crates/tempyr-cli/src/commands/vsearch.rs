use crate::config::ProjectContext;
use tempyr_index::embeddings::{self, EmbeddingStore, InputType};
use tempyr_index::indexer::Index;

pub fn run(
    ctx: &ProjectContext,
    query: &str,
    max_results: usize,
    node_type: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let index_path = ctx.current_index_path()?;
    let index = Index::open(&index_path)?;
    let resolved = ctx.resolved_embedding_config()?;
    let store_path = ctx.embedding_store_path(
        &resolved.provider,
        resolved.model.as_deref(),
        Some(resolved.dimensions),
    );
    let store = EmbeddingStore::open_or_create(&store_path)?;

    // Check if embeddings exist
    let store_embedding_count = store.count_embeddings_for_index(&index, node_type)?;
    let legacy_embedding_count = index.embedding_count_for_node_type(node_type)?;
    let use_legacy_index_embeddings = store_embedding_count == 0 && legacy_embedding_count > 0;
    if store_embedding_count == 0 && !use_legacy_index_embeddings {
        anyhow::bail!(
            "No embeddings found. Run `tempyr index rebuild` with an embedding \
             API key set (VOYAGE_API_KEY or GEMINI_API_KEY)."
        );
    }

    // Embed the query
    let provider = embeddings::create_provider_from_resolved(&resolved)?;

    let rt = tokio::runtime::Runtime::new()?;
    let query_embeddings = rt.block_on(provider.embed(&[query.to_string()], InputType::Query))?;

    if query_embeddings.is_empty() {
        anyhow::bail!("Failed to embed query");
    }

    let results = if use_legacy_index_embeddings {
        index.vector_search(&query_embeddings[0], max_results, node_type)?
    } else {
        store.vector_search(&index, &query_embeddings[0], max_results, node_type)?
    };

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
