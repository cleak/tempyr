use async_trait::async_trait;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::Path;
#[cfg(feature = "local-embeddings")]
use std::sync::Mutex;
use std::time::Duration;

use crate::IndexError;
use crate::Result;
use crate::indexer::Index;
use crate::vector::{VectorSearchResult, blob_to_embedding, cosine_similarity, embedding_to_blob};

/// Trait for embedding providers. Implementations call external APIs or local models.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Embed a batch of texts, returning one vector per input.
    async fn embed(&self, texts: &[String], input_type: InputType) -> Result<Vec<Vec<f32>>>;

    /// The dimensionality of the output vectors.
    fn dimensions(&self) -> usize;

    /// Provider name for display.
    fn name(&self) -> &str;
}

/// Whether the text is a query or a document (some models optimize differently).
#[derive(Debug, Clone, Copy)]
pub enum InputType {
    Query,
    Document,
}

// Voyage AI

pub struct VoyageClient {
    api_key: String,
    model: String,
    dimensions: usize,
    client: reqwest::Client,
}

impl VoyageClient {
    pub fn new(api_key: &str, model: &str, dimensions: usize) -> Self {
        Self {
            api_key: api_key.to_string(),
            model: model.to_string(),
            dimensions,
            client: reqwest::Client::new(),
        }
    }

    pub fn from_env(model: &str, dimensions: usize) -> Result<Self> {
        let api_key = std::env::var("VOYAGE_API_KEY").map_err(|_| {
            IndexError::General(
                "VOYAGE_API_KEY environment variable not set. \
                 Set it or switch to local embeddings with [embedding] provider = \"local\" in config.toml"
                    .to_string(),
            )
        })?;
        Ok(Self::new(&api_key, model, dimensions))
    }
}

#[derive(Serialize)]
struct VoyageRequest {
    input: Vec<String>,
    model: String,
    input_type: Option<String>,
    output_dimension: usize,
}

#[derive(Deserialize)]
struct VoyageResponse {
    data: Vec<VoyageEmbedding>,
}

#[derive(Deserialize)]
struct VoyageEmbedding {
    embedding: Vec<f32>,
}

#[async_trait]
impl EmbeddingProvider for VoyageClient {
    async fn embed(&self, texts: &[String], input_type: InputType) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let it = match input_type {
            InputType::Query => Some("query".to_string()),
            InputType::Document => Some("document".to_string()),
        };

        // Voyage allows up to 1000 items per request
        let mut all_embeddings = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(128) {
            let body = VoyageRequest {
                input: chunk.to_vec(),
                model: self.model.clone(),
                input_type: it.clone(),
                output_dimension: self.dimensions,
            };

            let resp = self
                .client
                .post("https://api.voyageai.com/v1/embeddings")
                .header("Authorization", format!("Bearer {}", self.api_key))
                .json(&body)
                .send()
                .await
                .map_err(|e| IndexError::General(format!("Voyage API request failed: {e}")))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(IndexError::General(format!(
                    "Voyage API error {status}: {body}"
                )));
            }

            let result: VoyageResponse = resp
                .json()
                .await
                .map_err(|e| IndexError::General(format!("Voyage API parse error: {e}")))?;

            for emb in result.data {
                all_embeddings.push(emb.embedding);
            }
        }

        Ok(all_embeddings)
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn name(&self) -> &str {
        "voyage"
    }
}

// Google Gemini

pub struct GeminiClient {
    api_key: String,
    model: String,
    dimensions: usize,
    client: reqwest::Client,
}

impl GeminiClient {
    pub fn new(api_key: &str, model: &str, dimensions: usize) -> Self {
        Self {
            api_key: api_key.to_string(),
            model: model.to_string(),
            dimensions,
            client: reqwest::Client::new(),
        }
    }

    pub fn from_env(model: &str, dimensions: usize) -> Result<Self> {
        let api_key = std::env::var("GEMINI_API_KEY").map_err(|_| {
            IndexError::General("GEMINI_API_KEY environment variable not set.".to_string())
        })?;
        Ok(Self::new(&api_key, model, dimensions))
    }
}

