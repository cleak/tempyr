//! Redaction: scan entries for secrets and PII before they hit disk.
//!
//! Two layered detectors:
//! 1. **Named regex rules** (gitleaks-style) for known credential shapes:
//!    OpenAI/Anthropic keys, GitHub PATs, AWS keys, JWTs, Slack tokens,
//!    private-key blocks, DB connection strings with passwords, absolute
//!    user-home paths.
//! 2. **Shannon entropy fallback** for runs of `[A-Za-z0-9_+/=-]` that look
//!    base64-ish — catches credentials without a known format.
//!
//! Three modes: `Block` rejects the write (default), `Redact` rewrites the
//! entry replacing matches with `<REDACTED:rule_name>`, `Warn` logs and lets
//! the entry through unchanged.
//!
//! The redactor scans the user-controllable fields: `summary`, `detail`,
//! `tags`, `files`, `cwd`, and the per-kind structured strings (`chosen`,
//! `rationale`, `approach`, `failure_mode`, `next_to_try`, `commit_sha`).

use std::sync::OnceLock;

use regex::Regex;

use crate::entry::Entry;
use crate::{JournalError, Result};

/// What to do when a sensitive pattern is matched.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    /// Reject the write with `JournalError::Redacted`. Default.
    #[default]
    Block,
    /// Rewrite the entry, replacing each match with `<REDACTED:rule_name>`.
    Redact,
    /// Allow the entry through unchanged; matches are still returned for
    /// logging/observability.
    Warn,
}

/// One regex-based rule.
struct Rule {
    name: &'static str,
    regex: Regex,
}

/// Result of a single match, with enough info for review/logging.
#[derive(Debug, Clone)]
pub struct Match {
    pub rule: String,
    pub field: &'static str,
    pub matched: String,
}

/// Configurable redactor. Created with `Redactor::default()` for built-in
/// rules and `Mode::Block`.
pub struct Redactor {
    mode: Mode,
    rules: Vec<Rule>,
    entropy_threshold: f64,
    entropy_min_len: usize,
    extra: Vec<Rule>,
}

impl Default for Redactor {
    fn default() -> Self {
        Redactor {
            mode: Mode::Block,
            rules: built_in_rules(),
            entropy_threshold: 4.5,
            entropy_min_len: 24,
            extra: Vec::new(),
        }
    }
}

impl Redactor {
    /// Override the action mode.
    pub fn with_mode(mut self, mode: Mode) -> Self {
        self.mode = mode;
        self
    }

    /// Disable Shannon-entropy detection (regex rules only).
    pub fn without_entropy(mut self) -> Self {
        self.entropy_threshold = f64::INFINITY;
        self
    }

    /// Tune entropy detector. Set `min_len` to the shortest run length to
    /// inspect; `threshold` is bits/char of Shannon entropy.
    pub fn entropy(mut self, threshold: f64, min_len: usize) -> Self {
        self.entropy_threshold = threshold;
        self.entropy_min_len = min_len;
        self
    }

    /// Add a user-supplied rule. Compiled at construction time so write-path
    /// `enforce` calls don't pay regex compilation cost.
    pub fn add_rule(mut self, name: &'static str, pattern: &str) -> Result<Self> {
        let regex = Regex::new(pattern)
            .map_err(|e| JournalError::InvalidEntry(format!("bad regex {name:?}: {e}")))?;
        self.extra.push(Rule { name, regex });
        Ok(self)
    }

    /// Diagnostic scan: returns every match without modifying the entry.
    /// Useful for `tempyr journal review` and dry-run flows.
    pub fn check(&self, entry: &Entry) -> Vec<Match> {
        let mut out = Vec::new();
        for (field, text) in scannable_fields(entry) {
            self.scan_text(field, text, &mut out);
        }
        out
    }

