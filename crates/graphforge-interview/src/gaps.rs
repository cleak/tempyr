use serde::{Deserialize, Serialize};

use graphforge_core::schema::Schema;

use crate::phases::InterviewPhase;
use crate::session::InterviewSession;

/// A gap in the interview — something missing that drives the next question.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gap {
    pub gap_type: GapType,
    pub priority: GapPriority,
    pub node_type_needed: String,
    pub context: String,
    pub suggested_question: String,
    pub phase: InterviewPhase,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum GapType {
    MissingPersona,
    MissingSuccessMetric,
    MissingConstraint,
    MissingRisk,
    UnclearProblemStatement,
    NoTechnicalDecision,
    MissingComponent,
    MissingDependency,
    NoTaskDecomposition,
    UnresolvedQuestion,
    MissingApiSurface,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum GapPriority {
    Required,
    Recommended,
    NiceToHave,
}

impl std::fmt::Display for GapPriority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Required => write!(f, "required"),
            Self::Recommended => write!(f, "recommended"),
            Self::NiceToHave => write!(f, "nice-to-have"),
        }
    }
}

/// Analyze the current session state and return gaps sorted by phase then priority.
pub fn detect_gaps(session: &InterviewSession, schema: &Schema) -> Vec<Gap> {
    let mut gaps = Vec::new();

    let root_type = &session.root_type;
    let root_title = &session.root_node.id;

    // Check for unclear problem statement
    if session.root_node.body.trim().is_empty()
        || session.root_node.body.len() < 30
    {
        gaps.push(Gap {
            gap_type: GapType::UnclearProblemStatement,
            priority: GapPriority::Required,
            node_type_needed: root_type.clone(),
            context: format!("The {root_type} '{root_title}' needs a clear problem statement."),
            suggested_question: format!(
                "What problem does '{root_title}' solve? Who experiences this problem and what \
                 happens if it's not addressed?"
            ),
            phase: InterviewPhase::Discovery,
        });
    }

    // Check schema-driven gaps based on allowed edges for the root type
    if let Some(node_def) = schema.node_types.get(root_type.as_str()) {
        for allowed_edge in &node_def.allowed_edges {
            let edge_type = &allowed_edge.edge_type;
            let target_type = &allowed_edge.target;

            // Check if this edge type is already covered
            let has_edge = session.has_edge_type_from_root(edge_type);
            let has_target_type = session.has_node_of_type(target_type)
                || session.graph_context.iter().any(|ctx| ctx.starts_with(&format!("{target_type}-")));

            if has_edge || has_target_type {
                continue;
            }

            let (gap_type, priority, phase) = classify_edge_gap(edge_type, target_type);

            let question = generate_question(edge_type, target_type, root_title);

            gaps.push(Gap {
                gap_type,
                priority,
                node_type_needed: target_type.clone(),
                context: format!(
                    "'{root_title}' has no {edge_type} relationship to any {target_type}."
                ),
                suggested_question: question,
                phase,
            });
        }
    }

    // Sort by phase, then by priority (required first)
    gaps.sort_by(|a, b| {
        a.phase.index().cmp(&b.phase.index())
            .then(a.priority.cmp(&b.priority))
    });

    // Deduplicate by gap_type + node_type_needed
    gaps.dedup_by(|a, b| a.gap_type == b.gap_type && a.node_type_needed == b.node_type_needed);

    gaps
}

/// Classify an edge gap into type, priority, and phase.
fn classify_edge_gap(edge_type: &str, target_type: &str) -> (GapType, GapPriority, InterviewPhase) {
    match (edge_type, target_type) {
        ("serves", "persona") => (
            GapType::MissingPersona,
            GapPriority::Required,
            InterviewPhase::Product,
        ),
        ("measured_by", "metric") => (
            GapType::MissingSuccessMetric,
            GapPriority::Required,
            InterviewPhase::Product,
        ),
        ("constrained_by", "constraint") => (
            GapType::MissingConstraint,
            GapPriority::Recommended,
            InterviewPhase::Product,
        ),
        ("has_risk", "risk") => (
            GapType::MissingRisk,
            GapPriority::Recommended,
            InterviewPhase::Product,
        ),
        ("depends_on", "decision") => (
            GapType::NoTechnicalDecision,
            GapPriority::Required,
            InterviewPhase::Technical,
        ),
        ("uses", "component") => (
            GapType::MissingComponent,
            GapPriority::Recommended,
            InterviewPhase::Technical,
        ),
        ("exposes", "api_surface") => (
            GapType::MissingApiSurface,
            GapPriority::NiceToHave,
            InterviewPhase::Technical,
        ),
        ("decomposes_to", "task") => (
            GapType::NoTaskDecomposition,
            GapPriority::Required,
            InterviewPhase::Decomposition,
        ),
        ("has_question", "open_question") => (
            GapType::UnresolvedQuestion,
            GapPriority::NiceToHave,
            InterviewPhase::Decomposition,
        ),
        _ => (
            GapType::MissingDependency,
            GapPriority::NiceToHave,
            InterviewPhase::Technical,
        ),
    }
}

