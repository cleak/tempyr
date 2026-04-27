use async_trait::async_trait;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
#[cfg(feature = "local-embeddings")]
use std::sync::Mutex;
use std::time::Duration;

use tempyr_core::project::CacheLayout;

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
        let api_key = read_required_api_key("VOYAGE_API_KEY")?;
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
        let api_key = read_required_api_key("GEMINI_API_KEY")?;
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
            model: None,
            dimensions: None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingConfigPartial {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub dimensions: Option<usize>,
}

impl EmbeddingConfig {
    pub fn apply_partial(&mut self, partial: EmbeddingConfigPartial) {
        if let Some(provider) = partial.provider {
            self.provider = provider;
            self.model = None;
            self.dimensions = None;
        }
        if let Some(model) = partial.model {
            self.model = Some(model);
        }
        if let Some(dimensions) = partial.dimensions {
            self.dimensions = Some(dimensions);
        }
    }
}

/// Read and parse the `[embedding]` section of a tempyr config.toml.
///
/// Returns `EmbeddingConfig::default()` if the file does not exist. Returns an
/// error if the file is unreadable or contains an invalid `[embedding]` table.
pub fn load_embedding_config_from_file(config_path: &Path) -> Result<EmbeddingConfig> {
    if !config_path.exists() {
        return Ok(EmbeddingConfig::default());
    }

    let content = std::fs::read_to_string(config_path).map_err(|err| {
        IndexError::General(format!("Failed to read {}: {err}", config_path.display()))
    })?;
    let table = content.parse::<toml::Table>().map_err(|err| {
        IndexError::General(format!("Failed to parse {}: {err}", config_path.display()))
    })?;

    let mut config = EmbeddingConfig::default();
    if let Some(emb) = table.get("embedding") {
        let partial: EmbeddingConfigPartial = emb.clone().try_into().map_err(|err| {
            IndexError::General(format!(
                "Failed to parse [embedding] section in {}: {err}",
                config_path.display()
            ))
        })?;
        config.apply_partial(partial);
    }
    Ok(config)
}

/// Compute the path to the embedding store database for a given provider/model/dimensions.
///
/// The filename is keyed by a blake3 hash of the provider configuration so that
/// switching providers does not silently mix vector spaces.
pub fn embedding_store_path(
    cache: &CacheLayout,
    provider: &str,
    model: Option<&str>,
    dimensions: Option<usize>,
) -> PathBuf {
    let key_src = format!(
        "provider={provider};model={};dimensions={}",
        model.unwrap_or("default"),
        dimensions.unwrap_or(0)
    );
    let digest = blake3::hash(key_src.as_bytes()).to_hex().to_string();
    cache.embeddings_dir().join(format!("{}.db", &digest[..16]))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEmbeddingConfig {
    pub provider: String,
    pub model: Option<String>,
    pub dimensions: usize,
}

const VOYAGE_MODEL: &str = "voyage-4";
const VOYAGE_DIMENSIONS: usize = 1024;
const GEMINI_MODEL: &str = "gemini-embedding-001";
const GEMINI_DIMENSIONS: usize = 768;
const LOCAL_MODEL: &str = "all-MiniLM-L6-v2";
const LOCAL_DIMENSIONS: usize = 384;
const PLACEHOLDER_API_KEYS: &[&str] = &[
    "api-key",
    "api_key",
    "changeme",
    "change-me",
    "change_me",
    "example",
    "key",
    "paste-key-here",
    "replace-me",
    "replace_me",
    "replace-with-real-key",
    "token",
    "xxx",
    "your-api-key",
    "your_api_key",
];

pub fn provider_api_key_env_var(provider: &str) -> Option<&'static str> {
    match provider {
        "voyage" => Some("VOYAGE_API_KEY"),
        "gemini" => Some("GEMINI_API_KEY"),
        _ => None,
    }
}

pub fn validate_api_key_value(env_var: &str, value: &str) -> Result<()> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(IndexError::General(format!(
            "{env_var} is empty. Fill it in with a real API key in Tempyr's shared worktree env, .env.local, or your shell environment before using hosted embeddings."
        )));
    }

    let normalized = trimmed
        .trim_matches(|c| matches!(c, '"' | '\'' | '<' | '>'))
        .to_ascii_lowercase();
    let looks_like_placeholder = PLACEHOLDER_API_KEYS.contains(&normalized.as_str())
        || normalized.contains("example.com")
        || normalized.ends_with("_here")
        || normalized.ends_with("-here")
        || normalized.ends_with(" here");
    if looks_like_placeholder {
        return Err(IndexError::General(format!(
            "{env_var} still looks like a placeholder. Replace it with a real API key before using hosted embeddings."
        )));
    }

    Ok(())
}

fn read_required_api_key(env_var: &'static str) -> Result<String> {
    let api_key = std::env::var(env_var).map_err(|_| {
        IndexError::General(format!(
            "{env_var} environment variable not set. Set it in Tempyr's shared worktree env, .env.local, or your shell environment, or switch to local embeddings with [embedding] provider = \"local\" in config.toml."
        ))
    })?;
    validate_api_key_value(env_var, &api_key)?;
    Ok(api_key)
}

