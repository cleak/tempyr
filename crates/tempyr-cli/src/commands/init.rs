const DEFAULT_SCHEMA: &str = include_str!("../../../../schema/default-schema.toml");

const DEFAULT_CONFIG: &str = r#"[general]
graph_dir = "graph"
schema_path = ".tempyr/schema.toml"

[embedding]
provider = "voyage"                    # voyage | gemini | local
model = "voyage-4"                     # voyage-4, voyage-4-large, gemini-embedding-001, etc.
dimensions = 1024                      # 1024 for voyage, 768 for gemini
# API key: set VOYAGE_API_KEY or GEMINI_API_KEY environment variable

[llm]
provider = "anthropic"
model = "claude-sonnet-4-20250514"
temperature = 0.1

[retrieval]
default_token_budget = 8000
structural_weight = 0.5
bm25_weight = 0.25
vector_weight = 0.25
recency_boost_days = 7
recency_boost_value = 0.1

[interview]
max_questions_per_turn = 3
auto_advance_phases = true
session_timeout_hours = 168

[mcp]
transport = "stdio"
"#;

pub fn run() -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let gf_dir = cwd.join(".tempyr");
    let graph_dir = cwd.join("graph");

    if gf_dir.exists() {
        anyhow::bail!("Already initialized: .tempyr/ exists");
    }

    // Create .tempyr/ directory
    std::fs::create_dir_all(&gf_dir)?;
    std::fs::create_dir_all(gf_dir.join("render"))?;
    std::fs::create_dir_all(gf_dir.join("sessions"))?;

    // Write schema and config
    std::fs::write(gf_dir.join("schema.toml"), DEFAULT_SCHEMA)?;
    std::fs::write(gf_dir.join("config.toml"), DEFAULT_CONFIG)?;

    // Load schema to get directory names
    let schema = tempyr_core::schema::Schema::from_str(DEFAULT_SCHEMA)?;

    // Create graph directories
    for node_type in schema.node_types.values() {
        std::fs::create_dir_all(graph_dir.join(&node_type.directory))?;
    }

    // Install .claude/ artifacts (hooks, skills, agents)
    let results = super::managed::install_all(&cwd, false)?;

    println!("Initialized tempyr project in {}", cwd.display());
    println!("  .tempyr/schema.toml  - node and edge type definitions");
    println!("  .tempyr/config.toml  - project configuration");
    println!("  graph/               - node files organized by type");
    for r in &results {
        if matches!(
            r.outcome,
            super::managed::WriteOutcome::Created | super::managed::WriteOutcome::Merged
        ) {
            println!("  {:<23}- {}", r.path, r.description);
        }
    }

    Ok(())
}
