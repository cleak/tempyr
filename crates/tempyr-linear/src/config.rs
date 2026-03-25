use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{LinearError, Result};

/// Persisted Linear integration configuration.
/// Stored at `.tempyr/linear.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinearConfig {
    /// Linear team ID to sync with.
    pub team_id: String,
    /// Linear team name (for display).
    pub team_name: String,
    /// Linear team key (e.g., "ENG") used in issue identifiers.
    pub team_key: String,
    /// Optional: default Linear project ID to place features under.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_project_id: Option<String>,
    /// Workflow state ID mappings: state_name -> state_id.
    #[serde(default)]
    pub workflow_states: HashMap<String, String>,
    /// Status mapping overrides: node_type -> { gf_status -> linear_state_name }.
    #[serde(default)]
    pub status_overrides: HashMap<String, HashMap<String, String>>,
}

impl LinearConfig {
    pub fn config_path(gf_dir: &Path) -> PathBuf {
        gf_dir.join("linear.json")
    }

    pub fn load(gf_dir: &Path) -> Result<Self> {
        let path = Self::config_path(gf_dir);
        if !path.exists() {
            return Err(LinearError::Config(
                "Linear integration not configured. Run `tempyr linear setup` first."
                    .to_string(),
            ));
        }
        let json = std::fs::read_to_string(&path)?;
        let config: Self = serde_json::from_str(&json)?;
        Ok(config)
    }

    pub fn save(&self, gf_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(gf_dir)?;
        let path = Self::config_path(gf_dir);
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }
}
