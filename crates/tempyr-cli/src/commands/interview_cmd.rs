use crate::config::ProjectContext;
use std::path::Path;
use tempyr_core::graph::Graph;
use tempyr_interview::phases;
use tempyr_interview::proposer;
use tempyr_interview::session::InterviewSession;
use tempyr_journal::{InterviewEvent, JournalError, auto_emit_interview_event, path as jpath};

pub fn run_start(
    ctx: &ProjectContext,
    brain_dump: &str,
    root_type: &str,
    agent: &str,
    json: bool,
) -> anyhow::Result<()> {
    let sessions_dir = ctx.tempyr_dir.join("sessions");

    // Get existing node IDs for context
    let graph = Graph::load_from_directory(&ctx.graph_dir, ctx.schema.clone())?;
    let existing_ids: Vec<String> = graph.nodes.keys().cloned().collect();

    let existing_suffixes = tempyr_core::id::collect_existing_suffixes(&ctx.graph_dir);
    let result = proposer::interview_start(
        brain_dump,
        root_type,
        &ctx.schema,
        &existing_ids,
        &existing_suffixes,
    )?;

    let session = result.session;
    session.save(&sessions_dir)?;

    // Phase 4b: best-effort journal entry on interview lifecycle.
    emit_interview_event(
        &ctx.root,
        agent,
        &InterviewEvent::Started {
            session_id: &session.id,
            root_node_id: &session.root_node.id,
            root_type: &session.root_type,
            phase: session.phase.display_name(),
        },
    );

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "session_id": session.id,
                "root_id": session.root_node.id,
                "phase": format!("{:?}", session.phase),
                "progress": result.progress,
                "questions": result.questions.iter().map(|q| serde_json::json!({
                    "type": format!("{:?}", q.gap_type),
                    "priority": format!("{}", q.priority),
                    "question": q.suggested_question,
                })).collect::<Vec<_>>(),
            }))?
        );
    } else {
        println!("Interview started: session {}", session.id);
        println!(
            "Root node: {} ({})",
            session.root_node.id, session.root_type
        );
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

pub fn run_answer(
    ctx: &ProjectContext,
    session_id: &str,
    answer: &str,
    agent: &str,
    json: bool,
) -> anyhow::Result<()> {
    let sessions_dir = ctx.tempyr_dir.join("sessions");
    let mut session = InterviewSession::load_by_id(&sessions_dir, session_id)?;

    let prior_phase = session.phase;
    let question = session
        .remaining_gaps
        .first()
        .map(|g| g.suggested_question.clone())
        .unwrap_or_else(|| "General question".to_string());

    let result = proposer::record_answer(&mut session, &question, answer, vec![], &ctx.schema);

    session.save(&sessions_dir)?;

    // Phase 4b: emit AnswerRecorded; if the reanalysis advanced the
    // phase, also emit PhaseAdvanced. Both best-effort.
    emit_interview_event(
        &ctx.root,
        agent,
        &InterviewEvent::AnswerRecorded {
            session_id: &session.id,
            answer,
            phase: session.phase.display_name(),
            filled_gap_count: result.filled_gaps.len(),
        },
    );
    if result.phase_changed {
        emit_interview_event(
            &ctx.root,
            agent,
            &InterviewEvent::PhaseAdvanced {
                session_id: &session.id,
                from: prior_phase.display_name(),
                to: session.phase.display_name(),
            },
        );
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "session_id": session.id,
                "phase": format!("{:?}", session.phase),
                "phase_changed": result.phase_changed,
                "filled_gaps": result.filled_gaps,
                "progress": result.progress,
                "questions": result.questions.iter().map(|q| serde_json::json!({
                    "type": format!("{:?}", q.gap_type),
                    "question": q.suggested_question,
                })).collect::<Vec<_>>(),
            }))?
        );
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
    println!(
        "Phase: {} ({}/5)",
        session.phase.display_name(),
        session.phase.index() + 1
    );
    println!("{}", phases::progress_summary(&session));
    println!();

    println!(
        "Root node: {} ({})",
        session.root_node.id, session.root_node.node_type
    );
    println!("  Status: {}", session.root_node.status);
    println!("  Confidence: {:.0}%", session.root_node.confidence * 100.0);
    println!();

    if !session.tentative_nodes.is_empty() {
        println!("Tentative nodes:");
        for node in &session.tentative_nodes {
            println!(
                "  {} ({}) - confidence {:.0}%",
                node.id,
                node.node_type,
                node.confidence * 100.0
            );
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
            println!(
                "  [{}] {}: {}",
                gap.priority,
                gap.phase.display_name(),
                gap.suggested_question
            );
        }
    }

    Ok(())
}

pub fn run_commit(ctx: &ProjectContext, session_id: &str, agent: &str) -> anyhow::Result<()> {
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
    super::warn_if_index_refresh_fails(ctx);

    // Phase 4b: emit Committed (final outcome) so the journal session
    // gets finalized and picked up by the publisher.
    emit_interview_event(
        &ctx.root,
        agent,
        &InterviewEvent::Committed {
            session_id: &session.id,
            node_count: result.node_count,
            edge_count: result.edge_count,
            files_created: result.created_files.len(),
        },
    );

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
        println!(
            "  {} | {} ({}) | {} | {} nodes | {}",
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

/// Best-effort wrapper around [`auto_emit_interview_event`]. Anchors
/// on the resolved project root (NOT shell cwd, which can point at a
/// different repo when `--graph-dir` is passed). Failures are
/// reported on stderr but never propagate — the underlying interview
/// operation has already mutated state on disk.
///
/// Error policy:
/// - [`JournalError::NotAGitRepo`] is swallowed silently. Tempyr
///   supports operating outside a git repo; "no journal" is the
///   expected fallthrough, not an error worth logging.
/// - Anything else (IO, git binary missing, redaction block, lock
///   contention, etc.) is logged to stderr with context so a real
///   bug isn't invisible.
fn emit_interview_event(project_root: &Path, agent: &str, event: &InterviewEvent<'_>) {
    let common_dir = match jpath::git_common_dir(project_root) {
        Ok(c) => c,
        Err(JournalError::NotAGitRepo(_)) => return,
        Err(e) => {
            eprintln!("warning: journal auto-emit skipped, git_common_dir failed: {e}");
            return;
        }
    };
    let worktree_top = match jpath::repo_toplevel(project_root) {
        Ok(w) => w,
        Err(JournalError::NotAGitRepo(_)) => return,
        Err(e) => {
            eprintln!("warning: journal auto-emit skipped, repo_toplevel failed: {e}");
            return;
        }
    };
    if let Err(e) = auto_emit_interview_event(&common_dir, &worktree_top, agent, event) {
        eprintln!("warning: journal auto-emit for interview event failed: {e}");
    }
}
