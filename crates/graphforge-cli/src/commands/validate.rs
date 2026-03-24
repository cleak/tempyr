use crate::config::ProjectContext;
use graphforge_core::graph::Graph;
use graphforge_core::validate::{validate_graph, Severity};

pub fn run(ctx: &ProjectContext, json: bool) -> anyhow::Result<()> {
    let graph = Graph::load_from_directory(&ctx.graph_dir, ctx.schema.clone())?;
    let issues = validate_graph(&graph);

    if json {
        let json_issues: Vec<_> = issues
            .iter()
            .map(|i| {
                serde_json::json!({
                    "severity": format!("{:?}", i.severity),
                    "kind": format!("{:?}", i.kind),
                    "node_id": i.node_id,
                    "message": i.message,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json_issues)?);
        return Ok(());
    }

    if issues.is_empty() {
        println!("Graph is valid. {} nodes, {} edges.",
            graph.node_count(), graph.edge_count());
        return Ok(());
    }

    let errors = issues.iter().filter(|i| i.severity == Severity::Error).count();
    let warnings = issues.iter().filter(|i| i.severity == Severity::Warning).count();

    for issue in &issues {
        eprintln!("{issue}");
    }

    eprintln!("\n{errors} error(s), {warnings} warning(s)");

    if errors > 0 {
        std::process::exit(1);
    }

    Ok(())
}
