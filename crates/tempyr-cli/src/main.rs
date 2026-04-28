mod commands;
mod config;

use clap::{Parser, Subcommand};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "tempyr",
    about = "File-based knowledge graph for AI-assisted design"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Path to graph directory (default: ./graph)
    #[arg(long, global = true)]
    pub graph_dir: Option<PathBuf>,

    /// Path to config file (default: ./.tempyr/config.toml)
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    /// Verbose output
    #[arg(long, short, global = true)]
    pub verbose: bool,

    /// JSON output for scripting
    #[arg(long, global = true)]
    pub json: bool,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Initialize a new graph in the current directory
    Init {
        /// Force the interactive ratatui onboarding flow
        #[arg(long, conflicts_with = "no_wizard")]
        wizard: bool,
        /// Skip interactive onboarding even in a terminal
        #[arg(long, conflicts_with = "wizard")]
        no_wizard: bool,
    },

    /// Check graph consistency
    Validate {
        /// Automatically add missing reverse edges
        #[arg(long)]
        fix: bool,
    },

    /// Create a new node
    Add {
        /// Node type (feature, task, decision, etc.)
        #[arg(name = "type")]
        node_type: String,

        /// Human-readable slug (system appends 6-char suffix)
        #[arg(long, group = "identifier")]
        slug: Option<String>,

        /// Full node ID (bypass auto-suffix, for migration/compat)
        #[arg(long, group = "identifier")]
        id: Option<String>,

        /// Node status
        #[arg(long)]
        status: Option<String>,

        /// Node owner
        #[arg(long)]
        owner: Option<String>,

        /// Node body (markdown content)
        #[arg(long)]
        body: Option<String>,
    },

    /// Add an edge between two nodes (writes both files)
    AddEdge {
        source: String,
        target: String,
        #[arg(name = "type")]
        edge_type: String,
    },

    /// Remove an edge between two nodes (writes both files)
    RemoveEdge {
        source: String,
        target: String,
        #[arg(name = "type")]
        edge_type: String,
    },

    /// Rename a node, updating all references
    Rename {
        /// Current node ID
        old_id: String,
        /// New full ID (changes everything)
        new_id: Option<String>,
        /// New slug only (preserves the 6-char suffix)
        #[arg(long)]
        slug: Option<String>,
    },

    /// Change a node's status
    Status { id: String, new_status: String },

    /// Show all nodes reachable from a root
    Traverse {
        id: String,
        /// Max traversal depth
        #[arg(long, default_value = "2")]
        depth: usize,
        /// Filter by edge type
        #[arg(long = "type")]
        edge_type: Option<String>,
    },

    /// Full-text keyword search
    Search {
        query: Vec<String>,
        /// Max results
        #[arg(long, default_value = "10")]
        max_results: usize,
        /// Filter by node type
        #[arg(long = "type")]
        node_type: Option<String>,
        /// Filter by status (e.g. backlog, in_progress, done, draft)
        #[arg(long)]
        status: Option<String>,
        /// Filter by owner
        #[arg(long)]
        owner: Option<String>,
    },

    /// List nodes by metadata (type, status, owner) — no search query needed
    List {
        /// Filter by node type
        #[arg(long = "type")]
        node_type: Option<String>,
        /// Filter by status (e.g. backlog, in_progress, done, draft)
        #[arg(long)]
        status: Option<String>,
        /// Filter by owner
        #[arg(long)]
        owner: Option<String>,
        /// Max results
        #[arg(long, default_value = "50")]
        max_results: usize,
    },

    /// Hybrid retrieval (structural + BM25)
    Context {
        query: Vec<String>,
        /// Start structural traversal from this node
        #[arg(long)]
        root: Option<String>,
        /// Max tokens of context to return
        #[arg(long, default_value = "8000")]
        budget: usize,
    },

    /// Generate an agent-ready implementation prompt from a task node
    Dispatch {
        /// Task node ID
        task_id: String,
        /// Target agent: claude (with MCP access) or codex (all context inline)
        #[arg(long, default_value = "claude")]
        target: String,
    },

    /// Render a document from a root node
    Render {
        /// Template name (prd, tdd, etc.)
        template: String,
        /// Root node ID
        root_id: String,
        /// Render graph state at a point in time
        #[arg(long)]
        as_of: Option<String>,
        /// Include superseded edges/nodes
        #[arg(long)]
        include_history: bool,
        /// Write to file
        #[arg(long)]
        output: Option<PathBuf>,
    },

    /// Vector similarity search (requires embeddings)
    Vsearch {
        query: Vec<String>,
        /// Max results
        #[arg(long, default_value = "10")]
        max_results: usize,
        /// Filter by node type
        #[arg(long = "type")]
        node_type: Option<String>,
    },

    /// Ask a question and get an answer grounded in graph context
    Ask {
        question: Vec<String>,
        /// Anchor to a specific node
        #[arg(long)]
        root: Option<String>,
    },

    /// Find and propose merges for duplicate nodes
    Dedupe {
        /// Similarity threshold (0.0-1.0)
        #[arg(long, default_value = "0.8")]
        threshold: f64,
    },

    /// Run a schema migration
    Migrate {
        /// Migration description (e.g., "rename-type old new")
        args: Vec<String>,
    },

    /// Import unstructured text and propose nodes
    Import {
        /// Path to file to import
        file: PathBuf,
    },

    /// Interview session management
    Interview {
        #[command(subcommand)]
        action: InterviewAction,
    },

    /// Index management
    Index {
        #[command(subcommand)]
        action: IndexAction,
    },

    /// Linear integration
    Linear {
        #[command(subcommand)]
        action: LinearAction,
    },

    /// Session journal: capture and inspect agent reasoning
    Journal {
        #[command(subcommand)]
        action: JournalAction,
    },

    /// Update managed .claude/ artifacts to match current tempyr version
    Update {
        /// Only check staleness, don't write (exit code 1 if stale)
        #[arg(long)]
        check: bool,
        /// Overwrite even user-modified files
        #[arg(long)]
        force: bool,
    },

    /// Report system health: embedding provider, config files, paths, and warnings
    Doctor,
}

