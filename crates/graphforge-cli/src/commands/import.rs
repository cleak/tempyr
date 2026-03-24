use std::path::Path;

use crate::config::ProjectContext;
use graphforge_interview::proposer;
use graphforge_core::graph::Graph;

pub fn run(ctx: &ProjectContext, file: &Path) -> anyhow::Result<()> {
    if !file.exists() {
        anyhow::bail!("File not found: {}", file.display());
    }

    let content = std::fs::read_to_string(file)?;
    if content.trim().is_empty() {
        anyhow::bail!("File is empty: {}", file.display());
    }

    // Get existing node IDs for context
    let graph = Graph::load_from_directory(&ctx.graph_dir, ctx.schema.clone())?;
    let existing_ids: Vec<String> = graph.nodes.keys().cloned().collect();

    // Start an interview session from the imported text
    let result = proposer::interview_start(&content, "feature", &ctx.schema, &existing_ids)?;

    let sessions_dir = ctx.graphforge_dir.join("sessions");
    let session = result.session;
    session.save(&sessions_dir)?;

    println!("Imported text from: {}", file.display());
    println!("Created interview session: {}", session.id);
    println!("Root node: {} ({})", session.root_node.id, session.root_type);
    println!("{}", result.progress);
    println!();
    println!("The import created an interview session. Use the interview commands to");
    println!("refine the extracted nodes before committing:");
    println!("  graphforge interview show {}", session.id);
    println!("  graphforge interview commit {}", session.id);

    if !result.questions.is_empty() {
        println!();
        println!("Gaps to address:");
        for (i, q) in result.questions.iter().enumerate() {
            println!("  {}. [{}] {}", i + 1, q.priority, q.suggested_question);
        }
    }

    Ok(())
}
