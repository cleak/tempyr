# Tempyr Journal: Append-Only Reasoning Log for AI Agents

## Document Metadata

- **Author**: Caleb (Principal Graphics Engineer)
- **Created**: 2026-04-28
- **Status**: Specification — Phases 1–4 implemented; cross-encoder rerank shipped post-Phase 4. Tracking remaining v2 backlog items inline.
- **Scope**: The `tempyr-journal` crate, the `tempyr journal *` CLI surface, and the `journal_log` MCP tool. Does **not** cover the knowledge graph (see `graphspec.md`) or the interview engine (see `tempyr-interview-spec.md`).
- **Repository language**: Rust (2024 edition, stable)

---

## 1. Executive Summary

The Tempyr **journal** is an append-only log of agent reasoning, captured during a coding session and committed as Git refs so it survives across worktrees, machines, and agent invocations. Entries fall into eight categorical kinds (decisions, dead-ends, findings, plans, etc.) and are stored as JSONL while live, then archived as parent-less commits under `refs/tempyr/journals/archive/<YYYY>/<MM>/<DD>/<session_id>` once finalized.

Two design rules govern the whole subsystem:

1. **Survive worktree abandonment.** Journal storage lives in the *primary* repo's `.git/` (resolved via `git rev-parse --git-common-dir`), not in a worktree's gitfile-style `.git`. Committing to refs means `git gc` won't reclaim the data; pushing means another machine can fetch it.
2. **No external dependencies at runtime.** The journal subsystem is git-only at the storage layer — no daemon, no API to call. Phase 3 added a *derived* SQLite + FTS5 + sqlite-vec index for search, but it sits next to the storage layer (rebuildable from git refs and the open JSONL files); the journal itself is still git. Agents write entries; a single in-process tokio ticker (or the `tempyr journal flush` CLI) commits and pushes them.

### Why a separate journal at all?

The graph (PRD/TDD/task nodes) captures the *current* state of a project. The journal captures the *history of reasoning that produced it*: which approaches failed, which alternatives were rejected, which assumptions a future agent should question. Graph nodes are revised; journal entries are immutable. A future agent searching `journal_search "auth middleware"` finds the dead-end someone hit two weeks ago and avoids repeating it — something `graph_search` can't surface because the graph stores the resolution, not the path.

### Phases

