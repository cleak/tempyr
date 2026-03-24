use serde::{Deserialize, Serialize};

use graphforge_core::graph::Graph;
use graphforge_core::schema::Schema;

use crate::phases::InterviewPhase;
use crate::session::InterviewSession;

/// A gap in the interview — something missing that drives the next question.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gap {
    /// Unique gap identifier (e.g., "gap-missing-persona-persona").
    #[serde(default)]
    pub id: String,
    pub gap_type: GapType,
    pub priority: GapPriority,
    pub node_type_needed: String,
    /// The edge type that would fill this gap, if applicable.
    #[serde(default)]
    pub edge_type_needed: Option<String>,
    pub context: String,
    pub suggested_question: String,
    /// Hint for Claude on how to approach the question — deterministic, context-aware.
    #[serde(default)]
    pub suggested_angle: String,
    /// IDs of existing nodes that might fill this gap.
    #[serde(default)]
    pub existing_related: Vec<String>,
    /// How the question should be phrased.
    #[serde(default)]
    pub question_type: QuestionType,
    pub phase: InterviewPhase,
    /// Whether this gap has been filled.
    #[serde(default)]
    pub filled: bool,
    /// The QAPair index or node ID that filled this gap.
    #[serde(default)]
    pub filled_by: Option<String>,
}

/// How a gap's question should be phrased by Claude.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum QuestionType {
    /// Yes/no confirmation (1 candidate exists).
    Closed,
    /// Free-form answer needed (no candidates).
    #[default]
    Open,
    /// Pick from 2-3 concrete options (multiple candidates).
    ForcedChoice,
    /// Surface something the user hasn't considered.
    Implication,
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
    MissingOwner,
    InsufficientDetail,
}

impl std::fmt::Display for GapType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingPersona => write!(f, "missing_persona"),
            Self::MissingSuccessMetric => write!(f, "missing_success_metric"),
            Self::MissingConstraint => write!(f, "missing_constraint"),
            Self::MissingRisk => write!(f, "missing_risk"),
            Self::UnclearProblemStatement => write!(f, "unclear_problem"),
            Self::NoTechnicalDecision => write!(f, "no_technical_decision"),
            Self::MissingComponent => write!(f, "missing_component"),
            Self::MissingDependency => write!(f, "missing_dependency"),
            Self::NoTaskDecomposition => write!(f, "no_task_decomposition"),
            Self::UnresolvedQuestion => write!(f, "unresolved_question"),
            Self::MissingApiSurface => write!(f, "missing_api_surface"),
            Self::MissingOwner => write!(f, "missing_owner"),
            Self::InsufficientDetail => write!(f, "insufficient_detail"),
        }
    }
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
/// If a graph is provided, populates `existing_related` and adjusts `question_type`
/// based on existing nodes that could fill each gap.
pub fn detect_gaps(session: &InterviewSession, schema: &Schema) -> Vec<Gap> {
    detect_gaps_with_graph(session, schema, None)
}

