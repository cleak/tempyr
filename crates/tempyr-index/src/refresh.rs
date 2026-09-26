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

/// Bring the index at `index_path` up to date with `graph`.
///
/// An existing file (usually the previous snapshot's index) is updated
/// incrementally. If it cannot be (not a SQLite database, corrupt, ...), it is
/// discarded and rebuilt: the index is derived, so a rebuild is always a
/// correct answer, only a slower one.
fn refresh_index_at_path(index_path: &Path, graph: &Graph) -> Result<()> {
    if index_path.exists() {
        // The connection is dropped at the end of the closure, before any
        // removal below (Windows cannot delete a file that is still open).
        if Index::open(index_path)
            .and_then(|index| index.incremental_update(graph))
            .is_ok()
        {
            return Ok(());
        }
        fs::remove_file(index_path).map_err(|err| {
            IndexError::General(format!("Failed to discard unusable index: {err}"))
        })?;
    } else if let Some(parent) = index_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| IndexError::General(format!("Failed to create index dir: {err}")))?;
    }
    let index = Index::create(index_path)?;
    index.rebuild(graph)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempyr_core::node::parse_node;
    use tempyr_core::schema::Schema;

    fn make_graph(status: &str) -> Graph {
        let schema_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("schema/default-schema.toml");
        let mut graph = Graph::new(Schema::load(&schema_path).unwrap());
        let task = format!("---\nid: task-a\ntype: task\nstatus: {status}\n---\n# Task A\n");
        graph.add_node(parse_node(&task, PathBuf::from("task-a.md")).unwrap());
        graph
    }

    fn status_of(index_path: &Path) -> String {
        Index::open(index_path)
            .unwrap()
            .conn
            .query_row("SELECT status FROM nodes WHERE id = 'task-a'", [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    #[test]
    fn refresh_creates_missing_index() {
        let tmp = tempfile::tempdir().unwrap();
        let index_path = tmp.path().join("nested").join("index.db");

        refresh_index_at_path(&index_path, &make_graph("backlog")).unwrap();

        assert_eq!(status_of(&index_path), "backlog");
    }

    #[test]
    fn refresh_updates_existing_index_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        let index_path = tmp.path().join("index.db");
        refresh_index_at_path(&index_path, &make_graph("backlog")).unwrap();
        // A table the index never touches tells an in-place update apart from
        // a discard-and-rebuild.
        Index::open(&index_path)
            .unwrap()
            .conn
            .execute_batch("CREATE TABLE sentinel (x INTEGER);")
            .unwrap();

        refresh_index_at_path(&index_path, &make_graph("done")).unwrap();

        assert_eq!(status_of(&index_path), "done");
        let sentinel_tables: i64 = Index::open(&index_path)
            .unwrap()
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'sentinel'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            sentinel_tables, 1,
            "existing index should be updated, not replaced"
        );
    }

    #[test]
    fn refresh_rebuilds_unusable_index() {
        let tmp = tempfile::tempdir().unwrap();
        let index_path = tmp.path().join("index.db");
        fs::write(&index_path, b"definitely not a sqlite database").unwrap();

        refresh_index_at_path(&index_path, &make_graph("done")).unwrap();

        assert_eq!(status_of(&index_path), "done");
    }
}
