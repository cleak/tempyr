use crate::config::ProjectContext;

use tempyr_core::graph::Graph;
use tempyr_index::embeddings::{self, EmbeddingStore};
use tempyr_index::hybrid::{HybridResult, RetrievalConfig};
use tempyr_index::indexer::Index;
use tempyr_index::semantic::SemanticSearchEngine;
use tempyr_index::vector::VectorSearchResult;

/// Runtime state for commands that need semantic search over the graph.
pub struct SemanticSearchRuntime {
    engine: SemanticSearchEngine,
    runtime: tokio::runtime::Runtime,
}

impl SemanticSearchRuntime {
    pub fn new(ctx: &ProjectContext) -> anyhow::Result<Self> {
        let index_path = ctx.queryable_index_path()?;
        let index = Index::open(&index_path)?;
        let resolved = ctx.resolved_embedding_config()?;
        let store_path = ctx.embedding_store_path(
            &resolved.provider,
            resolved.model.as_deref(),
            Some(resolved.dimensions),
        );
        let store = EmbeddingStore::open_or_create(&store_path)?;
        let provider = embeddings::create_provider_from_resolved(&resolved)?;
        let runtime = tokio::runtime::Runtime::new()?;
        let engine = SemanticSearchEngine::new(index, store, provider);

        Ok(Self { engine, runtime })
    }

    pub fn vector_search(
        &mut self,
        graph: &Graph,
        query: &str,
        max_results: usize,
        node_type: Option<&str>,
        min_similarity: Option<f64>,
    ) -> anyhow::Result<Vec<VectorSearchResult>> {
        Ok(self.runtime.block_on(self.engine.vector_search(
            graph,
            query,
            max_results,
            node_type,
            min_similarity,
        ))?)
    }

    pub fn hybrid_retrieve(
        &mut self,
        graph: &Graph,
        query: &str,
        root: Option<&str>,
        config: RetrievalConfig,
    ) -> anyhow::Result<Vec<HybridResult>> {
        Ok(self
            .runtime
            .block_on(self.engine.hybrid_retrieve(graph, query, root, config))?)
    }
}