/// Context-aware gap detection with optional graph for richer results.
pub fn detect_gaps_with_graph(
    session: &InterviewSession,
    schema: &Schema,
    graph: Option<&Graph>,
) -> Vec<Gap> {
    let mut gaps = Vec::new();

    let root_type = &session.root_type;
    let root_title = &session.root_node.id;

    // Check for unclear problem statement
    if session.root_node.body.trim().is_empty()
        || session.root_node.body.len() < 30
    {
        let gap_type = GapType::UnclearProblemStatement;
        gaps.push(Gap {
            id: format!("gap-{}-{}", gap_type, root_type),
            gap_type,
            priority: GapPriority::Required,
            node_type_needed: root_type.clone(),
            edge_type_needed: None,
            context: format!("The {root_type} '{root_title}' needs a clear problem statement."),
            suggested_question: format!(
                "What problem does '{root_title}' solve? Who experiences this problem and what \
                 happens if it's not addressed?"
            ),
            suggested_angle: "Ask what problem this solves and what happens if it's not addressed.".to_string(),
            existing_related: vec![],
            question_type: QuestionType::Open,
            phase: InterviewPhase::Discovery,
            filled: false,
            filled_by: None,
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
            let angle = build_suggested_angle(&gap_type, &session.root_node.body);

            // Find existing nodes that could fill this gap
            let (existing_related, question_type) = if let Some(g) = graph {
                let candidates: Vec<String> = g
                    .nodes_of_type(target_type)
                    .iter()
                    .map(|n| n.id().to_string())
                    .collect();
                let qt = match candidates.len() {
                    0 => classify_question_type(priority),
                    1 => QuestionType::Closed,
                    _ => QuestionType::ForcedChoice,
                };
                (candidates, qt)
            } else {
                (vec![], classify_question_type(priority))
            };

            gaps.push(Gap {
                id: format!("gap-{}-{}", gap_type, target_type),
                gap_type,
                priority,
                node_type_needed: target_type.clone(),
                edge_type_needed: Some(edge_type.clone()),
                context: format!(
                    "'{root_title}' has no {edge_type} relationship to any {target_type}."
                ),
                suggested_question: question,
                suggested_angle: angle,
                existing_related,
                question_type,
                phase,
                filled: false,
                filled_by: None,
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

/// Classify question type based on gap priority.
/// When graph context is available (Stage 4), this also considers candidate count.
fn classify_question_type(priority: GapPriority) -> QuestionType {
    match priority {
        GapPriority::Recommended => QuestionType::Implication,
        _ => QuestionType::Open,
    }
}

/// Build a deterministic suggested angle based on gap type and root body content.
/// This tells Claude *how* to approach the question, not *what* to ask.
fn build_suggested_angle(gap_type: &GapType, root_body: &str) -> String {
    let body_lower = root_body.to_lowercase();
    match gap_type {
        GapType::MissingPersona => {
            "Ask who will use this and what their primary goal is.".to_string()
        }
        GapType::MissingSuccessMetric => {
            "Ask what success looks like — quantitative if possible.".to_string()
        }
        GapType::MissingConstraint => {
            if body_lower.contains("data") || body_lower.contains("storage") {
                "User mentioned data. Ask about volume, cost, or retention constraints.".to_string()
            } else if body_lower.contains("latency") || body_lower.contains("performance") || body_lower.contains("fast") {
                "User mentioned performance. Ask for specific P99/throughput targets.".to_string()
            } else if body_lower.contains("compliance") || body_lower.contains("gdpr") || body_lower.contains("pii") {
                "User mentioned compliance. Ask about specific regulatory requirements.".to_string()
            } else {
                "Ask about technical, business, or regulatory constraints.".to_string()
            }
        }
        GapType::MissingRisk => {
            if body_lower.contains("migration") || body_lower.contains("legacy") {
                "User mentioned migration/legacy. Ask about backward-compatibility risks.".to_string()
            } else if body_lower.contains("scale") || body_lower.contains("volume") {
                "User mentioned scale. Ask about capacity and failure-mode risks.".to_string()
            } else {
                "Ask what could go wrong and what the biggest unknowns are.".to_string()
            }
        }
        GapType::NoTechnicalDecision => {
            "Present 2-3 architectural options and ask which direction to go.".to_string()
        }
        GapType::MissingComponent => {
            "Ask what existing systems or modules this interacts with.".to_string()
        }
        GapType::MissingApiSurface => {
            "Ask if this exposes any APIs or interfaces for other systems.".to_string()
        }
        GapType::NoTaskDecomposition => {
            "Ask the user to break this into concrete implementation tasks.".to_string()
        }
        GapType::UnresolvedQuestion => {
            "Ask if there are open questions that need answers before implementation.".to_string()
        }
        GapType::UnclearProblemStatement => {
            "Ask what problem this solves and what happens if it's not addressed.".to_string()
        }
        GapType::MissingDependency => {
            "Ask about external dependencies or prerequisites.".to_string()
        }
        GapType::MissingOwner => {
            "Ask who owns or is responsible for this.".to_string()
        }
        GapType::InsufficientDetail => {
            "Ask the user to elaborate — the description is too brief.".to_string()
        }
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

    #[test]
    fn test_detect_gaps_with_graph_populates_existing_related() {
        let session = InterviewSession::new(
            "feature",
            "feat-test",
            "# Test Feature\n\nA long enough problem statement that should pass the check.\n",
        );
        let schema = make_schema();

        // Build a graph directory with one persona node
        let tmp = tempfile::tempdir().unwrap();
        let graph_dir = tmp.path().join("graph");
        std::fs::create_dir_all(graph_dir.join("personas")).unwrap();
        std::fs::write(
            graph_dir.join("personas/persona-eng.md"),
            "---\nid: persona-eng\ntype: persona\nstatus: active\nowner: test\nedges: []\n---\n\n# Engineer\n",
        )
        .unwrap();

        let graph = Graph::load_from_directory(&graph_dir, schema.clone()).unwrap();
        let gaps = detect_gaps_with_graph(&session, &schema, Some(&graph));

        let persona_gap = gaps
            .iter()
            .find(|g| g.gap_type == GapType::MissingPersona)
            .expect("Should have a MissingPersona gap");

        assert!(
            persona_gap.existing_related.contains(&"persona-eng".to_string()),
            "existing_related should contain persona-eng, got: {:?}",
            persona_gap.existing_related,
        );
        assert_eq!(
            persona_gap.question_type,
            QuestionType::Closed,
            "1 candidate should yield Closed question type",
        );
    }

    #[test]
    fn test_detect_gaps_with_graph_forced_choice() {
        let session = InterviewSession::new(
            "feature",
            "feat-test",
            "# Test Feature\n\nA long enough problem statement that should pass the check.\n",
        );
        let schema = make_schema();

        // Build a graph directory with two persona nodes
        let tmp = tempfile::tempdir().unwrap();
        let graph_dir = tmp.path().join("graph");
        std::fs::create_dir_all(graph_dir.join("personas")).unwrap();
        std::fs::write(
            graph_dir.join("personas/persona-eng.md"),
            "---\nid: persona-eng\ntype: persona\nstatus: active\nowner: test\nedges: []\n---\n\n# Engineer\n",
        )
        .unwrap();
        std::fs::write(
            graph_dir.join("personas/persona-pm.md"),
            "---\nid: persona-pm\ntype: persona\nstatus: active\nowner: test\nedges: []\n---\n\n# Product Manager\n",
        )
        .unwrap();

        let graph = Graph::load_from_directory(&graph_dir, schema.clone()).unwrap();
        let gaps = detect_gaps_with_graph(&session, &schema, Some(&graph));

        let persona_gap = gaps
            .iter()
            .find(|g| g.gap_type == GapType::MissingPersona)
            .expect("Should have a MissingPersona gap");

        assert_eq!(
            persona_gap.existing_related.len(),
            2,
            "Should have 2 existing related personas",
        );
        assert_eq!(
            persona_gap.question_type,
            QuestionType::ForcedChoice,
            "2 candidates should yield ForcedChoice question type",
        );
    }

    #[test]
    fn test_build_suggested_angle_data_keyword() {
        let angle = build_suggested_angle(&GapType::MissingConstraint, "Store user data in S3");
        let angle_lower = angle.to_lowercase();
        assert!(
            angle_lower.contains("data") || angle_lower.contains("volume"),
            "Angle for body mentioning 'data' should reference data or volume, got: {angle}",
        );
    }

    #[test]
    fn test_build_suggested_angle_latency_keyword() {
        let angle = build_suggested_angle(&GapType::MissingConstraint, "Need fast latency");
        let angle_lower = angle.to_lowercase();
        assert!(
            angle_lower.contains("performance") || angle_lower.contains("p99"),
            "Angle for body mentioning 'latency'/'fast' should reference performance or P99, got: {angle}",
        );
    }
}
