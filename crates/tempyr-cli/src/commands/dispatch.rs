use std::collections::HashSet;

use crate::config::ProjectContext;
use tempyr_core::graph::Graph;
use tempyr_core::node::Node;
use tempyr_core::temporal::TemporalFilter;

use tempyr_render::collector::extract_body_section;

/// Target agent type for dispatch formatting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchTarget {
    /// Claude Code with MCP access to the knowledge graph.
    Claude,
    /// OpenAI Codex CLI with MCP access to the knowledge graph.
    Codex,
}

impl DispatchTarget {
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            _ => anyhow::bail!("Unknown target: '{s}'. Available: claude, codex"),
        }
    }
}

pub fn run(
    ctx: &ProjectContext,
    task_id: &str,
    target: DispatchTarget,
    json_output: bool,
) -> anyhow::Result<()> {
    let graph = Graph::load_from_directory(&ctx.graph_dir, ctx.schema.clone())?;

    let task = graph
        .get_node(task_id)
        .ok_or_else(|| anyhow::anyhow!("Task not found: '{task_id}'"))?;

    if task.node_type() != "task" {
        anyhow::bail!(
            "Node '{task_id}' is type '{}', not 'task'",
            task.node_type()
        );
    }

    let filter = TemporalFilter::current();
    let prompt = build_prompt(&graph, task, target, &filter);

    if json_output {
        let obj = serde_json::json!({
            "task_id": task_id,
            "target": format!("{target:?}").to_lowercase(),
            "prompt": prompt,
        });
        println!("{}", serde_json::to_string_pretty(&obj)?);
    } else {
        print!("{prompt}");
    }

    Ok(())
}

