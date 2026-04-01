use std::path::{Path, PathBuf};

use tempyr_core::project;
use tempyr_core::project::{CacheLayout, IndexLayout};
use tempyr_core::schema::Schema;

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
        let root = project::find_project_root()
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

    /// Resolve the best available index path for the current graph snapshot.
    pub fn current_index_path(&self) -> anyhow::Result<PathBuf> {
        self.index_layout()?.current_index_path().ok_or_else(|| {
            anyhow::anyhow!(
                "Index not found for current graph snapshot. Run `tempyr index rebuild` first."
            )
        })
    }

    /// Ensure the mutable per-worktree index exists, seeding it from a shared snapshot
    /// when possible.
    pub fn ensure_active_index_seeded(&self) -> anyhow::Result<(String, PathBuf)> {
        let layout = self.index_layout()?;
        let active = layout.ensure_active_index_seeded()?;
        Ok((layout.snapshot_key, active))
    }

    pub fn write_active_snapshot_key(&self, snapshot_key: &str) -> anyhow::Result<()> {
        let mut layout = self.index_layout()?;
        layout.snapshot_key = snapshot_key.to_string();
        layout.write_active_snapshot_key()?;
        Ok(())
    }

    pub fn publish_active_snapshot(&self, snapshot_key: &str) -> anyhow::Result<PathBuf> {
        let mut layout = self.index_layout()?;
        layout.snapshot_key = snapshot_key.to_string();
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
