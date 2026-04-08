use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use tempyr_core::graph::Graph;
use tempyr_core::id;
use tempyr_core::ops;
use tempyr_core::schema::Schema;
use tempyr_core::temporal::TemporalFilter;
use tempyr_core::traverse::bfs;
use tempyr_core::validate::validate_graph;
use tempyr_index::fts::MetadataFilter;
use tempyr_index::hybrid::{RetrievalConfig, hybrid_retrieve};
use tempyr_index::indexer::Index;
use tempyr_interview::gaps::next_questions;
use tempyr_interview::proposer;
use tempyr_interview::session::{
    EdgeSource, ExistingNodeSummary, InterviewSession, NodePatch, TentativeEdge, TentativeNode,
};
use tempyr_linear::client::LinearClient;
use tempyr_linear::config::LinearConfig;
use tempyr_linear::mapping::StatusMapper;
use tempyr_linear::queries::WorkflowState;
use tempyr_linear::state::SyncState;

// Parameter structs

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GraphSearchParams {
    /// Search terms (matched against body text, titles, tags)
    pub query: String,
    /// Maximum results to return (default: 10)
    #[schemars(default = "default_10")]
    pub max_results: Option<u64>,
    /// Filter by node type (e.g. task, feature, decision)
    pub node_type: Option<String>,
    /// Filter by status (e.g. backlog, in_progress, done, draft, active)
    pub status: Option<String>,
    /// Filter by owner
    pub owner: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GraphListParams {
    /// Filter by node type (e.g. task, feature, decision, epic)
    pub node_type: Option<String>,
    /// Filter by status (e.g. backlog, in_progress, done, draft, active, blocked)
    pub status: Option<String>,
    /// Filter by owner
    pub owner: Option<String>,
    /// Maximum results to return (default: 50)
    #[schemars(default = "default_50")]
    pub max_results: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GraphContextParams {
    /// Semantic query for hybrid retrieval
    pub query: String,
    /// Root node for structural traversal
    pub root_node: Option<String>,
    /// Token budget for context window (default: 8000)
    #[schemars(default = "default_8000")]
    pub token_budget: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GraphTraverseParams {
    /// Node ID to traverse from
    pub node_id: String,
    /// Maximum traversal depth (default: 2)
    #[schemars(default = "default_2")]
    pub depth: Option<u64>,
    /// Filter by edge types
    #[allow(dead_code)]
    pub edge_types: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GraphGetNodeParams {
    /// Full node ID (e.g. 'session-replay-a1b2c3') or 6-char suffix
    pub node_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GraphAddNodeParams {
    /// Human-readable kebab-case slug (e.g. 'session-replay'). Do NOT include type prefixes.
    pub slug: String,
    /// Node type (e.g. feature, epic, task, decision, constraint, persona, metric, risk)
    pub node_type: String,
    /// Markdown body content
    pub body: String,
    /// Node status
    pub status: Option<String>,
    /// Node owner
    pub owner: Option<String>,
    /// Tags
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GraphUpdateNodeParams {
    /// Full node ID or 6-char suffix
    pub node_id: String,
    /// New markdown body (replaces entire body)
    pub body: Option<String>,
    /// New status value
    pub status: Option<String>,
    /// New owner
    pub owner: Option<String>,
    /// New tags (replaces all tags)
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GraphAddEdgeParams {
    /// Source node ID (full ID or 6-char suffix)
    pub source: String,
    /// Target node ID (full ID or 6-char suffix)
    pub target: String,
    /// Edge type from source's perspective (e.g. 'child_of', 'has_risk', 'depends_on')
    pub edge_type: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GraphRenderParams {
    /// Template name: 'prd', 'tdd', or 'task-prompt'
    pub template: String,
    /// Root node ID to render from
    pub root_node: String,
    /// ISO timestamp for point-in-time rendering
    #[allow(dead_code)]
    pub as_of: Option<String>,
    /// Include historical/superseded nodes
    pub include_history: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct InterviewStartParams {
    /// The user's raw input describing what they want to build/plan
    pub brain_dump: String,
    /// Type of root node: feature (default), epic, or component
    #[schemars(default = "default_feature")]
    pub root_type: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct InterviewAnswerParams {
    /// Interview session ID
    pub session_id: String,
    /// The user's answer text
    pub answer: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct InterviewSessionParams {
    /// Interview session ID
    pub session_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct InterviewAdjustParams {
    /// Interview session ID
    pub session_id: String,
    /// ID of the tentative node to modify
    pub node_id: String,
    /// New body content
    pub body: Option<String>,
    /// New status
    pub status: Option<String>,
    /// New ID (rename)
    pub new_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct InterviewAddNodeParams {
    /// Interview session ID
    pub session_id: String,
    /// Human-readable kebab-case slug (no type prefix). A 6-char suffix is appended automatically.
    pub slug: String,
    /// Node type: feature, epic, task, decision, constraint, persona, metric, risk, open_question, component, api_surface, insight, note
    pub node_type: String,
    /// Markdown body content
    pub body: String,
    /// Node status (default: draft)
    #[schemars(default = "default_draft")]
    pub status: Option<String>,
    /// Extraction confidence 0.0-1.0 (0.9+ explicit, 0.6-0.8 inferred, default: 0.9)
    #[schemars(default = "default_confidence")]
    pub confidence: Option<f64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct InterviewAddEdgeParams {
    /// Interview session ID
    pub session_id: String,
    /// Source node ID (full ID or 6-char suffix)
    pub source: String,
    /// Target node ID (full ID or 6-char suffix)
    pub target: String,
    /// Edge type: child_of, serves, measured_by, constrained_by, depends_on, has_risk, decomposes_to, uses, has_question, etc.
    pub edge_type: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LinearPushParams {
    /// Specific node ID to push (omit for all syncable nodes)
    pub node_id: Option<String>,
    /// Preview what would happen without making changes
    pub dry_run: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LinearDryRunParams {
    /// Preview what would happen without making changes
    pub dry_run: Option<bool>,
}

// Schema defaults

fn default_2() -> u64 {
    2
}
fn default_10() -> u64 {
    10
}
fn default_50() -> u64 {
    50
}
fn default_8000() -> u64 {
    8000
}
fn default_feature() -> String {
    "feature".to_string()
}
fn default_draft() -> String {
    "draft".to_string()
}
fn default_confidence() -> f64 {
    0.9
}

// Helpers

fn find_project() -> Result<(PathBuf, PathBuf, Schema), String> {
    let root = tempyr_core::project::find_project_root().ok_or_else(|| {
        "Not a tempyr project (no .tempyr/ or .tempyr-redirect found)".to_string()
    })?;
    let gf_dir = root.join(".tempyr");
    let schema_path = gf_dir.join("schema.toml");
    let schema = Schema::load(&schema_path).map_err(|e| e.to_string())?;
    let graph_dir = root.join("graph");
    Ok((graph_dir, gf_dir, schema))
}

fn index_layout(
    graph_dir: &Path,
    gf_dir: &Path,
) -> Result<tempyr_core::project::IndexLayout, String> {
    let root = graph_dir
        .parent()
        .ok_or_else(|| "Failed to resolve project root from graph dir".to_string())?;
    tempyr_core::project::IndexLayout::resolve(root, graph_dir, gf_dir).map_err(|e| e.to_string())
}

fn open_optional_index(graph_dir: &Path, gf_dir: &Path) -> Result<Option<Index>, String> {
    let layout = index_layout(graph_dir, gf_dir)?;
    match layout.current_index_path().map_err(|e| e.to_string())? {
        Some(path) => Index::open(&path)
            .map(Some)
            .map_err(|e| format!("Index: {e}")),
        None => Ok(None),
    }
}

fn refresh_index_for_current_snapshot(
    graph_dir: &Path,
    gf_dir: &Path,
    schema: &Schema,
) -> Result<(), String> {
    let layout = index_layout(graph_dir, gf_dir)?;
    let index_path = layout
        .ensure_active_index_seeded()
        .map_err(|e| e.to_string())?;
    let graph = Graph::load_from_directory(graph_dir, schema.clone()).map_err(|e| e.to_string())?;

    if index_path.exists() {
        let index = Index::open(&index_path).map_err(|e| format!("Index: {e}"))?;
        index
            .incremental_update(&graph)
            .map_err(|e| e.to_string())?;
    } else {
        if let Some(parent) = index_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let index = Index::create(&index_path).map_err(|e| format!("Index: {e}"))?;
        index.rebuild(&graph).map_err(|e| e.to_string())?;
    }

    layout
        .write_active_snapshot_key()
        .map_err(|e| e.to_string())?;
    layout
        .publish_active_snapshot()
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn index_refresh_warning(graph_dir: &Path, gf_dir: &Path, schema: &Schema) -> Option<String> {
    refresh_index_for_current_snapshot(graph_dir, gf_dir, schema)
        .err()
        .map(|e| format!("Index update failed (run 'tempyr index rebuild'): {e}"))
}

fn sessions_dir(gf_dir: &Path) -> PathBuf {
    gf_dir.join("sessions")
}

fn session_state_json(session: &InterviewSession, _schema: &Schema) -> Value {
    let questions = next_questions(session, 3);
    let progress = proposer::compute_progress(session);

    let mut nodes_by_type: HashMap<String, Vec<Value>> = HashMap::new();
    nodes_by_type
        .entry(session.root_node.node_type.clone())
        .or_default()
        .push(json!({
            "id": session.root_node.id,
            "status": session.root_node.status,
            "body_preview": session.root_node.body.lines().take(3).collect::<Vec<_>>().join("\n"),
            "confidence": session.root_node.confidence,
        }));
    for node in &session.tentative_nodes {
        nodes_by_type
            .entry(node.node_type.clone())
            .or_default()
            .push(json!({
                "id": node.id,
                "status": node.status,
                "body_preview": node.body.lines().take(3).collect::<Vec<_>>().join("\n"),
                "confidence": node.confidence,
            }));
    }

    json!({
        "session_id": session.id,
        "root_node": {
            "id": session.root_node.id,
            "node_type": session.root_node.node_type,
            "status": session.root_node.status,
            "body": session.root_node.body,
        },
        "tentative_nodes_by_type": nodes_by_type,
        "tentative_edges": session.tentative_edges.iter().map(|e| json!({
            "source": e.source,
            "target": e.target,
            "edge_type": e.edge_type,
        })).collect::<Vec<_>>(),
        "graph_context": session.graph_context_rich,
        "remaining_gaps": session.remaining_gaps,
        "next_questions": questions,
        "qa_history": session.answered.iter().map(|qa| json!({
            "question": qa.question,
            "answer": qa.answer,
            "phase": qa.phase,
            "nodes_proposed": qa.nodes_proposed,
        })).collect::<Vec<_>>(),
        "phase": session.phase,
        "progress": {
            "filled": progress.filled,
            "total": progress.total,
            "percentage": progress.percentage,
        },
    })
}

fn resolve_interview_node_id(
    session: &InterviewSession,
    graph_dir: &Path,
    input: &str,
) -> Result<String, String> {
    let suffix_pattern = format!("-{input}");
    let mut matches = Vec::new();

    let root = &session.root_node;
    if root.id == input || root.id.ends_with(&suffix_pattern) {
        matches.push(root.id.clone());
    }
    for node in &session.tentative_nodes {
        if node.id == input || node.id.ends_with(&suffix_pattern) {
            matches.push(node.id.clone());
        }
    }

    match matches.len() {
        1 => Ok(matches.into_iter().next().unwrap()),
        0 => ops::resolve_node_id(graph_dir, input)
            .map_err(|e| format!("Node '{input}' not found in session or on disk: {e}")),
        _ => Err(format!(
            "Ambiguous node ID '{input}' matches multiple tentative nodes: {}",
            matches.join(", ")
        )),
    }
}

fn build_linear_deps() -> Result<(LinearClient, LinearConfig, PathBuf, PathBuf, Schema), String> {
    let (graph_dir, gf_dir, schema) = find_project()?;
    let client = LinearClient::from_env().map_err(|e| e.to_string())?;
    let config = LinearConfig::load(&gf_dir).map_err(|e| e.to_string())?;
    Ok((client, config, gf_dir, graph_dir, schema))
}

fn build_status_mapper_from_config(config: &LinearConfig) -> StatusMapper {
    let states: Vec<WorkflowState> = config
        .workflow_states
        .iter()
        .map(|(name, id)| WorkflowState {
            id: id.clone(),
            name: name.clone(),
            state_type: String::new(),
        })
        .collect();
    StatusMapper::new(states)
}

// Server

#[derive(Clone)]
pub struct TempyrServer {
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl TempyrServer {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
    // Graph query tools

    #[tool(
        name = "graph_search",
        description = "Full-text keyword search across all graph nodes. Searches body text, titles, and tags. Optionally filter results by metadata (type, status, owner)."
    )]
    fn graph_search(&self, Parameters(p): Parameters<GraphSearchParams>) -> Result<String, String> {
        let max_results = p.max_results.unwrap_or(10) as usize;
        let (graph_dir, gf_dir, _) = find_project()?;
        let index_path = index_layout(&graph_dir, &gf_dir)?
            .current_index_path()
            .map_err(|e| e.to_string())?
            .ok_or_else(|| {
                "Index not found for current graph snapshot. Run `tempyr index rebuild` first."
                    .to_string()
            })?;
        let index = Index::open(&index_path).map_err(|e| format!("Index: {e}"))?;

        let filter = MetadataFilter {
            node_type: p.node_type.as_deref(),
            status: p.status.as_deref(),
            owner: p.owner.as_deref(),
        };
        let results = index
            .search_fts_with_metadata(&p.query, &filter, max_results)
            .map_err(|e| e.to_string())?;

        let output: Vec<Value> = results
            .iter()
            .map(|r| {
                json!({
                    "node_id": r.node_id,
                    "title": r.title,
                    "node_type": r.node_type,
                    "status": r.status,
                    "snippet": r.snippet
                })
            })
            .collect();

        serde_json::to_string_pretty(&output).map_err(|e| e.to_string())
    }

    #[tool(
        name = "graph_list",
        description = "List graph nodes by metadata filters. Unlike graph_search, no search query is needed - filters on type, status, and owner directly."
    )]
    fn graph_list(&self, Parameters(p): Parameters<GraphListParams>) -> Result<String, String> {
        let max_results = p.max_results.unwrap_or(50) as usize;
        let (graph_dir, gf_dir, _) = find_project()?;
        let index_path = index_layout(&graph_dir, &gf_dir)?
            .current_index_path()
            .map_err(|e| e.to_string())?
            .ok_or_else(|| {
                "Index not found for current graph snapshot. Run `tempyr index rebuild` first."
                    .to_string()
            })?;
        let index = Index::open(&index_path).map_err(|e| format!("Index: {e}"))?;

        let filter = MetadataFilter {
            node_type: p.node_type.as_deref(),
            status: p.status.as_deref(),
            owner: p.owner.as_deref(),
        };
        let results = index
            .query_by_metadata(&filter, max_results)
            .map_err(|e| e.to_string())?;

        let output: Vec<Value> = results
            .iter()
            .map(|r| {
                json!({
                    "node_id": r.node_id,
                    "title": r.title,
                    "node_type": r.node_type,
                    "status": r.status,
                    "owner": r.owner
                })
            })
            .collect();

        serde_json::to_string_pretty(&output).map_err(|e| e.to_string())
    }

    #[tool(
        name = "graph_context",
        description = "Hybrid retrieval combining structural traversal, keyword search, and semantic search"
    )]
    fn graph_context(
        &self,
        Parameters(p): Parameters<GraphContextParams>,
    ) -> Result<String, String> {
        let budget = p.token_budget.unwrap_or(8000) as usize;
        let (graph_dir, gf_dir, schema) = find_project()?;
        let resolved_root = p
            .root_node
            .as_deref()
            .map(|r| ops::resolve_node_id(&graph_dir, r).map_err(|e| e.to_string()))
            .transpose()?;
        let graph = Graph::load_from_directory(&graph_dir, schema).map_err(|e| e.to_string())?;
        let index_path = index_layout(&graph_dir, &gf_dir)?
            .current_index_path()
            .map_err(|e| e.to_string())?
            .ok_or_else(|| {
                "Index not found for current graph snapshot. Run `tempyr index rebuild` first."
                    .to_string()
            })?;
        let index = Index::open(&index_path).map_err(|e| format!("Index: {e}"))?;

        let config = RetrievalConfig {
            token_budget: budget,
            ..RetrievalConfig::standard()
        };
        let results = hybrid_retrieve(
            &index,
            &graph,
            &p.query,
            resolved_root.as_deref(),
            &config,
            None,
        )
        .map_err(|e| e.to_string())?;

        let mut output = String::new();
        for r in &results {
            if let Some(node) = graph.get_node(&r.node_id) {
                output.push_str(&format!(
                    "### {} ({})\n**Score**: {:.3}\n\n{}\n\n---\n\n",
                    node.title(),
                    node.node_type(),
                    r.combined_score,
                    node.body.trim()
                ));
            }
        }

        Ok(output)
    }

    #[tool(
        name = "graph_traverse",
        description = "Follow edges from a node to find connected nodes"
    )]
    fn graph_traverse(
        &self,
        Parameters(p): Parameters<GraphTraverseParams>,
    ) -> Result<String, String> {
        let depth = p.depth.unwrap_or(2) as usize;
        let (graph_dir, _, schema) = find_project()?;
        let resolved = ops::resolve_node_id(&graph_dir, &p.node_id).map_err(|e| e.to_string())?;
        let graph = Graph::load_from_directory(&graph_dir, schema).map_err(|e| e.to_string())?;

        let results = bfs(&graph, &resolved, depth, None);
        let output: Vec<Value> = results
            .iter()
            .map(|r| {
                let node = graph.get_node(&r.node_id);
                json!({
                    "node_id": r.node_id,
                    "depth": r.depth,
                    "type": node.map(|n| n.node_type()),
                    "title": node.map(|n| n.title()),
                })
            })
            .collect();

        serde_json::to_string_pretty(&output).map_err(|e| e.to_string())
    }

    #[tool(
        name = "graph_get_node",
        description = "Get the full content of a specific node by ID"
    )]
    fn graph_get_node(
        &self,
        Parameters(p): Parameters<GraphGetNodeParams>,
    ) -> Result<String, String> {
        let (graph_dir, _, _) = find_project()?;
        let resolved = ops::resolve_node_id(&graph_dir, &p.node_id).map_err(|e| e.to_string())?;
        let path = ops::find_node_file(&graph_dir, &resolved).map_err(|e| e.to_string())?;
        std::fs::read_to_string(&path).map_err(|e| e.to_string())
    }

    #[tool(
        name = "graph_stats",
        description = "Get graph statistics: node counts by type, edge counts"
    )]
    fn graph_stats(&self) -> Result<String, String> {
        let (graph_dir, _, schema) = find_project()?;
        let graph = Graph::load_from_directory(&graph_dir, schema).map_err(|e| e.to_string())?;

        let mut type_counts: HashMap<String, usize> = HashMap::new();
        for node in graph.nodes.values() {
            *type_counts.entry(node.node_type().to_string()).or_default() += 1;
        }

        serde_json::to_string_pretty(&json!({
            "node_count": graph.node_count(),
            "edge_count": graph.edge_count(),
            "nodes_by_type": type_counts,
        }))
        .map_err(|e| e.to_string())
    }
    // Graph mutation tools

    #[tool(
        name = "graph_add_node",
        description = "Create a new node in the graph. Provide a human-readable slug; the system generates a 6-char suffix to form the full ID (e.g. slug 'session-replay' -> ID 'session-replay-a1b2c3')."
    )]
    fn graph_add_node(
        &self,
        Parameters(p): Parameters<GraphAddNodeParams>,
    ) -> Result<String, String> {
        let (graph_dir, gf_dir, schema) = find_project()?;
        let (generated_id, path) = ops::create_node_file_auto_id(
            &graph_dir,
            &p.slug,
            &p.node_type,
            p.status.as_deref(),
            p.owner.as_deref(),
            p.tags.as_deref(),
            &p.body,
        )
        .map_err(|e| e.to_string())?;

        let mut response = format!("Created node '{generated_id}' at {}", path.display());
        if let Some(warning) = index_refresh_warning(&graph_dir, &gf_dir, &schema) {
            response.push_str(&format!("\nWarning: {}", warning));
        }
        Ok(response)
    }

    #[tool(
        name = "graph_update_node",
        description = "Update an existing node's body, status, owner, or tags. Only provided fields are changed."
    )]
    fn graph_update_node(
        &self,
        Parameters(p): Parameters<GraphUpdateNodeParams>,
    ) -> Result<String, String> {
        let (graph_dir, gf_dir, schema) = find_project()?;
        let resolved = ops::resolve_node_id(&graph_dir, &p.node_id).map_err(|e| e.to_string())?;
        let path = ops::update_node(
            &graph_dir,
            &resolved,
            p.body.as_deref(),
            p.status.as_deref(),
            p.owner.as_deref(),
            p.tags.as_deref(),
            &schema,
        )
        .map_err(|e| e.to_string())?;

        let mut changed: Vec<&str> = Vec::new();
        if p.body.is_some() {
            changed.push("body");
        }
        if p.status.is_some() {
            changed.push("status");
        }
        if p.owner.is_some() {
            changed.push("owner");
        }
        if p.tags.is_some() {
            changed.push("tags");
        }

        let mut response = format!(
            "Updated node '{resolved}' ({}) at {}",
            changed.join(", "),
            path.display()
        );
        if let Some(warning) = index_refresh_warning(&graph_dir, &gf_dir, &schema) {
            response.push_str(&format!("\nWarning: {}", warning));
        }
        Ok(response)
    }

    #[tool(
        name = "graph_add_edge",
        description = "Add a directed edge between two existing nodes. The reverse edge is written automatically."
    )]
    fn graph_add_edge(
        &self,
        Parameters(p): Parameters<GraphAddEdgeParams>,
    ) -> Result<String, String> {
        let (graph_dir, gf_dir, schema) = find_project()?;
        let resolved_source =
            ops::resolve_node_id(&graph_dir, &p.source).map_err(|e| e.to_string())?;
        let resolved_target =
            ops::resolve_node_id(&graph_dir, &p.target).map_err(|e| e.to_string())?;
        ops::add_edge(
            &graph_dir,
            &resolved_source,
            &resolved_target,
            &p.edge_type,
            &schema,
        )
        .map_err(|e| e.to_string())?;

        let reverse = schema.reverse_edge_type(&p.edge_type).unwrap_or("?");
        let mut response = format!(
            "Added edge: {resolved_source} --{}--> {resolved_target} (reverse: {reverse})",
            p.edge_type
        );
        if let Some(warning) = index_refresh_warning(&graph_dir, &gf_dir, &schema) {
            response.push_str(&format!("\nWarning: {}", warning));
        }
        Ok(response)
    }

    #[tool(
        name = "graph_validate",
        description = "Validate graph consistency. Returns any errors or warnings."
    )]
    fn graph_validate(&self) -> Result<String, String> {
        let (graph_dir, _, schema) = find_project()?;
        let graph = Graph::load_from_directory(&graph_dir, schema).map_err(|e| e.to_string())?;
        let issues = validate_graph(&graph);

        if issues.is_empty() {
            Ok(format!(
                "Graph is valid. {} nodes, {} edges.",
                graph.node_count(),
                graph.edge_count()
            ))
        } else {
            let lines: Vec<String> = issues.iter().map(|i| i.to_string()).collect();
            Ok(lines.join("\n"))
        }
    }

    #[tool(
        name = "graph_render",
        description = "Render a document (PRD, TDD) from a root node"
    )]
    fn graph_render(&self, Parameters(p): Parameters<GraphRenderParams>) -> Result<String, String> {
        let (graph_dir, gf_dir, schema) = find_project()?;
        let root_id = ops::resolve_node_id(&graph_dir, &p.root_node).map_err(|e| e.to_string())?;
        let graph = Graph::load_from_directory(&graph_dir, schema).map_err(|e| e.to_string())?;

        let filter = if p.include_history.unwrap_or(false) {
            TemporalFilter::with_history()
        } else {
            TemporalFilter::current()
        };

        let local_path = gf_dir.join("render").join(format!("{}.toml", p.template));
        if local_path.exists() {
            tempyr_render::render(&graph, &local_path, &root_id, &filter).map_err(|e| e.to_string())
        } else {
            let template_toml = match p.template.as_str() {
                "prd" => include_str!("../../../templates/prd.toml"),
                "tdd" => include_str!("../../../templates/tdd.toml"),
                "task-prompt" => include_str!("../../../templates/task-prompt.toml"),
                _ => return Err(format!("Unknown template: '{}'", p.template)),
            };
            tempyr_render::render_from_str(&graph, template_toml, &root_id, &filter)
                .map_err(|e| e.to_string())
        }
    }
    // Interview tools

    #[tool(
        name = "interview_start",
        description = "Start a new interview session from a brain dump or idea description. Returns tentative nodes, existing graph context, gaps to explore, and the first questions to ask."
    )]
    fn interview_start(
        &self,
        Parameters(p): Parameters<InterviewStartParams>,
    ) -> Result<String, String> {
        let root_type = p.root_type.as_deref().unwrap_or("feature");
        let (graph_dir, gf_dir, schema) = find_project()?;
        let sessions = sessions_dir(&gf_dir);
        let graph = Graph::load_from_directory(&graph_dir, schema.clone()).ok();

        let mut existing_ids = Vec::new();
        let mut context_rich = Vec::new();
        if let Some(index) = open_optional_index(&graph_dir, &gf_dir)? {
            let results = index
                .search_fts_filtered(&p.brain_dump, None, 20)
                .map_err(|e| format!("Index search: {e}"))?;
            for r in &results {
                existing_ids.push(r.node_id.clone());
                context_rich.push(ExistingNodeSummary {
                    id: r.node_id.clone(),
                    title: r.title.clone(),
                    node_type: r.node_type.clone(),
                    summary: r.snippet.clone(),
                });
            }
        }

        let existing_suffixes = tempyr_core::id::collect_existing_suffixes(&graph_dir);
        let mut result = proposer::interview_start(
            &p.brain_dump,
            root_type,
            &schema,
            &existing_ids,
            &existing_suffixes,
        )
        .map_err(|e| e.to_string())?;

        result.session.graph_context_rich = context_rich;

        if let Some(ref g) = graph {
            let gaps =
                tempyr_interview::gaps::detect_gaps_with_graph(&result.session, &schema, Some(g));
            result.questions = gaps.iter().take(3).cloned().collect();
            result.session.remaining_gaps = gaps;
        }

        result.session.save(&sessions).map_err(|e| e.to_string())?;

        let state = session_state_json(&result.session, &schema);
        serde_json::to_string_pretty(&state).map_err(|e| e.to_string())
    }

    #[tool(
        name = "interview_answer",
        description = "Process a user's answer during an interview. Updates session state, fills gaps, may advance the interview phase."
    )]
    fn interview_answer(
        &self,
        Parameters(p): Parameters<InterviewAnswerParams>,
    ) -> Result<String, String> {
        let (graph_dir, gf_dir, schema) = find_project()?;
        let sessions = sessions_dir(&gf_dir);
        let graph = Graph::load_from_directory(&graph_dir, schema.clone()).ok();

        let mut session =
            InterviewSession::load_by_id(&sessions, &p.session_id).map_err(|e| e.to_string())?;

        let question_context: String = next_questions(&session, 3)
            .iter()
            .map(|g| g.suggested_question.as_str())
            .collect::<Vec<_>>()
            .join(" | ");

        session.record_answer(&question_context, &p.answer, vec![]);
        let update = proposer::reanalyze_with_graph(&mut session, &schema, graph.as_ref());

        session.save(&sessions).map_err(|e| e.to_string())?;

        serde_json::to_string_pretty(&json!({
            "session_id": session.id,
            "filled_gaps": update.filled_gaps,
            "next_questions": update.questions,
            "phase": session.phase,
            "phase_changed": update.phase_changed,
            "progress": {
                "filled": update.progress.filled,
                "total": update.progress.total,
                "percentage": update.progress.percentage,
            },
            "tentative_nodes_count": session.tentative_nodes.len() + 1,
            "tentative_edges_count": session.tentative_edges.len(),
        }))
        .map_err(|e| e.to_string())
    }

    #[tool(
        name = "interview_show",
        description = "Return the full tentative graph state for review. Shows all proposed nodes grouped by type, edges, remaining gaps, and Q&A history."
    )]
    fn interview_show(
        &self,
        Parameters(p): Parameters<InterviewSessionParams>,
    ) -> Result<String, String> {
        let (_, gf_dir, schema) = find_project()?;
        let sessions = sessions_dir(&gf_dir);
        let session =
            InterviewSession::load_by_id(&sessions, &p.session_id).map_err(|e| e.to_string())?;

        let state = session_state_json(&session, &schema);
        serde_json::to_string_pretty(&state).map_err(|e| e.to_string())
    }

    #[tool(
        name = "interview_commit",
        description = "Write all tentative nodes and edges to disk, creating graph files. Validates the resulting graph. Deletes the session on success."
    )]
    fn interview_commit(
        &self,
        Parameters(p): Parameters<InterviewSessionParams>,
    ) -> Result<String, String> {
        let (graph_dir, gf_dir, schema) = find_project()?;
        let sessions = sessions_dir(&gf_dir);
        let session =
            InterviewSession::load_by_id(&sessions, &p.session_id).map_err(|e| e.to_string())?;

        let result = session
            .commit(&graph_dir, &schema, &sessions)
            .map_err(|e| e.to_string())?;

        let mut all_warnings = result.warnings.clone();
        let validation_warnings = {
            let graph = Graph::load_from_directory(&graph_dir, schema.clone())
                .map_err(|e| e.to_string())?;
            let issues = validate_graph(&graph);
            issues.iter().map(|i| i.to_string()).collect::<Vec<_>>()
        };
        all_warnings.extend(validation_warnings);

        if let Some(warning) = index_refresh_warning(&graph_dir, &gf_dir, &schema) {
            all_warnings.push(warning);
        }

        serde_json::to_string_pretty(&json!({
            "files_created": result.created_files.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "files_modified": result.modified_files.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "warnings": all_warnings,
            "node_count": result.node_count,
            "edge_count": result.edge_count,
        }))
        .map_err(|e| e.to_string())
    }

    #[tool(
        name = "interview_adjust",
        description = "Modify a tentative node during an interview (change body, status, or ID). Re-runs gap analysis after the adjustment."
    )]
    fn interview_adjust(
        &self,
        Parameters(p): Parameters<InterviewAdjustParams>,
    ) -> Result<String, String> {
        let (_, gf_dir, schema) = find_project()?;
        let sessions = sessions_dir(&gf_dir);
        let mut session =
            InterviewSession::load_by_id(&sessions, &p.session_id).map_err(|e| e.to_string())?;

        let patch = NodePatch {
            id: p.new_id.clone(),
            body: p.body,
            status: p.status,
            ..Default::default()
        };

        session
            .adjust_node(&p.node_id, patch)
            .map_err(|e| e.to_string())?;

        if let Some(ref new_id) = p.new_id {
            for edge in &mut session.tentative_edges {
                if edge.source == p.node_id {
                    edge.source = new_id.clone();
                }
                if edge.target == p.node_id {
                    edge.target = new_id.clone();
                }
            }
        }

        let _update = proposer::reanalyze(&mut session, &schema);
        session.save(&sessions).map_err(|e| e.to_string())?;

        let state = session_state_json(&session, &schema);
        serde_json::to_string_pretty(&state).map_err(|e| e.to_string())
    }

    #[tool(
        name = "interview_resume",
        description = "Resume an interrupted interview session. Returns the full current state so the conversation can continue."
    )]
    fn interview_resume(
        &self,
        Parameters(p): Parameters<InterviewSessionParams>,
    ) -> Result<String, String> {
        let (_, gf_dir, schema) = find_project()?;
        let sessions = sessions_dir(&gf_dir);
        let session =
            InterviewSession::load_by_id(&sessions, &p.session_id).map_err(|e| e.to_string())?;

        let state = session_state_json(&session, &schema);
        serde_json::to_string_pretty(&state).map_err(|e| e.to_string())
    }

    #[tool(
        name = "interview_add_node",
        description = "Add a tentative node to an active interview session. The node is stored in session state (not written to disk) until interview_commit. Automatically re-analyzes gaps and may advance the interview phase."
    )]
    fn interview_add_node(
        &self,
        Parameters(p): Parameters<InterviewAddNodeParams>,
    ) -> Result<String, String> {
        let status = p.status.as_deref().unwrap_or("draft");
        let confidence = p.confidence.unwrap_or(0.9) as f32;

        let (graph_dir, gf_dir, schema) = find_project()?;
        let sessions = sessions_dir(&gf_dir);
        let graph = Graph::load_from_directory(&graph_dir, schema.clone()).ok();

        let mut session =
            InterviewSession::load_by_id(&sessions, &p.session_id).map_err(|e| e.to_string())?;

        let node_id = if id::is_hybrid_id(&p.slug) {
            p.slug.clone()
        } else {
            let existing: HashSet<String> = session
                .tentative_nodes
                .iter()
                .filter_map(|n| id::parse_node_id(&n.id).map(|parsed| parsed.suffix))
                .collect();
            id::make_node_id(&p.slug, &existing)
        };

        session.add_tentative_node(TentativeNode {
            id: node_id.clone(),
            node_type: p.node_type.clone(),
            status: status.to_string(),
            fields: HashMap::new(),
            body: p.body,
            confidence,
            source_qa: vec![session.answered.len()],
        });

        let update = proposer::reanalyze_with_graph(&mut session, &schema, graph.as_ref());
        session.save(&sessions).map_err(|e| e.to_string())?;

        serde_json::to_string_pretty(&json!({
            "session_id": session.id,
            "node_id": node_id,
            "node_type": p.node_type,
            "filled_gaps": update.filled_gaps,
            "next_questions": update.questions,
            "phase": session.phase,
            "phase_changed": update.phase_changed,
            "progress": {
                "filled": update.progress.filled,
                "total": update.progress.total,
                "percentage": update.progress.percentage,
            },
            "tentative_nodes_count": session.tentative_nodes.len() + 1,
            "tentative_edges_count": session.tentative_edges.len(),
        }))
        .map_err(|e| e.to_string())
    }

    #[tool(
        name = "interview_add_edge",
        description = "Add a tentative edge to an active interview session. Both source and target can be tentative node IDs (from interview_add_node) or existing graph node IDs. Stored in session state until interview_commit."
    )]
    fn interview_add_edge(
        &self,
        Parameters(p): Parameters<InterviewAddEdgeParams>,
    ) -> Result<String, String> {
        let (graph_dir, gf_dir, schema) = find_project()?;
        let sessions = sessions_dir(&gf_dir);
        let graph = Graph::load_from_directory(&graph_dir, schema.clone()).ok();

        let mut session =
            InterviewSession::load_by_id(&sessions, &p.session_id).map_err(|e| e.to_string())?;

        let resolved_source = resolve_interview_node_id(&session, &graph_dir, &p.source)?;
        let resolved_target = resolve_interview_node_id(&session, &graph_dir, &p.target)?;

        session.add_tentative_edge(TentativeEdge {
            source: resolved_source.clone(),
            target: resolved_target.clone(),
            edge_type: p.edge_type.clone(),
            source_type: EdgeSource::ExplicitFromAnswer,
        });

        let update = proposer::reanalyze_with_graph(&mut session, &schema, graph.as_ref());
        session.save(&sessions).map_err(|e| e.to_string())?;

        serde_json::to_string_pretty(&json!({
            "session_id": session.id,
            "edge": format!("{resolved_source} --{}--> {resolved_target}", p.edge_type),
            "filled_gaps": update.filled_gaps,
            "next_questions": update.questions,
            "phase": session.phase,
            "phase_changed": update.phase_changed,
            "progress": {
                "filled": update.progress.filled,
                "total": update.progress.total,
                "percentage": update.progress.percentage,
            },
            "tentative_nodes_count": session.tentative_nodes.len() + 1,
            "tentative_edges_count": session.tentative_edges.len(),
        }))
        .map_err(|e| e.to_string())
    }
    // Linear tools

    #[tool(
        name = "linear_push",
        description = "Push graph node(s) to Linear with full context and data lineage."
    )]
    fn linear_push(&self, Parameters(p): Parameters<LinearPushParams>) -> Result<String, String> {
        let dry_run = p.dry_run.unwrap_or(false);
        let (client, config, gf_dir, graph_dir, schema) = build_linear_deps()?;
        let resolved_node_id = p
            .node_id
            .as_deref()
            .map(|r| ops::resolve_node_id(&graph_dir, r).map_err(|e| e.to_string()))
            .transpose()?;
        let node_id = resolved_node_id.as_deref();
        let graph =
            Graph::load_from_directory(&graph_dir, schema.clone()).map_err(|e| e.to_string())?;
        let mut sync_state = SyncState::load(&gf_dir).map_err(|e| e.to_string())?;
        let status_mapper = build_status_mapper_from_config(&config);

        if dry_run {
            let syncable = ["epic", "feature", "task"];
            let mut would_create = 0usize;
            let mut would_update = 0usize;

            let nodes: Vec<&str> = if let Some(id) = node_id {
                vec![id]
            } else {
                graph
                    .nodes
                    .values()
                    .filter(|n| syncable.contains(&n.node_type()))
                    .map(|n| n.id())
                    .collect()
            };

            for id in &nodes {
                if let Some(entry) = sync_state.get_by_node_id(id) {
                    if let Some(node) = graph.get_node(id)
                        && node.content_hash != entry.content_hash_at_sync
                    {
                        would_update += 1;
                    }
                } else {
                    would_create += 1;
                }
            }

            return serde_json::to_string_pretty(&json!({
                "dry_run": true,
                "would_create": would_create,
                "would_update": would_update,
            }))
            .map_err(|e| e.to_string());
        }

        let index = open_optional_index(&graph_dir, &gf_dir)?;

        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                if let Some(id) = node_id {
                    let node = graph
                        .get_node(id)
                        .ok_or_else(|| format!("Node not found: {id}"))?;
                    let entry = tempyr_linear::push::push_node(
                        &client,
                        node,
                        &graph,
                        index.as_ref(),
                        &schema,
                        &config,
                        &mut sync_state,
                        &status_mapper,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                    sync_state.save(&gf_dir).map_err(|e| e.to_string())?;
                    let action = match entry.action {
                        tempyr_linear::push::PushAction::Created => "created",
                        tempyr_linear::push::PushAction::Updated => "updated",
                    };
                    serde_json::to_string_pretty(&json!({
                        "action": action,
                        "node_id": entry.node_id,
                        "linear_id": entry.linear_id,
                        "linear_identifier": entry.linear_identifier,
                    }))
                    .map_err(|e| e.to_string())
                } else {
                    let result = tempyr_linear::push::push_all(
                        &client,
                        &graph,
                        index.as_ref(),
                        &schema,
                        &config,
                        &mut sync_state,
                        &status_mapper,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                    sync_state.save(&gf_dir).map_err(|e| e.to_string())?;
                    serde_json::to_string_pretty(&json!({
                        "created": result.created.len(),
                        "updated": result.updated.len(),
                        "skipped": result.skipped.len(),
                        "errors": result.errors,
                    }))
                    .map_err(|e| e.to_string())
                }
            })
        })
    }

    #[tool(
        name = "linear_pull",
        description = "Pull changes from Linear into the graph. Updates node statuses based on Linear issue state changes."
    )]
    fn linear_pull(&self, Parameters(p): Parameters<LinearDryRunParams>) -> Result<String, String> {
        let dry_run = p.dry_run.unwrap_or(false);
        let (client, config, gf_dir, graph_dir, schema) = build_linear_deps()?;
        let mut sync_state = SyncState::load(&gf_dir).map_err(|e| e.to_string())?;
        let status_mapper = build_status_mapper_from_config(&config);

        if dry_run {
            return serde_json::to_string_pretty(&json!({
                "dry_run": true,
                "tracked_entries": sync_state.entries.len(),
                "last_sync": sync_state.last_sync_at,
            }))
            .map_err(|e| e.to_string());
        }

        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                let result = tempyr_linear::pull::pull(
                    &client,
                    &graph_dir,
                    &schema,
                    &config,
                    &mut sync_state,
                    &status_mapper,
                )
                .await
                .map_err(|e| e.to_string())?;
                sync_state.save(&gf_dir).map_err(|e| e.to_string())?;
                let mut warnings = result.warnings.clone();
                if result.changed_graph()
                    && let Some(warning) = index_refresh_warning(&graph_dir, &gf_dir, &schema)
                {
                    warnings.push(warning);
                }

                serde_json::to_string_pretty(&json!({
                    "created": result.created,
                    "updated": result.updated,
                    "status_changed": result.status_changed.len(),
                    "conflicts": result.conflicts.len(),
                    "warnings": warnings,
                    "errors": result.errors,
                }))
                .map_err(|e| e.to_string())
            })
        })
    }

    #[tool(
        name = "linear_sync",
        description = "Bidirectional sync: push local graph changes to Linear, then pull remote changes back."
    )]
    fn linear_sync(&self, Parameters(p): Parameters<LinearDryRunParams>) -> Result<String, String> {
        let dry_run = p.dry_run.unwrap_or(false);
        let (client, config, gf_dir, graph_dir, schema) = build_linear_deps()?;
        let graph =
            Graph::load_from_directory(&graph_dir, schema.clone()).map_err(|e| e.to_string())?;
        let mut sync_state = SyncState::load(&gf_dir).map_err(|e| e.to_string())?;
        let status_mapper = build_status_mapper_from_config(&config);

        if dry_run {
            let report = tempyr_linear::sync::status_summary(&sync_state, &graph);
            return serde_json::to_string_pretty(&json!({
                "dry_run": true,
                "would_push": report.stale_count + report.unlinked_syncable_count,
                "tracked_for_pull": report.linked_count,
            }))
            .map_err(|e| e.to_string());
        }

        let index = open_optional_index(&graph_dir, &gf_dir)?;

        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                let result = tempyr_linear::sync::sync(
                    &client,
                    &graph_dir,
                    &graph,
                    index.as_ref(),
                    &schema,
                    &config,
                    &mut sync_state,
                    &status_mapper,
                )
                .await
                .map_err(|e| e.to_string())?;
                sync_state.save(&gf_dir).map_err(|e| e.to_string())?;
                let mut warnings = Vec::new();
                if result.changed_graph()
                    && let Some(warning) = index_refresh_warning(&graph_dir, &gf_dir, &schema)
                {
                    warnings.push(warning);
                }

                serde_json::to_string_pretty(&json!({
                    "push": {
                        "created": result.push.created.len(),
                        "updated": result.push.updated.len(),
                        "errors": result.push.errors.len(),
                    },
                    "pull": {
                        "created": result.pull.created.len(),
                        "updated": result.pull.updated.len(),
                        "conflicts": result.pull.conflicts.len(),
                        "errors": result.pull.errors.len(),
                    },
                    "warnings": warnings,
                }))
                .map_err(|e| e.to_string())
            })
        })
    }

    #[tool(
        name = "linear_status",
        description = "Show Linear sync state: linked nodes, pending changes, stale entries, and conflicts."
    )]
    fn linear_status(&self) -> Result<String, String> {
        let (graph_dir, gf_dir, schema) = find_project()?;

        if LinearConfig::load(&gf_dir).is_err() {
            return serde_json::to_string_pretty(&json!({
                "configured": false,
                "message": "Linear integration not configured. Run `tempyr linear setup` first."
            }))
            .map_err(|e| e.to_string());
        }

        let graph = Graph::load_from_directory(&graph_dir, schema).map_err(|e| e.to_string())?;
        let sync_state = SyncState::load(&gf_dir).map_err(|e| e.to_string())?;
        let report = tempyr_linear::sync::status_summary(&sync_state, &graph);

        serde_json::to_string_pretty(&json!({
            "configured": true,
            "linked": report.linked_count,
            "unlinked_syncable": report.unlinked_syncable_count,
            "stale": report.stale_count,
            "orphaned": report.orphaned_count,
            "last_sync": report.last_sync,
            "entries": report.entries,
        }))
        .map_err(|e| e.to_string())
    }
}

impl Default for TempyrServer {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_handler]
impl ServerHandler for TempyrServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("tempyr-mcp", env!("CARGO_PKG_VERSION")))
    }
}
