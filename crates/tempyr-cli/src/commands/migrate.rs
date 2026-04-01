use std::collections::HashSet;
use std::path::Path;

use crate::config::ProjectContext;
use tempyr_core::id;
use tempyr_core::node::{parse_node, serialize_node};
use tempyr_core::ops;

use walkdir::WalkDir;

pub fn run(ctx: &ProjectContext, args: &[String]) -> anyhow::Result<()> {
    if args.is_empty() {
        println!("Usage: tempyr migrate <command> [args...]");
        println!();
        println!("Commands:");
        println!(
            "  add-suffix [--dry-run]                Add 6-char hybrid ID suffix to all legacy nodes"
        );
        println!("  rename-type <old-type> <new-type>     Rename a node type across all files");
        println!("  rename-status <type> <old> <new>      Rename a status value for a node type");
        println!(
            "  add-field <type> <field> <default>    Add a field with default value to all nodes of a type"
        );
        println!("  rename-edge <old-type> <new-type>     Rename an edge type across all files");
        return Ok(());
    }

    match args[0].as_str() {
        "add-suffix" => {
            let dry_run = args.iter().any(|a| a == "--dry-run");
            add_suffix(&ctx.graph_dir, &ctx.tempyr_dir, dry_run)
        }
        "rename-type" => {
            if args.len() != 3 {
                anyhow::bail!("Usage: tempyr migrate rename-type <old-type> <new-type>");
            }
            rename_type(&ctx.graph_dir, &args[1], &args[2])
        }
        "rename-status" => {
            if args.len() != 4 {
                anyhow::bail!(
                    "Usage: tempyr migrate rename-status <node-type> <old-status> <new-status>"
                );
            }
            rename_status(&ctx.graph_dir, &args[1], &args[2], &args[3])
        }
        "add-field" => {
            if args.len() != 4 {
                anyhow::bail!(
                    "Usage: tempyr migrate add-field <node-type> <field-name> <default-value>"
                );
            }
            add_field(&ctx.graph_dir, &args[1], &args[2], &args[3])
        }
        "rename-edge" => {
            if args.len() != 3 {
                anyhow::bail!("Usage: tempyr migrate rename-edge <old-edge-type> <new-edge-type>");
            }
            rename_edge_type(&ctx.graph_dir, &args[1], &args[2])
        }
        other => anyhow::bail!("Unknown migration command: {other}"),
    }
}

fn rename_type(graph_dir: &Path, old_type: &str, new_type: &str) -> anyhow::Result<()> {
    let mut modified = 0;

    for entry in WalkDir::new(graph_dir)
        .min_depth(2)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "md") {
            continue;
        }

        let content = std::fs::read_to_string(path)?;
        let mut node = match parse_node(&content, path.to_path_buf()) {
            Ok(n) => n,
            Err(_) => continue,
        };

        if node.node_type() == old_type {
            node.frontmatter.node_type = new_type.to_string();
            std::fs::write(path, serialize_node(&node)?)?;
            modified += 1;
        }
    }

    println!("Renamed type '{old_type}' to '{new_type}' in {modified} file(s).");
    println!("Remember to update schema.toml to reflect this change.");
    Ok(())
}

fn rename_status(
    graph_dir: &Path,
    node_type: &str,
    old_status: &str,
    new_status: &str,
) -> anyhow::Result<()> {
    let mut modified = 0;

    for entry in WalkDir::new(graph_dir)
        .min_depth(2)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "md") {
            continue;
        }

        let content = std::fs::read_to_string(path)?;
        let mut node = match parse_node(&content, path.to_path_buf()) {
            Ok(n) => n,
            Err(_) => continue,
        };

        if node.node_type() == node_type && node.status() == Some(old_status) {
            node.frontmatter.status = Some(new_status.to_string());
            std::fs::write(path, serialize_node(&node)?)?;
            modified += 1;
        }
    }

    println!(
        "Renamed status '{old_status}' to '{new_status}' for type '{node_type}' in {modified} file(s)."
    );
    Ok(())
}

fn add_field(graph_dir: &Path, node_type: &str, field: &str, default: &str) -> anyhow::Result<()> {
    let mut modified = 0;

    for entry in WalkDir::new(graph_dir)
        .min_depth(2)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "md") {
            continue;
        }

        let content = std::fs::read_to_string(path)?;
        let node = match parse_node(&content, path.to_path_buf()) {
            Ok(n) => n,
            Err(_) => continue,
        };

        if node.node_type() != node_type {
            continue;
        }

        // Check if field already exists by looking for it in frontmatter
        let has_field = match field {
            "status" => node.frontmatter.status.is_some(),
            "owner" => node.frontmatter.owner.is_some(),
            _ => false, // Custom fields would need serde_yml manipulation
        };

        if !has_field {
            let mut node = node;
            match field {
                "status" => node.frontmatter.status = Some(default.to_string()),
                "owner" => node.frontmatter.owner = Some(default.to_string()),
                _ => {
                    println!(
                        "  Skipping {}: custom field '{field}' not supported in migration",
                        node.id()
                    );
                    continue;
                }
            }
            std::fs::write(path, serialize_node(&node)?)?;
            modified += 1;
        }
    }

    println!("Added field '{field}' with default '{default}' to {modified} {node_type} node(s).");
    Ok(())
}

