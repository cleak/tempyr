# Tempyr: Knowledge Graph for AI-Assisted Product & Technical Design

## Document Metadata

- **Author**: Caleb (Principal Graphics Engineer)
- **Created**: 2026-03-23
- **Status**: Specification — ready for implementation
- **Target implementer**: Claude Code with Rust toolchain
- **Repository language**: Rust (2021 edition, stable)

---

## 1. Executive Summary

Tempyr is a file-based knowledge graph system with hybrid retrieval (structural traversal + vector search + BM25 full-text search) that serves as both a personal/team knowledge base and a project management substrate. Documents like PRDs and TDDs are not stored artifacts — they are rendered views (queries) over the graph. The system's primary interaction model is an AI-assisted interview flow that decomposes brain dumps, conversations, and raw ideas into typed graph nodes and edges.

### Core Design Principles

1. **Files are the source of truth.** Every node is a `.md` file with YAML frontmatter. Git provides versioning, branching, and collaboration. No database is authoritative — all indices are derived and rebuildable.
2. **Documents are views, not artifacts.** A PRD is a traversal query that collects feature, persona, constraint, metric, decision, and risk nodes and assembles them using a rendering template. A TDD follows different edges from the same root.
3. **Hybrid retrieval.** Structured graph traversal for known relationships, vector similarity for semantic discovery, BM25 for exact keyword matching. Structural and FTS data live in derived SQLite indices, while embeddings are cached separately by content hash and may be shared across worktrees.
4. **The interview is the product.** The AI doesn't generate documents — it interviews the user, proposes graph nodes and edges in real time, and commits them on approval. Every answer enriches the graph.
5. **Zero infrastructure.** No servers, no Docker, no Java. `git clone` + `tempyr index rebuild` and you're running on any machine. The only external dependency at runtime is an LLM API for embeddings and the interview flow.

### Key Architectural Decisions Already Made

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Source of truth | Markdown + YAML frontmatter in git | Human-readable, diffable, portable, Claude Code native |
| Index storage | Snapshot-specific SQLite indices plus shared embedding cache | Embedded, zero-config, rebuildable from files, worktree-friendly |
| Language | Rust | Performance, single binary, author's primary language |
| Primary frontend | Claude Code via MCP server | Conversation IS the UI, no custom frontend to maintain |
| Secondary frontend | CLI (same Rust binary) | Standalone queries, scripting, CI validation |
| Edge storage | Bidirectional (both sides of edge stored in both files) | Human-readability when opening files in any editor |
| ID scheme | Human-readable slugs, CLI-managed renames | Grep-friendly; `tempyr rename` updates all references atomically |
| Embedding model | API-based (Anthropic/OpenAI) with content-hash caching | High quality; re-embed only when body content changes |
| Temporal model | `valid_from`/`valid_until` on edges, `status` lifecycle on nodes | Stolen from Graphiti; decisions get superseded, not deleted |

---

## 2. Product Specification

### 2.1 User Personas

**Primary: Solo technical founder / principal engineer (Caleb)**
- Works across product and engineering on personal and startup projects
- Uses Claude Code as primary AI coding tool with git worktrees
- Writes Rust, works on game dev (Bevy), graphics programming
- Needs to capture technical insights, product decisions, and project structure in one system
- Currently loses knowledge across chat sessions, scattered notes, and ephemeral conversations

**Secondary: Small engineering team (2-5 people)**
- Collaborating on the same git repo
- Need shared context on decisions, architecture, and requirements
- PRDs and TDDs currently live in Google Docs or Notion, disconnected from code

### 2.2 User Stories

**Graph Creation via Interview**

- As a user, I can brain-dump a product idea in natural language and the system interviews me to extract structured graph nodes covering both product requirements and technical design
- As a user, I see tentative nodes being proposed as I answer questions, and I can confirm, modify, or reject them before they're committed
- As a user, the system asks me smart questions based on what it already knows from my graph — it doesn't re-ask things it can infer from existing nodes
- As a user, I can resume an interrupted interview session and pick up where I left off

**Knowledge Capture**

- As a user, I can quickly add an insight or tip (e.g., "always use custom materials for cel shading in Bevy") and the system suggests connections to existing nodes
- As a user, I can import raw unstructured text (Slack thread, meeting notes, voice memo transcript) and the system proposes a set of nodes and edges from it

**Retrieval & Discovery**

- As a user, I can ask a natural language question and get an answer grounded in my graph, with citations to specific nodes
- As a user, I can search semantically ("how should I handle transparency in the toon renderer") and find relevant nodes even without exact keyword matches
- As a user, I can traverse the graph structurally ("show me everything connected to the toon renderer feature within 2 hops")

**Document Rendering**

- As a user, I can render a PRD from any feature node that assembles all relevant product context (personas, constraints, metrics, decisions, risks, open questions)
- As a user, I can render a TDD from any feature node that assembles all relevant technical context (components, API surfaces, architecture decisions, task decomposition)
- As a user, I can render with `--as-of <date>` to see the state at a point in time, or `--include-history` to see the full evolution

**Graph Maintenance**

- As a user, I can validate the graph for consistency errors (dangling edges, missing required fields, schema violations)
- As a user, I can detect and merge duplicate nodes
- As a user, I can rename a node and have all references updated atomically
- As a user, I can migrate the graph when the schema changes (add new node types, rename fields, etc.)

### 2.3 Success Metrics

- **Adoption**: System is used daily for at least one active project within 2 weeks of Phase 2 completion
- **Knowledge retention**: Insights captured in the graph are surfaced in relevant contexts within `graph ask` queries at least 80% of the time
- **Interview quality**: Interviews produce nodes that require <20% manual post-editing before commit
- **Render accuracy**: Rendered PRDs/TDDs contain all relevant graph context with no manually-identified omissions in 90%+ of renders

---

## 3. Technical Specification

### 3.1 Project Structure

