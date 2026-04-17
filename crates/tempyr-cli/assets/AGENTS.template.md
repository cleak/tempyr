# Tempyr Knowledge Graph

This repository uses Tempyr, a file-based knowledge graph for product and technical design.

## Graph Location

- Graph nodes: `graph/<type>/*.md` (Markdown files with YAML frontmatter, e.g. `graph/features/feat-session-replay.md`)
- Schema: `.tempyr/schema.toml`
- Config: `.tempyr/config.toml`
- Render templates: `.tempyr/render/`
- Interview sessions: `.tempyr/sessions/`

## Agent Workflow

- Prefer Tempyr MCP tools over direct graph file edits whenever possible.
- Use the interview flow for new features, epics, and larger graph expansions.
- Keep changes small and validate graph consistency after writing.

## Tempyr Tools

When Tempyr MCP is available, prefer:

- `graph_search` / `graph_vsearch` / `graph_context`
- `graph_get_node`
- `graph_add_node` / `graph_add_edge`
- `graph_update_node`
- `graph_traverse`
- `graph_validate`
- `graph_render`
- `graph_ask`
- `interview_start` / `interview_answer` / `interview_commit`

## Rules

1. Never rename node IDs manually. Use `tempyr rename`.
2. Use human-readable kebab-case slugs when creating node IDs manually.
3. Store edges bidirectionally in YAML frontmatter, and keep each edge list alphabetized by target.
4. Run `tempyr validate` after manual graph edits.
5. Prefer updating existing nodes over creating near-duplicates.
6. If a change affects retrieval quality, rebuild or update the index.

## Environment

- Embedding provider settings live in `.tempyr/config.toml`
- API keys are typically loaded from Tempyr's shared Git-common-dir env (`tempyr/.env.local`), `.env.local`, `.env`, or the shell environment
- Repo-local `.env.local` overrides shared worktree defaults when both are present
- At each location, Tempyr loads `.env.local` before `.env`