#[derive(Subcommand)]
pub enum InterviewAction {
    /// Start a new interview from a brain dump
    Start {
        /// The brain dump text
        brain_dump: String,
        /// Root node type (default: feature)
        #[arg(long, default_value = "feature")]
        root_type: String,
    },
    /// Process an answer in an active interview
    Answer { session_id: String, answer: String },
    /// Show tentative graph state
    Show { session_id: String },
    /// Commit tentative nodes to disk
    Commit { session_id: String },
    /// Resume an interrupted session
    Resume { session_id: String },
    /// List active sessions
    List,
}

#[derive(Subcommand)]
pub enum IndexAction {
    /// Full index rebuild from source files
    Rebuild {
        /// Refresh structural search data only; do not call embedding providers
        #[arg(long)]
        skip_embeddings: bool,
    },
    /// Incremental update (changed files only)
    Update {
        /// Refresh structural search data only; do not call embedding providers
        #[arg(long)]
        skip_embeddings: bool,
    },
    /// Show index statistics
    Stats,
}

#[derive(Subcommand)]
pub enum LinearAction {
    /// Configure Linear integration: select team, test connection
    Setup,
    /// Push graph nodes to Linear
    Push {
        /// Specific node ID to push (default: all syncable)
        node_id: Option<String>,
        /// Show what would be pushed without making changes
        #[arg(long)]
        dry_run: bool,
    },
    /// Pull changes from Linear into the graph
    Pull {
        /// Show what would be pulled without making changes
        #[arg(long)]
        dry_run: bool,
    },
    /// Bidirectional sync: push then pull
    Sync {
        /// Show what would change without making changes
        #[arg(long)]
        dry_run: bool,
    },
    /// Show sync state summary
    Status,
    /// Manually link an existing node to a Linear issue/project
    Link { node_id: String, linear_id: String },
    /// Unlink a node from Linear (does not delete the Linear issue)
    Unlink { node_id: String },
}

#[derive(Subcommand)]
pub enum JournalAction {
    /// Append one moment of agent reasoning to the session journal.
    ///
    /// Kinds: plan | finding | decision | dead_end | assumption | question | risk | outcome.
    /// Required for `decision`: --chosen, --rationale, --reversible <true|false> (and detail >= 50 chars).
    /// Required for `dead_end`: --approach, --failure-mode (and detail >= 50 chars).
    /// Required for `assumption`: --polarity.
    Log(Box<commands::journal_cmd::LogArgs>),