fn build_prompt(
    graph: &Graph,
    task: &Node,
    _target: DispatchTarget,
    filter: &TemporalFilter,
) -> String {
    let mut out = String::new();
    let parent_features = collect_by_edge(graph, task, "child_of", Some("feature"), filter);
    let decisions = collect_decisions(graph, &parent_features, filter);
    let risks = collect_risks(graph, &parent_features, filter);
    let open_questions = collect_open_questions(graph, task, filter);
    let blockers = collect_by_edge(graph, task, "blocked_by", None, filter);
    let components = collect_by_edge(graph, task, "uses", Some("component"), filter);
    let sibling_tasks = collect_siblings(graph, task, &parent_features, filter);

    // ── 1. Title + metadata ──────────────────────────────────
    out.push_str(&format!("# {}\n\n", task.title()));
    out.push_str(&format!("**Task ID**: `{}`", task.id()));
    if let Some(status) = task.status() {
        out.push_str(&format!("  |  **Status**: {status}"));
    }
    out.push('\n');

    let breadcrumb = build_breadcrumb(graph, &parent_features, filter);
    if !breadcrumb.is_empty() {
        out.push_str(&format!("**Scope**: {breadcrumb}\n"));
    }
    out.push('\n');

    // ── 2. Directive ─────────────────────────────────────────
    let deliverables = extract_body_section(&task.body, "Deliverables");
    let deliverable_count = deliverables
        .as_ref()
        .map_or(0, |d| d.lines().filter(|l| l.starts_with("- ")).count());
    let is_complex = deliverable_count >= 4 || !blockers.is_empty();

    if is_complex {
        out.push_str(concat!(
            "This is a multi-part task. Before writing code, read all sections below ",
            "and outline your implementation plan. Then implement incrementally, ",
            "running tests after each significant change.\n\n",
        ));
    } else {
        out.push_str("Implement the deliverables below.\n\n");
    }

    // ── 3. Deliverables (extracted or full body) ─────────────
    if let Some(ref d) = deliverables {
        out.push_str("## Deliverables\n\n");
        out.push_str(d.trim());
        out.push_str("\n\n");

        let remaining = body_without_sections(&task.body, &["Deliverables"]);
        let remaining = strip_title_heading(&remaining, task.title());
        let remaining = remaining.trim();
        if !remaining.is_empty() {
            out.push_str(remaining);
            out.push_str("\n\n");
        }
    } else {
        let body = strip_title_heading(&task.body, task.title());
        let body = body.trim();
        if !body.is_empty() {
            out.push_str("## Objective\n\n");
            out.push_str(body);
            out.push_str("\n\n");
        }
    }

    // ── 4. Constraints (decisions + negative guardrails) ─────
    // Research: "Supply decisions; let agent supply implementation."
    // Research: "Anti-goals / negative constraints prevent scope creep."
    let has_constraints = !decisions.is_empty() || has_completed_siblings(&sibling_tasks);
    if has_constraints {
        out.push_str("## Constraints\n\n");

        // Decisions are hard constraints — the agent must follow them
        for d in &decisions {
            let status_tag = if d.status() == Some("discussing") {
                " (under discussion)"
            } else {
                ""
            };
            out.push_str(&format!("### {}{}\n\n", d.title(), status_tag));
            let body = strip_title_heading(&d.body, d.title());
            out.push_str(body.trim());
            out.push_str("\n\n");
        }

        // Scope guardrails — what NOT to do
        let completed_siblings: Vec<_> =
            sibling_tasks.iter().filter(|(_, s)| *s == "done").collect();
        if !completed_siblings.is_empty() {
            out.push_str("### Scope\n\n");
            out.push_str("Do not modify code belonging to these completed tasks:\n\n");
            for (t, _) in &completed_siblings {
                out.push_str(&format!("- {} (`{}`)\n", t.title(), t.id()));
            }
            out.push('\n');
        }
    }

    // ── 5. Feature context (compact summaries + IDs) ─────────
    if !parent_features.is_empty() {
        out.push_str("## Context\n\n");
        for feat in &parent_features {
            let is_completed = feat.status() == Some("completed");
            out.push_str(&format!("### {}", feat.title()));
            if let Some(status) = feat.status() {
                out.push_str(&format!(" ({status})"));
            }
            out.push_str(&format!(" `{}`", feat.id()));
            out.push('\n');

            if is_completed {
                out.push_str(&format!("*Completed.* {}\n\n", first_paragraph(&feat.body),));
            } else {
                let para = first_paragraph(&feat.body);
                if !para.is_empty() {
                    out.push_str(&format!("\n{para}\n\n"));
                }
            }
        }
    }

    // ── 6. Blocking dependencies ─────────────────────────────
    if !blockers.is_empty() {
        out.push_str("## Blocked By\n\n");
        for blocker in &blockers {
            let status = blocker.status().unwrap_or("unknown");
            let icon = match status {
                "done" | "decided" | "answered" => "[x]",
                _ => "[ ]",
            };
            out.push_str(&format!(
                "- {icon} **{}** (`{}`) — {status}\n",
                blocker.title(),
                blocker.id()
            ));
        }
        out.push('\n');
    }

    // ── 7. Components ────────────────────────────────────────
    if !components.is_empty() {
        out.push_str("## Components\n\n");
        for c in &components {
            out.push_str(&format!("### {}\n\n", c.title()));
            let body = strip_title_heading(&c.body, c.title());
            out.push_str(body.trim());
            out.push_str("\n\n");
        }
    }

    // ── 8. Warnings (risks + open questions) ─────────────────
    if !risks.is_empty() || !open_questions.is_empty() {
        out.push_str("## Warnings\n\n");
        for r in &risks {
            out.push_str(&format!(
                "- **Risk — {}**: {}\n",
                r.title(),
                first_paragraph(&r.body)
            ));
        }
        for q in &open_questions {
            out.push_str(&format!(
                "- **Open question — {}**: {}\n",
                q.title(),
                first_paragraph(&q.body)
            ));
        }
        out.push('\n');
    }

    // ── 9. Sibling tasks ─────────────────────────────────────
    if !sibling_tasks.is_empty() {
        out.push_str("## Sibling Tasks\n\n");
        for (t, status) in &sibling_tasks {
            let icon = match *status {
                "done" => "[x]",
                "in_progress" => "[~]",
                "blocked" => "[!]",
                "cut" => "[-]",
                _ => "[ ]",
            };
            out.push_str(&format!("- {icon} {} (`{}`)\n", t.title(), t.id()));
        }
        out.push('\n');
    }

    // ── 10. Verification (end of prompt — highest attention) ─
    // Research: "Tell the agent to run tests" is the single highest-leverage improvement.
    // Research: Models attend most to beginning and end of context.
    out.push_str("---\n\n");
    out.push_str("## When Done\n\n");
    out.push_str("- Build and confirm no compile errors\n");
    out.push_str("- Run existing tests — fix any regressions before moving on\n");
    out.push_str("- Write tests for each new behavior\n");
    if deliverables.is_some() {
        out.push_str("- Confirm each deliverable above is addressed\n");
    }
    out.push_str("- Do not refactor, rename, or modify code outside the scope of this task\n");
    out.push('\n');

    // MCP footer — last line, high-attention position
    out.push_str(concat!(
        "Use `graph_get_node <id>` to read any node referenced above in full. ",
        "Use `graph_search <query>` or `graph_context <query>` for broader discovery.\n",
    ));

    out
}

