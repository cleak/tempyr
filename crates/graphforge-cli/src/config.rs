use std::path::{Path, PathBuf};

use graphforge_core::schema::Schema;

/// Project context: resolved paths for a graphforge project.
pub struct ProjectContext {
    pub root: PathBuf,
    pub graph_dir: PathBuf,
    pub graphforge_dir: PathBuf,
    pub schema: Schema,
}

impl ProjectContext {
    /// Find the project root and load the schema.
    pub fn find(graph_dir_override: Option<&Path>) -> anyhow::Result<Self> {
        let root = find_project_root()
            .ok_or_else(|| anyhow::anyhow!("Not a graphforge project. Run `graphforge init` first."))?;

        let graphforge_dir = root.join(".graphforge");
        let graph_dir = graph_dir_override
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("graph"));

        let schema_path = graphforge_dir.join("schema.toml");
        let schema = Schema::load(&schema_path)?;

        Ok(Self {
            root,
            graph_dir,
            graphforge_dir,
            schema,
        })
    }

    /// Get the index database path.
    pub fn index_path(&self) -> PathBuf {
        self.graphforge_dir.join("index.db")
    }
}

/// Walk up the directory tree to find a `.graphforge/` directory.
pub fn find_project_root() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join(".graphforge").is_dir() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}
