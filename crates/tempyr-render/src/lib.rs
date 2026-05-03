//! Document rendering for Tempyr.
//!
//! PRDs, TDDs, and similar documents are rendered views over the graph: a
//! TOML template selects a root node and walks specified edges, then a
//! formatter assembles the collected nodes into a Markdown document.

pub mod collector;
pub mod formatter;
pub mod template;

use std::path::Path;

use tempyr_core::graph::Graph;
use tempyr_core::temporal::TemporalFilter;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RenderError {
    #[error("Core error: {0}")]
    Core(#[from] tempyr_core::TempyrError),

    #[error("Template error: {0}")]
    Template(String),

    #[error("Render error: {0}")]
    General(String),
}

pub type Result<T> = std::result::Result<T, RenderError>;

/// Request issued by a semantic-search render section.
#[derive(Debug, Clone, PartialEq)]
pub struct SemanticSearchRequest {
    pub query: String,
    pub target_type: Option<String>,
    pub max_results: usize,
    pub min_similarity: Option<f64>,
}

/// One semantic-search result returned to the renderer.
#[derive(Debug, Clone, PartialEq)]
pub struct SemanticSearchHit {
    pub node_id: String,
    pub score: f64,
}

/// Semantic search implementation supplied by callers that have index access.
pub trait SemanticSearchProvider {
    fn search(&mut self, request: &SemanticSearchRequest) -> Result<Vec<SemanticSearchHit>>;
}

#[derive(Default)]
pub struct RenderOptions<'a> {
    pub semantic_search: Option<&'a mut dyn SemanticSearchProvider>,
}

/// Render a document from a graph using a template.
pub fn render(
    graph: &Graph,
    template_path: &Path,
    root_id: &str,
    temporal_filter: &TemporalFilter,
) -> Result<String> {
    render_with_options(
        graph,
        template_path,
        root_id,
        temporal_filter,
        RenderOptions::default(),
    )
}

/// Render a document from a graph using a template and caller-provided options.
pub fn render_with_options(
    graph: &Graph,
    template_path: &Path,
    root_id: &str,
    temporal_filter: &TemporalFilter,
    options: RenderOptions<'_>,
) -> Result<String> {
    let tmpl = template::RenderTemplate::load(template_path)?;

    // Validate root type
    let root = graph
        .get_node(root_id)
        .ok_or_else(|| RenderError::General(format!("Root node not found: '{root_id}'")))?;

    if !tmpl.meta.root_types.contains(&root.node_type().to_string()) {
        return Err(RenderError::General(format!(
            "Node '{}' is type '{}', but template '{}' requires one of: {:?}",
            root_id,
            root.node_type(),
            tmpl.meta.name,
            tmpl.meta.root_types
        )));
    }

    // Collect all sections
    let sections = collect_sections(
        graph,
        root,
        &tmpl.sections,
        temporal_filter,
        options.semantic_search,
    )?;

    // Format to markdown
    Ok(formatter::render_to_markdown(&tmpl, root, sections))
}

/// Render using a template string (for built-in templates).
pub fn render_from_str(
    graph: &Graph,
    template_toml: &str,
    root_id: &str,
    temporal_filter: &TemporalFilter,
) -> Result<String> {
    render_from_str_with_options(
        graph,
        template_toml,
        root_id,
        temporal_filter,
        RenderOptions::default(),
    )
}

/// Render using a template string and caller-provided options.
pub fn render_from_str_with_options(
    graph: &Graph,
    template_toml: &str,
    root_id: &str,
    temporal_filter: &TemporalFilter,
    options: RenderOptions<'_>,
) -> Result<String> {
    let tmpl: template::RenderTemplate = template_toml.parse()?;

    let root = graph
        .get_node(root_id)
        .ok_or_else(|| RenderError::General(format!("Root node not found: '{root_id}'")))?;

    if !tmpl.meta.root_types.contains(&root.node_type().to_string()) {
        return Err(RenderError::General(format!(
            "Node '{}' is type '{}', but template '{}' requires one of: {:?}",
            root_id,
            root.node_type(),
            tmpl.meta.name,
            tmpl.meta.root_types
        )));
    }

    let sections = collect_sections(
        graph,
        root,
        &tmpl.sections,
        temporal_filter,
        options.semantic_search,
    )?;

    Ok(formatter::render_to_markdown(&tmpl, root, sections))
}