/// Check if any sibling tasks are completed (used for scope guardrails).
fn has_completed_siblings(siblings: &[(&Node, &str)]) -> bool {
    siblings.iter().any(|(_, s)| *s == "done")
}

// ── Helpers ──────────────────────────────────────────────────

/// Build a breadcrumb showing the hierarchical chain: "Epic > Feature" (deduplicated).
/// Shows at most 2 epics and 3 features to keep it concise.
fn build_breadcrumb(graph: &Graph, features: &[&Node], filter: &TemporalFilter) -> String {
    let mut seen_epics: HashSet<String> = HashSet::new();
    let mut epic_names = Vec::new();
    let mut feat_names = Vec::new();

    for feat in features {
        let epics = collect_by_edge(graph, feat, "child_of", Some("epic"), filter);
        for epic in &epics {
            if seen_epics.insert(epic.id().to_string()) {
                epic_names.push(epic.title().to_string());
            }
        }
        feat_names.push(feat.title().to_string());
    }

    // Cap to avoid breadcrumb explosion
    epic_names.truncate(2);
    feat_names.truncate(3);

    let mut parts = epic_names;
    parts.extend(feat_names);
    if features.len() > 3 {
        parts.push(format!("(+{} more)", features.len() - 3));
    }

    parts.join(" > ")
}

/// Collect decided/discussing decisions from parent features (deduplicated).
fn collect_decisions<'a>(
    graph: &'a Graph,
    features: &[&Node],
    filter: &TemporalFilter,
) -> Vec<&'a Node> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for feat in features {
        for d in collect_by_edge(graph, feat, "depends_on", Some("decision"), filter) {
            if (d.status() == Some("decided") || d.status() == Some("discussing"))
                && seen.insert(d.id().to_string())
            {
                result.push(d);
            }
        }
    }
    result
}

/// Collect unmitigated risks from parent features (deduplicated).
fn collect_risks<'a>(
    graph: &'a Graph,
    features: &[&Node],
    filter: &TemporalFilter,
) -> Vec<&'a Node> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for feat in features {
        for r in collect_by_edge(graph, feat, "has_risk", Some("risk"), filter) {
            if r.status() != Some("mitigated") && seen.insert(r.id().to_string()) {
                result.push(r);
            }
        }
    }
    result
}

/// Collect open questions from the task.
fn collect_open_questions<'a>(
    graph: &'a Graph,
    task: &Node,
    filter: &TemporalFilter,
) -> Vec<&'a Node> {
    collect_by_edge(graph, task, "has_question", Some("open_question"), filter)
        .into_iter()
        .filter(|q| q.status() == Some("open"))
        .collect()
}

