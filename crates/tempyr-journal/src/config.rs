//! `[journal]` section in `.tempyr/config.toml`.
//!
//! Tunables for the publisher and the in-process ticker. All fields are
//! optional in TOML; missing fields use the [`Default`] values, so an
//! existing project without a `[journal]` section keeps working unchanged.
//!
//! ```toml
//! [journal]
//! enabled = true               # off by default if the repo was detected
//!                              # as public during `tempyr init` (since
//!                              # journal refs would be public too)
//! remote = "origin"
//! tick_secs = 60               # in-process publisher ticker cadence
//! pack_refs_every_n_pushes = 50  # 0 disables pack-refs
//! push_timeout_secs = 30
//! ```

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{JournalError, Result};

/// Knobs for the journal subsystem. Mirrors the `[journal]` TOML section.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct JournalConfig {
    /// Master switch. False = no auto-publish, no ticker. The CLI
    /// `tempyr journal flush` still works (so users can opt-in
    /// per-invocation).
    pub enabled: bool,
    /// Remote name for `git push` and `git fetch`.
    pub remote: String,
    /// Cadence for the in-process publisher ticker, in seconds.
    pub tick_secs: u64,
    /// Run `git pack-refs --all` after every N successful pushes. Loose
    /// refs build up otherwise. 0 disables packing entirely.
    pub pack_refs_every_n_pushes: u64,
    /// Per-operation timeout for git subcommands (push, hash-object,
    /// etc.) in seconds.
    pub push_timeout_secs: u64,
}

impl Default for JournalConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            remote: "origin".to_string(),
            tick_secs: 60,
            pack_refs_every_n_pushes: 50,
            push_timeout_secs: 30,
        }
    }
}

impl JournalConfig {
    /// Load from `<tempyr_dir>/config.toml`. Missing file or missing
    /// `[journal]` section both yield [`Default`] — keep existing
    /// projects working without a config bump.
    pub fn load(tempyr_dir: &Path) -> Result<Self> {
        let path = tempyr_dir.join("config.toml");
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e.into()),
        };
        Self::from_toml_str(&text)
    }

    /// Parse from a TOML string. Designed so a top-level config without a
    /// `[journal]` table doesn't error — we just return defaults.
    pub fn from_toml_str(text: &str) -> Result<Self> {
        let value: toml::Value = text.parse().map_err(|e: toml::de::Error| {
            JournalError::InvalidEntry(format!("config.toml: {e}"))
        })?;
        let Some(section) = value.get("journal") else {
            return Ok(Self::default());
        };
        // serde-via-toml: re-serialize the section then deserialize through
        // `JournalConfig`'s `Deserialize`. This lets us reuse the
        // serde-default field handling instead of hand-rolling each lookup.
        let parsed: JournalConfig = section.clone().try_into().map_err(|e: toml::de::Error| {
            JournalError::InvalidEntry(format!("[journal] section: {e}"))
        })?;
        Ok(parsed)
    }

    /// Compute the per-git-op timeout as a [`Duration`].
    pub fn push_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.push_timeout_secs)
    }

    /// Compute the ticker cadence as a [`Duration`]. Zero falls back to
    /// the default to avoid busy-spin if a user accidentally writes 0.
    pub fn tick_interval(&self) -> std::time::Duration {
        let secs = if self.tick_secs == 0 {
            Self::default().tick_secs
        } else {
            self.tick_secs
        };
        std::time::Duration::from_secs(secs)
    }
}

impl PartialEq for JournalConfig {
    fn eq(&self, other: &Self) -> bool {
        self.enabled == other.enabled
            && self.remote == other.remote
            && self.tick_secs == other.tick_secs
            && self.pack_refs_every_n_pushes == other.pack_refs_every_n_pushes
            && self.push_timeout_secs == other.push_timeout_secs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_config_file_yields_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = JournalConfig::load(dir.path()).unwrap();
        let default = JournalConfig::default();
        assert_eq!(cfg.enabled, default.enabled);
        assert_eq!(cfg.remote, default.remote);
        assert_eq!(cfg.tick_secs, default.tick_secs);
        assert_eq!(
            cfg.pack_refs_every_n_pushes,
            default.pack_refs_every_n_pushes
        );
        assert_eq!(cfg.push_timeout_secs, default.push_timeout_secs);
    }

    #[test]
    fn missing_journal_section_yields_defaults() {
        // Existing projects pre-slice-3 won't have [journal]; we must
        // not break their flow.
        let toml = r#"
[general]
graph_dir = "graph"

[embedding]
provider = "local"
"#;
        let cfg = JournalConfig::from_toml_str(toml).unwrap();
        assert_eq!(cfg, JournalConfig::default());
    }

    #[test]
    fn parses_full_journal_section() {
        let toml = r#"
[journal]
enabled = false
remote = "upstream"
tick_secs = 120
pack_refs_every_n_pushes = 25
push_timeout_secs = 60
"#;
        let cfg = JournalConfig::from_toml_str(toml).unwrap();
        assert!(!cfg.enabled);
        assert_eq!(cfg.remote, "upstream");
        assert_eq!(cfg.tick_secs, 120);
        assert_eq!(cfg.pack_refs_every_n_pushes, 25);
        assert_eq!(cfg.push_timeout_secs, 60);
    }

    #[test]
    fn partial_section_inherits_defaults_for_missing_fields() {
        let toml = r#"
[journal]
remote = "upstream"
"#;
        let cfg = JournalConfig::from_toml_str(toml).unwrap();
        assert_eq!(cfg.remote, "upstream");
        // Other fields fall back to defaults.
        let default = JournalConfig::default();
        assert_eq!(cfg.tick_secs, default.tick_secs);
        assert_eq!(cfg.enabled, default.enabled);
    }

    #[test]
    fn loads_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[journal]
remote = "fork"
"#,
        )
        .unwrap();
        let cfg = JournalConfig::load(dir.path()).unwrap();
        assert_eq!(cfg.remote, "fork");
    }

    #[test]
    fn malformed_toml_returns_error() {
        let toml = "[journal\nremote = \"x\"\n";
        let err = JournalConfig::from_toml_str(toml).unwrap_err();
        assert!(matches!(err, JournalError::InvalidEntry(_)));
    }

    #[test]
    fn tick_interval_falls_back_when_zero() {
        let cfg = JournalConfig {
            tick_secs: 0,
            ..Default::default()
        };
        let default_secs = JournalConfig::default().tick_secs;
        assert_eq!(cfg.tick_interval().as_secs(), default_secs);
    }

    #[test]
    fn defaults_are_sensible() {
        // Spec sanity: if these change, callers depending on them
        // should be updated too. Pin them so a regression is loud.
        let d = JournalConfig::default();
        assert!(d.enabled);
        assert_eq!(d.remote, "origin");
        assert_eq!(d.tick_secs, 60);
        assert_eq!(d.pack_refs_every_n_pushes, 50);
        assert_eq!(d.push_timeout_secs, 30);
    }
}
