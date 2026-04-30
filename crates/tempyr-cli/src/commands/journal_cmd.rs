//! `tempyr journal` subcommands.

use std::str::FromStr;

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use clap::Args;
use tempyr_journal::{
    Confidence, EntryDraft, JournalConfig, Kind, Polarity, PublishOptions, PublisherLock,
    PublisherState, Session, SessionStatus, Severity, git as jgit, path as jpath,
    publish_ready_sessions, write_entry,
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

/// Translate the `--kind` flag's repeatable string values (used by
/// `journal search`, `journal range`, and `journal blame`) into the
/// typed `Vec<Kind>` the index layer expects. Empty input yields an
/// empty vec; any unparseable string surfaces as an `anyhow` error
/// the caller can `?`-propagate.
fn parse_kinds(strs: &[String]) -> Result<Vec<Kind>> {
    let mut out = Vec::with_capacity(strs.len());
    for s in strs {
        out.push(Kind::parse_helpful(s).map_err(|e| anyhow!(format!("{e}")))?);
    }
    Ok(out)
}

// `refresh_index_preferring_embeddings` lives in `tempyr-journal-index`
// (re-exported from the crate root) so the CLI and the MCP server
// share a single fallback policy: try the embedder path, log + retry
// with structural-only on embedder error, surface only hard failures.
use tempyr_journal_index::refresh_index_preferring_embeddings;

#[derive(Args, Debug)]
pub struct FlushArgs {
    /// Plan only; don't create refs, don't push, don't delete files.
    #[arg(long)]
    pub dry_run: bool,
    /// Skip `git push`. The ref is created locally and the open files are
    /// removed; the user is responsible for pushing later.
    #[arg(long = "no-push")]
    pub no_push: bool,
    /// Override the remote name. Defaults to the `[journal] remote` from
    /// `.tempyr/config.toml`, or `origin` if no config is found.
    #[arg(long)]
    pub remote: Option<String>,
}

pub fn run_flush(args: FlushArgs, json_output: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("read current directory")?;
    let common_dir =
        jpath::git_common_dir(&cwd).map_err(|e| anyhow!("not in a git repository: {e}"))?;
    let repo_root =
        jpath::repo_toplevel(&cwd).map_err(|e| anyhow!("could not resolve repo top-level: {e}"))?;

    // Resolve the publish options from config (if a `.tempyr/` is found
    // by walking up from cwd) and then apply CLI overrides on top. The
    // CLI flag wins when both are set; otherwise the config value flows
    // through (remote, push timeout, pack-refs threshold).
    let config = tempyr_core::project::find_project_root()
        .map(|root| JournalConfig::load(&root.join(".tempyr")))
        .transpose()
        .map_err(|e| anyhow!("load journal config: {e}"))?
        .unwrap_or_default();

    let mut opts = PublishOptions::from_config(&config);
    opts.dry_run = args.dry_run;
    opts.push = !args.no_push;
    if let Some(remote) = args.remote {
        opts.remote = remote;
    }

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

    // Best-effort index refresh after a successful publish. The index
    // is a derived cache — failure here must NOT fail the flush (the
    // commit + push already succeeded; the user has their data on the
    // remote). The user can always rebuild via `tempyr journal index
    // --rebuild`. Errors get a single warning line to stderr.
    if !args.dry_run
        && report.published_count() > 0
        && let Err(e) = refresh_index_preferring_embeddings(&common_dir, &repo_root)
    {
        // Refresh is best-effort post-flush — the publish already
        // succeeded. The helper handles the embedder-failure
        // fallback internally; only a hard structural-refresh
        // failure reaches us here.
        eprintln!("warning: post-flush index refresh failed: {e}");
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

#[derive(Args, Debug)]
pub struct IndexArgs {
    /// Truncate the index and re-ingest from scratch. Multi-session
    /// safe: uses `DELETE FROM` inside a transaction. For a corrupt
    /// db that won't open at all, add `--force`.
    #[arg(long)]
    pub rebuild: bool,
    /// Only valid with `--rebuild`. Removes the index file outright
    /// rather than truncating tables. Use only for corrupt-db
    /// recovery; quiesce other readers first.
    #[arg(long, requires = "rebuild")]
    pub force: bool,
}

pub fn run_index(args: IndexArgs, json_output: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("read current directory")?;
    let common_dir =
        jpath::git_common_dir(&cwd).map_err(|e| anyhow!("not in a git repository: {e}"))?;
    let repo_root =
        jpath::repo_toplevel(&cwd).map_err(|e| anyhow!("could not resolve repo top-level: {e}"))?;

    if args.rebuild {
        let db_path = tempyr_journal_index::index_db_path(&common_dir);
        if args.force {
            // Corrupt-db escape hatch. NotFound is fine — first-run
            // case shouldn't error.
            match std::fs::remove_file(&db_path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(anyhow!("remove index.db: {e}")),
            }
        } else {
            let mut conn = tempyr_journal_index::schema::open(&db_path)
                .map_err(|e| anyhow!("open index.db: {e}"))?;
            tempyr_journal_index::schema::truncate(&mut conn)
                .map_err(|e| anyhow!("truncate: {e}"))?;
        }
    }

    // Try the embedder path first (vector search is ready
    // immediately); fall back to structural-only refresh on
    // embedder load failure or per-call embed failure (the helper
    // handles both internally with a single warning line).
    let report = refresh_index_preferring_embeddings(&common_dir, &repo_root)
        .map_err(|e| anyhow!("refresh index: {e}"))?;

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "scanned": report.scanned,
                "inserted": report.inserted,
                "already_indexed": report.already_indexed,
                "corrupt_lines": report.corrupt_lines,
                "embedded": report.embedded,
                "embed_pending_total": report.embed_pending_total,
                "open_files": report.open_files,
                "archive_refs": report.archive_refs,
                "rebuilt": args.rebuild,
            }))
            .unwrap_or_default()
        );
    } else {
        if args.rebuild {
            println!(
                "rebuilt index (mode: {})",
                if args.force {
                    "force-delete"
                } else {
                    "truncate"
                }
            );
        }
        println!(
            "scanned {} line{} across {} open file{} + {} archive ref{} \u{2192} \
             {} new, {} already indexed, {} corrupt",
            report.scanned,
            plural(report.scanned),
            report.open_files,
            plural(report.open_files),
            report.archive_refs,
            plural(report.archive_refs),
            report.inserted,
            report.already_indexed,
            report.corrupt_lines,
        );
        if report.embedded > 0 || report.embed_pending_total > 0 {
            println!(
                "embedded {} entr{} ({} low-info entr{} in index, not vector-searchable)",
                report.embedded,
                if report.embedded == 1 { "y" } else { "ies" },
                report.embed_pending_total,
                if report.embed_pending_total == 1 {
                    "y"
                } else {
                    "ies"
                },
            );
        }
    }
    Ok(())
}

