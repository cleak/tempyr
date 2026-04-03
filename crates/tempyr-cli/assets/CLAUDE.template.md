# Tempyr Knowledge Graph

This repository uses Tempyr, a file-based knowledge graph for product and technical design.

## Graph Location

- Graph nodes: `graph/`
- Schema: `.tempyr/schema.toml`
- Config: `.tempyr/config.toml`
- Render templates: `.tempyr/render/`
- Interview sessions: `.tempyr/sessions/`

## Preferred Workflow

- Prefer Tempyr MCP tools when they are available instead of editing graph files directly.
- Use the interview flow for new features, epics, and multi-node changes.
- Use direct file edits only when Tempyr tools are unavailable or insufficient.

## MCP Tools

When the Tempyr MCP server is running, prefer these tools:

- `graph_search` / `graph_vsearch` / `graph_context` to discover relevant nodes
- `graph_get_node` to read a node in full
- `graph_add_node` / `graph_add_edge` to create graph content
- `graph_update_node` to update status, body, or metadata on existing nodes
- `graph_traverse` to follow graph relationships
- `graph_validate` to check graph consistency after changes
- `graph_render` to generate PRDs, TDDs, or other views
- `graph_ask` to answer questions grounded in graph context
- `interview_start` / `interview_answer` / `interview_commit` for guided creation

## Rules

1. Never rename node IDs manually. Use `tempyr rename`.
2. Keep edge lists alphabetized by target.
3. Run `tempyr validate` after manual graph edits.
4. Prefer updating existing nodes over creating near-duplicates.
5. If a change affects retrieval quality, rebuild or update the index.

## Environment

- Embedding provider settings live in `.tempyr/config.toml`
- API keys are typically loaded from `.env.local` or `.env`
- Tempyr loads `.env.local` before `.env`
