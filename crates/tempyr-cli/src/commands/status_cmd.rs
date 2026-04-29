use crate::config::ProjectContext;
use std::path::Path;
use tempyr_core::ops;
use tempyr_journal::{JournalError, TaskTransition, auto_emit_task_transition, path as jpath};

pub fn run(ctx: &ProjectContext, id: &str, new_status: &str, agent: &str) -> anyhow::Result<()> {
    // Resolve up front so the user-printed message, the file lookup
    // inside `update_status`, and the journal entry all reference the
    // canonical full id. Tempyr accepts the 6-char suffix as a short
    // form for any node — `find_node_file` (called by `update_status`)
    // only does exact-filename matching, so we must resolve here.
    let resolved_id = ops::resolve_node_id(&ctx.graph_dir, id)?;
    let outcome = ops::update_status(&ctx.graph_dir, &resolved_id, new_status, &ctx.schema)?;
    println!("Updated {resolved_id} status to '{new_status}'");
    super::warn_if_index_refresh_fails(ctx);

    // Phase 4a: best-effort journal entry on task status transitions.
    // A failure to find a git repo, open a session, or write the entry
    // must not roll back or report-fail the status change — we only log.
    emit_journal_for_transition(&ctx.root, agent, &resolved_id, new_status, &outcome);

    Ok(())
}

fn emit_journal_for_transition(
    project_root: &Path,
    agent: &str,
    id: &str,
    new_status: &str,
    outcome: &ops::UpdateOutcome,
) {
    // Anchor on the resolved project root, NOT `current_dir()`. With
    // `--graph-dir /elsewhere/graph` (or a redirect) the user's shell
    // can sit in a totally different repo — a cwd-based lookup would
    // either skip the journal write or, worse, target the wrong repo's
    // refs entirely.
    //
    // Error policy: silently swallow `NotAGitRepo` (tempyr supports
    // operating outside a git repo, so "no journal" is the expected
    // fallthrough). Surface any other error (IO, git binary missing,
    // etc.) on stderr so real bugs aren't invisible.
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

    let transition = TaskTransition {
        node_id: id,
        node_type: &outcome.node_type,
        title: &outcome.title,
        prior_status: outcome.prior_status.as_deref(),
        new_status,
    };
    if let Err(e) = auto_emit_task_transition(&common_dir, &worktree_top, agent, &transition) {
        eprintln!("warning: journal auto-emit for {id} failed: {e}");
    }
}