fn plural(n: u64) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[derive(Args, Debug)]
pub struct SearchArgs {
    /// Full-text query. FTS5 syntax: `"phrase"`, `term1 OR term2`, `prefix*`.
    pub query: String,
    /// Restrict to one or more kinds (repeatable).
    #[arg(long = "kind")]
    pub kinds: Vec<String>,
    /// Cap on returned hits (default 10).
    #[arg(long, default_value = "10")]
    pub limit: usize,
    /// Filter to entries newer than N days.
    #[arg(long)]
    pub since_days: Option<u32>,
    /// Token budget for the response. Detail bodies are truncated to fit.
    #[arg(long)]
    pub token_budget: Option<usize>,
    /// Show per-hit score breakdown (BM25 / recency / kind / total).
    #[arg(long)]
    pub explain: bool,
    /// Run a cross-encoder over the top-50 RRF candidates and re-sort
    /// by relevance. Higher accuracy on close calls (decisions vs.
    /// dead-ends, semantically-related-but-lexically-distant hits)
    /// at the cost of a one-time ~280 MB model download and ~200 ms
    /// of inference per query. Falls back transparently to the
    /// unreranked RRF order if the model fails to load.
    #[arg(long)]
    pub rerank: bool,
}

pub fn run_search(args: SearchArgs, json_output: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("read current directory")?;
    let common_dir =
        jpath::git_common_dir(&cwd).map_err(|e| anyhow!("not in a git repository: {e}"))?;
    let repo_root =
        jpath::repo_toplevel(&cwd).map_err(|e| anyhow!("could not resolve repo top-level: {e}"))?;

    // Refresh first so the search sees anything the user just
    // logged. Use the embedder when available so freshly-indexed
    // entries are immediately searchable via the vector path; the
    // helper handles BM25-only fallback on embedder failure.
    refresh_index_preferring_embeddings(&common_dir, &repo_root)
        .map_err(|e| anyhow!("refresh index: {e}"))?;
    let embedder = tempyr_journal_index::try_shared_embedder();

    let kinds = parse_kinds(&args.kinds)?;

    // Embed the query string if an embedder is loaded. None →
    // BM25-only mode; identical 3b1 behavior preserved. The shared
    // `warn_query_embed_failure_once` helper gates the warning so
    // a hard "embedding always fails" environment doesn't emit one
    // log line per `journal search` invocation.
    let query_vector = match embedder {
        Some(emb) => match emb.embed_one(&args.query) {
            Ok(v) => Some(v),
            Err(e) => {
                tempyr_journal_index::warn_query_embed_failure_once(&e);
                None
            }
        },
        None => None,
    };

    let opts = tempyr_journal_index::SearchOptions {
        query: args.query.clone(),
        query_vector,
        kinds,
        limit: args.limit,
        since_days: args.since_days,
        token_budget: args
            .token_budget
            .unwrap_or(tempyr_journal_index::search::DEFAULT_TOKEN_BUDGET),
        explain: args.explain,
        rerank: args.rerank,
    };

    let db_path = tempyr_journal_index::index_db_path(&common_dir);
    let conn =
        tempyr_journal_index::schema::open(&db_path).map_err(|e| anyhow!("open index: {e}"))?;
    let hits = tempyr_journal_index::search(&conn, &opts).map_err(|e| anyhow!("search: {e}"))?;

    if json_output {
        let payload = serde_json::json!({
            "query": args.query,
            "count": hits.len(),
            "hits": hits,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );
    } else if hits.is_empty() {
        println!("no hits");
    } else {
        for (i, hit) in hits.iter().enumerate() {
            let entry = &hit.entry;
            println!(
                "[{:>2}] {:<10} {} ({})",
                i + 1,
                entry.kind.as_str(),
                entry.id,
                entry.ts.format("%Y-%m-%d")
            );
            println!("     {}", entry.summary);
            if let Some(detail) = &entry.detail
                && !detail.is_empty()
            {
                println!("     {}", detail.lines().next().unwrap_or(detail));
            }
            if args.explain
                && let Some(b) = &hit.explain
            {
                // Vector and rrf are 0 in BM25-only mode; we still
                // print them so the line shape is stable across
                // modes and the agent can tell which mode is active
                // from the values. The `reranked` flag distinguishes
                // "rerank actually ran" from "rerank was requested
                // but the model failed to load and we fell back to
                // RRF" — `args.rerank` alone can't tell those apart.
                if b.reranked {
                    println!(
                        "     score: {:.3} = rerank (bm25={:.3}, vector={:.3}, rrf={:.3}, recency={:.3}, kind={:.3} — informational only)",
                        b.total, b.bm25, b.vector, b.rrf, b.recency, b.kind
                    );
                } else {
                    println!(
                        "     score: {:.3} (bm25={:.3}, vector={:.3}, rrf={:.3}, recency={:.3}, kind={:.3})",
                        b.total, b.bm25, b.vector, b.rrf, b.recency, b.kind
                    );
                }
            }
        }
    }
    Ok(())
}

#[derive(Args, Debug)]
pub struct RangeArgs {
    /// Range expression understood by `git rev-list`: `A..B`,
    /// `HEAD~10..HEAD`, `feature..main`, etc. The `A..B` form is
    /// the same one `git log A..B` accepts.
    pub range: String,
    /// Restrict to one or more kinds (repeatable).
    #[arg(long = "kind")]
    pub kinds: Vec<String>,
    /// Cap on returned hits (default 50).
    #[arg(long, default_value = "50")]
    pub limit: usize,
    /// Token budget for the response. Detail bodies are truncated to fit.
    #[arg(long)]
    pub token_budget: Option<usize>,
    /// Show per-hit score breakdown (recency / kind only — no
    /// query-string signal in range mode).
    #[arg(long)]
    pub explain: bool,
}

pub fn run_range(args: RangeArgs, json_output: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("read current directory")?;
    let common_dir =
        jpath::git_common_dir(&cwd).map_err(|e| anyhow!("not in a git repository: {e}"))?;
    let repo_root =
        jpath::repo_toplevel(&cwd).map_err(|e| anyhow!("could not resolve repo top-level: {e}"))?;

    // Refresh first so the range view sees anything the agent just
    // logged. Use the structural-only path — vector embeddings don't
    // affect range queries (no query-string signal in this mode), so
    // skipping the embedder load keeps the command fast.
    tempyr_journal_index::refresh_index(&common_dir, &repo_root)
        .map_err(|e| anyhow!("refresh index: {e}"))?;

    // Expand the range expression via `git rev-list`. The user's
    // input is forwarded unchanged so anything `git log A..B`
    // accepts also works here. We pin `--no-merges` off (default
    // includes merges) — for journal range we want every commit.
    let commits = git_rev_list(&repo_root, &args.range)?;

    let kinds = parse_kinds(&args.kinds)?;

    let opts = tempyr_journal_index::RangeOptions {
        commits,
        kinds,
        limit: args.limit,
        token_budget: args
            .token_budget
            .unwrap_or(tempyr_journal_index::search::DEFAULT_TOKEN_BUDGET),
        explain: args.explain,
    };

    let db_path = tempyr_journal_index::index_db_path(&common_dir);
    let conn =
        tempyr_journal_index::schema::open(&db_path).map_err(|e| anyhow!("open index: {e}"))?;
    let hits =
        tempyr_journal_index::range_query(&conn, &opts).map_err(|e| anyhow!("range: {e}"))?;

    if json_output {
        let payload = serde_json::json!({
            "range": args.range,
            "count": hits.len(),
            "hits": hits,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );
    } else if hits.is_empty() {
        println!("no entries in range {}", args.range);
    } else {
        for (i, hit) in hits.iter().enumerate() {
            let entry = &hit.entry;
            println!(
                "[{:>2}] {:<10} {} ({})",
                i + 1,
                entry.kind.as_str(),
                entry.id,
                entry.ts.format("%Y-%m-%d")
            );
            println!("     {}", entry.summary);
            if let Some(detail) = &entry.detail
                && !detail.is_empty()
            {
                println!("     {}", detail.lines().next().unwrap_or(detail));
            }
            if args.explain
                && let Some(b) = &hit.explain
            {
                println!(
                    "     score: {:.3} (recency={:.3}, kind={:.3})",
                    b.total, b.recency, b.kind
                );
            }
        }
    }
    Ok(())
}

/// Shell out to `git rev-list <expr>` to expand the user's range
/// expression into a concrete list of commit SHAs. Returns the SHAs
/// in the order git emitted them (newest first by default).
fn git_rev_list(repo_root: &std::path::Path, expr: &str) -> Result<Vec<String>> {
    // Pre-validate the expansion size with `git rev-list --count`.
    // A pathological range (e.g. `--all` on a large repo) would
    // otherwise expand into tens of thousands of SHAs that
    // `range_query` would silently truncate to MAX_RANGE_COMMITS.
    // Better to fail clean here with a message the user can act on.
    let count = git_rev_list_count(repo_root, expr)?;
    if count > tempyr_journal_index::MAX_RANGE_COMMITS {
        return Err(anyhow!(
            "range `{expr}` expands to {count} commits, exceeds limit of {} — narrow the range (e.g. `A..B` instead of `--all`)",
            tempyr_journal_index::MAX_RANGE_COMMITS
        ));
    }
    let out = std::process::Command::new("git")
        .args(["rev-list", expr])
        .current_dir(repo_root)
        .output()
        .map_err(|e| anyhow!("git rev-list: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!("git rev-list {expr} failed: {}", stderr.trim()));
    }
    let stdout = String::from_utf8(out.stdout).context("git rev-list emitted non-UTF8")?;
    Ok(stdout
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect())
}

fn git_rev_list_count(repo_root: &std::path::Path, expr: &str) -> Result<usize> {
    let out = std::process::Command::new("git")
        .args(["rev-list", "--count", expr])
        .current_dir(repo_root)
        .output()
        .map_err(|e| anyhow!("git rev-list --count: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!(
            "git rev-list --count {expr} failed: {}",
            stderr.trim()
        ));
    }
    let stdout = String::from_utf8(out.stdout).context("git rev-list --count emitted non-UTF8")?;
    stdout.trim().parse::<usize>().map_err(|e| {
        anyhow!(
            "git rev-list --count returned non-integer `{}`: {e}",
            stdout.trim()
        )
    })
}

#[derive(Args, Debug)]
pub struct BlameArgs {
    /// File path. Absolute paths under the worktree are normalized
    /// to repo-relative form; cwd-relative paths are joined against
    /// the current directory first. Backslashes are converted to
    /// forward-slash to match the on-disk format the indexer stores.
    pub path: String,
    /// Restrict to one or more kinds (repeatable). Useful filters:
    /// `--kind dead_end --kind decision` for the highest-signal
    /// reasoning about this file.
    #[arg(long = "kind")]
    pub kinds: Vec<String>,
    /// Cap on returned hits (default 50).
    #[arg(long, default_value = "50")]
    pub limit: usize,
    /// Token budget for the response. Detail bodies are truncated to fit.
    #[arg(long)]
    pub token_budget: Option<usize>,
    /// Show per-hit score breakdown (recency / kind only — no
    /// query-string signal in blame mode).
    #[arg(long)]
    pub explain: bool,
}

pub fn run_blame(args: BlameArgs, json_output: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("read current directory")?;
    let common_dir =
        jpath::git_common_dir(&cwd).map_err(|e| anyhow!("not in a git repository: {e}"))?;
    let worktree_top =
        jpath::repo_toplevel(&cwd).map_err(|e| anyhow!("could not resolve repo top-level: {e}"))?;

    // Refresh first so blame sees anything the agent just logged.
    // Structural-only — no query string means no embedding needed.
    tempyr_journal_index::refresh_index(&common_dir, &worktree_top)
        .map_err(|e| anyhow!("refresh index: {e}"))?;

    // Normalize the user's path the same way the writer normalized
    // entry.files at log time (repo-relative, forward-slash). This
    // is the canonical helper both sides share, so pass-by-value
    // accepts absolute / cwd-relative / Windows-style paths
    // equivalently.
    let normalized = jpath::resolve_file_path(&args.path, &worktree_top, Some(&cwd));

    let kinds = parse_kinds(&args.kinds)?;

    let opts = tempyr_journal_index::BlameOptions {
        path: normalized.clone(),
        kinds,
        limit: args.limit,
        token_budget: args
            .token_budget
            .unwrap_or(tempyr_journal_index::search::DEFAULT_TOKEN_BUDGET),
        explain: args.explain,
    };

    let db_path = tempyr_journal_index::index_db_path(&common_dir);
    let conn =
        tempyr_journal_index::schema::open(&db_path).map_err(|e| anyhow!("open index: {e}"))?;
    let hits =
        tempyr_journal_index::blame_query(&conn, &opts).map_err(|e| anyhow!("blame: {e}"))?;

    if json_output {
        let payload = serde_json::json!({
            "path": normalized,
            "count": hits.len(),
            "hits": hits,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );
    } else if hits.is_empty() {
        println!("no entries reference {}", normalized);
    } else {
        for (i, hit) in hits.iter().enumerate() {
            let entry = &hit.entry;
            println!(
                "[{:>2}] {:<10} {} ({})",
                i + 1,
                entry.kind.as_str(),
                entry.id,
                entry.ts.format("%Y-%m-%d")
            );
            println!("     {}", entry.summary);
            if let Some(detail) = &entry.detail
                && !detail.is_empty()
            {
                println!("     {}", detail.lines().next().unwrap_or(detail));
            }
            if args.explain
                && let Some(b) = &hit.explain
            {
                println!(
                    "     score: {:.3} (recency={:.3}, kind={:.3})",
                    b.total, b.recency, b.kind
                );
            }
        }
    }
    Ok(())
}

#[derive(Args, Debug)]
pub struct LintArgs {
    /// Exit non-zero on warnings instead of warn-only. Useful in CI;
    /// the managed pre-commit hook leaves this off so a stale task
    /// never blocks a commit.
    #[arg(long)]
    pub strict: bool,
}

/// Diagnostic returned by [`run_lint`]. Currently a single variant —
/// in-progress task with no journal references — but shaped as an
/// enum so future lints (stale dead-ends, orphaned sessions, etc.)
/// can land without changing the JSON wire format.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum LintWarning {
    InProgressTaskWithoutJournal { task_id: String, title: String },
}

pub fn run_lint(
    ctx: &crate::config::ProjectContext,
    args: LintArgs,
    json_output: bool,
) -> Result<()> {
    // Anchor on `ctx.root` (the resolved project root), NOT
    // `current_dir()`. With `--graph-dir /elsewhere` the user's
    // shell can sit in a different repo entirely, and the lint
    // should follow the project. Same fix the auto-emit slices
    // applied to status_cmd / interview_cmd.
    let common_dir = match jpath::git_common_dir(&ctx.root) {
        Ok(c) => c,
        Err(tempyr_journal::JournalError::NotAGitRepo(_)) => {
            // Outside a git repo: no journal to lint against. Silent
            // skip so the pre-commit hook stays harmless.
            print_lint_result(json_output, &[], true, "not in a git repository");
            return Ok(());
        }
        Err(e) => return Err(anyhow!("git_common_dir: {e}")),
    };
    let repo_root = jpath::repo_toplevel(&ctx.root)
        .map_err(|e| anyhow!("could not resolve repo top-level: {e}"))?;

    // Refresh structural-only so the lint sees anything the agent
    // just logged. No query string => no embedder needed.
    tempyr_journal_index::refresh_index(&common_dir, &repo_root)
        .map_err(|e| anyhow!("refresh index: {e}"))?;

    // Two-phase lookup so we can distinguish "graph is mid-edit, skip
    // the lint and let the commit through" from "real bug — propagate".
    let in_progress = match load_in_progress_tasks(ctx) {
        Ok(tasks) => tasks,
        Err(e) => {
            // Graph-load failure is tolerated: a half-broken graph
            // shouldn't block a commit. Mark skipped and continue.
            print_lint_result(json_output, &[], true, &format!("graph load failed: {e}"));
            return Ok(());
        }
    };

    // Index access errors past this point ARE real bugs (corrupt db,
    // permission failure, etc.) and should propagate so `--strict`
    // and JSON consumers see the failure rather than a misleading
    // empty `warnings` array.
    let warnings = count_journal_refs(&common_dir, &in_progress)?;

    print_lint_result(json_output, &warnings, false, "");

    if args.strict && !warnings.is_empty() {
        std::process::exit(1);
    }
    Ok(())
}

fn print_lint_result(
    json_output: bool,
    warnings: &[LintWarning],
    skipped: bool,
    skip_reason: &str,
) {
    if json_output {
        let mut payload = serde_json::json!({
            "warnings": warnings,
            "skipped": skipped,
        });
        if skipped {
            payload["reason"] = serde_json::Value::String(skip_reason.to_string());
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );
    } else if warnings.is_empty() {
        // Silent on success — pre-commit hooks should be quiet on
        // the happy path so they don't add noise to the user's
        // commit experience.
    } else {
        eprintln!(
            "tempyr journal lint: {} warning{}",
            warnings.len(),
            if warnings.len() == 1 { "" } else { "s" }
        );
        for w in warnings {
            match w {
                LintWarning::InProgressTaskWithoutJournal { task_id, title } => {
                    eprintln!("  task {task_id} ({title}): in_progress but has no journal entries");
                    // The auto-emit only fires on a real status
                    // *transition*; suggesting `tempyr status
                    // <id> in_progress` here would be a no-op since
                    // the task is already in_progress. Steer users
                    // toward an action that actually adds journal
                    // coverage.
                    eprintln!(
                        "    fix: `tempyr journal log finding \"...\" --ref {task_id}` to add context, or transition the task through `tempyr status` from a different starting status"
                    );
                }
            }
        }
    }
}

/// Phase 1 of the lint: load the graph and pick out tasks that are
/// currently `in_progress`. A failure here is treated as
/// graph-mid-edit and surfaced as a tolerated skip in the caller.
fn load_in_progress_tasks(ctx: &crate::config::ProjectContext) -> Result<Vec<(String, String)>> {
    use tempyr_core::graph::Graph;
    let graph = Graph::load_from_directory(&ctx.graph_dir, ctx.schema.clone())
        .map_err(|e| anyhow!("load graph: {e}"))?;

    Ok(graph
        .nodes_of_type("task")
        .into_iter()
        .filter(|n| n.status() == Some("in_progress"))
        .map(|n| (n.id().to_string(), n.title().to_string()))
        .collect())
}

/// Phase 2 of the lint: count journal entries referencing each
/// in-progress task. Failures here are real bugs (corrupt index,
/// missing schema, etc.) and propagate as `Err` so `--strict` and
/// JSON consumers see the failure instead of a silently-empty
/// warnings array.
fn count_journal_refs(
    common_dir: &std::path::Path,
    in_progress: &[(String, String)],
) -> Result<Vec<LintWarning>> {
    if in_progress.is_empty() {
        return Ok(Vec::new());
    }
    let db_path = tempyr_journal_index::index_db_path(common_dir);
    let conn =
        tempyr_journal_index::schema::open(&db_path).map_err(|e| anyhow!("open index: {e}"))?;

    let mut warnings = Vec::new();
    for (task_id, title) in in_progress {
        let count = tempyr_journal_index::count_entries_referencing_node(&conn, task_id)
            .map_err(|e| anyhow!("count refs: {e}"))?;
        if count == 0 {
            warnings.push(LintWarning::InProgressTaskWithoutJournal {
                task_id: task_id.clone(),
                title: title.clone(),
            });
        }
    }
    Ok(warnings)
}

#[derive(Args, Debug)]
pub struct StatsCmdArgs {
    /// Restrict aggregates to entries newer than this many days.
    /// Affects every section except the activity histogram (which
    /// has its own window). Default: no filter (all of history).
    #[arg(long)]
    pub since_days: Option<u32>,
    /// Cap the top-tags list at this many rows. Default 20.
    #[arg(long, default_value = "20")]
    pub top_tags: usize,
    /// Cap the top-files list at this many rows. Default 20.
    #[arg(long, default_value = "20")]
    pub top_files: usize,
    /// Days of activity histogram to render, counting back from
    /// today. Default 30.
    #[arg(long, default_value = "30")]
    pub activity_window_days: u32,
}

pub fn run_stats(args: StatsCmdArgs, json_output: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("read current directory")?;
    let common_dir =
        jpath::git_common_dir(&cwd).map_err(|e| anyhow!("not in a git repository: {e}"))?;
    let repo_root =
        jpath::repo_toplevel(&cwd).map_err(|e| anyhow!("could not resolve repo top-level: {e}"))?;

    // Refresh structural-only so the stats include anything the
    // agent just logged. No query string => no embedder needed.
    tempyr_journal_index::refresh_index(&common_dir, &repo_root)
        .map_err(|e| anyhow!("refresh index: {e}"))?;

    let opts = tempyr_journal_index::StatsOptions {
        since_days: args.since_days,
        top_tags: args.top_tags,
        top_files: args.top_files,
        activity_window_days: args.activity_window_days,
    };
    let db_path = tempyr_journal_index::index_db_path(&common_dir);
    let conn =
        tempyr_journal_index::schema::open(&db_path).map_err(|e| anyhow!("open index: {e}"))?;
    let report =
        tempyr_journal_index::compute_stats(&conn, &opts).map_err(|e| anyhow!("stats: {e}"))?;

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
        return Ok(());
    }
    render_stats_text(&report, &args);
    Ok(())
}

fn render_stats_text(report: &tempyr_journal_index::StatsReport, args: &StatsCmdArgs) {
    println!("Journal stats");
    if let Some(d) = args.since_days {
        println!("  filter: last {d} day(s)");
    }
    println!(
        "  total entries:  {} (across {} session(s), {} agent(s))",
        report.total_entries, report.total_sessions, report.total_agents
    );
    if report.total_entries > 0 {
        let provisional_pct =
            100.0 * report.provisional_entries as f64 / report.total_entries as f64;
        let final_pct = 100.0 * report.final_entries as f64 / report.total_entries as f64;
        println!(
            "  provisional:    {} ({:.1}%)   final: {} ({:.1}%)",
            report.provisional_entries, provisional_pct, report.final_entries, final_pct,
        );
    }
    if let Some(ratio) = report.dead_end_ratio {
        println!(
            "  dead-end ratio: {:.1}% (dead_end / (decision + dead_end))",
            ratio * 100.0
        );
        if ratio < 0.1 {
            println!("    note: low dead-end rate often means agents aren't logging failures");
        }
    }

    if !report.kind_distribution.is_empty() {
        println!();
        println!("Kind distribution");
        let total = report.total_entries.max(1);
        for k in &report.kind_distribution {
            let pct = 100.0 * k.count as f64 / total as f64;
            println!("  {:<11} {:>6}   {:>5.1}%", k.kind, k.count, pct);
        }
    }

    if !report.sessions_per_agent.is_empty() {
        println!();
        println!("Sessions per agent");
        for a in &report.sessions_per_agent {
            println!("  {:<20} {:>6}", a.agent, a.session_count);
        }
    }

    if !report.top_tags.is_empty() {
        println!();
        println!("Top tags");
        for t in &report.top_tags {
            println!("  {:<30} {:>6}", t.tag, t.count);
        }
    }

    if !report.top_files.is_empty() {
        println!();
        println!("Top files");
        for f in &report.top_files {
            println!("  {:<50} {:>6}", f.path, f.count);
        }
    }

    if !report.activity_per_day.is_empty() {
        println!();
        println!("Activity (last {} days)", report.activity_per_day.len());
        // Find max so we can scale a tiny ASCII bar.
        let max = report
            .activity_per_day
            .iter()
            .map(|d| d.count)
            .max()
            .unwrap_or(0)
            .max(1);
        for d in &report.activity_per_day {
            let bar_len = ((d.count as f64 / max as f64) * 30.0).round() as usize;
            let bar: String = std::iter::repeat_n('█', bar_len).collect();
            println!("  {} {:>5}  {}", d.date, d.count, bar);
        }
    }
}

#[derive(Args, Debug)]
pub struct ShowArgs {
    /// Entry id (e.g. `j-4b511f6f9cf9425b906ae90c31bd3367`).
    pub id: String,
}

pub fn run_show(args: ShowArgs, json_output: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("read current directory")?;
    let common_dir =
        jpath::git_common_dir(&cwd).map_err(|e| anyhow!("not in a git repository: {e}"))?;
    let repo_root =
        jpath::repo_toplevel(&cwd).map_err(|e| anyhow!("could not resolve repo top-level: {e}"))?;

    let db_path = tempyr_journal_index::index_db_path(&common_dir);

    // Mirror journal_get: try, refresh on miss, retry once.
    let conn =
        tempyr_journal_index::schema::open(&db_path).map_err(|e| anyhow!("open index: {e}"))?;
    let mut entry =
        tempyr_journal_index::get_entry(&conn, &args.id).map_err(|e| anyhow!("lookup: {e}"))?;
    drop(conn);
    if entry.is_none() {
        tempyr_journal_index::refresh_index(&common_dir, &repo_root)
            .map_err(|e| anyhow!("refresh index: {e}"))?;
        let conn =
            tempyr_journal_index::schema::open(&db_path).map_err(|e| anyhow!("open index: {e}"))?;
        entry =
            tempyr_journal_index::get_entry(&conn, &args.id).map_err(|e| anyhow!("lookup: {e}"))?;
    }

    let Some(entry) = entry else {
        if json_output {
            println!("null");
        } else {
            println!("no entry {} in index", args.id);
            println!(
                "(if the entry was published from another machine, run `tempyr journal fetch` first)"
            );
        }
        // Non-zero so scripts can tell.
        std::process::exit(1);
    };

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&entry).unwrap_or_default()
        );
    } else {
        println!("id:        {}", entry.id);
        println!("kind:      {}", entry.kind.as_str());
        println!("ts:        {}", entry.ts.to_rfc3339());
        println!("agent:     {}", entry.agent);
        println!("session:   {}", entry.session_id);
        if let Some(b) = &entry.branch {
            println!("branch:    {b}");
        }
        if let Some(h) = &entry.head {
            println!("head:      {h}");
        }
        println!();
        println!("summary:   {}", entry.summary);
        if let Some(d) = &entry.detail {
            println!();
            println!("detail:");
            for line in d.lines() {
                println!("  {line}");
            }
        }
        // Per-kind structured fields, when present.
        if let Some(c) = &entry.chosen {
            println!();
            println!("chosen:    {c}");
        }
        if let Some(r) = &entry.rationale {
            println!("rationale: {r}");
        }
        if let Some(rev) = entry.reversible {
            println!("reversible: {rev}");
        }
        if let Some(a) = &entry.approach {
            println!();
            println!("approach:     {a}");
        }
        if let Some(f) = &entry.failure_mode {
            println!("failure:      {f}");
        }
        if let Some(n) = &entry.next_to_try {
            println!("next-to-try:  {n}");
        }
    }
    Ok(())
}