pub fn resolve_embedding_config(config: &EmbeddingConfig) -> Result<ResolvedEmbeddingConfig> {
    match config.provider.as_str() {
        "voyage" => Ok(ResolvedEmbeddingConfig {
            provider: "voyage".to_string(),
            model: Some(
                config
                    .model
                    .clone()
                    .unwrap_or_else(|| VOYAGE_MODEL.to_string()),
            ),
            dimensions: config.dimensions.unwrap_or(VOYAGE_DIMENSIONS),
        }),
        "gemini" => Ok(ResolvedEmbeddingConfig {
            provider: "gemini".to_string(),
            model: Some(
                config
                    .model
                    .clone()
                    .unwrap_or_else(|| GEMINI_MODEL.to_string()),
            ),
            dimensions: config.dimensions.unwrap_or(GEMINI_DIMENSIONS),
        }),
        "local" => {
            if let Some(model) = config.model.as_deref()
                && model != LOCAL_MODEL
            {
                return Err(IndexError::General(format!(
                    "Local embeddings only support model '{LOCAL_MODEL}', got '{model}'"
                )));
            }
            if let Some(dimensions) = config.dimensions
                && dimensions != LOCAL_DIMENSIONS
            {
                return Err(IndexError::General(format!(
                    "Local embeddings only support {LOCAL_DIMENSIONS} dimensions, got {dimensions}"
                )));
            }

            Ok(ResolvedEmbeddingConfig {
                provider: "local".to_string(),
                model: Some(LOCAL_MODEL.to_string()),
                dimensions: LOCAL_DIMENSIONS,
            })
        }
        other => Err(IndexError::General(format!(
            "Unknown embedding provider: '{other}'. Use 'voyage', 'gemini', or 'local'."
        ))),
    }
}

