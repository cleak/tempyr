//! `tempyr schema upgrade` — apply named, versioned additive deltas to the
//! project's `.tempyr/schema.toml`.
//!
//! Deltas are keyed by name and recorded in `.tempyr/config.toml` under
//! `[schema] applied_deltas`. A recorded delta is never re-applied, so edges a
//! user removes after an upgrade stay removed. Edits go through `toml_edit` so
//! user comments and formatting survive.

use std::fs;
use std::path::Path;

use anyhow::Context;
use serde::Serialize;
use tempyr_core::schema::Schema;
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, TableLike, Value};

use super::gitignore;
use crate::config::ProjectContext;

/// A named, additive change to the default schema.
pub struct SchemaDelta {
    pub name: &'static str,
    pub description: &'static str,
    /// `(node_type, edge_type, target)` additions to `allowed_edges`.
    pub edges: &'static [(&'static str, &'static str, &'static str)],
    /// `(edge_type, reverse)` definitions added to `[edge_types]` when missing.
    pub edge_types: &'static [(&'static str, &'static str)],
}

/// Every known delta, oldest first. The default schema already contains all of
/// them, so `tempyr init` records every name as applied.
pub const SCHEMA_DELTAS: &[SchemaDelta] = &[SchemaDelta {
    name: "2026-10-interview-edges",
    description: "Risks and open questions on epics; risks on decisions",
    edges: &[
        ("epic", "has_risk", "risk"),
        ("epic", "has_question", "open_question"),
        ("decision", "has_risk", "risk"),
        ("risk", "risk_for", "epic"),
        ("risk", "risk_for", "decision"),
        ("open_question", "question_for", "epic"),
    ],
    edge_types: &[
        ("has_risk", "risk_for"),
        ("risk_for", "has_risk"),
        ("has_question", "question_for"),
        ("question_for", "has_question"),
    ],
}];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EdgeRef {
    pub node_type: String,
    pub edge_type: String,
    pub target: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkippedEdge {
    #[serde(flatten)]
    pub edge: EdgeRef,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeltaStatus {
    /// Recorded in config.toml; nothing is evaluated.
    AlreadyApplied,
    /// Not yet recorded; the listed changes will be (or were) applied.
    Pending,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeltaReport {
    pub name: String,
    pub description: String,
    pub status: DeltaStatus,
    pub added_edges: Vec<EdgeRef>,
    pub added_edge_types: Vec<(String, String)>,
    pub already_present: Vec<EdgeRef>,
    pub skipped: Vec<SkippedEdge>,
}

/// The computed result of upgrading a schema/config pair. Pure data so it
/// can be tested without touching the filesystem.
pub struct UpgradePlan {
    pub schema_text: String,
    pub config_text: String,
    pub schema_changed: bool,
    pub config_changed: bool,
    pub deltas: Vec<DeltaReport>,
}

/// Render the `[schema]` config block `tempyr init` appends, recording every
/// known delta as applied (the default schema already includes them).
pub fn render_applied_deltas_block() -> String {
    let names = SCHEMA_DELTAS
        .iter()
        .map(|delta| format!("\"{}\"", delta.name))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"
[schema]
# Named schema deltas already present in .tempyr/schema.toml.
# `tempyr schema upgrade` never re-applies a delta listed here.
applied_deltas = [{names}]
"#
    )
}

/// Compute the upgraded schema and config text for `deltas`.
pub fn plan_upgrade(
    schema_text: &str,
    config_text: &str,
    deltas: &[SchemaDelta],
) -> anyhow::Result<UpgradePlan> {
    // toml_edit emits `\n` after every line it re-renders, so edit on LF text
    // and restore CRLF afterwards (Windows checkouts with autocrlf).
    let schema_crlf = schema_text.contains("\r\n");
    let config_crlf = config_text.contains("\r\n");
    let mut schema_doc: DocumentMut = schema_text
        .replace("\r\n", "\n")
        .parse()
        .context("parsing schema.toml")?;
    let mut config_doc: DocumentMut = config_text
        .replace("\r\n", "\n")
        .parse()
        .context("parsing config.toml")?;
    let applied = applied_deltas(&config_doc)?;

    let mut reports = Vec::new();
    for delta in deltas {
        let mut report = DeltaReport {
            name: delta.name.to_string(),
            description: delta.description.to_string(),
            status: DeltaStatus::Pending,
            added_edges: Vec::new(),
            added_edge_types: Vec::new(),
            already_present: Vec::new(),
            skipped: Vec::new(),
        };
        if applied.iter().any(|name| name == delta.name) {
            report.status = DeltaStatus::AlreadyApplied;
            reports.push(report);
            continue;
        }

        apply_delta(&mut schema_doc, delta, &mut report)?;
        record_applied_delta(&mut config_doc, delta.name)?;
        reports.push(report);
    }

    let new_schema = restore_crlf(schema_doc.to_string(), schema_crlf);
    // The upgraded schema must still load and accept every edge we added.
    let parsed: Schema = new_schema
        .parse()
        .context("upgraded schema.toml no longer parses")?;
    for edge in reports.iter().flat_map(|report| &report.added_edges) {
        parsed
            .validate_edge(&edge.node_type, &edge.edge_type, &edge.target)
            .context("upgraded schema.toml rejects an added edge")?;
    }

    let new_config = restore_crlf(config_doc.to_string(), config_crlf);
    Ok(UpgradePlan {
        schema_changed: new_schema != schema_text,
        config_changed: new_config != config_text,
        schema_text: new_schema,
        config_text: new_config,
        deltas: reports,
    })
}

fn restore_crlf(text: String, crlf: bool) -> String {
    if crlf {
        text.replace('\n', "\r\n")
    } else {
        text
    }
}

fn applied_deltas(config: &DocumentMut) -> anyhow::Result<Vec<String>> {
    let Some(item) = config.get("schema").and_then(|s| s.get("applied_deltas")) else {
        return Ok(Vec::new());
    };
    let array = item
        .as_array()
        .context("config.toml: [schema] applied_deltas must be an array of strings")?;
    array
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .context("config.toml: [schema] applied_deltas must be an array of strings")
        })
        .collect()
}