```
tempyr/                              # Rust workspace root
├── Cargo.toml                       # Workspace manifest
├── crates/
│   ├── tempyr-core/             # Graph data model, parsing, validation
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── node.rs              # Node struct, YAML parsing
│   │   │   ├── edge.rs              # Edge struct, typed relationships
│   │   │   ├── schema.rs            # Schema loading and validation
│   │   │   ├── graph.rs             # In-memory graph representation
│   │   │   ├── traverse.rs          # Graph traversal algorithms
│   │   │   └── temporal.rs          # Temporal edge filtering
│   │   └── Cargo.toml
│   ├── tempyr-index/            # SQLite indexing (structural + FTS5 + vectors)
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── indexer.rs           # Full rebuild from files
│   │   │   ├── incremental.rs       # Incremental index updates
│   │   │   ├── fts.rs              # FTS5 full-text search
│   │   │   ├── vector.rs           # sqlite-vec embedding search
│   │   │   ├── hybrid.rs           # Combined retrieval + ranking
│   │   │   └── embeddings.rs       # Embedding API client + cache
│   │   └── Cargo.toml
│   ├── tempyr-interview/        # Interview state machine
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── session.rs          # InterviewSession struct
│   │   │   ├── phases.rs           # Phase definitions and transitions
│   │   │   ├── gaps.rs             # Gap detection engine
│   │   │   ├── proposer.rs         # Node/edge proposal from answers
│   │   │   └── brain_dump.rs       # Initial input parsing
│   │   └── Cargo.toml
│   ├── tempyr-render/           # Document rendering engine
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── template.rs         # Render template parsing
│   │   │   ├── collector.rs        # Graph traversal for rendering
│   │   │   └── formatter.rs        # Markdown output generation
│   │   └── Cargo.toml
│   ├── tempyr-cli/              # CLI binary
│   │   ├── src/
│   │   │   └── main.rs
│   │   └── Cargo.toml
│   └── tempyr-mcp/             # MCP server binary
│       ├── src/
│       │   └── main.rs
│       └── Cargo.toml
├── schema/
│   └── default-schema.toml          # Default schema shipped with the tool
└── templates/
    ├── prd.toml                     # PRD rendering template
    ├── tdd.toml                     # TDD rendering template
    └── epic-summary.toml            # Epic overview template
```

### 3.2 Graph Directory Structure (User's Project)

```
my-project/
├── .tempyr/
│   ├── config.toml                  # Project-level config (embedding model, API endpoint)
│   ├── schema.toml                  # Node types, edge types, validation rules
│   ├── render/
│   │   ├── prd.toml                 # PRD rendering template (can override defaults)
│   │   └── tdd.toml                 # TDD rendering template
│   ├── index.db                     # SQLite: nodes, edges, FTS5, vectors (GITIGNORED)
│   ├── embed-cache.json             # content-hash → embedding (GITIGNORED)
│   └── sessions/                    # Active interview sessions (GITIGNORED)
│       └── session-abc123.json
├── graph/
│   ├── epics/
│   │   └── epic-observability-v2.md
│   ├── features/
│   │   └── feat-session-replay.md
│   ├── decisions/
│   │   └── decision-storage-backend.md
│   ├── tasks/
│   │   ├── task-replay-ingestion.md
│   │   └── task-replay-viewer.md
│   ├── constraints/
│   │   └── constraint-p99-latency.md
│   ├── personas/
│   │   └── persona-platform-eng.md
│   ├── metrics/
│   │   └── metric-mttr-reduction.md
│   ├── risks/
│   │   └── risk-pii-in-replays.md
│   ├── questions/
│   │   └── question-oit-toon-renderer.md
│   ├── components/
│   │   └── comp-render-pipeline.md
│   ├── api_surfaces/
│   ├── insights/
│   │   └── tip-cel-shade-custom-material.md
│   └── notes/
└── renders/                          # Generated documents (optional, can be gitignored)
    ├── prd-session-replay.md
    └── tdd-session-replay.md
```

### 3.3 Node File Format

Every node is a Markdown file with YAML frontmatter. The frontmatter is the structured data; the body is the unstructured content.

```yaml
---
id: feat-session-replay
type: feature
status: draft                        # draft | active | completed | superseded | archived
created: 2026-03-20T14:30:00Z
updated: 2026-03-23T09:15:00Z
owner: caleb
tags: [replay, observability, q2-2026]
edges:
  - target: epic-observability-v2
    type: child_of
  - target: persona-platform-eng
    type: serves
    valid_from: 2026-03-20
  - target: constraint-p99-latency
    type: constrained_by
  - target: decision-storage-backend
    type: depends_on
  - target: metric-mttr-reduction
    type: measured_by
  - target: risk-pii-in-replays
    type: has_risk
  - target: task-replay-ingestion
    type: decomposes_to
  - target: task-replay-viewer
    type: decomposes_to
  - target: question-oit-toon-renderer
    type: has_question
---

# Session Replay for Funnel Steps

## Problem

Platform engineers currently debug funnel drop-offs by reading logs and
guessing at user behavior. They need to see what actually happened during
a session — clicks, navigation, errors — linked to specific funnel steps.

## Hypothesis

If we provide session replays linked to funnel step transitions, platform
engineers can reduce mean-time-to-resolution for conversion issues by 40%.

## Solution Overview

A recording agent captures DOM snapshots and interaction events, stored as
chunked replay data. A viewer component renders replays synchronized to
funnel step timestamps.
```

**Edge format details:**

```yaml
edges:
  - target: decision-old-approach     # the target node ID (slug)
    type: depends_on                  # must be valid per schema.toml
    valid_from: 2026-01-15            # optional: when this edge became true
    valid_until: 2026-03-01           # optional: when this edge was superseded
    annotation: "Superseded by new storage decision"  # optional: human note
```

**Bidirectional edge rule:** When an edge `A → B` of type `child_of` exists in A's frontmatter, node B must also contain a reverse edge `B → A` of type `parent_of`. The CLI writes both atomically. `tempyr validate` catches any inconsistency. Edge lists in YAML are sorted alphabetically by `target` to minimize merge conflicts.

### 3.4 Schema Definition (`schema.toml`)

