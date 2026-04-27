//! System status / health introspection.
//!
//! Builds a structured report describing the configured embedding provider,
//! resolved paths, config files, env files, and rebuildable artifacts. Used by
//! both the `tempyr doctor` CLI command and the `system_doctor` MCP tool.
//!
//! API key VALUES are never included in the report — only the env var name and
//! a boolean flag indicating whether it is set.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use tempyr_core::graph::Graph;
use tempyr_core::project::{self, CacheLayout, IndexLayout};
use tempyr_core::schema::Schema;

use crate::embeddings::{self, EmbeddingConfig, EmbeddingStore};
use crate::indexer::Index;

/// Inputs needed to build the health report. Both CLI and MCP build this from
/// their respective project-context types.
pub struct HealthInputs<'a> {
    pub root: &'a Path,
    pub graph_dir: &'a Path,
    pub tempyr_dir: &'a Path,
    pub cache: &'a CacheLayout,
    pub schema: &'a Schema,
    pub tempyr_version: &'a str,
}

#[derive(Debug, Serialize)]
pub struct HealthReport {
    pub tempyr_version: String,
    pub local_embeddings_compiled_in: bool,
    pub project: ProjectSection,
    pub embedding: EmbeddingSection,
    pub config_files: Vec<ConfigFileEntry>,
    pub env_files: Vec<EnvFileEntry>,
    pub graph: Option<GraphSection>,
    pub graph_error: Option<String>,
    pub index: IndexSection,
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ProjectSection {
    pub root: PathBuf,
    pub graph_dir: PathBuf,
    pub graph_dir_exists: bool,
    pub tempyr_dir: PathBuf,
    pub tempyr_dir_exists: bool,
    pub schema_version: String,
    pub schema_node_types: Vec<String>,
    pub schema_edge_types: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct EmbeddingSection {
    pub provider: String,
    pub model: Option<String>,
    pub dimensions: Option<usize>,
    /// "config.toml" if loaded from disk, "default" otherwise, "error" on failure.
    pub config_source: String,
    pub config_error: Option<String>,
    pub api_key_env_var: Option<String>,
    pub api_key_set: Option<bool>,
    pub store_path: Option<PathBuf>,
    pub store_exists: Option<bool>,
    pub store_count: Option<usize>,
    pub store_error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ConfigFileEntry {
    pub name: String,
    pub path: PathBuf,
    pub exists: bool,
    pub purpose: String,
}

#[derive(Debug, Serialize)]
pub struct EnvFileEntry {
    pub path: PathBuf,
    pub exists: bool,
}

#[derive(Debug, Serialize)]
pub struct GraphSection {
    pub node_count: usize,
    pub edge_count: usize,
    pub nodes_by_type: Vec<(String, usize)>,
}

#[derive(Debug, Serialize)]
pub struct IndexSection {
    pub active_index_path: PathBuf,
    pub active_index_exists: bool,
    pub legacy_index_path: PathBuf,
    pub legacy_index_exists: bool,
    pub current_snapshot_index_path: Option<PathBuf>,
    pub current_snapshot_index_exists: Option<bool>,
    /// Set when [`IndexLayout::current_index_path`] returned an error
    /// (e.g. the snapshot-key cache could not be read). Surfaces failures
    /// that would otherwise look identical to "no snapshot exists".
    pub current_snapshot_index_error: Option<String>,
    pub snapshot_key: Option<String>,
    pub snapshot_key_error: Option<String>,
    pub fts_entries: Option<usize>,
    pub embedding_count_for_index: Option<usize>,
}

const LOCAL_EMBEDDINGS_COMPILED_IN: bool = cfg!(feature = "local-embeddings");

/// Build a full system health report. Never returns Err — every sub-check
/// captures its failure into the report itself.
pub fn build_report(inputs: &HealthInputs<'_>) -> HealthReport {
    let mut warnings: Vec<String> = Vec::new();

    let project = build_project_section(inputs);
    let embedding = build_embedding_section(inputs, &mut warnings);
    let config_files = build_config_files(inputs);
    let env_files = build_env_files(inputs);
    let (graph_section, graph_error) = build_graph_section(inputs, &mut warnings);
    let index_section = build_index_section(inputs, embedding.store_path.as_deref());

    if !project.graph_dir_exists {
        warnings.push(format!(
            "Graph directory does not exist: {}",
            project.graph_dir.display()
        ));
    }
    if !index_section.active_index_exists
        && !matches!(index_section.current_snapshot_index_exists, Some(true))
        && !index_section.legacy_index_exists
    {
        warnings.push(
            "No queryable index found. Run `tempyr index rebuild` to populate the index."
                .to_string(),
        );
    }
    if let Some(err) = &index_section.current_snapshot_index_error {
        warnings.push(format!("Failed to resolve current snapshot index: {err}"));
    }
    if matches!(embedding.store_exists, Some(false)) && embedding.api_key_set != Some(false) {
        warnings.push(format!(
            "Embedding store does not exist at {}. Run `tempyr index rebuild` to populate embeddings.",
            embedding.store_path.as_ref().map(|p| p.display().to_string()).unwrap_or_default()
        ));
    }
    if matches!(embedding.api_key_set, Some(false))
        && let Some(env_var) = embedding.api_key_env_var.as_ref()
    {
        warnings.push(format!(
            "Embedding API key {env_var} is not set. Set it in your shell, .env.local, or shared worktree env, or switch to provider = \"local\"."
        ));
    }

    HealthReport {
        tempyr_version: inputs.tempyr_version.to_string(),
        local_embeddings_compiled_in: LOCAL_EMBEDDINGS_COMPILED_IN,
        project,
        embedding,
        config_files,
        env_files,
        graph: graph_section,
        graph_error,
        index: index_section,
        warnings,
    }
}

fn build_project_section(inputs: &HealthInputs<'_>) -> ProjectSection {
    let mut node_types: Vec<String> = inputs.schema.node_types.keys().cloned().collect();
    node_types.sort();
    let mut edge_types: Vec<String> = inputs.schema.edge_types.keys().cloned().collect();
    edge_types.sort();

    ProjectSection {
        root: inputs.root.to_path_buf(),
        graph_dir: inputs.graph_dir.to_path_buf(),
        graph_dir_exists: inputs.graph_dir.is_dir(),
        tempyr_dir: inputs.tempyr_dir.to_path_buf(),
        tempyr_dir_exists: inputs.tempyr_dir.is_dir(),
        schema_version: inputs.schema.meta.version.clone(),
        schema_node_types: node_types,
        schema_edge_types: edge_types,
    }
}

fn build_embedding_section(
    inputs: &HealthInputs<'_>,
    warnings: &mut Vec<String>,
) -> EmbeddingSection {
    let config_path = inputs.tempyr_dir.join("config.toml");
    let config_existed = config_path.exists();
    let (raw_config, mut config_error) =
        match embeddings::load_embedding_config_from_file(&config_path) {
            Ok(config) => (config, None),
            Err(err) => (EmbeddingConfig::default(), Some(err.to_string())),
        };
    let config_source = match (config_existed, config_error.is_some()) {
        (false, _) => "default",
        (true, false) => "config.toml",
        (true, true) => "error",
    }
    .to_string();

    let resolved = match embeddings::resolve_embedding_config(&raw_config) {
        Ok(resolved) => Some(resolved),
        Err(err) => {
            let message = err.to_string();
            warnings.push(format!("Embedding config invalid: {message}"));
            if config_error.is_none() {
                config_error = Some(message);
            }
            None
        }
    };

    let provider = resolved
        .as_ref()
        .map(|r| r.provider.clone())
        .unwrap_or_else(|| raw_config.provider.clone());
    let model = resolved
        .as_ref()
        .and_then(|r| r.model.clone())
        .or_else(|| raw_config.model.clone());
    let dimensions = resolved.as_ref().map(|r| r.dimensions);

    let api_key_env_var = embeddings::provider_api_key_env_var(&provider).map(str::to_string);
    let api_key_set = api_key_env_var.as_ref().map(|env_var| {
        std::env::var(env_var)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .is_some()
    });

    let store_path = resolved.as_ref().map(|r| {
        embeddings::embedding_store_path(
            inputs.cache,
            &r.provider,
            r.model.as_deref(),
            Some(r.dimensions),
        )
    });
    let (store_exists, store_count, store_error) = match store_path.as_deref() {
        Some(path) => probe_embedding_store(path),
        None => (None, None, None),
    };

    EmbeddingSection {
        provider,
        model,
        dimensions,
        config_source,
        config_error,
        api_key_env_var,
        api_key_set,
        store_path,
        store_exists,
        store_count,
        store_error,
    }
}

fn probe_embedding_store(path: &Path) -> (Option<bool>, Option<usize>, Option<String>) {
    if !path.exists() {
        return (Some(false), None, None);
    }
    match EmbeddingStore::open_or_create(path) {
        Ok(store) => match store.count() {
            Ok(count) => (Some(true), Some(count), None),
            Err(err) => (Some(true), None, Some(err.to_string())),
        },
        Err(err) => (Some(true), None, Some(err.to_string())),
    }
}

fn build_config_files(inputs: &HealthInputs<'_>) -> Vec<ConfigFileEntry> {
    let entries = [
        ("schema.toml", "Node/edge type definitions"),
        ("config.toml", "Embedding provider configuration"),
        ("linear.json", "Linear integration configuration"),
    ];

    entries
        .into_iter()
        .map(|(name, purpose)| {
            let path = inputs.tempyr_dir.join(name);
            ConfigFileEntry {
                name: name.to_string(),
                exists: path.is_file(),
                path,
                purpose: purpose.to_string(),
            }
        })
        .collect()
}

fn build_env_files(inputs: &HealthInputs<'_>) -> Vec<EnvFileEntry> {
    project::env_file_candidates(inputs.root)
        .into_iter()
        .map(|path| EnvFileEntry {
            exists: path.is_file(),
            path,
        })
        .collect()
}

fn build_graph_section(
    inputs: &HealthInputs<'_>,
    warnings: &mut Vec<String>,
) -> (Option<GraphSection>, Option<String>) {
    if !inputs.graph_dir.is_dir() {
        return (None, None);
    }

    match Graph::load_from_directory(inputs.graph_dir, inputs.schema.clone()) {
        Ok(graph) => {
            let mut counts: HashMap<String, usize> = HashMap::new();
            for node in graph.nodes.values() {
                *counts.entry(node.node_type().to_string()).or_default() += 1;
            }
            let mut nodes_by_type: Vec<(String, usize)> = counts.into_iter().collect();
            nodes_by_type.sort();

            (
                Some(GraphSection {
                    node_count: graph.node_count(),
                    edge_count: graph.edge_count(),
                    nodes_by_type,
                }),
                None,
            )
        }
        Err(err) => {
            let message = err.to_string();
            warnings.push(format!("Failed to load graph: {message}"));
            (None, Some(message))
        }
    }
}

fn build_index_section(inputs: &HealthInputs<'_>, store_path: Option<&Path>) -> IndexSection {
    let layout = match IndexLayout::resolve(inputs.root, inputs.graph_dir, inputs.tempyr_dir) {
        Ok(layout) => layout,
        Err(err) => {
            return IndexSection {
                active_index_path: inputs.cache.active_index_path(),
                active_index_exists: inputs.cache.active_index_path().exists(),
                legacy_index_path: inputs.tempyr_dir.join("index.db"),
                legacy_index_exists: inputs.tempyr_dir.join("index.db").exists(),
                current_snapshot_index_path: None,
                current_snapshot_index_exists: None,
                current_snapshot_index_error: None,
                snapshot_key: None,
                snapshot_key_error: Some(err.to_string()),
                fts_entries: None,
                embedding_count_for_index: None,
            };
        }
    };

    let active_index_path = layout.active_index_path();
    let active_index_exists = active_index_path.exists();
    let legacy_index_path = layout.legacy_index_path.clone();
    let legacy_index_exists = legacy_index_path.exists();

    let (snapshot_key, snapshot_key_error) = match layout.snapshot_key() {
        Ok(key) => (Some(key), None),
        Err(err) => (None, Some(err.to_string())),
    };

    let (current_snapshot_index_path, current_snapshot_index_error) =
        match layout.current_index_path() {
            Ok(path) => (path, None),
            Err(err) => (None, Some(err.to_string())),
        };
    let current_snapshot_index_exists = current_snapshot_index_path.as_ref().map(|p| p.exists());

    let (fts_entries, embedding_count_for_index) = match current_snapshot_index_path.as_deref() {
        Some(path) => probe_index(path, store_path),
        None => (None, None),
    };

    IndexSection {
        active_index_path,
        active_index_exists,
        legacy_index_path,
        legacy_index_exists,
        current_snapshot_index_path,
        current_snapshot_index_exists,
        current_snapshot_index_error,
        snapshot_key,
        snapshot_key_error,
        fts_entries,
        embedding_count_for_index,
    }
}

fn probe_index(index_path: &Path, store_path: Option<&Path>) -> (Option<usize>, Option<usize>) {
    let index = match Index::open(index_path) {
        Ok(index) => index,
        Err(_) => return (None, None),
    };

    let fts_entries = index.stats().ok().map(|s| s.fts_entries);
    let embedding_count = store_path.and_then(|path| {
        if !path.exists() {
            return None;
        }
        EmbeddingStore::open_or_create(path)
            .ok()
            .and_then(|store| store.count_embeddings_for_index(&index, None).ok())
    });
    (fts_entries, embedding_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn test_schema() -> Schema {
        r#"
[meta]
version = "1"
description = "test"

[node_types.feature]
description = "Feature"
directory = "features"
required_fields = []
optional_fields = []
allowed_statuses = ["draft"]
allowed_edges = []

[edge_types]
"#
        .parse()
        .unwrap()
    }

    fn make_inputs<'a>(
        root: &'a Path,
        graph_dir: &'a Path,
        tempyr_dir: &'a Path,
        cache: &'a CacheLayout,
        schema: &'a Schema,
    ) -> HealthInputs<'a> {
        HealthInputs {
            root,
            graph_dir,
            tempyr_dir,
            cache,
            schema,
            tempyr_version: "test-version",
        }
    }

    #[test]
    fn report_uses_default_embedding_config_when_no_config_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let tempyr_dir = root.join(".tempyr");
        let graph_dir = root.join("graph");
        fs::create_dir_all(&tempyr_dir).unwrap();
        fs::create_dir_all(&graph_dir).unwrap();
        let cache = project::cache_layout(root, &tempyr_dir);
        let schema = test_schema();

        let inputs = make_inputs(root, &graph_dir, &tempyr_dir, &cache, &schema);
        let report = build_report(&inputs);

        assert_eq!(report.embedding.provider, "voyage");
        assert_eq!(report.embedding.config_source, "default");
        assert!(report.embedding.config_error.is_none());
        assert_eq!(
            report.embedding.api_key_env_var.as_deref(),
            Some("VOYAGE_API_KEY")
        );
        assert_eq!(report.tempyr_version, "test-version");
    }

    // The "never leaks the API key value" property is covered by the
    // integration tests `test_doctor_does_not_leak_api_key_value` and
    // `test_mcp_system_doctor_returns_report_without_api_key`. Those run
    // `tempyr` in a child process with `Command::env(...)`, so the env var is
    // isolated. Doing the same at the unit-test layer would require mutating
    // `VOYAGE_API_KEY` in this process, which races with other tests.

    #[test]
    fn report_flags_invalid_embedding_provider() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let tempyr_dir = root.join(".tempyr");
        fs::create_dir_all(&tempyr_dir).unwrap();
        fs::write(
            tempyr_dir.join("config.toml"),
            "[embedding]\nprovider = \"bogus\"\n",
        )
        .unwrap();
        let cache = project::cache_layout(root, &tempyr_dir);
        let schema = test_schema();

        let graph_dir = root.join("graph");
        let inputs = make_inputs(root, &graph_dir, &tempyr_dir, &cache, &schema);
        let report = build_report(&inputs);

        assert_eq!(report.embedding.provider, "bogus");
        assert!(report.embedding.config_error.is_some());
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("Embedding config invalid"))
        );
    }

    #[test]
    fn report_marks_config_source_error_on_unparseable_config() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let tempyr_dir = root.join(".tempyr");
        fs::create_dir_all(&tempyr_dir).unwrap();
        // Truncated TOML — fails to parse.
        fs::write(tempyr_dir.join("config.toml"), "[embedding\n").unwrap();
        let cache = project::cache_layout(root, &tempyr_dir);
        let schema = test_schema();

        let graph_dir = root.join("graph");
        let inputs = make_inputs(root, &graph_dir, &tempyr_dir, &cache, &schema);
        let report = build_report(&inputs);

        assert_eq!(report.embedding.config_source, "error");
        assert!(
            report
                .embedding
                .config_error
                .as_deref()
                .unwrap_or("")
                .contains("Failed to parse")
        );
    }

    #[test]
    fn report_lists_known_config_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let tempyr_dir = root.join(".tempyr");
        fs::create_dir_all(&tempyr_dir).unwrap();
        fs::write(tempyr_dir.join("config.toml"), "").unwrap();
        let cache = project::cache_layout(root, &tempyr_dir);
        let schema = test_schema();

        let graph_dir = root.join("graph");
        let inputs = make_inputs(root, &graph_dir, &tempyr_dir, &cache, &schema);
        let report = build_report(&inputs);

        let names: Vec<&str> = report
            .config_files
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert!(names.contains(&"schema.toml"));
        assert!(names.contains(&"config.toml"));
        assert!(names.contains(&"linear.json"));

        let config_entry = report
            .config_files
            .iter()
            .find(|c| c.name == "config.toml")
            .unwrap();
        assert!(config_entry.exists);
    }
}
