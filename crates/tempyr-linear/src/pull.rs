use std::path::Path;

use chrono::Utc;
use serde_json::json;

use tempyr_core::ops;
use tempyr_core::schema::Schema;

use crate::Result;
use crate::client::LinearClient;
use crate::config::LinearConfig;
use crate::mapping::{StatusMapper, slugify};
use crate::queries::*;
use crate::state::{SyncEntry, SyncState};

/// Result of a pull operation.
#[derive(Debug, Default)]
pub struct PullResult {
    pub created: Vec<String>,
    pub updated: Vec<String>,
    pub status_changed: Vec<StatusChange>,
    pub conflicts: Vec<ConflictEntry>,
    pub warnings: Vec<String>,
    pub errors: Vec<(String, String)>,
}

impl PullResult {
    pub fn changed_graph(&self) -> bool {
        !self.created.is_empty() || !self.updated.is_empty()
    }
}

#[derive(Debug)]
pub struct StatusChange {
    pub node_id: String,
    pub old_status: String,
    pub new_status: String,
}

#[derive(Debug)]
pub struct ConflictEntry {
    pub node_id: String,
    pub linear_id: String,
    pub reason: String,
}

/// Poll Linear for changes since last sync and update the graph.
///
/// Only status changes propagate back from Linear. Body changes in Linear
/// do NOT overwrite graph files (graph is the content source of truth).
pub async fn pull(
    client: &LinearClient,
    graph_dir: &Path,
    schema: &Schema,
    config: &LinearConfig,
    state: &mut SyncState,
    status_mapper: &StatusMapper,
) -> Result<PullResult> {
    let mut result = PullResult::default();

    // Fetch issues updated since last sync
    let updated_after = state.last_sync_at.map(|t| t.to_rfc3339());
    let mut after_cursor: Option<String> = None;
    let mut all_issues: Vec<Issue> = Vec::new();

    loop {
        let data: IssuesData = client
            .execute(
                ISSUES_QUERY,
                json!({
                    "teamId": config.team_id,
                    "after": after_cursor,
                    "updatedAfter": updated_after,
                }),
            )
            .await?;

        all_issues.extend(data.issues.nodes);

        if data.issues.page_info.has_next_page {
            after_cursor = data.issues.page_info.end_cursor;
        } else {
            break;
        }
    }

    let now = Utc::now();

    for issue in &all_issues {
        // Check if this issue is tracked in sync state
        if let Some(entry) = state.get_by_linear_id(&issue.id).cloned() {
            // Check if the issue actually changed since our last sync
            if issue.updated_at <= entry.linear_updated_at {
                continue;
            }

            // Check for conflict: did the local node also change?
            let local_changed = check_local_changed(graph_dir, &entry);
            if local_changed {
                result.conflicts.push(ConflictEntry {
                    node_id: entry.node_id.clone(),
                    linear_id: entry.linear_id.clone(),
                    reason: "Both local node and Linear issue changed since last sync".into(),
                });
                continue;
            }

            // Handle archived/deleted issues
            if issue.archived_at.is_some() {
                result.warnings.push(format!(
                    "Linear issue {} ({}) was archived — node '{}' not modified",
                    issue.identifier, issue.id, entry.node_id
                ));
                continue;
            }

            // Sync status change from Linear → Tempyr
            let new_status = status_mapper.from_linear_state(&entry.node_type, &issue.state.name);
            if let Some(new_status) = new_status {
                let current_status = read_current_status(graph_dir, &entry.node_id);
                if current_status.as_deref() != Some(new_status.as_str()) {
                    match ops::update_status(graph_dir, &entry.node_id, &new_status, schema) {
                        Ok(()) => {
                            result.status_changed.push(StatusChange {
                                node_id: entry.node_id.clone(),
                                old_status: current_status.unwrap_or_default(),
                                new_status: new_status.clone(),
                            });
                            result.updated.push(entry.node_id.clone());
                        }
                        Err(e) => {
                            result.errors.push((
                                entry.node_id.clone(),
                                format!("Status update failed: {e}"),
                            ));
                        }
                    }
                }
            }

            // Update sync state with new Linear timestamp
            let mut updated_entry = entry.clone();
            updated_entry.linear_updated_at = issue.updated_at;
            updated_entry.last_synced_at = now;
            // Recompute content hash since we may have updated status
            if let Some(hash) = compute_current_hash(graph_dir, &entry.node_id) {
                updated_entry.content_hash_at_sync = hash;
            }
            state.upsert(updated_entry);
        } else {
            // New issue in Linear that isn't tracked — create a graph node
            // Only if it belongs to a tracked project or has a tracked parent
            let should_import = should_import_new_issue(issue, state);
            if !should_import {
                continue;
            }

            let node_type = infer_node_type(issue);
            let node_id = slugify(&issue.title, node_type);

            // Check if a node with this ID already exists
            if ops::find_node_file(graph_dir, &node_id).is_ok() {
                result.warnings.push(format!(
                    "Skipping Linear issue {} — node '{node_id}' already exists",
                    issue.identifier
                ));
                continue;
            }

            let status = status_mapper
                .from_linear_state(node_type, &issue.state.name)
                .unwrap_or_else(|| "backlog".to_string());

            let body = format!(
                "# {}\n\n{}\n\n---\n*Imported from Linear: {}*",
                issue.title,
                issue.description.as_deref().unwrap_or(""),
                issue.identifier
            );

            match ops::create_node_file(
                graph_dir,
                &node_id,
                node_type,
                Some(&status),
                None,
                None,
                &body,
            ) {
                Ok(_path) => {
                    // Wire parent edge if applicable
                    if let Some(parent_ref) = &issue.parent
                        && let Some(parent_entry) = state.get_by_linear_id(&parent_ref.id)
                    {
                        let _ = ops::add_edge(
                            graph_dir,
                            &node_id,
                            &parent_entry.node_id,
                            "child_of",
                            schema,
                        );
                    }

                    let content_hash = blake3::hash(body.as_bytes()).to_hex().to_string();
                    state.upsert(SyncEntry {
                        node_id: node_id.clone(),
                        linear_id: issue.id.clone(),
                        linear_identifier: Some(issue.identifier.clone()),
                        node_type: node_type.to_string(),
                        content_hash_at_sync: content_hash,
                        linear_updated_at: issue.updated_at,
                        last_synced_at: now,
                        attachment_ids: vec![],
                    });

                    result.created.push(node_id);
                }
                Err(e) => {
                    result
                        .errors
                        .push((node_id, format!("Failed to create node: {e}")));
                }
            }
        }
    }

    state.last_sync_at = Some(now);
    Ok(result)
}

