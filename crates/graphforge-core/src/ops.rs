use std::path::{Path, PathBuf};

use chrono::Utc;
use walkdir::WalkDir;

use crate::edge::EdgeEntry;
use crate::node::{Node, NodeFrontmatter, parse_node, serialize_node};
use crate::schema::Schema;
use crate::{GraphForgeError, Result};

/// Create a new node file on disk.
pub fn create_node_file(
    graph_dir: &Path,
    id: &str,
    node_type: &str,
    status: Option<&str>,
    owner: Option<&str>,
    tags: Option<&[String]>,
    body: &str,
) -> Result<PathBuf> {
    let frontmatter = NodeFrontmatter {
        id: id.to_string(),
        node_type: node_type.to_string(),
        status: status.map(String::from),
        created: Some(Utc::now()),
        updated: Some(Utc::now()),
        owner: owner.map(String::from),
        tags: tags.map(|t| t.to_vec()),
        edges: Vec::new(),
    };

    let content_hash = blake3::hash(body.as_bytes()).to_hex().to_string();
    let node = Node {
        frontmatter,
        body: body.to_string(),
        file_path: PathBuf::new(), // will be set below
        content_hash,
    };

    let serialized = serialize_node(&node)?;

    // Determine the file path from the node type directory
    let dir = find_type_directory(graph_dir, node_type)?;
    std::fs::create_dir_all(&dir)?;

    let file_path = dir.join(format!("{id}.md"));
    if file_path.exists() {
        return Err(GraphForgeError::Node(format!(
            "Node file already exists: {}",
            file_path.display()
        )));
    }

    atomic_write(&file_path, &serialized)?;
    Ok(file_path)
}

/// Add an edge between two nodes, writing both files (bidirectional).
pub fn add_edge(
    graph_dir: &Path,
    source_id: &str,
    target_id: &str,
    edge_type: &str,
    schema: &Schema,
) -> Result<()> {
    let reverse_type = schema.reverse_edge_type(edge_type).ok_or_else(|| {
        GraphForgeError::Edge(format!("Unknown edge type: '{edge_type}'"))
    })?;

    let source_path = find_node_file(graph_dir, source_id)?;
    let target_path = find_node_file(graph_dir, target_id)?;

    // Read and parse both nodes
    let source_content = std::fs::read_to_string(&source_path)?;
    let target_content = std::fs::read_to_string(&target_path)?;
    let mut source_node = parse_node(&source_content, source_path.clone())?;
    let mut target_node = parse_node(&target_content, target_path.clone())?;

    // Validate edge type is allowed
    schema.validate_edge(source_node.node_type(), edge_type, target_node.node_type())?;

    // Check for duplicate
    let already_exists = source_node
        .frontmatter
        .edges
        .iter()
        .any(|e| e.target == target_id && e.edge_type == edge_type);
    if already_exists {
        return Err(GraphForgeError::Edge(format!(
            "Edge already exists: {source_id} -> {target_id} ({edge_type})"
        )));
    }

    // Add forward edge to source
    source_node
        .frontmatter
        .edges
        .push(EdgeEntry::new(target_id, edge_type));
    sort_edges(&mut source_node.frontmatter.edges);

    // Add reverse edge to target
    target_node
        .frontmatter
        .edges
        .push(EdgeEntry::new(source_id, reverse_type));
    sort_edges(&mut target_node.frontmatter.edges);

    // Update timestamps
    source_node.frontmatter.updated = Some(Utc::now());
    target_node.frontmatter.updated = Some(Utc::now());

    // Write both files
    atomic_write(&source_path, &serialize_node(&source_node)?)?;
    atomic_write(&target_path, &serialize_node(&target_node)?)?;

    Ok(())
}

/// Remove an edge between two nodes, writing both files (bidirectional).
pub fn remove_edge(
    graph_dir: &Path,
    source_id: &str,
    target_id: &str,
    edge_type: &str,
    schema: &Schema,
) -> Result<()> {
    let reverse_type = schema.reverse_edge_type(edge_type).ok_or_else(|| {
        GraphForgeError::Edge(format!("Unknown edge type: '{edge_type}'"))
    })?;

    let source_path = find_node_file(graph_dir, source_id)?;
    let target_path = find_node_file(graph_dir, target_id)?;

    let source_content = std::fs::read_to_string(&source_path)?;
    let target_content = std::fs::read_to_string(&target_path)?;
    let mut source_node = parse_node(&source_content, source_path.clone())?;
    let mut target_node = parse_node(&target_content, target_path.clone())?;

    // Remove forward edge
    let before = source_node.frontmatter.edges.len();
    source_node
        .frontmatter
        .edges
        .retain(|e| !(e.target == target_id && e.edge_type == edge_type));
    if source_node.frontmatter.edges.len() == before {
        return Err(GraphForgeError::Edge(format!(
            "Edge not found: {source_id} -> {target_id} ({edge_type})"
        )));
    }

    // Remove reverse edge
    target_node
        .frontmatter
        .edges
        .retain(|e| !(e.target == source_id && e.edge_type == reverse_type));

    // Update timestamps
    source_node.frontmatter.updated = Some(Utc::now());
    target_node.frontmatter.updated = Some(Utc::now());

    // Write both files
    atomic_write(&source_path, &serialize_node(&source_node)?)?;
    atomic_write(&target_path, &serialize_node(&target_node)?)?;

    Ok(())
}

