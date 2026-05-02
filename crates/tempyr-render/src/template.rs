use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;

use serde::Deserialize;

use crate::RenderError;

/// A rendering template loaded from a TOML file.
#[derive(Debug, Clone, Deserialize)]
pub struct RenderTemplate {
    pub meta: TemplateMeta,
    pub sections: Vec<SectionDef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TemplateMeta {
    pub name: String,
    pub description: Option<String>,
    pub root_types: Vec<String>,
    pub output_format: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SectionDef {
    pub heading: String,
    pub source: Option<String>,
    pub body_section: Option<String>,
    pub traverse: Option<String>,
    pub target_type: Option<String>,
    pub include_body: Option<bool>,
    pub include_fields: Option<Vec<String>>,
    pub filter: Option<HashMap<String, Vec<String>>>,
    pub sub_traverse: Option<String>,
    pub sub_target_type: Option<String>,
    pub show_internal_edges: Option<bool>,
    pub internal_edge_types: Option<Vec<String>>,
    pub max_results: Option<usize>,
    pub min_similarity: Option<f64>,
    pub query_from: Option<String>,
}

impl RenderTemplate {
    /// Load a template from a TOML file.
    pub fn load(path: &Path) -> crate::Result<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| RenderError::Template(format!("Cannot read template: {e}")))?;
        content.parse()
    }
}

impl FromStr for RenderTemplate {
    type Err = RenderError;

    fn from_str(content: &str) -> crate::Result<Self> {
        toml::from_str(content)
            .map_err(|e| RenderError::Template(format!("Invalid template TOML: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn templates_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("templates")
    }

    #[test]
    fn test_template_load_prd() {
        let template = RenderTemplate::load(&templates_dir().join("prd.toml")).unwrap();
        assert_eq!(template.meta.name, "Product Requirements Document");
        assert_eq!(template.meta.root_types, vec!["feature"]);
        assert!(!template.sections.is_empty());

        // Verify known sections exist
        let headings: Vec<_> = template
            .sections
            .iter()
            .map(|s| s.heading.as_str())
            .collect();
        assert!(headings.contains(&"Overview"));
        assert!(headings.contains(&"Target Users"));
        assert!(headings.contains(&"Success Metrics"));
        assert!(headings.contains(&"Task Decomposition"));
    }

    #[test]
    fn test_template_load_task_prompt() {
        let template = RenderTemplate::load(&templates_dir().join("task-prompt.toml")).unwrap();
        assert_eq!(template.meta.name, "Task Prompt");
        assert_eq!(template.meta.root_types, vec!["task"]);
        assert!(!template.sections.is_empty());

        let headings: Vec<_> = template
            .sections
            .iter()
            .map(|s| s.heading.as_str())
            .collect();
        assert!(headings.contains(&"Objective"));
        assert!(headings.contains(&"Feature Context"));
        assert!(headings.contains(&"Blocked By"));
        assert!(headings.contains(&"Relevant Decisions"));
        assert!(headings.contains(&"Open Questions"));
        assert!(headings.contains(&"Related Tasks"));
    }

    #[test]
    fn test_template_load_tdd() {
        let template = RenderTemplate::load(&templates_dir().join("tdd.toml")).unwrap();
        assert_eq!(template.meta.name, "Technical Design Document");

        let headings: Vec<_> = template
            .sections
            .iter()
            .map(|s| s.heading.as_str())
            .collect();
        assert!(headings.contains(&"Architecture Decisions"));
        assert!(headings.contains(&"System Components"));
        assert!(headings.contains(&"Relevant Insights"));
    }

    #[test]
    fn test_template_section_details() {
        let template = RenderTemplate::load(&templates_dir().join("prd.toml")).unwrap();

        // Check the "Key Decisions" section has a status filter
        let decisions = template
            .sections
            .iter()
            .find(|s| s.heading == "Key Decisions")
            .unwrap();
        assert_eq!(decisions.traverse.as_deref(), Some("depends_on"));
        assert_eq!(decisions.target_type.as_deref(), Some("decision"));
        let filter = decisions.filter.as_ref().unwrap();
        assert!(
            filter
                .get("status")
                .unwrap()
                .contains(&"decided".to_string())
        );
    }

    #[test]
    fn test_template_semantic_search_section() {
        let template = RenderTemplate::load(&templates_dir().join("tdd.toml")).unwrap();

        let insights = template
            .sections
            .iter()
            .find(|s| s.heading == "Relevant Insights")
            .unwrap();
        assert_eq!(insights.source.as_deref(), Some("semantic_search"));
        assert_eq!(insights.query_from.as_deref(), Some("root"));
        assert_eq!(insights.max_results, Some(5));
        assert_eq!(insights.include_body, Some(true));
    }
}