fn record_applied_delta(config: &mut DocumentMut, name: &str) -> anyhow::Result<()> {
    let schema = config
        .entry("schema")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_like_mut()
        .context("config.toml: `schema` must be a table")?;
    let array = schema
        .entry("applied_deltas")
        .or_insert(Item::Value(Value::Array(Array::new())))
        .as_array_mut()
        .context("config.toml: [schema] applied_deltas must be an array of strings")?;
    array.push(name);
    Ok(())
}

fn apply_delta(
    doc: &mut DocumentMut,
    delta: &SchemaDelta,
    report: &mut DeltaReport,
) -> anyhow::Result<()> {
    let edge_types = doc
        .get_mut("edge_types")
        .and_then(Item::as_table_like_mut)
        .context("schema.toml has no [edge_types] table")?;
    for (edge_type, reverse) in delta.edge_types {
        if edge_types.contains_key(edge_type) {
            continue;
        }
        let mut def = InlineTable::new();
        def.insert("reverse", (*reverse).into());
        edge_types.insert(edge_type, Item::Value(Value::InlineTable(def)));
        report
            .added_edge_types
            .push((edge_type.to_string(), reverse.to_string()));
    }
    let known_edge_types: Vec<String> = edge_types.iter().map(|(k, _)| k.to_string()).collect();

    let node_types = doc
        .get_mut("node_types")
        .and_then(Item::as_table_like_mut)
        .context("schema.toml has no [node_types] table")?;
    let known_node_types: Vec<String> = node_types.iter().map(|(k, _)| k.to_string()).collect();

    for (node_type, edge_type, target) in delta.edges {
        let edge = EdgeRef {
            node_type: node_type.to_string(),
            edge_type: edge_type.to_string(),
            target: target.to_string(),
        };
        let skip_reason = if !known_node_types.iter().any(|t| t == node_type) {
            Some(format!("node type `{node_type}` is not in this schema"))
        } else if *target != "*" && !known_node_types.iter().any(|t| t == target) {
            Some(format!("target node type `{target}` is not in this schema"))
        } else if !known_edge_types.iter().any(|t| t == edge_type) {
            Some(format!("edge type `{edge_type}` is not in [edge_types]"))
        } else {
            None
        };
        if let Some(reason) = skip_reason {
            report.skipped.push(SkippedEdge { edge, reason });
            continue;
        }

        let node_def = node_types
            .get_mut(node_type)
            .and_then(Item::as_table_like_mut)
            .with_context(|| format!("schema.toml: node_types.{node_type} must be a table"))?;
        if push_allowed_edge(node_def, node_type, edge_type, target)? {
            report.added_edges.push(edge);
        } else {
            report.already_present.push(edge);
        }
    }
    Ok(())
}

