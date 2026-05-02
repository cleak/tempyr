//! Linear integration for Tempyr.
//!
//! Push and pull task nodes between the Tempyr graph and a Linear workspace,
//! including status mapping and rendering of context payloads for assignees.

#![allow(clippy::too_many_arguments)]

pub mod client;
pub mod config;
pub mod context;
pub mod mapping;
pub mod pull;
pub mod push;
pub mod queries;
pub mod state;
pub mod sync;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum LinearError {
    #[error("Core error: {0}")]
    Core(#[from] tempyr_core::TempyrError),

    #[error("Index error: {0}")]
    Index(#[from] tempyr_index::IndexError),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("GraphQL error: {0}")]
    GraphQL(String),

    #[error("Rate limited, retry after {retry_after_ms}ms")]
    RateLimited { retry_after_ms: u64 },

    #[error("Config error: {0}")]
    Config(String),

    #[error("Sync conflict on node '{node_id}': both local and remote changed since last sync")]
    Conflict { node_id: String },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Node '{0}' is not linked to a Linear entity")]
    NotLinked(String),
}

pub type Result<T> = std::result::Result<T, LinearError>;