```toml
[meta]
version = "1.0.0"
description = "Tempyr schema for product + technical knowledge graphs"

# ─── Node Types ──────────────────────────────────────────

[node_types.epic]
description = "A large body of work containing multiple features"
directory = "epics"
required_fields = ["status", "owner"]
allowed_statuses = ["draft", "active", "completed", "archived"]
allowed_edges = [
  { type = "parent_of", target = "feature" },
  { type = "serves", target = "persona" },
  { type = "measured_by", target = "metric" },
]

[node_types.feature]
description = "A user-facing capability or improvement"
directory = "features"
required_fields = ["status", "owner"]
allowed_statuses = ["draft", "active", "completed", "superseded", "archived"]
allowed_edges = [
  { type = "child_of", target = "epic" },
  { type = "serves", target = "persona" },
  { type = "constrained_by", target = "constraint" },
  { type = "depends_on", target = "decision" },
  { type = "depends_on", target = "feature" },
  { type = "measured_by", target = "metric" },
  { type = "has_risk", target = "risk" },
  { type = "decomposes_to", target = "task" },
  { type = "has_question", target = "open_question" },
  { type = "uses", target = "component" },
  { type = "exposes", target = "api_surface" },
  { type = "informed_by", target = "insight" },
]

[node_types.task]
description = "An implementable unit of work"
directory = "tasks"
required_fields = ["status"]
allowed_statuses = ["backlog", "in_progress", "done", "blocked", "cut"]
allowed_edges = [
  { type = "child_of", target = "feature" },
  { type = "child_of", target = "task" },        # subtasks
  { type = "blocked_by", target = "task" },
  { type = "blocked_by", target = "decision" },
  { type = "blocked_by", target = "open_question" },
  { type = "uses", target = "component" },
  { type = "has_question", target = "open_question" },
]

[node_types.decision]
description = "A technical or product decision with rationale"
directory = "decisions"
required_fields = ["status"]
allowed_statuses = ["proposed", "discussing", "decided", "superseded"]
allowed_edges = [
  { type = "decision_for", target = "feature" },
  { type = "decision_for", target = "component" },
  { type = "constrained_by", target = "constraint" },
  { type = "supersedes", target = "decision" },
  { type = "has_question", target = "open_question" },
]

[node_types.constraint]
description = "A technical, business, or regulatory constraint"
directory = "constraints"
required_fields = ["status"]
allowed_statuses = ["active", "relaxed", "removed"]
allowed_edges = [
  { type = "constrains", target = "feature" },
  { type = "constrains", target = "decision" },
  { type = "constrains", target = "component" },
]

[node_types.persona]
description = "A user type or stakeholder archetype"
directory = "personas"
required_fields = []
allowed_edges = [
  { type = "served_by", target = "feature" },
  { type = "served_by", target = "epic" },
]

[node_types.metric]
description = "A measurable success indicator"
directory = "metrics"
required_fields = ["status"]
allowed_statuses = ["proposed", "tracking", "met", "missed", "retired"]
allowed_edges = [
  { type = "measures", target = "feature" },
  { type = "measures", target = "epic" },
]

[node_types.risk]
description = "An identified risk with potential mitigations"
directory = "risks"
required_fields = ["status"]
allowed_statuses = ["identified", "mitigated", "accepted", "realized"]
allowed_edges = [
  { type = "risk_for", target = "feature" },
  { type = "mitigated_by", target = "task" },
]

[node_types.open_question]
description = "An unresolved question blocking or informing other work"
directory = "questions"
required_fields = ["status"]
allowed_statuses = ["open", "answered", "deferred", "moot"]
allowed_edges = [
  { type = "question_for", target = "feature" },
  { type = "question_for", target = "decision" },
  { type = "blocks", target = "task" },
  { type = "answered_by", target = "decision" },
]

[node_types.component]
description = "A technical system, module, or architectural element"
directory = "components"
required_fields = ["status"]
allowed_statuses = ["planned", "active", "deprecated"]
allowed_edges = [
  { type = "used_by", target = "feature" },
  { type = "used_by", target = "task" },
  { type = "depends_on", target = "component" },
  { type = "has_decision", target = "decision" },
  { type = "exposes", target = "api_surface" },
  { type = "informed_by", target = "insight" },
]

[node_types.api_surface]
description = "An API, interface, protocol, or contract between components"
directory = "api_surfaces"
required_fields = ["status"]
allowed_statuses = ["draft", "stable", "deprecated"]
allowed_edges = [
  { type = "exposed_by", target = "component" },
  { type = "exposed_by", target = "feature" },
  { type = "constrained_by", target = "constraint" },
]

[node_types.insight]
description = "A learned tip, trick, gotcha, or piece of reusable knowledge"
directory = "insights"
required_fields = []
optional_fields = ["source"]   # experience | conversation | article | docs
allowed_edges = [
  { type = "relates_to", target = "component" },
  { type = "relates_to", target = "feature" },
  { type = "relates_to", target = "decision" },
  { type = "relates_to", target = "insight" },
]

[node_types.note]
description = "A freeform note, meeting summary, or brain dump"
directory = "notes"
required_fields = []
allowed_edges = [
  { type = "relates_to", target = "*" },   # notes can link to anything
]

# ─── Edge Type Definitions ───────────────────────────────

# Each edge type has a defined reverse. When edge A->B of type X is created,
# edge B->A of reverse type Y is also created automatically.

[edge_types]
child_of = { reverse = "parent_of", description = "Hierarchical containment" }
parent_of = { reverse = "child_of" }
serves = { reverse = "served_by", description = "Delivers value to a persona" }
served_by = { reverse = "serves" }
constrained_by = { reverse = "constrains" }
constrains = { reverse = "constrained_by" }
depends_on = { reverse = "depended_on_by" }
depended_on_by = { reverse = "depends_on" }
measured_by = { reverse = "measures" }
measures = { reverse = "measured_by" }
has_risk = { reverse = "risk_for" }
risk_for = { reverse = "has_risk" }
decomposes_to = { reverse = "child_of" }
has_question = { reverse = "question_for" }
question_for = { reverse = "has_question" }
uses = { reverse = "used_by" }
used_by = { reverse = "uses" }
exposes = { reverse = "exposed_by" }
exposed_by = { reverse = "exposes" }
blocked_by = { reverse = "blocks" }
blocks = { reverse = "blocked_by" }
supersedes = { reverse = "superseded_by" }
superseded_by = { reverse = "supersedes" }
decision_for = { reverse = "has_decision" }
has_decision = { reverse = "decision_for" }
mitigated_by = { reverse = "mitigates" }
mitigates = { reverse = "mitigated_by" }
answered_by = { reverse = "answers" }
answers = { reverse = "answered_by" }
informed_by = { reverse = "informs" }
informs = { reverse = "informed_by" }
relates_to = { reverse = "relates_to", description = "Symmetric weak association" }
```

### 3.5 SQLite Index Schema

Derived indices are rebuildable cache artifacts. The structural index for a graph snapshot lives in a SQLite database, and embedding vectors may be stored in a separate shared cache keyed by content hash so identical worktrees can reuse them. Older projects may still use `.tempyr/index.db` as a legacy fallback, but it is no longer the only storage layout.

