//! `tempyr journal` subcommands.

use std::str::FromStr;

use anyhow::{Context, Result, anyhow};
use clap::Args;
use tempyr_journal::{
    Confidence, Entry, Kind, Polarity, Session, Severity, append, default_redactor, path as jpath,
};

#[derive(Args, Debug)]
pub struct LogArgs {
    /// One of: plan | finding | decision | dead_end | assumption | question | risk | outcome
    pub kind: String,
    /// Short summary (20-200 chars).
    pub summary: String,
    /// Longer body. Required for decision and dead_end (50+ chars).
    #[arg(long, short = 'd')]
    pub detail: Option<String>,
    /// Tag (repeatable). The `tool` tag is conventional for tool quirks.
    #[arg(long = "tag")]
    pub tags: Vec<String>,
    /// File path relevant to this entry (repeatable).
    #[arg(long = "file")]
    pub files: Vec<String>,
    /// Graph node ID this entry references (repeatable).
    #[arg(long = "ref")]
    pub references: Vec<String>,
    /// Agent name. Defaults to "claude".
    #[arg(long, default_value = "claude")]
    pub agent: String,
    /// Mark this entry as provisional (filterable at search time).
    #[arg(long)]
    pub provisional: bool,
    /// low | medium | high
    #[arg(long)]
    pub confidence: Option<String>,
    /// info | warn | high | blocker
    #[arg(long)]
    pub severity: Option<String>,

    /// `decision`: alternative considered (repeatable).
    #[arg(long = "alternative")]
    pub alternatives: Vec<String>,
    /// `decision`: which alternative was chosen.
    #[arg(long)]
    pub chosen: Option<String>,
    /// `decision`: rationale for the choice.
    #[arg(long)]
    pub rationale: Option<String>,
    /// `decision`: is this reversible? Pass `--reversible true` or `--reversible false`.
    #[arg(long)]
    pub reversible: Option<bool>,

    /// `dead_end`: the approach that was tried.
    #[arg(long)]
    pub approach: Option<String>,
    /// `dead_end`: how/why it failed.
    #[arg(long)]
    pub failure_mode: Option<String>,
    /// `dead_end`: a suggested next direction.
    #[arg(long)]
    pub next_to_try: Option<String>,

    /// `assumption`: positive | negative | unknown
    #[arg(long)]
    pub polarity: Option<String>,

    /// `outcome`: did the work succeed?
    #[arg(long)]
    pub passed: Option<bool>,
    /// `outcome`: did the build pass?
    #[arg(long)]
    pub build_ok: Option<bool>,
    /// `outcome`: commit SHA.
    #[arg(long)]
    pub commit_sha: Option<String>,
    /// `outcome`: marks the session-final entry. Triggers publish.
    #[arg(long = "final")]
    pub is_final: bool,
}

pub fn run_log(args: LogArgs, json_output: bool) -> Result<()> {
    let kind = Kind::parse_helpful(&args.kind).map_err(|e| anyhow!(format!("{e}")))?;

    let cwd = std::env::current_dir().context("read current directory")?;
    let common_dir =
        jpath::git_common_dir(&cwd).map_err(|e| anyhow!("not in a git repository: {e}"))?;
    let worktree_top =
        jpath::repo_toplevel(&cwd).map_err(|e| anyhow!("could not resolve repo top-level: {e}"))?;

    let session = Session::open(&common_dir, &worktree_top, &args.agent)
        .map_err(|e| anyhow!("open session: {e}"))?;

    let cwd_rel = if cwd == worktree_top {
        None
    } else {
        cwd.strip_prefix(&worktree_top)
            .ok()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
    };

    let mut entry = Entry::for_session(kind, args.summary, &session);
    entry.detail = args.detail;
    entry.tags = args.tags;
    entry.files = args.files;
    entry.references = args.references;
    entry.cwd = cwd_rel;
    entry.provisional = args.provisional;
    entry.confidence = parse_opt::<Confidence>(args.confidence.as_deref())?;
    entry.severity = parse_opt::<Severity>(args.severity.as_deref())?;
    entry.alternatives = args.alternatives;
    entry.chosen = args.chosen;
    entry.rationale = args.rationale;
    entry.reversible = args.reversible;
    entry.approach = args.approach;
    entry.failure_mode = args.failure_mode;
    entry.next_to_try = args.next_to_try;
    entry.polarity = parse_opt::<Polarity>(args.polarity.as_deref())?;
    entry.passed = args.passed;
    entry.build_ok = args.build_ok;
    entry.commit_sha = args.commit_sha;
    entry.is_final = args.is_final;

    default_redactor()
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
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );
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

fn parse_opt<T: FromStr<Err = tempyr_journal::JournalError>>(s: Option<&str>) -> Result<Option<T>> {
    s.map(T::from_str)
        .transpose()
        .map_err(|e| anyhow!(format!("{e}")))
}