    /// Apply redaction policy. In `Block` mode, returns `Err` on first match.
    /// In `Redact` mode, replaces matches in place and returns Ok.
    /// In `Warn` mode, returns Ok unconditionally; caller can still call
    /// `check` for the matches.
    pub fn enforce(&self, entry: &mut Entry) -> Result<Vec<Match>> {
        let matches = self.check(entry);
        if matches.is_empty() {
            return Ok(matches);
        }
        match self.mode {
            Mode::Block => {
                let first = &matches[0];
                Err(JournalError::Redacted {
                    rule: first.rule.clone(),
                    field: first.field.to_string(),
                })
            }
            Mode::Redact => {
                self.redact_in_place(entry);
                Ok(matches)
            }
            Mode::Warn => Ok(matches),
        }
    }

    fn scan_text(&self, field: &'static str, text: &str, out: &mut Vec<Match>) {
        for rule in self.rules.iter().chain(self.extra.iter()) {
            for m in rule.regex.find_iter(text) {
                out.push(Match {
                    rule: rule.name.to_string(),
                    field,
                    matched: m.as_str().to_string(),
                });
            }
        }
        // Entropy fallback: scan runs of high-entropy alphabet.
        if self.entropy_threshold.is_finite() {
            for run in entropy_candidate_runs(text, self.entropy_min_len) {
                let h = shannon_entropy(run);
                if h >= self.entropy_threshold {
                    out.push(Match {
                        rule: "high_entropy".to_string(),
                        field,
                        matched: run.to_string(),
                    });
                }
            }
        }
    }

    fn redact_in_place(&self, entry: &mut Entry) {
        redact_string(&mut entry.summary, self);
        if let Some(d) = entry.detail.as_mut() {
            redact_string(d, self);
        }
        for t in entry.tags.iter_mut() {
            redact_string(t, self);
        }
        for f in entry.files.iter_mut() {
            redact_string(f, self);
        }
        if let Some(c) = entry.cwd.as_mut() {
            redact_string(c, self);
        }
        for opt in [
            entry.chosen.as_mut(),
            entry.rationale.as_mut(),
            entry.approach.as_mut(),
            entry.failure_mode.as_mut(),
            entry.next_to_try.as_mut(),
            entry.commit_sha.as_mut(),
        ]
        .into_iter()
        .flatten()
        {
            redact_string(opt, self);
        }
        for alt in entry.alternatives.iter_mut() {
            redact_string(alt, self);
        }
    }
}

fn redact_string(s: &mut String, r: &Redactor) {
    let mut working = s.clone();
    for rule in r.rules.iter().chain(r.extra.iter()) {
        let replacement = format!("<REDACTED:{}>", rule.name);
        working = rule
            .regex
            .replace_all(&working, replacement.as_str())
            .into_owned();
    }
    if r.entropy_threshold.is_finite() {
        // Replace each high-entropy run independently. We rebuild the string
        // by walking candidates; if a candidate matches threshold, swap it.
        working = redact_high_entropy(&working, r.entropy_threshold, r.entropy_min_len);
    }
    *s = working;
}

fn redact_high_entropy(input: &str, threshold: f64, min_len: usize) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        if is_entropy_byte(bytes[i]) {
            // Find the end of this run.
            let start = i;
            while i < bytes.len() && is_entropy_byte(bytes[i]) {
                i += 1;
            }
            let run = &input[start..i];
            if run.chars().count() >= min_len && shannon_entropy(run) >= threshold {
                out.push_str("<REDACTED:high_entropy>");
            } else {
                out.push_str(run);
            }
        } else {
            // Non-ASCII or non-candidate char; copy as-is.
            // Step by char to avoid splitting multi-byte UTF-8.
            let ch = input[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

fn entropy_candidate_runs(text: &str, min_len: usize) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if is_entropy_byte(bytes[i]) {
            let start = i;
            while i < bytes.len() && is_entropy_byte(bytes[i]) {
                i += 1;
            }
            let slice = &text[start..i];
            if slice.chars().count() >= min_len {
                out.push(slice);
            }
        } else {
            i += 1;
        }
    }
    out
}

fn is_entropy_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'+' | b'/' | b'=' | b'-')
}

