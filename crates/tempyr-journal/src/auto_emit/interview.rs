//! Auto-emit on interview lifecycle events (§9 Phase 4b).
//!
//! Maps the five interview-lifecycle events the spec calls out to
//! journal entries. The entries are all marked `provisional = true`
//! while the interview is in flight; the `Committed` entry is the
//! one non-provisional terminator and is also marked
//! `is_final = true` so the publisher picks up the journal session.
//!
//! | Event             | Kind     | Notes                                     |
//! |-------------------|----------|-------------------------------------------|
//! | [`Started`]       | `plan`   | provisional; brain-dump → root summary    |
//! | [`AnswerRecorded`]| `finding`| provisional                               |
//! | [`PhaseAdvanced`] | `finding`| provisional                               |
//! | [`Adjusted`]      | `finding`| provisional; covers adjust/add/edge ops   |
//! | [`Committed`]     | `outcome`| `passed = true`, `is_final = true`        |
//!
//! [`Started`]: InterviewEvent::Started
//! [`AnswerRecorded`]: InterviewEvent::AnswerRecorded
//! [`PhaseAdvanced`]: InterviewEvent::PhaseAdvanced
//! [`Adjusted`]: InterviewEvent::Adjusted
//! [`Committed`]: InterviewEvent::Committed
//!
//! Rollback is in the spec but isn't wired here — the interview
//! engine has no rollback operation today, so there's no call site
//! to hook. When/if rollback lands, this module gains a sixth
//! variant.

use std::path::Path;

use super::summary::clamp_summary;
use crate::Result;
use crate::kind::Kind;
use crate::session::Session;
use crate::writer::{EntryDraft, WriteOutcome, write_entry};

/// One lifecycle moment of an interview, captured with whatever
/// scalar state is useful in a journal entry. Borrowed strings keep
/// the call sites allocation-free; the variants intentionally don't
/// carry the full `InterviewSession` so this stays cheap to build.
#[derive(Debug, Clone, Copy)]
pub enum InterviewEvent<'a> {
    /// A new interview was started from a brain dump.
    Started {
        session_id: &'a str,
        root_node_id: &'a str,
        root_type: &'a str,
        phase: &'a str,
    },
    /// The user answered a question; gap analysis re-ran.
    AnswerRecorded {
        session_id: &'a str,
        answer: &'a str,
        phase: &'a str,
        filled_gap_count: usize,
    },
    /// Gap analysis advanced the interview to a new phase. Emitted
    /// alongside the [`AnswerRecorded`] / [`Adjusted`] event whose
    /// reanalysis triggered the move.
    ///
    /// [`AnswerRecorded`]: InterviewEvent::AnswerRecorded
    /// [`Adjusted`]: InterviewEvent::Adjusted
    PhaseAdvanced {
        session_id: &'a str,
        from: &'a str,
        to: &'a str,
    },
    /// Tentative graph state was modified — covers `interview_adjust`,
    /// `interview_add_node`, and `interview_add_edge` MCP tools. The
    /// `operation` string distinguishes them in the journal summary.
    Adjusted {
        session_id: &'a str,
        operation: &'a str,
        target: &'a str,
    },
    /// The session was committed — tentative nodes/edges were written
    /// to the graph. Triggers session finalization.
    Committed {
        session_id: &'a str,
        node_count: usize,
        edge_count: usize,
        files_created: usize,
    },
}

/// Map an interview lifecycle event to a journal entry and write it.
/// Errors propagate to the caller, which downgrades them to non-fatal
/// warnings — the interview operation has already mutated state on
/// disk by the time we get here.
pub fn auto_emit_interview_event(
    common_dir: &Path,
    worktree_top: &Path,
    agent: &str,
    event: &InterviewEvent<'_>,
) -> Result<WriteOutcome> {
    let draft = build_draft(event);
    let session = Session::open_or_resume(common_dir, worktree_top, agent)?;
    write_entry(&session, worktree_top, draft)
}

