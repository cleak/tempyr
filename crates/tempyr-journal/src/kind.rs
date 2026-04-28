//! Journal entry kinds.
//!
//! Eight categorical kinds capture distinct moments of agent reasoning:
//! plan, finding, assumption, question, decision, dead_end, risk, outcome.
//! Lifecycle reads as: plan -> assumptions/questions -> findings -> decisions
//! -> outcomes, with risks and dead_ends as sidebars throughout.
//!
//! Per-kind required fields are validated at write time, not by serde, so the
//! JSONL schema stays forward-compatible.

use serde::{Deserialize, Serialize};

use crate::entry::Entry;
use crate::{JournalError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// What you're about to attempt and why. Forward-looking; one per
    /// non-trivial undertaking, not per micro-step.
    Plan,
    /// Something you learned by reading code, running a tool, or observing
    /// behavior. Verified.
    Finding,
    /// Something you're assuming without verifying. Distinct from `finding`
    /// (verified) and `risk` (potential problem). Most dead_ends trace back
    /// to unstated assumptions.
    Assumption,
    /// Something you don't know yet. Captures "should ask user" or "look up
    /// later" so it doesn't get buried.
    Question,
    /// A choice between alternatives, with reasoning. Requires `chosen`,
    /// `rationale`, and `reversible` fields.
    Decision,
    /// An approach you tried that didn't work. Requires `approach` and
    /// `failure_mode` fields. The single highest-value entry type — future
    /// agents read these to avoid repeating you.
    DeadEnd,
    /// A potential problem identified but not yet hit. `severity` recommended.
    Risk,
    /// The result of work: passed/failed, tests, build status. Set
    /// `final = true` on the session-closing entry to trigger publish.
    Outcome,
}

impl Kind {
    /// Snake-case string form (matches the JSON serialization).
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Plan => "plan",
            Kind::Finding => "finding",
            Kind::Assumption => "assumption",
            Kind::Question => "question",
            Kind::Decision => "decision",
            Kind::DeadEnd => "dead_end",
            Kind::Risk => "risk",
            Kind::Outcome => "outcome",
        }
    }

    /// All kinds, in canonical order.
    pub fn all() -> &'static [Kind] {
        &[
            Kind::Plan,
            Kind::Finding,
            Kind::Assumption,
            Kind::Question,
            Kind::Decision,
            Kind::DeadEnd,
            Kind::Risk,
            Kind::Outcome,
        ]
    }

    /// One-line description suitable for tool documentation.
    pub fn description(&self) -> &'static str {
        match self {
            Kind::Plan => "what you're about to attempt and why",
            Kind::Finding => "something you learned by reading code or running a tool",
            Kind::Assumption => "something you're assuming without verifying",
            Kind::Question => "something you don't know yet — to ask or look up",
            Kind::Decision => "a choice with reasoning (alternatives, chosen, rationale)",
            Kind::DeadEnd => "an approach that didn't work — high-value for future agents",
            Kind::Risk => "a potential problem identified but not yet hit",
            Kind::Outcome => "the result of a plan; set final=true on session end",
        }
    }

    /// True if this kind requires a `detail` field of at least 50 characters.
    pub fn requires_detail(&self) -> bool {
        matches!(self, Kind::Decision | Kind::DeadEnd)
    }

    /// Parse a snake_case string into a `Kind`. On failure, returns
    /// `JournalError::UnknownKind` with the closest match suggested.
    pub fn parse_helpful(s: &str) -> Result<Kind> {
        let normalized = s.trim().to_ascii_lowercase();
        for kind in Kind::all() {
            if kind.as_str() == normalized {
                return Ok(*kind);
            }
        }
        let suggestion = Kind::suggest(&normalized);
        Err(JournalError::UnknownKind(s.to_string(), suggestion))
    }

    /// Best-guess suggestion for an unknown kind string. Uses Levenshtein
    /// distance; falls back to listing all valid kinds when no match is close.
    fn suggest(input: &str) -> String {
        let mut best: Option<(Kind, usize)> = None;
        for kind in Kind::all() {
            let d = levenshtein(input, kind.as_str());
            if best.map(|(_, bd)| d < bd).unwrap_or(true) {
                best = Some((*kind, d));
            }
        }
        match best {
            Some((kind, d)) if d <= 3 => kind.as_str().to_string(),
            _ => {
                let names: Vec<&str> = Kind::all().iter().map(|k| k.as_str()).collect();
                format!("one of [{}]", names.join(", "))
            }
        }
    }
}

