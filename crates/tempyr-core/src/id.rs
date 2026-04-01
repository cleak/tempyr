//! Hybrid node ID: human-readable slug + 6-char Crockford Base32 suffix.
//!
//! Format: `{slug}-{suffix}` e.g. `session-replay-a1b2c3`
//!
//! The suffix is the stable canonical part. Renames change the slug; the suffix
//! stays fixed. Lookups accept either the full ID or the bare 6-char suffix.

use std::collections::HashSet;
use std::path::Path;

use rand::Rng;
use walkdir::WalkDir;

/// Crockford Base32 alphabet (lowercase): excludes I, L, O, U to avoid
/// ambiguity with 1, 1, 0, and accidental profanity.
const CROCKFORD: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// Length of the generated suffix.
const SUFFIX_LEN: usize = 6;

/// A parsed hybrid node ID.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeId {
    pub slug: String,
    pub suffix: String,
}

impl NodeId {
    /// The full ID string: `{slug}-{suffix}`.
    pub fn full(&self) -> String {
        format!("{}-{}", self.slug, self.suffix)
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}-{}", self.slug, self.suffix)
    }
}

/// Check whether a string is a valid 6-char Crockford Base32 suffix.
pub fn is_valid_suffix(s: &str) -> bool {
    s.len() == SUFFIX_LEN && s.bytes().all(|b| CROCKFORD.contains(&b))
}

/// Parse a full hybrid ID into slug + suffix.
///
/// Splits on the last `-`. If the trailing segment is exactly 6 Crockford
/// Base32 chars, it's the suffix. Returns `None` for legacy (non-hybrid) IDs.
pub fn parse_node_id(id: &str) -> Option<NodeId> {
    let dash = id.rfind('-')?;
    let suffix = &id[dash + 1..];
    if !is_valid_suffix(suffix) {
        return None;
    }
    let slug = &id[..dash];
    if slug.is_empty() {
        return None;
    }
    Some(NodeId {
        slug: slug.to_string(),
        suffix: suffix.to_string(),
    })
}

/// Check whether an ID is in hybrid format (slug + valid 6-char suffix).
pub fn is_hybrid_id(id: &str) -> bool {
    parse_node_id(id).is_some()
}

/// Generate a random 6-char Crockford Base32 suffix that doesn't collide
/// with any existing suffix in the set.
pub fn generate_suffix(existing: &HashSet<String>) -> String {
    let mut rng = rand::rng();
    loop {
        let suffix: String = (0..SUFFIX_LEN)
            .map(|_| {
                let idx = rng.random_range(0..32);
                CROCKFORD[idx] as char
            })
            .collect();
        if !existing.contains(&suffix) {
            return suffix;
        }
    }
}

/// Build a full hybrid ID from a human-readable slug and a set of existing
/// suffixes (for collision avoidance).
pub fn make_node_id(slug: &str, existing_suffixes: &HashSet<String>) -> String {
    let suffix = generate_suffix(existing_suffixes);
    format!("{slug}-{suffix}")
}

/// Scan all `.md` files in a graph directory and collect existing suffixes.
pub fn collect_existing_suffixes(graph_dir: &Path) -> HashSet<String> {
    let mut suffixes = HashSet::new();
    for entry in WalkDir::new(graph_dir)
        .min_depth(2)
        .max_depth(2)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.path().extension().is_none_or(|ext| ext != "md") {
            continue;
        }
        let stem = entry
            .path()
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        if let Some(node_id) = parse_node_id(stem) {
            suffixes.insert(node_id.suffix);
        }
    }
    suffixes
}

fn legacy_type_prefixes_for(node_type: &str) -> &'static [&'static str] {
    match node_type {
        "feature" => &["feature-", "feat-"],
        "decision" => &["decision-", "dec-"],
        "component" => &["component-", "comp-"],
        "constraint" => &["constraint-"],
        "api_surface" => &["api_surface-"],
        "open_question" => &["open_question-"],
        "persona" => &["persona-"],
        "metric" => &["metric-"],
        "insight" => &["insight-"],
        "epic" => &["epic-"],
        "note" => &["note-"],
        "risk" => &["risk-"],
        "task" => &["task-"],
        _ => &[],
    }
}

/// Check whether an ID still carries the legacy type prefix convention for its node type.
pub fn has_legacy_type_prefix(id: &str, node_type: &str) -> bool {
    legacy_type_prefixes_for(node_type)
        .iter()
        .any(|prefix| id.strip_prefix(prefix).is_some_and(|rest| !rest.is_empty()))
}

/// Known type prefixes that should be stripped during migration.
/// Ordered longest-first so `open_question-` matches before `open-`.
const TYPE_PREFIXES: &[&str] = &[
    "open_question-",
    "api_surface-",
    "constraint-",
    "component-",
    "decision-",
    "feature-",
    "insight-",
    "persona-",
    "metric-",
    "epic-",
    "note-",
    "risk-",
    "task-",
    // Common abbreviations
    "feat-",
    "dec-",
    "comp-",
];

