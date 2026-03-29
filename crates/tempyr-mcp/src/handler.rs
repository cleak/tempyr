use std::collections::{HashMap, HashSet};

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

use crate::protocol::JsonRpcResponse;

/// Resolve the project context from the current directory.
fn find_project() -> Result<(std::path::PathBuf, std::path::PathBuf, Schema), String> {
    let root = tempyr_core::project::find_project_root().ok_or_else(|| {
        "Not a tempyr project (no .tempyr/ or .tempyr-redirect found)".to_string()
    })?;
    let gf_dir = root.join(".tempyr");
    let schema_path = gf_dir.join("schema.toml");
    let schema = Schema::load(&schema_path).map_err(|e| e.to_string())?;
    let graph_dir = root.join("graph");
    Ok((graph_dir, gf_dir, schema))
}

pub fn handle_initialize(id: Value) -> JsonRpcResponse {
    JsonRpcResponse::success(
        id,
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "resources": {},
                "tools": {}
            },
            "serverInfo": {
                "name": "tempyr",
                "version": "0.1.0"
            }
        }),
    )
}

pub fn handle_tools_list(id: Value) -> JsonRpcResponse {
    JsonRpcResponse::success(
        id,
        json!({
            "tools": [
                {
                    "name": "graph_search",
                    "description": "Full-text keyword search across all graph nodes. Searches body text, titles, and tags. Optionally filter results by metadata (type, status, owner).",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "query": {"type": "string", "description": "Search terms (matched against body text, titles, tags)"},
                            "max_results": {"type": "integer", "default": 10},
                            "node_type": {"type": "string", "description": "Filter by node type (e.g. task, feature, decision)"},
                            "status": {"type": "string", "description": "Filter by status (e.g. backlog, in_progress, done, draft, active)"},
                            "owner": {"type": "string", "description": "Filter by owner"}
                        },
                        "required": ["query"]
                    }
                },
                {
                    "name": "graph_list",
                    "description": "List graph nodes by metadata filters. Unlike graph_search, no search query is needed — filters on type, status, and owner directly. Use this to find tasks by status (e.g. all backlog tasks), nodes by owner, etc.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "node_type": {"type": "string", "description": "Filter by node type (e.g. task, feature, decision, epic)"},
                            "status": {"type": "string", "description": "Filter by status (e.g. backlog, in_progress, done, draft, active, blocked)"},
                            "owner": {"type": "string", "description": "Filter by owner"},
                            "max_results": {"type": "integer", "default": 50}
                        }
                    }
                },
                {
                    "name": "graph_context",
                    "description": "Hybrid retrieval combining structural traversal, keyword search, and semantic search",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "query": {"type": "string"},
                            "root_node": {"type": "string"},
                            "token_budget": {"type": "integer", "default": 8000}
                        },
                        "required": ["query"]
                    }
                },
                {
                    "name": "graph_traverse",
                    "description": "Follow edges from a node to find connected nodes",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "node_id": {"type": "string"},
                            "depth": {"type": "integer", "default": 2},
                            "edge_types": {"type": "array", "items": {"type": "string"}}
                        },
                        "required": ["node_id"]
                    }
                },
                {
                    "name": "graph_get_node",
                    "description": "Get the full content of a specific node by ID",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "node_id": {"type": "string"}
                        },
                        "required": ["node_id"]
                    }
                },
                {
                    "name": "graph_add_node",
                    "description": "Create a new node in the graph. Provide a human-readable slug; the system generates a 6-char suffix to form the full ID (e.g. slug 'session-replay' → ID 'session-replay-a1b2c3'). The generated full ID is returned. Use graph_update_node to modify existing nodes.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "slug": {"type": "string", "description": "Human-readable kebab-case slug (e.g. 'session-replay', 'ore-bucket'). Do NOT include type prefixes like 'feat-' — the node_type field handles that."},
                            "node_type": {"type": "string"},
                            "status": {"type": "string"},
                            "body": {"type": "string"},
                            "owner": {"type": "string"},
                            "tags": {"type": "array", "items": {"type": "string"}}
                        },
                        "required": ["slug", "node_type", "body"]
                    }
                },
                {
                    "name": "graph_update_node",
                    "description": "Update an existing node's body, status, owner, or tags. Only provided fields are changed.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "node_id": {"type": "string", "description": "Full node ID (e.g. 'session-replay-a1b2c3') or 6-char suffix (e.g. 'a1b2c3')"},
                            "body": {"type": "string", "description": "New markdown body (replaces entire body)"},
                            "status": {"type": "string", "description": "New status value"},
                            "owner": {"type": "string", "description": "New owner"},
                            "tags": {"type": "array", "items": {"type": "string"}, "description": "New tags (replaces all tags)"}
                        },
                        "required": ["node_id"]
                    }
                },
                {
                    "name": "graph_add_edge",
                    "description": "Add a directed edge between two existing nodes. The reverse edge is written automatically. Valid source→target edge types: epic→feature(parent_of), epic→persona(serves), epic→metric(measured_by); feature→epic(child_of), feature→persona(serves), feature→constraint(constrained_by), feature→decision(depends_on), feature→feature(depends_on), feature→metric(measured_by), feature→risk(has_risk), feature→task(decomposes_to), feature→open_question(has_question), feature→component(uses), feature→api_surface(exposes), feature→insight(informed_by); task→feature(child_of), task→task(child_of/blocked_by), task→decision(blocked_by), task→open_question(blocked_by/has_question), task→component(uses); decision→feature(decision_for), decision→component(decision_for), decision→constraint(constrained_by), decision→decision(supersedes), decision→open_question(has_question); risk→feature(risk_for), risk→task(mitigated_by); note→*(relates_to); insight→component/feature/decision/insight(relates_to).",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "source": {"type": "string", "description": "Full node ID (e.g. 'session-replay-a1b2c3') or 6-char suffix (e.g. 'a1b2c3')"},
                            "target": {"type": "string", "description": "Full node ID (e.g. 'session-replay-a1b2c3') or 6-char suffix (e.g. 'a1b2c3')"},
                            "edge_type": {"type": "string", "description": "Edge type from source's perspective (e.g. 'child_of', 'has_risk'). See tool description for valid combinations."}
                        },
                        "required": ["source", "target", "edge_type"]
                    }
                },
                {
                    "name": "graph_validate",
                    "description": "Validate graph consistency. Returns any errors or warnings.",
                    "inputSchema": { "type": "object", "properties": {} }
                },
                {
                    "name": "graph_render",
                    "description": "Render a document (PRD, TDD) from a root node",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "template": {"type": "string"},
                            "root_node": {"type": "string"},
                            "as_of": {"type": "string"},
                            "include_history": {"type": "boolean", "default": false}
                        },
                        "required": ["template", "root_node"]
                    }
                },
                {
                    "name": "graph_stats",
                    "description": "Get graph statistics: node counts by type, edge counts",
                    "inputSchema": { "type": "object", "properties": {} }
                },
                {
                    "name": "interview_start",
                    "description": "Start a new interview session from a brain dump or idea description. Returns tentative nodes, existing graph context, gaps to explore, and the first questions to ask.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "brain_dump": {"type": "string", "description": "The user's raw input describing what they want to build/plan"},
                            "root_type": {"type": "string", "description": "Type of root node: feature (default), epic, or component", "default": "feature"}
                        },
                        "required": ["brain_dump"]
                    }
                },
                {
                    "name": "interview_answer",
                    "description": "Process a user's answer during an interview. Updates session state, fills gaps, may advance the interview phase. Returns what changed and the next questions.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "session_id": {"type": "string"},
                            "answer": {"type": "string", "description": "The user's answer text"}
                        },
                        "required": ["session_id", "answer"]
                    }
                },
                {
                    "name": "interview_show",
                    "description": "Return the full tentative graph state for review. Shows all proposed nodes grouped by type, edges, remaining gaps, and Q&A history.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "session_id": {"type": "string"}
                        },
                        "required": ["session_id"]
                    }
                },
                {
                    "name": "interview_commit",
                    "description": "Write all tentative nodes and edges to disk, creating graph files. Validates the resulting graph. Deletes the session on success.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "session_id": {"type": "string"}
                        },
                        "required": ["session_id"]
                    }
                },
                {
                    "name": "interview_adjust",
                    "description": "Modify a tentative node during an interview (change body, status, or ID). Re-runs gap analysis after the adjustment.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "session_id": {"type": "string"},
                            "node_id": {"type": "string", "description": "ID of the tentative node to modify"},
                            "body": {"type": "string", "description": "New body content"},
                            "status": {"type": "string", "description": "New status"},
                            "new_id": {"type": "string", "description": "New ID (rename)"}
                        },
                        "required": ["session_id", "node_id"]
                    }
                },
                {
                    "name": "interview_resume",
                    "description": "Resume an interrupted interview session. Returns the full current state so the conversation can continue.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "session_id": {"type": "string"}
                        },
                        "required": ["session_id"]
                    }
                },
                {
                    "name": "interview_add_node",
                    "description": "Add a tentative node to an active interview session. The node is stored in session state (not written to disk) until interview_commit. Automatically re-analyzes gaps and may advance the interview phase.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "session_id": {"type": "string"},
                            "slug": {"type": "string", "description": "Human-readable kebab-case slug (no type prefix). A 6-char suffix is appended automatically."},
                            "node_type": {"type": "string", "description": "Node type: feature, epic, task, decision, constraint, persona, metric, risk, open_question, component, api_surface, insight, note"},
                            "body": {"type": "string", "description": "Markdown body content"},
                            "status": {"type": "string", "description": "Node status (default: draft)", "default": "draft"},
                            "confidence": {"type": "number", "description": "Extraction confidence 0.0-1.0 (0.9+ explicit, 0.6-0.8 inferred)", "default": 0.9}
                        },
                        "required": ["session_id", "slug", "node_type", "body"]
                    }
                },
                {
                    "name": "interview_add_edge",
                    "description": "Add a tentative edge to an active interview session. Both source and target can be tentative node IDs (from interview_add_node) or existing graph node IDs. Stored in session state until interview_commit.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "session_id": {"type": "string"},
                            "source": {"type": "string", "description": "Source node ID (full ID or 6-char suffix)"},
                            "target": {"type": "string", "description": "Target node ID (full ID or 6-char suffix)"},
                            "edge_type": {"type": "string", "description": "Edge type: child_of, serves, measured_by, constrained_by, depends_on, has_risk, decomposes_to, uses, has_question, etc."}
                        },
                        "required": ["session_id", "source", "target", "edge_type"]
                    }
                },
                {
                    "name": "linear_push",
                    "description": "Push graph node(s) to Linear with full context and data lineage. Creates or updates Linear issues/projects with rich descriptions containing parent context, related decisions, blocking items, and MCP breadcrumbs.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "node_id": {"type": "string", "description": "Specific node ID to push (omit for all syncable nodes)"},
                            "dry_run": {"type": "boolean", "default": false}
                        }
                    }
                },
                {
                    "name": "linear_pull",
                    "description": "Pull changes from Linear into the graph. Updates node statuses based on Linear issue state changes. Creates new graph nodes for issues created in Linear under tracked projects/parents.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "dry_run": {"type": "boolean", "default": false}
                        }
                    }
                },
                {
                    "name": "linear_sync",
                    "description": "Bidirectional sync: push local graph changes to Linear, then pull remote changes back. Reports conflicts when both sides changed.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "dry_run": {"type": "boolean", "default": false}
                        }
                    }
                },
                {
                    "name": "linear_status",
                    "description": "Show Linear sync state: linked nodes, pending changes, stale entries, and conflicts.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {}
                    }
                }
            ]
        }),
    )
}

