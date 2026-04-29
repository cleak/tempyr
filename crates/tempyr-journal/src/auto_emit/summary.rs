//! Summary clamping shared by the auto-emit submodules.
//!
//! Synthesized journal summaries reuse user-supplied text (node
//! titles, interview answers) verbatim. The journal validator caps
//! summary length at 200 chars, so each call site needs to truncate
//! before constructing the [`crate::EntryDraft`]. Doing the truncation
//! at *Unicode scalar* granularity (not bytes) keeps multi-byte
//! codepoints intact; the trailing marker makes truncation visible to
//! anyone reading the entry.

/// Largest summary length the journal validator will accept (matches
/// the upper bound enforced by `kind::validate_entry`).
pub(super) const MAX_SUMMARY_CHARS: usize = 200;

/// Trailing marker appended when a summary is shortened. Picked to
/// fit inside the budget and read clearly in a CLI listing.
pub(super) const TRUNCATION_MARKER: &str = " […cut]";

pub(super) fn clamp_summary(s: String) -> String {
    let len = s.chars().count();
    if len <= MAX_SUMMARY_CHARS {
        return s;
    }
    let marker_chars = TRUNCATION_MARKER.chars().count();
    let keep = MAX_SUMMARY_CHARS.saturating_sub(marker_chars);
    let mut out: String = s.chars().take(keep).collect();
    out.push_str(TRUNCATION_MARKER);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_budget_passes_through() {
        let s = "short".to_string();
        assert_eq!(clamp_summary(s.clone()), s);
    }

    #[test]
    fn over_budget_gets_marker() {
        let s = "x".repeat(500);
        let out = clamp_summary(s);
        assert!(out.chars().count() <= MAX_SUMMARY_CHARS);
        assert!(out.ends_with(TRUNCATION_MARKER));
    }

    #[test]
    fn multibyte_safe() {
        // Each crab is 4 bytes / 1 char. A naive byte-cut at MAX*1
        // would split the codepoint; clamp_summary works in chars.
        let s: String = "🦀".repeat(300);
        let out = clamp_summary(s);
        assert!(out.chars().count() <= MAX_SUMMARY_CHARS);
        assert!(out.ends_with(TRUNCATION_MARKER));
    }
}