#[derive(Serialize)]
struct GeminiBatchRequest {
    requests: Vec<GeminiSingleRequest>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiSingleRequest {
    model: String,
    content: GeminiContent,
    task_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_dimensionality: Option<usize>,
}

#[derive(Serialize)]
struct GeminiContent {
    parts: Vec<GeminiPart>,
}

#[derive(Serialize)]
struct GeminiPart {
    text: String,
}

#[derive(Deserialize)]
struct GeminiBatchResponse {
    embeddings: Vec<GeminiEmbedding>,
}

#[derive(Deserialize)]
struct GeminiSingleResponse {
    embedding: GeminiEmbedding,
}

#[derive(Deserialize)]
struct GeminiEmbedding {
    values: Vec<f32>,
}

#[async_trait]
impl EmbeddingProvider for GeminiClient {
    async fn embed(&self, texts: &[String], input_type: InputType) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let task_type = match input_type {
            InputType::Query => "RETRIEVAL_QUERY",
            InputType::Document => "RETRIEVAL_DOCUMENT",
        };

        let model_path = format!("models/{}", self.model);

        // Use batch endpoint for multiple texts
        if texts.len() > 1 {
            let mut all_embeddings = Vec::with_capacity(texts.len());

            // Gemini batch: process in chunks (no documented max, be conservative)
            for chunk in texts.chunks(100) {
                let requests: Vec<GeminiSingleRequest> = chunk
                    .iter()
                    .map(|text| GeminiSingleRequest {
                        model: model_path.clone(),
                        content: GeminiContent {
                            parts: vec![GeminiPart { text: text.clone() }],
                        },
                        task_type: task_type.to_string(),
                        output_dimensionality: Some(self.dimensions),
                    })
                    .collect();

                let body = GeminiBatchRequest { requests };
                let url = format!(
                    "https://generativelanguage.googleapis.com/v1beta/models/{}:batchEmbedContents",
                    self.model
                );

                let resp = self
                    .client
                    .post(&url)
                    .header("x-goog-api-key", &self.api_key)
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| IndexError::General(format!("Gemini API request failed: {e}")))?;

                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    return Err(IndexError::General(format!(
                        "Gemini API error {status}: {body}"
                    )));
                }

                let result: GeminiBatchResponse = resp
                    .json()
                    .await
                    .map_err(|e| IndexError::General(format!("Gemini API parse error: {e}")))?;

                for emb in result.embeddings {
                    all_embeddings.push(emb.values);
                }
            }

            Ok(all_embeddings)
        } else {
            // Single text - use single endpoint
            let body = GeminiSingleRequest {
                model: model_path,
                content: GeminiContent {
                    parts: vec![GeminiPart {
                        text: texts[0].clone(),
                    }],
                },
                task_type: task_type.to_string(),
                output_dimensionality: Some(self.dimensions),
            };

            let url = format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{}:embedContent",
                self.model
            );

            let resp = self
                .client
                .post(&url)
                .header("x-goog-api-key", &self.api_key)
                .json(&body)
                .send()
                .await
                .map_err(|e| IndexError::General(format!("Gemini API request failed: {e}")))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(IndexError::General(format!(
                    "Gemini API error {status}: {body}"
                )));
            }

            let result: GeminiSingleResponse = resp
                .json()
                .await
                .map_err(|e| IndexError::General(format!("Gemini API parse error: {e}")))?;

            Ok(vec![result.embedding.values])
        }
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn name(&self) -> &str {
        "gemini"
    }
}

// Local fastembed fallback

#[cfg(feature = "local-embeddings")]
pub struct FastembedClient {
    model: Mutex<fastembed::TextEmbedding>,
    dims: usize,
}

#[cfg(feature = "local-embeddings")]
impl FastembedClient {
    pub fn new() -> Result<Self> {
        use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

        let model = TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::AllMiniLML6V2).with_show_download_progress(true),
        )
        .map_err(|e| IndexError::General(format!("Failed to load fastembed model: {e}")))?;

        Ok(Self {
            model: Mutex::new(model),
            dims: 384,
        })
    }
}

#[cfg(feature = "local-embeddings")]
#[async_trait]
impl EmbeddingProvider for FastembedClient {
    async fn embed(&self, texts: &[String], _input_type: InputType) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let mut model = self
            .model
            .lock()
            .map_err(|_| IndexError::General("Fastembed model lock poisoned".to_string()))?;
        let embeddings = model
            .embed(texts, None)
            .map_err(|e| IndexError::General(format!("Fastembed error: {e}")))?;

        Ok(embeddings)
    }

    fn dimensions(&self) -> usize {
        self.dims
    }

    fn name(&self) -> &str {
        "local (all-MiniLM-L6-v2)"
    }
}

