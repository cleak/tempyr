//! Journal entry schema.
//!
//! Each entry is one line of JSON in a JSONL file. The schema is intentionally
//! flat so that JSONL stays append-friendly and readers ignore unknown fields
//! for forward compatibility. Per-kind structured fields are validated at write
//! time (see `crate::writer::validate_entry`).
//!
//! Field ordering in the struct follows serialization order for readable JSONL.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::Kind;

/// JSONL schema version. Bump when making breaking changes to the field set.
pub const SCHEMA_VERSION: u32 = 1;

/// Confidence level on a finding, decision, or assumption.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

/// Severity classification for risks, dead ends, and outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warn,
    High,
    Blocker,
}

/// Direction of an assumption: is the assumed thing helpful, harmful, or
/// unknown until verified?
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Polarity {
    Positive,
    Negative,
    Unknown,
}

/// Test result counts for outcome entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestResults {
    pub pass: u32,
    pub fail: u32,
}

/// One journal entry. Serialized as a single line of JSON in a JSONL file.
///
/// Required fields per kind are enforced at write time, not by serde:
/// - `decision` requires `chosen`, `rationale`, `reversible`
/// - `dead_end` requires `approach`, `failure_mode`
/// - `assumption` requires `polarity`
///
/// Other fields (`detail`, `tags`, etc.) are optional regardless of kind.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// Schema version. Always `SCHEMA_VERSION` on write.
    #[serde(rename = "v")]
    pub schema_version: u32,

    /// Unique entry ID. Format: `j-<uuid_v4>`.
    pub id: String,

    /// Wall-clock timestamp at write time.
    pub ts: DateTime<Utc>,

    /// Agent that wrote this entry (e.g. "claude", "codex", "human").
    pub agent: String,

    /// Categorical kind. See `crate::Kind` for variants and semantics.
    pub kind: Kind,

    /// Short human-readable title (validated 20..=200 chars at write time).
    pub summary: String,

    /// Optional longer body. Required for `decision` and `dead_end` (50+ chars).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,

    /// User-defined labels. `tool` is a reserved tag (replaces the deprecated
    /// `tool` kind from blueberry).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,

    /// File paths relevant to this entry, normalized relative to the repo root
    /// when possible.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,

    /// Graph node IDs this entry references. One-way link only — the graph
    /// nodes do not gain reverse edges.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<String>,

    /// Session ID this entry belongs to. Format: `<YYYYMMDD>-<wt_hash>-<HHMMSS>`.
    pub session_id: String,

    /// First 8 hex chars of blake3 over the normalized worktree path.
    pub worktree_hash: String,

    /// Current branch at write time, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,

    /// Git HEAD SHA at write time, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,

    /// Working directory relative to repo root, or absolute if outside.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    /// True if this entry was emitted from in-flight state that may roll back
    /// (e.g. interview tentative state, in-progress task before final outcome).
    /// Default false. Filterable at search time via `--exclude-provisional`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub provisional: bool,

    /// Confidence level. Optional, defaults to medium when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<Confidence>,

    /// Severity. Required for `risk`; recommended for `dead_end` and failed
    /// outcomes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<Severity>,

    // ---- Per-kind structured fields ----
    // All optional at the type level; validated at write time per kind.
    /// `decision`: alternatives considered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<String>,

    /// `decision`: which alternative was chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chosen: Option<String>,

    /// `decision`: why this choice was made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,

    /// `decision`: is the decision reversible?
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reversible: Option<bool>,

    /// `dead_end`: the approach that was tried.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approach: Option<String>,

    /// `dead_end`: how/why it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_mode: Option<String>,

    /// `dead_end`: a suggested next direction, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_to_try: Option<String>,

    /// `assumption`: direction (positive/negative/unknown).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub polarity: Option<Polarity>,

    /// `outcome`: did the work succeed?
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passed: Option<bool>,

    /// `outcome`: test results, if applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests: Option<TestResults>,

    /// `outcome`: did the build succeed, if applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_ok: Option<bool>,

    /// `outcome`: commit SHA produced, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_sha: Option<String>,

    /// `outcome`: marks the final outcome of a session. Triggers publish.
    /// JSON field is `final`; renamed to avoid the Rust reserved word.
    #[serde(default, rename = "final", skip_serializing_if = "is_false")]
    pub is_final: bool,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(b: &bool) -> bool {
    !*b
}

