# Tempyr

[![CI](https://github.com/cleak/tempyr/actions/workflows/ci.yml/badge.svg)](https://github.com/cleak/tempyr/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Rust](https://img.shields.io/badge/rust-2024_edition-orange.svg)](https://www.rust-lang.org)
[![Status: experimental](https://img.shields.io/badge/status-experimental-yellow.svg)](#status)

## Status

> Early, experimental, solo side project. APIs, schemas, and the on-disk
> graph layout may change without notice. Use at your own risk.

Tempyr is a file-based knowledge graph for AI-assisted product and technical
design. It sits between a PRD helper, a project management system, and an
AI-centric task system: graph files are the source of truth, while PRDs, TDDs,
task prompts, and search context are rendered views over that graph.

The primary workflow is an AI-assisted interview. The assistant turns a brain
dump into typed graph nodes and edges, asks follow-up questions for missing
context, and commits the result only after review.

## Core Ideas

- Markdown plus YAML frontmatter is the source of truth.
- Git provides history, branching, review, and collaboration.
- SQLite indices are derived and rebuildable, not authoritative.
- Retrieval combines graph traversal, BM25 full-text search, and vector search.
- PRDs and TDDs are rendered from graph queries instead of hand-maintained docs.
- The project runs as a single Rust binary with no server requirement.

## Workspace

This repository is a Rust workspace:

| Crate | Purpose |
| --- | --- |
| `tempyr-core` | Graph data model, parsing, validation, traversal, and temporal filtering |
| `tempyr-index` | SQLite indexing, full-text search, embeddings, and hybrid retrieval |
| `tempyr-interview` | Interview state machine, gap detection, and extraction flow |
| `tempyr-render` | Template parsing and rendered Markdown output |
| `tempyr-linear` | Linear push/pull sync and status mapping |
| `tempyr-journal` | Append-only agent session journal |
| `tempyr-journal-index` | Search index for journal entries |
| `tempyr-cli` | User-facing `tempyr` command |
| `tempyr-mcp` | MCP server library used by `tempyr --mcp` |

## Install

Build from source:

```sh
cargo build --workspace
```

Install the CLI from the local checkout:

```sh
cargo install --path crates/tempyr-cli --locked
```

Confirm the binary is available:

```sh
tempyr --help
```

## Quick Start

Create or enter a project directory, then initialize Tempyr:

```sh
mkdir tempyr-demo
cd tempyr-demo
tempyr init --no-wizard
```

Add a node:

```sh
tempyr add feature --id feat-session-replay --status draft --owner alice --body "Capture and replay user sessions."
```

Validate the graph:

```sh
tempyr validate
```

Rebuild derived search data:

```sh
tempyr index rebuild --skip-embeddings
```

Search the graph:

```sh
tempyr search session replay
```

Render a document from a root node:

```sh
tempyr render prd feat-session-replay --output renders/session-replay-prd.md
```

Inspect project health:

```sh
tempyr doctor
```

Start an interview from a brain dump:

```sh
tempyr interview start "I want a session replay feature for debugging customer reports."
```

Run the MCP server over stdio:

```sh
tempyr --mcp
```

## Configuration

Runtime project files live under `.tempyr/`. The index database and embedding
cache are derived artifacts and can be rebuilt from source graph files.

For embeddings or Linear integration, copy `.env.example` to `.env` and set the
provider-specific keys you need. `.env` is intentionally ignored by git.

## Development

Useful commands:

```sh
cargo build --workspace --locked
cargo test --workspace --locked
cargo test --lib
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
cargo audit
cargo run -- <subcommand>
```

The graph format and product specification are documented in
`docs/graphspec.md`. The journal subsystem is documented in
`docs/journal-spec.md`.

## License

Tempyr is licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in Tempyr is dual licensed as above, without additional terms or
conditions.