/// Append `{ type, target }` to a node type's `allowed_edges`, matching the
/// existing layout (inline array or `[[...allowed_edges]]`). Returns `false`
/// when the triple is already present.
fn push_allowed_edge(
    node_def: &mut dyn TableLike,
    node_type: &str,
    edge_type: &str,
    target: &str,
) -> anyhow::Result<bool> {
    let matches = |entry: &dyn TableLike| {
        entry.get("type").and_then(Item::as_str) == Some(edge_type)
            && entry.get("target").and_then(Item::as_str) == Some(target)
    };

    let item = node_def
        .entry("allowed_edges")
        .or_insert(Item::Value(Value::Array(Array::new())));
    match item {
        Item::Value(Value::Array(array)) => {
            if array
                .iter()
                .filter_map(Value::as_inline_table)
                .any(|entry| matches(entry))
            {
                return Ok(false);
            }
            let mut entry = InlineTable::new();
            entry.insert("type", edge_type.into());
            entry.insert("target", target.into());
            let mut value = Value::InlineTable(entry);

            value.decor_mut().set_prefix(append_prefix(array));
            value.decor_mut().set_suffix("");
            array.push_formatted(value);
            Ok(true)
        }
        Item::ArrayOfTables(tables) => {
            if tables.iter().any(|entry| matches(entry)) {
                return Ok(false);
            }
            let mut entry = Table::new();
            entry.insert("type", toml_edit::value(edge_type));
            entry.insert("target", toml_edit::value(target));
            tables.push(entry);
            Ok(true)
        }
        _ => anyhow::bail!("schema.toml: node_types.{node_type}.allowed_edges must be an array"),
    }
}

/// Prepare `array` for one more element and return the prefix that element
/// needs so the result matches the existing layout.
///
/// toml_edit keeps the whitespace (and any comment) between the last element
/// and `]` as that element's suffix when there is no trailing comma, so a
/// plain push would land after the newline. That text is moved to the array's
/// trailing decor first; a multi-line array also gains a trailing comma. A
/// same-line comment after the last element stays with that element.
fn append_prefix(array: &mut Array) -> String {
    fn raw(text: Option<&toml_edit::RawString>) -> String {
        text.and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string()
    }

    let Some(last_prefix) = array.iter().last().map(|last| raw(last.decor().prefix())) else {
        // Keep any comment lines that sit inside the empty array.
        let existing = raw(Some(array.trailing()));
        array.set_trailing("\n");
        array.set_trailing_comma(true);
        return format!("{}\n  ", existing.trim_end());
    };

    // Reuse the indentation of the last element, dropping any comment lines
    // that precede it.
    let indent = match last_prefix.rfind('\n') {
        Some(i) => last_prefix[i..].to_string(),
        None => " ".to_string(),
    };

    if !array.trailing_comma() {
        let last = array.iter_mut().last().expect("non-empty array");
        let suffix = raw(last.decor().suffix());
        last.decor_mut().set_suffix("");
        let trailing = format!("{suffix}{}", raw(Some(array.trailing())));
        array.set_trailing_comma(trailing.contains('\n'));
        array.set_trailing(trailing);
    }

    let trailing = raw(Some(array.trailing()));
    match trailing.find('\n') {
        Some(i) if trailing[..i].trim_start().starts_with('#') => {
            array.set_trailing(&trailing[i..]);
            format!("{}{indent}", &trailing[..i])
        }
        _ => indent,
    }
}

