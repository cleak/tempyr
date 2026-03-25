use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::Result;

/// Persistent sync state mapping Tempyr nodes to Linear entities.
/// Stored at `.tempyr/linear-sync.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SyncState {
    /// Entries keyed by Tempyr node ID.
    pub entries: HashMap<String, SyncEntry>,
    /// Last time a full sync was run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_sync_at: Option<DateTime<Utc>>,
}

/// A single node-to-Linear entity mapping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncEntry {
    pub node_id: String,
    /// Linear issue ID or project ID (UUID).
    pub linear_id: String,
    /// Linear identifier (e.g., "ENG-123") for display.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linear_identifier: Option<String>,
    /// "epic", "feature", or "task".
    pub node_type: String,
    /// blake3 hash of the node body at last sync.
    pub content_hash_at_sync: String,
    /// Linear entity's updatedAt at last sync.
    pub linear_updated_at: DateTime<Utc>,
    /// When we last synced this entry.
    pub last_synced_at: DateTime<Utc>,
    /// Linear attachment IDs for context nodes (for cleanup on re-sync).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachment_ids: Vec<String>,
}

impl SyncState {
    fn state_path(gf_dir: &Path) -> PathBuf {
        gf_dir.join("linear-sync.json")
    }

    /// Load sync state from disk, or return empty state if file doesn't exist.
    pub fn load(gf_dir: &Path) -> Result<Self> {
        let path = Self::state_path(gf_dir);
        if !path.exists() {
            return Ok(Self::default());
        }
        let json = std::fs::read_to_string(&path)?;
        let state: Self = serde_json::from_str(&json)?;
        Ok(state)
    }

    /// Save sync state to disk.
    pub fn save(&self, gf_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(gf_dir)?;
        let path = Self::state_path(gf_dir);
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Look up an entry by Tempyr node ID.
    pub fn get_by_node_id(&self, node_id: &str) -> Option<&SyncEntry> {
        self.entries.get(node_id)
    }

    /// Look up an entry by Linear entity ID.
    pub fn get_by_linear_id(&self, linear_id: &str) -> Option<&SyncEntry> {
        self.entries.values().find(|e| e.linear_id == linear_id)
    }

    /// Insert or update an entry.
    pub fn upsert(&mut self, entry: SyncEntry) {
        self.entries.insert(entry.node_id.clone(), entry);
    }

    /// Remove an entry by node ID.
    pub fn remove_by_node_id(&mut self, node_id: &str) -> Option<SyncEntry> {
        self.entries.remove(node_id)
    }

    /// Find orphaned entries (node IDs no longer in the graph).
    pub fn orphaned_entries<'a>(
        &'a self,
        graph_node_ids: &[&str],
    ) -> Vec<&'a SyncEntry> {
        self.entries
            .values()
            .filter(|e| !graph_node_ids.contains(&e.node_id.as_str()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_sync_state_roundtrip() {
        let dir = TempDir::new().unwrap();
        let gf_dir = dir.path();

        let mut state = SyncState::default();
        state.upsert(SyncEntry {
            node_id: "task-build-auth".to_string(),
            linear_id: "uuid-123".to_string(),
            linear_identifier: Some("ENG-42".to_string()),
            node_type: "task".to_string(),
            content_hash_at_sync: "abc123".to_string(),
            linear_updated_at: Utc::now(),
            last_synced_at: Utc::now(),
            attachment_ids: vec!["att-1".to_string()],
        });
        state.last_sync_at = Some(Utc::now());

        state.save(gf_dir).unwrap();
        let loaded = SyncState::load(gf_dir).unwrap();

        assert_eq!(loaded.entries.len(), 1);
        assert!(loaded.last_sync_at.is_some());
        let entry = loaded.get_by_node_id("task-build-auth").unwrap();
        assert_eq!(entry.linear_id, "uuid-123");
        assert_eq!(entry.linear_identifier.as_deref(), Some("ENG-42"));
        assert_eq!(entry.attachment_ids, vec!["att-1"]);
    }

    #[test]
    fn test_sync_state_empty_load() {
        let dir = TempDir::new().unwrap();
        let state = SyncState::load(dir.path()).unwrap();
        assert!(state.entries.is_empty());
        assert!(state.last_sync_at.is_none());
    }

    #[test]
    fn test_get_by_linear_id() {
        let mut state = SyncState::default();
        state.upsert(SyncEntry {
            node_id: "feat-replay".to_string(),
            linear_id: "lin-abc".to_string(),
            linear_identifier: None,
            node_type: "feature".to_string(),
            content_hash_at_sync: "hash".to_string(),
            linear_updated_at: Utc::now(),
            last_synced_at: Utc::now(),
            attachment_ids: vec![],
        });

        assert!(state.get_by_linear_id("lin-abc").is_some());
        assert!(state.get_by_linear_id("nonexistent").is_none());
    }

    #[test]
    fn test_orphaned_entries() {
        let mut state = SyncState::default();
        state.upsert(SyncEntry {
            node_id: "task-a".to_string(),
            linear_id: "lin-1".to_string(),
            linear_identifier: None,
            node_type: "task".to_string(),
            content_hash_at_sync: "h1".to_string(),
            linear_updated_at: Utc::now(),
            last_synced_at: Utc::now(),
            attachment_ids: vec![],
        });
        state.upsert(SyncEntry {
            node_id: "task-b".to_string(),
            linear_id: "lin-2".to_string(),
            linear_identifier: None,
            node_type: "task".to_string(),
            content_hash_at_sync: "h2".to_string(),
            linear_updated_at: Utc::now(),
            last_synced_at: Utc::now(),
            attachment_ids: vec![],
        });

        // Only task-a exists in graph
        let orphans = state.orphaned_entries(&["task-a"]);
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].node_id, "task-b");
    }
}