```sql
-- Structural index
CREATE TABLE nodes (
    id          TEXT PRIMARY KEY,     -- slug, e.g. "feat-session-replay"
    node_type   TEXT NOT NULL,        -- "feature", "decision", etc.
    status      TEXT,
    owner       TEXT,
    title       TEXT,                 -- extracted from first H1 in body
    body_text   TEXT,                 -- full markdown body (no frontmatter)
    file_path   TEXT NOT NULL,        -- relative to graph/ dir
    created_at  TEXT,
    updated_at  TEXT,
    tags        TEXT,                 -- JSON array
    content_hash TEXT NOT NULL        -- blake3 hash of body for cache invalidation
);

CREATE TABLE edges (
    source_id   TEXT NOT NULL,
    target_id   TEXT NOT NULL,
    edge_type   TEXT NOT NULL,
    valid_from  TEXT,                 -- ISO date, nullable
    valid_until TEXT,                 -- ISO date, nullable
    annotation  TEXT,
    PRIMARY KEY (source_id, target_id, edge_type),
    FOREIGN KEY (source_id) REFERENCES nodes(id),
    FOREIGN KEY (target_id) REFERENCES nodes(id)
);

CREATE INDEX idx_edges_target ON edges(target_id);
CREATE INDEX idx_edges_type ON edges(edge_type);
CREATE INDEX idx_nodes_type ON nodes(node_type);
CREATE INDEX idx_nodes_status ON nodes(status);

-- FTS5 full-text search index
CREATE VIRTUAL TABLE nodes_fts USING fts5(
    id,
    title,
    body_text,
    tags,
    content='nodes',
    content_rowid='rowid',
    tokenize='porter unicode61'
);

-- Legacy vector similarity search index (sqlite-vec compatibility path)
CREATE VIRTUAL TABLE nodes_vec USING vec0(
    embedding float[1536]            -- dimension matches embedding model
);

-- Legacy embedding cache inside the index DB (compatibility fallback)
CREATE TABLE embedding_cache (
    node_id      TEXT PRIMARY KEY,
    content_hash TEXT NOT NULL,
    embedding    BLOB NOT NULL        -- raw float32 bytes
);
```

### 3.6 Hybrid Retrieval Pipeline

The `tempyr context` command and the `graph_context` MCP tool execute this pipeline:

```
Input: query string + optional root node ID + token budget

Step 1: Structural retrieval (if root node provided)
  - Load root node
  - BFS traversal to depth 2
  - Score by hop distance: hop 0 = 1.0, hop 1 = 0.8, hop 2 = 0.5
  - Result: Set<(NodeId, structural_score)>

Step 2: BM25 full-text search
  - Query FTS5 index with input query
  - Take top 30 results
  - Normalize scores to 0.0..1.0
  - Result: Set<(NodeId, bm25_score)>

Step 3: Vector similarity search
  - Embed query via embedding API
  - KNN search against nodes_vec, k=30
  - Convert distances to similarity scores 0.0..1.0
  - Result: Set<(NodeId, vector_score)>

Step 4: Merge and rank
  - Union all result sets
  - For each node, compute combined score:
      combined = (structural * 0.5) + (bm25 * 0.25) + (vector * 0.25)
    (Structural gets highest weight because explicit links are the most
     reliable signal. BM25 and vector split the discovery weight.)
  - Apply recency boost: nodes updated in last 7 days get +0.1
  - Apply type priority boost: decisions +0.05, constraints +0.05
    (these are disproportionately useful as context)

Step 5: Budget enforcement
  - Sort by combined score descending
  - Greedily add nodes until token budget is reached
  - Estimate tokens per node: len(title + body) / 4
  - Default budget: 8000 tokens

Step 6: Output
  - Return ordered list of (NodeId, score, node_content) for the AI to consume
  - Include a graph_summary: counts by type, any open_questions in results
```

**Retrieval modes exposed via CLI and MCP:**

| Command | Behavior |
|---------|----------|
| `tempyr search <query>` | BM25 only (fast, keyword-exact) |
| `tempyr vsearch <query>` | Vector only (semantic similarity) |
| `tempyr context <query> [--root <id>]` | Full hybrid pipeline |
| `tempyr traverse <id> [--depth N]` | Structural only, no ranking |
| `tempyr ask <question>` | Full hybrid → feed to LLM → answer |

### 3.7 Interview Engine

The interview engine is the core product differentiator. It manages a structured conversation that produces graph nodes.

#### 3.7.1 Interview Session State

```rust
pub struct InterviewSession {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub root_type: NodeType,           // usually Feature, could be Epic
    pub root_node: TentativeNode,      // the primary node being constructed
    pub phase: InterviewPhase,
    pub tentative_nodes: Vec<TentativeNode>,
    pub tentative_edges: Vec<TentativeEdge>,
    pub answered: Vec<QAPair>,
    pub remaining_gaps: Vec<Gap>,
    pub graph_context: Vec<NodeId>,    // existing nodes pulled in as relevant
    pub token_budget_used: usize,
}

pub enum InterviewPhase {
    /// Parse initial input, query graph for related nodes, identify what exists
    Discovery,
    /// Who is this for? What problem does it solve? What does success look like?
    Product,
    /// How does this interact with existing systems? What are the technical constraints?
    /// What architectural decisions need to be made?
    Technical,
    /// What are the tasks? What depends on what? What questions are still open?
    Decomposition,
    /// Present the full tentative graph for review and approval
    Review,
}

pub struct TentativeNode {
    pub id: String,                    // proposed slug
    pub node_type: NodeType,
    pub status: String,
    pub fields: HashMap<String, String>,
    pub body: String,
    pub confidence: f32,               // how confident the system is in this node
    pub source_qa: Vec<usize>,         // indices into answered[] that produced this
}

pub struct TentativeEdge {
    pub source: String,
    pub target: String,
    pub edge_type: String,
    pub source_type: EdgeSource,       // inferred from answer, inherited from parent, etc.
}

pub enum EdgeSource {
    ExplicitFromAnswer,    // user directly stated this relationship
    InferredFromContext,   // system inferred from graph context
    InheritedFromParent,   // parent epic has this edge, likely applies here
}

pub struct Gap {
    pub gap_type: GapType,
    pub priority: GapPriority,         // required | recommended | nice_to_have
    pub node_type_needed: NodeType,    // what type of node would fill this gap
    pub context: String,               // why this gap matters for THIS feature
    pub suggested_question: String,    // contextual question, not a template
    pub phase: InterviewPhase,         // which phase this gap belongs to
}

pub enum GapType {
    MissingPersona,
    MissingSuccessMetric,
    MissingConstraint,
    MissingRisk,
    UnclearProblemStatement,
    NoTechnicalDecision { topic: String },
    MissingComponent,
    MissingDependency,
    NoTaskDecomposition,
    UnresolvedQuestion { question: String },
    MissingApiSurface,
    // etc.
}

pub struct QAPair {
    pub question: String,
    pub answer: String,
    pub phase: InterviewPhase,
    pub timestamp: DateTime<Utc>,
    pub nodes_proposed: Vec<String>,   // IDs of tentative nodes created from this
}
```

#### 3.7.2 Interview Flow

**`interview_start(brain_dump: String, root_type: NodeType)`**

1. Run hybrid retrieval against the brain dump text to find related existing nodes
2. Use LLM to extract from the brain dump:
   - A proposed title and slug for the root node
   - An initial problem statement / description
   - Any explicitly mentioned personas, constraints, or technical details
   - References to things that might match existing graph nodes
