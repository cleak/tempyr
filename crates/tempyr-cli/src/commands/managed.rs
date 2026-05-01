use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::Path;

// ---------------------------------------------------------------------------
// Embedded artifacts
// ---------------------------------------------------------------------------

const TEMPYR_HOOKS_JSON: &str = include_str!("../../assets/claude.settings.json");
const SKILL_INTERVIEW_MD: &str =
    include_str!("../../../../.claude/skills/tempyr-interview/SKILL.md");
const AGENT_EXTRACTOR_MD: &str = include_str!("../../../../.claude/agents/tempyr-extractor.md");
const TEMPYR_VERSION: &str = env!("CARGO_PKG_VERSION");

// ---------------------------------------------------------------------------
// Managed file definitions
// ---------------------------------------------------------------------------

struct ManagedFileDef {
    artifact: ManagedArtifact,
    path: &'static str,
    content: &'static str,
    strategy: Strategy,
    description: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedArtifact {
    Hooks,
    Skill,
    Agent,
}

const MANAGED_FILES: &[ManagedFileDef] = &[
    ManagedFileDef {
        artifact: ManagedArtifact::Hooks,
        path: ".claude/settings.json",
        content: TEMPYR_HOOKS_JSON,
        strategy: Strategy::Merge,
        description: "Claude Code hooks for journaling, validation, and indexing",
    },
    ManagedFileDef {
        artifact: ManagedArtifact::Skill,
        path: ".claude/skills/tempyr-interview/SKILL.md",
        content: SKILL_INTERVIEW_MD,
        strategy: Strategy::Overwrite,
        description: "interview skill definition",
    },
    ManagedFileDef {
        artifact: ManagedArtifact::Agent,
        path: ".claude/agents/tempyr-extractor.md",
        content: AGENT_EXTRACTOR_MD,
        strategy: Strategy::Overwrite,
        description: "extraction agent definition",
    },
];

// ---------------------------------------------------------------------------
// Manifest types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub tempyr_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_artifacts: Option<Vec<ManagedArtifact>>,
    pub files: Vec<ManagedFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedFile {
    pub path: String,
    pub strategy: Strategy,
    pub written_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tempyr_hash: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Strategy {
    Overwrite,
    Merge,
}

// ---------------------------------------------------------------------------
// Status detection
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    /// On-disk matches current embedded content.
    UpToDate,
    /// Tempyr has newer content; on-disk matches what tempyr last wrote.
    Stale,
    /// Tempyr has newer content AND the user edited the file since last write.
    UserModified,
    /// File does not exist on disk.
    Missing,
}

pub struct UpdateReport {
    pub path: &'static str,
    pub description: &'static str,
    pub status: FileStatus,
}

pub struct InstallResult {
    pub path: &'static str,
    pub description: &'static str,
    pub outcome: WriteOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    Created,
    Updated,
    Merged,
    Skipped,
    Unchanged,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Check status of all managed files without writing anything.
pub fn check_all(root: &Path) -> anyhow::Result<Vec<UpdateReport>> {
    let manifest = load_manifest(root)?;
    let mut reports = Vec::new();

    for def in selected_defs(&manifest) {
        let status = check_file(root, def, &manifest)?;
        reports.push(UpdateReport {
            path: def.path,
            description: def.description,
            status,
        });
    }

    Ok(reports)
}

pub fn install_selected(
    root: &Path,
    force: bool,
    artifacts: &[ManagedArtifact],
) -> anyhow::Result<Vec<InstallResult>> {
    let selected_artifacts = canonical_artifacts(artifacts);
    if selected_artifacts.is_empty() {
        return Ok(Vec::new());
    }

    let manifest = load_manifest(root)?;
    let mut results = Vec::new();
    let mut new_manifest_files = manifest
        .as_ref()
        .map(|m| {
            m.files
                .iter()
                .filter(|entry| {
                    managed_file_def_for_path(&entry.path)
                        .map(|def| selected_artifacts.contains(&def.artifact))
                        .unwrap_or(false)
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();

    for def in selected_artifacts
        .iter()
        .filter_map(|artifact| managed_file_def(*artifact))
    {
        let previous_entry = manifest
            .as_ref()
            .and_then(|m| m.files.iter().find(|entry| entry.path == def.path))
            .cloned();
        let status = check_file(root, def, &manifest)?;
        let (outcome, written_hash, tempyr_hash) = write_file(root, def, status, force)?;

        match outcome {
            WriteOutcome::Skipped => {
                // Keep the last Tempyr-written manifest entry intact so later runs
                // continue to detect the on-disk file as user-modified.
                if let Some(entry) = previous_entry {
                    upsert_manifest_entry(&mut new_manifest_files, entry);
                }
            }
            _ => upsert_manifest_entry(
                &mut new_manifest_files,
                ManagedFile {
                    path: def.path.to_string(),
                    strategy: def.strategy,
                    written_hash,
                    tempyr_hash,
                },
            ),
        }

        results.push(InstallResult {
            path: def.path,
            description: def.description,
            outcome,
        });
    }

    let new_manifest = Manifest {
        tempyr_version: TEMPYR_VERSION.to_string(),
        managed_artifacts: Some(selected_artifacts),
        files: new_manifest_files,
    };
    save_manifest(root, &new_manifest)?;

    Ok(results)
}

/// Install/update all managed files and write the manifest.
/// If `force` is false, files detected as user-modified are skipped.
pub fn install_all(root: &Path, force: bool) -> anyhow::Result<Vec<InstallResult>> {
    install_selected(
        root,
        force,
        &[
            ManagedArtifact::Hooks,
            ManagedArtifact::Skill,
            ManagedArtifact::Agent,
        ],
    )
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

fn hash(content: &str) -> String {
    blake3::hash(content.as_bytes()).to_hex().to_string()
}

fn all_artifacts() -> Vec<ManagedArtifact> {
    MANAGED_FILES.iter().map(|def| def.artifact).collect()
}

fn canonical_artifacts(artifacts: &[ManagedArtifact]) -> Vec<ManagedArtifact> {
    MANAGED_FILES
        .iter()
        .filter_map(|def| artifacts.contains(&def.artifact).then_some(def.artifact))
        .collect()
}

fn managed_file_def(artifact: ManagedArtifact) -> Option<&'static ManagedFileDef> {
    MANAGED_FILES.iter().find(|def| def.artifact == artifact)
}

fn managed_file_def_for_path(path: &str) -> Option<&'static ManagedFileDef> {
    MANAGED_FILES.iter().find(|def| def.path == path)
}

fn selected_defs(manifest: &Option<Manifest>) -> Vec<&'static ManagedFileDef> {
    let artifacts = manifest
        .as_ref()
        .and_then(|manifest| manifest.managed_artifacts.as_deref())
        .map(canonical_artifacts)
        .unwrap_or_else(all_artifacts);

    artifacts.into_iter().filter_map(managed_file_def).collect()
}

fn check_file(
    root: &Path,
    def: &ManagedFileDef,
    manifest: &Option<Manifest>,
) -> anyhow::Result<FileStatus> {
    let file_path = root.join(def.path);
    let embedded_hash = hash(def.content);

    let on_disk = if file_path.exists() {
        Some(
            std::fs::read_to_string(&file_path)
                .with_context(|| format!("Failed to read {}", def.path))?,
        )
    } else {
        None
    };

    let manifest_entry = manifest
        .as_ref()
        .and_then(|m| m.files.iter().find(|f| f.path == def.path));

    match (on_disk, manifest_entry) {
        // File doesn't exist on disk.
        (None, _) => Ok(FileStatus::Missing),

        // File exists but tempyr never wrote it (no manifest entry).
        (Some(content), None) => {
            match def.strategy {
                Strategy::Overwrite => {
                    if hash(&content) == embedded_hash {
                        Ok(FileStatus::UpToDate)
                    } else {
                        // File exists with different content and no manifest.
                        // Treat as user-modified to be safe.
                        Ok(FileStatus::UserModified)
                    }
                }
                Strategy::Merge => {
                    // File exists but we never merged into it. Treat as stale
                    // so we can merge our hooks in.
                    Ok(FileStatus::Stale)
                }
            }
        }

        // File exists and we have a manifest entry.
        (Some(content), Some(entry)) => {
            let on_disk_hash = hash(&content);

            match def.strategy {
                Strategy::Overwrite => {
                    if on_disk_hash == embedded_hash {
                        Ok(FileStatus::UpToDate)
                    } else if on_disk_hash == entry.written_hash {
                        Ok(FileStatus::Stale)
                    } else {
                        Ok(FileStatus::UserModified)
                    }
                }
                Strategy::Merge => {
                    let tempyr_content_hash = hash(def.content);
                    if on_disk_hash != entry.written_hash {
                        Ok(FileStatus::UserModified)
                    } else if entry.tempyr_hash.as_deref() == Some(tempyr_content_hash.as_str()) {
                        Ok(FileStatus::UpToDate)
                    } else {
                        Ok(FileStatus::Stale)
                    }
                }
            }
        }
    }
}

fn write_file(
    root: &Path,
    def: &ManagedFileDef,
    status: FileStatus,
    force: bool,
) -> anyhow::Result<(WriteOutcome, String, Option<String>)> {
    let file_path = root.join(def.path);

    match status {
        FileStatus::UpToDate => {
            // Nothing to do; return current hashes.
            let content = if file_path.exists() {
                std::fs::read_to_string(&file_path)?
            } else {
                String::new()
            };
            let written_hash = hash(&content);
            let tempyr_hash = if def.strategy == Strategy::Merge {
                Some(hash(def.content))
            } else {
                None
            };
            Ok((WriteOutcome::Unchanged, written_hash, tempyr_hash))
        }
        FileStatus::UserModified if !force => {
            // Preserve user changes. The caller preserves any existing manifest
            // entry so later runs still detect this file as user-modified.
            let content = std::fs::read_to_string(&file_path)?;
            let written_hash = hash(&content);
            Ok((WriteOutcome::Skipped, written_hash, None))
        }
        _ => {
            // Create parent directories.
            if let Some(parent) = file_path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            let outcome = match status {
                FileStatus::Missing => WriteOutcome::Created,
                FileStatus::Stale if def.strategy == Strategy::Merge => WriteOutcome::Merged,
                FileStatus::UserModified if def.strategy == Strategy::Merge => WriteOutcome::Merged,
                _ => WriteOutcome::Updated,
            };

            let final_content = match def.strategy {
                Strategy::Overwrite => def.content.to_string(),
                Strategy::Merge => {
                    let existing = if file_path.exists() {
                        std::fs::read_to_string(&file_path)
                            .with_context(|| format!("Failed to read {}", def.path))?
                    } else {
                        String::new()
                    };
                    merge_settings(&existing, def.content)?
                }
            };

            std::fs::write(&file_path, &final_content)
                .with_context(|| format!("Failed to write {}", def.path))?;

            let written_hash = hash(&final_content);
            let tempyr_hash = if def.strategy == Strategy::Merge {
                Some(hash(def.content))
            } else {
                None
            };

            Ok((outcome, written_hash, tempyr_hash))
        }
    }
}

// ---------------------------------------------------------------------------
// Manifest I/O
// ---------------------------------------------------------------------------

fn manifest_path(root: &Path) -> std::path::PathBuf {
    root.join(".tempyr").join("managed.toml")
}

fn load_manifest(root: &Path) -> anyhow::Result<Option<Manifest>> {
    let path = manifest_path(root);
    if !path.exists() {
        return Ok(None);
    }
    let content =
        std::fs::read_to_string(&path).with_context(|| "Failed to read .tempyr/managed.toml")?;
    let manifest: Manifest =
        toml::from_str(&content).with_context(|| "Failed to parse .tempyr/managed.toml")?;
    Ok(Some(manifest))
}

fn save_manifest(root: &Path, manifest: &Manifest) -> anyhow::Result<()> {
    let path = manifest_path(root);
    let content =
        toml::to_string_pretty(manifest).with_context(|| "Failed to serialize manifest")?;
    std::fs::write(&path, content).with_context(|| "Failed to write .tempyr/managed.toml")?;
    Ok(())
}

fn upsert_manifest_entry(entries: &mut Vec<ManagedFile>, entry: ManagedFile) {
    if let Some(existing) = entries
        .iter_mut()
        .find(|existing| existing.path == entry.path)
    {
        *existing = entry;
    } else {
        entries.push(entry);
    }
}

// ---------------------------------------------------------------------------
// settings.json merge
// ---------------------------------------------------------------------------

fn is_managed_hook(entry: &serde_json::Value, managed_entries: &[serde_json::Value]) -> bool {
    managed_entries
        .iter()
        .any(|managed_entry| entry == managed_entry)
        || is_legacy_managed_hook(entry)
}

fn is_legacy_managed_hook(entry: &serde_json::Value) -> bool {
    const LEGACY_GRAPH_WRITE_COMMAND: &str = "bash -c 'INPUT=$(cat); if echo \"$INPUT\" | grep -q \"graph/\"; then tempyr index update --json; fi'";

    if entry.get("matcher").and_then(|m| m.as_str()) != Some("Edit|Write") {
        return false;
    }

    entry
        .get("hooks")
        .and_then(|h| h.as_array())
        .is_some_and(|hooks| {
            hooks.iter().any(|hook| {
                hook.get("command").and_then(|c| c.as_str()) == Some(LEGACY_GRAPH_WRITE_COMMAND)
            })
        })
}

fn merge_settings(existing_json: &str, tempyr_hooks_json: &str) -> anyhow::Result<String> {
    let mut doc: serde_json::Value = if existing_json.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(existing_json)
            .with_context(|| "Failed to parse existing .claude/settings.json")?
    };

    let tempyr_settings: serde_json::Value = serde_json::from_str(tempyr_hooks_json)
        .with_context(|| "Failed to parse embedded tempyr hooks")?;

    let tempyr_hooks = tempyr_settings
        .get("hooks")
        .and_then(|v| v.as_object())
        .ok_or_else(|| anyhow::anyhow!("embedded tempyr hooks must contain a hooks object"))?;

    // Ensure hooks exists as an object.
    let hooks = doc
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!(".claude/settings.json root is not an object"))?
        .entry("hooks")
        .or_insert(serde_json::json!({}));
    let hooks_obj = hooks
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("hooks is not an object in .claude/settings.json"))?;