#[derive(Args, Debug)]
pub struct BootstrapArgs {
    /// Suppress the "bootstrapped" success line. Useful inside hook
    /// commands where stdout is shown to the user verbatim and we
    /// only want output on failure.
    #[arg(long)]
    pub quiet: bool,
}

/// Ensure `<git-common-dir>/tempyr/journals/{open,publisher.log}` exists.
/// Designed for the `SessionStart` Claude Code hook so a freshly-opened
/// agent session has a place to write journal entries before any other
/// tool runs. Idempotent — calling it twice (or against an already-
/// populated layout) is a no-op.
///
/// Outside a git repo we exit 0 silently rather than erroring: tempyr
/// supports operating outside git (no journal, no publisher) and a
/// hook that fails on every Claude session start would be worse than
/// no hook at all.
pub fn run_bootstrap(args: BootstrapArgs, json_output: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("read current directory")?;
    let common_dir = match jpath::git_common_dir(&cwd) {
        Ok(c) => c,
        Err(_) => {
            // Not in a git repo — silently succeed. Hooks should not
            // fail just because the user opened Claude in a non-tempyr
            // directory.
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "bootstrapped": false,
                        "reason": "not in a git repository",
                    }))
                    .unwrap_or_default()
                );
            }
            return Ok(());
        }
    };
    jpath::ensure_layout(&common_dir).context("ensure journal layout")?;

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "bootstrapped": true,
                "journals_dir": jpath::journals_root(&common_dir).to_string_lossy(),
            }))
            .unwrap_or_default()
        );
    } else if !args.quiet {
        println!(
            "Journal layout ready at {}",
            jpath::journals_root(&common_dir).display()
        );
    }
    Ok(())
}

