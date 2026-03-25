use crate::config::ProjectContext;
use tempyr_core::graph::Graph;
use tempyr_interview::phases;
use tempyr_interview::proposer;
use tempyr_interview::session::InterviewSession;

pub fn run_start(ctx: &ProjectContext, brain_dump: &str, root_type: &str, json: bool) -> anyhow::Result<()> {
    let sessions_dir = ctx.tempyr_dir.join("sessions");

    // Get existing node IDs for context
    let graph = Graph::load_from_directory(&ctx.graph_dir, ctx.schema.clone())?;
    let existing_ids: Vec<String> = graph.nodes.keys().cloned().collect();

    let result = proposer::interview_start(brain_dump, root_type, &ctx.schema, &existing_ids)?;

    let session = result.session;
    session.save(&sessions_dir)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
            "session_id": session.id,
            "root_id": session.root_node.id,
            "phase": format!("{:?}", session.phase),
            "progress": result.progress,
            "questions": result.questions.iter().map(|q| serde_json::json!({
                "type": format!("{:?}", q.gap_type),
                "priority": format!("{}", q.priority),
                "question": q.suggested_question,
            })).collect::<Vec<_>>(),
        }))?);
    } else {
        println!("Interview started: session {}", session.id);
        println!("Root node: {} ({})", session.root_node.id, session.root_type);
        println!("{}", result.progress);
        println!();
        if !result.questions.is_empty() {
            println!("Next questions:");
            for (i, q) in result.questions.iter().enumerate() {
                println!("  {}. [{}] {}", i + 1, q.priority, q.suggested_question);
            }
        }
    }

    Ok(())
}

pub fn run_answer(ctx: &ProjectContext, session_id: &str, answer: &str, json: bool) -> anyhow::Result<()> {
    let sessions_dir = ctx.tempyr_dir.join("sessions");
    let mut session = InterviewSession::load_by_id(&sessions_dir, session_id)?;

    let question = session.remaining_gaps
        .first()
        .map(|g| g.suggested_question.clone())
        .unwrap_or_else(|| "General question".to_string());

    let result = proposer::record_answer(&mut session, &question, answer, vec![], &ctx.schema);

    session.save(&sessions_dir)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
            "session_id": session.id,
            "phase": format!("{:?}", session.phase),
            "phase_changed": result.phase_changed,
            "filled_gaps": result.filled_gaps,
            "progress": result.progress,
            "questions": result.questions.iter().map(|q| serde_json::json!({
                "type": format!("{:?}", q.gap_type),
                "question": q.suggested_question,
            })).collect::<Vec<_>>(),
        }))?);
    } else {
        println!("Answer recorded.");
        if result.phase_changed {
            println!("Phase advanced to: {}", session.phase.display_name());
        }
        println!("{}", result.progress);
        if !result.filled_gaps.is_empty() {
            println!("Gaps filled: {}", result.filled_gaps.join(", "));
        }
        println!();
        if !result.questions.is_empty() {
            println!("Next questions:");
            for (i, q) in result.questions.iter().enumerate() {
                println!("  {}. [{}] {}", i + 1, q.priority, q.suggested_question);
            }
        }
    }

    Ok(())
}

pub fn run_show(ctx: &ProjectContext, session_id: &str, json: bool) -> anyhow::Result<()> {
    let sessions_dir = ctx.tempyr_dir.join("sessions");
    let session = InterviewSession::load_by_id(&sessions_dir, session_id)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&session)?);
        return Ok(());
    }

    println!("Session: {}", session.id);
    println!("Phase: {} ({}/5)", session.phase.display_name(), session.phase.index() + 1);
    println!("{}", phases::progress_summary(&session));
    println!();

    println!("Root node: {} ({})", session.root_node.id, session.root_node.node_type);
    println!("  Status: {}", session.root_node.status);
    println!("  Confidence: {:.0}%", session.root_node.confidence * 100.0);
    println!();

    if !session.tentative_nodes.is_empty() {
        println!("Tentative nodes:");
        for node in &session.tentative_nodes {
            println!("  {} ({}) - confidence {:.0}%",
                node.id, node.node_type, node.confidence * 100.0);
        }
        println!();
    }

    if !session.tentative_edges.is_empty() {
        println!("Tentative edges:");
        for edge in &session.tentative_edges {
            println!("  {} --{}--> {}", edge.source, edge.edge_type, edge.target);
        }
        println!();
    }

    if !session.remaining_gaps.is_empty() {
        println!("Remaining gaps ({}):", session.remaining_gaps.len());
        for gap in &session.remaining_gaps {
            println!("  [{}] {}: {}", gap.priority, gap.phase.display_name(), gap.suggested_question);
        }
    }

    Ok(())
}

pub fn run_commit(ctx: &ProjectContext, session_id: &str) -> anyhow::Result<()> {
    let sessions_dir = ctx.tempyr_dir.join("sessions");
    let session = InterviewSession::load_by_id(&sessions_dir, session_id)?;

    let result = session.commit(&ctx.graph_dir, &ctx.schema, &sessions_dir)?;

    println!("Committed session {}", session_id);
    println!("Created {} file(s):", result.created_files.len());
    for path in &result.created_files {
        println!("  {}", path.display());
    }
    if !result.modified_files.is_empty() {
        println!("Modified {} file(s):", result.modified_files.len());
        for path in &result.modified_files {
            println!("  {}", path.display());
        }
    }

    Ok(())
}

pub fn run_list(ctx: &ProjectContext, json: bool) -> anyhow::Result<()> {
    let sessions_dir = ctx.tempyr_dir.join("sessions");
    let sessions = InterviewSession::list_sessions(&sessions_dir)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&sessions)?);
        return Ok(());
    }

    if sessions.is_empty() {
        println!("No active interview sessions.");
        return Ok(());
    }

    println!("Active sessions:");
    for s in &sessions {
        println!("  {} | {} ({}) | {} | {} nodes | {}",
            &s.id[..8],
            s.root_id,
            s.root_type,
            s.phase.display_name(),
            s.node_count,
            s.updated_at.format("%Y-%m-%d %H:%M"),
        );
    }

    Ok(())
}
