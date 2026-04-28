//! `tempyr journal` subcommands.

use std::str::FromStr;

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use clap::Args;
use tempyr_journal::{
    Confidence, EntryDraft, Kind, Polarity, PublishOptions, PublisherLock, PublisherState, Session,
    SessionStatus, Severity, git as jgit, path as jpath, publish_ready_sessions, write_entry,
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

    // Reuse the active session for this (worktree, agent) if one exists,
    // so back-to-back `tempyr journal log` calls land in the same session
    // instead of spawning a new one per invocation.
    let session = Session::open_or_resume(&common_dir, &worktree_top, &args.agent)
        .map_err(|e| anyhow!("open session: {e}"))?;

    let draft = EntryDraft {
        kind,
        summary: args.summary,
        detail: args.detail,
        tags: args.tags,
        files: args.files,
        references: args.references,
        cwd: Some(cwd),
        provisional: args.provisional,
        confidence: parse_opt::<Confidence>(args.confidence.as_deref())?,
        severity: parse_opt::<Severity>(args.severity.as_deref())?,
        alternatives: args.alternatives,
        chosen: args.chosen,
        rationale: args.rationale,
        reversible: args.reversible,
        approach: args.approach,
        failure_mode: args.failure_mode,
        next_to_try: args.next_to_try,
        polarity: parse_opt::<Polarity>(args.polarity.as_deref())?,
        passed: args.passed,
        build_ok: args.build_ok,
        commit_sha: args.commit_sha,
        is_final: args.is_final,
    };

    let outcome = write_entry(&session, &worktree_top, draft)
        .map_err(|e| anyhow!(format!("write entry: {e}")))?;
    let entry = &outcome.entry;

    if json_output {
        let payload = serde_json::json!({
            "id": entry.id,
            "session_id": session.id().as_str(),
            "kind": entry.kind.as_str(),
            "jsonl_path": session.jsonl_path().to_string_lossy(),
            "finalized": outcome.finalized,
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
        if outcome.finalized {
            println!("Session finalized: {}", session.id());
        }
    }

    Ok(())
}

fn parse_opt<T: FromStr<Err = tempyr_journal::JournalError>>(s: Option<&str>) -> Result<Option<T>> {
    s.map(T::from_str)
        .transpose()
        .map_err(|e| anyhow!(format!("{e}")))
}

#[derive(Args, Debug)]
pub struct FlushArgs {
    /// Plan only; don't create refs, don't push, don't delete files.
    #[arg(long)]
    pub dry_run: bool,
    /// Skip `git push`. The ref is created locally and the open files are
    /// removed; the user is responsible for pushing later.
    #[arg(long = "no-push")]
    pub no_push: bool,
    /// Remote to push to. Defaults to `origin`.
    #[arg(long, default_value = "origin")]
    pub remote: String,
}

pub fn run_flush(args: FlushArgs, json_output: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("read current directory")?;
    let common_dir =
        jpath::git_common_dir(&cwd).map_err(|e| anyhow!("not in a git repository: {e}"))?;
    let repo_root =
        jpath::repo_toplevel(&cwd).map_err(|e| anyhow!("could not resolve repo top-level: {e}"))?;

    let opts = PublishOptions {
        dry_run: args.dry_run,
        push: !args.no_push,
        remote: args.remote,
    };

    let outcome = publish_ready_sessions(&common_dir, &repo_root, &opts)
        .map_err(|e| anyhow!(format!("publish: {e}")))?;

    let report = match outcome {
        Ok(r) => r,
        Err(_already_running) => {
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "already_running": true,
                        "scanned": 0,
                        "published": 0,
                        "failed": 0,
                    }))
                    .unwrap_or_default()
                );
            } else {
                println!("publisher already running, skipping");
            }
            return Ok(());
        }
    };

    let any_failed = report.failed_count() > 0;

    if json_output {
        let results: Vec<_> = report
            .results
            .iter()
            .map(|(id, status)| match status {
                SessionStatus::Published { refname, pushed } => serde_json::json!({
                    "session_id": id,
                    "status": "published",
                    "ref": refname,
                    "pushed": pushed,
                }),
                SessionStatus::AlreadyArchived { refname, pushed } => serde_json::json!({
                    "session_id": id,
                    "status": "already_archived",
                    "ref": refname,
                    "pushed": pushed,
                }),
                SessionStatus::DryRun { refname } => serde_json::json!({
                    "session_id": id,
                    "status": "dry_run",
                    "ref": refname,
                }),
                SessionStatus::Failed { error } => serde_json::json!({
                    "session_id": id,
                    "status": "failed",
                    "error": error,
                }),
            })
            .collect();
        let payload = serde_json::json!({
            "scanned": report.scanned,
            "published": report.published_count(),
            "failed": report.failed_count(),
            "results": results,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );
    } else if report.scanned == 0 {
        println!("no ready sessions");
    } else {
        for (id, status) in &report.results {
            match status {
                SessionStatus::Published { refname, pushed } => {
                    let suffix = if *pushed { " (pushed)" } else { "" };
                    println!("published {id} -> {refname}{suffix}");
                }
                SessionStatus::AlreadyArchived { refname, pushed } => {
                    let suffix = if *pushed { " (pushed)" } else { "" };
                    println!("already archived {id} -> {refname}{suffix}");
                }
                SessionStatus::DryRun { refname } => {
                    println!("[dry-run] would publish {id} -> {refname}");
                }
                SessionStatus::Failed { error } => {
                    println!("FAILED {id}: {error}");
                }
            }
        }
        println!(
            "{} scanned, {} published, {} failed",
            report.scanned,
            report.published_count(),
            report.failed_count()
        );
    }

    if any_failed {
        // Exit non-zero so CI / hooks can detect partial failures.
        std::process::exit(1);
    }
    Ok(())
}