/// Collect sibling tasks (other tasks under the same parent features), sorted by status.
fn collect_siblings<'a>(
    graph: &'a Graph,
    task: &Node,
    features: &[&Node],
    filter: &TemporalFilter,
) -> Vec<(&'a Node, &'a str)> {
    let status_order = |s: &str| match s {
        "done" => 0,
        "in_progress" => 1,
        "backlog" => 2,
        "blocked" => 3,
        "cut" => 4,
        _ => 5,
    };

    let mut seen = HashSet::new();
    let mut result = Vec::new();
    seen.insert(task.id().to_string());

    for feat in features {
        for t in collect_by_edge(graph, feat, "decomposes_to", Some("task"), filter) {
            if seen.insert(t.id().to_string()) {
                let status = t.status().unwrap_or("unknown");
                result.push((t, status));
            }
        }
    }

    result.sort_by_key(|(_, s)| status_order(s));
    result
}

/// Collect nodes reachable by a single edge type from the given node.
fn collect_by_edge<'a>(
    graph: &'a Graph,
    node: &Node,
    edge_type: &str,
    target_type: Option<&str>,
    filter: &TemporalFilter,
) -> Vec<&'a Node> {
    use tempyr_core::temporal::{filter_edges, is_node_visible};

    let visible = filter_edges(node.edges(), filter);
    let mut result = Vec::new();

    for edge in visible {
        if edge.edge_type != edge_type {
            continue;
        }
        let Some(target) = graph.get_node(&edge.target) else {
            continue;
        };
        if let Some(tt) = target_type
            && target.node_type() != tt
        {
            continue;
        }
        if !is_node_visible(target, filter) {
            continue;
        }
        result.push(target);
    }

    result
}

/// Strip the `# Title` heading from a markdown body if it matches the node title.
fn strip_title_heading(body: &str, title: &str) -> String {
    let mut lines = body.lines().peekable();
    // Skip leading blank lines
    while lines.peek().is_some_and(|l| l.trim().is_empty()) {
        lines.next();
    }
    // If the first non-blank line is `# <title>`, skip it
    if let Some(first) = lines.peek() {
        let heading_text = first.trim_start_matches('#').trim();
        if heading_text == title {
            lines.next();
        }
    }
    lines.collect::<Vec<_>>().join("\n")
}

/// Remove named sections from a markdown body, keeping everything else.
fn body_without_sections(body: &str, sections_to_remove: &[&str]) -> String {
    let mut result = Vec::new();
    let mut skip = false;

    for line in body.lines() {
        if line.starts_with("## ") {
            let heading = line.trim_start_matches('#').trim();
            skip = sections_to_remove.contains(&heading);
        }
        if !skip {
            result.push(line);
        }
    }

    result.join("\n")
}

