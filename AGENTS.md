# AGENTS.md

This file provides guidance to AI coding agents working with code in this repository.

## Project Overview

Tempyr is a file-based knowledge graph system for AI-assisted product and technical design. It sits between a PRD helper, a project management system, and an AI-centric task system. The primary interaction model is an AI-assisted interview flow that decomposes brain dumps into typed graph nodes and edges.

The full specification lives in `docs/graphspec.md`. The journal subsystem (append-only agent reasoning log) has its own spec at `docs/journal-spec.md`. Read the relevant sections before implementing any component.

**Core principles:**
- Files (markdown + YAML frontmatter) are the source of truth; git provides versioning
- Documents (PRDs, TDDs) are rendered views (queries) over the graph, not stored artifacts
- Hybrid retrieval: structural traversal + BM25 full-text + vector similarity, all from a derived SQLite index
- The interview is the product: AI interviews the user, proposes nodes/edges, commits on approval
- Zero infrastructure: single binary, no servers, `git clone` + `tempyr index rebuild`

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

The project is a **Rust workspace** with eight crates:

| Crate | Purpose |
|-------|---------|
| `tempyr-core` | Graph data model: node/edge parsing, schema validation, in-memory graph, traversal, temporal filtering |
| `tempyr-index` | SQLite indexing: FTS5 full-text, sqlite-vec embeddings, hybrid retrieval pipeline, incremental updates |
| `tempyr-interview` | Interview state machine: session management, phase transitions, gap detection, LLM-based extraction |
| `tempyr-render` | Document rendering: TOML template parsing, graph collection, markdown output |
| `tempyr-linear` | Linear integration: push/pull sync, status mapping, context generation |
| `tempyr-journal` | Session journal: append-only JSONL of agent reasoning (decisions, dead ends, etc.) under `<git-common-dir>/tempyr/journals/` with cross-platform locking and secret redaction |
| `tempyr-cli` | CLI binary (`tempyr`): clap-based, all user-facing commands |
| `tempyr-mcp` | MCP server library used by `tempyr --mcp`: exposes graph operations as tools for Claude Code |

Crate dependency order: `core` and `journal` are leaves. `index`/`interview`/`render`/`linear` depend on `core`. `cli`/`mcp` depend on the layer above plus `journal`.

### Data Model

- **Nodes** are `.md` files with YAML frontmatter in `graph/<type>/` directories (e.g., `graph/features/feat-session-replay.md`)
- **Edges** are stored bidirectionally in YAML frontmatter — both source and target files contain the edge. Edge lists are sorted alphabetically by target.
- **Schema** (`schema.toml`) defines node types, required fields, allowed statuses, and valid edge types with reverse mappings
- **Index** (`.tempyr/index.db`) is a derived SQLite database (gitignored, rebuildable) containing structural data, FTS5, and vector embeddings

### Key Design Patterns

- **LLM is for extraction only.** Gap detection, phase transitions, duplicate checking, and graph operations are deterministic Rust code. The LLM extracts structured data from natural language — it doesn't make control-flow decisions.
- **Bidirectional edge sync.** Every `add-edge` writes both files atomically. `validate` catches drift. Edge type pairs are defined in `schema.toml` (e.g., `child_of` <-> `parent_of`).
- **Temporal edges.** Edges have optional `valid_from`/`valid_until` for point-in-time rendering. Nodes have a `status` lifecycle (e.g., `superseded`). Decisions get superseded, not deleted.
- **Content-hash embedding cache.** Embeddings are keyed by blake3 hash of the markdown body (not frontmatter). Re-embed only when body changes.
- **Token budget enforcement.** Hybrid retrieval greedily fills context by combined score until the token budget is exhausted.

### Interview Engine Flow

The interview is a 5-phase state machine: Discovery -> Product -> Technical -> Decomposition -> Review. Each phase has typed gaps (e.g., `MissingPersona`, `NoTechnicalDecision`) that drive contextual questions. Phase transitions happen when required gaps in the current phase are filled. All proposals are tentative until the user commits.

## Key Dependencies

- `rusqlite` (bundled) + `sqlite-vec` — index storage
- `serde` + `serde_yaml` + `serde_json` + `toml` — serialization
- `tokio` — async runtime (MCP server, API calls)
- `reqwest` — LLM/embedding API client
- `clap` — CLI
- `blake3` — content hashing
- `walkdir` — filesystem traversal
- `chrono` + `uuid` — timestamps and session IDs

## Task Tracking

When your work corresponds to a Tempyr task node (type `task` in `graph/tasks/`), keep it updated as you go using `graph_update_node`. This applies whether you were explicitly given a task ID, or you can identify the matching task via `graph_search` or `graph_list`.

### Status transitions

| When | Set status to |
|------|---------------|
| Starting work on the task | `in_progress` |
| Blocked by something outside your control | `blocked` |
| Work is complete (code written, tests pass) | `done` |

Update status as soon as the transition happens — not batched at the end.

### Adding context

Append implementation context to the task body at natural milestones:
- **Approach chosen**: if you made a non-obvious design decision, note it briefly
- **Key files touched**: list the primary files so reviewers know where to look
- **Blockers or surprises**: anything that deviated from the original plan