/// Validate per-kind required fields and value bounds. Called by the writer
/// before any line is appended.
pub fn validate_entry(entry: &Entry) -> Result<()> {
    // Summary length: 20..=200 chars (UTF-8 graphemes approximated by chars).
    let len = entry.summary.chars().count();
    if !(20..=200).contains(&len) {
        return Err(JournalError::InvalidEntry(format!(
            "summary length {len} out of bounds (must be 20..=200 chars)"
        )));
    }

    // Detail required for decision and dead_end.
    if entry.kind.requires_detail() {
        let detail_len = entry
            .detail
            .as_ref()
            .map(|d| d.chars().count())
            .unwrap_or(0);
        if detail_len < 50 {
            return Err(JournalError::InvalidEntry(format!(
                "{} requires detail >= 50 chars (got {detail_len})",
                entry.kind.as_str()
            )));
        }
    }

    // Per-kind structured field validation.
    match entry.kind {
        Kind::Decision => {
            if entry.chosen.as_ref().map(|s| s.is_empty()).unwrap_or(true) {
                return Err(JournalError::InvalidEntry(
                    "decision requires non-empty `chosen`".into(),
                ));
            }
            if entry
                .rationale
                .as_ref()
                .map(|s| s.is_empty())
                .unwrap_or(true)
            {
                return Err(JournalError::InvalidEntry(
                    "decision requires non-empty `rationale`".into(),
                ));
            }
            if entry.reversible.is_none() {
                return Err(JournalError::InvalidEntry(
                    "decision requires `reversible` (true|false)".into(),
                ));
            }
        }
        Kind::DeadEnd => {
            if entry.approach.as_ref().map(|s| s.is_empty()).unwrap_or(true) {
                return Err(JournalError::InvalidEntry(
                    "dead_end requires non-empty `approach`".into(),
                ));
            }
            if entry
                .failure_mode
                .as_ref()
                .map(|s| s.is_empty())
                .unwrap_or(true)
            {
                return Err(JournalError::InvalidEntry(
                    "dead_end requires non-empty `failure_mode`".into(),
                ));
            }
        }
        Kind::Assumption => {
            if entry.polarity.is_none() {
                return Err(JournalError::InvalidEntry(
                    "assumption requires `polarity` (positive|negative|unknown)".into(),
                ));
            }
        }
        // No kind-specific required fields for plan/finding/question/risk/outcome.
        _ => {}
    }

    Ok(())
}

