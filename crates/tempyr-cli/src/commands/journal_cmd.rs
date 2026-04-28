//! `tempyr journal` subcommands.
//!
//! Phase 1 ships only `log` (capture). The Phase 2 commands (`flush`,
//! `status`, `doctor`, `logs`, `fetch`, `bootstrap`, `finalize`) and Phase 3
//! commands (`search`, `show`, `sessions`, `tail`, `index`) live in stub
//! modules that return a "not implemented yet" error so the CLI surface is
//! discoverable from day one.

use anyhow::{Context, Result, anyhow, bail};
use tempyr_journal::{
    Confidence, Entry, Kind, Polarity, Redactor, Session, Severity, append, path as jpath,
};

#[allow(clippy::too_many_arguments)]
pub fn run_log(
    kind: &str,
    summary: String,
    detail: Option<String>,
    tags: Vec<String>,
    files: Vec<String>,
    references: Vec<String>,
    agent: String,
    provisional: bool,
    confidence: Option<String>,
    severity: Option<String>,
    alternatives: Vec<String>,
    chosen: Option<String>,
    rationale: Option<String>,
    reversible: Option<bool>,
    approach: Option<String>,
    failure_mode: Option<String>,
    next_to_try: Option<String>,
    polarity: Option<String>,
    passed: Option<bool>,
    build_ok: Option<bool>,
    commit_sha: Option<String>,
    is_final: bool,
    json_output: bool,
) -> Result<()> {
    let parsed_kind = Kind::parse_helpful(kind)
        .map_err(|e| anyhow!(format!("{e}")))?;

    let cwd = std::env::current_dir().context("read current directory")?;
    let common_dir = jpath::git_common_dir(&cwd)
        .map_err(|e| anyhow!("not in a git repository: {e}"))?;
    let worktree_top = jpath::repo_toplevel(&cwd)
        .map_err(|e| anyhow!("could not resolve repo top-level: {e}"))?;

    let session = Session::open(&common_dir, &worktree_top, &agent)
        .map_err(|e| anyhow!("open session: {e}"))?;

    let cwd_rel = if cwd == worktree_top {
        None
    } else {
        cwd.strip_prefix(&worktree_top)
            .ok()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
    };

    let mut entry = Entry {
        schema_version: tempyr_journal::SCHEMA_VERSION,
        id: Entry::new_id(),
        ts: chrono::Utc::now(),
        agent,
        kind: parsed_kind,
        summary,
        detail,
        tags,
        files,
        references,
        session_id: session.id().as_str().to_string(),
        worktree_hash: session.meta().worktree_hash.clone(),
        branch: session.meta().branch.clone(),
        head: session.meta().head.clone(),
        cwd: cwd_rel,
        provisional,
        confidence: parse_confidence(confidence.as_deref())?,
        severity: parse_severity(severity.as_deref())?,
        alternatives,
        chosen,
        rationale,
        reversible,
        approach,
        failure_mode,
        next_to_try,
        polarity: parse_polarity(polarity.as_deref())?,
        passed,
        tests: None,
        build_ok,
        commit_sha,
        is_final,
    };

    Redactor::default()
        .enforce(&mut entry)
        .map_err(|e| anyhow!(format!("{e}")))?;
    append(&session, &entry).map_err(|e| anyhow!("append: {e}"))?;

    if json_output {
        let payload = serde_json::json!({
            "id": entry.id,
            "session_id": session.id().as_str(),
            "kind": entry.kind.as_str(),
            "jsonl_path": session.jsonl_path().to_string_lossy(),
        });
        println!("{}", serde_json::to_string_pretty(&payload).unwrap_or_default());
    } else {
        println!(
            "Logged {} {} -> {}",
            entry.kind.as_str(),
            entry.id,
            session.jsonl_path().display()
        );
    }

    Ok(())
}

fn parse_confidence(s: Option<&str>) -> Result<Option<Confidence>> {
    match s {
        None => Ok(None),
        Some(v) => match v.trim().to_ascii_lowercase().as_str() {
            "low" => Ok(Some(Confidence::Low)),
            "medium" | "med" => Ok(Some(Confidence::Medium)),
            "high" => Ok(Some(Confidence::High)),
            other => bail!("invalid confidence {other:?}: expected low | medium | high"),
        },
    }
}

fn parse_severity(s: Option<&str>) -> Result<Option<Severity>> {
    match s {
        None => Ok(None),
        Some(v) => match v.trim().to_ascii_lowercase().as_str() {
            "info" => Ok(Some(Severity::Info)),
            "warn" | "warning" => Ok(Some(Severity::Warn)),
            "high" => Ok(Some(Severity::High)),
            "blocker" => Ok(Some(Severity::Blocker)),
            other => bail!("invalid severity {other:?}: expected info | warn | high | blocker"),
        },
    }
}

fn parse_polarity(s: Option<&str>) -> Result<Option<Polarity>> {
    match s {
        None => Ok(None),
        Some(v) => match v.trim().to_ascii_lowercase().as_str() {
            "positive" | "pos" => Ok(Some(Polarity::Positive)),
            "negative" | "neg" => Ok(Some(Polarity::Negative)),
            "unknown" => Ok(Some(Polarity::Unknown)),
            other => bail!("invalid polarity {other:?}: expected positive | negative | unknown"),
        },
    }
}