#[derive(Args, Debug, Default)]
pub struct StatusArgs {}

pub fn run_status(_args: StatusArgs, json_output: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("read current directory")?;
    let common_dir =
        jpath::git_common_dir(&cwd).map_err(|e| anyhow!("not in a git repository: {e}"))?;
    let state = PublisherState::load(&common_dir).map_err(|e| anyhow!("load state.json: {e}"))?;

    let counts = scan_open_dir(&common_dir)?;
    let publisher_running = PublisherLock::is_held(&common_dir);
    let stamped_pid = if publisher_running {
        PublisherLock::stamped_pid(&common_dir)
    } else {
        None
    };

    if json_output {
        let payload = serde_json::json!({
            "common_dir": common_dir.to_string_lossy(),
            "open_sessions": counts.open,
            "ready_sessions": counts.ready,
            "publisher_running": publisher_running,
            "publisher_pid": stamped_pid,
            "last_push_ok_utc": state.last_push_ok_utc,
            "last_error": state.last_error.as_ref().map(|e| serde_json::json!({
                "ts_utc": e.ts_utc,
                "op": e.op,
                "message": e.message,
            })),
            "commits_total": state.commits_total,
            "pushes_total": state.pushes_total,
            "push_failures_total": state.push_failures_total,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );
    } else {
        println!("journal:    {}", common_dir.display());
        println!(
            "open:       {} session{} ({} ready to publish)",
            counts.open,
            if counts.open == 1 { "" } else { "s" },
            counts.ready
        );
        match (publisher_running, stamped_pid) {
            (true, Some(pid)) => println!("publisher:  running (pid {pid})"),
            (true, None) => println!("publisher:  running"),
            (false, _) => println!("publisher:  idle"),
        }
        println!(
            "last push:  {}",
            state
                .last_push_ok_utc
                .map(format_age)
                .unwrap_or_else(|| "never".to_string())
        );
        if let Some(e) = &state.last_error {
            println!(
                "last error: [{}] {} ({})",
                e.op,
                e.message,
                format_age(e.ts_utc)
            );
        } else {
            println!("last error: none");
        }
        println!(
            "totals:     {} commit{}, {} push{}, {} push failure{}",
            state.commits_total,
            if state.commits_total == 1 { "" } else { "s" },
            state.pushes_total,
            if state.pushes_total == 1 { "" } else { "es" },
            state.push_failures_total,
            if state.push_failures_total == 1 {
                ""
            } else {
                "s"
            },
        );
    }
    Ok(())
}

struct OpenCounts {
    open: usize,
    ready: usize,
}

fn scan_open_dir(common_dir: &std::path::Path) -> Result<OpenCounts> {
    let open = jpath::open_dir(common_dir);
    let read_dir = match std::fs::read_dir(&open) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(OpenCounts { open: 0, ready: 0 });
        }
        Err(e) => return Err(anyhow!("read open dir: {e}")),
    };
    let mut sessions = 0usize;
    let mut ready = 0usize;
    for entry in read_dir {
        let entry = entry.map_err(|e| anyhow!("read open entry: {e}"))?;
        let name = entry.file_name();
        let s = name.to_string_lossy();
        if s.ends_with(".meta.json") {
            sessions += 1;
        } else if s.ends_with(".ready") {
            ready += 1;
        }
    }
    Ok(OpenCounts {
        open: sessions,
        ready,
    })
}

