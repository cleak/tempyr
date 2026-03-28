pub mod edge;
pub mod graph;
pub mod id;
pub mod node;
pub mod ops;
pub mod project;
pub mod schema;
pub mod temporal;
pub mod traverse;
pub mod validate;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum TempyrError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("YAML parse error: {0}")]
    Yaml(String),

    #[error("TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("Schema error: {0}")]
    Schema(String),

    #[error("Node error: {0}")]
    Node(String),

    #[error("Edge error: {0}")]
    Edge(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Not found: {0}")]
    NotFound(String),
}

pub type Result<T> = std::result::Result<T, TempyrError>;
