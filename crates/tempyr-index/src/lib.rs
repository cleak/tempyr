pub mod embeddings;
pub mod fts;
pub mod health;
pub mod hybrid;
pub mod incremental;
pub mod indexer;
pub mod refresh;
pub mod semantic;
pub mod vector;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum IndexError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("Core error: {0}")]
    Core(#[from] tempyr_core::TempyrError),

    #[error("Index error: {0}")]
    General(String),
}

pub type Result<T> = std::result::Result<T, IndexError>;
