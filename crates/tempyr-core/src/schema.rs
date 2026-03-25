use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::{TempyrError, Result};

/// The full schema definition loaded from schema.toml.
#[derive(Debug, Clone)]
pub struct Schema {
    pub meta: SchemaMeta,
    pub node_types: HashMap<String, NodeTypeDef>,
    pub edge_types: HashMap<String, EdgeTypeDef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SchemaMeta {
    pub version: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct NodeTypeDef {
    pub description: String,
    pub directory: String,
    pub required_fields: Vec<String>,
    pub optional_fields: Vec<String>,
    pub allowed_statuses: Vec<String>,
    pub allowed_edges: Vec<AllowedEdge>,
}

#[derive(Debug, Clone)]
pub struct AllowedEdge {
    pub edge_type: String,
    pub target: String,
}

#[derive(Debug, Clone)]
pub struct EdgeTypeDef {
    pub reverse: String,
    pub description: Option<String>,
}

// Raw deserialization types (TOML structure doesn't directly map to our domain types)
mod raw {
    use serde::Deserialize;
    use std::collections::HashMap;

    #[derive(Deserialize)]
    pub struct SchemaFile {
        pub meta: super::SchemaMeta,
        pub node_types: HashMap<String, NodeTypeRaw>,
        pub edge_types: HashMap<String, EdgeTypeRaw>,
    }

    #[derive(Deserialize)]
    pub struct NodeTypeRaw {
        pub description: String,
        pub directory: String,
        #[serde(default)]
        pub required_fields: Vec<String>,
        #[serde(default)]
        pub optional_fields: Vec<String>,
        #[serde(default)]
        pub allowed_statuses: Vec<String>,
        #[serde(default)]
        pub allowed_edges: Vec<AllowedEdgeRaw>,
    }

    #[derive(Deserialize)]
    pub struct AllowedEdgeRaw {
        #[serde(rename = "type")]
        pub edge_type: String,
        pub target: String,
    }

    #[derive(Deserialize)]
    pub struct EdgeTypeRaw {
        pub reverse: String,
        pub description: Option<String>,
    }
}

impl Schema {
    /// Load a schema from a TOML file.
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Self::from_str(&content)
    }

    /// Parse a schema from a TOML string.
    pub fn from_str(content: &str) -> Result<Self> {
        let raw: raw::SchemaFile = toml::from_str(content)?;

        let node_types = raw
            .node_types
            .into_iter()
            .map(|(name, nt)| {
                let def = NodeTypeDef {
                    description: nt.description,
                    directory: nt.directory,
                    required_fields: nt.required_fields,
                    optional_fields: nt.optional_fields,
                    allowed_statuses: nt.allowed_statuses,
                    allowed_edges: nt
                        .allowed_edges
                        .into_iter()
                        .map(|ae| AllowedEdge {
                            edge_type: ae.edge_type,
                            target: ae.target,
                        })
                        .collect(),
                };
                (name, def)
            })
            .collect();

        let edge_types = raw
            .edge_types
            .into_iter()
            .map(|(name, et)| {
                let def = EdgeTypeDef {
                    reverse: et.reverse,
                    description: et.description,
                };
                (name, def)
            })
            .collect();

        Ok(Schema {
            meta: raw.meta,
            node_types,
            edge_types,
        })
    }

    /// Get the reverse edge type for a given edge type.
    pub fn reverse_edge_type(&self, edge_type: &str) -> Option<&str> {
        self.edge_types.get(edge_type).map(|et| et.reverse.as_str())
    }

    /// Get the directory name for a node type.
    pub fn directory_for_type(&self, node_type: &str) -> Option<&str> {
        self.node_types.get(node_type).map(|nt| nt.directory.as_str())
    }

    /// Find the node type name for a given directory name.
    pub fn type_for_directory(&self, directory: &str) -> Option<&str> {
        self.node_types
            .iter()
            .find(|(_, nt)| nt.directory == directory)
            .map(|(name, _)| name.as_str())
    }

    /// Validate that an edge type is allowed from source_type to target_type.
    pub fn validate_edge(
        &self,
        source_type: &str,
        edge_type: &str,
        target_type: &str,
    ) -> Result<()> {
        // Check the edge type exists
        if !self.edge_types.contains_key(edge_type) {
            return Err(TempyrError::Schema(format!(
                "Unknown edge type: '{edge_type}'"
            )));
        }

        // Check the source node type exists
        let source_def = self.node_types.get(source_type).ok_or_else(|| {
            TempyrError::Schema(format!("Unknown node type: '{source_type}'"))
        })?;

        // Check the edge is allowed for this source type -> target type
        let allowed = source_def.allowed_edges.iter().any(|ae| {
            ae.edge_type == edge_type && (ae.target == target_type || ae.target == "*")
        });

        if !allowed {
            return Err(TempyrError::Schema(format!(
                "Edge type '{edge_type}' is not allowed from '{source_type}' to '{target_type}'"
            )));
        }

        Ok(())
    }

    /// Validate that a status is allowed for a given node type.
    pub fn validate_status(&self, node_type: &str, status: &str) -> Result<()> {
        let node_def = self.node_types.get(node_type).ok_or_else(|| {
            TempyrError::Schema(format!("Unknown node type: '{node_type}'"))
        })?;

        // If allowed_statuses is empty, any status is ok (e.g., persona, insight, note)
        if node_def.allowed_statuses.is_empty() {
            return Ok(());
        }

        if !node_def.allowed_statuses.contains(&status.to_string()) {
            return Err(TempyrError::Schema(format!(
                "Status '{status}' is not allowed for node type '{node_type}'. \
                 Allowed: {:?}",
                node_def.allowed_statuses
            )));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_default_schema() -> Schema {
        let schema_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("schema/default-schema.toml");
        Schema::load(&schema_path).unwrap()
    }

    #[test]
    fn test_schema_load() {
        let schema = load_default_schema();

        assert_eq!(schema.meta.version, "1.0.0");
        assert!(schema.node_types.contains_key("feature"));
        assert!(schema.node_types.contains_key("epic"));
        assert!(schema.node_types.contains_key("task"));
        assert!(schema.node_types.contains_key("decision"));
        assert!(schema.node_types.contains_key("note"));
        assert!(schema.edge_types.contains_key("child_of"));
        assert!(schema.edge_types.contains_key("parent_of"));

        // Verify feature node type details
        let feature = &schema.node_types["feature"];
        assert_eq!(feature.directory, "features");
        assert!(feature.required_fields.contains(&"status".to_string()));
        assert!(feature.required_fields.contains(&"owner".to_string()));
        assert!(feature.allowed_statuses.contains(&"draft".to_string()));
    }

    #[test]
    fn test_schema_reverse_edge() {
        let schema = load_default_schema();

        assert_eq!(schema.reverse_edge_type("child_of"), Some("parent_of"));
        assert_eq!(schema.reverse_edge_type("parent_of"), Some("child_of"));
        assert_eq!(schema.reverse_edge_type("relates_to"), Some("relates_to"));
        assert_eq!(schema.reverse_edge_type("serves"), Some("served_by"));
        assert_eq!(schema.reverse_edge_type("nonexistent"), None);
    }

    #[test]
    fn test_schema_validate_edge_valid() {
        let schema = load_default_schema();

        // feature -> epic via child_of is valid
        assert!(schema.validate_edge("feature", "child_of", "epic").is_ok());
        // feature -> persona via serves is valid
        assert!(schema.validate_edge("feature", "serves", "persona").is_ok());
        // note -> anything via relates_to is valid (wildcard target)
        assert!(schema.validate_edge("note", "relates_to", "feature").is_ok());
        assert!(schema.validate_edge("note", "relates_to", "decision").is_ok());
    }

    #[test]
    fn test_schema_validate_edge_invalid() {
        let schema = load_default_schema();

        // feature -> persona via child_of is NOT valid
        assert!(schema.validate_edge("feature", "child_of", "persona").is_err());
        // epic -> task via child_of is NOT valid (epic -> feature, not task)
        assert!(schema.validate_edge("epic", "child_of", "task").is_err());
    }

    #[test]
    fn test_schema_validate_edge_unknown_type() {
        let schema = load_default_schema();
        assert!(schema.validate_edge("feature", "fake_edge", "epic").is_err());
    }

    #[test]
    fn test_schema_directory_lookup() {
        let schema = load_default_schema();

        assert_eq!(schema.directory_for_type("feature"), Some("features"));
        assert_eq!(schema.directory_for_type("epic"), Some("epics"));
        assert_eq!(schema.directory_for_type("open_question"), Some("questions"));
        assert_eq!(schema.directory_for_type("nonexistent"), None);
    }

    #[test]
    fn test_schema_type_for_directory() {
        let schema = load_default_schema();

        assert_eq!(schema.type_for_directory("features"), Some("feature"));
        assert_eq!(schema.type_for_directory("epics"), Some("epic"));
        assert_eq!(schema.type_for_directory("nonexistent"), None);
    }

    #[test]
    fn test_schema_validate_status() {
        let schema = load_default_schema();

        assert!(schema.validate_status("feature", "draft").is_ok());
        assert!(schema.validate_status("feature", "active").is_ok());
        assert!(schema.validate_status("feature", "invalid_status").is_err());

        // persona has no allowed_statuses — any status is ok
        assert!(schema.validate_status("persona", "whatever").is_ok());
    }
}
