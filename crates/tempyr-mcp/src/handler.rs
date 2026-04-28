use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use rmcp::RoleServer;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{Peer, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use std::str::FromStr;
use tempyr_core::graph::Graph;
use tempyr_core::id;
use tempyr_core::ops;
use tempyr_core::schema::Schema;
use tempyr_core::temporal::TemporalFilter;
use tempyr_core::traverse::bfs;
use tempyr_core::validate::validate_graph;
use tempyr_index::fts::MetadataFilter;
use tempyr_index::health::{self, HealthInputs};
use tempyr_index::hybrid::{RetrievalConfig, hybrid_retrieve};
use tempyr_index::indexer::Index;
use tempyr_index::refresh::refresh_index_for_graph;
use tempyr_interview::gaps::next_questions;
use tempyr_interview::proposer;
use tempyr_interview::session::{
    EdgeSource, ExistingNodeSummary, InterviewSession, NodePatch, TentativeEdge, TentativeNode,
};

use tempyr_journal::path as jpath;
use tempyr_journal::{
    Confidence, Entry, Kind, Polarity, Session, Severity, append, default_redactor,
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

const ROOTS_LIST_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone)]
struct ProjectAnchorState {
    ready: Arc<(Mutex<bool>, Condvar)>,
}

impl ProjectAnchorState {
    fn ready() -> Self {
        Self {
            ready: Arc::new((Mutex::new(true), Condvar::new())),
        }
    }

    fn pending() -> Self {
        Self {
            ready: Arc::new((Mutex::new(false), Condvar::new())),
        }
    }

    fn mark_ready(&self) {
        let (lock, condvar) = &*self.ready;
        let mut ready = lock.lock().expect("project anchor state poisoned");
        *ready = true;
        condvar.notify_all();
    }

    fn wait_ready(&self) {
        let (lock, condvar) = &*self.ready;
        let mut ready = lock.lock().expect("project anchor state poisoned");
        while !*ready {
            ready = condvar
                .wait(ready)
                .expect("project anchor state poisoned while waiting");
        }
    }
}

fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let raw = uri.strip_prefix("file://")?;
    let (raw, is_unc_path) = match raw.strip_prefix("localhost") {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => (rest, false),
        _ if raw.starts_with("//") => (raw.trim_start_matches('/'), true),
        _ if raw.starts_with('/') => (raw, false),
        _ => (raw, true),
    };
    let decoded = percent_decode(raw)?;

    #[cfg(windows)]
    {
        let normalized = decoded.replace('/', "\\");
        if is_unc_path {
            Some(PathBuf::from(format!(
                "\\\\{}",
                normalized.trim_start_matches('\\')
            )))
        } else {
            Some(PathBuf::from(
                normalized.strip_prefix('\\').unwrap_or(&normalized),
            ))
        }
    }

    #[cfg(not(windows))]
    {
        let _ = is_unc_path;
        Some(PathBuf::from(decoded))
    }
}