/// Generate a contextual question for a gap.
fn generate_question(edge_type: &str, target_type: &str, root_title: &str) -> String {
    match (edge_type, target_type) {
        ("serves", "persona") => format!(
            "Who is the target user for '{root_title}'? What role do they have and \
             what are they trying to accomplish?"
        ),
        ("measured_by", "metric") => format!(
            "How will we measure the success of '{root_title}'? What specific metric \
             would indicate this is working?"
        ),
        ("constrained_by", "constraint") => format!(
            "Are there any constraints on '{root_title}'? Think about performance, \
             budget, compliance, timeline, or technical limitations."
        ),
        ("has_risk", "risk") => format!(
            "What could go wrong with '{root_title}'? What are the biggest risks \
             or unknowns?"
        ),
        ("depends_on", "decision") => format!(
            "What technical or product decisions need to be made for '{root_title}'? \
             Are there architecture choices, technology selections, or trade-offs to consider?"
        ),
        ("uses", "component") => format!(
            "What existing systems, modules, or components does '{root_title}' \
             interact with or depend on?"
        ),
        ("exposes", "api_surface") => format!(
            "Does '{root_title}' expose any APIs, interfaces, or contracts that \
             other systems will consume?"
        ),
        ("decomposes_to", "task") => format!(
            "What are the implementation tasks for '{root_title}'? Break it down \
             into concrete work items."
        ),
        ("has_question", "open_question") => format!(
            "Are there any open questions or unknowns about '{root_title}' that \
             need to be resolved before or during implementation?"
        ),
        _ => format!(
            "Is there a {target_type} related to '{root_title}' via {edge_type}?"
        ),
    }
}

/// Get the next N questions from the remaining gaps for the current phase.
pub fn next_questions(session: &InterviewSession, max: usize) -> Vec<&Gap> {
    session
        .remaining_gaps
        .iter()
        .filter(|g| g.phase == session.phase || g.phase.index() <= session.phase.index())
        .take(max)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::InterviewSession;
    use graphforge_core::schema::Schema;
    use std::path::Path;

    fn make_schema() -> Schema {
        let schema_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent().unwrap().parent().unwrap()
            .join("schema/default-schema.toml");
        Schema::load(&schema_path).unwrap()
    }

    #[test]
    fn test_detect_gaps_empty_feature() {
        let session = InterviewSession::new("feature", "feat-test", "");
        let schema = make_schema();
        let gaps = detect_gaps(&session, &schema);

        // Should have many gaps for a bare feature
        assert!(!gaps.is_empty());

        // Should have an unclear problem statement gap
        assert!(gaps.iter().any(|g| g.gap_type == GapType::UnclearProblemStatement));

        // Should have a missing persona gap
        assert!(gaps.iter().any(|g| g.gap_type == GapType::MissingPersona));

        // Should have a missing metric gap
        assert!(gaps.iter().any(|g| g.gap_type == GapType::MissingSuccessMetric));
    }

    #[test]
    fn test_detect_gaps_sorted_by_phase_then_priority() {
        let session = InterviewSession::new("feature", "feat-test", "");
        let schema = make_schema();
        let gaps = detect_gaps(&session, &schema);

        // Gaps should be sorted: Discovery first, then Product, Technical, Decomposition
        let mut last_phase = 0;
        let mut last_priority = GapPriority::Required;
        for gap in &gaps {
            let phase = gap.phase.index();
            if phase > last_phase {
                last_priority = GapPriority::Required; // reset priority check on phase change
            }
            assert!(phase >= last_phase, "Gaps not sorted by phase");
            if phase == last_phase {
                assert!(gap.priority >= last_priority, "Gaps not sorted by priority within phase");
            }
            last_phase = phase;
            last_priority = gap.priority;
        }
    }

    #[test]
    fn test_gaps_reduced_with_nodes() {
        let mut session = InterviewSession::new(
            "feature", "feat-test",
            "# Test Feature\n\nA long enough problem statement that should pass the check.\n"
        );
        let schema = make_schema();

        let gaps_before = detect_gaps(&session, &schema);
        let persona_gaps_before = gaps_before.iter().filter(|g| g.gap_type == GapType::MissingPersona).count();
        assert!(persona_gaps_before > 0);

        // Add a persona
        session.add_tentative_node(crate::session::TentativeNode {
            id: "persona-eng".to_string(),
            node_type: "persona".to_string(),
            status: "".to_string(),
            fields: std::collections::HashMap::new(),
            body: "# Engineer\n".to_string(),
            confidence: 0.8,
            source_qa: vec![],
        });
        session.add_tentative_edge(crate::session::TentativeEdge {
            source: "feat-test".to_string(),
            target: "persona-eng".to_string(),
            edge_type: "serves".to_string(),
            source_type: crate::session::EdgeSource::ExplicitFromAnswer,
        });

        let gaps_after = detect_gaps(&session, &schema);
        let persona_gaps_after = gaps_after.iter().filter(|g| g.gap_type == GapType::MissingPersona).count();
        assert_eq!(persona_gaps_after, 0, "Persona gap should be resolved");
    }

    #[test]
    fn test_next_questions() {
        let mut session = InterviewSession::new("feature", "feat-test", "");
        let schema = make_schema();

        session.remaining_gaps = detect_gaps(&session, &schema);

        let questions = next_questions(&session, 3);
        assert!(questions.len() <= 3);
        assert!(!questions.is_empty());
    }

    #[test]
    fn test_generate_question_persona() {
        let q = generate_question("serves", "persona", "feat-replay");
        assert!(q.contains("target user"));
        assert!(q.contains("feat-replay"));
    }

    #[test]
    fn test_no_problem_statement_gap_when_body_sufficient() {
        let session = InterviewSession::new(
            "feature", "feat-test",
            "# Test Feature\n\nA well-described problem that is sufficiently long.\n"
        );
        let schema = make_schema();
        let gaps = detect_gaps(&session, &schema);

        assert!(gaps.iter().all(|g| g.gap_type != GapType::UnclearProblemStatement));
    }
}