fn collect_sections(
    graph: &Graph,
    root: &tempyr_core::node::Node,
    section_defs: &[template::SectionDef],
    temporal_filter: &TemporalFilter,
    semantic_search: Option<&mut dyn SemanticSearchProvider>,
) -> Result<Vec<collector::SectionData>> {
    let mut sections = Vec::with_capacity(section_defs.len());
    if let Some(provider) = semantic_search {
        for section_def in section_defs {
            sections.push(collector::collect_section_with_semantic_search(
                graph,
                root,
                section_def,
                temporal_filter,
                provider,
            )?);
        }
    } else {
        for section_def in section_defs {
            sections.push(collector::collect_section(
                graph,
                root,
                section_def,
                temporal_filter,
                None,
            )?);
        }
    }
    Ok(sections)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use tempyr_core::graph::Graph;
    use tempyr_core::node::parse_node;
    use tempyr_core::schema::Schema;

    fn make_schema() -> Schema {
        let schema_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("schema/default-schema.toml");
        Schema::load(&schema_path).unwrap()
    }

    fn build_full_graph() -> Graph {
        let mut graph = Graph::new(make_schema());

        let feat = r#"---
id: feat-replay
type: feature
status: draft
owner: alice
edges:
  - target: decision-storage
    type: depends_on
  - target: persona-eng
    type: serves
  - target: task-ingestion
    type: decomposes_to
---
# Session Replay

## Problem

Engineers need to see what happened during a session.

## Solution

A recording agent captures DOM snapshots.
"#;
        let persona = "---\nid: persona-eng\ntype: persona\nedges:\n  - target: feat-replay\n    type: served_by\n---\n# Platform Engineer\n\nDebug funnel issues.\n";
        let decision = "---\nid: decision-storage\ntype: decision\nstatus: decided\nedges:\n  - target: feat-replay\n    type: decision_for\n---\n# Storage Backend\n\nUse ClickHouse.\n";
        let task = "---\nid: task-ingestion\ntype: task\nstatus: backlog\nedges:\n  - target: feat-replay\n    type: child_of\n---\n# Build Ingestion\n\nImplement pipeline.\n";

        graph.add_node(parse_node(feat, PathBuf::from("f.md")).unwrap());
        graph.add_node(parse_node(persona, PathBuf::from("p.md")).unwrap());
        graph.add_node(parse_node(decision, PathBuf::from("d.md")).unwrap());
        graph.add_node(parse_node(task, PathBuf::from("t.md")).unwrap());

        graph
    }

    struct FakeSemanticSearch;

    impl SemanticSearchProvider for FakeSemanticSearch {
        fn search(&mut self, request: &SemanticSearchRequest) -> Result<Vec<SemanticSearchHit>> {
            assert!(request.query.contains("Session Replay"));
            assert_eq!(request.target_type.as_deref(), Some("decision"));
            Ok(vec![SemanticSearchHit {
                node_id: "decision-storage".to_string(),
                score: 0.95,
            }])
        }
    }

    #[test]
    fn test_render_prd_integration() {
        let graph = build_full_graph();
        let template_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("templates/prd.toml");

        let result = render(
            &graph,
            &template_path,
            "feat-replay",
            &TemporalFilter::current(),
        )
        .unwrap();

        assert!(result.contains("Product Requirements Document: Session Replay"));
        assert!(result.contains("## Overview"));
        assert!(result.contains("## Target Users"));
        assert!(result.contains("### Platform Engineer"));
        assert!(result.contains("## Key Decisions"));
        assert!(result.contains("### Storage Backend"));
        assert!(result.contains("## Task Decomposition"));
    }

    #[test]
    fn test_render_wrong_root_type() {
        let graph = build_full_graph();
        let template_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("templates/prd.toml");

        // Try to render a PRD from a persona (should fail)
        let result = render(
            &graph,
            &template_path,
            "persona-eng",
            &TemporalFilter::current(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_render_from_str() {
        let graph = build_full_graph();
        let template_toml = r#"
[meta]
name = "Simple Doc"
root_types = ["feature"]
output_format = "markdown"

[[sections]]
heading = "Overview"
source = "root"
"#;
        let result = render_from_str(
            &graph,
            template_toml,
            "feat-replay",
            &TemporalFilter::current(),
        )
        .unwrap();
        assert!(result.contains("Simple Doc: Session Replay"));
        assert!(result.contains("## Overview"));
    }

    #[test]
    fn test_render_from_str_with_semantic_provider() {
        let graph = build_full_graph();
        let template_toml = r#"
[meta]
name = "Semantic Doc"
root_types = ["feature"]
output_format = "markdown"

[[sections]]
heading = "Related Decisions"
source = "semantic_search"
query_from = "root"
target_type = "decision"
max_results = 3
min_similarity = 0.7
include_body = true
"#;
        let mut provider = FakeSemanticSearch;

        let result = render_from_str_with_options(
            &graph,
            template_toml,
            "feat-replay",
            &TemporalFilter::current(),
            RenderOptions {
                semantic_search: Some(&mut provider),
            },
        )
        .unwrap();

        assert!(result.contains("## Related Decisions"));
        assert!(result.contains("### Storage Backend"));
        assert!(result.contains("Use ClickHouse."));
    }
}
