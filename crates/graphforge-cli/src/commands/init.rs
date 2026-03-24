const DEFAULT_SCHEMA: &str = include_str!("../../../../schema/default-schema.toml");

const DEFAULT_CONFIG: &str = r#"[general]
graph_dir = "graph"
schema_path = ".graphforge/schema.toml"

[embedding]
provider = "anthropic"
model = "voyage-3"
dimensions = 1024
batch_size = 50

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
    let gf_dir = cwd.join(".graphforge");
    let graph_dir = cwd.join("graph");

    if gf_dir.exists() {
        anyhow::bail!("Already initialized: .graphforge/ exists");
    }

    // Create .graphforge/ directory
    std::fs::create_dir_all(&gf_dir)?;
    std::fs::create_dir_all(gf_dir.join("render"))?;
    std::fs::create_dir_all(gf_dir.join("sessions"))?;

    // Write schema and config
    std::fs::write(gf_dir.join("schema.toml"), DEFAULT_SCHEMA)?;
    std::fs::write(gf_dir.join("config.toml"), DEFAULT_CONFIG)?;

    // Load schema to get directory names
    let schema = graphforge_core::schema::Schema::from_str(DEFAULT_SCHEMA)?;

    // Create graph directories
    for node_type in schema.node_types.values() {
        std::fs::create_dir_all(graph_dir.join(&node_type.directory))?;
    }

    println!("Initialized graphforge project in {}", cwd.display());
    println!("  .graphforge/schema.toml  - node and edge type definitions");
    println!("  .graphforge/config.toml  - project configuration");
    println!("  graph/                   - node files organized by type");

    Ok(())
}