    /// Commit finalized journal sessions as Git refs and push to the remote.
    ///
    /// Scans `<git-common-dir>/tempyr/journals/open/` for sessions with a
    /// `.ready` marker, archives each as a parent-less commit under
    /// `refs/tempyr/journals/archive/<YYYY>/<MM>/<DD>/<session_id>`, pushes
    /// the ref, and deletes the local files. Idempotent: a re-run after a
    /// crash picks up where it left off.
    Flush(commands::journal_cmd::FlushArgs),

    /// Show publisher health: open/ready counts, last push, last error,
    /// totals, and whether a publisher is currently running.
    Status(commands::journal_cmd::StatusArgs),

    /// Show recent publisher events from `<journals>/publisher.log`.
    Logs(commands::journal_cmd::LogsArgs),

    /// Pull journal refs from a remote (`+refs/tempyr/journals/*:...`).
    /// Required for multi-machine sync — without it, an agent's pushed
    /// journals don't appear in another agent's local repo.
    Fetch(commands::journal_cmd::FetchArgs),

    /// Refresh the derived SQLite index from open JSONL files and
    /// archived `refs/tempyr/journals/*` refs. Idempotent. Use
    /// `--rebuild` to truncate and re-ingest from scratch (multi-
    /// session safe); add `--force` to delete the db file outright
    /// for corrupt-db recovery.
    Index(commands::journal_cmd::IndexArgs),
}

#[derive(Debug, PartialEq, Eq)]
enum LaunchMode {
    Cli,
    Mcp { project_root: Option<PathBuf> },
    InvalidMcpArgs,
}

fn detect_launch_mode_from_args<I, S>(args: I) -> LaunchMode
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut args = args.into_iter().map(Into::into);
    let _program = args.next();

    match args.next() {
        Some(arg) if arg == OsStr::new("--mcp") => parse_mcp_launch_args(args),
        _ => LaunchMode::Cli,
    }
}

fn parse_mcp_launch_args<I>(mut args: I) -> LaunchMode
where
    I: Iterator<Item = OsString>,
{
    match args.next() {
        None => LaunchMode::Mcp { project_root: None },
        Some(arg) if arg == OsStr::new("--project-root") => match (args.next(), args.next()) {
            (Some(project_root), None) => LaunchMode::Mcp {
                project_root: Some(PathBuf::from(project_root)),
            },
            _ => LaunchMode::InvalidMcpArgs,
        },
        _ => LaunchMode::InvalidMcpArgs,
    }
}

fn mcp_args_error() -> anyhow::Error {
    anyhow::anyhow!(
        "`--mcp` must be the first argument, and if using `--project-root` it must be \
provided with a valid value. Launch the MCP server with `tempyr --mcp` or \
`tempyr --mcp --project-root <path>`."
    )
}

fn main() {
    let result: anyhow::Result<()> = match detect_launch_mode_from_args(std::env::args_os()) {
        LaunchMode::Cli => run_cli_mode(),
        LaunchMode::Mcp { project_root } => run_mcp_mode(project_root),
        LaunchMode::InvalidMcpArgs => Err(mcp_args_error()),
    };

    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

fn run_cli_mode() -> anyhow::Result<()> {
    let cli = Cli::parse();
    load_cli_project_env(cli.graph_dir.as_deref())?;
    run(cli)
}

fn run_mcp_mode(project_root: Option<PathBuf>) -> anyhow::Result<()> {
    let mut relative_project_root_fallback = None;
    if let Some(project_root) = project_root {
        let is_relative = project_root.is_relative();
        if is_relative {
            // Resolve relative anchors against MCP client roots after initialization;
            // the process cwd may be unrelated to the active workspace.
            relative_project_root_fallback = Some(project_root);
        } else {
            let roots = tempyr_core::project::find_project_roots_from(project_root.clone())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Not a tempyr project from --project-root {} (no .tempyr/ or .tempyr-redirect found)",
                        project_root.display()
                    )
                })?;
            std::env::set_current_dir(&roots.anchor_root)?;
            // The explicit CLI anchor must win over stale process environment left by
            // an MCP client or parent shell before the MCP layer loads project env.
            clear_mcp_project_env_overrides();
        }
    }

    if relative_project_root_fallback.is_some() {
        clear_mcp_project_env_overrides();
    }

    let rt = tokio::runtime::Runtime::new()?;
    let result = rt.block_on(tempyr_mcp::serve_stdio_with_project_root_fallback(
        relative_project_root_fallback,
    ));
    rt.shutdown_timeout(std::time::Duration::from_secs(1));
    result
}