Use `graph_update_node` with the `body` field. Read the existing body first (via `graph_get_node`) and append — don't overwrite.

### Finding your task

If you're given a task description but not an ID:
1. `graph_search` with keywords from the task description
2. `graph_list` filtered to type `task` with status `backlog` or `in_progress`
3. If no matching task exists, proceed without tracking — don't create task nodes ad hoc

## Session Journal

The journal is an append-only log of agent reasoning -- decisions, dead ends, findings, plans, risks -- stored as JSONL under `<git-common-dir>/tempyr/journals/` and published as Git refs (`refs/tempyr/journals/archive/<YYYY>/<MM>/<DD>/<id>`). It runs alongside the graph: the graph is curated knowledge that outlives this session, the journal is *how that knowledge was reached* (and how it changes when you have to throw it out). Spec: `docs/journal-spec.md`.

### When to log manually

Use `journal_log` (MCP) or `tempyr journal log` (CLI) freely as you work. The eight kinds:

| Kind | When |
|------|------|
| `plan` | What you're about to attempt and why |
| `finding` | Something you learned by reading code or running a tool |
| `assumption` | Something you're acting on without verifying (`polarity` required) |
| `question` | Something you don't know yet -- to ask or look up |
| `decision` | A choice with reasoning (`chosen`, `rationale`, `reversible` required; `detail` >= 50 chars) |
| `dead_end` | An approach that didn't work (`approach`, `failure_mode` required; `detail` >= 50 chars). **Highest signal** -- future agents read these to avoid repeating you. |
| `risk` | A potential problem identified but not yet hit (`severity` recommended) |
| `outcome` | The result of a plan (`passed`, optional `commit_sha`); set `final = true` to close the session and trigger publish |

Log freely on dead ends and decisions -- the journal is empty if you don't, and a missing entry teaches no one. Successes alone are low-signal here.

### What's auto-emitted (don't double-log)

These transitions emit a journal entry automatically -- don't also call `journal_log` for them:

| Trigger | Emits |
|---------|-------|
| `tempyr status <task> in_progress` (or MCP `graph_update_node` from `backlog`) | `plan` (provisional) |
| Task `in_progress -> done` | `outcome` with `passed = true`, `final = true` |
| Task `in_progress -> blocked` | `risk` with `severity = blocker` |
| `interview start` | `plan` (provisional) |
| `interview answer` | `finding` (provisional); plus `finding` for any phase advance |
| `interview adjust`/`add_node`/`add_edge` | `finding` (provisional) |
| `interview commit` | `outcome` with `final = true` |

A failed auto-emit is downgraded to a warning, never aborts the underlying mutation.

### Searching prior reasoning

Before re-deriving something, search the journal:

- `journal_search "<query>"` -- hybrid retrieval (BM25 + vec0 RRF + recency + kind boost). Pass `--rerank` to run a BGE cross-encoder over the top 50 RRF candidates and re-sort by relevance -- better on close calls (e.g. lexically distant but semantically on-topic). Filter by `--kind dead_end` to surface "approaches that didn't work."
- `journal_get <id>` -- fetch one entry by id, transparently refreshes the index on a cache miss.
- `journal_range "<A..B>"` -- list entries written while one of the in-range commits was checked out. Pairs with `git log A..B` for "what reasoning happened during this span of work?" Accepts any range expression `git rev-list` understands (`A..B`, `HEAD~10..HEAD`, `feature..main`).
- `journal_blame <file>` -- every entry whose `files` field referenced this path. The *why* complement of `git blame`'s *who/when*: surfaces decisions, dead-ends, and findings tied to a specific file. Highest signal when the file accumulated several dead-ends before its current shape.

### Session lifecycle

Sessions are per `(worktree, agent)` pair. The first `journal_log` opens one; subsequent calls reuse it. Closing the session means writing a `.ready` marker, which the publisher (`tempyr journal flush`) archives as a Git ref and pushes to the remote.

Three ways a session ends:

1. An entry with `final = true` (your own `outcome`, or an auto-emitted task-done / interview-commit) -> marker written immediately.
2. The `SessionEnd` Claude Code hook -> runs `tempyr journal finalize`, idempotent.
3. Manual: `tempyr journal finalize` from the shell.

The `SessionStart` hook runs `tempyr journal bootstrap` to ensure the layout exists. Both hooks are no-ops outside a git repo, so opening Claude Code in a non-tempyr directory is safe.

### Diagnostics

`tempyr doctor` shows a `Journal` section: open / ready session counts, publisher lock state, stamped PID. Use it when sessions seem to be queuing up locally -- usually means the publisher hasn't run.

## Conventions

- Node IDs are human-readable kebab-case slugs (e.g., `feat-session-replay`, `decision-storage-backend`)
- Never manually rename node IDs — use `tempyr rename` which updates all references atomically
- Node granularity rule: one decision, one fact, or one concept per node. If it can't be independently linked, it's a paragraph, not a node.
- Interview extraction prompts use temperature 0.1 for structured JSON output
- The binary name is `tempyr`
