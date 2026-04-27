//! `tempyr doctor` — system status, health checks, and config-path discovery.
//!
//! Reports the active embedding provider, paths to all config files, index
//! state, and any warnings. Never prints API key values.

use std::io::{self, Write};

use crate::config::ProjectContext;
use tempyr_index::health::{self, HealthInputs, HealthReport};

pub fn run(ctx: &ProjectContext, json: bool) -> anyhow::Result<()> {
    let report = build_report(ctx);

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    let stdout = io::stdout();
    let mut handle = stdout.lock();
    render_text(&mut handle, &report)?;
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

/// Render the report as human-readable text. Tests can pass a `Vec<u8>`
/// buffer in place of stdout to assert on the exact output.
pub(crate) fn render_text(out: &mut impl Write, report: &HealthReport) -> io::Result<()> {
    writeln!(out, "tempyr {}", report.tempyr_version)?;
    writeln!(
        out,
        "  local-embeddings feature: {}",
        if report.local_embeddings_compiled_in {
            "enabled"
        } else {
            "disabled"
        }
    )?;

    writeln!(out, "\nProject")?;
    writeln!(out, "  root:       {}", report.project.root.display())?;
    writeln!(
        out,
        "  graph dir:  {}{}",
        report.project.graph_dir.display(),
        missing_marker(report.project.graph_dir_exists)
    )?;
    writeln!(
        out,
        "  tempyr dir: {}{}",
        report.project.tempyr_dir.display(),
        missing_marker(report.project.tempyr_dir_exists)
    )?;
    writeln!(
        out,
        "  schema:     v{} ({} node types, {} edge types)",
        report.project.schema_version,
        report.project.schema_node_types.len(),
        report.project.schema_edge_types.len(),
    )?;

    writeln!(out, "\nEmbedding")?;
    writeln!(
        out,
        "  provider:   {} (config: {})",
        report.embedding.provider, report.embedding.config_source,
    )?;
    if let Some(model) = &report.embedding.model {
        writeln!(out, "  model:      {model}")?;
    }
    if let Some(dimensions) = report.embedding.dimensions {
        writeln!(out, "  dimensions: {dimensions}")?;
    }
    if let Some(env_var) = &report.embedding.api_key_env_var {
        let state = match report.embedding.api_key_set {
            Some(true) => "set",
            Some(false) => "NOT set",
            None => "unknown",
        };
        writeln!(
            out,
            "  API key:    {env_var} ({state}) [value never displayed]"
        )?;
    } else {
        writeln!(out, "  API key:    not required for this provider")?;
    }
    if let Some(path) = &report.embedding.store_path {
        let exists = match report.embedding.store_exists {
            Some(true) => "exists",
            Some(false) => "MISSING",
            None => "unknown",
        };
        writeln!(out, "  store:      {} ({exists})", path.display())?;
    }
    if let Some(count) = report.embedding.store_count {
        writeln!(out, "  store entries: {count}")?;
    }
    if let Some(err) = &report.embedding.config_error {
        writeln!(out, "  config error: {err}")?;
    }
    if let Some(err) = &report.embedding.store_error {
        writeln!(out, "  store error:  {err}")?;
    }

    writeln!(out, "\nConfig files")?;
    for cf in &report.config_files {
        writeln!(
            out,
            "  {:<13} {} ({}) — {}",
            cf.name,
            cf.path.display(),
            if cf.exists { "exists" } else { "missing" },
            cf.purpose,
        )?;
    }

    writeln!(out, "\nEnv files (load order)")?;
    for ef in &report.env_files {
        writeln!(
            out,
            "  {} ({})",
            ef.path.display(),
            if ef.exists { "exists" } else { "missing" }
        )?;
    }

    writeln!(out, "\nGraph")?;
    match (&report.graph, &report.graph_error) {
        (Some(g), _) => {
            writeln!(out, "  nodes: {}, edges: {}", g.node_count, g.edge_count)?;
            for (node_type, count) in &g.nodes_by_type {
                writeln!(out, "    {node_type}: {count}")?;
            }
        }
        (None, Some(err)) => writeln!(out, "  failed to load: {err}")?,
        (None, None) => writeln!(out, "  graph directory not found")?,
    }

    writeln!(out, "\nIndex")?;
    writeln!(
        out,
        "  active:   {} ({})",
        report.index.active_index_path.display(),
        if report.index.active_index_exists {
            "exists"
        } else {
            "missing"
        }
    )?;
    if report.index.legacy_index_exists {
        writeln!(
            out,
            "  legacy:   {} (exists)",
            report.index.legacy_index_path.display()
        )?;
    }
    match (
        &report.index.current_snapshot_index_path,
        report.index.current_snapshot_index_exists,
    ) {
        (Some(path), Some(true)) => {
            writeln!(out, "  snapshot: {} (exists)", path.display())?;
        }
        (Some(path), _) => writeln!(out, "  snapshot: {} (missing)", path.display())?,
        (None, _) => writeln!(out, "  snapshot: <none>")?,
    }
    if let Some(key) = &report.index.snapshot_key {
        writeln!(out, "  snapshot key: {key}")?;
    }
    if let Some(err) = &report.index.snapshot_key_error {
        writeln!(out, "  snapshot key error: {err}")?;
    }
    if let Some(err) = &report.index.current_snapshot_index_error {
        writeln!(out, "  snapshot lookup error: {err}")?;
    }
    if let Some(fts) = report.index.fts_entries {
        writeln!(out, "  FTS entries: {fts}")?;
    }
    if let Some(count) = report.index.embedding_count_for_index {
        writeln!(out, "  embedded nodes: {count}")?;
    }

    if !report.warnings.is_empty() {
        writeln!(out, "\nWarnings")?;
        for warning in &report.warnings {
            writeln!(out, "  - {warning}")?;
        }
    } else {
        writeln!(out, "\nNo warnings.")?;
    }

    Ok(())
}

fn missing_marker(exists: bool) -> &'static str {
    if exists { "" } else { " (MISSING)" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempyr_index::health::{
        ConfigFileEntry, EmbeddingSection, EnvFileEntry, GraphSection, HealthReport, IndexSection,
        ProjectSection,
    };

    fn fixture_report() -> HealthReport {
        HealthReport {
            tempyr_version: "9.9.9".to_string(),
            local_embeddings_compiled_in: false,
            project: ProjectSection {
                root: "/tmp/proj".into(),
                graph_dir: "/tmp/proj/graph".into(),
                graph_dir_exists: true,
                tempyr_dir: "/tmp/proj/.tempyr".into(),
                tempyr_dir_exists: true,
                schema_version: "1.0.0".to_string(),
                schema_node_types: vec!["feature".to_string(), "task".to_string()],
                schema_edge_types: vec!["child_of".to_string(), "parent_of".to_string()],
            },
            embedding: EmbeddingSection {
                provider: "voyage".to_string(),
                model: Some("voyage-4".to_string()),
                dimensions: Some(1024),
                config_source: "default".to_string(),
                config_error: None,
                api_key_env_var: Some("VOYAGE_API_KEY".to_string()),
                api_key_set: Some(true),
                store_path: Some("/tmp/proj/.tempyr/cache/embeddings/abc.db".into()),
                store_exists: Some(false),
                store_count: None,
                store_error: None,
            },
            config_files: vec![ConfigFileEntry {
                name: "schema.toml".to_string(),
                path: "/tmp/proj/.tempyr/schema.toml".into(),
                exists: true,
                purpose: "Node/edge type definitions".to_string(),
            }],
            env_files: vec![EnvFileEntry {
                path: "/tmp/proj/.env".into(),
                exists: false,
            }],
            graph: Some(GraphSection {
                node_count: 3,
                edge_count: 2,
                nodes_by_type: vec![("feature".to_string(), 2), ("task".to_string(), 1)],
            }),
            graph_error: None,
            index: IndexSection {
                active_index_path: "/tmp/proj/.tempyr/cache/index.db".into(),
                active_index_exists: false,
                legacy_index_path: "/tmp/proj/.tempyr/index.db".into(),
                legacy_index_exists: false,
                current_snapshot_index_path: None,
                current_snapshot_index_exists: None,
                current_snapshot_index_error: None,
                snapshot_key: Some("abc123".to_string()),
                snapshot_key_error: None,
                fts_entries: None,
                embedding_count_for_index: None,
            },
            warnings: vec!["something is amiss".to_string()],
        }
    }

    fn render_to_string(report: &HealthReport) -> String {
        let mut buf: Vec<u8> = Vec::new();
        render_text(&mut buf, report).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn render_text_emits_sections_in_expected_order() {
        let out = render_to_string(&fixture_report());

        // Each section header must appear, and they must be in this order.
        let expected_order = [
            "tempyr 9.9.9",
            "Project",
            "Embedding",
            "Config files",
            "Env files (load order)",
            "Graph",
            "Index",
            "Warnings",
        ];
        let mut cursor = 0;
        for marker in expected_order {
            let position = out[cursor..]
                .find(marker)
                .unwrap_or_else(|| panic!("missing section {marker:?} in output:\n{out}"));
            cursor += position + marker.len();
        }
    }

    #[test]
    fn render_text_redacts_api_key_and_never_inlines_value() {
        // Regression guard: render_text must only echo the env var name and a
        // status word — never any value sourced from the environment.
        let report = fixture_report();
        let out = render_to_string(&report);

        assert!(out.contains("VOYAGE_API_KEY (set) [value never displayed]"));
        // The renderer reads no env vars itself, so this is a structural assertion:
        // ensure no field on the embedding section is named in a way that could
        // leak a secret. If a future change adds e.g. `embedding.api_key`, the
        // rendered output would still not include it unless render_text is
        // modified — the redaction phrase test above pins that line.
        assert!(
            !out.contains("api_key:"),
            "unexpected api_key field in output"
        );
    }

    #[test]
    fn render_text_lists_warnings_when_present() {
        let out = render_to_string(&fixture_report());
        assert!(out.contains("Warnings"));
        assert!(out.contains("- something is amiss"));
    }

    #[test]
    fn render_text_says_no_warnings_when_empty() {
        let mut report = fixture_report();
        report.warnings.clear();
        let out = render_to_string(&report);
        assert!(out.contains("No warnings."));
        assert!(!out.contains("\nWarnings\n"));
    }
}
