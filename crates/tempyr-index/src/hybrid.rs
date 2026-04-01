use chrono::Utc;

use tempyr_core::graph::Graph;
use tempyr_core::traverse::bfs_scored;

use crate::Result;
use crate::embeddings::EmbeddingStore;
use crate::indexer::Index;

/// Configuration for the hybrid retrieval pipeline.
#[derive(Debug, Clone, Default)]
pub struct RetrievalConfig {
    pub structural_weight: f64,
    pub bm25_weight: f64,
    pub vector_weight: f64,
    pub recency_boost_days: i64,
    pub recency_boost_value: f64,
    pub token_budget: usize,
    /// If set, vector similarity search is included in the pipeline.
    pub query_embedding: Option<Vec<f32>>,
}

impl RetrievalConfig {
    pub fn standard() -> Self {
        Self {
            structural_weight: 0.5,
            bm25_weight: 0.25,
            vector_weight: 0.25,
            recency_boost_days: 7,
            recency_boost_value: 0.1,
            token_budget: 8000,
            query_embedding: None,
        }
    }
}

/// A single result from the hybrid retrieval pipeline.
#[derive(Debug, Clone)]
pub struct HybridResult {
    pub node_id: String,
    pub combined_score: f64,
    pub structural_score: Option<f64>,
    pub bm25_score: Option<f64>,
    pub vector_score: Option<f64>,
}

type ScoreTriplet = (Option<f64>, Option<f64>, Option<f64>);
type ScoreMap = std::collections::HashMap<String, ScoreTriplet>;