fn clear_mcp_project_env_overrides() {
    // Rust 2024 marks environment mutation unsafe because other threads may read it.
    // This runs before the MCP Tokio runtime is started.
    unsafe {
        std::env::remove_var(tempyr_core::project::PROJECT_ROOT_ENV_VAR);
        std::env::remove_var(tempyr_core::project::GRAPH_DIR_ENV_VAR);
    }
}

fn load_cli_project_env(graph_dir: Option<&Path>) -> anyhow::Result<()> {
    match graph_dir {
        Some(graph_dir) => {
            tempyr_core::project::load_project_env_from(graph_dir.to_path_buf())?;
        }
        None => {
            tempyr_core::project::load_project_env()?;
        }
    }
    Ok(())
}

fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Commands::Init { wizard, no_wizard } => commands::init::run(cli.json, wizard, no_wizard),
        Commands::Validate { fix } => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            commands::validate::run(&ctx, cli.json, fix)
        }
        Commands::Add {
            node_type,
            slug,
            id,
            status,
            owner,
            body,
        } => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            commands::add::run(
                &ctx,
                &node_type,
                slug.as_deref(),
                id.as_deref(),
                status.as_deref(),
                owner.as_deref(),
                body.as_deref(),
            )
        }
        Commands::AddEdge {
            source,
            target,
            edge_type,
        } => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            commands::edge::run_add(&ctx, &source, &target, &edge_type)
        }
        Commands::RemoveEdge {
            source,
            target,
            edge_type,
        } => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            commands::edge::run_remove(&ctx, &source, &target, &edge_type)
        }
        Commands::Rename {
            old_id,
            new_id,
            slug,
        } => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            commands::rename::run(&ctx, &old_id, new_id.as_deref(), slug.as_deref())
        }
        Commands::Status { id, new_status } => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            commands::status_cmd::run(&ctx, &id, &new_status)
        }
        Commands::Traverse {
            id,
            depth,
            edge_type,
        } => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            commands::traverse::run(&ctx, &id, depth, edge_type.as_deref(), cli.json)
        }
        Commands::Search {
            query,
            max_results,
            node_type,
            status,
            owner,
        } => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            commands::search::run(
                &ctx,
                &query.join(" "),
                max_results,
                node_type.as_deref(),
                status.as_deref(),
                owner.as_deref(),
                cli.json,
            )
        }
        Commands::List {
            node_type,
            status,
            owner,
            max_results,
        } => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            commands::list::run(
                &ctx,
                node_type.as_deref(),
                status.as_deref(),
                owner.as_deref(),
                max_results,
                cli.json,
            )
        }
        Commands::Context {
            query,
            root,
            budget,
        } => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            commands::context::run(&ctx, &query.join(" "), root.as_deref(), budget, cli.json)
        }
        Commands::Dispatch { task_id, target } => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            let target = commands::dispatch::DispatchTarget::from_str(&target)?;
            commands::dispatch::run(&ctx, &task_id, target, cli.json)
        }
        Commands::Render {
            template,
            root_id,
            as_of,
            include_history,
            output,
        } => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            commands::render_cmd::run(
                &ctx,
                &template,
                &root_id,
                as_of.as_deref(),
                include_history,
                output.as_deref(),
            )
        }
        Commands::Vsearch {
            query,
            max_results,
            node_type,
        } => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            commands::vsearch::run(
                &ctx,
                &query.join(" "),
                max_results,
                node_type.as_deref(),
                cli.json,
            )
        }
        Commands::Ask { question, root } => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            commands::ask::run(&ctx, &question.join(" "), root.as_deref(), cli.json)
        }
        Commands::Dedupe { threshold } => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            commands::dedupe::run(&ctx, threshold, cli.json)
        }
        Commands::Migrate { args } => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            commands::migrate::run(&ctx, &args)
        }
        Commands::Import { file } => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            commands::import::run(&ctx, &file)
        }
        Commands::Interview { action } => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            match action {
                InterviewAction::Start {
                    brain_dump,
                    root_type,
                } => commands::interview_cmd::run_start(&ctx, &brain_dump, &root_type, cli.json),
                InterviewAction::Answer { session_id, answer } => {
                    commands::interview_cmd::run_answer(&ctx, &session_id, &answer, cli.json)
                }
                InterviewAction::Show { session_id } => {
                    commands::interview_cmd::run_show(&ctx, &session_id, cli.json)
                }
                InterviewAction::Commit { session_id } => {
                    commands::interview_cmd::run_commit(&ctx, &session_id)
                }
                InterviewAction::Resume { session_id } => {
                    commands::interview_cmd::run_show(&ctx, &session_id, cli.json)
                }
                InterviewAction::List => commands::interview_cmd::run_list(&ctx, cli.json),
            }
        }
        Commands::Index { action } => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            match action {
                IndexAction::Rebuild { skip_embeddings } => {
                    commands::index_cmd::run_rebuild(&ctx, cli.json, skip_embeddings)
                }
                IndexAction::Update { skip_embeddings } => {
                    commands::index_cmd::run_update(&ctx, cli.json, skip_embeddings)
                }
                IndexAction::Stats => commands::index_cmd::run_stats(&ctx, cli.json),
            }
        }
        Commands::Linear { action } => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            match action {
                LinearAction::Setup => commands::linear_cmd::run_setup(&ctx, cli.json),
                LinearAction::Push { node_id, dry_run } => {
                    commands::linear_cmd::run_push(&ctx, node_id.as_deref(), dry_run, cli.json)
                }
                LinearAction::Pull { dry_run } => {
                    commands::linear_cmd::run_pull(&ctx, dry_run, cli.json)
                }
                LinearAction::Sync { dry_run } => {
                    commands::linear_cmd::run_sync(&ctx, dry_run, cli.json)
                }
                LinearAction::Status => commands::linear_cmd::run_status(&ctx, cli.json),
                LinearAction::Link { node_id, linear_id } => {
                    commands::linear_cmd::run_link(&ctx, &node_id, &linear_id)
                }
                LinearAction::Unlink { node_id } => {
                    commands::linear_cmd::run_unlink(&ctx, &node_id)
                }
            }
        }
        Commands::Journal { action } => match action {
            JournalAction::Log(args) => commands::journal_cmd::run_log(*args, cli.json),
            JournalAction::Flush(args) => commands::journal_cmd::run_flush(args, cli.json),
            JournalAction::Status(args) => commands::journal_cmd::run_status(args, cli.json),
            JournalAction::Logs(args) => commands::journal_cmd::run_logs(args, cli.json),
            JournalAction::Fetch(args) => commands::journal_cmd::run_fetch(args, cli.json),
            JournalAction::Index(args) => commands::journal_cmd::run_index(args, cli.json),
        },
        Commands::Update { check, force } => commands::update::run(check, force),
        Commands::Doctor => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            commands::doctor::run(&ctx, cli.json)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Cli, LaunchMode, detect_launch_mode_from_args};
    use clap::Parser;
    use std::path::PathBuf;

    #[test]
    fn detect_mcp_mode_when_flag_is_first_arg() {
        assert_eq!(
            detect_launch_mode_from_args(["tempyr", "--mcp"]),
            LaunchMode::Mcp { project_root: None }
        );
    }

    #[test]
    fn detect_mcp_project_root_arg() {
        assert_eq!(
            detect_launch_mode_from_args(["tempyr", "--mcp", "--project-root", "C:\\repo"]),
            LaunchMode::Mcp {
                project_root: Some(PathBuf::from("C:\\repo"))
            }
        );
    }

    #[test]
    fn detect_cli_mode_for_normal_commands() {
        assert_eq!(
            detect_launch_mode_from_args(["tempyr", "validate"]),
            LaunchMode::Cli
        );
    }

    #[test]
    fn reject_extra_args_after_mcp_flag() {
        assert_eq!(
            detect_launch_mode_from_args(["tempyr", "--mcp", "validate"]),
            LaunchMode::InvalidMcpArgs
        );
    }

    #[test]
    fn reject_missing_mcp_project_root_value() {
        assert_eq!(
            detect_launch_mode_from_args(["tempyr", "--mcp", "--project-root"]),
            LaunchMode::InvalidMcpArgs
        );
    }

    #[test]
    fn keep_cli_mode_for_existing_global_flags() {
        assert_eq!(
            detect_launch_mode_from_args(["tempyr", "--json", "validate"]),
            LaunchMode::Cli
        );
    }

    #[test]
    fn init_rejects_conflicting_wizard_flags() {
        assert!(Cli::try_parse_from(["tempyr", "init", "--wizard", "--no-wizard"]).is_err());
    }
}
