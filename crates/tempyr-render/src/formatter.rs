use crate::collector::SectionData;
use crate::template::RenderTemplate;

use tempyr_core::node::Node;

/// Render collected sections into a markdown document.
pub fn render_to_markdown(
    template: &RenderTemplate,
    root: &Node,
    sections: Vec<SectionData>,
) -> String {
    let mut output = String::new();

    // Document title
    output.push_str(&format!("# {}: {}\n\n", template.meta.name, root.title()));

    for section in sections {
        if section.items.is_empty() && !section.is_root_section {
            continue; // Skip empty sections
        }

        output.push_str(&format!("## {}\n\n", section.heading));

        for item in &section.items {
            if section.is_root_section {
                // Root sections: render fields then body directly
                render_fields(&mut output, &item.fields);
                if let Some(body) = &item.body {
                    output.push_str(body.trim());
                    output.push_str("\n\n");
                }
            } else {
                // Traversed nodes: render as subsections
                output.push_str(&format!("### {}\n\n", item.title));
                render_fields(&mut output, &item.fields);
                if let Some(body) = &item.body {
                    output.push_str(body.trim());
                    output.push_str("\n\n");
                }

                // Internal edges (e.g., blocked_by)
                for (_from, to, edge_type) in &item.internal_edges {
                    output.push_str(&format!("- *{edge_type}*: {to}\n"));
                }
                if !item.internal_edges.is_empty() {
                    output.push('\n');
                }

                // Sub-items (second hop)
                for sub in &item.sub_items {
                    output.push_str(&format!("#### {}\n\n", sub.title));
                    if let Some(body) = &sub.body {
                        output.push_str(body.trim());
                        output.push_str("\n\n");
                    }
                }
            }
        }
    }

    output.trim_end().to_string() + "\n"
}

fn render_fields(output: &mut String, fields: &[(String, String)]) {
    if fields.is_empty() {
        return;
    }
    for (key, value) in fields {
        output.push_str(&format!("**{key}**: {value}  \n"));
    }
    output.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collector::{SectionData, SectionItem};
    use crate::template::{RenderTemplate, TemplateMeta, SectionDef};
    use tempyr_core::node::parse_node;
    use std::path::PathBuf;

    fn make_root() -> Node {
        let content = "---\nid: feat-test\ntype: feature\nstatus: draft\nowner: caleb\n---\n# Test Feature\n\nA test feature body.\n";
        parse_node(content, PathBuf::from("test.md")).unwrap()
    }

    fn make_template() -> RenderTemplate {
        RenderTemplate {
            meta: TemplateMeta {
                name: "Test Document".to_string(),
                description: None,
                root_types: vec!["feature".to_string()],
                output_format: "markdown".to_string(),
            },
            sections: Vec::new(),
        }
    }

    #[test]
    fn test_format_basic() {
        let root = make_root();
        let template = make_template();

        let sections = vec![SectionData {
            heading: "Overview".to_string(),
            items: vec![SectionItem {
                node_id: "feat-test".to_string(),
                title: "Test Feature".to_string(),
                node_type: "feature".to_string(),
                fields: vec![("status".to_string(), "draft".to_string())],
                body: Some("A test feature body.".to_string()),
                sub_items: Vec::new(),
                internal_edges: Vec::new(),
            }],
            is_root_section: true,
        }];

        let md = render_to_markdown(&template, &root, sections);
        assert!(md.starts_with("# Test Document: Test Feature\n"));
        assert!(md.contains("## Overview"));
        assert!(md.contains("**status**: draft"));
        assert!(md.contains("A test feature body."));
    }

    #[test]
    fn test_format_skips_empty_sections() {
        let root = make_root();
        let template = make_template();

        let sections = vec![
            SectionData {
                heading: "Has Content".to_string(),
                items: vec![SectionItem {
                    node_id: "x".to_string(),
                    title: "X".to_string(),
                    node_type: "persona".to_string(),
                    fields: Vec::new(),
                    body: Some("Content".to_string()),
                    sub_items: Vec::new(),
                    internal_edges: Vec::new(),
                }],
                is_root_section: false,
            },
            SectionData {
                heading: "Empty Section".to_string(),
                items: Vec::new(),
                is_root_section: false,
            },
        ];

        let md = render_to_markdown(&template, &root, sections);
        assert!(md.contains("## Has Content"));
        assert!(!md.contains("## Empty Section"));
    }

    #[test]
    fn test_format_traversed_nodes() {
        let root = make_root();
        let template = make_template();

        let sections = vec![SectionData {
            heading: "Personas".to_string(),
            items: vec![SectionItem {
                node_id: "persona-eng".to_string(),
                title: "Platform Engineer".to_string(),
                node_type: "persona".to_string(),
                fields: Vec::new(),
                body: Some("Debugs funnel issues.".to_string()),
                sub_items: Vec::new(),
                internal_edges: Vec::new(),
            }],
            is_root_section: false,
        }];

        let md = render_to_markdown(&template, &root, sections);
        assert!(md.contains("### Platform Engineer"));
        assert!(md.contains("Debugs funnel issues."));
    }
}
