use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use graphforge_core::ops;
use graphforge_core::schema::Schema;

use crate::gaps::Gap;
use crate::phases::InterviewPhase;
use crate::{InterviewError, Result};

/// A full interview session with all state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterviewSession {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub root_type: String,
    pub root_node: TentativeNode,
    pub phase: InterviewPhase,
    pub tentative_nodes: Vec<TentativeNode>,
    pub tentative_edges: Vec<TentativeEdge>,
    pub answered: Vec<QAPair>,
    pub remaining_gaps: Vec<Gap>,
    pub graph_context: Vec<String>,
    pub token_budget_used: usize,
}

/// A proposed node not yet committed to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TentativeNode {
    pub id: String,
    pub node_type: String,
    pub status: String,
    pub fields: HashMap<String, String>,
    pub body: String,
    pub confidence: f32,
    pub source_qa: Vec<usize>,
}

/// A proposed edge not yet committed to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TentativeEdge {
    pub source: String,
    pub target: String,
    pub edge_type: String,
    pub source_type: EdgeSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum EdgeSource {
    ExplicitFromAnswer,
    InferredFromContext,
    InheritedFromParent,
}

/// A question-answer pair from the interview.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QAPair {
    pub question: String,
    pub answer: String,
    pub phase: InterviewPhase,
    pub timestamp: DateTime<Utc>,
    pub nodes_proposed: Vec<String>,
}

impl InterviewSession {
    /// Create a new session for a given root type.
    pub fn new(root_type: &str, root_id: &str, root_body: &str) -> Self {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();

        let root_node = TentativeNode {
            id: root_id.to_string(),
            node_type: root_type.to_string(),
            status: "draft".to_string(),
            fields: HashMap::new(),
            body: root_body.to_string(),
            confidence: 0.5,
            source_qa: Vec::new(),
        };

        Self {
            id,
            created_at: now,
            updated_at: now,
            root_type: root_type.to_string(),
            root_node,
            phase: InterviewPhase::Discovery,
            tentative_nodes: Vec::new(),
            tentative_edges: Vec::new(),
            answered: Vec::new(),
            remaining_gaps: Vec::new(),
            graph_context: Vec::new(),
            token_budget_used: 0,
        }
    }