/// Strip a type prefix from an old-format ID to produce a clean slug.
///
/// `feat-session-replay` -> `session-replay`
/// `decision-use-sqlite` -> `use-sqlite`
/// `my-custom-thing` -> `my-custom-thing` (no known prefix, unchanged)
pub fn strip_type_prefix(id: &str) -> &str {
    for prefix in TYPE_PREFIXES {
        if let Some(rest) = id.strip_prefix(prefix)
            && !rest.is_empty()
        {
            return rest;
        }
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hybrid_id() {
        let id = parse_node_id("session-replay-a1b2c3").unwrap();
        assert_eq!(id.slug, "session-replay");
        assert_eq!(id.suffix, "a1b2c3");
        assert_eq!(id.full(), "session-replay-a1b2c3");
    }

    #[test]
    fn test_parse_single_word_slug() {
        let id = parse_node_id("caravan-a1b2c3").unwrap();
        assert_eq!(id.slug, "caravan");
        assert_eq!(id.suffix, "a1b2c3");
    }

    #[test]
    fn test_parse_legacy_id_returns_none() {
        // No valid 6-char suffix
        assert!(parse_node_id("feat-session-replay").is_none());
        // Suffix too short
        assert!(parse_node_id("thing-a1b2").is_none());
        // Suffix contains invalid chars (I, L, O, U)
        assert!(parse_node_id("thing-abiLo1").is_none());
    }

    #[test]
    fn test_is_valid_suffix() {
        assert!(is_valid_suffix("a1b2c3"));
        assert!(is_valid_suffix("000000"));
        assert!(is_valid_suffix("zzzzzz"));
        // Invalid: contains 'i'
        assert!(!is_valid_suffix("a1b2ci"));
        // Invalid: contains 'l'
        assert!(!is_valid_suffix("a1b2cl"));
        // Invalid: contains 'o'
        assert!(!is_valid_suffix("a1b2co"));
        // Invalid: contains 'u'
        assert!(!is_valid_suffix("a1b2cu"));
        // Invalid: wrong length
        assert!(!is_valid_suffix("a1b2"));
        assert!(!is_valid_suffix("a1b2c3d"));
    }

    #[test]
    fn test_generate_suffix_unique() {
        let existing: HashSet<String> = HashSet::new();
        let s1 = generate_suffix(&existing);
        assert_eq!(s1.len(), SUFFIX_LEN);
        assert!(is_valid_suffix(&s1));
    }

    #[test]
    fn test_generate_suffix_avoids_collision() {
        let mut existing = HashSet::new();
        let s1 = generate_suffix(&existing);
        existing.insert(s1.clone());
        let s2 = generate_suffix(&existing);
        assert_ne!(s1, s2);
    }

    #[test]
    fn test_has_legacy_type_prefix() {
        assert!(has_legacy_type_prefix("feat-session-replay", "feature"));
        assert!(has_legacy_type_prefix("decision-storage", "decision"));
        assert!(!has_legacy_type_prefix("session-replay-a1b2c3", "feature"));
        assert!(!has_legacy_type_prefix("storage-a1b2c3", "decision"));
    }

    #[test]
    fn test_make_node_id() {
        let existing = HashSet::new();
        let id = make_node_id("session-replay", &existing);
        let parsed = parse_node_id(&id).unwrap();
        assert_eq!(parsed.slug, "session-replay");
        assert!(is_valid_suffix(&parsed.suffix));
    }

    #[test]
    fn test_strip_type_prefix() {
        assert_eq!(strip_type_prefix("feat-session-replay"), "session-replay");
        assert_eq!(
            strip_type_prefix("feature-session-replay"),
            "session-replay"
        );
        assert_eq!(strip_type_prefix("decision-use-sqlite"), "use-sqlite");
        assert_eq!(strip_type_prefix("dec-use-sqlite"), "use-sqlite");
        assert_eq!(strip_type_prefix("epic-observability"), "observability");
        assert_eq!(strip_type_prefix("task-implement-auth"), "implement-auth");
        assert_eq!(strip_type_prefix("risk-data-loss"), "data-loss");
        assert_eq!(strip_type_prefix("persona-platform-eng"), "platform-eng");
        assert_eq!(strip_type_prefix("component-api-gateway"), "api-gateway");
        assert_eq!(strip_type_prefix("constraint-latency"), "latency");
        assert_eq!(strip_type_prefix("metric-mttr"), "mttr");
        assert_eq!(
            strip_type_prefix("open_question-auth-approach"),
            "auth-approach"
        );
        assert_eq!(strip_type_prefix("api_surface-graphql"), "graphql");
        assert_eq!(strip_type_prefix("note-meeting-notes"), "meeting-notes");
        assert_eq!(
            strip_type_prefix("insight-caching-gotcha"),
            "caching-gotcha"
        );
        // No matching prefix - unchanged
        assert_eq!(strip_type_prefix("my-custom-thing"), "my-custom-thing");
        // Don't strip prefix that leaves nothing
        assert_eq!(strip_type_prefix("feat-"), "feat-");
    }

    #[test]
    fn test_is_hybrid_id() {
        assert!(is_hybrid_id("session-replay-a1b2c3"));
        assert!(is_hybrid_id("x-000000"));
        assert!(!is_hybrid_id("feat-session-replay"));
        assert!(!is_hybrid_id("a1b2c3")); // no slug part
    }
}