/// Migrate all legacy nodes to hybrid IDs by stripping type prefixes and
/// appending a 6-char Crockford Base32 suffix.
fn add_suffix(graph_dir: &Path, tempyr_dir: &Path, dry_run: bool) -> anyhow::Result<()> {
    // Collect all current node IDs and identify which need migration
    let mut to_migrate: Vec<(String, String, String)> = Vec::new(); // (old_id, new_slug, node_type)
    let mut existing_suffixes: HashSet<String> = id::collect_existing_suffixes(graph_dir);

    for entry in WalkDir::new(graph_dir)
        .min_depth(2)
        .max_depth(2)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "md") {
            continue;
        }

        let content = std::fs::read_to_string(path)?;
        let node = match parse_node(&content, path.to_path_buf()) {
            Ok(n) => n,
            Err(_) => continue,
        };

        let old_id = node.id().to_string();

        // Skip nodes that already have a hybrid ID with a known suffix.
        // is_hybrid_id alone isn't enough — words like "system" pass
        // Crockford validation. Cross-check against suffixes we've seen.
        // The prefix check avoids self-confirming false positives from legacy IDs like
        // `feat-system`, where the trailing segment happens to be six valid Crockford chars.
        if id::parse_node_id(&old_id).is_some()
            && !id::has_legacy_type_prefix(&old_id, node.node_type())
        {
            continue;
        }

        let slug = id::strip_type_prefix(&old_id).to_string();
        to_migrate.push((old_id, slug, node.node_type().to_string()));
    }

    if to_migrate.is_empty() {
        println!("All nodes already have hybrid IDs. Nothing to migrate.");
        return Ok(());
    }

    // Generate suffixes and plan renames
    let mut renames: Vec<(String, String)> = Vec::new(); // (old_id, new_id)
    for (old_id, slug, _node_type) in &to_migrate {
        let suffix = id::generate_suffix(&existing_suffixes);
        existing_suffixes.insert(suffix.clone());
        let new_id = format!("{slug}-{suffix}");
        renames.push((old_id.clone(), new_id));
    }

    if dry_run {
        println!("Dry run — would migrate {} node(s):", renames.len());
        for (old, new) in &renames {
            println!("  {old} -> {new}");
        }
        return Ok(());
    }

    // Execute renames
    let mut total_modified = 0;
    for (old_id, new_id) in &renames {
        let modified = ops::rename_node(graph_dir, old_id, new_id)?;
        total_modified += modified.len();
        println!("  {old_id} -> {new_id} ({} files)", modified.len());
    }

    // Update linear-sync.json if it exists
    let sync_path = tempyr_dir.join("linear-sync.json");
    if sync_path.exists() {
        let sync_content = std::fs::read_to_string(&sync_path)?;
        let mut updated = sync_content.clone();
        for (old_id, new_id) in &renames {
            updated = updated.replace(&format!("\"{old_id}\""), &format!("\"{new_id}\""));
        }
        if updated != sync_content {
            std::fs::write(&sync_path, &updated)?;
            println!("Updated linear-sync.json");
        }
    }

    println!(
        "\nMigrated {} nodes ({} files modified). Run `tempyr index rebuild` to update the index.",
        renames.len(),
        total_modified,
    );

    Ok(())
}

fn rename_edge_type(graph_dir: &Path, old_type: &str, new_type: &str) -> anyhow::Result<()> {
    let mut modified = 0;

    for entry in WalkDir::new(graph_dir)
        .min_depth(2)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "md") {
            continue;
        }

        let content = std::fs::read_to_string(path)?;
        let mut node = match parse_node(&content, path.to_path_buf()) {
            Ok(n) => n,
            Err(_) => continue,
        };

        let mut changed = false;
        for edge in &mut node.frontmatter.edges {
            if edge.edge_type == old_type {
                edge.edge_type = new_type.to_string();
                changed = true;
            }
        }

        if changed {
            std::fs::write(path, serialize_node(&node)?)?;
            modified += 1;
        }
    }

    println!("Renamed edge type '{old_type}' to '{new_type}' in {modified} file(s).");
    println!("Remember to update schema.toml to reflect this change.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_node(graph_dir: &Path, node_type_dir: &str, id: &str, node_type: &str) {
        let dir = graph_dir.join(node_type_dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(format!("{id}.md")),
            format!("---\nid: {id}\ntype: {node_type}\nstatus: draft\nowner: caleb\n---\n# {id}\n"),
        )
        .unwrap();
    }

    #[test]
    fn add_suffix_migrates_legacy_id_that_looks_hybrid() {
        let tmp = tempfile::tempdir().unwrap();
        let graph_dir = tmp.path().join("graph");
        let tempyr_dir = tmp.path().join(".tempyr");
        fs::create_dir_all(&tempyr_dir).unwrap();

        write_node(&graph_dir, "features", "feat-system", "feature");

        add_suffix(&graph_dir, &tempyr_dir, false).unwrap();

        assert!(!graph_dir.join("features/feat-system.md").exists());
        let entries = fs::read_dir(graph_dir.join("features"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].starts_with("system-"));
    }

    #[test]
    fn add_suffix_skips_existing_hybrid_id() {
        let tmp = tempfile::tempdir().unwrap();
        let graph_dir = tmp.path().join("graph");
        let tempyr_dir = tmp.path().join(".tempyr");
        fs::create_dir_all(&tempyr_dir).unwrap();

        write_node(&graph_dir, "features", "session-replay-a1b2c3", "feature");

        add_suffix(&graph_dir, &tempyr_dir, false).unwrap();

        assert!(graph_dir.join("features/session-replay-a1b2c3.md").exists());
        let entries = fs::read_dir(graph_dir.join("features"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(entries, vec!["session-replay-a1b2c3.md".to_string()]);
    }
}