fn format_age(ts: DateTime<Utc>) -> String {
    let now = Utc::now();
    let delta = now.signed_duration_since(ts);
    let secs = delta.num_seconds();
    let abs_secs = secs.unsigned_abs();
    let suffix = if secs < 0 { "from now" } else { "ago" };
    let pretty = if abs_secs < 60 {
        format!("{abs_secs}s")
    } else if abs_secs < 3600 {
        format!("{}m", abs_secs / 60)
    } else if abs_secs < 86_400 {
        format!("{}h", abs_secs / 3600)
    } else {
        format!("{}d", abs_secs / 86_400)
    };
    format!("{} ({pretty} {suffix})", ts.to_rfc3339())
}

#[derive(Args, Debug)]
pub struct LogsArgs {
    /// Number of recent log lines to show. Defaults to 50.
    #[arg(long, default_value = "50")]
    pub lines: usize,
}

pub fn run_logs(args: LogsArgs, json_output: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("read current directory")?;
    let common_dir =
        jpath::git_common_dir(&cwd).map_err(|e| anyhow!("not in a git repository: {e}"))?;
    let log_path = jpath::publisher_log_path(&common_dir);
    let text = match std::fs::read_to_string(&log_path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(anyhow!("read publisher.log: {e}")),
    };

    let all_lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    let start = all_lines.len().saturating_sub(args.lines);
    let tail = &all_lines[start..];

    if json_output {
        // Pass through the JSONL — readers can parse line-by-line.
        for line in tail {
            println!("{line}");
        }
        return Ok(());
    }

    if tail.is_empty() {
        println!("no publisher events");
        return Ok(());
    }

    for line in tail {
        // Try to parse as a structured LogLine; if the file is corrupted
        // mid-rotation, just print the raw line as-is.
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(line) {
            let ts = parsed.get("ts").and_then(|v| v.as_str()).unwrap_or("?");
            let level = parsed
                .get("level")
                .and_then(|v| v.as_str())
                .unwrap_or("info")
                .to_uppercase();
            let event = parsed.get("event").and_then(|v| v.as_str()).unwrap_or("?");
            let fields = parsed
                .get("fields")
                .filter(|v| v.as_object().is_some_and(|m| !m.is_empty()))
                .map(|v| format!(" {v}"))
                .unwrap_or_default();
            println!("{ts}  {level:5}  {event}{fields}");
        } else {
            println!("{line}");
        }
    }
    Ok(())
}

#[derive(Args, Debug)]
pub struct FetchArgs {
    /// Remote to fetch journal refs from. Defaults to `origin`.
    #[arg(long, default_value = "origin")]
    pub remote: String,
}

pub fn run_fetch(args: FetchArgs, json_output: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("read current directory")?;
    let repo_root =
        jpath::repo_toplevel(&cwd).map_err(|e| anyhow!("could not resolve repo top-level: {e}"))?;

    // `+refs/tempyr/journals/*:refs/tempyr/journals/*` mirrors all journal
    // refs from the remote into the local repo. The leading `+` allows
    // forced updates so a republished session (rare but possible) doesn't
    // wedge the fetch.
    let refspec = "+refs/tempyr/journals/*:refs/tempyr/journals/*";
    let result = jgit::run(
        &repo_root,
        &["fetch", "--quiet", &args.remote, refspec],
        None,
        jgit::DEFAULT_TIMEOUT,
    )
    .map_err(|e| anyhow!("git fetch: {e}"))?
    .ok_or_err("fetch")
    .map_err(|e| anyhow!(format!("{e}")))?;

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "remote": args.remote,
                "refspec": refspec,
                "ok": true,
                "stderr": result.stderr.trim(),
            }))
            .unwrap_or_default()
        );
    } else {
        let trimmed = result.stderr.trim();
        if trimmed.is_empty() {
            println!("fetched journal refs from {}", args.remote);
        } else {
            println!("{trimmed}");
        }
    }
    Ok(())
}
