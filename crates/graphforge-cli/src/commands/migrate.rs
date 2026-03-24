use std::path::Path;

use crate::config::ProjectContext;
use graphforge_core::node::{parse_node, serialize_node};

use walkdir::WalkDir;

pub fn run(ctx: &ProjectContext, args: &[String]) -> anyhow::Result<()> {
    if args.is_empty() {
        println!("Usage: graphforge migrate <command> [args...]");
        println!();
        println!("Commands:");
        println!("  rename-type <old-type> <new-type>     Rename a node type across all files");
        println!("  rename-status <type> <old> <new>      Rename a status value for a node type");
        println!("  add-field <type> <field> <default>    Add a field with default value to all nodes of a type");
        println!("  rename-edge <old-type> <new-type>     Rename an edge type across all files");
        return Ok(());
    }

    match args[0].as_str() {
        "rename-type" => {
            if args.len() != 3 {
                anyhow::bail!("Usage: graphforge migrate rename-type <old-type> <new-type>");
            }
            rename_type(&ctx.graph_dir, &args[1], &args[2])
        }
        "rename-status" => {
            if args.len() != 4 {
                anyhow::bail!("Usage: graphforge migrate rename-status <node-type> <old-status> <new-status>");
            }
            rename_status(&ctx.graph_dir, &args[1], &args[2], &args[3])
        }
        "add-field" => {
            if args.len() != 4 {
                anyhow::bail!("Usage: graphforge migrate add-field <node-type> <field-name> <default-value>");
            }
            add_field(&ctx.graph_dir, &args[1], &args[2], &args[3])
        }
        "rename-edge" => {
            if args.len() != 3 {
                anyhow::bail!("Usage: graphforge migrate rename-edge <old-edge-type> <new-edge-type>");
            }
            rename_edge_type(&ctx.graph_dir, &args[1], &args[2])
        }
        other => anyhow::bail!("Unknown migration command: {other}"),
    }
}

fn rename_type(graph_dir: &Path, old_type: &str, new_type: &str) -> anyhow::Result<()> {
    let mut modified = 0;

    for entry in WalkDir::new(graph_dir).min_depth(2).into_iter().filter_map(|e| e.ok()) {
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

fn rename_status(graph_dir: &Path, node_type: &str, old_status: &str, new_status: &str) -> anyhow::Result<()> {
    let mut modified = 0;

    for entry in WalkDir::new(graph_dir).min_depth(2).into_iter().filter_map(|e| e.ok()) {
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

    println!("Renamed status '{old_status}' to '{new_status}' for type '{node_type}' in {modified} file(s).");
    Ok(())
}

fn add_field(graph_dir: &Path, node_type: &str, field: &str, default: &str) -> anyhow::Result<()> {
    let mut modified = 0;

    for entry in WalkDir::new(graph_dir).min_depth(2).into_iter().filter_map(|e| e.ok()) {
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
                    println!("  Skipping {}: custom field '{field}' not supported in migration", node.id());
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

fn rename_edge_type(graph_dir: &Path, old_type: &str, new_type: &str) -> anyhow::Result<()> {
    let mut modified = 0;

    for entry in WalkDir::new(graph_dir).min_depth(2).into_iter().filter_map(|e| e.ok()) {
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