// Provider factory

/// Embedding provider configuration from config.toml.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    pub provider: String,          // "voyage", "gemini", "local"
    pub model: Option<String>,     // model name override
    pub dimensions: Option<usize>, // dimension override
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            provider: "voyage".to_string(),
            model: Some("voyage-4".to_string()),
            dimensions: Some(1024),
        }
    }
}

/// Create an embedding provider from configuration.
pub fn create_provider(config: &EmbeddingConfig) -> Result<Box<dyn EmbeddingProvider>> {
    match config.provider.as_str() {
        "voyage" => {
            let model = config.model.as_deref().unwrap_or("voyage-4");
            let dims = config.dimensions.unwrap_or(1024);
            Ok(Box::new(VoyageClient::from_env(model, dims)?))
        }
        "gemini" => {
            let model = config.model.as_deref().unwrap_or("gemini-embedding-001");
            let dims = config.dimensions.unwrap_or(768);
            Ok(Box::new(GeminiClient::from_env(model, dims)?))
        }
        #[cfg(feature = "local-embeddings")]
        "local" => Ok(Box::new(FastembedClient::new()?)),
        #[cfg(not(feature = "local-embeddings"))]
        "local" => Err(IndexError::General(
            "Local embeddings require the 'local-embeddings' feature. \
             Rebuild with: cargo build --features local-embeddings"
                .to_string(),
        )),
        other => Err(IndexError::General(format!(
            "Unknown embedding provider: '{other}'. Use 'voyage', 'gemini', or 'local'."
        ))),
    }
}

// Embed graph nodes

use tempyr_core::graph::Graph;

pub struct EmbeddingStore {
    conn: Connection,
}

