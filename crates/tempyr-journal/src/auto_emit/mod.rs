//! Auto-emit journal entries for tempyr lifecycle events.
//!
//! Phase 4 hooks the CLI and MCP transports into the journal so the
//! moments that matter — task status transitions, interview lifecycle
//! events — get captured automatically, without an agent having to
//! call `journal_log` explicitly.
//!
//! Submodules:
//! - [`task`] — the 3 task status transitions from §9 Phase 4a
//!   (`backlog → in_progress`, `in_progress → done`,
//!   `in_progress → blocked`).
//! - [`interview`] — the 5 interview lifecycle events from §9
//!   Phase 4b (start, answer, phase advance, adjust, commit).
//!
//! **Best-effort by contract**: every entry point in this module
//! returns its error to the caller, but callers (CLI handlers, MCP
//! tools) wrap that as a soft warning. The journal write must never
//! fail the surrounding graph mutation.

pub mod interview;
mod summary;
pub mod task;

pub use interview::{InterviewEvent, auto_emit_interview_event};
pub use task::{TaskTransition, auto_emit_task_transition};
