use crate::config::ProjectContext;
use tempyr_core::graph::Graph;
use tempyr_core::traverse::bfs;

pub fn run(
    ctx: &ProjectContext,
    id: &str,
    depth: usize,
    edge_type: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let graph = Graph::load_from_directory(&ctx.graph_dir, ctx.schema.clone())?;
    let results = bfs(&graph, id, depth, edge_type);

    if results.is_empty() {
        anyhow::bail!("Node not found: {id}");
    }

    if json {
        let json_results: Vec<_> = results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "node_id": r.node_id,
                    "depth": r.depth,
                    "path": r.path,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json_results)?);
        return Ok(());
    }

    for result in &results {
        let indent = "  ".repeat(result.depth);
        let node = graph.get_node(&result.node_id);
        let type_str = node.map(|n| n.node_type()).unwrap_or("?");
        let title = node.map(|n| n.title()).unwrap_or("?");
        println!("{indent}{} ({type_str}) - {title}", result.node_id);
    }

    Ok(())
}
