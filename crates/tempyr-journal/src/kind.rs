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

    /// Best-guess advice for an unknown kind string. Falls back to listing
    /// all valid kinds when no Levenshtein match is close enough.
    fn suggest(input: &str) -> String {
        let best = Kind::all()
            .iter()
            .map(|k| (*k, strsim::levenshtein(input, k.as_str())))
            .min_by_key(|(_, d)| *d);
        match best {
            Some((kind, d)) if d <= 3 => format!("Did you mean `{}`?", kind.as_str()),
            _ => {
                let names: Vec<&str> = Kind::all().iter().map(|k| k.as_str()).collect();
                format!("Valid kinds: {}", names.join(", "))
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

    let blank = |o: &Option<String>| o.as_deref().is_none_or(str::is_empty);

    match entry.kind {
        Kind::Decision => {
            if blank(&entry.chosen) {
                return Err(missing("decision", "chosen"));
            }
            if blank(&entry.rationale) {
                return Err(missing("decision", "rationale"));
            }
            if entry.reversible.is_none() {
                return Err(JournalError::InvalidEntry(
                    "decision requires `reversible` (true|false)".into(),
                ));
            }
        }
        Kind::DeadEnd => {
            if blank(&entry.approach) {
                return Err(missing("dead_end", "approach"));
            }
            if blank(&entry.failure_mode) {
                return Err(missing("dead_end", "failure_mode"));
            }
        }
        Kind::Assumption => {
            if entry.polarity.is_none() {
                return Err(JournalError::InvalidEntry(
                    "assumption requires `polarity` (positive|negative|unknown)".into(),
                ));
            }
        }
        Kind::Plan | Kind::Finding | Kind::Question | Kind::Risk | Kind::Outcome => {}
    }

    Ok(())
}

fn missing(kind: &str, field: &str) -> JournalError {
    JournalError::InvalidEntry(format!("{kind} requires non-empty `{field}`"))
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
            JournalError::UnknownKind(input, advice) => {
                assert_eq!(input, "desicion");
                assert_eq!(advice, "Did you mean `decision`?");
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
                JournalError::UnknownKind(_, advice) => {
                    let want = format!("Did you mean `{}`?", expected.as_str());
                    assert_eq!(advice, want, "for input {input:?}");
                }
                other => panic!("expected UnknownKind for {input:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn parse_helpful_far_match_lists_all() {
        let err = Kind::parse_helpful("asdfqwerty").unwrap_err();
        if let JournalError::UnknownKind(_, advice) = err {
            assert!(advice.starts_with("Valid kinds: "), "got {advice:?}");
            assert!(advice.contains("decision"));
            assert!(advice.contains("dead_end"));
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
        for kind in [
            Kind::Plan,
            Kind::Finding,
            Kind::Question,
            Kind::Risk,
            Kind::Outcome,
        ] {
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
