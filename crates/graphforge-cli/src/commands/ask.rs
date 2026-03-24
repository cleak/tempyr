use crate::config::ProjectContext;
use graphforge_core::graph::Graph;
use graphforge_index::hybrid::{hybrid_retrieve, RetrievalConfig};
use graphforge_index::indexer::Index;

pub fn run(
    ctx: &ProjectContext,
    question: &str,
    root: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let index_path = ctx.index_path();
    if !index_path.exists() {
        anyhow::bail!("Index not found. Run `graphforge index rebuild` first.");
    }

    let graph = Graph::load_from_directory(&ctx.graph_dir, ctx.schema.clone())?;
    let index = Index::open(&index_path)?;
    let config = RetrievalConfig {
        token_budget: 8000,
        ..RetrievalConfig::standard()
    };

    let results = hybrid_retrieve(&index, &graph, question, root, &config)?;

    if results.is_empty() {
        println!("No relevant context found for: {question}");
        return Ok(());
    }

    if json {
        let mut context_nodes = Vec::new();
        for r in &results {
            if let Some(node) = graph.get_node(&r.node_id) {
                context_nodes.push(serde_json::json!({
                    "node_id": r.node_id,
                    "type": node.node_type(),
                    "title": node.title(),
                    "score": r.combined_score,
                    "body": node.body.trim(),
                }));
            }
        }
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
            "question": question,
            "context_nodes": context_nodes,
            "note": "Use the MCP server graph_ask tool for LLM-generated answers. This output provides the retrieved context."
        }))?);
        return Ok(());
    }

    // Without LLM, show the retrieved context that would be used to answer
    println!("Question: {question}\n");
    println!("Relevant context ({} nodes):\n", results.len());

    for r in &results {
        if let Some(node) = graph.get_node(&r.node_id) {
            println!("--- {} ({}) [score: {:.3}] ---", node.title(), node.node_type(), r.combined_score);
            // Show first few lines of body
            let preview: String = node.body.lines().take(5).collect::<Vec<_>>().join("\n");
            println!("{preview}");
            println!();
        }
    }

    println!("Note: For a synthesized answer, use the MCP server's graph_ask tool with Claude Code.");

    Ok(())
}