    /// Save session to a JSON file in the sessions directory.
    pub fn save(&self, sessions_dir: &Path) -> Result<PathBuf> {
        std::fs::create_dir_all(sessions_dir)?;
        let path = sessions_dir.join(format!("session-{}.json", self.id));
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json)?;
        Ok(path)
    }

    /// Load a session from a JSON file.
    pub fn load(path: &Path) -> Result<Self> {
        let json = std::fs::read_to_string(path)?;
        let session: Self = serde_json::from_str(&json)?;
        Ok(session)
    }

    /// Load a session by ID from the sessions directory.
    pub fn load_by_id(sessions_dir: &Path, session_id: &str) -> Result<Self> {
        let path = sessions_dir.join(format!("session-{session_id}.json"));
        if !path.exists() {
            return Err(InterviewError::Session(format!(
                "Session not found: {session_id}"
            )));
        }
        Self::load(&path)
    }

    /// List all active sessions in the sessions directory.
    pub fn list_sessions(sessions_dir: &Path) -> Result<Vec<SessionSummary>> {
        let mut summaries = Vec::new();

        if !sessions_dir.exists() {
            return Ok(summaries);
        }

        for entry in std::fs::read_dir(sessions_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json")
                && let Ok(session) = Self::load(&path) {
                    summaries.push(SessionSummary {
                        id: session.id.clone(),
                        root_type: session.root_type.clone(),
                        root_id: session.root_node.id.clone(),
                        phase: session.phase,
                        node_count: session.tentative_nodes.len() + 1, // +1 for root
                        created_at: session.created_at,
                        updated_at: session.updated_at,
                    });
                }
        }

        summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(summaries)
    }

    /// Delete the session file.
    pub fn delete(&self, sessions_dir: &Path) -> Result<()> {
        let path = sessions_dir.join(format!("session-{}.json", self.id));
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    /// Add a tentative node to the session.
    pub fn add_tentative_node(&mut self, node: TentativeNode) {
        // Check for duplicates
        if self.root_node.id == node.id {
            return;
        }
        if let Some(existing) = self.tentative_nodes.iter_mut().find(|n| n.id == node.id) {
            *existing = node;
        } else {
            self.tentative_nodes.push(node);
        }
        self.updated_at = Utc::now();
    }

    /// Add a tentative edge to the session.
    pub fn add_tentative_edge(&mut self, edge: TentativeEdge) {
        // Check for duplicates
        let exists = self.tentative_edges.iter().any(|e| {
            e.source == edge.source && e.target == edge.target && e.edge_type == edge.edge_type
        });
        if !exists {
            self.tentative_edges.push(edge);
            self.updated_at = Utc::now();
        }
    }

    /// Record a question-answer pair.
    pub fn record_answer(&mut self, question: &str, answer: &str, proposed_node_ids: Vec<String>) {
        self.answered.push(QAPair {
            question: question.to_string(),
            answer: answer.to_string(),
            phase: self.phase,
            timestamp: Utc::now(),
            nodes_proposed: proposed_node_ids,
        });
        self.updated_at = Utc::now();
    }

    /// Get a tentative node by ID (including root).
    pub fn get_tentative_node(&self, id: &str) -> Option<&TentativeNode> {
        if self.root_node.id == id {
            return Some(&self.root_node);
        }
        self.tentative_nodes.iter().find(|n| n.id == id)
    }

    /// Modify a tentative node (adjust).
    pub fn adjust_node(&mut self, node_id: &str, changes: NodePatch) -> Result<()> {
        let node = if self.root_node.id == node_id {
            &mut self.root_node
        } else {
            self.tentative_nodes
                .iter_mut()
                .find(|n| n.id == node_id)
                .ok_or_else(|| {
                    InterviewError::Session(format!("Tentative node not found: {node_id}"))
                })?
        };

        if let Some(body) = changes.body {
            node.body = body;
        }
        if let Some(status) = changes.status {
            node.status = status;
        }
        if let Some(id) = changes.id {
            node.id = id;
        }
        for (key, value) in changes.fields {
            node.fields.insert(key, value);
        }
        self.updated_at = Utc::now();

        Ok(())
    }

    /// Check if a tentative or existing node with the given type exists.
    pub fn has_node_of_type(&self, node_type: &str) -> bool {
        if self.root_node.node_type == node_type {
            return true;
        }
        self.tentative_nodes.iter().any(|n| n.node_type == node_type)
    }

    /// Check if an edge of the given type exists from the root.
    pub fn has_edge_type_from_root(&self, edge_type: &str) -> bool {
        self.tentative_edges
            .iter()
            .any(|e| e.source == self.root_node.id && e.edge_type == edge_type)
    }

    /// Count tentative nodes of a given type.
    pub fn count_nodes_of_type(&self, node_type: &str) -> usize {
        let root = if self.root_node.node_type == node_type { 1 } else { 0 };
        let others = self.tentative_nodes.iter().filter(|n| n.node_type == node_type).count();
        root + others
    }

    /// Commit all tentative nodes and edges to disk.
    pub fn commit(
        &self,
        graph_dir: &Path,
        schema: &Schema,
        sessions_dir: &Path,
    ) -> Result<CommitResult> {
        let mut created_files = Vec::new();
        let mut modified_files = Vec::new();

        // Write root node
        let root_path = write_tentative_node(graph_dir, &self.root_node, schema)?;
        created_files.push(root_path);

        // Write all tentative nodes
        for node in &self.tentative_nodes {
            let path = write_tentative_node(graph_dir, node, schema)?;
            created_files.push(path);
        }

        // Write all edges (bidirectional)
        for edge in &self.tentative_edges {
            // Check both source and target exist on disk
            let source_exists = graphforge_core::ops::find_node_file(graph_dir, &edge.source).is_ok();
            let target_exists = graphforge_core::ops::find_node_file(graph_dir, &edge.target).is_ok();

            if source_exists && target_exists {
                match ops::add_edge(graph_dir, &edge.source, &edge.target, &edge.edge_type, schema) {
                    Ok(()) => {
                        // Track modified files
                        if let Ok(p) = ops::find_node_file(graph_dir, &edge.source)
                            && !created_files.contains(&p) {
                                modified_files.push(p);
                            }
                        if let Ok(p) = ops::find_node_file(graph_dir, &edge.target)
                            && !created_files.contains(&p) {
                                modified_files.push(p);
                            }
                    }
                    Err(graphforge_core::GraphForgeError::Edge(msg)) if msg.contains("already exists") => {
                        // Edge already exists — skip silently
                    }
                    Err(e) => {
                        eprintln!("Warning: could not add edge {} -> {} ({}): {e}",
                            edge.source, edge.target, edge.edge_type);
                    }
                }
            }
        }

        // Delete session file
        self.delete(sessions_dir)?;

        modified_files.dedup();

        Ok(CommitResult {
            created_files,
            modified_files,
            warnings: Vec::new(),
        })
    }
}

/// Partial update to a tentative node.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodePatch {
    pub id: Option<String>,
    pub body: Option<String>,
    pub status: Option<String>,
    #[serde(default)]
    pub fields: HashMap<String, String>,
}

