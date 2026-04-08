use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Serialize;

use tempyr_core::graph::Graph;
use tempyr_core::schema::Schema;
use tempyr_index::indexer::Index;

use crate::client::LinearClient;
use crate::config::LinearConfig;
use crate::mapping::StatusMapper;
use crate::pull::{self, PullResult};
use crate::push::{self, PushResult};
use crate::state::SyncState;
use crate::Result;

/// Result of a full bidirectional sync.
pub struct SyncResult {
    pub push: PushResult,
    pub pull: PullResult,
}

impl SyncResult {
    pub fn changed_graph(&self) -> bool {
        self.pull.changed_graph()
    }
}

/// Full bidirectional sync: push local changes, then pull remote changes.
pub async fn sync(
    client: &LinearClient,
    graph_dir: &Path,
    graph: &Graph,
    index: Option<&Index>,
    schema: &Schema,
    config: &LinearConfig,
    state: &mut SyncState,
    status_mapper: &StatusMapper,
) -> Result<SyncResult> {
    // Push first (local changes take priority)
    let push_result =
        push::push_all(client, graph, index, schema, config, state, status_mapper).await?;

    // Then pull remote changes
    let pull_result =
        pull::pull(client, graph_dir, schema, config, state, status_mapper).await?;

    Ok(SyncResult {
        push: push_result,
        pull: pull_result,
    })
}

/// Summary of sync state for display.
#[derive(Debug, Serialize)]
pub struct SyncStatusReport {
    pub linked_count: usize,
    pub unlinked_syncable_count: usize,
    pub stale_count: usize,
    pub orphaned_count: usize,
    pub last_sync: Option<DateTime<Utc>>,
    pub entries: Vec<SyncStatusEntry>,
}

#[derive(Debug, Serialize)]
pub struct SyncStatusEntry {
    pub node_id: String,
    pub linear_id: String,
    pub linear_identifier: Option<String>,
    pub node_type: String,
    pub is_stale: bool,
    pub last_synced: DateTime<Utc>,
}

/// Generate a sync status report.
pub fn status_summary(state: &SyncState, graph: &Graph) -> SyncStatusReport {
    let syncable_types = ["epic", "feature", "task"];

    // All syncable nodes in the graph
    let all_syncable: Vec<&str> = graph
        .nodes
        .values()
        .filter(|n| syncable_types.contains(&n.node_type()))
        .map(|n| n.id())
        .collect();

    let linked_count = state.entries.len();
    let linked_ids: Vec<&str> = state.entries.keys().map(|s| s.as_str()).collect();
    let unlinked_syncable_count = all_syncable
        .iter()
        .filter(|id| !linked_ids.contains(id))
        .count();

    // Count stale entries (content hash differs)
    let mut stale_count = 0;
    let mut entries = Vec::new();

    for entry in state.entries.values() {
        let is_stale = graph
            .get_node(&entry.node_id)
            .is_some_and(|n| n.content_hash != entry.content_hash_at_sync);

        if is_stale {
            stale_count += 1;
        }

        entries.push(SyncStatusEntry {
            node_id: entry.node_id.clone(),
            linear_id: entry.linear_id.clone(),
            linear_identifier: entry.linear_identifier.clone(),
            node_type: entry.node_type.clone(),
            is_stale,
            last_synced: entry.last_synced_at,
        });
    }

    entries.sort_by(|a, b| a.node_type.cmp(&b.node_type).then(a.node_id.cmp(&b.node_id)));

    let orphaned_count = state.orphaned_entries(&all_syncable).len();

    SyncStatusReport {
        linked_count,
        unlinked_syncable_count,
        stale_count,
        orphaned_count,
        last_sync: state.last_sync_at,
        entries,
    }
}
