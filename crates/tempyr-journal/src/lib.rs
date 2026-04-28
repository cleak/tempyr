//! Tempyr session journal: append-only agent reasoning log stored in Git refs.
//!
//! Captures decisions, findings, dead ends, assumptions, and other moments of
//! agent reasoning during a coding session. Entries are written to JSONL files
//! under `<git-common-dir>/tempyr/journals/open/` while a session is live, then
//! committed to `refs/tempyr/journals/archive/<YYYY>/<MM>/<DD>/<session_id>` by
//! the publisher and pushed to the remote.
//!
//! Survives worktree abandonment because Git refs are shared across worktrees.

pub mod entry;
pub mod kind;
pub mod path;
pub mod redact;
pub mod session;
pub mod state;
pub mod writer;

pub use entry::{Confidence, Entry, Polarity, SCHEMA_VERSION, Severity};
pub use kind::{Kind, validate_entry};
pub use redact::{Match as RedactionMatch, Mode as RedactionMode, Redactor, default_redactor};
pub use session::{Session, SessionId, SessionMeta};
pub use state::{LogLevel, LogLine, PublisherState};
pub use writer::append;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum JournalError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Invalid entry: {0}")]
    InvalidEntry(String),

    #[error("Unknown kind {0:?}. {1}")]
    UnknownKind(String, String),

    #[error("Git command failed: {0}")]
    Git(String),

    #[error("Redaction blocked write: {rule} matched in {field}")]
    Redacted { rule: String, field: String },

    #[error("Lock acquisition failed: {0}")]
    Lock(String),

    #[error("Not a git repository: {0}")]
    NotAGitRepo(String),

    #[error("session id collision: existing agent {existing}, requested {requested}")]
    AgentMismatch { existing: String, requested: String },
}

pub type Result<T> = std::result::Result<T, JournalError>;
