use std::path::Path;

use chrono::NaiveDate;

use crate::config::ProjectContext;
use tempyr_core::graph::Graph;
use tempyr_core::temporal::TemporalFilter;

const BUILTIN_PRD: &str = include_str!("../../../../templates/prd.toml");
const BUILTIN_TDD: &str = include_str!("../../../../templates/tdd.toml");

pub fn run(
    ctx: &ProjectContext,
    template_name: &str,
    root_id: &str,
    as_of: Option<&str>,
    include_history: bool,
    output: Option<&Path>,
) -> anyhow::Result<()> {
    let graph = Graph::load_from_directory(&ctx.graph_dir, ctx.schema.clone())?;

    let temporal_filter = if include_history {
        TemporalFilter::with_history()
    } else if let Some(date_str) = as_of {
        let date = NaiveDate::parse_from_str(date_str, "%Y-%m-%d")
            .map_err(|e| anyhow::anyhow!("Invalid date format: {e}. Use YYYY-MM-DD."))?;
        TemporalFilter::as_of(date)
    } else {
        TemporalFilter::current()
    };

    // Try project-local template first, then built-in
    let local_path = ctx.tempyr_dir.join("render").join(format!("{template_name}.toml"));
    let result = if local_path.exists() {
        tempyr_render::render(&graph, &local_path, root_id, &temporal_filter)?
    } else {
        // Use built-in template
        let template_toml = match template_name {
            "prd" => BUILTIN_PRD,
            "tdd" => BUILTIN_TDD,
            _ => anyhow::bail!(
                "Unknown template: '{template_name}'. Available: prd, tdd (or place a custom template in .tempyr/render/)"
            ),
        };
        tempyr_render::render_from_str(&graph, template_toml, root_id, &temporal_filter)?
    };

    if let Some(output_path) = output {
        std::fs::write(output_path, &result)?;
        println!("Rendered to {}", output_path.display());
    } else {
        print!("{result}");
    }

    Ok(())
}
