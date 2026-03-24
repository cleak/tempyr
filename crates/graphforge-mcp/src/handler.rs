use serde_json::{Value, json};

use graphforge_core::graph::Graph;
use graphforge_core::ops;
use graphforge_core::schema::Schema;
use graphforge_core::temporal::TemporalFilter;
use graphforge_core::traverse::bfs;
use graphforge_core::validate::validate_graph;
use graphforge_index::hybrid::{hybrid_retrieve, RetrievalConfig};
use graphforge_index::indexer::Index;

use crate::protocol::JsonRpcResponse;

/// Resolve the project context from the current directory.
fn find_project() -> Result<(std::path::PathBuf, std::path::PathBuf, Schema), String> {
    let mut dir = std::env::current_dir().map_err(|e| e.to_string())?;
    loop {
        let gf_dir = dir.join(".graphforge");
        if gf_dir.is_dir() {
            let schema_path = gf_dir.join("schema.toml");
            let schema = Schema::load(&schema_path).map_err(|e| e.to_string())?;
            let graph_dir = dir.join("graph");
            return Ok((graph_dir, gf_dir, schema));
        }
        if !dir.pop() {
            return Err("Not a graphforge project".to_string());
        }
    }
}

pub fn handle_initialize(id: Value) -> JsonRpcResponse {
    JsonRpcResponse::success(
        id,
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": "graphforge",
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
                    "description": "Full-text keyword search across all graph nodes",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "query": {"type": "string"},
                            "max_results": {"type": "integer", "default": 10},
                            "node_type": {"type": "string"}
                        },
                        "required": ["query"]
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
                    "description": "Create a new node in the graph",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "id": {"type": "string"},
                            "node_type": {"type": "string"},
                            "status": {"type": "string"},
                            "body": {"type": "string"},
                            "owner": {"type": "string"},
                            "tags": {"type": "array", "items": {"type": "string"}}
                        },
                        "required": ["id", "node_type", "body"]
                    }
                },
                {
                    "name": "graph_add_edge",
                    "description": "Add an edge between two existing nodes",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "source": {"type": "string"},
                            "target": {"type": "string"},
                            "edge_type": {"type": "string"}
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
        "graph_context" => tool_graph_context(&arguments),
        "graph_traverse" => tool_graph_traverse(&arguments),
        "graph_get_node" => tool_graph_get_node(&arguments),
        "graph_add_node" => tool_graph_add_node(&arguments),
        "graph_add_edge" => tool_graph_add_edge(&arguments),
        "graph_validate" => tool_graph_validate(&arguments),
        "graph_render" => tool_graph_render(&arguments),
        "graph_stats" => tool_graph_stats(&arguments),
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
    let query = args.get("query").and_then(|v| v.as_str()).ok_or("Missing 'query'")?;
    let max_results = args.get("max_results").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
    let node_type = args.get("node_type").and_then(|v| v.as_str());

    let (_, gf_dir, _) = find_project()?;
    let index_path = gf_dir.join("index.db");
    let index = Index::open(&index_path).map_err(|e| format!("Index: {e}"))?;
    let results = index.search_fts_filtered(query, node_type, max_results).map_err(|e| e.to_string())?;

    let output: Vec<Value> = results.iter().map(|r| {
        json!({
            "node_id": r.node_id,
            "title": r.title,
            "node_type": r.node_type,
            "snippet": r.snippet
        })
    }).collect();

    serde_json::to_string_pretty(&output).map_err(|e| e.to_string())
}

fn tool_graph_context(args: &Value) -> Result<String, String> {
    let query = args.get("query").and_then(|v| v.as_str()).ok_or("Missing 'query'")?;
    let root = args.get("root_node").and_then(|v| v.as_str());
    let budget = args.get("token_budget").and_then(|v| v.as_u64()).unwrap_or(8000) as usize;

    let (graph_dir, gf_dir, schema) = find_project()?;
    let graph = Graph::load_from_directory(&graph_dir, schema).map_err(|e| e.to_string())?;
    let index_path = gf_dir.join("index.db");
    let index = Index::open(&index_path).map_err(|e| format!("Index: {e}"))?;

    let config = RetrievalConfig {
        token_budget: budget,
        ..RetrievalConfig::standard()
    };
    let results = hybrid_retrieve(&index, &graph, query, root, &config).map_err(|e| e.to_string())?;

    let mut output = String::new();
    for r in &results {
        if let Some(node) = graph.get_node(&r.node_id) {
            output.push_str(&format!("### {} ({})\n**Score**: {:.3}\n\n{}\n\n---\n\n",
                node.title(), node.node_type(), r.combined_score, node.body.trim()));
        }
    }

    Ok(output)
}

