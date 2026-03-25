use crate::config::ProjectContext;
use tempyr_index::embeddings::{self, EmbeddingConfig, InputType};
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

    // Check if embeddings exist
    let emb_count = index.embedding_count()?;
    if emb_count == 0 {
        anyhow::bail!(
            "No embeddings found. Run `tempyr index rebuild` with an embedding \
             API key set (VOYAGE_API_KEY or GEMINI_API_KEY)."
        );
    }

    // Embed the query
    let config = load_embedding_config(ctx);
    let provider = embeddings::create_provider(&config)?;

    let rt = tokio::runtime::Runtime::new()?;
    let query_embeddings = rt.block_on(provider.embed(&[query.to_string()], InputType::Query))?;

    if query_embeddings.is_empty() {
        anyhow::bail!("Failed to embed query");
    }

    let results = index.vector_search(&query_embeddings[0], max_results, node_type)?;

    if json {
        let json_results: Vec<_> = results.iter().map(|r| {
            serde_json::json!({
                "node_id": r.node_id,
                "similarity": r.similarity,
            })
        }).collect();
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

fn load_embedding_config(ctx: &ProjectContext) -> EmbeddingConfig {
    let config_path = ctx.tempyr_dir.join("config.toml");
    if let Ok(content) = std::fs::read_to_string(&config_path) {
        if let Ok(table) = content.parse::<toml::Table>() {
            if let Some(emb) = table.get("embedding") {
                if let Ok(config) = emb.clone().try_into::<EmbeddingConfig>() {
                    return config;
                }
            }
        }
    }
    EmbeddingConfig::default()
}