    for (event, tempyr_entries) in tempyr_hooks {
        let tempyr_entries = tempyr_entries
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("embedded hooks.{event} is not an array"))?
            .clone();
        let entries = hooks_obj
            .entry(event.clone())
            .or_insert(serde_json::json!([]));
        let arr = entries
            .as_array_mut()
            .ok_or_else(|| anyhow::anyhow!("hooks.{event} is not an array"))?;

        arr.retain(|entry| !is_managed_hook(entry, &tempyr_entries));
        arr.extend(tempyr_entries);
    }

    let mut output = serde_json::to_string_pretty(&doc)?;
    output.push('\n');
    Ok(output)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_managed_hook_matches_exact_managed_entry() {
        let managed_entry = serde_json::json!({
            "matcher": "mcp__tempyr__graph_add_node|mcp__tempyr__graph_update_node",
            "hooks": [{"type": "command", "command": "tempyr validate --json"}]
        });
        assert!(is_managed_hook(
            &managed_entry,
            std::slice::from_ref(&managed_entry)
        ));
    }

    #[test]
    fn test_is_managed_hook_matches_legacy_graph_write_hook() {
        let entry = serde_json::json!({
            "matcher": "Edit|Write",
            "hooks": [{"type": "command", "command": "bash -c 'INPUT=$(cat); if echo \"$INPUT\" | grep -q \"graph/\"; then tempyr index update --json; fi'"}]
        });
        assert!(is_managed_hook(&entry, &[]));
    }