/// Extract the first non-empty paragraph from a markdown body.
fn first_paragraph(body: &str) -> String {
    let trimmed = body.trim();
    let content: String = trimmed
        .lines()
        .skip_while(|l| l.starts_with('#') || l.trim().is_empty())
        .take_while(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    if content.len() > 200 {
        format!("{}...", &content[..197])
    } else {
        content
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use tempyr_core::graph::Graph;
    use tempyr_core::node::parse_node;
    use tempyr_core::schema::Schema;

    fn make_schema() -> Schema {
        let schema_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("schema/default-schema.toml");
        Schema::load(&schema_path).unwrap()
    }

    fn build_dispatch_graph() -> Graph {
        let mut graph = Graph::new(make_schema());

        let epic = r#"---
id: epic-vs
type: epic
status: active
owner: caleb
edges:
  - target: feat-chain
    type: parent_of
---
# Vertical Slice

Build the core caravan escort loop.
"#;

        let feat = r#"---
id: feat-chain
type: feature
status: active
owner: caleb
edges:
  - target: epic-vs
    type: child_of
  - target: task-physics
    type: decomposes_to
  - target: task-joints
    type: decomposes_to
  - target: decision-joint-type
    type: depends_on
  - target: risk-stability
    type: has_risk
---
# Caravan Chain

Physics-driven wagon chain with spring joints.
"#;

        let task_physics = r#"---
id: task-physics
type: task
status: in_progress
edges:
  - target: feat-chain
    type: child_of
  - target: comp-rapier
    type: uses
---
# Physics Foundation

## Deliverables
- Wagon rigid bodies with damping
- Chain joint between wagons
- Oxen kinematic puller

## Dependencies
None.
"#;

        let task_joints = r#"---
id: task-joints
type: task
status: blocked
edges:
  - target: feat-chain
    type: child_of
  - target: task-physics
    type: blocked_by
---
# Joint Tuning

Tune spring/damper parameters for stability.
"#;

        let decision = r#"---
id: decision-joint-type
type: decision
status: decided
edges:
  - target: feat-chain
    type: decision_for
---
# Joint Type: Spherical

Use SphericalJoint for inter-wagon coupling. Permits yaw/pitch/roll, prevents translation.
"#;

        let risk = r#"---
id: risk-stability
type: risk
status: identified
edges:
  - target: feat-chain
    type: risk_for
---
# Chain Physics Stability

Joint constraints may fight at high speeds, causing jitter or explosion.
"#;

        let comp = r#"---
id: comp-rapier
type: component
status: active
edges:
  - target: feat-chain
    type: used_by
---
# Rapier Physics

bevy_rapier3d physics engine integration.
"#;

        graph.add_node(parse_node(epic, PathBuf::from("e.md")).unwrap());
        graph.add_node(parse_node(feat, PathBuf::from("f.md")).unwrap());
        graph.add_node(parse_node(task_physics, PathBuf::from("tp.md")).unwrap());
        graph.add_node(parse_node(task_joints, PathBuf::from("tj.md")).unwrap());
        graph.add_node(parse_node(decision, PathBuf::from("d.md")).unwrap());
        graph.add_node(parse_node(risk, PathBuf::from("r.md")).unwrap());
        graph.add_node(parse_node(comp, PathBuf::from("c.md")).unwrap());

        graph
    }

    #[test]
    fn test_dispatch_claude_prompt() {
        let graph = build_dispatch_graph();
        let task = graph.get_node("task-physics").unwrap();
        let filter = TemporalFilter::current();

        let prompt = build_prompt(&graph, task, DispatchTarget::Claude, &filter);

        // Has title and metadata
        assert!(prompt.contains("# Physics Foundation"));
        assert!(prompt.contains("**Task ID**: `task-physics`"));

        // Has breadcrumb scope
        assert!(prompt.contains("**Scope**:"));
        assert!(prompt.contains("Vertical Slice"));
        assert!(prompt.contains("Caravan Chain"));

        // Has deliverables extracted
        assert!(prompt.contains("## Deliverables"));
        assert!(prompt.contains("Wagon rigid bodies"));

        // Has constraints with decisions
        assert!(prompt.contains("## Constraints"));
        assert!(prompt.contains("SphericalJoint"));

        // Has feature context
        assert!(prompt.contains("## Context"));
        assert!(prompt.contains("Caravan Chain"));

        // Has components
        assert!(prompt.contains("## Components"));
        assert!(prompt.contains("Rapier Physics"));

        // Has warnings (risks)
        assert!(prompt.contains("## Warnings"));
        assert!(prompt.contains("jitter"));

        // Has sibling tasks
        assert!(prompt.contains("## Sibling Tasks"));
        assert!(prompt.contains("Joint Tuning"));

        // Has verification section (at end — "lost in the middle" research)
        assert!(prompt.contains("## When Done"));
        assert!(prompt.contains("Confirm each deliverable"));
        // Has scope guardrail in verification
        assert!(prompt.contains("Do not refactor"));

        // Has MCP footer
        assert!(prompt.contains("graph_get_node"));
    }

    #[test]
    fn test_dispatch_codex_same_as_claude() {
        let graph = build_dispatch_graph();
        let task = graph.get_node("task-physics").unwrap();
        let filter = TemporalFilter::current();

        let claude = build_prompt(&graph, task, DispatchTarget::Claude, &filter);
        let codex = build_prompt(&graph, task, DispatchTarget::Codex, &filter);

        // Both targets produce the same prompt — both have MCP access
        assert_eq!(claude, codex);
        assert!(codex.contains("graph_get_node"));
    }

    #[test]
    fn test_dispatch_blocked_task_gets_plan_directive() {
        let graph = build_dispatch_graph();
        let task = graph.get_node("task-joints").unwrap();
        let filter = TemporalFilter::current();

        let prompt = build_prompt(&graph, task, DispatchTarget::Claude, &filter);

        // Blocked task triggers the "multi-part" plan-first directive
        assert!(prompt.contains("outline your implementation plan"));
        assert!(prompt.contains("## Blocked By"));
        assert!(prompt.contains("Physics Foundation"));
        assert!(prompt.contains("[ ]")); // in_progress blocker is not done
    }

    #[test]
    fn test_dispatch_scope_guardrails() {
        let mut graph = build_dispatch_graph();

        // Mark task-joints as done so it becomes a completed sibling
        let task_joints_done = r#"---
id: task-joints
type: task
status: done
edges:
  - target: feat-chain
    type: child_of
  - target: task-physics
    type: blocked_by
---
# Joint Tuning

Tune spring/damper parameters for stability.
"#;
        graph.add_node(parse_node(task_joints_done, PathBuf::from("tj.md")).unwrap());

        let task = graph.get_node("task-physics").unwrap();
        let filter = TemporalFilter::current();
        let prompt = build_prompt(&graph, task, DispatchTarget::Claude, &filter);

        // Should have scope guardrail for the completed sibling
        assert!(prompt.contains("Do not modify code belonging to these completed tasks"));
        assert!(prompt.contains("Joint Tuning"));
    }

    #[test]
    fn test_dispatch_completed_feature_compact() {
        let mut graph = build_dispatch_graph();

        // Add a completed feature as parent of a new task
        let feat_done = r#"---
id: feat-done
type: feature
status: completed
owner: caleb
edges:
  - target: task-after
    type: decomposes_to
---
# Completed Feature

This feature has a very long body that should NOT be included in full.

It has many paragraphs of detailed specification.

Paragraph after paragraph of content that would pollute context.
"#;
        let task_after = r#"---
id: task-after
type: task
status: backlog
edges:
  - target: feat-done
    type: child_of
---
# After Task

Do something after the completed feature.
"#;

        graph.add_node(parse_node(feat_done, PathBuf::from("fd.md")).unwrap());
        graph.add_node(parse_node(task_after, PathBuf::from("ta.md")).unwrap());

        let task = graph.get_node("task-after").unwrap();
        let filter = TemporalFilter::current();
        let prompt = build_prompt(&graph, task, DispatchTarget::Claude, &filter);

        // Should have compact completed feature reference
        assert!(prompt.contains("*Completed.*"));
        // Should NOT have the full body
        assert!(!prompt.contains("pollute context"));
    }

    #[test]
    fn test_strip_title_heading() {
        assert_eq!(
            strip_title_heading("# My Title\n\nBody content.", "My Title").trim(),
            "Body content."
        );
        assert_eq!(
            strip_title_heading("Body without heading.", "Missing").trim(),
            "Body without heading."
        );
    }

    #[test]
    fn test_first_paragraph() {
        assert_eq!(
            first_paragraph("# Heading\n\nFirst paragraph content.\n\nSecond paragraph."),
            "First paragraph content."
        );
        assert_eq!(
            first_paragraph("Inline content immediately."),
            "Inline content immediately."
        );
    }
}
