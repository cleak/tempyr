use serde::{Deserialize, Serialize};

use crate::session::InterviewSession;

/// The 5 phases of an interview.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InterviewPhase {
    /// Parse initial input, query graph for related nodes, identify what exists
    Discovery,
    /// Who is this for? What problem does it solve? What does success look like?
    Product,
    /// How does this interact with existing systems? What are the technical constraints?
    Technical,
    /// What are the tasks? What depends on what? What questions are still open?
    Decomposition,
    /// Present the full tentative graph for review and approval
    Review,
}

impl InterviewPhase {
    /// Get the next phase in the sequence.
    pub fn next(self) -> Option<Self> {
        match self {
            Self::Discovery => Some(Self::Product),
            Self::Product => Some(Self::Technical),
            Self::Technical => Some(Self::Decomposition),
            Self::Decomposition => Some(Self::Review),
            Self::Review => None,
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Discovery => "Discovery",
            Self::Product => "Product",
            Self::Technical => "Technical",
            Self::Decomposition => "Decomposition",
            Self::Review => "Review",
        }
    }

    /// Return phase index (0-4) for progress display.
    pub fn index(&self) -> usize {
        match self {
            Self::Discovery => 0,
            Self::Product => 1,
            Self::Technical => 2,
            Self::Decomposition => 3,
            Self::Review => 4,
        }
    }
}

/// Check if the session is ready to transition to the next phase.
/// Returns the next phase if a transition should happen, None otherwise.
///
/// Every transition needs at least one answer recorded in the current
/// phase *and* that phase's node condition, or else `N` answers in the
/// phase as a fallback so no answer is ever forced. Node conditions
/// ignore tentative nodes still marked `needs:`, so a weak answer the
/// interviewer is pushing on can't carry the interview forward.
///
/// Callers should only evaluate this after recording an answer (see
/// `proposer::record_answer`); adding nodes or edges mid-turn must not
/// move the phase before the user has responded to the push.
pub fn check_phase_transition(session: &InterviewSession) -> Option<InterviewPhase> {
    let answers = session.answers_in_phase(session.phase);
    let ready = |nodes_ready: bool, fallback: usize| -> bool {
        (answers >= 1 && nodes_ready) || answers >= fallback
    };

    match session.phase {
        InterviewPhase::Discovery => {
            // Discovery → Product: the user has replied once (the opener's
            // premise list). No node condition.
            ready(true, 1).then_some(InterviewPhase::Product)
        }

        InterviewPhase::Product => {
            // Product → Technical: a persona, a success metric, and a
            // substantive problem statement.
            let has_persona = session.has_ready_node_of_type("persona")
                || session.has_ready_edge_type_from_root("serves");
            let has_metric = session.has_ready_node_of_type("metric")
                || session.has_ready_edge_type_from_root("measured_by");
            let has_problem = session.root_node.body.len() > 50;

            ready(has_persona && has_metric && has_problem, 4).then_some(InterviewPhase::Technical)
        }

        InterviewPhase::Technical => {
            // Technical → Decomposition: a component or an architecture
            // decision.
            let has_component = session.has_ready_node_of_type("component")
                || session.has_ready_edge_type_from_root("uses");
            let has_decision = session.has_ready_node_of_type("decision")
                || session.has_ready_edge_type_from_root("depends_on");

            ready(has_component || has_decision, 3).then_some(InterviewPhase::Decomposition)
        }

        InterviewPhase::Decomposition => {
            // Decomposition → Review: at least one task.
            let has_task = session.has_ready_node_of_type("task")
                || session.has_ready_edge_type_from_root("decomposes_to");

            ready(has_task, 2).then_some(InterviewPhase::Review)
        }

        InterviewPhase::Review => {
            // Review is the final phase
            None
        }
    }
}

/// Advance the session to the next phase if conditions are met.
/// Returns true if a transition occurred.
pub fn try_advance_phase(session: &mut InterviewSession) -> bool {
    if let Some(next_phase) = check_phase_transition(session) {
        session.phase = next_phase;
        true
    } else {
        false
    }
}