/// `tempyr schema upgrade`. Dry-run unless `apply` is set.
pub fn run_upgrade(ctx: &ProjectContext, apply: bool, json: bool) -> anyhow::Result<()> {
    let schema_path = ctx.tempyr_dir.join("schema.toml");
    let config_path = ctx.tempyr_dir.join("config.toml");
    let schema_text = fs::read_to_string(&schema_path)
        .with_context(|| format!("reading {}", schema_path.display()))?;
    let config_text = read_optional(&config_path)?;

    let plan = plan_upgrade(&schema_text, &config_text, SCHEMA_DELTAS)?;
    let missing_ignores =
        gitignore::missing_entries(&ctx.root, gitignore::INTERVIEW_ARCHIVE_ENTRIES)?;

    let mut written = Vec::new();
    if apply {
        // Schema first: if the config write fails, a re-run finds the edges
        // already present and only records the delta.
        if plan.schema_changed {
            fs::write(&schema_path, &plan.schema_text)
                .with_context(|| format!("writing {}", schema_path.display()))?;
            written.push(".tempyr/schema.toml");
        }
        if plan.config_changed {
            fs::write(&config_path, &plan.config_text)
                .with_context(|| format!("writing {}", config_path.display()))?;
            written.push(".tempyr/config.toml");
        }
        let added = gitignore::ensure_entries(
            &ctx.root,
            gitignore::INTERVIEW_ARCHIVE_HEADER,
            gitignore::INTERVIEW_ARCHIVE_ENTRIES,
        )?;
        if !added.is_empty() {
            written.push(".gitignore");
        }
    }

    if json {
        let output = serde_json::json!({
            "applied": apply,
            "schema_changed": plan.schema_changed,
            "config_changed": plan.config_changed,
            "deltas": plan.deltas,
            "gitignore_additions": missing_ignores,
            "files_written": written,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        let written = apply.then_some(written.as_slice());
        print!("{}", render_text(&plan, &missing_ignores, written));
    }
    Ok(())
}

fn read_optional(path: &Path) -> anyhow::Result<String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
    }
}

/// `written` is `None` for a dry run, otherwise the files actually updated.
fn render_text(plan: &UpgradePlan, missing_ignores: &[&str], written: Option<&[&str]>) -> String {
    let mut out = String::new();
    let pending = plan.deltas.iter().any(|d| d.status == DeltaStatus::Pending);

    for delta in &plan.deltas {
        if delta.status == DeltaStatus::AlreadyApplied {
            out.push_str(&format!("= {} (already applied)\n", delta.name));
            continue;
        }
        out.push_str(&format!("+ {}: {}\n", delta.name, delta.description));
        for (edge_type, reverse) in &delta.added_edge_types {
            out.push_str(&format!(
                "    [edge_types] + {edge_type} = {{ reverse = \"{reverse}\" }}\n"
            ));
        }
        for edge in &delta.added_edges {
            out.push_str(&format!(
                "    [node_types.{}] + {{ type = \"{}\", target = \"{}\" }}\n",
                edge.node_type, edge.edge_type, edge.target
            ));
        }
        for edge in &delta.already_present {
            out.push_str(&format!(
                "    already present: {} -{}-> {}\n",
                edge.node_type, edge.edge_type, edge.target
            ));
        }
        for skipped in &delta.skipped {
            out.push_str(&format!(
                "    skipped: {} -{}-> {} ({})\n",
                skipped.edge.node_type, skipped.edge.edge_type, skipped.edge.target, skipped.reason
            ));
        }
    }

    for entry in missing_ignores {
        out.push_str(&format!("+ .gitignore: {entry}\n"));
    }

    if !pending && missing_ignores.is_empty() {
        out.push_str("Schema is up to date.\n");
    } else if let Some(written) = written {
        if written.is_empty() {
            out.push_str("Applied. No files needed changes.\n");
        } else {
            out.push_str(&format!("Applied. Updated {}.\n", join_list(written)));
        }
    } else {
        out.push_str(
            "Dry run: nothing written. Re-run with `tempyr schema upgrade --apply` to apply.\n",
        );
    }
    out
}

