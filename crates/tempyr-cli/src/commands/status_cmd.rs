use crate::config::ProjectContext;
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
    emit_journal_for_transition(id, new_status, &outcome);

    Ok(())
}

fn emit_journal_for_transition(id: &str, new_status: &str, outcome: &ops::UpdateOutcome) {
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(_) => return,
    };
    let common_dir = match jpath::git_common_dir(&cwd) {
        Ok(c) => c,
        // Not in a git repo — graph could still live outside one in
        // tests / unusual setups. Silently skip the journal hook.
        Err(_) => return,
    };
    let worktree_top = match jpath::repo_toplevel(&cwd) {
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
