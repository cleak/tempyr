pub mod gaps;
pub mod llm;
pub mod phases;
pub mod proposer;
pub mod session;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum InterviewError {
    #[error("Core error: {0}")]
    Core(#[from] graphforge_core::GraphForgeError),

    #[error("Index error: {0}")]
    Index(#[from] graphforge_index::IndexError),

    #[error("Session error: {0}")]
    Session(String),

    #[error("LLM error: {0}")]
    Llm(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, InterviewError>;