#[derive(Args, Debug)]
pub struct FinalizeArgs {
    /// Agent name. Defaults to "claude". A finalize call only acts on
    /// the active session belonging to *this* agent on *this* worktree;
    /// other agents' sessions are left alone.
    #[arg(long, default_value = "claude")]
    pub agent: String,
    /// Suppress success output, mirroring the `bootstrap` flag.
    #[arg(long)]
    pub quiet: bool,
}

/// Mark the active journal session for the (worktree, agent) pair as
/// ready for the publisher to archive. Designed for the `SessionEnd`
/// Claude Code hook.
///
/// Behavior:
/// - Outside a git repo → silent no-op (same rationale as `bootstrap`).
/// - No active session → silent no-op (the agent never logged anything,
///   so there's nothing to finalize).
/// - Active session → write the `.ready` marker. Idempotent: if the
///   marker already exists `Session::finalize` just touches it again.
///
/// Doesn't trigger publish — that stays in `tempyr journal flush` so
/// CI/local timing stays explicit. A user wanting auto-publish on
/// session end can chain `tempyr journal finalize && tempyr journal flush`
/// in their hook.
pub fn run_finalize(args: FinalizeArgs, json_output: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("read current directory")?;
    let common_dir = match jpath::git_common_dir(&cwd) {
        Ok(c) => c,
        Err(_) => {
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "finalized": false,
                        "reason": "not in a git repository",
                    }))
                    .unwrap_or_default()
                );
            }
            return Ok(());
        }
    };
    let worktree_top =
        jpath::repo_toplevel(&cwd).map_err(|e| anyhow!("could not resolve repo top-level: {e}"))?;

    let active = Session::find_active(&common_dir, &worktree_top, &args.agent)
        .map_err(|e| anyhow!("look up active session: {e}"))?;

    let outcome = match active {
        Some(session) => {
            session
                .finalize()
                .map_err(|e| anyhow!("finalize session: {e}"))?;
            Some(session.id().as_str().to_string())
        }
        None => None,
    };

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "finalized": outcome.is_some(),
                "session_id": outcome,
            }))
            .unwrap_or_default()
        );
    } else if !args.quiet {
        match &outcome {
            Some(id) => println!("Finalized session {id}"),
            None => {
                println!(
                    "No active session for agent '{}' on this worktree.",
                    args.agent
                )
            }
        }
    }
    Ok(())
}
