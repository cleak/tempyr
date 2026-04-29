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
    let summary = match (prior, t.new_status) {
        ("backlog", "in_progress") => format!("starting task {}: {}", t.node_id, t.title),
        ("in_progress", "done") => format!("completed task {}: {}", t.node_id, t.title),
        ("in_progress", "blocked") => format!("task {} blocked: {}", t.node_id, t.title),
        _ => return None,
    };
    let summary = clamp_summary(summary);
    let mut d = match (prior, t.new_status) {
        ("backlog", "in_progress") => {
            let mut d = EntryDraft::new(Kind::Plan, summary);
            d.provisional = true;
            d
        }
        ("in_progress", "done") => {
            let mut d = EntryDraft::new(Kind::Outcome, summary);
            d.passed = Some(true);
            d.is_final = true;
            d
        }
        ("in_progress", "blocked") => {
            let mut d = EntryDraft::new(Kind::Risk, summary);
            d.severity = Some(Severity::Blocker);
            d
        }
        _ => unreachable!("filtered above"),
    };
    d.references = vec![t.node_id.to_string()];
    Some(d)
}

/// Largest summary length the journal validator will accept (matches
/// the upper bound enforced by `kind::validate_entry`). Synthesized
/// summaries reuse the user-supplied node title verbatim, so a long
/// H1 — or a future schema with relaxed title bounds — could blow the
/// limit. Truncate at this many *Unicode scalars* so we never split a
/// multi-byte char, and append a marker so the truncation is visible
/// in the journal.
const MAX_SUMMARY_CHARS: usize = 200;

/// Truncation marker appended when a summary is shortened. Six chars
/// is enough for `…` plus a `[cut]` tag — picked so the marker fits
/// inside the budget and reads clearly in a CLI listing.
const TRUNCATION_MARKER: &str = " […cut]";

fn clamp_summary(s: String) -> String {
    let len = s.chars().count();
    if len <= MAX_SUMMARY_CHARS {
        return s;
    }
    let marker_chars = TRUNCATION_MARKER.chars().count();
    let keep = MAX_SUMMARY_CHARS.saturating_sub(marker_chars);
    let mut out: String = s.chars().take(keep).collect();
    out.push_str(TRUNCATION_MARKER);
    out
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

    #[test]
    fn long_title_is_truncated_below_summary_limit() {
        // Construct a title pushing the formatted summary well past the
        // 200-char ceiling. Without truncation this would fail the
        // writer's `validate_entry` length check at runtime.
        let long_title = "x".repeat(500);
        let transition = TaskTransition {
            node_id: "task-overflow-aaaaaa",
            node_type: "task",
            title: &long_title,
            prior_status: Some("backlog"),
            new_status: "in_progress",
        };
        let draft = build_draft(&transition).unwrap();
        assert!(
            draft.summary.chars().count() <= MAX_SUMMARY_CHARS,
            "summary still over limit: {} chars",
            draft.summary.chars().count()
        );
        assert!(draft.summary.ends_with(TRUNCATION_MARKER));
    }

    #[test]
    fn truncation_does_not_split_multibyte_characters() {
        // Padding from the front with a 4-byte emoji ensures the
        // boundary cuts inside what would be a multi-byte sequence
        // for a byte-oriented truncation. Char-aware truncation must
        // produce a valid UTF-8 string regardless.
        let title: String = "🦀".repeat(300);
        let transition = TaskTransition {
            node_id: "task-utf8-aaaaaa",
            node_type: "task",
            title: &title,
            prior_status: Some("backlog"),
            new_status: "in_progress",
        };
        let draft = build_draft(&transition).unwrap();
        // Round-trip through validation surrogate: every char boundary
        // is intact (this would have panicked on a byte-split slice).
        assert!(draft.summary.chars().count() <= MAX_SUMMARY_CHARS);
        let reparsed: String = draft.summary.chars().collect();
        assert_eq!(reparsed, draft.summary);
    }

    #[test]
    fn short_summary_is_left_unchanged() {
        let draft = build_draft(&t("task", Some("backlog"), "in_progress")).unwrap();
        assert!(!draft.summary.ends_with(TRUNCATION_MARKER));
    }
}
