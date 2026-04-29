use crate::config::ProjectContext;
use std::path::Path;
use tempyr_core::ops;
use tempyr_journal::{TaskTransition, auto_emit_task_transition, path as jpath};

const DEFAULT_JOURNAL_AGENT: &str = "claude";

pub fn run(ctx: &ProjectContext, id: &str, new_status: &str) -> anyhow::Result<()> {
    let outcome = ops::update_status(&ctx.graph_dir, id, new_status, &ctx.schema)?;
    println!("Updated {id} status to '{new_status}'");
    super::warn_if_index_refresh_fails(ctx);

    // Phase 4a: best-effort journal entry on task status transitions.
    // A failure to find a git repo, open a session, or write the entry
    // must not roll back or report-fail the status change — we only log.
    emit_journal_for_transition(&ctx.root, id, new_status, &outcome);

    Ok(())
}

fn emit_journal_for_transition(
    project_root: &Path,
    id: &str,
    new_status: &str,
    outcome: &ops::UpdateOutcome,
) {
    // Anchor on the resolved project root, NOT `current_dir()`. With
    // `--graph-dir /elsewhere/graph` (or a redirect) the user's shell
    // can sit in a totally different repo — a cwd-based lookup would
    // either skip the journal write or, worse, target the wrong repo's
    // refs entirely.
    let common_dir = match jpath::git_common_dir(project_root) {
        Ok(c) => c,
        // Project is not inside a git repo. Tempyr supports that mode
        // (no journals, no publisher) — silently skip the auto-emit.
        Err(_) => return,
    };
    let worktree_top = match jpath::repo_toplevel(project_root) {
        Ok(w) => w,
        Err(_) => return,
    };

    let transition = TaskTransition {
        node_id: id,
        node_type: &outcome.node_type,
        title: &outcome.title,
        prior_status: outcome.prior_status.as_deref(),
        new_status,
    };
    if let Err(e) = auto_emit_task_transition(
        &common_dir,
        &worktree_top,
        DEFAULT_JOURNAL_AGENT,
        &transition,
    ) {
        eprintln!("warning: journal auto-emit for {id} failed: {e}");
    }
}
