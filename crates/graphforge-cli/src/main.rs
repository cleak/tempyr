mod commands;
mod config;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "graphforge", about = "File-based knowledge graph for AI-assisted design")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Path to graph directory (default: ./graph)
    #[arg(long, global = true)]
    pub graph_dir: Option<PathBuf>,

    /// Path to config file (default: ./.graphforge/config.toml)
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
    Init,

    /// Check graph consistency
    Validate,

    /// Create a new node
    Add {
        /// Node type (feature, task, decision, etc.)
        #[arg(name = "type")]
        node_type: String,

        /// Node ID (slug)
        #[arg(long)]
        id: String,

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
        old_id: String,
        new_id: String,
    },

    /// Change a node's status
    Status {
        id: String,
        new_status: String,
    },

    /// Show all nodes reachable from a root
    Traverse {
        id: String,
        /// Max traversal depth
        #[arg(long, default_value = "2")]
        depth: usize,
        /// Filter by edge type
        #[arg(long, name = "type")]
        edge_type: Option<String>,
    },

    /// Full-text keyword search
    Search {
        query: Vec<String>,
        /// Max results
        #[arg(long, default_value = "10")]
        max_results: usize,
        /// Filter by node type
        #[arg(long, name = "type")]
        node_type: Option<String>,
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
        #[arg(long, name = "type")]
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
    Answer {
        session_id: String,
        answer: String,
    },
    /// Show tentative graph state
    Show {
        session_id: String,
    },
    /// Commit tentative nodes to disk
    Commit {
        session_id: String,
    },
    /// Resume an interrupted session
    Resume {
        session_id: String,
    },
    /// List active sessions
    List,
}

#[derive(Subcommand)]
pub enum IndexAction {
    /// Full index rebuild from source files
    Rebuild,
    /// Incremental update (changed files only)
    Update,
    /// Show index statistics
    Stats,
}

fn main() {
    let cli = Cli::parse();

    if let Err(e) = run(cli) {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Commands::Init => commands::init::run(),
        Commands::Validate => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            commands::validate::run(&ctx, cli.json)
        }
        Commands::Add { node_type, id, status, owner, body } => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            commands::add::run(&ctx, &node_type, &id, status.as_deref(), owner.as_deref(), body.as_deref())
        }
        Commands::AddEdge { source, target, edge_type } => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            commands::edge::run_add(&ctx, &source, &target, &edge_type)
        }
        Commands::RemoveEdge { source, target, edge_type } => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            commands::edge::run_remove(&ctx, &source, &target, &edge_type)
        }
        Commands::Rename { old_id, new_id } => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            commands::rename::run(&ctx, &old_id, &new_id)
        }
        Commands::Status { id, new_status } => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            commands::status_cmd::run(&ctx, &id, &new_status)
        }
        Commands::Traverse { id, depth, edge_type } => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            commands::traverse::run(&ctx, &id, depth, edge_type.as_deref(), cli.json)
        }
        Commands::Search { query, max_results, node_type } => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            commands::search::run(&ctx, &query.join(" "), max_results, node_type.as_deref(), cli.json)
        }
        Commands::Context { query, root, budget } => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            commands::context::run(&ctx, &query.join(" "), root.as_deref(), budget, cli.json)
        }
        Commands::Render { template, root_id, as_of, include_history, output } => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            commands::render_cmd::run(&ctx, &template, &root_id, as_of.as_deref(), include_history, output.as_deref())
        }
        Commands::Vsearch { query, max_results, node_type } => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            commands::vsearch::run(&ctx, &query.join(" "), max_results, node_type.as_deref(), cli.json)
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
                InterviewAction::Start { brain_dump, root_type } => {
                    commands::interview_cmd::run_start(&ctx, &brain_dump, &root_type, cli.json)
                }
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
                InterviewAction::List => {
                    commands::interview_cmd::run_list(&ctx, cli.json)
                }
            }
        }
        Commands::Index { action } => {
            let ctx = config::ProjectContext::find(cli.graph_dir.as_deref())?;
            match action {
                IndexAction::Rebuild => commands::index_cmd::run_rebuild(&ctx, cli.json),
                IndexAction::Update => commands::index_cmd::run_update(&ctx, cli.json),
                IndexAction::Stats => commands::index_cmd::run_stats(&ctx, cli.json),
            }
        }
    }
}