/// `a`, `a and b`, `a, b, and c`.
fn join_list(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => one.to_string(),
        [a, b] => format!("{a} and {b}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT_SCHEMA: &str = include_str!("../../../../schema/default-schema.toml");

    /// A pre-delta schema: the shipped default with every delta edge removed.
    fn legacy_schema() -> String {
        let mut text = DEFAULT_SCHEMA.to_string();
        // Windows checkouts may have CRLF line endings.
        let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
        for delta in SCHEMA_DELTAS {
            for (node_type, edge_type, target) in delta.edges {
                let line = format!("  {{ type = \"{edge_type}\", target = \"{target}\" }},{eol}");
                // Identical lines belong to other node types; strip the one
                // inside the owning node type's section.
                let section = text
                    .find(&format!("[node_types.{node_type}]"))
                    .expect("node type in default schema");
                let idx = section + text[section..].find(&line).expect("delta edge in schema");
                text.replace_range(idx..idx + line.len(), "");
            }
        }
        text
    }

    fn delta() -> &'static SchemaDelta {
        &SCHEMA_DELTAS[0]
    }

    #[test]
    fn default_schema_already_contains_every_delta() {
        let plan = plan_upgrade(DEFAULT_SCHEMA, "", SCHEMA_DELTAS).unwrap();
        assert!(!plan.schema_changed);
        for report in &plan.deltas {
            assert!(report.added_edges.is_empty(), "{report:?}");
            assert!(report.skipped.is_empty(), "{report:?}");
        }
    }

    #[test]
    fn applies_missing_edges_and_records_delta() {
        let legacy = legacy_schema();
        let before: Schema = legacy.parse().unwrap();
        assert!(before.validate_edge("epic", "has_risk", "risk").is_err());

        let plan = plan_upgrade(&legacy, "", SCHEMA_DELTAS).unwrap();
        assert!(plan.schema_changed && plan.config_changed);
        assert_eq!(plan.deltas[0].added_edges.len(), delta().edges.len());

        let after: Schema = plan.schema_text.parse().unwrap();
        for (node_type, edge_type, target) in delta().edges {
            after.validate_edge(node_type, edge_type, target).unwrap();
        }
        assert!(plan.config_text.contains("[schema]"));
        assert!(plan.config_text.contains("2026-10-interview-edges"));
        // Appended entries reuse the existing multi-line layout: upgrading the
        // pre-delta schema reproduces the shipped default byte for byte.
        assert_eq!(plan.schema_text, DEFAULT_SCHEMA);
    }

    #[test]
    fn preserves_crlf_line_endings() {
        let crlf = legacy_schema().replace("\r\n", "\n").replace('\n', "\r\n");
        let plan = plan_upgrade(&crlf, "", SCHEMA_DELTAS).unwrap();
        assert!(!plan.schema_text.replace("\r\n", "").contains('\n'));
        assert_eq!(
            plan.schema_text,
            DEFAULT_SCHEMA.replace("\r\n", "\n").replace('\n', "\r\n")
        );
    }

    #[test]
    fn upgrade_is_idempotent() {
        let legacy = legacy_schema();
        let first = plan_upgrade(&legacy, "", SCHEMA_DELTAS).unwrap();
        let second = plan_upgrade(&first.schema_text, &first.config_text, SCHEMA_DELTAS).unwrap();
        assert!(!second.schema_changed);
        assert!(!second.config_changed);
        assert_eq!(second.deltas[0].status, DeltaStatus::AlreadyApplied);
    }

    #[test]
    fn already_present_edges_are_not_duplicated() {
        // Unrecorded delta against a schema that already has the edges: only
        // the config changes.
        let plan = plan_upgrade(DEFAULT_SCHEMA, "", SCHEMA_DELTAS).unwrap();
        assert!(!plan.schema_changed);
        assert!(plan.config_changed);
        assert_eq!(plan.deltas[0].already_present.len(), delta().edges.len());
    }

    #[test]
    fn recorded_delta_is_never_reapplied() {
        let legacy = legacy_schema();
        let config = "[schema]\napplied_deltas = [\"2026-10-interview-edges\"]\n";
        let plan = plan_upgrade(&legacy, config, SCHEMA_DELTAS).unwrap();
        assert!(!plan.schema_changed);
        assert!(!plan.config_changed);
        assert_eq!(plan.deltas[0].status, DeltaStatus::AlreadyApplied);
    }

    #[test]
    fn preserves_comments_and_other_config_sections() {
        let schema = r#"# top comment
[meta]
version = "1.0.0" # trailing
description = "custom"

[node_types.epic]
description = "Epic"
directory = "epics"
allowed_edges = [
  # hierarchy
  { type = "parent_of", target = "feature" },
]

[node_types.risk]
description = "Risk"
directory = "risks"
allowed_edges = [{ type = "relates_to", target = "risk" }]

[node_types.feature]
description = "Feature"
directory = "features"

[edge_types]
parent_of = { reverse = "child_of" }
child_of = { reverse = "parent_of" }
relates_to = { reverse = "relates_to" }
"#;
        let config = "# my config\n[journal]\nenabled = false # keep off\n";
        let plan = plan_upgrade(schema, config, SCHEMA_DELTAS).unwrap();

        assert!(plan.schema_text.starts_with("# top comment\n"));
        assert!(plan.schema_text.contains("version = \"1.0.0\" # trailing"));
        assert!(plan.schema_text.contains(
            "  # hierarchy\n  { type = \"parent_of\", target = \"feature\" },\n  { type = \"has_risk\", target = \"risk\" },\n"
        ));
        assert!(
            plan.schema_text
                .contains("allowed_edges = [{ type = \"relates_to\", target = \"risk\" }, { type = \"risk_for\", target = \"epic\" }]")
        );
        // Reverse pairs were missing and got added.
        assert!(
            plan.schema_text
                .contains("has_risk = { reverse = \"risk_for\" }")
        );
        assert!(
            plan.config_text
                .starts_with("# my config\n[journal]\nenabled = false # keep off\n")
        );

        // Edges with missing endpoint types are reported, not added.
        let report = &plan.deltas[0];
        assert!(report.skipped.iter().any(|s| {
            s.edge.node_type == "decision"
                && s.reason
                    .contains("node type `decision` is not in this schema")
        }));
        assert!(
            report
                .skipped
                .iter()
                .any(|s| s.edge.node_type == "open_question")
        );
        assert!(
            report
                .skipped
                .iter()
                .any(|s| s.edge.target == "open_question")
        );

        // The rewritten config still parses for the other loaders.
        let journal =
            tempyr_journal::config::JournalConfig::from_toml_str(&plan.config_text).unwrap();
        assert!(!journal.enabled);
        plan.schema_text.parse::<Schema>().unwrap();
    }

    #[test]
    fn appends_to_array_of_tables_layout() {
        let schema = r#"[meta]
version = "1"
description = "aot"

[node_types.epic]
description = "Epic"
directory = "epics"

[[node_types.epic.allowed_edges]]
type = "parent_of"
target = "epic"

[node_types.risk]
description = "Risk"
directory = "risks"

[edge_types]
parent_of = { reverse = "child_of" }
child_of = { reverse = "parent_of" }
"#;
        let plan = plan_upgrade(schema, "", SCHEMA_DELTAS).unwrap();
        let parsed: Schema = plan.schema_text.parse().unwrap();
        parsed.validate_edge("epic", "has_risk", "risk").unwrap();
        parsed.validate_edge("risk", "risk_for", "epic").unwrap();
        assert_eq!(parsed.node_types["epic"].allowed_edges.len(), 2);
    }

    /// Upgrade a minimal schema whose epic has `allowed_edges = <array>` and
    /// return the rewritten array text.
    fn upgraded_epic_array(array: &str) -> String {
        let schema = format!(
            r#"[meta]
version = "1"
description = "arrays"

[node_types.epic]
description = "Epic"
directory = "epics"
allowed_edges = {array}

[node_types.risk]
description = "Risk"
directory = "risks"

[edge_types]
parent_of = {{ reverse = "child_of" }}
child_of = {{ reverse = "parent_of" }}
"#
        );
        let plan = plan_upgrade(&schema, "", SCHEMA_DELTAS).unwrap();
        let parsed: Schema = plan.schema_text.parse().unwrap();
        parsed.validate_edge("epic", "has_risk", "risk").unwrap();
        let start = plan.schema_text.find("allowed_edges = ").unwrap();
        let end = start + plan.schema_text[start..].find("\n\n").unwrap();
        plan.schema_text[start..end].to_string()
    }

    const PARENT: &str = r#"{ type = "parent_of", target = "epic" }"#;
    const CHILD: &str = r#"{ type = "child_of", target = "epic" }"#;
    const RISK: &str = r#"{ type = "has_risk", target = "risk" }"#;

    #[test]
    fn appends_to_multiline_array_without_trailing_comma() {
        let out = upgraded_epic_array(&format!("[\n  {PARENT},\n  {CHILD}\n]"));
        assert_eq!(
            out,
            format!("allowed_edges = [\n  {PARENT},\n  {CHILD},\n  {RISK},\n]")
        );
    }

    #[test]
    fn appends_to_multiline_array_with_trailing_comma() {
        let out = upgraded_epic_array(&format!("[\n  {PARENT},\n  {CHILD},\n]"));
        assert_eq!(
            out,
            format!("allowed_edges = [\n  {PARENT},\n  {CHILD},\n  {RISK},\n]")
        );
    }

    #[test]
    fn same_line_comment_stays_with_last_element() {
        let expected = format!("allowed_edges = [\n  {PARENT},\n  {CHILD}, # c2\n  {RISK},\n]");
        let with_comma = upgraded_epic_array(&format!("[\n  {PARENT},\n  {CHILD}, # c2\n]"));
        assert_eq!(with_comma, expected);
        let without_comma = upgraded_epic_array(&format!("[\n  {PARENT},\n  {CHILD} # c2\n]"));
        assert_eq!(without_comma, expected);
    }

    #[test]
    fn appends_to_single_line_and_empty_arrays() {
        assert_eq!(
            upgraded_epic_array(&format!("[ {PARENT} ]")),
            format!("allowed_edges = [ {PARENT}, {RISK} ]")
        );
        // Comment lines inside an empty array survive.
        assert_eq!(
            upgraded_epic_array("[\n  # none yet\n]"),
            format!("allowed_edges = [\n  # none yet\n  {RISK},\n]")
        );
    }

    fn pending_plan() -> UpgradePlan {
        plan_upgrade(&legacy_schema(), "", SCHEMA_DELTAS).unwrap()
    }

    #[test]
    fn applied_message_lists_only_written_files() {
        let plan = pending_plan();
        let all = render_text(
            &plan,
            &[".tempyr/spar/"],
            Some(&[".tempyr/schema.toml", ".tempyr/config.toml", ".gitignore"]),
        );
        assert!(all.ends_with(
            "Applied. Updated .tempyr/schema.toml, .tempyr/config.toml, and .gitignore.\n"
        ));

        let config_only = render_text(&plan, &[], Some(&[".tempyr/config.toml"]));
        assert!(config_only.ends_with("Applied. Updated .tempyr/config.toml.\n"));

        let two = render_text(
            &plan,
            &[],
            Some(&[".tempyr/schema.toml", ".tempyr/config.toml"]),
        );
        assert!(two.ends_with("Applied. Updated .tempyr/schema.toml and .tempyr/config.toml.\n"));

        let dry = render_text(&plan, &[], None);
        assert!(dry.ends_with("Re-run with `tempyr schema upgrade --apply` to apply.\n"));
    }

    #[test]
    fn init_block_records_every_delta() {
        let block = render_applied_deltas_block();
        let doc: DocumentMut = block.parse().unwrap();
        let applied = applied_deltas(&doc).unwrap();
        assert_eq!(applied.len(), SCHEMA_DELTAS.len());
        let plan = plan_upgrade(&legacy_schema(), &block, SCHEMA_DELTAS).unwrap();
        assert!(!plan.schema_changed && !plan.config_changed);
    }
}