fn shannon_entropy(s: &str) -> f64 {
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len() as f64;
    if len == 0.0 {
        return 0.0;
    }
    let mut counts: std::collections::HashMap<char, usize> = Default::default();
    for c in &chars {
        *counts.entry(*c).or_insert(0) += 1;
    }
    let mut h = 0.0;
    for &n in counts.values() {
        let p = n as f64 / len;
        h -= p * p.log2();
    }
    h
}

fn scannable_fields(entry: &Entry) -> Vec<(&'static str, &str)> {
    let mut out: Vec<(&'static str, &str)> = vec![("summary", entry.summary.as_str())];
    if let Some(d) = &entry.detail {
        out.push(("detail", d));
    }
    for t in &entry.tags {
        out.push(("tags", t));
    }
    for f in &entry.files {
        out.push(("files", f));
    }
    if let Some(c) = &entry.cwd {
        out.push(("cwd", c));
    }
    if let Some(s) = &entry.chosen {
        out.push(("chosen", s));
    }
    if let Some(s) = &entry.rationale {
        out.push(("rationale", s));
    }
    if let Some(s) = &entry.approach {
        out.push(("approach", s));
    }
    if let Some(s) = &entry.failure_mode {
        out.push(("failure_mode", s));
    }
    if let Some(s) = &entry.next_to_try {
        out.push(("next_to_try", s));
    }
    if let Some(s) = &entry.commit_sha {
        out.push(("commit_sha", s));
    }
    for a in &entry.alternatives {
        out.push(("alternatives", a));
    }
    out
}

fn built_in_rules() -> Vec<Rule> {
    fn r(name: &'static str, pat: &'static str) -> Rule {
        Rule {
            name,
            regex: Regex::new(pat).expect("built-in regex must compile"),
        }
    }
    vec![
        r(
            "anthropic_or_openai_key",
            r"\bsk-(?:ant-)?[A-Za-z0-9_-]{20,}\b",
        ),
        r("github_pat", r"\bgh[pousr]_[A-Za-z0-9]{36,}\b"),
        r("slack_token", r"\bxox[abprs]-[A-Za-z0-9-]{10,}\b"),
        r("aws_access_key", r"\bAKIA[0-9A-Z]{16}\b"),
        r(
            "bearer_token",
            r"(?i)authorization:\s*bearer\s+[A-Za-z0-9._\-]{20,}",
        ),
        r(
            "jwt",
            r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b",
        ),
        r(
            "private_key_block",
            r"-----BEGIN (?:RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----",
        ),
        r(
            "db_url_with_password",
            r"(?i)(?:postgres|postgresql|mysql|mongodb)(?:\+\w+)?://[^@\s]+:[^@\s]+@",
        ),
        r(
            "user_home_path",
            r#"(?i)(?:[A-Z]:\\Users\\|/Users/|/home/)[^\s'"]+"#,
        ),
    ]
}

/// Lazy-initialized default redactor for callers that don't need a custom
/// configuration.
pub fn default_redactor() -> &'static Redactor {
    static R: OnceLock<Redactor> = OnceLock::new();
    R.get_or_init(Redactor::default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Kind;
    use crate::entry::SCHEMA_VERSION;
    use chrono::{TimeZone, Utc};

    fn entry_with_summary(summary: &str) -> Entry {
        Entry {
            schema_version: SCHEMA_VERSION,
            id: Entry::new_id(),
            ts: Utc.with_ymd_and_hms(2026, 4, 27, 12, 0, 0).unwrap(),
            agent: "claude".into(),
            kind: Kind::Finding,
            summary: summary.into(),
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
    fn detects_anthropic_key() {
        let e = entry_with_summary(
            "oops I pasted sk-ant-abcdefghijklmnop1234567890qrstuvwx into a comment",
        );
        let r = Redactor::default().without_entropy();
        let matches = r.check(&e);
        assert!(matches.iter().any(|m| m.rule == "anthropic_or_openai_key"));
    }

    #[test]
    fn detects_openai_key() {
        let e = entry_with_summary("token sk-proj-abc1234567890defghij is in our env");
        let r = Redactor::default().without_entropy();
        let matches = r.check(&e);
        assert!(matches.iter().any(|m| m.rule == "anthropic_or_openai_key"));
    }

    #[test]
    fn detects_github_pat() {
        let e = entry_with_summary("token: ghp_abcdefghijklmnopqrstuvwxyz0123456789AB and a story");
        let r = Redactor::default().without_entropy();
        let matches = r.check(&e);
        assert!(matches.iter().any(|m| m.rule == "github_pat"));
    }

    #[test]
    fn detects_aws_key() {
        let e = entry_with_summary("found AKIAIOSFODNN7EXAMPLE in our config which is bad news");
        let r = Redactor::default().without_entropy();
        let matches = r.check(&e);
        assert!(matches.iter().any(|m| m.rule == "aws_access_key"));
    }

    #[test]
    fn detects_jwt() {
        let e = entry_with_summary(
            "Authorization eyJhbGciOiJIUzI1NiIs.eyJzdWIiOiIxMjM0.SflKxwRJSMeKKF3Q in headers",
        );
        let r = Redactor::default().without_entropy();
        let matches = r.check(&e);
        assert!(matches.iter().any(|m| m.rule == "jwt"));
    }

    #[test]
    fn detects_private_key_block() {
        let mut e = entry_with_summary("public summary text that is long enough to validate");
        e.detail = Some(
            "-----BEGIN RSA PRIVATE KEY-----\nMIIEvQ...etc\n-----END RSA PRIVATE KEY-----".into(),
        );
        let r = Redactor::default().without_entropy();
        let matches = r.check(&e);
        assert!(matches.iter().any(|m| m.rule == "private_key_block"));
    }

    #[test]
    fn detects_db_url_with_password() {
        let e = entry_with_summary("connecting to postgres://user:hunter2@db.internal:5432/foo");
        let r = Redactor::default().without_entropy();
        let matches = r.check(&e);
        assert!(matches.iter().any(|m| m.rule == "db_url_with_password"));
    }

    #[test]
    fn detects_user_home_path() {
        let e = entry_with_summary("the file at C:\\Users\\caleb\\secrets.txt has bad data");
        let r = Redactor::default().without_entropy();
        let matches = r.check(&e);
        assert!(matches.iter().any(|m| m.rule == "user_home_path"));
    }

    #[test]
    fn entropy_fallback_catches_random_blob() {
        // Random base64-ish blob with high entropy.
        let blob = "Zm9vYmFyYmF6cXV1eHF1dXhmb29iYXJiYXpxdXV4cXV1eGZvb2JhcmJhenF1dXg=";
        let e = entry_with_summary(&format!(
            "env had value {blob} which I copy-pasted hopefully"
        ));
        let r = Redactor::default();
        let matches = r.check(&e);
        assert!(
            matches.iter().any(|m| m.rule == "high_entropy"),
            "expected entropy match, got: {matches:?}"
        );
    }

    #[test]
    fn entropy_does_not_fire_on_english_prose() {
        let e = entry_with_summary(
            "the quick brown fox jumps over the lazy dog and continues to run for ages",
        );
        let r = Redactor::default();
        let matches = r.check(&e);
        // Tokens like "continues" are short; English prose entropy is < 4.5.
        let high_entropy: Vec<_> = matches
            .iter()
            .filter(|m| m.rule == "high_entropy")
            .collect();
        assert!(
            high_entropy.is_empty(),
            "english prose should not trigger entropy: {high_entropy:?}"
        );
    }

    #[test]
    fn block_mode_returns_redacted_error() {
        let mut e = entry_with_summary("token sk-ant-abcdefghijklmnop1234567890qrstuvwx is bad");
        let r = Redactor::default().with_mode(Mode::Block).without_entropy();
        let err = r.enforce(&mut e).unwrap_err();
        match err {
            JournalError::Redacted { rule, field } => {
                assert_eq!(rule, "anthropic_or_openai_key");
                assert_eq!(field, "summary");
            }
            other => panic!("expected Redacted, got {other:?}"),
        }
        // Entry untouched in block mode.
        assert!(e.summary.contains("sk-ant-"));
    }

    #[test]
    fn redact_mode_replaces_match() {
        let mut e =
            entry_with_summary("token ghp_abcdefghijklmnopqrstuvwxyz0123456789AB please remove");
        let r = Redactor::default()
            .with_mode(Mode::Redact)
            .without_entropy();
        let matches = r.enforce(&mut e).unwrap();
        assert!(!matches.is_empty());
        assert!(e.summary.contains("<REDACTED:github_pat>"));
        assert!(!e.summary.contains("ghp_abcdef"));
    }

    #[test]
    fn warn_mode_passes_through_unchanged() {
        let mut e =
            entry_with_summary("token ghp_abcdefghijklmnopqrstuvwxyz0123456789AB please remove");
        let original = e.summary.clone();
        let r = Redactor::default().with_mode(Mode::Warn).without_entropy();
        let matches = r.enforce(&mut e).unwrap();
        assert!(!matches.is_empty());
        assert_eq!(e.summary, original);
    }

    #[test]
    fn clean_entry_passes_in_block_mode() {
        let mut e = entry_with_summary("a perfectly innocent finding about the index module");
        let r = Redactor::default().with_mode(Mode::Block);
        let matches = r.enforce(&mut e).unwrap();
        assert!(matches.is_empty());
    }

    #[test]
    fn extra_user_rule_works() {
        let mut e = entry_with_summary("hostname is db.internal.acme.corp and the secret is x");
        let r = Redactor::default()
            .without_entropy()
            .add_rule("internal_host", r"\b\w+\.internal\.\w+\.\w+\b")
            .unwrap();
        let matches = r.check(&e);
        assert!(matches.iter().any(|m| m.rule == "internal_host"));

        let r = r.with_mode(Mode::Redact);
        r.enforce(&mut e).unwrap();
        assert!(e.summary.contains("<REDACTED:internal_host>"));
    }

    #[test]
    fn redact_handles_unicode_after_match() {
        let mut e =
            entry_with_summary("ghp_abcdefghijklmnopqrstuvwxyz0123456789AB followed by 🦀 emoji");
        let r = Redactor::default()
            .with_mode(Mode::Redact)
            .without_entropy();
        r.enforce(&mut e).unwrap();
        assert!(e.summary.contains("🦀"));
        assert!(e.summary.contains("<REDACTED:github_pat>"));
    }

    #[test]
    fn shannon_entropy_known_values() {
        // All same char => 0 bits.
        assert!((shannon_entropy("aaaaaa") - 0.0).abs() < 1e-9);
        // Two chars, 50/50 => 1 bit.
        let h = shannon_entropy("ababababab");
        assert!((h - 1.0).abs() < 1e-9);
        // Random base64 should be > 5 bits.
        let h = shannon_entropy("Zm9vYmFyYmF6cXV1eHF1dXhmb29iYXJiYXo");
        assert!(h > 4.0, "got entropy {h}");
    }

    #[test]
    fn checks_detail_field() {
        let mut e = entry_with_summary("clean summary that is long enough to satisfy validator");
        e.detail = Some("token sk-ant-abcdefghijklmnop1234567890qrstuvwx in detail".into());
        let r = Redactor::default().without_entropy();
        let matches = r.check(&e);
        assert!(matches.iter().any(|m| m.field == "detail"));
    }

    #[test]
    fn checks_files_and_tags_fields() {
        let mut e = entry_with_summary("clean summary that is long enough to satisfy validator");
        e.files = vec!["C:\\Users\\caleb\\secret.rs".into()];
        e.tags = vec![
            "normal".into(),
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789AB".into(),
        ];
        let r = Redactor::default().without_entropy();
        let matches = r.check(&e);
        assert!(matches.iter().any(|m| m.field == "files"));
        assert!(matches.iter().any(|m| m.field == "tags"));
    }

    #[test]
    fn default_redactor_singleton() {
        let r1 = default_redactor();
        let r2 = default_redactor();
        assert!(std::ptr::eq(r1, r2));
    }
}