impl Entry {
    /// Generate a fresh entry ID. Format: `j-<uuid_v4_simple>`.
    pub fn new_id() -> String {
        format!("j-{}", uuid::Uuid::new_v4().simple())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_entry(kind: Kind) -> Entry {
        Entry {
            schema_version: SCHEMA_VERSION,
            id: Entry::new_id(),
            ts: Utc::now(),
            agent: "claude".to_string(),
            kind,
            summary: "test summary that is long enough to pass validation".to_string(),
            detail: None,
            tags: vec![],
            files: vec![],
            references: vec![],
            session_id: "20260427-abcd1234-120000".to_string(),
            worktree_hash: "abcd1234".to_string(),
            branch: None,
            head: None,
            cwd: None,
            provisional: false,
            confidence: None,
            severity: None,
            alternatives: vec![],
            chosen: None,
            rationale: None,
            reversible: None,
            approach: None,
            failure_mode: None,
            next_to_try: None,
            polarity: None,
            passed: None,
            tests: None,
            build_ok: None,
            commit_sha: None,
            is_final: false,
        }
    }

    #[test]
    fn roundtrip_minimal_finding() {
        let entry = minimal_entry(Kind::Finding);
        let line = serde_json::to_string(&entry).unwrap();
        // No newlines in serialized JSONL line.
        assert!(!line.contains('\n'));
        let parsed: Entry = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed.id, entry.id);
        assert_eq!(parsed.kind, Kind::Finding);
        assert_eq!(parsed.schema_version, SCHEMA_VERSION);
    }

    #[test]
    fn empty_optionals_omitted() {
        let entry = minimal_entry(Kind::Plan);
        let line = serde_json::to_string(&entry).unwrap();
        // Default empty fields should not appear in output.
        assert!(!line.contains("\"detail\""));
        assert!(!line.contains("\"tags\""));
        assert!(!line.contains("\"alternatives\""));
        assert!(!line.contains("\"final\""));
        assert!(!line.contains("\"provisional\""));
    }

    #[test]
    fn final_field_renamed_in_json() {
        let mut entry = minimal_entry(Kind::Outcome);
        entry.is_final = true;
        let line = serde_json::to_string(&entry).unwrap();
        assert!(line.contains("\"final\":true"));
        assert!(!line.contains("\"is_final\""));
    }

    #[test]
    fn provisional_round_trip() {
        let mut entry = minimal_entry(Kind::Plan);
        entry.provisional = true;
        let line = serde_json::to_string(&entry).unwrap();
        let parsed: Entry = serde_json::from_str(&line).unwrap();
        assert!(parsed.provisional);
    }

    #[test]
    fn newlines_in_summary_are_escaped() {
        let mut entry = minimal_entry(Kind::Finding);
        entry.summary = "line one\nline two\twith tab".to_string();
        let line = serde_json::to_string(&entry).unwrap();
        // Must remain a single line in JSONL.
        assert!(!line.contains('\n'));
        let parsed: Entry = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed.summary, "line one\nline two\twith tab");
    }

    #[test]
    fn unknown_fields_ignored_for_forward_compat() {
        let entry = minimal_entry(Kind::Finding);
        let mut value: serde_json::Value = serde_json::to_value(&entry).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("future_field".to_string(), serde_json::json!("hello"));
        let line = serde_json::to_string(&value).unwrap();
        let parsed: Entry = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed.id, entry.id);
    }

    #[test]
    fn entry_id_format() {
        let id = Entry::new_id();
        assert!(id.starts_with("j-"));
        // simple UUID is 32 hex chars + "j-" prefix
        assert_eq!(id.len(), 34);
    }
}