/// Run the full hybrid retrieval pipeline.
///
/// Combines structural traversal (if root_id provided), BM25 full-text search,
/// and vector similarity (deferred — always None for now).
pub fn hybrid_retrieve(
    index: &Index,
    graph: &Graph,
    query: &str,
    root_id: Option<&str>,
    config: &RetrievalConfig,
    embedding_store: Option<&EmbeddingStore>,
) -> Result<Vec<HybridResult>> {
    let mut scores: ScoreMap = ScoreMap::new();

    // Step 1: Structural retrieval (if root provided)
    if let Some(root) = root_id {
        let structural = bfs_scored(graph, root, 2);
        for (node_id, score) in structural {
            scores.entry(node_id).or_insert((None, None, None)).0 = Some(score);
        }
    }

    // Step 2: BM25 full-text search
    let fts_results = index.search_fts(query, 30)?;
    if !fts_results.is_empty() {
        // Normalize BM25 scores to 0.0..1.0
        // FTS5 rank is negative (lower = better), so we invert
        let min_score = fts_results
            .iter()
            .map(|r| r.score)
            .fold(f64::INFINITY, f64::min);
        let max_score = fts_results
            .iter()
            .map(|r| r.score)
            .fold(f64::NEG_INFINITY, f64::max);
        let range = max_score - min_score;

        for result in &fts_results {
            let normalized = if range.abs() < f64::EPSILON {
                1.0
            } else {
                // Invert: best rank (most negative) → highest score
                1.0 - (result.score - min_score) / range
            };
            scores
                .entry(result.node_id.clone())
                .or_insert((None, None, None))
                .1 = Some(normalized);
        }
    }

    // Step 3: Vector similarity search (if query embedding provided)
    if let Some(ref query_emb) = config.query_embedding {
        let vec_results = if let Some(store) = embedding_store {
            store.vector_search(index, query_emb, 30, None)?
        } else {
            index.vector_search(query_emb, 30, None)?
        };
        for result in &vec_results {
            // Similarity is already 0.0..1.0 (cosine similarity)
            let normalized = result.similarity.max(0.0);
            scores
                .entry(result.node_id.clone())
                .or_insert((None, None, None))
                .2 = Some(normalized);
        }
    }

    // Step 4: Merge and rank
    let has_vector = config.query_embedding.is_some();
    // When vector is not available, redistribute its weight to structural and BM25
    let effective_structural = if has_vector {
        config.structural_weight
    } else {
        config.structural_weight + config.vector_weight / 2.0
    };
    let effective_bm25 = if has_vector {
        config.bm25_weight
    } else {
        config.bm25_weight + config.vector_weight / 2.0
    };
    let effective_vector = if has_vector {
        config.vector_weight
    } else {
        0.0
    };

    let now = Utc::now();

    let mut results: Vec<HybridResult> = scores
        .into_iter()
        .map(|(node_id, (structural, bm25, vector))| {
            let mut combined = structural.unwrap_or(0.0) * effective_structural
                + bm25.unwrap_or(0.0) * effective_bm25
                + vector.unwrap_or(0.0) * effective_vector;

            // Recency boost
            if let Ok(Some(updated_str)) = index.get_updated_at(&node_id)
                && let Ok(updated) = chrono::DateTime::parse_from_rfc3339(&updated_str)
            {
                let days_ago = (now - updated.to_utc()).num_days();
                if days_ago <= config.recency_boost_days {
                    combined += config.recency_boost_value;
                }
            }

            // Type priority boost: decisions and constraints get +0.05
            if let Ok(Some(node_type)) = index.get_node_type(&node_id)
                && (node_type == "decision" || node_type == "constraint")
            {
                combined += 0.05;
            }

            HybridResult {
                node_id,
                combined_score: combined,
                structural_score: structural,
                bm25_score: bm25,
                vector_score: vector,
            }
        })
        .collect();

    // Step 5: Sort by combined score descending
    results.sort_by(|a, b| {
        b.combined_score
            .partial_cmp(&a.combined_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Step 6: Budget enforcement
    results = apply_budget(results, index, config.token_budget)?;

    Ok(results)
}

/// Greedily fill results until the token budget is exhausted.
fn apply_budget(
    results: Vec<HybridResult>,
    index: &Index,
    budget: usize,
) -> Result<Vec<HybridResult>> {
    let mut output = Vec::new();
    let mut tokens_used = 0usize;

    for result in results {
        let node_tokens = estimate_tokens(index, &result.node_id)?;
        if tokens_used + node_tokens > budget && !output.is_empty() {
            break;
        }
        tokens_used += node_tokens;
        output.push(result);
    }

    Ok(output)
}

/// Estimate token count for a node: len(title + body) / 4.
fn estimate_tokens(index: &Index, node_id: &str) -> Result<usize> {
    let body = index.get_body_text(node_id)?.unwrap_or_default();
    let title = index.get_title_text(node_id)?.unwrap_or_default();
    Ok((title.len() + body.len()) / 4)
}

impl Index {
    /// Get the title of a node from the index (for token estimation).
    pub fn get_title_text(&self, node_id: &str) -> Result<Option<String>> {
        let result =
            self.conn
                .query_row("SELECT title FROM nodes WHERE id = ?1", [node_id], |row| {
                    row.get(0)
                });
        match result {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embeddings::EmbeddingStore;
    use std::path::{Path, PathBuf};
    use tempyr_core::graph::Graph;
    use tempyr_core::node::parse_node;
    use tempyr_core::schema::Schema;

    fn make_schema() -> Schema {
        let schema_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("schema/default-schema.toml");
        Schema::load(&schema_path).unwrap()
    }

    fn build_graph_and_index() -> (Graph, Index) {
        let mut graph = Graph::new(make_schema());

        let feat = "---\nid: feat-replay\ntype: feature\nstatus: draft\nowner: caleb\nupdated: 2026-03-23T10:00:00Z\nedges:\n  - target: decision-storage\n    type: depends_on\n---\n# Session Replay\n\nCapture and replay user sessions.\n";
        let decision = "---\nid: decision-storage\ntype: decision\nstatus: decided\nupdated: 2026-03-23T10:00:00Z\nedges:\n  - target: feat-replay\n    type: decision_for\n---\n# Storage Backend\n\nUse ClickHouse for replay storage.\n";
        let task = "---\nid: task-ingestion\ntype: task\nstatus: backlog\nupdated: 2026-01-01T00:00:00Z\n---\n# Ingestion Pipeline\n\nBuild the session replay ingestion pipeline.\n";

        graph.add_node(parse_node(feat, PathBuf::from("f.md")).unwrap());
        graph.add_node(parse_node(decision, PathBuf::from("d.md")).unwrap());
        graph.add_node(parse_node(task, PathBuf::from("t.md")).unwrap());

        let index = Index::create_in_memory().unwrap();
        index.rebuild(&graph).unwrap();

        (graph, index)
    }

    #[test]
    fn test_hybrid_bm25_only() {
        let (graph, index) = build_graph_and_index();
        let config = RetrievalConfig::standard();

        let results =
            hybrid_retrieve(&index, &graph, "replay sessions", None, &config, None).unwrap();
        assert!(!results.is_empty());
        // All results should have bm25_score set, no structural
        for r in &results {
            assert!(r.structural_score.is_none());
        }
    }

    #[test]
    fn test_hybrid_structural_only() {
        let (graph, index) = build_graph_and_index();
        let config = RetrievalConfig::standard();

        // Use root but a query that matches nothing
        let results = hybrid_retrieve(
            &index,
            &graph,
            "xyznonexistent",
            Some("feat-replay"),
            &config,
            None,
        )
        .unwrap();

        assert!(!results.is_empty());
        // Should have structural scores
        let root = results.iter().find(|r| r.node_id == "feat-replay").unwrap();
        assert!(root.structural_score.is_some());
    }

    #[test]
    fn test_hybrid_combined() {
        let (graph, index) = build_graph_and_index();
        let config = RetrievalConfig::standard();

        let results = hybrid_retrieve(
            &index,
            &graph,
            "storage replay",
            Some("feat-replay"),
            &config,
            None,
        )
        .unwrap();

        assert!(!results.is_empty());
        // Both feat-replay and decision-storage should appear (structural + BM25)
        let ids: Vec<_> = results.iter().map(|r| r.node_id.as_str()).collect();
        assert!(ids.contains(&"feat-replay"));
        assert!(ids.contains(&"decision-storage"));
        // feat-replay should have both structural and bm25 scores
        let feat = results.iter().find(|r| r.node_id == "feat-replay").unwrap();
        assert!(feat.structural_score.is_some());
        assert!(feat.bm25_score.is_some());
    }

    #[test]
    fn test_type_priority_boost() {
        let (graph, index) = build_graph_and_index();
        let config = RetrievalConfig::standard();

        let results = hybrid_retrieve(&index, &graph, "storage", None, &config, None).unwrap();

        // decision-storage should get a +0.05 type boost
        let decision = results
            .iter()
            .find(|r| r.node_id == "decision-storage")
            .unwrap();
        assert!(decision.combined_score > 0.0);
    }

    #[test]
    fn test_budget_enforcement() {
        let (graph, index) = build_graph_and_index();
        let config = RetrievalConfig {
            token_budget: 10, // Very small budget — only 1 node should fit
            ..RetrievalConfig::standard()
        };

        let results = hybrid_retrieve(
            &index,
            &graph,
            "replay storage ingestion",
            None,
            &config,
            None,
        )
        .unwrap();

        // With a budget of 10 tokens, at most 1-2 nodes should fit
        assert!(results.len() <= 2);
    }

    #[test]
    fn test_hybrid_uses_shared_embedding_store_when_query_embedding_is_present() {
        let (graph, index) = build_graph_and_index();
        let tmp = tempfile::tempdir().unwrap();
        let store = EmbeddingStore::open_or_create(&tmp.path().join("embeddings.db")).unwrap();

        let replay_hash = index.get_content_hash("feat-replay").unwrap().unwrap();
        let decision_hash = index.get_content_hash("decision-storage").unwrap().unwrap();
        store.store_embedding(&replay_hash, &[1.0, 0.0]).unwrap();
        store.store_embedding(&decision_hash, &[0.0, 1.0]).unwrap();

        let config = RetrievalConfig {
            query_embedding: Some(vec![1.0, 0.0]),
            ..RetrievalConfig::standard()
        };

        let results = hybrid_retrieve(&index, &graph, "zzz", None, &config, Some(&store)).unwrap();

        assert!(!results.is_empty());
        let replay = results.iter().find(|r| r.node_id == "feat-replay").unwrap();
        assert!(replay.vector_score.is_some());
        assert!(replay.vector_score.unwrap() > 0.0);
    }
}