3. Create tentative root node and any obviously extractable child nodes
4. For each extracted reference, fuzzy-match against existing node titles + vector similarity to detect if it already exists in the graph. If match confidence > 0.85, propose linking to the existing node rather than creating a duplicate.
5. Run gap analysis:
   - Load schema for the root node type
   - For each `allowed_edges` entry, check if a corresponding tentative or linked node exists
   - For each missing required relationship, create a `Gap` with:
     - `priority`: required if the edge type is critical for rendering (e.g., features need at least one persona), recommended otherwise
     - `suggested_question`: generated with context from existing graph nodes, NOT a template. Example: "The observability epic targets platform engineers. Is session replay for the same audience, or is there a different persona like support engineers who'd use this for debugging?"
     - `phase`: assigned based on the gap type (personas → Product, components → Technical, etc.)
6. Sort gaps by phase, then by priority
7. Return to Claude Code:
   - The tentative graph state (nodes + edges proposed so far)
   - The related existing nodes found (with summaries)
   - The first 2-3 questions (from the highest-priority gaps in the current phase)
   - A phase indicator and progress summary

**`interview_answer(session_id: String, answer: String)`**

1. Load session state
2. Use LLM to extract structured information from the answer:
   - Prompt includes: the question that was asked, the current tentative graph state, the schema for relevant node types, and existing graph context
   - Output: proposed new nodes, proposed edges, modifications to existing tentative nodes
3. For each proposed node:
   - Check for duplicates against existing graph (fuzzy title + vector similarity)
   - If duplicate detected, propose linking instead of creating
   - Otherwise, add to `tentative_nodes` with `confidence` score
4. Re-run gap analysis with updated state
5. Check for phase transition:
   - If all `required` gaps in current phase are filled → advance to next phase
   - If phase advances, run gap analysis for the new phase (which may query different parts of the graph)
6. Return:
   - New/modified tentative nodes and edges
   - Which gaps were filled by this answer
   - Next 2-3 questions
   - Updated phase and progress

**Phase transition logic:**

```
Discovery → Product:   when root node has a body (problem statement) and at least
                       one existing graph node is linked for context

Product → Technical:   when at least one persona, one success metric, and a clear
                       problem statement exist. Constraints are recommended but
                       don't block transition.

Technical → Decomp:    when at least one component or architecture decision exists,
                       and the user has addressed the most critical technical gaps

Decomp → Review:       when at least one task exists OR the user explicitly says
                       "let's review what we have"
```

**`interview_show_tentative(session_id: String)`**

Returns the full set of tentative nodes and edges in a readable format, organized by type, with confidence indicators and source attributions.

**`interview_commit(session_id: String)`**