/// Iterative DP Levenshtein distance with O(min(a,b)) memory. Adequate for
/// short strings (<= 16 chars) we compare here.
fn levenshtein(a: &str, b: &str) -> usize {
    let (a, b) = if a.len() < b.len() { (b, a) } else { (a, b) };
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (curr[j] + 1)
                .min(prev[j + 1] + 1)
                .min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{Entry, SCHEMA_VERSION};
    use chrono::Utc;

    fn entry_for(kind: Kind) -> Entry {
        Entry {
            schema_version: SCHEMA_VERSION,
            id: Entry::new_id(),
            ts: Utc::now(),
            agent: "claude".into(),
            kind,
            summary: "this summary is long enough to satisfy the validator".into(),
            detail: None,
            tags: vec![],
            files: vec![],
            references: vec![],
            session_id: "20260427-abcd1234-120000".into(),
            worktree_hash: "abcd1234".into(),
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
    fn parse_known_kinds_round_trips() {
        for kind in Kind::all() {
            let parsed = Kind::parse_helpful(kind.as_str()).unwrap();
            assert_eq!(parsed, *kind);
        }
    }

    #[test]
    fn parse_helpful_suggests_close_match() {
        let err = Kind::parse_helpful("desicion").unwrap_err();
        match err {
            JournalError::UnknownKind(input, suggestion) => {
                assert_eq!(input, "desicion");
                assert_eq!(suggestion, "decision");
            }
            other => panic!("expected UnknownKind, got {other:?}"),
        }
    }

    #[test]
    fn parse_helpful_close_typos_for_each_kind() {
        // Common typos each kind should match for.
        let cases = [
            ("plann", Kind::Plan),
            ("findng", Kind::Finding),
            ("assumtion", Kind::Assumption),
            ("questin", Kind::Question),
            ("desicion", Kind::Decision),
            ("dead-end", Kind::DeadEnd),
            ("risck", Kind::Risk),
            ("outome", Kind::Outcome),
        ];
        for (input, expected) in cases {
            let err = Kind::parse_helpful(input).unwrap_err();
            match err {
                JournalError::UnknownKind(_, suggestion) => {
                    assert_eq!(suggestion, expected.as_str(), "for input {input:?}");
                }
                other => panic!("expected UnknownKind for {input:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn parse_helpful_far_match_lists_all() {
        let err = Kind::parse_helpful("asdfqwerty").unwrap_err();
        if let JournalError::UnknownKind(_, suggestion) = err {
            assert!(suggestion.starts_with("one of ["));
            assert!(suggestion.contains("decision"));
            assert!(suggestion.contains("dead_end"));
        } else {
            panic!("expected UnknownKind variant");
        }
    }

    #[test]
    fn parse_helpful_is_case_insensitive_and_trims() {
        assert_eq!(Kind::parse_helpful("  Decision ").unwrap(), Kind::Decision);
        assert_eq!(Kind::parse_helpful("DEAD_END").unwrap(), Kind::DeadEnd);
    }

    #[test]
    fn validate_rejects_short_summary() {
        let mut entry = entry_for(Kind::Finding);
        entry.summary = "too short".into();
        assert!(matches!(
            validate_entry(&entry),
            Err(JournalError::InvalidEntry(_))
        ));
    }

    #[test]
    fn validate_rejects_long_summary() {
        let mut entry = entry_for(Kind::Finding);
        entry.summary = "x".repeat(201);
        assert!(matches!(
            validate_entry(&entry),
            Err(JournalError::InvalidEntry(_))
        ));
    }

    #[test]
    fn validate_decision_requires_structured_fields() {
        let mut entry = entry_for(Kind::Decision);
        // Missing chosen/rationale/reversible/detail.
        assert!(validate_entry(&entry).is_err());

        entry.detail = Some("a".repeat(60));
        entry.chosen = Some("option a".into());
        entry.rationale = Some("better fit for our constraints".into());
        entry.reversible = Some(true);
        validate_entry(&entry).unwrap();
    }

    #[test]
    fn validate_dead_end_requires_approach_and_failure_mode() {
        let mut entry = entry_for(Kind::DeadEnd);
        entry.detail = Some("a".repeat(60));
        // Missing approach and failure_mode.
        assert!(validate_entry(&entry).is_err());

        entry.approach = Some("tried gix push".into());
        entry.failure_mode = Some("credential helper bug #1284".into());
        validate_entry(&entry).unwrap();
    }

    #[test]
    fn validate_decision_requires_detail_50_chars() {
        let mut entry = entry_for(Kind::Decision);
        entry.detail = Some("too short".into());
        entry.chosen = Some("a".into());
        entry.rationale = Some("b".into());
        entry.reversible = Some(false);
        let err = validate_entry(&entry).unwrap_err();
        assert!(matches!(err, JournalError::InvalidEntry(msg) if msg.contains("detail")));
    }

    #[test]
    fn validate_assumption_requires_polarity() {
        let mut entry = entry_for(Kind::Assumption);
        assert!(validate_entry(&entry).is_err());
        entry.polarity = Some(crate::entry::Polarity::Positive);
        validate_entry(&entry).unwrap();
    }

    #[test]
    fn validate_simple_kinds_pass_with_only_summary() {
        for kind in [Kind::Plan, Kind::Finding, Kind::Question, Kind::Risk, Kind::Outcome] {
            let entry = entry_for(kind);
            validate_entry(&entry).unwrap_or_else(|e| panic!("{kind:?} failed: {e:?}"));
        }
    }

    #[test]
    fn description_non_empty_for_each_kind() {
        for kind in Kind::all() {
            assert!(!kind.description().is_empty());
        }
    }
}
