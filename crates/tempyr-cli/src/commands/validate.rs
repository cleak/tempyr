use crate::config::ProjectContext;
use tempyr_core::graph::Graph;
use tempyr_core::ops;
use tempyr_core::validate::{validate_graph, Severity, ValidationKind};

pub fn run(ctx: &ProjectContext, json: bool, fix: bool) -> anyhow::Result<()> {
    if fix {
        return run_fix(ctx);
    }

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

    let reverse_errors = issues.iter()
        .filter(|i| i.kind == ValidationKind::MissingReverseEdge)
        .count();

    for issue in &issues {
        eprintln!("{issue}");
    }

    eprintln!("\n{errors} error(s), {warnings} warning(s)");

    if reverse_errors > 0 {
        eprintln!("\nTip: run `tempyr validate --fix` to add {reverse_errors} missing reverse edge(s)");
    }

    if errors > 0 {
        std::process::exit(1);
    }

    Ok(())
}

fn run_fix(ctx: &ProjectContext) -> anyhow::Result<()> {
    let repairs = ops::repair_reverse_edges(&ctx.graph_dir, &ctx.schema)?;

    if repairs.is_empty() {
        println!("No missing reverse edges found.");
        return Ok(());
    }

    for (target_id, source_id, reverse_type) in &repairs {
        println!("  Added: {target_id} --{reverse_type}--> {source_id}");
    }

    println!("\nRepaired {} missing reverse edge(s).", repairs.len());

    // Re-validate to show remaining issues
    let graph = Graph::load_from_directory(&ctx.graph_dir, ctx.schema.clone())?;
    let issues = validate_graph(&graph);
    let errors = issues.iter().filter(|i| i.severity == Severity::Error).count();

    if errors == 0 {
        println!("Graph is now valid. {} nodes, {} edges.",
            graph.node_count(), graph.edge_count());
    } else {
        eprintln!("\n{errors} error(s) remain after repair:");
        for issue in issues.iter().filter(|i| i.severity == Severity::Error) {
            eprintln!("  {issue}");
        }
    }

    Ok(())
}
