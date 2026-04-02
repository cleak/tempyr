use anyhow::Context;
use std::path::{Path, PathBuf};

use tempyr_core::project;
use tempyr_core::project::{CacheLayout, IndexLayout};
use tempyr_core::schema::Schema;
use tempyr_index::embeddings::{
    EmbeddingConfig, EmbeddingConfigPartial, ResolvedEmbeddingConfig, resolve_embedding_config,
};

/// Project context: resolved paths for a tempyr project.
pub struct ProjectContext {
    /// Project root directory - canonical anchor from which other paths are derived.
    pub root: PathBuf,
    pub graph_dir: PathBuf,
    pub tempyr_dir: PathBuf,
    pub cache: CacheLayout,
    pub schema: Schema,
}

impl ProjectContext {
    /// Find the project root and load the schema.
    pub fn find(graph_dir_override: Option<&Path>) -> anyhow::Result<Self> {
        let root = graph_dir_override
            .map(|graph_dir| project::find_project_root_from(graph_dir.to_path_buf()))
            .unwrap_or_else(project::find_project_root)
            .ok_or_else(|| anyhow::anyhow!("Not a tempyr project (no .tempyr/ or .tempyr-redirect found). Run `tempyr init` first."))?;

        let tempyr_dir = root.join(".tempyr");
        let graph_dir = graph_dir_override
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("graph"));

        let schema_path = tempyr_dir.join("schema.toml");
        let schema = Schema::load(&schema_path)?;
        let cache = project::cache_layout(&root, &tempyr_dir);

        Ok(Self {
            root,
            graph_dir,
            tempyr_dir,
            cache,
            schema,
        })
    }

    pub fn shared_embeddings_dir(&self) -> PathBuf {
        self.cache.embeddings_dir()
    }

    pub fn embedding_store_path(
        &self,
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
        self.shared_embeddings_dir()
            .join(format!("{}.db", &digest[..16]))
    }

    pub fn embedding_config(&self) -> anyhow::Result<EmbeddingConfig> {
        let config_path = self.tempyr_dir.join("config.toml");
        if !config_path.exists() {
            return Ok(EmbeddingConfig::default());
        }

        let content = std::fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read {}", config_path.display()))?;
        let table = content
            .parse::<toml::Table>()
            .with_context(|| format!("Failed to parse {}", config_path.display()))?;

        let mut config = EmbeddingConfig::default();
        if let Some(emb) = table.get("embedding") {
            let partial = emb
                .clone()
                .try_into::<EmbeddingConfigPartial>()
                .with_context(|| {
                    format!(
                        "Failed to parse [embedding] section in {}",
                        config_path.display()
                    )
                })?;
            config.apply_partial(partial);
        }

        Ok(config)
    }

    pub fn resolved_embedding_config(&self) -> anyhow::Result<ResolvedEmbeddingConfig> {
        let config = self.embedding_config()?;
        Ok(resolve_embedding_config(&config)?)
    }

    /// Resolve the best available index path for the current graph snapshot.
    pub fn current_index_path(&self) -> anyhow::Result<PathBuf> {
        self.index_layout()?.current_index_path()?.ok_or_else(|| {
            anyhow::anyhow!(
                "Index not found for current graph snapshot. Run `tempyr index rebuild` first."
            )
        })
    }

    /// Ensure the mutable per-worktree index exists, seeding it from a shared snapshot
    /// when possible.
    pub fn ensure_active_index_seeded(&self) -> anyhow::Result<(String, PathBuf)> {
        let layout = self.index_layout()?;
        let snapshot_key = layout.snapshot_key()?;
        let active = layout.ensure_active_index_seeded()?;
        Ok((snapshot_key, active))
    }

    pub fn write_active_snapshot_key(&self, snapshot_key: &str) -> anyhow::Result<()> {
        let layout = self.index_layout()?;
        layout.set_snapshot_key(snapshot_key);
        layout.write_active_snapshot_key()?;
        Ok(())
    }

    pub fn publish_active_snapshot(&self, snapshot_key: &str) -> anyhow::Result<PathBuf> {
        let layout = self.index_layout()?;
        layout.set_snapshot_key(snapshot_key);
        Ok(layout.publish_active_snapshot()?)
    }

    fn index_layout(&self) -> anyhow::Result<IndexLayout> {
        Ok(project::IndexLayout::resolve(
            &self.root,
            &self.graph_dir,
            &self.tempyr_dir,
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn make_context(root: &Path) -> ProjectContext {
        let tempyr_dir = root.join(".tempyr");
        std::fs::create_dir_all(&tempyr_dir).unwrap();

        ProjectContext {
            root: root.to_path_buf(),
            graph_dir: root.join("graph"),
            tempyr_dir: tempyr_dir.clone(),
            cache: project::cache_layout(root, &tempyr_dir),
            schema: test_schema(),
        }
    }

    #[test]
    fn embedding_config_defaults_when_config_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = make_context(tmp.path());

        let config = ctx.embedding_config().unwrap();

        assert_eq!(config.provider, "voyage");
        assert_eq!(config.model, None);
        assert_eq!(config.dimensions, None);
    }

    #[test]
    fn embedding_config_errors_on_invalid_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = make_context(tmp.path());
        std::fs::write(ctx.tempyr_dir.join("config.toml"), "[embedding\n").unwrap();

        let err = ctx.embedding_config().unwrap_err();

        assert!(err.to_string().contains("Failed to parse"));
    }

    #[test]
    fn embedding_config_errors_on_invalid_embedding_section() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = make_context(tmp.path());
        std::fs::write(
            ctx.tempyr_dir.join("config.toml"),
            "[embedding]\ndimensions = \"oops\"\n",
        )
        .unwrap();

        let err = ctx.embedding_config().unwrap_err();

        assert!(
            err.to_string()
                .contains("Failed to parse [embedding] section")
        );
    }

    #[test]
    fn embedding_config_errors_on_unknown_embedding_key() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = make_context(tmp.path());
        std::fs::write(
            ctx.tempyr_dir.join("config.toml"),
            "[embedding]\nprovidr = \"voyage\"\n",
        )
        .unwrap();

        let err = ctx.embedding_config().unwrap_err();

        assert!(
            err.to_string()
                .contains("Failed to parse [embedding] section")
        );
    }

    #[test]
    fn find_uses_graph_dir_override_to_resolve_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let tempyr_dir = root.join(".tempyr");
        std::fs::create_dir_all(root.join("graph")).unwrap();
        std::fs::create_dir_all(&tempyr_dir).unwrap();
        std::fs::write(
            tempyr_dir.join("schema.toml"),
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
"#,
        )
        .unwrap();

        let ctx = ProjectContext::find(Some(root.join("graph").as_path())).unwrap();

        assert_eq!(ctx.root, root);
        assert_eq!(ctx.graph_dir, root.join("graph"));
        assert_eq!(ctx.tempyr_dir, tempyr_dir);
    }
}