/// Get progress as a human-readable string.
pub fn progress_summary(session: &InterviewSession) -> String {
    let phase = session.phase.display_name();
    let phase_idx = session.phase.index() + 1;
    let total_nodes = session.tentative_nodes.len() + 1; // +1 for root
    let total_edges = session.tentative_edges.len();
    let answered = session.answered.len();
    let remaining = session.remaining_gaps.len();

    format!(
        "Phase {phase_idx}/5 ({phase}) | {total_nodes} nodes, {total_edges} edges | \
         {answered} questions answered, {remaining} gaps remaining"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{InterviewSession, TentativeNode};
    use std::collections::HashMap;

    #[test]
    fn test_phase_ordering() {
        assert_eq!(
            InterviewPhase::Discovery.next(),
            Some(InterviewPhase::Product)
        );
        assert_eq!(
            InterviewPhase::Product.next(),
            Some(InterviewPhase::Technical)
        );
        assert_eq!(
            InterviewPhase::Technical.next(),
            Some(InterviewPhase::Decomposition)
        );
        assert_eq!(
            InterviewPhase::Decomposition.next(),
            Some(InterviewPhase::Review)
        );
        assert_eq!(InterviewPhase::Review.next(), None);
    }

    #[test]
    fn test_discovery_to_product_transition() {
        let mut session = InterviewSession::new(
            "feature",
            "feat-a",
            "# Session Replay\n\n## Problem\n\nEngineers need to see what happened during sessions to debug funnel issues.\n",
        );

        // Graph context alone no longer advances: the user hasn't replied yet.
        session.graph_context.push("epic-observability".to_string());
        assert!(check_phase_transition(&session).is_none());

        // The premise reply is the one Discovery answer needed.
        session.record_answer("Premises 1-3?", "1y 2n 3y", vec![]);

        assert_eq!(
            check_phase_transition(&session),
            Some(InterviewPhase::Product)
        );
    }

    #[test]
    fn test_product_to_technical_transition() {
        let mut session = InterviewSession::new(
            "feature",
            "feat-a",
            "# Feature\n\n## Problem\n\nA real problem statement that is long enough to pass the threshold check.\n",
        );
        session.phase = InterviewPhase::Product;

        // Not yet: no persona or metric
        assert!(check_phase_transition(&session).is_none());

        // Add persona
        session.add_tentative_node(TentativeNode {
            id: "persona-eng".to_string(),
            node_type: "persona".to_string(),
            status: "".to_string(),
            fields: HashMap::new(),
            body: "# Engineer\n".to_string(),
            confidence: 0.8,
            source_qa: vec![],
        });

        // Still not: no metric
        assert!(check_phase_transition(&session).is_none());

        // Add metric
        session.add_tentative_node(TentativeNode {
            id: "metric-mttr".to_string(),
            node_type: "metric".to_string(),
            status: "proposed".to_string(),
            fields: HashMap::new(),
            body: "# Reduce MTTR\n".to_string(),
            confidence: 0.7,
            source_qa: vec![],
        });

        // Nodes alone aren't enough: no answer recorded in Product yet.
        assert!(check_phase_transition(&session).is_none());

        session.record_answer("Who and how measured?", "SREs; MTTR < 30m", vec![]);

        assert_eq!(
            check_phase_transition(&session),
            Some(InterviewPhase::Technical)
        );
    }

    #[test]
    fn test_needs_marked_node_does_not_satisfy_phase() {
        let mut session = InterviewSession::new(
            "feature",
            "feat-a",
            "# Feature\n\n## Problem\n\nA real problem statement that is long enough to pass the threshold check.\n",
        );
        session.phase = InterviewPhase::Product;
        session.record_answer("Who and how measured?", "SREs, faster debugging", vec![]);

        session.add_tentative_node(TentativeNode {
            id: "persona-sre".to_string(),
            node_type: "persona".to_string(),
            status: "".to_string(),
            fields: HashMap::new(),
            body: "# SRE\n\nGoal: fix incidents fast.\n".to_string(),
            confidence: 0.9,
            source_qa: vec![],
        });
        session.add_tentative_node(TentativeNode {
            id: "metric-faster".to_string(),
            node_type: "metric".to_string(),
            status: "proposed".to_string(),
            fields: HashMap::new(),
            body: "# Faster debugging\n\nneeds: a threshold\n".to_string(),
            confidence: 0.6,
            source_qa: vec![],
        });

        // The metric is still being pushed on, so Product holds.
        assert!(check_phase_transition(&session).is_none());

        // Resolving the push clears the marker.
        session
            .adjust_node(
                "metric-faster",
                crate::session::NodePatch {
                    body: Some("# Faster debugging\n\nMTTR under 30 minutes.\n".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            check_phase_transition(&session),
            Some(InterviewPhase::Technical)
        );
    }

    #[test]
    fn test_product_fallback_after_four_answers() {
        let mut session = InterviewSession::new("feature", "feat-a", "# A\n");
        session.phase = InterviewPhase::Product;
        for i in 0..3 {
            session.record_answer("q", &format!("answer {i}"), vec![]);
            assert!(check_phase_transition(&session).is_none());
        }
        session.record_answer("q", "answer 3", vec![]);
        assert_eq!(
            check_phase_transition(&session),
            Some(InterviewPhase::Technical)
        );
    }

    #[test]
    fn test_technical_to_decomposition_transition() {
        let mut session =
            InterviewSession::new("feature", "feat-a", "# A\n\nLong enough body for checks.\n");
        session.phase = InterviewPhase::Technical;

        assert!(check_phase_transition(&session).is_none());

        // Add a decision
        session.add_tentative_node(TentativeNode {
            id: "decision-storage".to_string(),
            node_type: "decision".to_string(),
            status: "proposed".to_string(),
            fields: HashMap::new(),
            body: "# Storage Decision\n".to_string(),
            confidence: 0.8,
            source_qa: vec![],
        });

        assert!(check_phase_transition(&session).is_none());
        session.record_answer("Storage?", "SQLite, considered Postgres", vec![]);

        assert_eq!(
            check_phase_transition(&session),
            Some(InterviewPhase::Decomposition)
        );
    }

    #[test]
    fn test_decomposition_to_review_transition() {
        let mut session = InterviewSession::new("feature", "feat-a", "# A\n");
        session.phase = InterviewPhase::Decomposition;

        assert!(check_phase_transition(&session).is_none());

        // Add a task
        session.add_tentative_node(TentativeNode {
            id: "task-impl".to_string(),
            node_type: "task".to_string(),
            status: "backlog".to_string(),
            fields: HashMap::new(),
            body: "# Implement\n".to_string(),
            confidence: 0.8,
            source_qa: vec![],
        });

        assert!(check_phase_transition(&session).is_none());
        session.record_answer("Tasks?", "Implement it", vec![]);

        assert_eq!(
            check_phase_transition(&session),
            Some(InterviewPhase::Review)
        );
    }

    #[test]
    fn test_try_advance_phase() {
        let mut session = InterviewSession::new(
            "feature",
            "feat-a",
            "# Feature\n\nA long enough problem statement for the body check.\n",
        );
        session.graph_context.push("existing-node".to_string());

        assert_eq!(session.phase, InterviewPhase::Discovery);
        assert!(!try_advance_phase(&mut session));
        session.record_answer("Premises?", "1y 2y", vec![]);
        assert!(try_advance_phase(&mut session));
        assert_eq!(session.phase, InterviewPhase::Product);

        // Shouldn't advance again without meeting Product conditions
        assert!(!try_advance_phase(&mut session));
        assert_eq!(session.phase, InterviewPhase::Product);
    }

    #[test]
    fn test_progress_summary() {
        let session = InterviewSession::new("feature", "feat-a", "# A\n");
        let summary = progress_summary(&session);
        assert!(summary.contains("Phase 1/5"));
        assert!(summary.contains("Discovery"));
        assert!(summary.contains("1 nodes"));
    }

    #[test]
    fn test_discovery_advances_after_one_answer_without_context() {
        let mut session = InterviewSession::new(
            "feature",
            "feat-a",
            "# Feature\n\nA long enough problem statement for the body check.\n",
        );
        // No graph_context — simulates empty/new graph
        assert!(session.graph_context.is_empty());
        assert!(check_phase_transition(&session).is_none());

        session.record_answer("Premises?", "1y 2n", vec![]);

        assert_eq!(
            check_phase_transition(&session),
            Some(InterviewPhase::Product)
        );
    }

    #[test]
    fn test_technical_fallback_after_three_answers() {
        let mut session = InterviewSession::new(
            "feature",
            "feat-a",
            "# Feature\n\nA long enough problem statement for the body check.\n",
        );
        session.phase = InterviewPhase::Technical;

        // No component or decision nodes — fallback path only
        assert!(check_phase_transition(&session).is_none());

        // Record 3 answers in the Technical phase
        session.record_answer("What stack?", "Rust + SQLite.", vec![]);
        session.record_answer("Any constraints?", "Must run offline.", vec![]);
        session.record_answer("Performance needs?", "Sub-second queries.", vec![]);

        // Fallback should trigger: 3 answers in Technical → advance to Decomposition
        assert_eq!(
            check_phase_transition(&session),
            Some(InterviewPhase::Decomposition)
        );
    }

    #[test]
    fn test_decomposition_fallback_after_two_answers() {
        let mut session = InterviewSession::new(
            "feature",
            "feat-a",
            "# Feature\n\nA long enough problem statement for the body check.\n",
        );
        session.phase = InterviewPhase::Decomposition;

        // No task nodes — fallback path only
        assert!(check_phase_transition(&session).is_none());

        // Record 2 answers in the Decomposition phase
        session.record_answer(
            "What are the subtasks?",
            "Index rebuild and query engine.",
            vec![],
        );
        session.record_answer("Any dependencies?", "Query depends on index.", vec![]);

        // Fallback should trigger: 2 answers in Decomposition → advance to Review
        assert_eq!(
            check_phase_transition(&session),
            Some(InterviewPhase::Review)
        );
    }
}
