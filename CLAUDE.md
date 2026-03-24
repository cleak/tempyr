# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

GraphForge (package name: `tempyr`) is a file-based knowledge graph system for AI-assisted product and technical design. It sits between a PRD helper, a project management system, and an AI-centric task system. The primary interaction model is an AI-assisted interview flow that decomposes brain dumps into typed graph nodes and edges.

The full specification lives in `docs/graphspec.md`. Read the relevant sections before implementing any component.

**Core principles:**
- Files (markdown + YAML frontmatter) are the source of truth; git provides versioning
- Documents (PRDs, TDDs) are rendered views (queries) over the graph, not stored artifacts
- Hybrid retrieval: structural traversal + BM25 full-text + vector similarity, all from a derived SQLite index
- The interview is the product: AI interviews the user, proposes nodes/edges, commits on approval
- Zero infrastructure: single binary, no servers, `git clone` + `graphforge index rebuild`

## Build & Test Commands

```bash
cargo build                    # build
cargo test                     # run all tests
cargo test <test_name>         # run a single test
cargo test --lib               # lib tests only (no integration tests)
cargo clippy                   # lint
cargo fmt --check              # check formatting
cargo run -- <subcommand>      # run the CLI
```

Rust edition is 2024. Target toolchain is stable.

## Architecture

The project is a **Rust workspace** with six crates:

| Crate | Purpose |
|-------|---------|
| `graphforge-core` | Graph data model: node/edge parsing, schema validation, in-memory graph, traversal, temporal filtering |
| `graphforge-index` | SQLite indexing: FTS5 full-text, sqlite-vec embeddings, hybrid retrieval pipeline, incremental updates |
| `graphforge-interview` | Interview state machine: session management, phase transitions, gap detection, LLM-based extraction |
| `graphforge-render` | Document rendering: TOML template parsing, graph collection, markdown output |
| `graphforge-cli` | CLI binary (`graphforge`): clap-based, all user-facing commands |
| `graphforge-mcp` | MCP server binary: exposes graph operations as tools for Claude Code |

Crate dependency order: `core` ← `index` ← `interview`/`render` ← `cli`/`mcp`.

### Data Model

- **Nodes** are `.md` files with YAML frontmatter in `graph/<type>/` directories (e.g., `graph/features/feat-session-replay.md`)
- **Edges** are stored bidirectionally in YAML frontmatter — both source and target files contain the edge. Edge lists are sorted alphabetically by target.
- **Schema** (`schema.toml`) defines node types, required fields, allowed statuses, and valid edge types with reverse mappings
- **Index** (`.graphforge/index.db`) is a derived SQLite database (gitignored, rebuildable) containing structural data, FTS5, and vector embeddings

### Key Design Patterns

- **LLM is for extraction only.** Gap detection, phase transitions, duplicate checking, and graph operations are deterministic Rust code. The LLM extracts structured data from natural language — it doesn't make control-flow decisions.
- **Bidirectional edge sync.** Every `add-edge` writes both files atomically. `validate` catches drift. Edge type pairs are defined in `schema.toml` (e.g., `child_of` ↔ `parent_of`).
- **Temporal edges.** Edges have optional `valid_from`/`valid_until` for point-in-time rendering. Nodes have a `status` lifecycle (e.g., `superseded`). Decisions get superseded, not deleted.
- **Content-hash embedding cache.** Embeddings are keyed by blake3 hash of the markdown body (not frontmatter). Re-embed only when body changes.
- **Token budget enforcement.** Hybrid retrieval greedily fills context by combined score until the token budget is exhausted.

### Interview Engine Flow

The interview is a 5-phase state machine: Discovery → Product → Technical → Decomposition → Review. Each phase has typed gaps (e.g., `MissingPersona`, `NoTechnicalDecision`) that drive contextual questions. Phase transitions happen when required gaps in the current phase are filled. All proposals are tentative until the user commits.

## Key Dependencies

- `rusqlite` (bundled) + `sqlite-vec` — index storage
- `serde` + `serde_yaml` + `serde_json` + `toml` — serialization
- `tokio` — async runtime (MCP server, API calls)
- `reqwest` — LLM/embedding API client
- `clap` — CLI
- `blake3` — content hashing
- `walkdir` — filesystem traversal
- `chrono` + `uuid` — timestamps and session IDs

## Conventions

- Node IDs are human-readable kebab-case slugs (e.g., `feat-session-replay`, `decision-storage-backend`)
- Never manually rename node IDs — use `graphforge rename` which updates all references atomically
- Node granularity rule: one decision, one fact, or one concept per node. If it can't be independently linked, it's a paragraph, not a node.
- Interview extraction prompts use temperature 0.1 for structured JSON output
- The binary name is `graphforge`, not `tempyr` (tempyr is the package/repo name)