1. For each tentative node: write `.md` file with YAML frontmatter + body to the appropriate `graph/<type>/` directory
2. For each tentative edge: ensure both source and target files contain the edge (bidirectional writes)
3. Run `tempyr validate` on the affected subgraph
4. Update the SQLite index incrementally (don't full rebuild)
5. Delete the session file
6. Return: list of files created/modified, any validation warnings

**`interview_adjust(session_id: String, node_id: String, changes: NodePatch)`**

Allows the user (via Claude Code) to modify a tentative node before commit — change the title, edit the body, adjust edges, change status. This is how the user corrects course mid-interview.

**`interview_resume(session_id: String)`**

Loads a persisted session from `.tempyr/sessions/` and returns the current state: where we left off, what's been answered, what gaps remain, what the tentative graph looks like.

#### 3.7.3 LLM Interaction in the Interview

The interview engine makes two types of LLM calls:

**1. Brain dump parsing (in `interview_start`)**

```
System: You are a product and technical analyst. Given a brain dump from a
user about a feature or project, extract structured elements.

You have access to the following existing graph context:
{existing_nodes_summary}

The user's input:
{brain_dump}

Extract:
- A proposed title and slug (lowercase-kebab-case)
- A problem statement (2-3 sentences)
- Any mentioned personas or user types
- Any mentioned constraints (technical, business, regulatory)
- Any mentioned technical components or systems
- Any references to things that might already exist in the graph (match against
  the existing nodes listed above)

Respond in JSON.
```

**2. Answer processing (in `interview_answer`)**

```
System: You are processing an interview answer to extract graph nodes and edges.

Current graph state:
{tentative_nodes_summary}
{existing_linked_nodes_summary}

Schema for relevant node types:
{schema_excerpt}

The question that was asked:
{question}

The user's answer:
{answer}

Extract:
- New nodes to create (with type, proposed slug, status, body content)
- New edges to create (with source, target, edge_type)
- Modifications to existing tentative nodes (if the answer changes them)
- Any follow-up clarifications needed

Respond in JSON.
```

**Important: The LLM is used for extraction and natural language understanding only.** The gap detection, phase transitions, duplicate checking, and graph operations are all deterministic Rust code. This keeps the system predictable and debuggable.

### 3.8 Rendering Engine

Rendering templates are TOML files that define how to traverse the graph from a root node and assemble a document.

#### 3.8.1 Template Format

```toml
# .tempyr/render/prd.toml

[meta]
name = "Product Requirements Document"
description = "Standard PRD rendering from a feature node"
root_types = ["feature"]                # which node types can be the root
output_format = "markdown"

[[sections]]
heading = "Overview"
source = "root"                         # the root node's body
include_fields = ["status", "owner", "created", "updated"]

[[sections]]
heading = "Problem Statement"
source = "root"
body_section = "Problem"                # extract the ## Problem section from the body

[[sections]]
heading = "Target Users"
traverse = "serves"                     # follow 'serves' edges from root
target_type = "persona"
include_body = true

[[sections]]
heading = "Success Metrics"
traverse = "measured_by"
target_type = "metric"
include_body = true
include_fields = ["status"]

[[sections]]
heading = "Constraints"
traverse = "constrained_by"
target_type = "constraint"
include_body = true

[[sections]]
heading = "Key Decisions"
traverse = "depends_on"
target_type = "decision"
include_body = true
include_fields = ["status", "decided_date"]
filter = { status = ["decided", "discussing"] }  # skip superseded

[[sections]]
heading = "Risks"
traverse = "has_risk"
target_type = "risk"
include_body = true
include_fields = ["status"]

[[sections]]
heading = "Open Questions"
traverse = "has_question"
target_type = "open_question"
filter = { status = ["open"] }
include_body = true

[[sections]]
heading = "Task Decomposition"
traverse = "decomposes_to"
target_type = "task"
include_fields = ["status"]
include_body = false                    # just list tasks with status
```

```toml
# .tempyr/render/tdd.toml

[meta]
name = "Technical Design Document"
root_types = ["feature"]
output_format = "markdown"

[[sections]]
heading = "Overview"
source = "root"

[[sections]]
heading = "Technical Constraints"
traverse = "constrained_by"
target_type = "constraint"
include_body = true

[[sections]]
heading = "Architecture Decisions"
traverse = "depends_on"
target_type = "decision"
include_body = true                     # includes Options Considered, Decision, Consequences
include_fields = ["status", "decided_date"]

[[sections]]
heading = "System Components"
traverse = "uses"
target_type = "component"
include_body = true
# Also follow one more hop to get sub-components
sub_traverse = "depends_on"
sub_target_type = "component"

[[sections]]
heading = "API Surfaces"
traverse = "exposes"
target_type = "api_surface"
include_body = true

[[sections]]
heading = "Implementation Tasks"
traverse = "decomposes_to"
target_type = "task"
include_body = true
include_fields = ["status"]
# Show dependency edges between tasks
show_internal_edges = true
internal_edge_types = ["blocked_by"]

[[sections]]
heading = "Open Technical Questions"
traverse = "has_question"
target_type = "open_question"
filter = { status = ["open"] }
include_body = true

[[sections]]
heading = "Relevant Insights"
# This section uses semantic search, not traversal
source = "semantic_search"
query_from = "root"                     # use root node body as search query
target_type = "insight"
max_results = 5
min_similarity = 0.7
```

#### 3.8.2 Rendering with Temporal Filters

```bash
# Current state (default)
tempyr render prd feat-session-replay

# State as of a specific date
tempyr render prd feat-session-replay --as-of 2026-03-01

# Include historical/superseded edges annotated with validity periods
tempyr render prd feat-session-replay --include-history
```

When `--as-of` is provided, the renderer filters edges: include only edges where `valid_from <= as_of` AND (`valid_until IS NULL` OR `valid_until > as_of`). Superseded nodes (status = "superseded" with their `updated_at` before the as-of date) are excluded.

### 3.9 CLI Specification

Binary name: `tempyr`

```
USAGE:
    tempyr <COMMAND> [OPTIONS]

COMMANDS:
    # ─── Graph Operations ────────────────────
    init                    Initialize a new graph in the current directory
    validate                Check graph consistency (dangling edges, schema violations)
    add <type>              Create a new node interactively (opens $EDITOR)
    add-edge <src> <tgt> <type>   Add an edge between two nodes (writes both files)
    remove-edge <src> <tgt> <type> Remove an edge (writes both files)
    rename <old-id> <new-id>       Rename a node, updating all references
    status <id> <new-status>       Change a node's status

    # ─── Search & Retrieval ──────────────────
    search <query>          BM25 full-text search
    vsearch <query>         Vector similarity search
    context <query>         Hybrid retrieval (structural + BM25 + vector)
      --root <id>           Start structural traversal from this node
      --budget <tokens>     Max tokens of context to return (default: 8000)
    traverse <id>           Show all nodes reachable from a root
      --depth <n>           Max traversal depth (default: 2)
      --type <edge_type>    Filter by edge type
    ask <question>          Hybrid retrieval + LLM answer generation
      --root <id>           Anchor the search to a specific node

    # ─── Interview ───────────────────────────
    interview start <brain_dump>    Start a new interview session
      --type <node_type>            Root node type (default: feature)
    interview answer <session_id> <answer>  Process an answer
    interview show <session_id>     Show tentative graph state
    interview commit <session_id>   Write tentative nodes to files
    interview resume <session_id>   Resume an interrupted session
    interview list                  List active sessions

    # ─── Rendering ───────────────────────────
    render <template> <root_id>     Render a document from a root node
      --as-of <date>                Render graph state at a point in time
      --include-history             Include superseded edges/nodes
      --output <path>               Write to file (default: stdout)

    # ─── Index Management ────────────────────
    index rebuild           Full index rebuild from source files
    index update            Incremental update (changed files only)
    index stats             Show index statistics

    # ─── Maintenance ─────────────────────────
    dedupe                  Find and propose merges for duplicate nodes
    migrate <migration>     Run a schema migration
    import <file>           Import unstructured text and propose nodes

    # ─── MCP Server ──────────────────────────
    serve                   Start the MCP server (for Claude Code / other clients)
      --port <port>         TCP port (default: stdio for MCP)

OPTIONS:
    --graph-dir <path>      Path to graph directory (default: ./graph)
    --config <path>         Path to config file (default: ./.tempyr/config.toml)
    --verbose               Verbose output
    --json                  JSON output (for scripting / MCP)
```

### 3.10 MCP Server Specification

The MCP server exposes the following tools to Claude Code and other MCP clients:

```json
{
  "tools": [
    {
      "name": "graph_search",
      "description": "Full-text keyword search across all graph nodes",
      "parameters": {
        "query": "string",
        "max_results": "integer (default 10)",
        "node_type": "string (optional filter)"
      }
    },
    {
      "name": "graph_vsearch",
      "description": "Semantic vector similarity search across all graph nodes",
      "parameters": {
        "query": "string",
        "max_results": "integer (default 10)",
        "node_type": "string (optional filter)"
      }
    },
    {
      "name": "graph_context",
      "description": "Hybrid retrieval combining structural traversal, keyword search, and semantic search. Use this when you need comprehensive context about a topic.",
      "parameters": {
        "query": "string",
        "root_node": "string (optional: node ID to anchor traversal)",
        "token_budget": "integer (default 8000)"
      }
    },
    {
      "name": "graph_traverse",
      "description": "Follow edges from a node to find connected nodes",
      "parameters": {
        "node_id": "string",
        "depth": "integer (default 2)",
        "edge_types": "string[] (optional filter)"
      }
    },
    {
      "name": "graph_get_node",
      "description": "Get the full content of a specific node by ID",
      "parameters": {
        "node_id": "string"
      }
    },
    {
      "name": "graph_add_node",
      "description": "Create a new node in the graph",
      "parameters": {
        "id": "string (slug)",
        "node_type": "string",
        "status": "string",
        "body": "string (markdown content)",
        "edges": "array of {target, type}",
        "tags": "string[] (optional)",
        "owner": "string (optional)"
      }
    },
    {
      "name": "graph_add_edge",
      "description": "Add an edge between two existing nodes",
      "parameters": {
        "source": "string (node ID)",
        "target": "string (node ID)",
        "edge_type": "string",
        "annotation": "string (optional)"
      }
    },
    {
      "name": "graph_validate",
      "description": "Validate graph consistency. Returns any errors or warnings.",
      "parameters": {}
    },
    {
      "name": "graph_render",
      "description": "Render a document (PRD, TDD, etc.) from a root node",
      "parameters": {
        "template": "string (prd, tdd, epic-summary)",
        "root_node": "string (node ID)",
        "as_of": "string (ISO date, optional)",
        "include_history": "boolean (default false)"
      }
    },
    {
      "name": "graph_ask",
      "description": "Ask a question and get an answer grounded in graph context. Returns both the answer and the source nodes used.",
      "parameters": {
        "question": "string",
        "root_node": "string (optional: anchor to a specific node)"
      }
    },
    {
      "name": "interview_start",
      "description": "Start a new interview session to create graph nodes from a brain dump or idea. Returns initial context, tentative nodes, and questions to ask.",
      "parameters": {
        "brain_dump": "string",
        "root_type": "string (default: feature)"
      }
    },
    {
      "name": "interview_answer",
      "description": "Process an answer in an active interview session. Returns new/modified tentative nodes and next questions.",
      "parameters": {
        "session_id": "string",
        "answer": "string"
      }
    },
    {
      "name": "interview_show",
      "description": "Show the current tentative graph state in an active interview",
      "parameters": {
        "session_id": "string"
      }
    },
    {
      "name": "interview_commit",
      "description": "Commit all tentative nodes from an interview to the graph as files",
      "parameters": {
        "session_id": "string"
      }
    },
    {
      "name": "interview_adjust",
      "description": "Modify a tentative node before committing",
      "parameters": {
        "session_id": "string",
        "node_id": "string",
        "changes": "object (partial node fields to update)"
      }
    },
    {
      "name": "interview_resume",
      "description": "Resume an interrupted interview session",
      "parameters": {
        "session_id": "string"
      }
    },
    {
      "name": "graph_suggest_connections",
      "description": "Given a node ID, find semantically similar nodes that aren't currently linked and suggest edges",
      "parameters": {
        "node_id": "string",
        "max_suggestions": "integer (default 5)"
      }
    },
    {
      "name": "graph_stats",
      "description": "Get graph statistics: node counts by type, edge counts, open questions, coverage metrics",
      "parameters": {}
    }
  ]
}
```

### 3.11 Configuration

```toml
# .tempyr/config.toml

[general]
graph_dir = "graph"                     # relative to project root
schema_path = ".tempyr/schema.toml"

[embedding]
provider = "anthropic"                  # anthropic | openai | local
model = "voyage-3"                      # or text-embedding-3-small for openai
dimensions = 1024
batch_size = 50                         # embeddings per API call

[llm]
provider = "anthropic"
model = "claude-sonnet-4-20250514"      # for extraction tasks in interview
temperature = 0.1                       # low temp for structured extraction

[retrieval]
default_token_budget = 8000
structural_weight = 0.5
bm25_weight = 0.25
vector_weight = 0.25
recency_boost_days = 7
recency_boost_value = 0.1

[interview]
max_questions_per_turn = 3              # don't overwhelm the user
auto_advance_phases = true              # auto-transition when gaps are filled
session_timeout_hours = 168             # sessions expire after 7 days

[mcp]
transport = "stdio"                     # stdio | tcp
# tcp_port = 3000                      # only if transport = tcp
```

---

## 4. Implementation Plan

### Phase 1: Manual Validation (Week 1-2)

**Goal**: Validate the data model using real project content before writing any tooling.

**Deliverables**:
- Directory structure created manually for an existing project (e.g., the Bevy game)
- 15-20 seed nodes written by hand across at least 5 node types
- `schema.toml` with the full type system
- `CLAUDE.md` instructions that teach Claude Code how to read/write graph nodes
- Shell scripts: `validate.sh` (uses `yq` + `grep` to check dangling edges), `list-gaps.sh` (finds features missing personas/metrics)
- One manually written PRD and TDD to serve as target output for the render engine

**Validation criteria**: Can you use this daily for 1 week? Does the data model feel natural? Are there missing node types or edge types? Does the directory layout make sense?

### Phase 2: MCP Server + Core (Week 3-5)

**Goal**: Working MCP server that Claude Code can use for basic graph operations and simple interviews.

**Deliverables**:
- `tempyr-core`: Node/edge parsing, schema validation, in-memory graph construction
- `tempyr-mcp`: MCP server with tools: `graph_get_node`, `graph_add_node`, `graph_add_edge`, `graph_search` (basic grep-based before FTS5), `graph_validate`, `graph_traverse`
- Basic `interview_start` and `interview_answer` (gap detection against schema, LLM calls for extraction)
- `interview_commit` writes files
- Session persistence to JSON files

**Rust dependencies**:
- `serde`, `serde_yaml`, `serde_json` — serialization
- `serde_toml` — config/schema parsing
- `tokio` — async runtime (for MCP server + API calls)
- `reqwest` — HTTP client for LLM/embedding APIs
- `walkdir` — filesystem traversal
- `glob` — file pattern matching
- `clap` — CLI argument parsing
- `blake3` — content hashing
- `chrono` — datetime handling
- `uuid` — session IDs
- MCP server library (evaluate `mcp-server` crate or implement stdio JSON-RPC directly)

**Not in this phase**: SQLite index, embeddings, vector search, rendering engine. Search is basic file-content grep. This phase is about proving the interview flow works.

### Phase 3: Index + Retrieval (Week 6-8)

**Goal**: Full hybrid retrieval pipeline with SQLite, FTS5, and vector search.

**Deliverables**:
- `tempyr-index`: SQLite schema creation, full rebuild from files, incremental updates
- FTS5 integration for keyword search
- `sqlite-vec` integration for vector similarity
- Embedding API client with content-hash caching
- Hybrid retrieval pipeline with configurable weights
- `graph_context` and `graph_vsearch` MCP tools
- `graph_ask` tool (retrieval + LLM answer generation)
- `graph_suggest_connections` tool (vector similarity for unlinked nodes)
- `tempyr-cli`: All search and traverse commands

**Rust dependencies (additional)**:
- `rusqlite` with `bundled` feature — SQLite
- `sqlite-vec` — vector extension
- `zerocopy` — efficient vector passing to SQLite

### Phase 4: Rendering + Polish (Week 9-11)

**Goal**: Document rendering, graph maintenance tools, and production polish.

**Deliverables**:
- `tempyr-render`: Template parser, graph collector, markdown formatter
- Default templates for PRD, TDD, epic summary
- Temporal filtering (`--as-of`, `--include-history`)
- `tempyr rename` with atomic reference updates
- `tempyr dedupe` with fuzzy matching + vector similarity
- `tempyr migrate` for schema changes
- `tempyr import` for unstructured text ingestion
- Improved interview gap detection using vector search (find related insights, existing decisions)
- `graph_render` MCP tool

### Phase 5: Iteration (Ongoing)

- Custom rendering templates for specific use cases
- Graph visualization (lightweight web UI reading SQLite index, D3 or cytoscape)
- Multi-project graph federation (cross-project insights)
- Collaborative features (conflict resolution for concurrent edits)
- Performance optimization at scale (1000+ nodes)

---

## 5. Known Edge Cases & Design Decisions

These are documented so the implementer doesn't have to re-derive them.

### 5.1 Granularity Problem
**Problem**: When does something deserve its own node vs. being a section in an existing node?
**Rule**: A node represents one decision, one fact, or one concept that might be independently linked. If you can't imagine referencing it from another node without its surrounding context, it's a paragraph, not a node.
**Mitigation**: Support `contains` edge type for hierarchical nesting. Build `tempyr merge` early for when two nodes should have been one.

### 5.2 Stale Embeddings
**Problem**: Editing a node's body invalidates its embedding, but editing frontmatter doesn't.
**Solution**: Content hash is computed from the body text only (not YAML). Re-embed only when body hash changes. Do NOT embed neighbor context — accept that the embedding is an approximation and let graph structure compensate at query time by expanding results by one hop.

### 5.3 ID Stability
**Problem**: Renaming a node requires updating every file that references it.
**Solution**: `tempyr rename old-id new-id` greps all YAML frontmatter, updates edge targets, and commits as one atomic change. Forbid manual renames — always use the CLI command.

### 5.4 Bidirectional Edge Sync
**Problem**: Edges stored on both sides can drift if someone manually edits one file.
**Solution**: `tempyr validate` checks that every edge has a matching reverse edge. Edge lists are sorted alphabetically by target to minimize merge conflicts. `tempyr add-edge` always writes both files.

### 5.5 Temporal Semantics
**Problem**: When rendering, should superseded decisions be shown?
**Solution**: Default render shows current state only (edges where `valid_until IS NULL` or `valid_until > now()`). `--include-history` shows all edges with annotations. `--as-of <date>` filters to the state at that date. Superseded nodes (`status: superseded`) are excluded from default renders but included with `--include-history`.

### 5.6 Context Window Overflow
**Problem**: Hybrid retrieval can return too many nodes for the LLM context window.
**Solution**: Token budget enforcement. Default 8000 tokens. Greedily fill by combined score. Structural proximity > recency > semantic similarity. `decision` and `constraint` nodes get a type priority boost because they're disproportionately useful.

### 5.7 AI Proposal Quality
**Problem**: LLM-proposed nodes during interviews may be wrong, duplicative, or low quality.
**Solution**: All proposals are tentative until committed. Duplicate detection via fuzzy title match + vector similarity (threshold 0.85). `confidence` score on tentative nodes. `tempyr dedupe` as a periodic maintenance tool. The interview engine uses structured JSON extraction with low temperature (0.1) to minimize hallucination.

### 5.8 Schema Migration
**Problem**: Adding/renaming node types or fields requires updating existing files.
**Solution**: `tempyr migrate` command from day one. Migrations are scripts: "rename all nodes of type X to type Y", "add required field Z with default value W". Migrations modify files in place and commit.

### 5.9 Merge Conflicts
**Problem**: Two concurrent editors (or two Claude Code sessions) adding edges to the same node cause YAML merge conflicts.
**Solution**: Sort edge lists alphabetically by target. This means independent additions append to different positions in the sorted order. For remaining conflicts, `tempyr resolve` understands YAML edge list structure and can three-way merge intelligently. Not a day-one feature — address when the problem actually occurs.

### 5.10 Bootstrapping / Cold Start
**Problem**: An empty graph provides no value, and the activation energy to populate it is high.
**Solution**: `tempyr import` accepts raw text (meeting notes, Slack threads, brain dumps) and proposes a batch of nodes and edges. The interview flow itself is designed to be the primary bootstrapping mechanism — start with a brain dump, end with 10-15 nodes. Also provide a `tempyr init --seed` that creates example nodes demonstrating the schema.

---

## 6. CLAUDE.md Instructions

The following should be placed in the project's `CLAUDE.md` to instruct Claude Code on working with the graph:

```markdown
# Tempyr Knowledge Graph

This project uses Tempyr, a file-based knowledge graph system.

## Graph Location
- Graph nodes: `graph/` directory, organized by type subdirectory
- Schema: `.tempyr/schema.toml`
- Config: `.tempyr/config.toml`
- Render templates: `.tempyr/render/`

## MCP Tools Available
When the Tempyr MCP server is running, use these tools instead of
directly reading/writing files:

- `graph_search` / `graph_vsearch` / `graph_context` — find relevant nodes
- `graph_get_node` — read a specific node
- `graph_add_node` / `graph_add_edge` — create new graph elements
- `graph_traverse` — follow edges from a node
- `graph_validate` — check graph consistency
- `graph_render` — generate PRD/TDD/other documents
- `graph_ask` — answer questions grounded in graph context
- `interview_start` / `interview_answer` / `interview_commit` — guided node creation
- `graph_suggest_connections` — find unlinked but related nodes

## Node File Format
Each node is a .md file with YAML frontmatter. See schema.toml for valid
types, statuses, and edge types. Edges are bidirectional — when adding
an edge, both source and target files must be updated.

## Key Rules
1. NEVER manually edit node IDs — use `tempyr rename`
2. Always sort edge lists alphabetically by target in YAML
3. Run `tempyr validate` after manual edits
4. Use the interview flow for creating new features — it ensures
   completeness and discovers relevant existing context
5. When in doubt about node granularity: one decision, one fact, or
   one concept per node
```

---

## 7. Open Questions for Implementation

These are decisions that should be made during implementation, not before:

1. **MCP transport**: Should the MCP server use stdio (simplest, works with Claude Code directly) or TCP (supports multiple clients simultaneously)? Start with stdio, add TCP later if needed.

2. **Embedding model choice**: Anthropic's voyage-3 vs OpenAI's text-embedding-3-small vs a local model via `fastembed-rs`. Start with API-based, switch to local if latency or cost becomes an issue.

3. **Interview LLM model**: Should extraction use the same model as the user's Claude Code session (likely Opus) or a cheaper model (Sonnet)? Sonnet at temperature 0.1 is probably sufficient for structured extraction and significantly cheaper.

4. **Session persistence format**: JSON files in `.tempyr/sessions/` is the simple answer. Could also use SQLite. JSON is more debuggable during early development.

5. **Rendering output**: Should `tempyr render` write to `renders/` directory by default, or stdout? Stdout is more unix-y but less convenient. Default to file, support `--stdout`.

6. **Graph visualization**: Defer this entirely. If/when needed, a small HTML page that reads the SQLite index and renders with cytoscape.js is the minimal approach. Don't build until you know what you need to see.