| Phase | Status | Scope |
|---|---|---|
| **1. Capture** | ✅ Shipped (PR #20) | JSONL writer, redaction, session lifecycle, `journal_log` MCP tool, `tempyr journal log` CLI |
| **2. Publish** | ✅ Shipped (PRs #22, #23, #24) | Publisher pipeline (commit + push + cleanup), in-process tokio ticker, lockfile coordination, `[journal]` config, `tempyr journal flush`/`status`/`logs`/`fetch`, init wizard with public-repo detection, pack-refs cadence, multi-machine sync |
| **3. Search** | ✅ Shipped (PRs #26, #27, #28) | SQLite + FTS5 + sqlite-vec index, hybrid retrieval (BM25 + vector + RRF + recency + kind boost), optional BGE-Reranker cross-encoder pass, `journal_search` and `journal_get` MCP tools, `tempyr journal search`/`show`/`sessions`/`tail`/`index` CLIs |
| **4. Polish** | ✅ Shipped (PRs #29, #30, #31, #32) | Auto-emit on task status transitions (4a) and interview lifecycle (4b), `.claude/settings.json` SessionStart/SessionEnd hooks + `tempyr journal bootstrap`/`finalize` (4c), MCP annotations across all tools (4c), `tempyr doctor` journal section (4c), CLAUDE.md/AGENTS.md journal section (4d) |

---

## 2. Storage Architecture

### Filesystem layout

All paths are relative to `<git-common-dir>` (the primary repo's `.git/`, even when invoked from a worktree).

```text
<git-common-dir>/
  tempyr/
    journals/
      open/                                   # in-flight session files
        <session-id>.jsonl                    # append-only entries
        <session-id>.meta.json                # one-time session facts
        <session-id>.ready                    # marker: "publisher may commit"
      publisher.lock                          # try-lock; held while publisher runs
      state.json                              # last push, last error, totals
      publisher.log                           # rotating event log (5 MB / .log + .log.1)
```

Once the publisher commits a finalized session, the three `open/` files are removed and the data lives only in the Git ref.

### Git refs

```text
refs/tempyr/journals/
  archive/<YYYY>/<MM>/<DD>/<session_id>       # one parent-less commit per session
                                              # tree contains entries.jsonl + meta.json
```

Refs are **parent-less commits** so the archive doesn't grow a chain that pulls extra objects when fetched. The date hierarchy keeps `git for-each-ref refs/tempyr/journals/` enumerable when a project accumulates thousands of sessions; `git pack-refs --all` runs on the publisher every N pushes (configurable, default 50) to consolidate loose refs.

### Why git refs?

| Property | Why journals need it |
|---|---|
| Survives `git gc` | Entries are valuable months later; pruning loose objects would be data loss |
| Cross-worktree shared | Branching off `feat-x` shouldn't fork the journal |
| Atomic per-ref | `update-ref` is a single transactional rename |
| Cheap to push/fetch | Refs ride existing transport; no new server |
| Browseable | `git log refs/tempyr/journals/archive/2026/04/27/...` works out of the box |

### Why JSONL while live?

JSONL is append-friendly with a single `write_all` per line, lets concurrent writers interleave without coordination beyond a flock, and survives arbitrary content (newlines, control chars, unicode) inside JSON-encoded strings. Once finalized, the JSONL is hashed into a blob and the file is deleted.

### Worktree path normalization

`worktree_hash` is the first 8 hex chars of `blake3(canonical_path)`, where `canonical_path` is the worktree root after `Path::canonicalize` and (on Windows only) `to_ascii_lowercase`. This gives a stable per-worktree identifier that survives different relative-path quirks but distinguishes sibling worktrees.

---

## 3. Entry Schema

The schema version (`v: 1`) ships in every entry; readers ignore unknown fields for forward compatibility. Structured per-kind fields are validated at write time, not by serde, so the JSONL is forward-compatible across kind additions.

### Common fields (all kinds)

| Field | Type | Required | Notes |
|---|---|---|---|
| `v` | u32 | yes | Schema version, currently `1` |
| `id` | string | yes | `j-<uuid_v4>` |
| `ts` | RFC3339 datetime | yes | UTC wall-clock at write time |
| `agent` | string | yes | Default `"claude"`; identifies which agent wrote this |
| `kind` | enum | yes | One of `plan` / `finding` / `assumption` / `question` / `decision` / `dead_end` / `risk` / `outcome` |
| `summary` | string | yes | 20–200 chars after trim |
| `detail` | string | optional* | Required for `decision` and `dead_end` (50+ chars after trim) |
| `tags` | string[] | optional | Free-form labels |
| `files` | string[] | optional | Repo-relative paths (forward slashes, even on Windows) |
| `references` | string[] | optional | One-way links to graph node IDs |
| `session_id` | string | yes | `<YYYYMMDD>-<wt_hash>-<HHMMSS>` |
| `worktree_hash` | string | yes | 8 hex chars |
| `branch` | string | optional | Captured from `git rev-parse --abbrev-ref HEAD` |
| `head` | string | optional | Captured from `git rev-parse HEAD` |
| `cwd` | string | optional | Repo-relative; absent when cwd equals worktree root |
| `provisional` | bool | optional | Defaults false; filterable at search time |
| `confidence` | enum | optional | `low` / `medium` / `high` |
| `severity` | enum | optional | `info` / `warn` / `high` / `blocker` |
| `final` | bool | optional | When true, finalizes the session (triggers publish) |

### Per-kind structured fields

| Kind | Required structured fields | Other constraints |
|---|---|---|
| `plan` | — | summary 20–200 chars |
| `finding` | — | summary 20–200 chars |
| `assumption` | `polarity` (positive/negative/unknown) | — |
| `question` | — | — |
| `decision` | `chosen`, `rationale`, `reversible: bool`; alternatives optional | detail 50+ chars |
| `dead_end` | `approach`, `failure_mode`; `next_to_try` optional | detail 50+ chars |
| `risk` | — | `severity` recommended |
| `outcome` | — | `passed` / `build_ok` / `commit_sha` optional; `final = true` triggers publish |

### Why eight kinds?

The taxonomy was tuned to match the observed structure of agent reasoning, not to be exhaustive. Three principles drove the cut:

- **`assumption` is distinct from `finding`.** Most dead-ends trace back to unstated assumptions. Forcing the agent to label "assuming X" vs. "verified X" is a cheap nudge toward better reasoning.
- **`dead_end` is the highest-value kind.** A future agent searching the journal benefits most from "tried X, failed because Y" — that's the entry that prevents replays. Required `approach` + `failure_mode` fields keep the entry useful, not just narrative.
- **`outcome` doubles as session terminator.** Setting `final = true` on an outcome triggers publish, so the writer doesn't need a separate "session-end" event.

Tags carry the long tail (`"tool"` for tool quirks, `"perf"`, `"flaky"`, etc.) without inflating the kind enum.

### Redaction

Every entry passes through the default `Redactor` before append. Two-layered detectors:

1. **Named regex rules** (gitleaks-style):
   - `anthropic_or_openai_key` — `\bsk-(?:ant-)?[A-Za-z0-9_-]{20,}\b`
   - `github_pat` — `\bgh[pousr]_[A-Za-z0-9]{36,}\b`
   - `slack_token` — `\bxox[abprs]-[A-Za-z0-9-]{10,}\b`
   - `aws_access_key` — `\bAKIA[0-9A-Z]{16}\b`
   - `bearer_token`, `jwt`, `private_key_block`, `db_url_with_password`, `user_home_path`

2. **Shannon entropy fallback** for runs of `[A-Za-z0-9_+/=-]` ≥ 24 chars with entropy ≥ 4.5 bits — catches credentials without a known format.

Default mode is `Block`: a match on any user-controllable field (`summary`, `detail`, `tags`, `files`, `cwd`, and the per-kind structured strings) rejects the write with `JournalError::Redacted`. Alternative modes are `Redact` (rewrite the value, replacing matches with `<REDACTED:rule_name>`) and `Warn` (let through but log).

The redactor is deliberately conservative — false positives are recoverable (caller retries with sanitized input), false negatives are not.

---

## 4. Session Lifecycle

A *session* is one continuous span of agent activity on a single worktree by a single agent. Multiple `tempyr journal log` calls from the same agent within a few minutes group into one session; an outcome with `final = true` closes it.

### Session ID format

`YYYYMMDD-<wt_hash>-HHMMSS` — strictly regex-validated as `^\d{8}-[0-9a-f]{8}-\d{6}$` to defend against path injection (the ID flows into filesystem paths and Git ref names).

The `YYYYMMDD` prefix lets the publisher derive the archive date hierarchy without parsing JSON. The `wt_hash` distinguishes sibling worktrees of the same repo. The `HHMMSS` second-precision is a deliberate trade-off: collision risk per worktree per second is negligible, and a strict format simplifies validation. Two agents racing within the same second on the same worktree produce the same ID; the second caller errors with `JournalError::AgentMismatch` and the live `Session::open` API retries until the clock advances.

### Open / resume / finalize

```rust
// Reuse an active (non-finalized) session for this (worktree, agent) pair,
// or open a fresh one. Prevents per-CLI-invocation session sprawl.
let session = Session::open_or_resume(common_dir, worktree_top, "claude")?;
```

- `find_active` scans `open/` for `.meta.json` files matching `(worktree_hash, agent)` whose `.ready` marker is absent, returning the newest.
- `open_at` writes the `meta.json` sidecar via atomic `hard_link` (write tmp → fsync → hard_link → either won or `AlreadyExists`). Different-agent collisions on the same `(worktree, second)` slot return `AgentMismatch`.
- `finalize` is idempotent: it touches `<id>.ready`. The publisher owns ready sessions thereafter.

### Append atomicity

`append(session, entry)` does the following under one exclusive `File::lock` on the JSONL:

1. Validate per-kind required fields and length constraints.
2. Check `session.is_ready()`. The `.ready` marker is the **finalized-and-handed-off** signal: if it exists, the publisher owns the session and may commit/cleanup at any moment, so refuse with `InvalidEntry("session is finalized; refuse to append")`.
3. Serialize the entry to a single buffer with a trailing newline.
4. `write_all` then `sync_data`.
5. If `entry.is_final`, **create** the `.ready` marker via `session.finalize()` (same `touch` semantics as `finalize` itself — idempotent, leaves the marker in place if it already exists). The publisher is the only actor that ever **removes** the marker, and only after a successful commit + push (or `--no-push` cleanup); see [§5](#5-publisher-pipeline).

Holding the lock across all five steps prevents concurrent writers from slipping an append between this entry and the marker, or writing to a session the publisher has already taken ownership of. The `read(true)` flag on the OpenOptions is required for `File::lock` on Windows (rust-lang/rust#54118) — a Unix-only flock pattern would silently fail there.

There's a lower-level `append_validated(jsonl_path, entry)` escape hatch used by the publisher / future indexer that bypasses the session-finalized check.

---

## 5. Publisher Pipeline

The publisher takes finalized sessions in `open/` and turns them into pushed Git refs. Per ready session:

1. `git hash-object -w --stdin` writes `<id>.jsonl` as a blob.
2. `git hash-object -w --stdin` writes `<id>.meta.json` as a blob (required — agent name, branch, HEAD captured there are load-bearing for the future search index).
3. `git mktree` builds a tree containing both blobs (entries sorted: `entries.jsonl` < `meta.json`).
4. `git commit-tree <tree> -m "tempyr journal: <session_id>"` (parent-less).
5. `git update-ref refs/tempyr/journals/archive/<YYYY>/<MM>/<DD>/<session_id> <commit>`.
6. (Unless `--no-push`) `git push --quiet <remote> <ref>:<ref>`.
7. Cleanup: delete `<id>.jsonl`, `<id>.meta.json`, `<id>.ready` in that order. **`.ready` is removed last** so a partial cleanup leaves the session retriable.

### Idempotency on crash

If a prior run died between step 5 and step 6, re-running the publisher sees the ref already exists (`git rev-parse --verify --quiet`), skips steps 1–5, and resumes from push. Same if it died between step 6 and step 7: cleanup is `remove_file` with `NotFound` ignored, so a partial cleanup re-completes safely.

### Phase tracking

Each session's `publish_one` returns `OneOutcome { progress, result }`. The caller records milestones as they happen — `record_commit` when `progress.fresh_commit`, `record_push_ok` when `progress.pushed` — so `state.json` reflects "commit landed but push failed" instead of attributing every error to push. Phase classification on error: `push` (committed but didn't push), `cleanup` (push succeeded, cleanup failed), or `commit` (failed before commit). Only `push` failures bump `push_failures_total`; other phases stamp `last_error` directly.

### Single-publisher coordination

`<journals>/publisher.lock` is held via `std::fs::File::try_lock` for the duration of `publish_ready_sessions`. A second invocation (e.g. `tempyr journal flush` from CI while the in-process ticker is running) returns `AlreadyRunning` cleanly without waiting. The lockfile content is a stamped PID for diagnostics — informational only, never trusted for liveness decisions (the OS reclaims the lock if the process dies).

### Hardening of git invocations

Every git subprocess runs with:

- `GIT_TERMINAL_PROMPT=0` — no credential prompt can wedge the publisher
- 30s timeout (configurable via `[journal] push_timeout_secs`); if exceeded, kill the child, drain readers, return error
- stdout/stderr piped to helper threads to avoid >64 KB pipe-fill deadlocks
- `current_dir` set explicitly; `GIT_DIR` and `GIT_WORK_TREE` cleared to avoid env contamination

### Pack-refs cadence

After every `pack_refs_every_n_pushes` successful pushes (default 50, 0 disables), run `git pack-refs --all`. Without this, each archived session leaves a loose ref under `refs/tempyr/journals/archive/...` and `git for-each-ref` slows down. Boundary detection uses `pushes_at_start / N < state.pushes_total / N` so a multi-push run that crosses the threshold still triggers exactly one pack.

### In-process ticker

When the long-running MCP server (`tempyr --mcp`) hosts an agent, a tokio task spawned after the project anchor settles wakes every `tick_secs` (default 60) and calls `publish_ready_sessions` via `spawn_blocking`. On the `ShutdownCoordinator`'s cancellation token (stdin EOF or parent process exit), the loop breaks and runs **one final flush** before returning — finalized sessions don't strand on disk until the next agent invocation.

The ticker silently no-ops if the project root isn't a git repo (`SpawnOutcome::NotAGitRepo`). It surfaces config-load errors as `Unavailable(msg)` rather than falling back to defaults, so a malformed `.tempyr/config.toml` can't silently re-enable auto-publish for a user who set `enabled = false`.

---

## 6. Operational Surface

### `state.json`

Sticky publisher state, atomically written via temp-and-rename:

```json
{
  "last_push_ok_utc": "2026-04-28T19:21:18Z",
  "last_error": { "ts_utc": "...", "op": "push", "message": "auth failed" },
  "commits_total": 42,
  "pushes_total": 41,
  "push_failures_total": 1
}
```

Read by `tempyr journal status` and consulted by external tools (no JSON-RPC; `state.json` is the contract).

### `publisher.log`

Append-only structured event log, rotated when it exceeds 5 MB:

```json
{"ts":"2026-04-28T19:21:17Z","level":"info","event":"publish_started","fields":{"scanned":1,"dry_run":false,"push":true}}
{"ts":"2026-04-28T19:21:18Z","level":"info","event":"publish_finished","fields":{"failed":0,"published":1,"scanned":1}}
```

One JSON object per line. Rotation: rename to `publisher.log.1`, dropping any prior `.1`. We keep one history slice — agents read `tempyr journal logs` for current state, not historical analysis.

### CLI surface (Phases 1+2)

| Command | Purpose |
|---|---|
| `tempyr journal log <kind> <summary> [...]` | Append one entry. Per-kind flags (`--chosen`, `--rationale`, etc.) carry structured fields. `--final` finalizes the session. |
| `tempyr journal flush [--dry-run] [--no-push] [--remote NAME]` | Run the publisher pipeline once. `--remote` overrides `[journal] remote`. Exits non-zero on any failure. |
| `tempyr journal status [--json]` | Show open/ready counts, last push, last error, totals, publisher running flag. |
| `tempyr journal logs [--lines N] [--json]` | Tail `publisher.log`. Pretty mode formats `<ts> LEVEL event {fields}`. |
| `tempyr journal fetch [--remote NAME]` | `git fetch <remote> +refs/tempyr/journals/*:refs/tempyr/journals/*`. Required for multi-machine sync. |

### MCP tool surface (Phases 1+2)

| Tool | Purpose |
|---|---|
| `journal_log` | Same shape as `tempyr journal log`. The MCP server caches the active session per `(common_dir, worktree_top, agent_id)` so repeated calls within an agent loop don't sprawl into multiple sessions. |

---

## 7. Configuration

`[journal]` section in `.tempyr/config.toml`. All fields optional; missing fields fall back to defaults. Existing projects without this section keep working unchanged.

```toml
[journal]
enabled = true                  # master switch; off = no auto-publish + no ticker
remote = "origin"               # for push and fetch
tick_secs = 60                  # in-process ticker cadence
pack_refs_every_n_pushes = 50   # 0 disables `git pack-refs --all`
push_timeout_secs = 30          # per-git-op timeout
```

### `tempyr init` integration

On a fresh init in a git repo, `tempyr init` does three things:

1. Detects origin visibility via `gh repo view <owner>/<repo> --json visibility -q .visibility`. Maps `PUBLIC` → `Visibility::Public`, `PRIVATE`/`INTERNAL` → `Visibility::Private`, else `Undetermined`.
2. Writes `[journal]` to `config.toml` with `enabled = true` for private/undetermined repos, `enabled = false` for public (with a clear summary line explaining how to flip it).
3. Adds `+refs/tempyr/journals/*:refs/tempyr/journals/*` to `remote.origin.fetch` via `git config --add` (idempotent — checks `git config --get-all` first). After this, a regular `git fetch origin` mirrors journal refs from another machine without needing `tempyr journal fetch`.

`gh` is the only visibility backend. We deliberately don't fall back to unauthenticated GitHub API calls — that would pull `reqwest` into the CLI for one-shot use. If `gh` is missing, visibility is Undetermined and we ship `enabled = true` with a summary line suggesting installation.

All init sub-steps are non-fatal. Any failure becomes a warning line in the init summary; project setup continues.

---

## 8. Multi-Machine Sync

Journals committed on machine A appear on machine B in two ways:

1. **Auto**: if `remote.origin.fetch` has `+refs/tempyr/journals/*:refs/tempyr/journals/*` (added at `tempyr init` time), a regular `git fetch origin` pulls journal refs.
2. **Manual**: `tempyr journal fetch [--remote NAME]` runs the explicit refspec without depending on the per-remote config.

The leading `+` allows forced updates so a republished session (rare but possible — e.g. after a remote-side history rewrite) doesn't wedge the fetch.

GitHub's web UI doesn't render arbitrary refs under `refs/tempyr/journals/*`, so journal refs are invisible from the PR view. That's intentional — the journal is for agents and CLI tools, not human browsing. A `tempyr journal pr` digest the user can paste into a PR description is tracked in the v2 backlog ("PR description block"); not shipped yet.

---

## 9. Roadmap

### Phase 3: Search and Retrieval (shipped — PRs #26, #27, #28)

The journal becomes useful when agents can find old reasoning. Phase 3 built a derived search index next to the existing graph index. Sub-slices and the artifacts they produced:

#### Slice 3a — Index foundation

- `<git-common-dir>/tempyr/journals/index.db` — derived SQLite (rebuildable; lives inside `.git/`, so it's not part of repository content and never tracked). Schema:
  - `entries(id, session_id, ts, agent, kind, summary, detail, body_hash, ...)` — one row per entry
  - `entries_fts` — FTS5 virtual table mirroring `summary` and `detail`
  - `entry_tags(entry_id, tag)`, `entry_files(entry_id, path)`, `entry_refs(entry_id, node_id)` — junction tables
  - `entry_embeddings(entry_id, model, dim, vec)` via sqlite-vec — content-hash keyed
  - `meta(session_id, agent, branch, head, ...)` — joined from session meta.json
- Indexer reads both:
  - `<journals>/open/*.jsonl` (live, in-flight)
  - `git for-each-ref refs/tempyr/journals/archive/*` followed by `git cat-file blob <ref>:entries.jsonl` (committed, archived)
- Incremental: track last-seen ref SHAs and JSONL byte offsets in a `meta` table. Re-run is idempotent — content-hash dedup means re-indexing the same data is a no-op.
- New CLI: `tempyr journal index [--rebuild]` triggers a refresh.
- New MCP tool: `journal_get(id)` for full-entry retrieval by ID (no search yet).

#### Slice 3b — Hybrid retrieval

- Embedding pipeline: fastembed BGE-small as default (zero-config, runs locally after one-time ONNX download). OpenAI/Voyage optional. Embed-on-flush with 5s debounce, filtered to `decision`/`finding`/`dead_end`/`outcome` kinds (the rest don't carry enough semantic content to be worth the embedding cost).
- Embedding cache outside `index.db` so a reindex doesn't re-call any provider.
- Retrieval pipeline:
  - BM25 query against `entries_fts`
  - Vector query against `entry_embeddings` via sqlite-vec cosine similarity
  - RRF fusion (k=60) blends the two ranked lists
  - Recency boost: exponential decay with 14d half-life, additive in fused-rank space
  - Kind boost: `decision`/`dead_end` weighted higher than `plan`/`finding`/`question`
  - **Optional cross-encoder rerank** (opt-in via `--rerank` / `rerank: true`): the top 50 RRF candidates are re-scored by the BGE-Reranker-base cross-encoder and re-sorted; the rerank score replaces the sort key while the other components stay populated for `--explain` inspection. Off by default because the model is ~280 MB and adds ~200 ms inference per query. Falls back transparently to the unreranked RRF order on model-load or inference failure.
  - Deduplication by `(summary, kind)` hash
  - Token-budget greedy fill: order by combined score, truncate `detail` to fit
- New MCP tools: `journal_search(query, ...)` with `--explain` score breakdown and a `rerank: bool` parameter.
- New CLIs: `tempyr journal search` (with `--rerank`), `show <id>`, `sessions`, `tail`.

### Phase 4: Auto-Emit, Hooks, and Polish (shipped — PRs #29, #30, #31, #32)

With search shipped, the journal became useful when agents queried it. Phase 4 closed the loop by making sure entries get *written* without an agent having to call `journal_log` explicitly, and tightened the surface around the existing tool set.

- Auto-emit on task status transitions (CLI `tempyr status` and MCP `graph_update_node`) — **implemented in slice 4a**:
  - `backlog → in_progress` → emit `plan` (provisional)
  - `in_progress → done` → emit `outcome` with `passed = true` and `final = true`
  - `in_progress → blocked` → emit `risk` with `severity = blocker`
  - Implementation lives in [`tempyr_journal::auto_emit`](../crates/tempyr-journal/src/auto_emit.rs); both call sites treat write failures as soft warnings, never aborting the underlying status change.
- Auto-emit on interview lifecycle: start, answer, adjust, phase, commit, rollback. All provisional until session commit. **Implemented in slice 4b** for the five operations that exist today (start / answer / phase / adjust / commit); rollback is deferred until the interview engine grows a corresponding operation. Implementation lives in [`tempyr_journal::auto_emit::interview`](../crates/tempyr-journal/src/auto_emit/interview.rs); both call sites treat write failures as soft warnings, never aborting the underlying interview operation.
- `.claude/settings.json` template with `SessionStart`/`SessionEnd` hooks invoking `tempyr journal bootstrap` and `tempyr journal finalize`. **Implemented in slice 4c.** The hooks are part of the managed `.claude/settings.json` written by `tempyr init` / `tempyr update`. `bootstrap` ensures the journal layout exists (idempotent); `finalize` writes the `.ready` marker on the active session for the (worktree, agent) pair, leaving `tempyr journal flush` to actually push to the remote. Both commands silently no-op outside a git repository so a Claude session opened in a non-tempyr directory doesn't fail.
- MCP annotations (`read_only`/`destructive`/`idempotent`/`open_world`) across all existing tempyr tools (orthogonal but worth bundling). **Implemented in slice 4c.** All 27 tools tagged with explicit hints — read-only queries (`graph_search`, `graph_get_node`, `system_doctor`, etc.), destructive mutations (`graph_update_node`, `interview_commit`, `linear_pull`/`sync`), and `open_world_hint = true` for the Linear bridge tools.
- README "Session journal" section + mirror to `CLAUDE.md` and `AGENTS.md`. **Implemented in slice 4d.** The repo uses `CLAUDE.md` / `AGENTS.md` as its agent-facing entry-point docs (no separate README), so the section lives in both — covering when to log manually, what's auto-emitted (the 4a/4b transitions), how to search prior reasoning, the session lifecycle (manual finalize / `final = true` / `SessionEnd` hook), and the `tempyr doctor` diagnostics.
- `tempyr doctor` extension to surface journal-related health checks (lockfile orphaned, state.json corrupt, etc.). **Implemented in slice 4c.** Both `tempyr doctor` and the `system_doctor` MCP tool now emit a journal section: open / ready session counts, publisher lock state, and the stamped publisher PID. The probe is best-effort — read errors during the scan surface as a `probe error` line rather than failing the whole report.

### Beyond Phase 4: v2 Backlog

Tracked but not committed. Listed roughly in expected priority order:

- **HTML viewer** — `tempyr journal serve` opens a local axum SPA for browsing sessions.
<!-- Cross-encoder reranking implemented post-Phase 4; folded into the
     canonical Slice 3b retrieval pipeline above. -->

- ~~**Range queries**~~ — *implemented post-Phase 4.* `tempyr journal range A..B` and the `journal_range` MCP tool expand the range expression via `git rev-list` and return entries whose HEAD-at-write-time fell inside it. Output shape mirrors `journal search` (same `SearchHit` struct, same token-budget pass) so consumers can render either uniformly.
- ~~**Path-scoped queries**~~ — *implemented post-Phase 4.* `tempyr journal blame <file>` and the `journal_blame` MCP tool surface every entry whose `files` field referenced the given path, ordered by recency with kind boost. The *why* complement of `git blame`. Reuses the existing `entry_files(entry_id, path)` table — no schema change needed.
- **PR description block** — `tempyr journal pr` generates a markdown summary suitable for paste-into-PR.
- **Session expansion in search** — when a result is in a session with related entries, surface the session summary inline.
- **Stats dashboard** — `tempyr journal stats` shows kind distribution, dead-end rate, top tags by week.
- ~~**Pre-commit lint**~~ — *implemented post-Phase 4.* `tempyr journal lint` flags every task with `status = in_progress` that has no journal entries referencing it. Wired as a managed `pre-commit` git hook in warn-only mode so it never blocks a commit; CI can opt into `--strict` to make warnings fatal. Required parameterizing the existing managed-hooks framework (previously hard-coded for index-warmup hooks) to support per-hook bodies — `post-checkout`/`post-merge` keep their old behavior, `pre-commit` runs `journal lint`.
- **Encrypted journals** — opt-in symmetric encryption with a per-repo key for sensitive projects.
- **Live secret verification** — TruffleHog-style API checks on top of regex matches to reduce redaction false positives.

---

## 10. Known Edge Cases & Design Decisions

### Why orphan commits, not a journal branch?

Two alternatives were considered:

- **Single branch** (`refs/heads/tempyr-journal`) with a chain of commits, one per session. Rejected: commits would all be reachable from the branch tip, but pushing a single branch ref means one machine's state perpetually overwrites another's unless they fast-forward; cross-machine merge would require manual reconciliation.
- **Separate orphan branch per agent**. Rejected: agents share a session model already (the worktree-and-agent pair). Branches would force per-agent reasoning to live in silos that can't easily be merged.

Parent-less commits per session under a date-hierarchy ref namespace gave us both atomic per-session pushes (no merge conflicts) and a flat, cheap-to-fetch shape.

### Why second-precision session IDs?

Same-second collisions on the same `(worktree, agent)` pair are the boring common case (rapid CLI invocations) and resolved by `find_active` returning the existing session. Same-second cross-agent collisions are vanishingly rare; the `AgentMismatch` retry in `Session::open` advances the clock past them. Sub-second precision would tighten the ID format without measurably reducing collision risk.

### Why is the publisher single-process?

Multi-publisher would require a much heavier coordination protocol: leader election, ref-namespace partitioning, conflict resolution on `update-ref`. The expected workload is a few sessions per agent per hour — a single publisher per repo, with its lockfile gate, comfortably handles 100x that. If a deployment outgrows it, the `archive/<YYYY>/<MM>/<DD>` namespace can be partitioned on agent name without changing the on-disk format.

### Why no SQLite in Phase 1+2?

The capture and publish layers don't need a query interface — entries are written and pushed, never read except by humans browsing refs. Phase 3 introduced SQLite specifically for the search use case, where FTS5 + sqlite-vec deliver hybrid retrieval without external infrastructure. Building it in earlier would have added boot-time cost (db migrations, schema versioning) for no immediate benefit.

### Why `gh` for visibility, not the GitHub API?

The visibility check is one-shot at init time and Undetermined is a recoverable outcome (the user can flip `enabled` manually). Adding `reqwest` to `tempyr-cli` would carry an HTTP/TLS stack into a binary that doesn't otherwise need it. Most users on agent setups already have `gh`. If a real user reports the gap, the API fallback is small.

### Path normalization on Windows

Windows is case-insensitive, so two strings pointing at the same directory may differ in case. `worktree_hash` lowercases the canonicalized path *before* hashing on Windows only — preserving case on Linux/macOS where it's significant. Without this, `C:\Projects\Foo` and `c:\projects\foo` would hash to different worktrees and split journals.

### Locked file readability

On Windows, exclusive byte-range locks (`File::lock`) prevent reads from a *different* file handle. The `stamped_pid` helper in the lockfile module returns `None` when the file is currently locked, since opening for read fails. The PID stamp is for diagnostics, never for liveness, so the degraded behavior is acceptable.

---

## 11. Open Questions (Phase 3 — historical)

These were the open questions captured before slice 3a shipped. Kept here for the trade-off context they encode; the **resolution** column records what the implementation actually picked.

1. **Embedding model migration**: when fastembed adds a newer/better default, do we re-embed everything or version the embeddings table by `(model, dim)` and let queries fall back to the best available? — *Resolution: deferred. The shipped index keys embeddings by `(body_hash, model)` in the cache so a model swap re-embeds rather than version-falling-back; no production model swap has happened yet.*
2. **Cross-repo search**: if a user has multiple tempyr-initialized projects, should `journal_search` search across all of them or just the current one? — *Resolution: current repo only; `--scope all` not implemented and lives in the v2 backlog.*
3. **Provisional filtering**: `--exclude-provisional` is the obvious flag, but should `journal_search` *exclude* provisional by default and require `--include-provisional`? — *Resolution: include-by-default. The auto-emit slices (4a/4b) write a lot of provisional entries; excluding them by default would hide most lifecycle context.*
4. **PII redaction at search time**: should the search index re-apply the redactor on read in case the rules tightened since write? — *Resolution: not implemented. Redaction runs at write time only; tightening rules requires re-flushing affected sessions.*
5. **Auto-emit volume**: per-task and per-interview-action emissions could 10× the entry count. Do we deduplicate at write time (skip if last entry for this `(kind, ref)` was identical) or at search time? — *Resolution: neither yet. The auto-emit transitions are bounded (≤1 per status change, ≤1 per interview event), so the predicted 10× growth hasn't materialized. Search-time dedup by `(summary_normalized, kind)` happens regardless.*

---

## 12. References

- `crates/tempyr-journal/` — implementation
- `crates/tempyr-cli/src/commands/journal_cmd.rs` — CLI surface
- `crates/tempyr-cli/src/commands/journal_init.rs` — init wizard helpers
- `crates/tempyr-mcp/src/journal_ticker.rs` — in-process tokio ticker
- `crates/tempyr-mcp/src/handler.rs` — MCP tool surface (search for `journal_log`)
- PRs that built this: [#20](https://github.com/cleak/tempyr/pull/20) (capture), [#22](https://github.com/cleak/tempyr/pull/22) (publish), [#23](https://github.com/cleak/tempyr/pull/23) (CLIs + ticker), [#24](https://github.com/cleak/tempyr/pull/24) (config + init), [#26](https://github.com/cleak/tempyr/pull/26) (slice 3a index foundation), [#27](https://github.com/cleak/tempyr/pull/27) (slice 3b1 BM25 search), [#28](https://github.com/cleak/tempyr/pull/28) (slice 3b2 hybrid retrieval), [#29](https://github.com/cleak/tempyr/pull/29) (slice 4a task auto-emit), [#30](https://github.com/cleak/tempyr/pull/30) (slice 4b interview auto-emit), [#31](https://github.com/cleak/tempyr/pull/31) (slice 4c hooks/annotations/doctor), [#32](https://github.com/cleak/tempyr/pull/32) (slice 4d docs)