// ─── Helpers ───────────────────────────────────────────

/// Check if the local node has changed since the last sync.
fn check_local_changed(graph_dir: &Path, entry: &SyncEntry) -> bool {
    compute_current_hash(graph_dir, &entry.node_id)
        .is_some_and(|hash| hash != entry.content_hash_at_sync)
}

/// Read the current content hash of a node file.
fn compute_current_hash(graph_dir: &Path, node_id: &str) -> Option<String> {
    let path = ops::find_node_file(graph_dir, node_id).ok()?;
    let content = std::fs::read_to_string(&path).ok()?;
    let node = tempyr_core::node::parse_node(&content, path).ok()?;
    Some(node.content_hash)
}

/// Read the current status of a node.
fn read_current_status(graph_dir: &Path, node_id: &str) -> Option<String> {
    let path = ops::find_node_file(graph_dir, node_id).ok()?;
    let content = std::fs::read_to_string(&path).ok()?;
    let node = tempyr_core::node::parse_node(&content, path).ok()?;
    node.status().map(|s| s.to_string())
}

/// Determine if a new Linear issue should be imported into the graph.
fn should_import_new_issue(issue: &Issue, state: &SyncState) -> bool {
    // Import if parent issue is tracked
    if let Some(parent_ref) = &issue.parent
        && state.get_by_linear_id(&parent_ref.id).is_some()
    {
        return true;
    }
    // Import if project is tracked (maps to an epic)
    if let Some(project_ref) = &issue.project
        && state.get_by_linear_id(&project_ref.id).is_some()
    {
        return true;
    }
    false
}

/// Infer the Tempyr node type from a Linear issue's structure.
fn infer_node_type(issue: &Issue) -> &'static str {
    if issue.parent.is_some() {
        "task"
    } else {
        "feature"
    }
}
