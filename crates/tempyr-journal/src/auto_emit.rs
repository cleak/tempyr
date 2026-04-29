//! Auto-emit journal entries on task status transitions.
//!
//! Phase 4a hooks the CLI `tempyr status` and the MCP `graph_update_node`
//! tool into the journal. When a node of type `task` moves between statuses
//! the indexer would care about, we synthesize one entry that captures the
//! transition. Per the spec (`docs/journal-spec.md` §9 Phase 4):
//!
//! | From → To                | Kind     | Notes                                  |
//! |--------------------------|----------|----------------------------------------|
//! | `backlog` → `in_progress`| `plan`   | provisional                            |
//! | `in_progress` → `done`   | `outcome`| `passed = true`, `final = true`        |
//! | `in_progress` → `blocked`| `risk`   | `severity = blocker`                   |
//!
//! Anything else (e.g. status changes on non-task nodes, or transitions
//! outside the table) is a no-op — the function returns `Ok(None)`.
//!
//! **Best-effort only**: callers wrap errors as warnings rather than
//! failing the underlying status change. That's the contract: a journal
//! hiccup must never block a graph mutation.

use std::path::Path;

use crate::kind::Kind;
use crate::session::Session;
use crate::writer::{EntryDraft, WriteOutcome, write_entry};
use crate::{Result, Severity};

/// Snapshot of a graph-node status change captured by the caller before
/// and after [`tempyr_core::ops::update_status`] / `update_node` runs.
/// Borrowed strings keep this allocation-free for the common case
/// (the caller already owns these as `String`s on the parsed `Node`).
#[derive(Debug, Clone, Copy)]
pub struct TaskTransition<'a> {
    pub node_id: &'a str,
    pub node_type: &'a str,
    /// First H1 in the node body or the id (matches `Node::title`).
    /// Used in the journal entry summary so the line reads as "starting
    /// task <title>" instead of a bare slug.
    pub title: &'a str,
    /// Status the node had on disk before the update. `None` if the
    /// node had no status set previously — those transitions still
    /// don't fire, since none of the spec'd rules match a `None` source.
    pub prior_status: Option<&'a str>,
    pub new_status: &'a str,
}

/// Map a status transition on a task node to a journal entry and write
/// it. Returns `Ok(None)` for any input that doesn't match one of the
/// three spec'd rules — including non-task nodes, no-op transitions
/// (`prior == new`), and any transition outside the rule table.
///
/// Errors propagate to the caller, which is responsible for downgrading
/// them to non-fatal warnings (the auto-emit must never fail the
/// surrounding status-change operation).
pub fn auto_emit_task_transition(
    common_dir: &Path,
    worktree_top: &Path,
    agent: &str,
    transition: &TaskTransition<'_>,
) -> Result<Option<WriteOutcome>> {
    let Some(draft) = build_draft(transition) else {
        return Ok(None);
    };
    let session = Session::open_or_resume(common_dir, worktree_top, agent)?;
    let outcome = write_entry(&session, worktree_top, draft)?;
    Ok(Some(outcome))
}

fn build_draft(t: &TaskTransition<'_>) -> Option<EntryDraft> {
    if t.node_type != "task" {
        return None;
    }
    let prior = t.prior_status?;
    if prior == t.new_status {
        return None;
    }
    match (prior, t.new_status) {
        ("backlog", "in_progress") => {
            let mut d = EntryDraft::new(
                Kind::Plan,
                format!("starting task {}: {}", t.node_id, t.title),
            );
            d.provisional = true;
            d.references = vec![t.node_id.to_string()];
            Some(d)
        }
        ("in_progress", "done") => {
            let mut d = EntryDraft::new(
                Kind::Outcome,
                format!("completed task {}: {}", t.node_id, t.title),
            );
            d.passed = Some(true);
            d.is_final = true;
            d.references = vec![t.node_id.to_string()];
            Some(d)
        }
        ("in_progress", "blocked") => {
            let mut d = EntryDraft::new(
                Kind::Risk,
                format!("task {} blocked: {}", t.node_id, t.title),
            );
            d.severity = Some(Severity::Blocker);
            d.references = vec![t.node_id.to_string()];
            Some(d)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Kind;

    fn t<'a>(node_type: &'a str, prior: Option<&'a str>, new: &'a str) -> TaskTransition<'a> {
        TaskTransition {
            node_id: "task-do-the-thing-abc123",
            node_type,
            title: "Do the thing",
            prior_status: prior,
            new_status: new,
        }
    }

    #[test]
    fn backlog_to_in_progress_emits_provisional_plan() {
        let d = build_draft(&t("task", Some("backlog"), "in_progress")).unwrap();
        assert_eq!(d.kind, Kind::Plan);
        assert!(d.provisional);
        assert!(!d.is_final);
        assert!(d.summary.contains("task-do-the-thing-abc123"));
        assert!(d.summary.contains("Do the thing"));
        assert!(d.summary.chars().count() >= 20);
        assert_eq!(d.references, vec!["task-do-the-thing-abc123".to_string()]);
    }

    #[test]
    fn in_progress_to_done_emits_final_outcome() {
        let d = build_draft(&t("task", Some("in_progress"), "done")).unwrap();
        assert_eq!(d.kind, Kind::Outcome);
        assert_eq!(d.passed, Some(true));
        assert!(d.is_final);
        assert!(d.summary.chars().count() >= 20);
    }

    #[test]
    fn in_progress_to_blocked_emits_blocker_risk() {
        let d = build_draft(&t("task", Some("in_progress"), "blocked")).unwrap();
        assert_eq!(d.kind, Kind::Risk);
        assert_eq!(d.severity, Some(Severity::Blocker));
        assert!(!d.is_final);
        assert!(d.summary.chars().count() >= 20);
    }

    #[test]
    fn non_task_nodes_dont_emit() {
        assert!(build_draft(&t("feature", Some("backlog"), "in_progress")).is_none());
        assert!(build_draft(&t("decision", Some("draft"), "accepted")).is_none());
    }

    #[test]
    fn no_op_transitions_dont_emit() {
        assert!(build_draft(&t("task", Some("in_progress"), "in_progress")).is_none());
    }

    #[test]
    fn missing_prior_status_doesnt_emit() {
        // None of the rules have `None` as the source; freshly-created
        // tasks with no prior status should pass through silently.
        assert!(build_draft(&t("task", None, "in_progress")).is_none());
    }

    #[test]
    fn unmapped_transitions_dont_emit() {
        // backlog -> done skips the in_progress phase; spec doesn't
        // enumerate it, so we don't synthesize anything.
        assert!(build_draft(&t("task", Some("backlog"), "done")).is_none());
        // done -> in_progress (re-opening) — also unmapped.
        assert!(build_draft(&t("task", Some("done"), "in_progress")).is_none());
    }
}
