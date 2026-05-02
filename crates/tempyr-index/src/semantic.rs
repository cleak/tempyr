use tempyr_core::graph::Graph;

use crate::embeddings::{self, EmbeddingProvider, EmbeddingStore, InputType};
use crate::hybrid::{HybridResult, RetrievalConfig, hybrid_retrieve};
use crate::indexer::Index;
use crate::vector::VectorSearchResult;
use crate::{IndexError, Result};

/// Provider-backed semantic retrieval over a graph index.
pub struct SemanticSearchEngine {
    index: Index,
    store: EmbeddingStore,
    provider: Box<dyn EmbeddingProvider>,
    embeddings_ready: bool,
}

impl SemanticSearchEngine {
    pub fn new(index: Index, store: EmbeddingStore, provider: Box<dyn EmbeddingProvider>) -> Self {
        Self {
            index,
            store,
            provider,
            embeddings_ready: false,
        }
    }

    pub async fn ensure_embeddings(&mut self, graph: &Graph) -> Result<()> {
        if self.embeddings_ready {
            return Ok(());
        }

        embeddings::embed_graph(&self.store, graph, self.provider.as_ref()).await?;
        self.embeddings_ready = true;
        Ok(())
    }

    pub async fn vector_search(
        &mut self,
        graph: &Graph,
        query: &str,
        max_results: usize,
        node_type: Option<&str>,
        min_similarity: Option<f64>,
    ) -> Result<Vec<VectorSearchResult>> {
        self.ensure_embeddings(graph).await?;
        let query_embedding = self.embed_query(query).await?;

        let mut results =
            self.store
                .vector_search(&self.index, &query_embedding, max_results, node_type)?;
        if let Some(min_similarity) = min_similarity {
            results.retain(|result| result.similarity >= min_similarity);
        }
        Ok(results)
    }

    pub async fn hybrid_retrieve(
        &mut self,
        graph: &Graph,
        query: &str,
        root: Option<&str>,
        mut config: RetrievalConfig,
    ) -> Result<Vec<HybridResult>> {
        self.ensure_embeddings(graph).await?;
        config.query_embedding = Some(self.embed_query(query).await?);
        hybrid_retrieve(&self.index, graph, query, root, &config, Some(&self.store))
    }

    async fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        let query_embeddings = self
            .provider
            .embed(&[query.to_string()], InputType::Query)
            .await?;
        query_embeddings.into_iter().next().ok_or_else(|| {
            IndexError::General("Embedding provider returned no vector for the query.".to_string())
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::PathBuf;

    use async_trait::async_trait;
    use tempyr_core::node::parse_node;
    use tempyr_core::schema::Schema;

    use super::*;
    use crate::embeddings::EmbeddingProvider;

    fn make_schema() -> Schema {
        r#"
[meta]
version = "1"
description = "test"

[node_types.feature]
description = "Feature"
directory = "features"
required_fields = []
optional_fields = ["status"]
allowed_statuses = ["draft"]
allowed_edges = []

[node_types.insight]
description = "Insight"
directory = "insights"
required_fields = []
optional_fields = []
allowed_edges = []

[edge_types]
"#
        .parse()
        .unwrap()
    }

    fn make_graph() -> Graph {
        let mut graph = Graph::new(make_schema());
        graph.add_node(
            parse_node(
                "---\nid: feat-a\ntype: feature\nstatus: draft\n---\n# Search Topic\n\nFind related insight.\n",
                PathBuf::from("graph/features/feat-a.md"),
            )
            .unwrap(),
        );
        graph.add_node(
            parse_node(
                "---\nid: insight-a\ntype: insight\n---\n# Related Insight\n\nRelevant context.\n",
                PathBuf::from("graph/insights/insight-a.md"),
            )
            .unwrap(),
        );
        graph
    }

    struct FixedProvider;

    #[async_trait]
    impl EmbeddingProvider for FixedProvider {
        async fn embed(&self, texts: &[String], input_type: InputType) -> Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|text| match input_type {
                    InputType::Query => vec![0.0, 1.0],
                    InputType::Document if text.contains("Related Insight") => vec![0.0, 1.0],
                    InputType::Document => vec![1.0, 0.0],
                })
                .collect())
        }

        fn dimensions(&self) -> usize {
            2
        }

        fn name(&self) -> &str {
            "fixed"
        }
    }

    #[test]
    fn vector_search_embeds_missing_graph_nodes_before_searching() {
        let tmp = tempfile::tempdir().unwrap();
        let graph = make_graph();
        let index = Index::create_in_memory().unwrap();
        index.rebuild(&graph).unwrap();
        let store = EmbeddingStore::open_or_create(&tmp.path().join("embeddings.db")).unwrap();
        let provider = Box::new(FixedProvider);
        let mut engine = SemanticSearchEngine::new(index, store, provider);

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let results = runtime
            .block_on(engine.vector_search(&graph, "related", 10, Some("insight"), Some(0.7)))
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].node_id, "insight-a");
    }

    #[test]
    fn hybrid_retrieve_includes_vector_scores_after_embedding() {
        let tmp = tempfile::tempdir().unwrap();
        let graph = make_graph();
        let index = Index::create_in_memory().unwrap();
        index.rebuild(&graph).unwrap();
        let store = EmbeddingStore::open_or_create(&tmp.path().join("embeddings.db")).unwrap();
        let provider = Box::new(FixedProvider);
        let mut engine = SemanticSearchEngine::new(index, store, provider);

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let results = runtime
            .block_on(engine.hybrid_retrieve(&graph, "related", None, RetrievalConfig::standard()))
            .unwrap();

        assert!(
            results
                .iter()
                .any(|result| { result.node_id == "insight-a" && result.vector_score.is_some() })
        );
        assert_eq!(
            results
                .iter()
                .map(|result| result.node_id.as_str())
                .collect::<HashSet<_>>()
                .len(),
            results.len()
        );
    }
}
