use chrono::Utc;
use serde_json::json;

use tempyr_core::graph::Graph;
use tempyr_core::node::Node;
use tempyr_core::schema::Schema;
use tempyr_index::indexer::Index;

use crate::client::LinearClient;
use crate::config::LinearConfig;
use crate::context::{build_attachments, build_issue_description};
use crate::mapping::{StatusMapper, node_title};
use crate::queries::*;
use crate::state::{SyncEntry, SyncState};
use crate::{LinearError, Result};

/// Result of a push operation.
#[derive(Debug, Default)]
pub struct PushResult {
    pub created: Vec<PushEntry>,
    pub updated: Vec<PushEntry>,
    pub skipped: Vec<String>,
    pub errors: Vec<(String, String)>,
}

#[derive(Debug)]
pub struct PushEntry {
    pub node_id: String,
    pub linear_id: String,
    pub linear_identifier: Option<String>,
    pub action: PushAction,
}

#[derive(Debug)]
pub enum PushAction {
    Created,
    Updated,
}

/// Push all syncable nodes (epics, features, tasks) to Linear.
///
/// Pushes in dependency order: epics → features → tasks.
pub async fn push_all(
    client: &LinearClient,
    graph: &Graph,
    index: Option<&Index>,
    schema: &Schema,
    config: &LinearConfig,
    state: &mut SyncState,
    status_mapper: &StatusMapper,
) -> Result<PushResult> {
    let mut result = PushResult::default();

    // Collect nodes by type
    let epics: Vec<&Node> = graph.nodes_of_type("epic");
    let features: Vec<&Node> = graph.nodes_of_type("feature");
    let tasks: Vec<&Node> = graph.nodes_of_type("task");

    // Push epics first (as projects)
    for node in &epics {
        match push_epic(client, node, graph, schema, config, state, status_mapper).await {
            Ok(entry) => match entry.action {
                PushAction::Created => result.created.push(entry),
                PushAction::Updated => result.updated.push(entry),
            },
            Err(LinearError::NotLinked(_)) => result.skipped.push(node.id().to_string()),
            Err(e) => result.errors.push((node.id().to_string(), e.to_string())),
        }
    }

    // Push features (as top-level issues)
    for node in &features {
        match push_feature(
            client,
            node,
            graph,
            index,
            schema,
            config,
            state,
            status_mapper,
        )
        .await
        {
            Ok(Some(entry)) => match entry.action {
                PushAction::Created => result.created.push(entry),
                PushAction::Updated => result.updated.push(entry),
            },
            Ok(None) => result.skipped.push(node.id().to_string()),
            Err(e) => result.errors.push((node.id().to_string(), e.to_string())),
        }
    }

    // Push tasks (as sub-issues)
    for node in &tasks {
        match push_task(
            client,
            node,
            graph,
            index,
            schema,
            config,
            state,
            status_mapper,
        )
        .await
        {
            Ok(Some(entry)) => match entry.action {
                PushAction::Created => result.created.push(entry),
                PushAction::Updated => result.updated.push(entry),
            },
            Ok(None) => result.skipped.push(node.id().to_string()),
            Err(e) => result.errors.push((node.id().to_string(), e.to_string())),
        }
    }

    state.last_sync_at = Some(Utc::now());
    Ok(result)
}

/// Push a single node, detecting its type automatically.
pub async fn push_node(
    client: &LinearClient,
    node: &Node,
    graph: &Graph,
    index: Option<&Index>,
    schema: &Schema,
    config: &LinearConfig,
    state: &mut SyncState,
    status_mapper: &StatusMapper,
) -> Result<PushEntry> {
    match node.node_type() {
        "epic" => push_epic(client, node, graph, schema, config, state, status_mapper).await,
        "feature" => push_feature(
            client,
            node,
            graph,
            index,
            schema,
            config,
            state,
            status_mapper,
        )
        .await?
        .ok_or_else(|| LinearError::NotLinked(node.id().to_string())),
        "task" => push_task(
            client,
            node,
            graph,
            index,
            schema,
            config,
            state,
            status_mapper,
        )
        .await?
        .ok_or_else(|| LinearError::NotLinked(node.id().to_string())),
        other => Err(LinearError::Config(format!(
            "Node type '{other}' is not syncable to Linear"
        ))),
    }
}

// ─── Epic → Project ────────────────────────────────────