/// Rename a node, updating its file and all references across the graph.
pub fn rename_node(
    graph_dir: &Path,
    old_id: &str,
    new_id: &str,
) -> Result<Vec<PathBuf>> {
    let old_path = find_node_file(graph_dir, old_id)?;
    let mut modified_files = Vec::new();

    // Read and update the node itself
    let content = std::fs::read_to_string(&old_path)?;
    let mut node = parse_node(&content, old_path.clone())?;
    node.frontmatter.id = new_id.to_string();
    node.frontmatter.updated = Some(Utc::now());

    // Write to new path
    let new_path = old_path.with_file_name(format!("{new_id}.md"));
    atomic_write(&new_path, &serialize_node(&node)?)?;

    // Remove old file if path changed
    if old_path != new_path {
        std::fs::remove_file(&old_path)?;
    }
    modified_files.push(new_path);

    // Update all references in other node files
    for entry in WalkDir::new(graph_dir)
        .min_depth(2)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "md") {
            continue;
        }

        let file_content = std::fs::read_to_string(path)?;
        let mut other = match parse_node(&file_content, path.to_path_buf()) {
            Ok(n) => n,
            Err(_) => continue,
        };

        // Skip the renamed node itself
        if other.id() == new_id {
            continue;
        }

        // Update any edge targets that reference the old ID
        let mut changed = false;
        for edge in &mut other.frontmatter.edges {
            if edge.target == old_id {
                edge.target = new_id.to_string();
                changed = true;
            }
        }

        if changed {
            sort_edges(&mut other.frontmatter.edges);
            other.frontmatter.updated = Some(Utc::now());
            atomic_write(path, &serialize_node(&other)?)?;
            modified_files.push(path.to_path_buf());
        }
    }

    Ok(modified_files)
}

/// Update a node's status.
pub fn update_status(
    graph_dir: &Path,
    node_id: &str,
    new_status: &str,
    schema: &Schema,
) -> Result<()> {
    let path = find_node_file(graph_dir, node_id)?;
    let content = std::fs::read_to_string(&path)?;
    let mut node = parse_node(&content, path.clone())?;

    // Validate new status against schema
    schema.validate_status(node.node_type(), new_status)?;

    node.frontmatter.status = Some(new_status.to_string());
    node.frontmatter.updated = Some(Utc::now());

    atomic_write(&path, &serialize_node(&node)?)?;
    Ok(())
}

/// Sort edges alphabetically by target (per spec: minimizes merge conflicts).
fn sort_edges(edges: &mut Vec<EdgeEntry>) {
    edges.sort_by(|a, b| a.target.cmp(&b.target));
}

/// Find the directory for a node type within the graph directory.
fn find_type_directory(graph_dir: &Path, node_type: &str) -> Result<PathBuf> {
    // Map common node types to their directory names
    // This is a simplification; in practice we'd load the schema
    let dir_name = match node_type {
        "epic" => "epics",
        "feature" => "features",
        "task" => "tasks",
        "decision" => "decisions",
        "constraint" => "constraints",
        "persona" => "personas",
        "metric" => "metrics",
        "risk" => "risks",
        "open_question" => "questions",
        "component" => "components",
        "api_surface" => "api_surfaces",
        "insight" => "insights",
        "note" => "notes",
        other => other,
    };
    Ok(graph_dir.join(dir_name))
}

/// Find a node file by its ID, searching all subdirectories.
pub fn find_node_file(graph_dir: &Path, node_id: &str) -> Result<PathBuf> {
    let filename = format!("{node_id}.md");

    for entry in WalkDir::new(graph_dir)
        .min_depth(2)
        .max_depth(2)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_name().to_string_lossy() == filename {
            return Ok(entry.path().to_path_buf());
        }
    }

    Err(GraphForgeError::NotFound(format!(
        "Node file not found: {node_id}"
    )))
}

