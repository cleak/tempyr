//! AI-assisted interview engine for Tempyr.
//!
//! Drives the five-phase interview state machine (Discovery → Product →
//! Technical → Decomposition → Review). Phase transitions and gap detection
//! are deterministic Rust; the LLM only extracts structured data from
//! natural-language answers. Proposals are tentative until the user commits.

pub mod gaps;
pub mod llm;
pub mod phases;
pub mod proposer;
pub mod session;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum InterviewError {
    #[error("Core error: {0}")]
    Core(#[from] tempyr_core::TempyrError),

    #[error("Index error: {0}")]
    Index(#[from] tempyr_index::IndexError),

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