async fn push_epic(
    client: &LinearClient,
    node: &Node,
    graph: &Graph,
    schema: &Schema,
    config: &LinearConfig,
    state: &mut SyncState,
    _status_mapper: &StatusMapper,
) -> Result<PushEntry> {
    let title = node_title(node);
    let description = build_issue_description(node, graph, schema);
    let now = Utc::now();

    // Map epic status to project state
    let project_state = match node.status() {
        Some("draft") => "planned",
        Some("active") => "started",
        Some("completed") => "completed",
        Some("archived") => "canceled",
        _ => "planned",
    };

    if let Some(existing) = state.get_by_node_id(node.id()) {
        // Update existing project
        if node.content_hash == existing.content_hash_at_sync {
            return Ok(PushEntry {
                node_id: node.id().to_string(),
                linear_id: existing.linear_id.clone(),
                linear_identifier: None,
                action: PushAction::Updated,
            });
        }

        let data: ProjectUpdateData = client
            .execute(
                PROJECT_UPDATE_MUTATION,
                json!({
                    "id": existing.linear_id,
                    "input": {
                        "name": title,
                        "description": description,
                        "state": project_state,
                    }
                }),
            )
            .await?;

        let project = data
            .project_update
            .project
            .ok_or_else(|| LinearError::GraphQL("Project update returned no project".into()))?;

        state.upsert(SyncEntry {
            node_id: node.id().to_string(),
            linear_id: project.id.clone(),
            linear_identifier: None,
            node_type: "epic".to_string(),
            content_hash_at_sync: node.content_hash.clone(),
            linear_updated_at: project.updated_at,
            last_synced_at: now,
            attachment_ids: vec![],
        });

        Ok(PushEntry {
            node_id: node.id().to_string(),
            linear_id: project.id,
            linear_identifier: None,
            action: PushAction::Updated,
        })
    } else {
        // Create new project
        let data: ProjectCreateData = client
            .execute(
                PROJECT_CREATE_MUTATION,
                json!({
                    "input": {
                        "name": title,
                        "description": description,
                        "teamIds": [config.team_id],
                        "state": project_state,
                    }
                }),
            )
            .await?;

        let project = data
            .project_create
            .project
            .ok_or_else(|| LinearError::GraphQL("Project create returned no project".into()))?;

        state.upsert(SyncEntry {
            node_id: node.id().to_string(),
            linear_id: project.id.clone(),
            linear_identifier: None,
            node_type: "epic".to_string(),
            content_hash_at_sync: node.content_hash.clone(),
            linear_updated_at: project.updated_at,
            last_synced_at: now,
            attachment_ids: vec![],
        });

        Ok(PushEntry {
            node_id: node.id().to_string(),
            linear_id: project.id,
            linear_identifier: None,
            action: PushAction::Created,
        })
    }
}

// ─── Feature/Task → Issue ──────────────────────────────

async fn push_feature(
    client: &LinearClient,
    node: &Node,
    graph: &Graph,
    index: Option<&Index>,
    schema: &Schema,
    config: &LinearConfig,
    state: &mut SyncState,
    status_mapper: &StatusMapper,
) -> Result<Option<PushEntry>> {
    // Find parent epic's Linear project ID
    let project_id = find_parent_linear_id(node, graph, state, "epic");

    push_issue(
        client,
        node,
        graph,
        index,
        schema,
        config,
        state,
        status_mapper,
        project_id.as_deref(),
        None, // features are top-level issues
    )
    .await
}

async fn push_task(
    client: &LinearClient,
    node: &Node,
    graph: &Graph,
    index: Option<&Index>,
    schema: &Schema,
    config: &LinearConfig,
    state: &mut SyncState,
    status_mapper: &StatusMapper,
) -> Result<Option<PushEntry>> {
    // Find parent feature's Linear issue ID
    let parent_issue_id = find_parent_linear_id(node, graph, state, "feature")
        // Also try parent task for sub-sub-tasks
        .or_else(|| find_parent_linear_id(node, graph, state, "task"));

    // Find project ID via parent feature's parent epic
    let project_id = find_grandparent_project_id(node, graph, state);

    push_issue(
        client,
        node,
        graph,
        index,
        schema,
        config,
        state,
        status_mapper,
        project_id.as_deref(),
        parent_issue_id.as_deref(),
    )
    .await
}