pub fn handle_tool_call(id: Value, params: Value) -> JsonRpcResponse {
    let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

    let result = match tool_name {
        "graph_search" => tool_graph_search(&arguments),
        "graph_list" => tool_graph_list(&arguments),
        "graph_context" => tool_graph_context(&arguments),
        "graph_traverse" => tool_graph_traverse(&arguments),
        "graph_get_node" => tool_graph_get_node(&arguments),
        "graph_add_node" => tool_graph_add_node(&arguments),
        "graph_update_node" => tool_graph_update_node(&arguments),
        "graph_add_edge" => tool_graph_add_edge(&arguments),
        "graph_validate" => tool_graph_validate(&arguments),
        "graph_render" => tool_graph_render(&arguments),
        "graph_stats" => tool_graph_stats(&arguments),
        "interview_start" => tool_interview_start(&arguments),
        "interview_answer" => tool_interview_answer(&arguments),
        "interview_show" => tool_interview_show(&arguments),
        "interview_commit" => tool_interview_commit(&arguments),
        "interview_adjust" => tool_interview_adjust(&arguments),
        "interview_resume" => tool_interview_resume(&arguments),
        "interview_add_node" => tool_interview_add_node(&arguments),
        "interview_add_edge" => tool_interview_add_edge(&arguments),
        "linear_push" => tool_linear_push(&arguments),
        "linear_pull" => tool_linear_pull(&arguments),
        "linear_sync" => tool_linear_sync(&arguments),
        "linear_status" => tool_linear_status(&arguments),
        _ => Err(format!("Unknown tool: {tool_name}")),
    };

    match result {
        Ok(content) => JsonRpcResponse::success(
            id,
            json!({
                "content": [{"type": "text", "text": content}]
            }),
        ),
        Err(e) => JsonRpcResponse::success(
            id,
            json!({
                "content": [{"type": "text", "text": format!("Error: {e}")}],
                "isError": true
            }),
        ),
    }
}