    #[test]
    fn test_is_managed_hook_preserves_user_tempyr_command() {
        let entry = serde_json::json!({
            "hooks": [{"type": "command", "command": "tempyr custom-health --json"}]
        });
        assert!(!is_managed_hook(&entry, &[]));
    }

    #[test]
    fn test_merge_settings_empty_existing() {
        let tempyr = r#"{"hooks":{"PostToolUse":[{"matcher":"mcp__tempyr__graph_add_node","hooks":[{"type":"command","command":"tempyr validate --json"}]}]}}"#;
        let result = merge_settings("", tempyr).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let arr = parsed
            .pointer("/hooks/PostToolUse")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(arr.len(), 1);
        let managed_entry = arr[0].clone();
        assert!(is_managed_hook(
            &managed_entry,
            std::slice::from_ref(&managed_entry)
        ));
    }

    #[test]
    fn test_merge_settings_preserves_user_hooks() {
        let existing = r#"{
  "hooks": {
    "PostToolUse": [
      {"matcher": "Edit|Write", "hooks": [{"type": "command", "command": "eslint --fix $FILE"}]},
      {"matcher": "mcp__tempyr__graph_add_node|mcp__tempyr__graph_update_node", "hooks": [{"type":"command","command":"tempyr validate --json"},{"type":"command","command":"tempyr index update --json"}]}
    ]
  }
}"#;
        let tempyr = r#"{"hooks":{"PostToolUse":[{"matcher":"mcp__tempyr__graph_add_node|mcp__tempyr__graph_update_node","hooks":[{"type":"command","command":"tempyr validate --json"},{"type":"command","command":"tempyr index update --json"}]}]}}"#;
        let result = merge_settings(existing, tempyr).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let arr = parsed
            .pointer("/hooks/PostToolUse")
            .unwrap()
            .as_array()
            .unwrap();

