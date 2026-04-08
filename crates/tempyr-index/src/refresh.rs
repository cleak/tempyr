use std::fs;
use std::io;
use std::path::Path;

use tempyr_core::graph::Graph;
use tempyr_core::project::IndexLayout;

use crate::indexer::Index;
use crate::{IndexError, Result};

/// Refresh the staged index for the current snapshot and publish it through the
/// provided layout.
pub fn refresh_index_for_graph(layout: &IndexLayout, graph: &Graph) -> Result<()> {
    layout
        .update_active_index_atomically(|index_path| {
            refresh_index_at_path(index_path, graph)
                .map_err(|err| io::Error::other(err.to_string()))
        })
        .map_err(|err| IndexError::General(format!("Index refresh failed: {err}")))?;
    Ok(())
}

fn refresh_index_at_path(index_path: &Path, graph: &Graph) -> Result<()> {
    if index_path.exists() {
        let index = Index::open(index_path)?;
        index.incremental_update(graph)?;
    } else {
        if let Some(parent) = index_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| IndexError::General(format!("Failed to create index dir: {err}")))?;
        }
        let index = Index::create(index_path)?;
        index.rebuild(graph)?;
    }
    Ok(())
}
