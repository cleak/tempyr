use std::collections::HashMap;

use graphforge_core::schema::Schema;

use crate::gaps::{detect_gaps, Gap};
use crate::phases;
use crate::session::{
    EdgeSource, InterviewSession, TentativeEdge, TentativeNode,
};
use crate::Result;

/// The result of starting an interview.
pub struct InterviewStartResult {
    pub session: InterviewSession,
    pub questions: Vec<Gap>,
    pub progress: String,
}

/// The result of processing an answer / re-analyzing gaps.
pub struct InterviewUpdateResult {
    pub filled_gaps: Vec<String>,
    pub questions: Vec<Gap>,
    pub phase_changed: bool,
    pub progress: String,
}

/// Start a new interview from a brain dump.
///
/// Creates a session with the brain dump as the root node body, runs gap
/// analysis, and returns the initial questions. Claude Code (the LLM) is
/// responsible for interpreting the brain dump and calling add_node/add_edge
/// to populate the session with extracted entities.
pub fn interview_start(
    brain_dump: &str,
    root_type: &str,
    schema: &Schema,
    existing_node_ids: &[String],
) -> Result<InterviewStartResult> {
    let slug = slugify(brain_dump);
    let title = first_line(brain_dump);

    let body = format!("# {title}\n\n## Problem\n\n{brain_dump}\n");

    let mut session = InterviewSession::new(root_type, &slug, &body);

    // Record existing nodes as graph context
    for id in existing_node_ids {
        session.graph_context.push(id.clone());
    }

    // Try to advance from Discovery if we have context
    phases::try_advance_phase(&mut session);

    // Detect gaps
    let gaps = detect_gaps(&session, schema);
    let questions: Vec<Gap> = gaps.iter().take(3).cloned().collect();
    session.remaining_gaps = gaps;

    let progress = phases::progress_summary(&session);

    Ok(InterviewStartResult {
        session,
        questions,
        progress,
    })
}

/// Add a tentative node to a session and re-analyze gaps.
///
/// Called by Claude Code after it extracts entities from the user's answers.
pub fn add_proposed_node(
    session: &mut InterviewSession,
    id: &str,
    node_type: &str,
    status: &str,
    body: &str,
    confidence: f32,
    schema: &Schema,
) -> InterviewUpdateResult {
    session.add_tentative_node(TentativeNode {
        id: id.to_string(),
        node_type: node_type.to_string(),
        status: status.to_string(),
        fields: HashMap::new(),
        body: body.to_string(),
        confidence,
        source_qa: vec![session.answered.len()],
    });

    reanalyze(session, schema)
}

/// Add a tentative edge to a session and re-analyze gaps.
pub fn add_proposed_edge(
    session: &mut InterviewSession,
    source: &str,
    target: &str,
    edge_type: &str,
    schema: &Schema,
) -> InterviewUpdateResult {
    session.add_tentative_edge(TentativeEdge {
        source: source.to_string(),
        target: target.to_string(),
        edge_type: edge_type.to_string(),
        source_type: EdgeSource::ExplicitFromAnswer,
    });

    reanalyze(session, schema)
}

/// Record a question-answer exchange and re-analyze gaps.
///
/// The answer text is stored for provenance. Claude Code should call
/// add_proposed_node/add_proposed_edge separately for any entities it
/// extracts from the answer.
pub fn record_answer(
    session: &mut InterviewSession,
    question: &str,
    answer: &str,
    proposed_node_ids: Vec<String>,
    schema: &Schema,
) -> InterviewUpdateResult {
    session.record_answer(question, answer, proposed_node_ids);
    reanalyze(session, schema)
}

/// Re-run gap analysis and phase transition checks.
fn reanalyze(session: &mut InterviewSession, schema: &Schema) -> InterviewUpdateResult {
    let phase_changed = phases::try_advance_phase(session);

    let gaps = detect_gaps(session, schema);
    let filled_gaps: Vec<String> = session
        .remaining_gaps
        .iter()
        .filter(|old| {
            !gaps.iter().any(|new| {
                new.gap_type == old.gap_type && new.node_type_needed == old.node_type_needed
            })
        })
        .map(|g| format!("{}: {}", g.node_type_needed, g.context))
        .collect();

    let questions: Vec<Gap> = gaps.iter().take(3).cloned().collect();
    session.remaining_gaps = gaps;

    let progress = phases::progress_summary(session);

    InterviewUpdateResult {
        filled_gaps,
        questions,
        phase_changed,
        progress,
    }
}

/// Convert text to a kebab-case slug.
pub fn slugify(text: &str) -> String {
    let first = first_line(text);
    first
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        .chars()
        .take(50)
        .collect()
}