fn build_draft(e: &InterviewEvent<'_>) -> EntryDraft {
    match *e {
        InterviewEvent::Started {
            session_id,
            root_node_id,
            root_type,
            phase,
        } => {
            let summary = clamp_summary(format!(
                "interview started ({phase}): root {root_node_id} ({root_type})"
            ));
            let mut d = EntryDraft::new(Kind::Plan, summary);
            d.provisional = true;
            d.references = vec![root_node_id.to_string()];
            d.tags = vec!["interview".into(), session_id.to_string()];
            d
        }
        InterviewEvent::AnswerRecorded {
            session_id,
            answer,
            phase,
            filled_gap_count,
        } => {
            let summary = clamp_summary(format!(
                "interview answer ({phase}, {filled_gap_count} gap(s) filled): {answer}"
            ));
            let mut d = EntryDraft::new(Kind::Finding, summary);
            d.provisional = true;
            d.tags = vec!["interview".into(), session_id.to_string()];
            d
        }
        InterviewEvent::PhaseAdvanced {
            session_id,
            from,
            to,
        } => {
            let summary = clamp_summary(format!("interview phase advanced: {from} → {to}"));
            let mut d = EntryDraft::new(Kind::Finding, summary);
            d.provisional = true;
            d.tags = vec!["interview".into(), session_id.to_string()];
            d
        }
        InterviewEvent::Adjusted {
            session_id,
            operation,
            target,
        } => {
            // The leading literal here is at least 22 chars so even a
            // 1-char node id clears the 20-char minimum the writer
            // enforces; real node ids are slug + 6-char suffix and
            // never that short, but the floor matters for tests and
            // for any future short-id corner case.
            let summary = clamp_summary(format!("interview tentative graph {operation}: {target}"));
            let mut d = EntryDraft::new(Kind::Finding, summary);
            d.provisional = true;
            d.references = vec![target.to_string()];
            d.tags = vec!["interview".into(), session_id.to_string()];
            d
        }
        InterviewEvent::Committed {
            session_id,
            node_count,
            edge_count,
            files_created,
        } => {
            let summary = clamp_summary(format!(
                "interview committed: {node_count} node(s), {edge_count} edge(s), \
                 {files_created} file(s) created"
            ));
            let mut d = EntryDraft::new(Kind::Outcome, summary);
            d.passed = Some(true);
            d.is_final = true;
            d.tags = vec!["interview".into(), session_id.to_string()];
            d
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Kind;
    use crate::auto_emit::summary::MAX_SUMMARY_CHARS;

    #[test]
    fn started_emits_provisional_plan_with_root_reference() {
        let d = build_draft(&InterviewEvent::Started {
            session_id: "sess-abc",
            root_node_id: "feat-something-aaaaaa",
            root_type: "feature",
            phase: "Discovery",
        });
        assert_eq!(d.kind, Kind::Plan);
        assert!(d.provisional);
        assert!(!d.is_final);
        assert!(d.summary.contains("feat-something-aaaaaa"));
        assert!(d.summary.contains("Discovery"));
        assert!(d.references.iter().any(|r| r == "feat-something-aaaaaa"));
        assert!(d.tags.iter().any(|t| t == "interview"));
        assert!(d.tags.iter().any(|t| t == "sess-abc"));
    }

    #[test]
    fn answer_recorded_emits_provisional_finding() {
        let d = build_draft(&InterviewEvent::AnswerRecorded {
            session_id: "sess-abc",
            answer: "the persona is mid-market PMs",
            phase: "Product",
            filled_gap_count: 2,
        });
        assert_eq!(d.kind, Kind::Finding);
        assert!(d.provisional);
        assert!(!d.is_final);
        assert!(d.summary.contains("Product"));
        assert!(d.summary.contains("mid-market PMs"));
    }

    #[test]
    fn phase_advanced_emits_provisional_finding() {
        let d = build_draft(&InterviewEvent::PhaseAdvanced {
            session_id: "sess-abc",
            from: "Discovery",
            to: "Product",
        });
        assert_eq!(d.kind, Kind::Finding);
        assert!(d.provisional);
        assert!(d.summary.contains("Discovery"));
        assert!(d.summary.contains("Product"));
    }

    #[test]
    fn adjusted_emits_provisional_finding_with_target_reference() {
        let d = build_draft(&InterviewEvent::Adjusted {
            session_id: "sess-abc",
            operation: "adjust",
            target: "feat-thing-aaaaaa",
        });
        assert_eq!(d.kind, Kind::Finding);
        assert!(d.provisional);
        assert!(d.references.iter().any(|r| r == "feat-thing-aaaaaa"));
    }

    #[test]
    fn committed_emits_final_outcome_with_passed() {
        let d = build_draft(&InterviewEvent::Committed {
            session_id: "sess-abc",
            node_count: 4,
            edge_count: 3,
            files_created: 4,
        });
        assert_eq!(d.kind, Kind::Outcome);
        assert_eq!(d.passed, Some(true));
        assert!(d.is_final);
        assert!(!d.provisional);
        assert!(d.summary.contains("4 node"));
        assert!(d.summary.contains("3 edge"));
    }

    #[test]
    fn long_answer_is_truncated() {
        let answer = "x".repeat(500);
        let d = build_draft(&InterviewEvent::AnswerRecorded {
            session_id: "sess-abc",
            answer: &answer,
            phase: "Discovery",
            filled_gap_count: 0,
        });
        assert!(d.summary.chars().count() <= MAX_SUMMARY_CHARS);
    }

    #[test]
    fn every_event_satisfies_minimum_summary_length() {
        // Every variant's summary must clear the 20-char floor enforced
        // by `validate_entry`. Probe each with the smallest-plausible
        // inputs to lock that in.
        let cases = [
            InterviewEvent::Started {
                session_id: "s",
                root_node_id: "n",
                root_type: "t",
                phase: "Discovery",
            },
            InterviewEvent::AnswerRecorded {
                session_id: "s",
                answer: "a",
                phase: "Product",
                filled_gap_count: 0,
            },
            InterviewEvent::PhaseAdvanced {
                session_id: "s",
                from: "Discovery",
                to: "Product",
            },
            InterviewEvent::Adjusted {
                session_id: "s",
                operation: "adjust",
                target: "n",
            },
            InterviewEvent::Committed {
                session_id: "s",
                node_count: 0,
                edge_count: 0,
                files_created: 0,
            },
        ];
        for c in cases {
            let d = build_draft(&c);
            assert!(
                d.summary.chars().count() >= 20,
                "summary too short ({} chars) for {:?}",
                d.summary.chars().count(),
                c
            );
        }
    }
}
