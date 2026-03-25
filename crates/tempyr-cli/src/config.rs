use std::path::{Path, PathBuf};

use tempyr_core::schema::Schema;

/// Project context: resolved paths for a tempyr project.
pub struct ProjectContext {
    pub root: PathBuf,
    pub graph_dir: PathBuf,
    pub tempyr_dir: PathBuf,
    pub schema: Schema,
}

impl ProjectContext {
    /// Find the project root and load the schema.
    pub fn find(graph_dir_override: Option<&Path>) -> anyhow::Result<Self> {
        let root = find_project_root()
            .ok_or_else(|| anyhow::anyhow!("Not a tempyr project. Run `tempyr init` first."))?;

        let tempyr_dir = root.join(".tempyr");
        let graph_dir = graph_dir_override
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("graph"));

        let schema_path = tempyr_dir.join("schema.toml");
        let schema = Schema::load(&schema_path)?;

        Ok(Self {
            root,
            graph_dir,
            tempyr_dir,
            schema,
        })
    }

    /// Get the index database path.
    pub fn index_path(&self) -> PathBuf {
        self.tempyr_dir.join("index.db")
    }
}

/// Walk up the directory tree to find a `.tempyr/` directory.
pub fn find_project_root() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join(".tempyr").is_dir() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}