fn percent_decode(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = bytes.get(i + 1).copied().and_then(hex_value)?;
            let lo = bytes.get(i + 2).copied().and_then(hex_value)?;
            decoded.push((hi << 4) | lo);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn find_project_now() -> Result<(PathBuf, PathBuf, Schema), String> {
    let root = tempyr_core::project::find_project_root().ok_or_else(|| {
        let cwd = std::env::current_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "<unknown>".to_string());
        format!(
            "Not a tempyr project from server cwd {cwd} (no .tempyr/ or .tempyr-redirect found). \
Set {} or {} if your MCP client launches tempyr from a different workspace.",
            tempyr_core::project::PROJECT_ROOT_ENV_VAR,
            tempyr_core::project::GRAPH_DIR_ENV_VAR,
        )
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

fn ensure_index_path(
    graph_dir: &Path,
    gf_dir: &Path,
    schema: &Schema,
    graph: Option<&Graph>,
) -> Result<PathBuf, String> {
    let layout = index_layout(graph_dir, gf_dir)?;
    if let Some(path) = layout.current_index_path().map_err(|e| e.to_string())? {
        return Ok(path);
    }

    let loaded_graph;
    let graph = match graph {
        Some(graph) => graph,
        None => {
            loaded_graph =
                Graph::load_from_directory(graph_dir, schema.clone()).map_err(|e| e.to_string())?;
            &loaded_graph
        }
    };
    refresh_index_for_graph(&layout, graph).map_err(|e| e.to_string())?;
    layout
        .current_index_path()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Index refresh did not produce a queryable snapshot.".to_string())
}

fn refresh_index_for_current_snapshot(
    graph_dir: &Path,
    gf_dir: &Path,
    schema: &Schema,
) -> Result<(), String> {
    let graph = Graph::load_from_directory(graph_dir, schema.clone()).map_err(|e| e.to_string())?;
    refresh_index_for_loaded_graph(graph_dir, gf_dir, &graph)
}

fn refresh_index_for_loaded_graph(
    graph_dir: &Path,
    gf_dir: &Path,
    graph: &Graph,
) -> Result<(), String> {
    let layout = index_layout(graph_dir, gf_dir)?;
    refresh_index_for_graph(&layout, graph).map_err(|e| e.to_string())?;
    Ok(())
}

fn format_index_refresh_warning(err: String) -> String {
    format!("Index update failed (run 'tempyr index rebuild'): {err}")
}

fn index_refresh_warning_for_loaded_graph(
    graph_dir: &Path,
    gf_dir: &Path,
    graph: &Graph,
) -> Option<String> {
    refresh_index_for_loaded_graph(graph_dir, gf_dir, graph)
        .err()
        .map(format_index_refresh_warning)
}

fn index_refresh_warning(graph_dir: &Path, gf_dir: &Path, schema: &Schema) -> Option<String> {
    refresh_index_for_current_snapshot(graph_dir, gf_dir, schema)
        .err()
        .map(format_index_refresh_warning)
}

// Keep graph queries aligned with the just-written snapshot even if saving
// Linear sync metadata fails afterwards.
fn finalize_linear_graph_update(
    sync_state: &SyncState,
    base_warnings: Vec<String>,
    graph_changed: bool,
    graph_dir: &Path,
    gf_dir: &Path,
    schema: &Schema,
) -> Result<Vec<String>, String> {
    let mut warnings = base_warnings;
    let refresh_warning = if graph_changed {
        index_refresh_warning(graph_dir, gf_dir, schema)
    } else {
        None
    };

    if let Some(warning) = &refresh_warning {
        warnings.push(warning.clone());
    }

    sync_state.save(gf_dir).map_err(|e| {
        let save_err = e.to_string();
        match refresh_warning {
            Some(warning) => format!("{save_err}; {warning}"),
            None => save_err,
        }
    })?;

    Ok(warnings)
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

fn build_linear_deps(
    project: (PathBuf, PathBuf, Schema),
) -> Result<(LinearClient, LinearConfig, PathBuf, PathBuf, Schema), String> {
    let (graph_dir, gf_dir, schema) = project;
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

#[derive(Debug, Deserialize, JsonSchema)]
pub struct JournalLogParams {
    /// One of: plan | finding | decision | dead_end | assumption | question | risk | outcome.
    /// Snake_case. Capitalization is normalized.
    pub kind: String,
    /// Short title (20-200 chars). One sentence describing the moment.
    pub summary: String,
    /// Optional longer body. REQUIRED for `decision` and `dead_end` (50+ chars).
    pub detail: Option<String>,
    /// User-defined labels. Use the `tool` tag for tool quirks/findings.
    pub tags: Option<Vec<String>>,
    /// File paths relevant to this entry, normalized relative to the repo root.
    pub files: Option<Vec<String>>,
    /// Graph node IDs this entry references (e.g. ["task-foo-abc123"]).
    pub references: Option<Vec<String>>,
    /// True if this entry is from in-flight state that may roll back. Default false.
    pub provisional: Option<bool>,
    /// "low" | "medium" | "high".
    pub confidence: Option<String>,
    /// "info" | "warn" | "high" | "blocker". Recommended for `risk`/`dead_end`.
    pub severity: Option<String>,

    // ---- decision-specific ----
    /// `decision`: alternatives considered.
    pub alternatives: Option<Vec<String>>,
    /// `decision`: which alternative was chosen.
    pub chosen: Option<String>,
    /// `decision`: rationale for the choice.
    pub rationale: Option<String>,
    /// `decision`: is the decision reversible?
    pub reversible: Option<bool>,

    // ---- dead_end-specific ----
    /// `dead_end`: the approach that was tried.
    pub approach: Option<String>,
    /// `dead_end`: how/why it failed.
    pub failure_mode: Option<String>,
    /// `dead_end`: a suggested next direction, if any.
    pub next_to_try: Option<String>,

    // ---- assumption-specific ----
    /// `assumption`: "positive" | "negative" | "unknown".
    pub polarity: Option<String>,

    // ---- outcome-specific ----
    /// `outcome`: did the work succeed?
    pub passed: Option<bool>,
    /// `outcome`: did the build pass?
    pub build_ok: Option<bool>,
    /// `outcome`: commit SHA, if any.
    pub commit_sha: Option<String>,
    /// `outcome`: marks the final outcome of a session. Triggers publish.
    /// Renamed to `final` in JSON; `final` is a Rust reserved keyword.
    #[serde(rename = "final")]
    pub is_final: Option<bool>,
}

// Journal helper functions

fn parse_opt<T: FromStr<Err = tempyr_journal::JournalError>>(
    s: Option<&str>,
) -> Result<Option<T>, String> {
    s.map(T::from_str).transpose().map_err(|e| e.to_string())
}

/// Relative path of `cwd` under `worktree_top`, or `None` when `cwd` is
/// either the worktree root (avoid a noisy `cwd: "."`) or outside the repo
/// entirely. Mirrors the CLI behavior — we never log absolute home-dir paths.
fn relative_cwd(cwd: &Path, worktree_top: &Path) -> Option<String> {
    if cwd == worktree_top {
        return None;
    }
    cwd.strip_prefix(worktree_top)
        .ok()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
}

// Server

#[derive(Clone)]
pub struct TempyrServer {
    tool_router: ToolRouter<Self>,
    relative_project_root_fallback: Option<PathBuf>,
    project_anchor_state: ProjectAnchorState,
    /// Cached journal session, keyed by `(common_dir, worktree_top)`. Opened
    /// lazily on the first `journal_log` call. Keyed so a server that ends up
    /// serving more than one repo/worktree doesn't keep appending to the wrong
    /// session metadata.
    journal_session: Arc<Mutex<Option<JournalSessionCache>>>,
}

#[derive(Clone)]
struct JournalSessionCache {
    common_dir: PathBuf,
    worktree_top: PathBuf,
    session: Session,
}

#[tool_router]
impl TempyrServer {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
            relative_project_root_fallback: None,
            project_anchor_state: ProjectAnchorState::ready(),
            journal_session: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn with_relative_project_root_fallback(mut self, fallback: Option<PathBuf>) -> Self {
        self.relative_project_root_fallback = fallback;
        self
    }

    pub(crate) fn with_deferred_project_anchor(mut self) -> Self {
        self.project_anchor_state = ProjectAnchorState::pending();
        self
    }

    pub(crate) fn mark_project_anchor_ready(&self) {
        self.project_anchor_state.mark_ready();
    }

    fn find_project(&self) -> Result<(PathBuf, PathBuf, Schema), String> {
        self.project_anchor_state.wait_ready();
        find_project_now()
    }

    /// Get the current journal session for `(common_dir, worktree_top)`,
    /// opening one lazily on first use and caching it. If the cached entry is
    /// for a different repo/worktree, replace it — a single MCP server should
    /// not silently keep writing to a stale session.
    fn journal_session_or_open(
        &self,
        common_dir: &Path,
        worktree_top: &Path,
    ) -> Result<Session, String> {
        let mut guard = self
            .journal_session
            .lock()
            .map_err(|e| format!("journal session mutex poisoned: {e}"))?;
        if let Some(cache) = guard.as_ref()
            && cache.common_dir == common_dir
            && cache.worktree_top == worktree_top
        {
            return Ok(cache.session.clone());
        }
        // Reuse an active on-disk session if one exists for this
        // (worktree, agent) pair, so a freshly-launched MCP server picks up
        // a session that previous CLI calls or a prior server already started.
        let session = Session::open_or_resume(common_dir, worktree_top, "claude")
            .map_err(|e| format!("open journal session: {e}"))?;
        *guard = Some(JournalSessionCache {
            common_dir: common_dir.to_path_buf(),
            worktree_top: worktree_top.to_path_buf(),
            session: session.clone(),
        });
        Ok(session)
    }

    pub(crate) async fn try_anchor_from_client_roots(&self, peer: Peer<RoleServer>) {
        if self.relative_project_root_fallback.is_none()
            && tempyr_core::project::find_project_roots().is_some()
        {
            return;
        }

        let supports_roots = peer
            .peer_info()
            .and_then(|client| client.capabilities.roots.as_ref())
            .is_some();
        if !supports_roots {
            return;
        }

        let Ok(Ok(result)) = tokio::time::timeout(ROOTS_LIST_TIMEOUT, peer.list_roots()).await
        else {
            return;
        };

        for root in result.roots {
            let Some(mut path) = file_uri_to_path(&root.uri) else {
                continue;
            };
            if let Some(relative) = &self.relative_project_root_fallback {
                path = path.join(relative);
            }
            if let Some(roots) = tempyr_core::project::find_project_roots_from(path)
                && std::env::set_current_dir(&roots.anchor_root).is_ok()
            {
                let _ = tempyr_core::project::load_project_env_from(roots.anchor_root);
                return;
            }
        }
    }
    // Graph query tools

    #[tool(
        name = "graph_search",
        description = "Full-text keyword search across all graph nodes. Searches body text, titles, and tags. Optionally filter results by metadata (type, status, owner)."
    )]
    fn graph_search(&self, Parameters(p): Parameters<GraphSearchParams>) -> Result<String, String> {
        let max_results = p.max_results.unwrap_or(10) as usize;
        let (graph_dir, gf_dir, schema) = self.find_project()?;
        let index_path = ensure_index_path(&graph_dir, &gf_dir, &schema, None)?;
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
        let (graph_dir, gf_dir, schema) = self.find_project()?;
        let index_path = ensure_index_path(&graph_dir, &gf_dir, &schema, None)?;
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
        let (graph_dir, gf_dir, schema) = self.find_project()?;
        let resolved_root = p
            .root_node
            .as_deref()
            .map(|r| ops::resolve_node_id(&graph_dir, r).map_err(|e| e.to_string()))
            .transpose()?;
        let graph =
            Graph::load_from_directory(&graph_dir, schema.clone()).map_err(|e| e.to_string())?;
        let index_path = ensure_index_path(&graph_dir, &gf_dir, &schema, Some(&graph))?;
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
        let (graph_dir, _, schema) = self.find_project()?;
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
        let (graph_dir, _, _) = self.find_project()?;
        let resolved = ops::resolve_node_id(&graph_dir, &p.node_id).map_err(|e| e.to_string())?;
        let path = ops::find_node_file(&graph_dir, &resolved).map_err(|e| e.to_string())?;
        std::fs::read_to_string(&path).map_err(|e| e.to_string())
    }

    #[tool(
        name = "graph_stats",
        description = "Get graph statistics: node counts by type, edge counts"
    )]
    fn graph_stats(&self) -> Result<String, String> {
        let (graph_dir, _, schema) = self.find_project()?;
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
        let (graph_dir, gf_dir, schema) = self.find_project()?;
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
        let (graph_dir, gf_dir, schema) = self.find_project()?;
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
        let (graph_dir, gf_dir, schema) = self.find_project()?;
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
        let (graph_dir, _, schema) = self.find_project()?;
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
        let (graph_dir, gf_dir, schema) = self.find_project()?;
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
        let (graph_dir, gf_dir, schema) = self.find_project()?;
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
        let (graph_dir, gf_dir, schema) = self.find_project()?;
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
        let (_, gf_dir, schema) = self.find_project()?;
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
        let (graph_dir, gf_dir, schema) = self.find_project()?;
        let sessions = sessions_dir(&gf_dir);
        let session =
            InterviewSession::load_by_id(&sessions, &p.session_id).map_err(|e| e.to_string())?;

        let result = session
            .commit(&graph_dir, &schema, &sessions)
            .map_err(|e| e.to_string())?;

        let mut all_warnings = result.warnings.clone();
        let graph =
            Graph::load_from_directory(&graph_dir, schema.clone()).map_err(|e| e.to_string())?;
        let validation_warnings = validate_graph(&graph)
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>();
        all_warnings.extend(validation_warnings);

        if let Some(warning) = index_refresh_warning_for_loaded_graph(&graph_dir, &gf_dir, &graph) {
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
        let (_, gf_dir, schema) = self.find_project()?;
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
        let (_, gf_dir, schema) = self.find_project()?;
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

        let (graph_dir, gf_dir, schema) = self.find_project()?;
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
        let (graph_dir, gf_dir, schema) = self.find_project()?;
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
        let (client, config, gf_dir, graph_dir, schema) = build_linear_deps(self.find_project()?)?;
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
        let (client, config, gf_dir, graph_dir, schema) = build_linear_deps(self.find_project()?)?;
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
                let warnings = finalize_linear_graph_update(
                    &sync_state,
                    result.warnings.clone(),
                    result.changed_graph(),
                    &graph_dir,
                    &gf_dir,
                    &schema,
                )?;

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
        let (client, config, gf_dir, graph_dir, schema) = build_linear_deps(self.find_project()?)?;
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
                let warnings = finalize_linear_graph_update(
                    &sync_state,
                    result.pull.warnings.clone(),
                    result.changed_graph(),
                    &graph_dir,
                    &gf_dir,
                    &schema,
                )?;

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
        name = "system_doctor",
        description = "Report system health: active embedding provider/model, paths to all config files, index state, env files, and warnings. API key VALUES are never returned — only env var names and a boolean indicating whether the key is set."
    )]
    fn system_doctor(&self) -> Result<String, String> {
        let (graph_dir, gf_dir, schema) = self.find_project()?;
        let root = graph_dir
            .parent()
            .ok_or_else(|| "Failed to resolve project root from graph dir".to_string())?
            .to_path_buf();
        let cache = tempyr_core::project::cache_layout(&root, &gf_dir);

        let inputs = HealthInputs {
            root: &root,
            graph_dir: &graph_dir,
            tempyr_dir: &gf_dir,
            cache: &cache,
            schema: &schema,
            tempyr_version: env!("CARGO_PKG_VERSION"),
        };
        let report = health::build_report(&inputs);
        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())
    }

    #[tool(
        name = "journal_log",
        description = "Append one moment of agent reasoning to the session journal: a plan, finding, decision, dead end, assumption, question, risk, or outcome. Cheap and append-only — log freely, including failures and surprises. This is NOT how knowledge graduates into the project; promote durable facts via graph_add_node.\n\nKinds:\n  plan       — what you're about to attempt and why\n  finding    — something you learned by reading code or running a tool\n  assumption — something you're assuming without verifying (polarity required)\n  question   — something you don't know yet — to ask or look up\n  decision   — a choice with reasoning (chosen, rationale, reversible required; detail ≥ 50 chars)\n  dead_end   — an approach that didn't work (approach, failure_mode required; detail ≥ 50 chars). HIGH-VALUE — future agents read these to avoid repeating you.\n  risk       — a potential problem identified but not yet hit (severity recommended)\n  outcome    — the result of work; set final=true on the session-closing entry to trigger publish\n\nLog freely on dead ends and decisions — the system is empty if you don't. Successes are less valuable than failures here. For curated knowledge that should outlive this session, use graph_add_node."
    )]
    fn journal_log(&self, Parameters(p): Parameters<JournalLogParams>) -> Result<String, String> {
        let kind = Kind::parse_helpful(&p.kind).map_err(|e| e.to_string())?;

        // Resolve the repo through the anchored project, so journal_log uses
        // the same root/env path as every other tool (find_project() also
        // blocks until the deferred client-roots anchor is ready).
        let (graph_dir, _gf_dir, _schema) = self.find_project()?;
        let project_root = graph_dir
            .parent()
            .ok_or_else(|| "Failed to resolve project root from graph dir".to_string())?
            .to_path_buf();
        let common_dir = jpath::git_common_dir(&project_root).map_err(|e| e.to_string())?;
        let worktree_top = jpath::repo_toplevel(&project_root).map_err(|e| e.to_string())?;

        let session = self.journal_session_or_open(&common_dir, &worktree_top)?;

        let mut entry = Entry::for_session(kind, p.summary, &session);
        entry.detail = p.detail;
        entry.tags = p.tags.unwrap_or_default();
        entry.files = p
            .files
            .unwrap_or_default()
            .into_iter()
            .map(|f| jpath::repo_relative_path(&f, &worktree_top))
            .collect();
        entry.references = p.references.unwrap_or_default();
        let cwd = std::env::current_dir().map_err(|e| format!("cwd: {e}"))?;
        entry.cwd = relative_cwd(&cwd, &worktree_top);
        entry.provisional = p.provisional.unwrap_or(false);
        entry.confidence = parse_opt::<Confidence>(p.confidence.as_deref())?;
        entry.severity = parse_opt::<Severity>(p.severity.as_deref())?;
        entry.alternatives = p.alternatives.unwrap_or_default();
        entry.chosen = p.chosen;
        entry.rationale = p.rationale;
        entry.reversible = p.reversible;
        entry.approach = p.approach;
        entry.failure_mode = p.failure_mode;
        entry.next_to_try = p.next_to_try;
        entry.polarity = parse_opt::<Polarity>(p.polarity.as_deref())?;
        entry.passed = p.passed;
        entry.build_ok = p.build_ok;
        entry.commit_sha = p.commit_sha;
        entry.is_final = p.is_final.unwrap_or(false);

        default_redactor()
            .enforce(&mut entry)
            .map_err(|e| e.to_string())?;
        append(&session, &entry).map_err(|e| e.to_string())?;

        // Drop the session-final entry's `.ready` marker so the publisher
        // picks it up and `Session::find_active` stops resuming this id.
        if entry.is_final {
            session
                .finalize()
                .map_err(|e| format!("finalize session: {e}"))?;
        }

        Ok(serde_json::to_string_pretty(&json!({
            "id": entry.id,
            "session_id": session.id().as_str(),
            "kind": entry.kind.as_str(),
            "finalized": entry.is_final,
        }))
        .unwrap_or_default())
    }

    #[tool(
        name = "linear_status",
        description = "Show Linear sync state: linked nodes, pending changes, stale entries, and conflicts."
    )]
    fn linear_status(&self) -> Result<String, String> {
        let (graph_dir, gf_dir, schema) = self.find_project()?;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_decodes_hex_bytes() {
        assert_eq!(percent_decode("space%20path"), Some("space path".into()));
    }

    #[test]
    fn relative_cwd_returns_none_for_worktree_root() {
        let root = PathBuf::from("/repo/top");
        assert_eq!(relative_cwd(&root, &root), None);
    }

    #[test]
    fn relative_cwd_returns_relative_for_subdir() {
        let root = PathBuf::from("/repo/top");
        let sub = root.join("crates").join("foo");
        assert_eq!(relative_cwd(&sub, &root), Some("crates/foo".into()));
    }

    #[test]
    fn relative_cwd_returns_none_for_out_of_repo_path() {
        // Out-of-worktree must NOT leak as an absolute path string —
        // the redactor would block any /Users/<n>/ or C:\Users\<n>\ value.
        let root = PathBuf::from("/repo/top");
        let elsewhere = PathBuf::from("/somewhere/else");
        assert_eq!(relative_cwd(&elsewhere, &root), None);
    }

    #[test]
    fn percent_decode_rejects_invalid_hex() {
        assert_eq!(percent_decode("bad%zzpath"), None);
    }

    #[cfg(windows)]
    #[test]
    fn file_uri_to_path_converts_windows_file_uri() {
        assert_eq!(
            file_uri_to_path("file:///C:/Projects/Rust/tempyr"),
            Some(PathBuf::from(r"C:\Projects\Rust\tempyr"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn file_uri_to_path_accepts_localhost_authority() {
        assert_eq!(
            file_uri_to_path("file://localhost/C:/Projects/Rust/tempyr"),
            Some(PathBuf::from(r"C:\Projects\Rust\tempyr"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn file_uri_to_path_preserves_unc_authority() {
        assert_eq!(
            file_uri_to_path("file://server/share/project"),
            Some(PathBuf::from(r"\\server\share\project"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn file_uri_to_path_preserves_unc_path_without_authority() {
        assert_eq!(
            file_uri_to_path("file:////server/share/project"),
            Some(PathBuf::from(r"\\server\share\project"))
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn file_uri_to_path_converts_unix_file_uri() {
        assert_eq!(
            file_uri_to_path("file:///tmp/tempyr"),
            Some(PathBuf::from("/tmp/tempyr"))
        );
    }
}