fn tool_graph_traverse(args: &Value) -> Result<String, String> {
    let node_id = args.get("node_id").and_then(|v| v.as_str()).ok_or("Missing 'node_id'")?;
    let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(2) as usize;

    let (graph_dir, _, schema) = find_project()?;
    let graph = Graph::load_from_directory(&graph_dir, schema).map_err(|e| e.to_string())?;

    let results = bfs(&graph, node_id, depth, None);
    let output: Vec<Value> = results.iter().map(|r| {
        let node = graph.get_node(&r.node_id);
        json!({
            "node_id": r.node_id,
            "depth": r.depth,
            "type": node.map(|n| n.node_type()),
            "title": node.map(|n| n.title()),
        })
    }).collect();

    serde_json::to_string_pretty(&output).map_err(|e| e.to_string())
}

fn tool_graph_get_node(args: &Value) -> Result<String, String> {
    let node_id = args.get("node_id").and_then(|v| v.as_str()).ok_or("Missing 'node_id'")?;

    let (graph_dir, _, _) = find_project()?;
    let path = ops::find_node_file(&graph_dir, node_id).map_err(|e| e.to_string())?;
    let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;

    Ok(content)
}

fn tool_graph_add_node(args: &Value) -> Result<String, String> {
    let id = args.get("id").and_then(|v| v.as_str()).ok_or("Missing 'id'")?;
    let node_type = args.get("node_type").and_then(|v| v.as_str()).ok_or("Missing 'node_type'")?;
    let body = args.get("body").and_then(|v| v.as_str()).ok_or("Missing 'body'")?;
    let status = args.get("status").and_then(|v| v.as_str());
    let owner = args.get("owner").and_then(|v| v.as_str());

    let (graph_dir, _, _) = find_project()?;
    let path = ops::create_node_file(&graph_dir, id, node_type, status, owner, None, body)
        .map_err(|e| e.to_string())?;

    Ok(format!("Created node '{id}' at {}", path.display()))
}

fn tool_graph_add_edge(args: &Value) -> Result<String, String> {
    let source = args.get("source").and_then(|v| v.as_str()).ok_or("Missing 'source'")?;
    let target = args.get("target").and_then(|v| v.as_str()).ok_or("Missing 'target'")?;
    let edge_type = args.get("edge_type").and_then(|v| v.as_str()).ok_or("Missing 'edge_type'")?;

    let (graph_dir, _, schema) = find_project()?;
    ops::add_edge(&graph_dir, source, target, edge_type, &schema).map_err(|e| e.to_string())?;

    let reverse = schema.reverse_edge_type(edge_type).unwrap_or("?");
    Ok(format!("Added edge: {source} --{edge_type}--> {target} (reverse: {reverse})"))
}

fn tool_graph_validate(_args: &Value) -> Result<String, String> {
    let (graph_dir, _, schema) = find_project()?;
    let graph = Graph::load_from_directory(&graph_dir, schema).map_err(|e| e.to_string())?;
    let issues = validate_graph(&graph);

    if issues.is_empty() {
        Ok(format!("Graph is valid. {} nodes, {} edges.", graph.node_count(), graph.edge_count()))
    } else {
        let lines: Vec<String> = issues.iter().map(|i| i.to_string()).collect();
        Ok(lines.join("\n"))
    }
}

fn tool_graph_render(args: &Value) -> Result<String, String> {
    let template = args.get("template").and_then(|v| v.as_str()).ok_or("Missing 'template'")?;
    let root_id = args.get("root_node").and_then(|v| v.as_str()).ok_or("Missing 'root_node'")?;
    let include_history = args.get("include_history").and_then(|v| v.as_bool()).unwrap_or(false);

    let (graph_dir, gf_dir, schema) = find_project()?;
    let graph = Graph::load_from_directory(&graph_dir, schema).map_err(|e| e.to_string())?;

    let filter = if include_history {
        TemporalFilter::with_history()
    } else {
        TemporalFilter::current()
    };

    // Try project-local template first, then built-in
    let local_path = gf_dir.join("render").join(format!("{template}.toml"));
    if local_path.exists() {
        graphforge_render::render(&graph, &local_path, root_id, &filter).map_err(|e| e.to_string())
    } else {
        let template_toml = match template {
            "prd" => include_str!("../../../templates/prd.toml"),
            "tdd" => include_str!("../../../templates/tdd.toml"),
            _ => return Err(format!("Unknown template: '{template}'")),
        };
        graphforge_render::render_from_str(&graph, template_toml, root_id, &filter).map_err(|e| e.to_string())
    }
}

fn tool_graph_stats(_args: &Value) -> Result<String, String> {
    let (graph_dir, _gf_dir, schema) = find_project()?;
    let graph = Graph::load_from_directory(&graph_dir, schema).map_err(|e| e.to_string())?;

    let mut type_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
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