fn tool_graph_search(args: &Value) -> Result<String, String> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'query'")?;
    let max_results = args
        .get("max_results")
        .and_then(|v| v.as_u64())
        .unwrap_or(10) as usize;
    let node_type = args.get("node_type").and_then(|v| v.as_str());
    let status = args.get("status").and_then(|v| v.as_str());
    let owner = args.get("owner").and_then(|v| v.as_str());

    let (_, gf_dir, _) = find_project()?;
    let index_path = gf_dir.join("index.db");
    let index = Index::open(&index_path).map_err(|e| format!("Index: {e}"))?;

    let filter = MetadataFilter {
        node_type,
        status,
        owner,
    };
    let results = index
        .search_fts_with_metadata(query, &filter, max_results)
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

fn tool_graph_list(args: &Value) -> Result<String, String> {
    let node_type = args.get("node_type").and_then(|v| v.as_str());
    let status = args.get("status").and_then(|v| v.as_str());
    let owner = args.get("owner").and_then(|v| v.as_str());
    let max_results = args
        .get("max_results")
        .and_then(|v| v.as_u64())
        .unwrap_or(50) as usize;

    let (_, gf_dir, _) = find_project()?;
    let index_path = gf_dir.join("index.db");
    let index = Index::open(&index_path).map_err(|e| format!("Index: {e}"))?;

    let filter = MetadataFilter {
        node_type,
        status,
        owner,
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

fn tool_graph_context(args: &Value) -> Result<String, String> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'query'")?;
    let root = args.get("root_node").and_then(|v| v.as_str());
    let budget = args
        .get("token_budget")
        .and_then(|v| v.as_u64())
        .unwrap_or(8000) as usize;

    let (graph_dir, gf_dir, schema) = find_project()?;
    let resolved_root = root
        .map(|r| ops::resolve_node_id(&graph_dir, r).map_err(|e| e.to_string()))
        .transpose()?;
    let graph = Graph::load_from_directory(&graph_dir, schema).map_err(|e| e.to_string())?;
    let index_path = gf_dir.join("index.db");
    let index = Index::open(&index_path).map_err(|e| format!("Index: {e}"))?;

    let config = RetrievalConfig {
        token_budget: budget,
        ..RetrievalConfig::standard()
    };
    let results = hybrid_retrieve(&index, &graph, query, resolved_root.as_deref(), &config)
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

fn tool_graph_traverse(args: &Value) -> Result<String, String> {
    let node_id = args
        .get("node_id")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'node_id'")?;
    let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(2) as usize;

    let (graph_dir, _, schema) = find_project()?;
    let resolved = ops::resolve_node_id(&graph_dir, node_id).map_err(|e| e.to_string())?;
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

fn tool_graph_get_node(args: &Value) -> Result<String, String> {
    let node_id = args
        .get("node_id")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'node_id'")?;

    let (graph_dir, _, _) = find_project()?;
    let resolved = ops::resolve_node_id(&graph_dir, node_id).map_err(|e| e.to_string())?;
    let path = ops::find_node_file(&graph_dir, &resolved).map_err(|e| e.to_string())?;
    let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;

    Ok(content)
}

fn tool_graph_add_node(args: &Value) -> Result<String, String> {
    let slug = args
        .get("slug")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'slug'")?;
    let node_type = args
        .get("node_type")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'node_type'")?;
    let body = args
        .get("body")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'body'")?;
    let status = args.get("status").and_then(|v| v.as_str());
    let owner = args.get("owner").and_then(|v| v.as_str());
    let tags: Option<Vec<String>> = args.get("tags").and_then(|v| {
        v.as_array().map(|arr| {
            arr.iter()
                .filter_map(|t| t.as_str().map(String::from))
                .collect()
        })
    });

    let (graph_dir, _, _) = find_project()?;
    let (generated_id, path) = ops::create_node_file_auto_id(
        &graph_dir,
        slug,
        node_type,
        status,
        owner,
        tags.as_deref(),
        body,
    )
    .map_err(|e| e.to_string())?;

    Ok(format!(
        "Created node '{generated_id}' at {}",
        path.display()
    ))
}

fn tool_graph_update_node(args: &Value) -> Result<String, String> {
    let node_id = args
        .get("node_id")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'node_id'")?;
    let body = args.get("body").and_then(|v| v.as_str());
    let status = args.get("status").and_then(|v| v.as_str());
    let owner = args.get("owner").and_then(|v| v.as_str());
    let tags: Option<Vec<String>> = args.get("tags").and_then(|v| {
        v.as_array().map(|arr| {
            arr.iter()
                .filter_map(|t| t.as_str().map(String::from))
                .collect()
        })
    });

    let (graph_dir, _, schema) = find_project()?;
    let resolved = ops::resolve_node_id(&graph_dir, node_id).map_err(|e| e.to_string())?;
    let path = ops::update_node(
        &graph_dir,
        &resolved,
        body,
        status,
        owner,
        tags.as_deref(),
        &schema,
    )
    .map_err(|e| e.to_string())?;

    let mut changed: Vec<&str> = Vec::new();
    if body.is_some() {
        changed.push("body");
    }
    if status.is_some() {
        changed.push("status");
    }
    if owner.is_some() {
        changed.push("owner");
    }
    if tags.is_some() {
        changed.push("tags");
    }

    Ok(format!(
        "Updated node '{resolved}' ({}) at {}",
        changed.join(", "),
        path.display()
    ))
}

fn tool_graph_add_edge(args: &Value) -> Result<String, String> {
    let source = args
        .get("source")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'source'")?;
    let target = args
        .get("target")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'target'")?;
    let edge_type = args
        .get("edge_type")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'edge_type'")?;

    let (graph_dir, _, schema) = find_project()?;
    let resolved_source = ops::resolve_node_id(&graph_dir, source).map_err(|e| e.to_string())?;
    let resolved_target = ops::resolve_node_id(&graph_dir, target).map_err(|e| e.to_string())?;
    ops::add_edge(
        &graph_dir,
        &resolved_source,
        &resolved_target,
        edge_type,
        &schema,
    )
    .map_err(|e| e.to_string())?;

    let reverse = schema.reverse_edge_type(edge_type).unwrap_or("?");
    Ok(format!(
        "Added edge: {resolved_source} --{edge_type}--> {resolved_target} (reverse: {reverse})"
    ))
}

fn tool_graph_validate(_args: &Value) -> Result<String, String> {
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

fn tool_graph_render(args: &Value) -> Result<String, String> {
    let template = args
        .get("template")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'template'")?;
    let root_raw = args
        .get("root_node")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'root_node'")?;
    let include_history = args
        .get("include_history")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let (graph_dir, gf_dir, schema) = find_project()?;
    let root_id = ops::resolve_node_id(&graph_dir, root_raw).map_err(|e| e.to_string())?;
    let graph = Graph::load_from_directory(&graph_dir, schema).map_err(|e| e.to_string())?;

    let filter = if include_history {
        TemporalFilter::with_history()
    } else {
        TemporalFilter::current()
    };

    // Try project-local template first, then built-in
    let local_path = gf_dir.join("render").join(format!("{template}.toml"));
    if local_path.exists() {
        tempyr_render::render(&graph, &local_path, &root_id, &filter).map_err(|e| e.to_string())
    } else {
        let template_toml = match template {
            "prd" => include_str!("../../../templates/prd.toml"),
            "tdd" => include_str!("../../../templates/tdd.toml"),
            "task-prompt" => include_str!("../../../templates/task-prompt.toml"),
            _ => return Err(format!("Unknown template: '{template}'")),
        };
        tempyr_render::render_from_str(&graph, template_toml, &root_id, &filter)
            .map_err(|e| e.to_string())
    }
}

fn tool_graph_stats(_args: &Value) -> Result<String, String> {
    let (graph_dir, _gf_dir, schema) = find_project()?;
    let graph = Graph::load_from_directory(&graph_dir, schema).map_err(|e| e.to_string())?;

    let mut type_counts: HashMap<String, usize> = HashMap::new();
    for node in graph.nodes.values() {
        *type_counts.entry(node.node_type().to_string()).or_default() += 1;
    }

    let stats = json!({
        "node_count": graph.node_count(),
        "edge_count": graph.edge_count(),
        "nodes_by_type": type_counts,
    });

    serde_json::to_string_pretty(&stats).map_err(|e| e.to_string())
}

// ── Interview tools ─────────────────────────────────────────────────────

/// Helper to get the sessions directory.
fn sessions_dir(gf_dir: &std::path::Path) -> std::path::PathBuf {
    gf_dir.join("sessions")
}

/// Helper to serialize a session state for MCP responses.
fn session_state_json(session: &InterviewSession, _schema: &Schema) -> Value {
    let questions = next_questions(session, 3);
    let progress = proposer::compute_progress(session);

    let mut nodes_by_type: HashMap<String, Vec<Value>> = HashMap::new();
    // Include root node
    nodes_by_type
        .entry(session.root_node.node_type.clone())
        .or_default()
        .push(json!({
            "id": session.root_node.id,
            "status": session.root_node.status,
            "body_preview": session.root_node.body.lines().take(3).collect::<Vec<_>>().join("\n"),
            "confidence": session.root_node.confidence,
        }));
    // Include tentative nodes
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

fn tool_interview_start(args: &Value) -> Result<String, String> {
    let brain_dump = args
        .get("brain_dump")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'brain_dump'")?;
    let root_type = args
        .get("root_type")
        .and_then(|v| v.as_str())
        .unwrap_or("feature");

    let (graph_dir, gf_dir, schema) = find_project()?;
    let sessions = sessions_dir(&gf_dir);

    // Load graph for context-aware gap detection (degrade gracefully)
    let graph = Graph::load_from_directory(&graph_dir, schema.clone()).ok();

    // Search existing graph for context (degrade gracefully if no index)
    let mut existing_ids = Vec::new();
    let mut context_rich = Vec::new();

    let index_path = gf_dir.join("index.db");
    if index_path.exists() {
        if let Ok(index) = Index::open(&index_path) {
            if let Ok(results) = index.search_fts_filtered(brain_dump, None, 20) {
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
        }
    }

    let existing_suffixes = tempyr_core::id::collect_existing_suffixes(&graph_dir);
    let mut result = proposer::interview_start(
        brain_dump,
        root_type,
        &schema,
        &existing_ids,
        &existing_suffixes,
    )
    .map_err(|e| e.to_string())?;

    // Populate rich context
    result.session.graph_context_rich = context_rich;

    // Re-run gap detection with graph for context-aware existing_related + question_type
    if let Some(ref g) = graph {
        let gaps =
            tempyr_interview::gaps::detect_gaps_with_graph(&result.session, &schema, Some(g));
        result.questions = gaps.iter().take(3).cloned().collect();
        result.session.remaining_gaps = gaps;
    }

    // Save session
    result.session.save(&sessions).map_err(|e| e.to_string())?;

    let state = session_state_json(&result.session, &schema);
    serde_json::to_string_pretty(&state).map_err(|e| e.to_string())
}

fn tool_interview_answer(args: &Value) -> Result<String, String> {
    let session_id = args
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'session_id'")?;
    let answer = args
        .get("answer")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'answer'")?;

    let (graph_dir, gf_dir, schema) = find_project()?;
    let sessions = sessions_dir(&gf_dir);

    // Load graph for context-aware gap detection
    let graph = Graph::load_from_directory(&graph_dir, schema.clone()).ok();

    let mut session =
        InterviewSession::load_by_id(&sessions, session_id).map_err(|e| e.to_string())?;

    // Capture the current top gaps as the "question" context
    let question_context: String = next_questions(&session, 3)
        .iter()
        .map(|g| g.suggested_question.as_str())
        .collect::<Vec<_>>()
        .join(" | ");

    // Record answer, then reanalyze with graph context
    session.record_answer(&question_context, answer, vec![]);
    let update = proposer::reanalyze_with_graph(&mut session, &schema, graph.as_ref());

    // Save session
    session.save(&sessions).map_err(|e| e.to_string())?;

    let response = json!({
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
    });

    serde_json::to_string_pretty(&response).map_err(|e| e.to_string())
}

fn tool_interview_show(args: &Value) -> Result<String, String> {
    let session_id = args
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'session_id'")?;

    let (_, gf_dir, schema) = find_project()?;
    let sessions = sessions_dir(&gf_dir);

    let session = InterviewSession::load_by_id(&sessions, session_id).map_err(|e| e.to_string())?;

    let state = session_state_json(&session, &schema);
    serde_json::to_string_pretty(&state).map_err(|e| e.to_string())
}

fn tool_interview_commit(args: &Value) -> Result<String, String> {
    let session_id = args
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'session_id'")?;

    let (graph_dir, gf_dir, schema) = find_project()?;
    let sessions = sessions_dir(&gf_dir);

    let session = InterviewSession::load_by_id(&sessions, session_id).map_err(|e| e.to_string())?;

    let result = session
        .commit(&graph_dir, &schema, &sessions)
        .map_err(|e| e.to_string())?;

    // Run validation on the resulting graph
    let mut all_warnings = result.warnings.clone();
    let validation_warnings = {
        let graph = Graph::load_from_directory(&graph_dir, schema).map_err(|e| e.to_string())?;
        let issues = validate_graph(&graph);
        issues.iter().map(|i| i.to_string()).collect::<Vec<_>>()
    };
    all_warnings.extend(validation_warnings.clone());

    // Attempt incremental index update (degrade gracefully)
    let index_path = gf_dir.join("index.db");
    if index_path.exists() {
        if let Err(e) = (|| -> std::result::Result<(), String> {
            let schema2 = Schema::load(&gf_dir.join("schema.toml")).map_err(|e| e.to_string())?;
            let graph =
                Graph::load_from_directory(&graph_dir, schema2).map_err(|e| e.to_string())?;
            let index = Index::open(&index_path).map_err(|e| e.to_string())?;
            index
                .incremental_update(&graph)
                .map_err(|e| e.to_string())?;
            Ok(())
        })() {
            all_warnings.push(format!(
                "Index update failed (run 'tempyr index rebuild'): {e}"
            ));
        }
    }

    let response = json!({
        "files_created": result.created_files.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        "files_modified": result.modified_files.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        "warnings": all_warnings,
        "node_count": result.node_count,
        "edge_count": result.edge_count,
    });

    serde_json::to_string_pretty(&response).map_err(|e| e.to_string())
}

fn tool_interview_adjust(args: &Value) -> Result<String, String> {
    let session_id = args
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'session_id'")?;
    let node_id = args
        .get("node_id")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'node_id'")?;

    let (_, gf_dir, schema) = find_project()?;
    let sessions = sessions_dir(&gf_dir);

    let mut session =
        InterviewSession::load_by_id(&sessions, session_id).map_err(|e| e.to_string())?;

    let new_id = args
        .get("new_id")
        .and_then(|v| v.as_str())
        .map(String::from);

    let patch = NodePatch {
        id: new_id.clone(),
        body: args.get("body").and_then(|v| v.as_str()).map(String::from),
        status: args
            .get("status")
            .and_then(|v| v.as_str())
            .map(String::from),
        ..Default::default()
    };

    session
        .adjust_node(node_id, patch)
        .map_err(|e| e.to_string())?;

    // If the node was renamed, update all edge references
    if let Some(ref new_id) = new_id {
        for edge in &mut session.tentative_edges {
            if edge.source == node_id {
                edge.source = new_id.clone();
            }
            if edge.target == node_id {
                edge.target = new_id.clone();
            }
        }
    }

    // Reanalyze gaps after adjustment (without recording a phantom QA pair)
    let _update = proposer::reanalyze(&mut session, &schema);

    session.save(&sessions).map_err(|e| e.to_string())?;

    let state = session_state_json(&session, &schema);
    serde_json::to_string_pretty(&state).map_err(|e| e.to_string())
}

fn tool_interview_resume(args: &Value) -> Result<String, String> {
    let session_id = args
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'session_id'")?;

    let (_, gf_dir, schema) = find_project()?;
    let sessions = sessions_dir(&gf_dir);

    let session = InterviewSession::load_by_id(&sessions, session_id).map_err(|e| e.to_string())?;

    let state = session_state_json(&session, &schema);
    serde_json::to_string_pretty(&state).map_err(|e| e.to_string())
}

fn tool_interview_add_node(args: &Value) -> Result<String, String> {
    let session_id = args
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'session_id'")?;
    let slug = args
        .get("slug")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'slug'")?;
    let node_type = args
        .get("node_type")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'node_type'")?;
    let body = args
        .get("body")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'body'")?;
    let status = args
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("draft");
    let confidence = args
        .get("confidence")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.9) as f32;

    let (graph_dir, gf_dir, schema) = find_project()?;
    let sessions = sessions_dir(&gf_dir);
    let graph = Graph::load_from_directory(&graph_dir, schema.clone()).ok();

    let mut session =
        InterviewSession::load_by_id(&sessions, session_id).map_err(|e| e.to_string())?;

    // Generate hybrid ID (same logic as proposer::add_proposed_node, but we
    // reanalyze only once with graph context instead of twice).
    let node_id = if id::is_hybrid_id(slug) {
        slug.to_string()
    } else {
        let existing: HashSet<String> = session
            .tentative_nodes
            .iter()
            .filter_map(|n| id::parse_node_id(&n.id).map(|p| p.suffix))
            .collect();
        id::make_node_id(slug, &existing)
    };

    session.add_tentative_node(TentativeNode {
        id: node_id.clone(),
        node_type: node_type.to_string(),
        status: status.to_string(),
        fields: HashMap::new(),
        body: body.to_string(),
        confidence,
        source_qa: vec![session.answered.len()],
    });

    let update = proposer::reanalyze_with_graph(&mut session, &schema, graph.as_ref());

    session.save(&sessions).map_err(|e| e.to_string())?;

    let response = json!({
        "session_id": session.id,
        "node_id": node_id,
        "node_type": node_type,
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
    });

    serde_json::to_string_pretty(&response).map_err(|e| e.to_string())
}

fn tool_interview_add_edge(args: &Value) -> Result<String, String> {
    let session_id = args
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'session_id'")?;
    let source = args
        .get("source")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'source'")?;
    let target = args
        .get("target")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'target'")?;
    let edge_type = args
        .get("edge_type")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'edge_type'")?;

    let (graph_dir, gf_dir, schema) = find_project()?;
    let sessions = sessions_dir(&gf_dir);
    let graph = Graph::load_from_directory(&graph_dir, schema.clone()).ok();

    let mut session =
        InterviewSession::load_by_id(&sessions, session_id).map_err(|e| e.to_string())?;

    // Resolve source/target: check tentative nodes first, then try disk resolution
    let resolved_source = resolve_interview_node_id(&session, &graph_dir, source)?;
    let resolved_target = resolve_interview_node_id(&session, &graph_dir, target)?;

    session.add_tentative_edge(TentativeEdge {
        source: resolved_source.clone(),
        target: resolved_target.clone(),
        edge_type: edge_type.to_string(),
        source_type: EdgeSource::ExplicitFromAnswer,
    });

    let update = proposer::reanalyze_with_graph(&mut session, &schema, graph.as_ref());

    session.save(&sessions).map_err(|e| e.to_string())?;

    let response = json!({
        "session_id": session.id,
        "edge": format!("{resolved_source} --{edge_type}--> {resolved_target}"),
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
    });

    serde_json::to_string_pretty(&response).map_err(|e| e.to_string())
}

/// Resolve a node ID within an interview context.
/// Checks tentative nodes first (root + proposed), then falls back to disk resolution.
/// Returns an error if the suffix matches multiple tentative nodes (ambiguity).
fn resolve_interview_node_id(
    session: &InterviewSession,
    graph_dir: &std::path::Path,
    input: &str,
) -> Result<String, String> {
    let suffix_pattern = format!("-{input}");

    // Collect all tentative matches (root + proposed)
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
        0 => {
            // Fall back to disk resolution for existing graph nodes
            ops::resolve_node_id(graph_dir, input)
                .map_err(|e| format!("Node '{input}' not found in session or on disk: {e}"))
        }
        _ => Err(format!(
            "Ambiguous node ID '{input}' matches multiple tentative nodes: {}",
            matches.join(", ")
        )),
    }
}

// ─── Linear Tools ──────────────────────────────────────

fn build_linear_deps() -> Result<
    (
        LinearClient,
        LinearConfig,
        std::path::PathBuf,
        std::path::PathBuf,
        Schema,
    ),
    String,
> {
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

fn tool_linear_push(args: &Value) -> Result<String, String> {
    let node_id_raw = args.get("node_id").and_then(|v| v.as_str());
    let dry_run = args
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let (client, config, gf_dir, graph_dir, schema) = build_linear_deps()?;
    let resolved_node_id = node_id_raw
        .map(|r| ops::resolve_node_id(&graph_dir, r).map_err(|e| e.to_string()))
        .transpose()?;
    let node_id = resolved_node_id.as_deref();
    let graph =
        Graph::load_from_directory(&graph_dir, schema.clone()).map_err(|e| e.to_string())?;
    let index = Index::open(&gf_dir.join("index.db")).ok();
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
                if let Some(node) = graph.get_node(id) {
                    if node.content_hash != entry.content_hash_at_sync {
                        would_update += 1;
                    }
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

    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(async {
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
}

fn tool_linear_pull(args: &Value) -> Result<String, String> {
    let dry_run = args
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

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

    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(async {
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
        serde_json::to_string_pretty(&json!({
            "created": result.created,
            "updated": result.updated,
            "status_changed": result.status_changed.len(),
            "conflicts": result.conflicts.len(),
            "warnings": result.warnings,
            "errors": result.errors,
        }))
        .map_err(|e| e.to_string())
    })
}

fn tool_linear_sync(args: &Value) -> Result<String, String> {
    let dry_run = args
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let (client, config, gf_dir, graph_dir, schema) = build_linear_deps()?;
    let graph =
        Graph::load_from_directory(&graph_dir, schema.clone()).map_err(|e| e.to_string())?;
    let index = Index::open(&gf_dir.join("index.db")).ok();
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

    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(async {
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
            }
        }))
        .map_err(|e| e.to_string())
    })
}

fn tool_linear_status(_args: &Value) -> Result<String, String> {
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