pub fn create_provider_from_resolved(
    config: &ResolvedEmbeddingConfig,
) -> Result<Box<dyn EmbeddingProvider>> {
    match config.provider.as_str() {
        "voyage" => Ok(Box::new(VoyageClient::from_env(
            config.model.as_deref().unwrap_or(VOYAGE_MODEL),
            config.dimensions,
        )?)),
        "gemini" => Ok(Box::new(GeminiClient::from_env(
            config.model.as_deref().unwrap_or(GEMINI_MODEL),
            config.dimensions,
        )?)),
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

/// Create an embedding provider from configuration.
pub fn create_provider(config: &EmbeddingConfig) -> Result<Box<dyn EmbeddingProvider>> {
    let resolved = resolve_embedding_config(config)?;
    create_provider_from_resolved(&resolved)
}

// Embed graph nodes

use tempyr_core::graph::Graph;

pub struct EmbeddingStore {
    conn: Connection,
}

impl EmbeddingStore {
    const SQLITE_BATCH_SIZE: usize = 900;

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

    fn get_embeddings_batch(&self, content_hashes: &[String]) -> Result<HashMap<String, Vec<f32>>> {
        let mut embeddings = HashMap::new();
        if content_hashes.is_empty() {
            return Ok(embeddings);
        }

        for chunk in content_hashes.chunks(Self::SQLITE_BATCH_SIZE) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "SELECT content_hash, embedding FROM embeddings WHERE content_hash IN ({placeholders})"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
                let content_hash: String = row.get(0)?;
                let blob: Vec<u8> = row.get(1)?;
                Ok((content_hash, blob))
            })?;

            for row in rows {
                let (content_hash, blob) = row?;
                embeddings.insert(content_hash, blob_to_embedding(&blob));
            }
        }

        Ok(embeddings)
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
        let nodes = index.node_ids_and_content_hashes(node_type_filter)?;
        let unique_hashes = nodes
            .iter()
            .map(|(_, content_hash)| content_hash.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let embeddings = self.get_embeddings_batch(&unique_hashes)?;

        Ok(nodes
            .iter()
            .filter(|(_, content_hash)| embeddings.contains_key(content_hash))
            .count())
    }

    pub fn vector_search(
        &self,
        index: &Index,
        query_embedding: &[f32],
        max_results: usize,
        node_type_filter: Option<&str>,
    ) -> Result<Vec<VectorSearchResult>> {
        let nodes = index.node_ids_and_content_hashes(node_type_filter)?;
        let unique_hashes = nodes
            .iter()
            .map(|(_, content_hash)| content_hash.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let embeddings = self.get_embeddings_batch(&unique_hashes)?;

        let mut results = Vec::new();
        for (node_id, content_hash) in nodes {
            if let Some(embedding) = embeddings.get(&content_hash) {
                results.push(VectorSearchResult {
                    node_id,
                    similarity: cosine_similarity(query_embedding, embedding),
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
    let mut seen_hashes = HashSet::new();

    for node in graph.nodes.values() {
        let needs_embedding = seen_hashes.insert(node.content_hash.clone())
            && !store.has_embedding(&node.content_hash)?;
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

    #[test]
    fn resolve_embedding_config_applies_provider_defaults() {
        let mut config = EmbeddingConfig::default();
        config.apply_partial(EmbeddingConfigPartial {
            provider: Some("gemini".to_string()),
            ..Default::default()
        });

        let resolved = resolve_embedding_config(&config).unwrap();

        assert_eq!(resolved.provider, "gemini");
        assert_eq!(resolved.model.as_deref(), Some(GEMINI_MODEL));
        assert_eq!(resolved.dimensions, GEMINI_DIMENSIONS);
    }

    #[test]
    fn resolve_embedding_config_rejects_invalid_local_dimensions() {
        let config = EmbeddingConfig {
            provider: "local".to_string(),
            model: None,
            dimensions: Some(1024),
        };

        let err = resolve_embedding_config(&config).unwrap_err();

        assert!(
            err.to_string()
                .contains("Local embeddings only support 384 dimensions")
        );
    }

    #[test]
    fn embed_graph_dedupes_duplicate_content_hashes() {
        let tmp = tempfile::tempdir().unwrap();
        let store = EmbeddingStore::open_or_create(&tmp.path().join("embeddings.db")).unwrap();
        let schema = make_schema();
        let mut graph = Graph::new(schema);
        let node_a = parse_node(
            "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: caleb\n---\n# Shared Title\n\nSame body.\n",
            PathBuf::from("graph/features/feat-a.md"),
        )
        .unwrap();
        let node_b = parse_node(
            "---\nid: feat-b\ntype: feature\nstatus: draft\nowner: caleb\n---\n# Shared Title\n\nSame body.\n",
            PathBuf::from("graph/features/feat-b.md"),
        )
        .unwrap();
        graph.add_node(node_a);
        graph.add_node(node_b);

        let provider = FixedProvider {
            embeddings: vec![vec![1.0, 0.0]],
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        let stats = rt.block_on(embed_graph(&store, &graph, &provider)).unwrap();

        assert_eq!(stats.embedded, 1);
        assert_eq!(store.count().unwrap(), 1);
    }

    #[test]
    fn count_embeddings_for_index_counts_all_nodes_sharing_a_cached_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let store = EmbeddingStore::open_or_create(&tmp.path().join("embeddings.db")).unwrap();
        let index = Index::create_in_memory().unwrap();

        index
            .conn
            .execute(
                "INSERT INTO nodes (id, node_type, file_path, content_hash, body_text, title) VALUES ('feat-a', 'feature', 'a.md', 'shared', '', '')",
                [],
            )
            .unwrap();
        index
            .conn
            .execute(
                "INSERT INTO nodes (id, node_type, file_path, content_hash, body_text, title) VALUES ('feat-b', 'feature', 'b.md', 'shared', '', '')",
                [],
            )
            .unwrap();
        store.store_embedding("shared", &[1.0, 0.0]).unwrap();

        assert_eq!(store.count_embeddings_for_index(&index, None).unwrap(), 2);
    }

    #[test]
    fn vector_search_returns_all_nodes_sharing_a_cached_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let store = EmbeddingStore::open_or_create(&tmp.path().join("embeddings.db")).unwrap();
        let index = Index::create_in_memory().unwrap();

        index
            .conn
            .execute(
                "INSERT INTO nodes (id, node_type, file_path, content_hash, body_text, title) VALUES ('feat-a', 'feature', 'a.md', 'shared', '', '')",
                [],
            )
            .unwrap();
        index
            .conn
            .execute(
                "INSERT INTO nodes (id, node_type, file_path, content_hash, body_text, title) VALUES ('feat-b', 'feature', 'b.md', 'shared', '', '')",
                [],
            )
            .unwrap();
        store.store_embedding("shared", &[1.0, 0.0]).unwrap();

        let results = store.vector_search(&index, &[1.0, 0.0], 10, None).unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].similarity, 1.0);
        assert_eq!(results[1].similarity, 1.0);
        assert_eq!(
            results
                .iter()
                .map(|result| result.node_id.as_str())
                .collect::<HashSet<_>>(),
            HashSet::from(["feat-a", "feat-b"])
        );
    }

    #[test]
    fn validate_api_key_value_rejects_blank_and_placeholder_values() {
        let blank = validate_api_key_value("VOYAGE_API_KEY", "  ").unwrap_err();
        assert!(blank.to_string().contains("VOYAGE_API_KEY is empty"));

        let placeholder = validate_api_key_value("GEMINI_API_KEY", "changeme").unwrap_err();
        assert!(
            placeholder
                .to_string()
                .contains("GEMINI_API_KEY still looks like a placeholder")
        );
    }

    #[test]
    fn validate_api_key_value_accepts_realistic_token_shapes() {
        validate_api_key_value("VOYAGE_API_KEY", "pa-1234567890abcdef").unwrap();
        validate_api_key_value("GEMINI_API_KEY", "AIzaSyA-LongerLookingKey123").unwrap();
    }
}