async fn push_issue(
    client: &LinearClient,
    node: &Node,
    graph: &Graph,
    index: Option<&Index>,
    schema: &Schema,
    config: &LinearConfig,
    state: &mut SyncState,
    status_mapper: &StatusMapper,
    project_id: Option<&str>,
    parent_issue_id: Option<&str>,
) -> Result<Option<PushEntry>> {
    let title = node_title(node);
    let description = build_issue_description(node, graph, schema);
    let now = Utc::now();

    let state_id = node.status().and_then(|s| {
        status_mapper.to_linear_state_id(node.node_type(), s, &config.status_overrides)
    });

    if let Some(existing) = state.get_by_node_id(node.id()) {
        // Skip if no changes
        if node.content_hash == existing.content_hash_at_sync {
            return Ok(None);
        }

        // Update existing issue
        let mut input = json!({
            "title": title,
            "description": description,
        });
        if let Some(sid) = &state_id {
            input["stateId"] = json!(sid);
        }

        let data: IssueUpdateData = client
            .execute(
                ISSUE_UPDATE_MUTATION,
                json!({
                    "id": existing.linear_id,
                    "input": input,
                }),
            )
            .await?;

        let issue = data
            .issue_update
            .issue
            .ok_or_else(|| LinearError::GraphQL("Issue update returned no issue".into()))?;

        // Sync attachments
        let att_ids =
            sync_attachments(client, &issue.id, node, graph, index, schema, existing).await?;

        state.upsert(SyncEntry {
            node_id: node.id().to_string(),
            linear_id: issue.id.clone(),
            linear_identifier: Some(issue.identifier.clone()),
            node_type: node.node_type().to_string(),
            content_hash_at_sync: node.content_hash.clone(),
            linear_updated_at: issue.updated_at,
            last_synced_at: now,
            attachment_ids: att_ids,
        });

        Ok(Some(PushEntry {
            node_id: node.id().to_string(),
            linear_id: issue.id,
            linear_identifier: Some(issue.identifier),
            action: PushAction::Updated,
        }))
    } else {
        // Create new issue
        let mut input = json!({
            "title": title,
            "description": description,
            "teamId": config.team_id,
        });
        if let Some(sid) = &state_id {
            input["stateId"] = json!(sid);
        }
        if let Some(pid) = project_id {
            input["projectId"] = json!(pid);
        }
        if let Some(parent_id) = parent_issue_id {
            input["parentId"] = json!(parent_id);
        }

        let data: IssueCreateData = client
            .execute(ISSUE_CREATE_MUTATION, json!({ "input": input }))
            .await?;

        let issue = data
            .issue_create
            .issue
            .ok_or_else(|| LinearError::GraphQL("Issue create returned no issue".into()))?;

        // Create attachments for context nodes
        let att_ids = create_attachments(client, &issue.id, node, graph, schema).await?;

        state.upsert(SyncEntry {
            node_id: node.id().to_string(),
            linear_id: issue.id.clone(),
            linear_identifier: Some(issue.identifier.clone()),
            node_type: node.node_type().to_string(),
            content_hash_at_sync: node.content_hash.clone(),
            linear_updated_at: issue.updated_at,
            last_synced_at: now,
            attachment_ids: att_ids,
        });

        Ok(Some(PushEntry {
            node_id: node.id().to_string(),
            linear_id: issue.id,
            linear_identifier: Some(issue.identifier),
            action: PushAction::Created,
        }))
    }
}

// ─── Attachments ───────────────────────────────────────

async fn create_attachments(
    client: &LinearClient,
    issue_id: &str,
    node: &Node,
    graph: &Graph,
    schema: &Schema,
) -> Result<Vec<String>> {
    let inputs = build_attachments(node, graph, schema);
    let mut ids = Vec::new();

    for att in inputs {
        let data: AttachmentCreateData = client
            .execute(
                ATTACHMENT_CREATE_MUTATION,
                json!({
                    "input": {
                        "issueId": issue_id,
                        "title": att.title,
                        "subtitle": att.subtitle,
                        "url": att.url,
                        "metadata": att.metadata,
                    }
                }),
            )
            .await?;

        if let Some(attachment) = data.attachment_create.attachment {
            ids.push(attachment.id);
        }
    }

    Ok(ids)
}

async fn sync_attachments(
    client: &LinearClient,
    issue_id: &str,
    node: &Node,
    graph: &Graph,
    _index: Option<&Index>,
    schema: &Schema,
    existing: &SyncEntry,
) -> Result<Vec<String>> {
    // Delete old attachments
    for att_id in &existing.attachment_ids {
        let _ = client
            .execute::<AttachmentDeleteData>(ATTACHMENT_DELETE_MUTATION, json!({ "id": att_id }))
            .await;
        // Ignore errors — attachment may already be deleted
    }

    // Create new ones
    create_attachments(client, issue_id, node, graph, schema).await
}

// ─── Helpers ───────────────────────────────────────────

/// Find the Linear ID of a parent node linked via `child_of` edge.
fn find_parent_linear_id(
    node: &Node,
    graph: &Graph,
    state: &SyncState,
    parent_type: &str,
) -> Option<String> {
    for edge in node.edges() {
        if edge.edge_type != "child_of" {
            continue;
        }
        if let Some(parent) = graph.get_node(&edge.target)
            && parent.node_type() == parent_type
            && let Some(entry) = state.get_by_node_id(parent.id())
        {
            return Some(entry.linear_id.clone());
        }
    }
    None
}

/// Walk up two levels: node → feature (child_of) → epic (child_of) → project ID.
fn find_grandparent_project_id(node: &Node, graph: &Graph, state: &SyncState) -> Option<String> {
    // Find parent feature
    for edge in node.edges() {
        if edge.edge_type != "child_of" {
            continue;
        }
        if let Some(parent) = graph.get_node(&edge.target)
            && parent.node_type() == "feature"
        {
            // Find grandparent epic
            return find_parent_linear_id(parent, graph, state, "epic");
        }
    }
    None
}