/// Get the first non-empty line of text, stripping markdown heading prefix.
fn first_line(text: &str) -> &str {
    text.lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .map(|l| l.strip_prefix("# ").unwrap_or(l))
        .unwrap_or("untitled")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phases::InterviewPhase;
    use graphforge_core::schema::Schema;
    use std::path::Path;

    fn make_schema() -> Schema {
        let schema_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("schema/default-schema.toml");
        Schema::load(&schema_path).unwrap()
    }

    #[test]
    fn test_slugify() {
        assert_eq!(slugify("Session Replay for Users"), "session-replay-for-users");
        assert_eq!(slugify("  Hello  World  "), "hello-world");
        assert_eq!(slugify("# My Feature"), "my-feature");
    }

    #[test]
    fn test_first_line() {
        assert_eq!(first_line("# Hello\nWorld"), "Hello");
        assert_eq!(first_line("plain text\nsecond"), "plain text");
        assert_eq!(first_line(""), "untitled");
    }

    #[test]
    fn test_interview_start() {
        let schema = make_schema();
        let result = interview_start(
            "Session replay for debugging funnel drop-offs",
            "feature",
            &schema,
            &[],
        )
        .unwrap();

        assert_eq!(result.session.root_type, "feature");
        assert!(!result.session.root_node.id.is_empty());
        assert!(!result.questions.is_empty());
        assert!(!result.progress.is_empty());
    }

    #[test]
    fn test_interview_start_with_existing_context() {
        let schema = make_schema();
        let existing = vec!["epic-observability".to_string()];
        let result = interview_start(
            "Session replay for debugging funnel drop-offs",
            "feature",
            &schema,
            &existing,
        )
        .unwrap();

        assert!(result.session.graph_context.contains(&"epic-observability".to_string()));
        // Should advance past Discovery since we have context and a body
        assert_eq!(result.session.phase, InterviewPhase::Product);
    }

    #[test]
    fn test_add_proposed_node_fills_gap() {
        let schema = make_schema();
        let mut result = interview_start(
            "Session replay for debugging",
            "feature",
            &schema,
            &["existing-epic".to_string()],
        )
        .unwrap();

        let mut session = result.session;

        // Should have a persona gap
        assert!(session.remaining_gaps.iter().any(|g| g.node_type_needed == "persona"));

        // Add a persona
        let update = add_proposed_node(
            &mut session,
            "persona-eng",
            "persona",
            "",
            "# Platform Engineer\n\nDebugs production issues.\n",
            0.9,
            &schema,
        );

        assert!(session.has_node_of_type("persona"));
        // Persona gap should be resolved after adding edge too
        let root_id = session.root_node.id.clone();
        add_proposed_edge(&mut session, &root_id, "persona-eng", "serves", &schema);
        assert!(!session.remaining_gaps.iter().any(|g| g.node_type_needed == "persona"));
    }

    #[test]
    fn test_add_proposed_edge() {
        let schema = make_schema();
        let result = interview_start("A feature idea", "feature", &schema, &["ctx".to_string()]).unwrap();
        let mut session = result.session;

        let root_id = session.root_node.id.clone();
        session.add_tentative_node(TentativeNode {
            id: "persona-x".to_string(),
            node_type: "persona".to_string(),
            status: "".to_string(),
            fields: HashMap::new(),
            body: "# X\n".to_string(),
            confidence: 0.8,
            source_qa: vec![],
        });

        let update = add_proposed_edge(&mut session, &root_id, "persona-x", "serves", &schema);
        assert!(session.has_edge_type_from_root("serves"));
    }

    #[test]
    fn test_record_answer_and_reanalyze() {
        let schema = make_schema();
        let result = interview_start("A feature", "feature", &schema, &["ctx".to_string()]).unwrap();
        let mut session = result.session;

        let update = record_answer(
            &mut session,
            "Who is the target user?",
            "Platform engineers who debug production issues",
            vec![],
            &schema,
        );

        assert_eq!(session.answered.len(), 1);
        assert!(!update.progress.is_empty());
    }

    #[test]
    fn test_phase_advances_when_conditions_met() {
        let schema = make_schema();
        let result = interview_start(
            "A feature with a long enough problem description for the phase checks to pass",
            "feature",
            &schema,
            &["ctx".to_string()],
        ).unwrap();
        let mut session = result.session;

        // Should be in Product phase (has context + body)
        assert_eq!(session.phase, InterviewPhase::Product);

        // Add persona + edge
        let root_id = session.root_node.id.clone();
        add_proposed_node(&mut session, "persona-eng", "persona", "", "# Eng\n", 0.9, &schema);
        add_proposed_edge(&mut session, &root_id, "persona-eng", "serves", &schema);

        // Add metric — this should trigger Product → Technical since we now have
        // persona + metric + substantive body
        let update = add_proposed_node(&mut session, "metric-mttr", "metric", "proposed", "# Reduce MTTR\n", 0.8, &schema);
        add_proposed_edge(&mut session, &root_id, "metric-mttr", "measured_by", &schema);

        // Should now be in Technical phase (transition happened when metric was added)
        assert!(update.phase_changed);
        assert_eq!(session.phase, InterviewPhase::Technical);
    }
}