impl EmbeddingStore {
    pub fn open_or_create(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                IndexError::General(format!("Failed to create embeddings dir: {e}"))
            })?;
        }

        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        let store = Self { conn };
        store.create_tables()?;
        Ok(store)
    }

    fn create_tables(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS embeddings (
                content_hash TEXT PRIMARY KEY,
                embedding    BLOB NOT NULL
            );
            ",
        )?;
        Ok(())
    }

    pub fn has_embedding(&self, content_hash: &str) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM embeddings WHERE content_hash = ?1",
            [content_hash],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn store_embedding(&self, content_hash: &str, embedding: &[f32]) -> Result<()> {
        let blob = embedding_to_blob(embedding);
        self.conn.execute(
            "INSERT OR REPLACE INTO embeddings (content_hash, embedding) VALUES (?1, ?2)",
            rusqlite::params![content_hash, blob],
        )?;
        Ok(())
    }

    pub fn get_embedding(&self, content_hash: &str) -> Result<Option<Vec<f32>>> {
        let result = self.conn.query_row(
            "SELECT embedding FROM embeddings WHERE content_hash = ?1",
            [content_hash],
            |row| {
                let blob: Vec<u8> = row.get(0)?;
                Ok(blob)
            },
        );

        match result {
            Ok(blob) => Ok(Some(blob_to_embedding(&blob))),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn count(&self) -> Result<usize> {
        let count: usize = self
            .conn
            .query_row("SELECT COUNT(*) FROM embeddings", [], |row| row.get(0))?;
        Ok(count)
    }

    pub fn count_embeddings_for_index(
        &self,
        index: &Index,
        node_type_filter: Option<&str>,
    ) -> Result<usize> {
        let mut count = 0;
        for (_, content_hash) in index.node_ids_and_content_hashes(node_type_filter)? {
            if self.has_embedding(&content_hash)? {
                count += 1;
            }
        }
        Ok(count)
    }

    pub fn vector_search(
        &self,
        index: &Index,
        query_embedding: &[f32],
        max_results: usize,
        node_type_filter: Option<&str>,
    ) -> Result<Vec<VectorSearchResult>> {
        let mut results = Vec::new();
        for (node_id, content_hash) in index.node_ids_and_content_hashes(node_type_filter)? {
            if let Some(embedding) = self.get_embedding(&content_hash)? {
                results.push(VectorSearchResult {
                    node_id,
                    similarity: cosine_similarity(query_embedding, &embedding),
                });
            }
        }

        results.sort_by(|a, b| {
            b.similarity
                .partial_cmp(&a.similarity)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(max_results);
        Ok(results)
    }
}

/// Embed all nodes in the graph, using the cache to skip unchanged nodes.
pub async fn embed_graph(
    store: &EmbeddingStore,
    graph: &Graph,
    provider: &dyn EmbeddingProvider,
) -> Result<EmbedStats> {
    let mut to_embed: Vec<(String, String, String)> = Vec::new(); // (id, hash, text)

    for node in graph.nodes.values() {
        let needs_embedding = !store.has_embedding(&node.content_hash)?;
        if needs_embedding {
            let text = format!("{}\n\n{}", node.title(), node.body.trim());
            to_embed.push((node.id().to_string(), node.content_hash.clone(), text));
        }
    }

    let skipped = graph.nodes.len() - to_embed.len();

    if to_embed.is_empty() {
        return Ok(EmbedStats {
            embedded: 0,
            skipped,
            dimensions: provider.dimensions(),
        });
    }

    // Batch embed
    let texts: Vec<String> = to_embed.iter().map(|(_, _, t)| t.clone()).collect();
    let embeddings = provider.embed(&texts, InputType::Document).await?;
    if embeddings.len() != texts.len() {
        return Err(IndexError::General(format!(
            "Embedding provider returned {} vectors for {} texts",
            embeddings.len(),
            texts.len()
        )));
    }

    // Store in cache
    for ((_, hash, _), embedding) in to_embed.iter().zip(embeddings.iter()) {
        store.store_embedding(hash, embedding)?;
    }

    Ok(EmbedStats {
        embedded: texts.len(),
        skipped,
        dimensions: provider.dimensions(),
    })
}

#[derive(Debug)]
pub struct EmbedStats {
    pub embedded: usize,
    pub skipped: usize,
    pub dimensions: usize,
}

impl std::fmt::Display for EmbedStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Embedded {} nodes ({} cached, {} dimensions)",
            self.embedded, self.skipped, self.dimensions
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use tempyr_core::node::parse_node;
    use tempyr_core::schema::Schema;

    fn make_schema() -> Schema {
        r#"
[meta]
version = "1"
description = "test"

[node_types.feature]
description = "Feature"
directory = "features"
required_fields = []
optional_fields = ["status", "owner"]
allowed_statuses = ["draft"]
allowed_edges = []

[edge_types.depends_on]
reverse = "dependency_of"
"#
        .parse()
        .unwrap()
    }

    fn make_graph() -> Graph {
        let mut graph = Graph::new(make_schema());
        let node = parse_node(
            "---\nid: feat-replay\ntype: feature\nstatus: draft\nowner: caleb\n---\n# Session Replay\n\nCapture sessions.\n",
            PathBuf::from("graph/features/feat-replay.md"),
        )
        .unwrap();
        graph.add_node(node);
        graph
    }

    struct FixedProvider {
        embeddings: Vec<Vec<f32>>,
    }

    #[async_trait]
    impl EmbeddingProvider for FixedProvider {
        async fn embed(&self, _texts: &[String], _input_type: InputType) -> Result<Vec<Vec<f32>>> {
            Ok(self.embeddings.clone())
        }

        fn dimensions(&self) -> usize {
            self.embeddings.first().map_or(0, Vec::len)
        }

        fn name(&self) -> &str {
            "fixed"
        }
    }

    #[test]
    fn embed_graph_rejects_mismatched_provider_output_count() {
        let tmp = tempfile::tempdir().unwrap();
        let store = EmbeddingStore::open_or_create(&tmp.path().join("embeddings.db")).unwrap();
        let graph = make_graph();
        let provider = FixedProvider {
            embeddings: Vec::new(),
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt
            .block_on(embed_graph(&store, &graph, &provider))
            .unwrap_err();

        assert!(
            err.to_string()
                .contains("Embedding provider returned 0 vectors for 1 texts")
        );
    }

    #[test]
    fn count_embeddings_for_index_ignores_unrelated_cache_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let store = EmbeddingStore::open_or_create(&tmp.path().join("embeddings.db")).unwrap();
        let graph = make_graph();
        let index = Index::create_in_memory().unwrap();
        index.rebuild(&graph).unwrap();

        let content_hash = graph.get_node("feat-replay").unwrap().content_hash.clone();
        store.store_embedding("unrelated", &[0.0, 1.0]).unwrap();
        store.store_embedding(&content_hash, &[1.0, 0.0]).unwrap();

        assert_eq!(store.count().unwrap(), 2);
        assert_eq!(store.count_embeddings_for_index(&index, None).unwrap(), 1);
    }
}