        // User's eslint hook preserved, old tempyr hook removed, new tempyr hook added.
        assert_eq!(arr.len(), 2);
        assert!(!is_managed_hook(&arr[0], &[])); // eslint
        assert_eq!(
            arr[1]["matcher"].as_str().unwrap(),
            "mcp__tempyr__graph_add_node|mcp__tempyr__graph_update_node"
        );
        assert_eq!(
            arr[0]["hooks"][0]["command"].as_str().unwrap(),
            "eslint --fix $FILE"
        );
    }

    #[test]
    fn test_merge_settings_idempotent() {
        let tempyr = TEMPYR_HOOKS_JSON;
        let first = merge_settings("", tempyr).unwrap();
        let second = merge_settings(&first, tempyr).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn test_merge_settings_installs_full_tempyr_hook_set() {
        let result = merge_settings("", TEMPYR_HOOKS_JSON).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();

        for event in ["SessionStart", "SessionEnd", "PostToolUse"] {
            let arr = parsed
                .pointer(&format!("/hooks/{event}"))
                .and_then(|value| value.as_array())
                .unwrap_or_else(|| panic!("missing hooks.{event}"));
            assert!(
                !arr.is_empty(),
                "hooks.{event} should include a managed tempyr hook"
            );
        }
    }

    #[test]
    fn test_merge_settings_preserves_user_session_hooks() {
        let existing = r#"{
  "hooks": {
    "SessionStart": [
      {"hooks": [{"type": "command", "command": "echo hello"}]}
    ]
  }
}"#;

        let result = merge_settings(existing, TEMPYR_HOOKS_JSON).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let arr = parsed
            .pointer("/hooks/SessionStart")
            .and_then(|value| value.as_array())
            .unwrap();

        assert_eq!(arr.len(), 2);
        assert_eq!(
            arr[0]["hooks"][0]["command"].as_str().unwrap(),
            "echo hello"
        );
        assert_eq!(
            arr[1]["hooks"][0]["command"].as_str().unwrap(),
            "tempyr journal bootstrap --quiet --json"
        );
    }

    #[test]
    fn test_merge_settings_preserves_user_tempyr_session_hook() {
        let existing = r#"{
  "hooks": {
    "SessionStart": [
      {"hooks": [{"type": "command", "command": "tempyr custom-health --json"}]}
    ]
  }
}"#;

        let result = merge_settings(existing, TEMPYR_HOOKS_JSON).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let arr = parsed
            .pointer("/hooks/SessionStart")
            .and_then(|value| value.as_array())
            .unwrap();

        assert_eq!(arr.len(), 2);
        assert_eq!(
            arr[0]["hooks"][0]["command"].as_str().unwrap(),
            "tempyr custom-health --json"
        );
        assert_eq!(
            arr[1]["hooks"][0]["command"].as_str().unwrap(),
            "tempyr journal bootstrap --quiet --json"
        );
    }

    #[test]
    fn test_manifest_round_trip() {
        let manifest = Manifest {
            tempyr_version: "0.1.0".to_string(),
            managed_artifacts: Some(vec![ManagedArtifact::Hooks, ManagedArtifact::Skill]),
            files: vec![
                ManagedFile {
                    path: ".claude/settings.json".to_string(),
                    strategy: Strategy::Merge,
                    written_hash: "abc".to_string(),
                    tempyr_hash: Some("def".to_string()),
                },
                ManagedFile {
                    path: ".claude/skills/tempyr-interview/SKILL.md".to_string(),
                    strategy: Strategy::Overwrite,
                    written_hash: "ghi".to_string(),
                    tempyr_hash: None,
                },
            ],
        };
        let serialized = toml::to_string_pretty(&manifest).unwrap();
        let deserialized: Manifest = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.tempyr_version, manifest.tempyr_version);
        assert_eq!(
            deserialized.managed_artifacts,
            Some(vec![ManagedArtifact::Hooks, ManagedArtifact::Skill])
        );
        assert_eq!(deserialized.files.len(), 2);
        assert_eq!(deserialized.files[0].strategy, Strategy::Merge);
        assert_eq!(deserialized.files[0].tempyr_hash, Some("def".to_string()));
        assert_eq!(deserialized.files[1].strategy, Strategy::Overwrite);
        assert!(deserialized.files[1].tempyr_hash.is_none());
    }

    #[test]
    fn test_check_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".tempyr")).unwrap();
        let def = &MANAGED_FILES[1]; // SKILL.md
        let status = check_file(dir.path(), def, &None).unwrap();
        assert_eq!(status, FileStatus::Missing);
    }

    #[test]
    fn test_check_file_up_to_date() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".tempyr")).unwrap();
        let def = &MANAGED_FILES[1]; // SKILL.md
        let file_path = dir.path().join(def.path);
        std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        std::fs::write(&file_path, def.content).unwrap();

        let manifest = Some(Manifest {
            tempyr_version: "0.1.0".to_string(),
            managed_artifacts: None,
            files: vec![ManagedFile {
                path: def.path.to_string(),
                strategy: Strategy::Overwrite,
                written_hash: hash(def.content),
                tempyr_hash: None,
            }],
        });

        let status = check_file(dir.path(), def, &manifest).unwrap();
        assert_eq!(status, FileStatus::UpToDate);
    }

    #[test]
    fn test_check_file_stale() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".tempyr")).unwrap();
        let def = &MANAGED_FILES[1]; // SKILL.md
        let file_path = dir.path().join(def.path);
        std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        let old_content = "old content that tempyr wrote";
        std::fs::write(&file_path, old_content).unwrap();

        let manifest = Some(Manifest {
            tempyr_version: "0.0.1".to_string(),
            managed_artifacts: None,
            files: vec![ManagedFile {
                path: def.path.to_string(),
                strategy: Strategy::Overwrite,
                written_hash: hash(old_content),
                tempyr_hash: None,
            }],
        });

        let status = check_file(dir.path(), def, &manifest).unwrap();
        assert_eq!(status, FileStatus::Stale);
    }

    #[test]
    fn test_check_file_user_modified() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".tempyr")).unwrap();
        let def = &MANAGED_FILES[1]; // SKILL.md
        let file_path = dir.path().join(def.path);
        std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        std::fs::write(&file_path, "user edited this").unwrap();

        let manifest = Some(Manifest {
            tempyr_version: "0.1.0".to_string(),
            managed_artifacts: None,
            files: vec![ManagedFile {
                path: def.path.to_string(),
                strategy: Strategy::Overwrite,
                written_hash: hash("what tempyr originally wrote"),
                tempyr_hash: None,
            }],
        });

        let status = check_file(dir.path(), def, &manifest).unwrap();
        assert_eq!(status, FileStatus::UserModified);
    }

    #[test]
    fn test_install_all_creates_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".tempyr")).unwrap();

        let results = install_all(dir.path(), false).unwrap();

        assert_eq!(results.len(), 3);
        for r in &results {
            assert!(
                r.outcome == WriteOutcome::Created || r.outcome == WriteOutcome::Merged,
                "expected Created or Merged for {}, got {:?}",
                r.path,
                r.outcome
            );
            assert!(dir.path().join(r.path).exists(), "{} should exist", r.path);
        }

        // Manifest should exist.
        assert!(dir.path().join(".tempyr/managed.toml").exists());

        // Running again should be unchanged.
        let results2 = install_all(dir.path(), false).unwrap();
        for r in &results2 {
            assert_eq!(
                r.outcome,
                WriteOutcome::Unchanged,
                "{} should be unchanged on second run",
                r.path
            );
        }

        let manifest = load_manifest(dir.path()).unwrap().unwrap();
        assert_eq!(manifest.managed_artifacts, Some(all_artifacts()));
    }

    #[test]
    fn test_skip_preserves_manifest_for_user_modified_overwrite_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".tempyr")).unwrap();
        install_all(dir.path(), false).unwrap();

        let before_manifest = load_manifest(dir.path()).unwrap().unwrap();
        let def = &MANAGED_FILES[1]; // SKILL.md
        let before_entry = before_manifest
            .files
            .iter()
            .find(|entry| entry.path == def.path)
            .unwrap()
            .clone();

        let file_path = dir.path().join(def.path);
        std::fs::write(&file_path, "user edited this").unwrap();

        let results = install_selected(dir.path(), false, &[ManagedArtifact::Skill]).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].outcome, WriteOutcome::Skipped);

        let after_manifest = load_manifest(dir.path()).unwrap().unwrap();
        let after_entry = after_manifest
            .files
            .iter()
            .find(|entry| entry.path == def.path)
            .unwrap();
        assert_eq!(after_entry.written_hash, before_entry.written_hash);
        assert_eq!(after_entry.tempyr_hash, before_entry.tempyr_hash);

        let after_manifest = Some(after_manifest);
        assert_eq!(
            check_file(dir.path(), def, &after_manifest).unwrap(),
            FileStatus::UserModified
        );
    }

    #[test]
    fn test_skip_preserves_manifest_for_user_modified_merge_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".tempyr")).unwrap();
        install_all(dir.path(), false).unwrap();

        let before_manifest = load_manifest(dir.path()).unwrap().unwrap();
        let def = &MANAGED_FILES[0]; // settings.json
        let before_entry = before_manifest
            .files
            .iter()
            .find(|entry| entry.path == def.path)
            .unwrap()
            .clone();

        let file_path = dir.path().join(def.path);
        std::fs::write(&file_path, "{\"hooks\":{},\"user\":true}\n").unwrap();

        let results = install_selected(dir.path(), false, &[ManagedArtifact::Hooks]).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].outcome, WriteOutcome::Skipped);

        let after_manifest = load_manifest(dir.path()).unwrap().unwrap();
        let after_entry = after_manifest
            .files
            .iter()
            .find(|entry| entry.path == def.path)
            .unwrap();
        assert_eq!(after_entry.written_hash, before_entry.written_hash);
        assert_eq!(after_entry.tempyr_hash, before_entry.tempyr_hash);

        let after_manifest = Some(after_manifest);
        assert_eq!(
            check_file(dir.path(), def, &after_manifest).unwrap(),
            FileStatus::UserModified
        );
    }

    #[test]
    fn test_install_selected_empty_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();

        let results = install_selected(dir.path(), false, &[]).unwrap();

        assert!(results.is_empty());
        assert!(!dir.path().join(".tempyr/managed.toml").exists());
    }

    #[test]
    fn test_check_all_respects_selected_artifacts_from_manifest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".tempyr")).unwrap();

        let results = install_selected(dir.path(), false, &[ManagedArtifact::Hooks]).unwrap();
        assert_eq!(results.len(), 1);

        let reports = check_all(dir.path()).unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].path, ".claude/settings.json");
    }
}