/// Summary of a session for listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub root_type: String,
    pub root_id: String,
    pub phase: InterviewPhase,
    pub node_count: usize,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Result of committing an interview session.
#[derive(Debug, Clone)]
pub struct CommitResult {
    pub created_files: Vec<PathBuf>,
    pub modified_files: Vec<PathBuf>,
    pub warnings: Vec<String>,
}

/// Write a tentative node to disk as a markdown file.
fn write_tentative_node(graph_dir: &Path, node: &TentativeNode, schema: &Schema) -> Result<PathBuf> {
    let mut owner = node.fields.get("owner").cloned();
    let tags: Option<Vec<String>> = node
        .fields
        .get("tags")
        .map(|t| t.split(',').map(|s| s.trim().to_string()).collect());

    // If owner is required but not set, default to "unknown"
    if owner.is_none()
        && let Some(node_def) = schema.node_types.get(&node.node_type)
            && node_def.required_fields.contains(&"owner".to_string()) {
                owner = Some("unknown".to_string());
            }

    // If status is empty but required, use "draft"
    let status = if node.status.is_empty() { "draft" } else { &node.status };

    let path = ops::create_node_file(
        graph_dir,
        &node.id,
        &node.node_type,
        Some(status),
        owner.as_deref(),
        tags.as_deref(),
        &node.body,
    )?;

    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_session_create() {
        let session = InterviewSession::new("feature", "feat-test", "# Test\n\nA test feature.\n");
        assert_eq!(session.root_type, "feature");
        assert_eq!(session.root_node.id, "feat-test");
        assert_eq!(session.phase, InterviewPhase::Discovery);
        assert!(session.tentative_nodes.is_empty());
    }

    #[test]
    fn test_session_save_and_load() {
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");

        let session = InterviewSession::new("feature", "feat-test", "# Test\n");
        let path = session.save(&sessions_dir).unwrap();
        assert!(path.exists());

        let loaded = InterviewSession::load(&path).unwrap();
        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.root_node.id, "feat-test");
    }

    #[test]
    fn test_session_load_by_id() {
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");

        let session = InterviewSession::new("feature", "feat-x", "# X\n");
        let sid = session.id.clone();
        session.save(&sessions_dir).unwrap();

        let loaded = InterviewSession::load_by_id(&sessions_dir, &sid).unwrap();
        assert_eq!(loaded.root_node.id, "feat-x");
    }

    #[test]
    fn test_session_list() {
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");

        let s1 = InterviewSession::new("feature", "feat-a", "# A\n");
        let s2 = InterviewSession::new("epic", "epic-b", "# B\n");
        s1.save(&sessions_dir).unwrap();
        s2.save(&sessions_dir).unwrap();

        let list = InterviewSession::list_sessions(&sessions_dir).unwrap();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_add_tentative_node() {
        let mut session = InterviewSession::new("feature", "feat-a", "# A\n");

        session.add_tentative_node(TentativeNode {
            id: "persona-eng".to_string(),
            node_type: "persona".to_string(),
            status: "".to_string(),
            fields: HashMap::new(),
            body: "# Platform Engineer\n".to_string(),
            confidence: 0.8,
            source_qa: vec![0],
        });

        assert_eq!(session.tentative_nodes.len(), 1);
        assert!(session.has_node_of_type("persona"));
    }

    #[test]
    fn test_add_tentative_node_deduplicates() {
        let mut session = InterviewSession::new("feature", "feat-a", "# A\n");

        session.add_tentative_node(TentativeNode {
            id: "persona-eng".to_string(),
            node_type: "persona".to_string(),
            status: "".to_string(),
            fields: HashMap::new(),
            body: "# V1\n".to_string(),
            confidence: 0.5,
            source_qa: vec![],
        });
        session.add_tentative_node(TentativeNode {
            id: "persona-eng".to_string(),
            node_type: "persona".to_string(),
            status: "".to_string(),
            fields: HashMap::new(),
            body: "# V2 (updated)\n".to_string(),
            confidence: 0.9,
            source_qa: vec![0],
        });

        assert_eq!(session.tentative_nodes.len(), 1);
        assert!(session.tentative_nodes[0].body.contains("V2"));
    }

    #[test]
    fn test_add_tentative_edge() {
        let mut session = InterviewSession::new("feature", "feat-a", "# A\n");

        session.add_tentative_edge(TentativeEdge {
            source: "feat-a".to_string(),
            target: "persona-eng".to_string(),
            edge_type: "serves".to_string(),
            source_type: EdgeSource::ExplicitFromAnswer,
        });

        assert_eq!(session.tentative_edges.len(), 1);
        assert!(session.has_edge_type_from_root("serves"));
    }

    #[test]
    fn test_add_tentative_edge_deduplicates() {
        let mut session = InterviewSession::new("feature", "feat-a", "# A\n");

        let edge = TentativeEdge {
            source: "feat-a".to_string(),
            target: "persona-eng".to_string(),
            edge_type: "serves".to_string(),
            source_type: EdgeSource::ExplicitFromAnswer,
        };
        session.add_tentative_edge(edge.clone());
        session.add_tentative_edge(edge);

        assert_eq!(session.tentative_edges.len(), 1);
    }

    #[test]
    fn test_record_answer() {
        let mut session = InterviewSession::new("feature", "feat-a", "# A\n");

        session.record_answer("Who is the target user?", "Platform engineers", vec!["persona-eng".to_string()]);

        assert_eq!(session.answered.len(), 1);
        assert_eq!(session.answered[0].phase, InterviewPhase::Discovery);
    }

    #[test]
    fn test_adjust_node() {
        let mut session = InterviewSession::new("feature", "feat-a", "# A\n");

        session.adjust_node("feat-a", NodePatch {
            body: Some("# Updated Body\n".to_string()),
            status: Some("active".to_string()),
            ..Default::default()
        }).unwrap();

        assert_eq!(session.root_node.body, "# Updated Body\n");
        assert_eq!(session.root_node.status, "active");
    }

    #[test]
    fn test_adjust_tentative_node() {
        let mut session = InterviewSession::new("feature", "feat-a", "# A\n");
        session.add_tentative_node(TentativeNode {
            id: "persona-eng".to_string(),
            node_type: "persona".to_string(),
            status: "".to_string(),
            fields: HashMap::new(),
            body: "# Old\n".to_string(),
            confidence: 0.5,
            source_qa: vec![],
        });

        session.adjust_node("persona-eng", NodePatch {
            body: Some("# Updated Persona\n".to_string()),
            ..Default::default()
        }).unwrap();

        assert!(session.tentative_nodes[0].body.contains("Updated Persona"));
    }

    #[test]
    fn test_adjust_nonexistent_node_errors() {
        let mut session = InterviewSession::new("feature", "feat-a", "# A\n");
        let result = session.adjust_node("nonexistent", NodePatch::default());
        assert!(result.is_err());
    }

    #[test]
    fn test_commit() {
        let tmp = TempDir::new().unwrap();
        let graph_dir = tmp.path().join("graph");
        let sessions_dir = tmp.path().join("sessions");

        // Create graph directories
        for dir in &["features", "personas", "epics"] {
            std::fs::create_dir_all(graph_dir.join(dir)).unwrap();
        }

        let schema_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent().unwrap().parent().unwrap()
            .join("schema/default-schema.toml");
        let schema = graphforge_core::schema::Schema::load(&schema_path).unwrap();

        let mut session = InterviewSession::new("feature", "feat-test", "# Test Feature\n\nA test.\n");
        session.root_node.fields.insert("owner".to_string(), "caleb".to_string());

        session.add_tentative_node(TentativeNode {
            id: "persona-dev".to_string(),
            node_type: "persona".to_string(),
            status: "".to_string(),
            fields: HashMap::new(),
            body: "# Developer\n\nWrites code.\n".to_string(),
            confidence: 0.9,
            source_qa: vec![0],
        });

        session.save(&sessions_dir).unwrap();

        let result = session.commit(&graph_dir, &schema, &sessions_dir).unwrap();
        assert_eq!(result.created_files.len(), 2);
        assert!(graph_dir.join("features/feat-test.md").exists());
        assert!(graph_dir.join("personas/persona-dev.md").exists());

        // Session file should be deleted
        assert!(InterviewSession::list_sessions(&sessions_dir).unwrap().is_empty());
    }

    #[test]
    fn test_count_nodes_of_type() {
        let mut session = InterviewSession::new("feature", "feat-a", "# A\n");
        session.add_tentative_node(TentativeNode {
            id: "persona-a".to_string(),
            node_type: "persona".to_string(),
            status: "".to_string(),
            fields: HashMap::new(),
            body: "# P\n".to_string(),
            confidence: 0.8,
            source_qa: vec![],
        });

        assert_eq!(session.count_nodes_of_type("feature"), 1);
        assert_eq!(session.count_nodes_of_type("persona"), 1);
        assert_eq!(session.count_nodes_of_type("task"), 0);
    }

    #[test]
    fn test_session_delete() {
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");

        let session = InterviewSession::new("feature", "feat-a", "# A\n");
        let path = session.save(&sessions_dir).unwrap();
        assert!(path.exists());

        session.delete(&sessions_dir).unwrap();
        assert!(!path.exists());
    }
}
