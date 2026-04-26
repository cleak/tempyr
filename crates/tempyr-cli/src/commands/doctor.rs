//! `tempyr doctor` — system status, health checks, and config-path discovery.
//!
//! Reports the active embedding provider, paths to all config files, index
//! state, and any warnings. Never prints API key values.

use crate::config::ProjectContext;
use tempyr_index::health::{self, HealthInputs, HealthReport};

pub fn run(ctx: &ProjectContext, json: bool) -> anyhow::Result<()> {
    let report = build_report(ctx);

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    render_text(&report);
    Ok(())
}

pub(crate) fn build_report(ctx: &ProjectContext) -> HealthReport {
    let inputs = HealthInputs {
        root: &ctx.root,
        graph_dir: &ctx.graph_dir,
        tempyr_dir: &ctx.tempyr_dir,
        cache: &ctx.cache,
        schema: &ctx.schema,
        tempyr_version: env!("CARGO_PKG_VERSION"),
    };
    health::build_report(&inputs)
}

fn render_text(report: &HealthReport) {
    println!("tempyr {}", report.tempyr_version);
    println!(
        "  local-embeddings feature: {}",
        if report.local_embeddings_compiled_in {
            "enabled"
        } else {
            "disabled"
        }
    );

    println!("\nProject");
    println!("  root:       {}", report.project.root.display());
    println!(
        "  graph dir:  {}{}",
        report.project.graph_dir.display(),
        missing_marker(report.project.graph_dir_exists)
    );
    println!(
        "  tempyr dir: {}{}",
        report.project.tempyr_dir.display(),
        missing_marker(report.project.tempyr_dir_exists)
    );
    println!(
        "  schema:     v{} ({} node types, {} edge types)",
        report.project.schema_version,
        report.project.schema_node_types.len(),
        report.project.schema_edge_types.len(),
    );

    println!("\nEmbedding");
    println!(
        "  provider:   {} (config: {})",
        report.embedding.provider, report.embedding.config_source,
    );
    if let Some(model) = &report.embedding.model {
        println!("  model:      {model}");
    }
    if let Some(dimensions) = report.embedding.dimensions {
        println!("  dimensions: {dimensions}");
    }
    if let Some(env_var) = &report.embedding.api_key_env_var {
        let state = match report.embedding.api_key_set {
            Some(true) => "set",
            Some(false) => "NOT set",
            None => "unknown",
        };
        println!("  API key:    {env_var} ({state}) [value never displayed]");
    } else {
        println!("  API key:    not required for this provider");
    }
    if let Some(path) = &report.embedding.store_path {
        let exists = match report.embedding.store_exists {
            Some(true) => "exists",
            Some(false) => "MISSING",
            None => "unknown",
        };
        println!("  store:      {} ({exists})", path.display());
    }
    if let Some(count) = report.embedding.store_count {
        println!("  store entries: {count}");
    }
    if let Some(err) = &report.embedding.config_error {
        println!("  config error: {err}");
    }
    if let Some(err) = &report.embedding.store_error {
        println!("  store error:  {err}");
    }

    println!("\nConfig files");
    for cf in &report.config_files {
        println!(
            "  {:<13} {} ({}) — {}",
            cf.name,
            cf.path.display(),
            if cf.exists { "exists" } else { "missing" },
            cf.purpose,
        );
    }

    println!("\nEnv files (load order)");
    for ef in &report.env_files {
        println!(
            "  {} ({})",
            ef.path.display(),
            if ef.exists { "exists" } else { "missing" }
        );
    }

    println!("\nGraph");
    match (&report.graph, &report.graph_error) {
        (Some(g), _) => {
            println!("  nodes: {}, edges: {}", g.node_count, g.edge_count);
            for (node_type, count) in &g.nodes_by_type {
                println!("    {node_type}: {count}");
            }
        }
        (None, Some(err)) => println!("  failed to load: {err}"),
        (None, None) => println!("  graph directory not found"),
    }

    println!("\nIndex");
    println!(
        "  active:   {} ({})",
        report.index.active_index_path.display(),
        if report.index.active_index_exists {
            "exists"
        } else {
            "missing"
        }
    );
    if report.index.legacy_index_exists {
        println!(
            "  legacy:   {} (exists)",
            report.index.legacy_index_path.display()
        );
    }
    match (
        &report.index.current_snapshot_index_path,
        report.index.current_snapshot_index_exists,
    ) {
        (Some(path), Some(true)) => {
            println!("  snapshot: {} (exists)", path.display());
        }
        (Some(path), _) => println!("  snapshot: {} (missing)", path.display()),
        (None, _) => println!("  snapshot: <none>"),
    }
    if let Some(key) = &report.index.snapshot_key {
        println!("  snapshot key: {key}");
    }
    if let Some(err) = &report.index.snapshot_key_error {
        println!("  snapshot key error: {err}");
    }
    if let Some(err) = &report.index.current_snapshot_index_error {
        println!("  snapshot lookup error: {err}");
    }
    if let Some(fts) = report.index.fts_entries {
        println!("  FTS entries: {fts}");
    }
    if let Some(count) = report.index.embedding_count_for_index {
        println!("  embedded nodes: {count}");
    }

    if !report.warnings.is_empty() {
        println!("\nWarnings");
        for warning in &report.warnings {
            println!("  - {warning}");
        }
    } else {
        println!("\nNo warnings.");
    }
}

fn missing_marker(exists: bool) -> &'static str {
    if exists { "" } else { " (MISSING)" }
}
