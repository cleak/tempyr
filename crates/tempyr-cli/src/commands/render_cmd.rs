use std::path::Path;

use chrono::NaiveDate;

use crate::commands::semantic::SemanticSearchRuntime;
use crate::config::ProjectContext;
use tempyr_core::graph::Graph;
use tempyr_core::temporal::TemporalFilter;

const BUILTIN_PRD: &str = include_str!("../../../../templates/prd.toml");
const BUILTIN_TDD: &str = include_str!("../../../../templates/tdd.toml");
const BUILTIN_TASK_PROMPT: &str = include_str!("../../../../templates/task-prompt.toml");

struct RenderSemanticSearch<'a> {
    ctx: &'a ProjectContext,
    graph: &'a Graph,
    runtime: Option<SemanticSearchRuntime>,
}

impl<'a> RenderSemanticSearch<'a> {
    fn new(ctx: &'a ProjectContext, graph: &'a Graph) -> Self {
        Self {
            ctx,
            graph,
            runtime: None,
        }
    }

    fn runtime(&mut self) -> tempyr_render::Result<&mut SemanticSearchRuntime> {
        if self.runtime.is_none() {
            let runtime = SemanticSearchRuntime::new(self.ctx).map_err(render_error)?;
            self.runtime = Some(runtime);
        }
        Ok(self.runtime.as_mut().expect("semantic runtime initialized"))
    }
}

impl tempyr_render::SemanticSearchProvider for RenderSemanticSearch<'_> {
    fn search(
        &mut self,
        request: &tempyr_render::SemanticSearchRequest,
    ) -> tempyr_render::Result<Vec<tempyr_render::SemanticSearchHit>> {
        let graph = self.graph;
        let results = self
            .runtime()?
            .vector_search(
                graph,
                &request.query,
                request.max_results,
                request.target_type.as_deref(),
                request.min_similarity,
            )
            .map_err(render_error)?;

        Ok(results
            .into_iter()
            .map(|result| tempyr_render::SemanticSearchHit {
                node_id: result.node_id,
                score: result.similarity,
            })
            .collect())
    }
}

fn render_error(err: anyhow::Error) -> tempyr_render::RenderError {
    tempyr_render::RenderError::General(err.to_string())
}

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
    let local_path = ctx
        .tempyr_dir
        .join("render")
        .join(format!("{template_name}.toml"));
    let mut semantic_search = RenderSemanticSearch::new(ctx, &graph);
    let result = if local_path.exists() {
        tempyr_render::render_with_options(
            &graph,
            &local_path,
            root_id,
            &temporal_filter,
            tempyr_render::RenderOptions {
                semantic_search: Some(&mut semantic_search),
            },
        )?
    } else {
        // Use built-in template
        let template_toml = match template_name {
            "prd" => BUILTIN_PRD,
            "tdd" => BUILTIN_TDD,
            "task-prompt" => BUILTIN_TASK_PROMPT,
            _ => anyhow::bail!(
                "Unknown template: '{template_name}'. Available: prd, tdd, task-prompt (or place a custom template in .tempyr/render/)"
            ),
        };
        tempyr_render::render_from_str_with_options(
            &graph,
            template_toml,
            root_id,
            &temporal_filter,
            tempyr_render::RenderOptions {
                semantic_search: Some(&mut semantic_search),
            },
        )?
    };

    if let Some(output_path) = output {
        if let Some(parent) = output_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(output_path, &result)?;
        println!("Rendered to {}", output_path.display());
    } else {
        print!("{result}");
    }

    Ok(())
}