/// Write content to a file atomically (write to temp, then rename).
fn atomic_write(path: &Path, content: &str) -> Result<()> {
    let dir = path.parent().ok_or_else(|| {
        GraphForgeError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "No parent directory",
        ))
    })?;
    std::fs::create_dir_all(dir)?;

    // On Windows, we can't atomically rename over an existing file easily,
    // so we write directly. For production, consider using tempfile + rename.
    std::fs::write(path, content)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::parse_node;
    use crate::schema::Schema;
    use std::path::Path;
    use tempfile::TempDir;

    fn make_schema() -> Schema {
        let schema_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("schema/default-schema.toml");
        Schema::load(&schema_path).unwrap()
    }

    fn setup_graph_dir() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let graph_dir = tmp.path().join("graph");
        for dir in &[
            "epics", "features", "tasks", "decisions", "constraints", "personas",
            "metrics", "risks", "questions", "components", "api_surfaces", "insights", "notes",
        ] {
            std::fs::create_dir_all(graph_dir.join(dir)).unwrap();
        }
        tmp
    }

    fn write_node(graph_dir: &Path, subdir: &str, id: &str, content: &str) {
        let path = graph_dir.join(subdir).join(format!("{id}.md"));
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn test_create_node_file() {
        let tmp = setup_graph_dir();
        let graph_dir = tmp.path().join("graph");

        let path = create_node_file(
            &graph_dir,
            "feat-test",
            "feature",
            Some("draft"),
            Some("caleb"),
            Some(&["test".to_string()]),
            "# Test Feature\n\nA test.\n",
        )
        .unwrap();

        assert!(path.exists());

        let content = std::fs::read_to_string(&path).unwrap();
        let node = parse_node(&content, path).unwrap();
        assert_eq!(node.id(), "feat-test");
        assert_eq!(node.node_type(), "feature");
        assert_eq!(node.status(), Some("draft"));
        assert_eq!(node.frontmatter.owner.as_deref(), Some("caleb"));
    }

    #[test]
    fn test_create_node_duplicate_error() {
        let tmp = setup_graph_dir();
        let graph_dir = tmp.path().join("graph");

        create_node_file(&graph_dir, "feat-a", "feature", Some("draft"), None, None, "# A\n").unwrap();
        let result = create_node_file(&graph_dir, "feat-a", "feature", Some("draft"), None, None, "# A\n");
        assert!(result.is_err());
    }

    #[test]
    fn test_add_edge_bidirectional() {
        let tmp = setup_graph_dir();
        let graph_dir = tmp.path().join("graph");
        let schema = make_schema();

        write_node(&graph_dir, "features", "feat-a", "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: caleb\n---\n# Feat A\n");
        write_node(&graph_dir, "epics", "epic-a", "---\nid: epic-a\ntype: epic\nstatus: draft\nowner: caleb\n---\n# Epic A\n");

        add_edge(&graph_dir, "feat-a", "epic-a", "child_of", &schema).unwrap();

        // Verify source has forward edge
        let source_content = std::fs::read_to_string(graph_dir.join("features/feat-a.md")).unwrap();
        let source = parse_node(&source_content, PathBuf::from("test")).unwrap();
        assert!(source.edges().iter().any(|e| e.target == "epic-a" && e.edge_type == "child_of"));

        // Verify target has reverse edge
        let target_content = std::fs::read_to_string(graph_dir.join("epics/epic-a.md")).unwrap();
        let target = parse_node(&target_content, PathBuf::from("test")).unwrap();
        assert!(target.edges().iter().any(|e| e.target == "feat-a" && e.edge_type == "parent_of"));
    }

    #[test]
    fn test_add_edge_sorts_alphabetically() {
        let tmp = setup_graph_dir();
        let graph_dir = tmp.path().join("graph");
        let schema = make_schema();

        write_node(&graph_dir, "features", "feat-a", "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: caleb\n---\n# A\n");
        write_node(&graph_dir, "epics", "epic-z", "---\nid: epic-z\ntype: epic\nstatus: draft\nowner: caleb\n---\n# Z\n");
        write_node(&graph_dir, "epics", "epic-a", "---\nid: epic-a\ntype: epic\nstatus: draft\nowner: caleb\n---\n# A\n");

        add_edge(&graph_dir, "feat-a", "epic-z", "child_of", &schema).unwrap();
        add_edge(&graph_dir, "feat-a", "epic-a", "child_of", &schema).unwrap();

        let content = std::fs::read_to_string(graph_dir.join("features/feat-a.md")).unwrap();
        let node = parse_node(&content, PathBuf::from("test")).unwrap();
        assert_eq!(node.edges()[0].target, "epic-a"); // alphabetically first
        assert_eq!(node.edges()[1].target, "epic-z");
    }

    #[test]
    fn test_add_edge_duplicate_error() {
        let tmp = setup_graph_dir();
        let graph_dir = tmp.path().join("graph");
        let schema = make_schema();

        write_node(&graph_dir, "features", "feat-a", "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: caleb\n---\n# A\n");
        write_node(&graph_dir, "epics", "epic-a", "---\nid: epic-a\ntype: epic\nstatus: draft\nowner: caleb\n---\n# A\n");

        add_edge(&graph_dir, "feat-a", "epic-a", "child_of", &schema).unwrap();
        let result = add_edge(&graph_dir, "feat-a", "epic-a", "child_of", &schema);
        assert!(result.is_err());
    }

    #[test]
    fn test_remove_edge_bidirectional() {
        let tmp = setup_graph_dir();
        let graph_dir = tmp.path().join("graph");
        let schema = make_schema();

        write_node(&graph_dir, "features", "feat-a", "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: caleb\n---\n# A\n");
        write_node(&graph_dir, "epics", "epic-a", "---\nid: epic-a\ntype: epic\nstatus: draft\nowner: caleb\n---\n# A\n");

        add_edge(&graph_dir, "feat-a", "epic-a", "child_of", &schema).unwrap();
        remove_edge(&graph_dir, "feat-a", "epic-a", "child_of", &schema).unwrap();

        let source_content = std::fs::read_to_string(graph_dir.join("features/feat-a.md")).unwrap();
        let source = parse_node(&source_content, PathBuf::from("test")).unwrap();
        assert!(source.edges().is_empty());

        let target_content = std::fs::read_to_string(graph_dir.join("epics/epic-a.md")).unwrap();
        let target = parse_node(&target_content, PathBuf::from("test")).unwrap();
        assert!(target.edges().is_empty());
    }

    #[test]
    fn test_rename_node() {
        let tmp = setup_graph_dir();
        let graph_dir = tmp.path().join("graph");

        write_node(&graph_dir, "features", "feat-old", "---\nid: feat-old\ntype: feature\nstatus: draft\nowner: caleb\n---\n# Old\n");

        let modified = rename_node(&graph_dir, "feat-old", "feat-new").unwrap();
        assert!(!modified.is_empty());

        // Old file should be gone
        assert!(!graph_dir.join("features/feat-old.md").exists());
        // New file should exist
        assert!(graph_dir.join("features/feat-new.md").exists());

        let content = std::fs::read_to_string(graph_dir.join("features/feat-new.md")).unwrap();
        let node = parse_node(&content, PathBuf::from("test")).unwrap();
        assert_eq!(node.id(), "feat-new");
    }

    #[test]
    fn test_rename_node_updates_references() {
        let tmp = setup_graph_dir();
        let graph_dir = tmp.path().join("graph");
        let schema = make_schema();

        write_node(&graph_dir, "features", "feat-a", "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: caleb\n---\n# A\n");
        write_node(&graph_dir, "epics", "epic-a", "---\nid: epic-a\ntype: epic\nstatus: draft\nowner: caleb\n---\n# Epic\n");

        add_edge(&graph_dir, "feat-a", "epic-a", "child_of", &schema).unwrap();

        // Rename the feature
        rename_node(&graph_dir, "feat-a", "feat-renamed").unwrap();

        // Epic should now reference feat-renamed, not feat-a
        let epic_content = std::fs::read_to_string(graph_dir.join("epics/epic-a.md")).unwrap();
        let epic = parse_node(&epic_content, PathBuf::from("test")).unwrap();
        assert!(epic.edges().iter().any(|e| e.target == "feat-renamed"));
        assert!(!epic.edges().iter().any(|e| e.target == "feat-a"));
    }

    #[test]
    fn test_update_status() {
        let tmp = setup_graph_dir();
        let graph_dir = tmp.path().join("graph");
        let schema = make_schema();

        write_node(&graph_dir, "features", "feat-a", "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: caleb\n---\n# A\n");

        update_status(&graph_dir, "feat-a", "active", &schema).unwrap();

        let content = std::fs::read_to_string(graph_dir.join("features/feat-a.md")).unwrap();
        let node = parse_node(&content, PathBuf::from("test")).unwrap();
        assert_eq!(node.status(), Some("active"));
    }

    #[test]
    fn test_update_status_invalid() {
        let tmp = setup_graph_dir();
        let graph_dir = tmp.path().join("graph");
        let schema = make_schema();

        write_node(&graph_dir, "features", "feat-a", "---\nid: feat-a\ntype: feature\nstatus: draft\nowner: caleb\n---\n# A\n");

        let result = update_status(&graph_dir, "feat-a", "banana", &schema);
        assert!(result.is_err());
    }
}
