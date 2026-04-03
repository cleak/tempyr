# Tempyr Interview Engine: Technical Specification

## Document Metadata

- **Author**: Caleb
- **Created**: 2026-03-23
- **Status**: Specification — ready for implementation
- **Parent spec**: `graphspec.md` (Tempyr core system)
- **Scope**: The guided Q&A system that interviews users to produce graph nodes

---

## 1. Architecture Overview

The interview engine is distributed across four Claude Code extension points, each handling the layer it's best suited for:

```
┌──────────────────────────────────────────────────────────────┐
│  User (in Claude Code terminal)                              │
│  "I want to add session replay to the observability platform"│
└────────────────────┬─────────────────────────────────────────┘
                     │
┌────────────────────▼─────────────────────────────────────────┐
│  Claude Code (main agent)                                    │
│  ├── Reads CLAUDE.md (project context, concise rules)        │
│  ├── Activates interview skill (guides conversation flow)    │
│  ├── Calls MCP tools (graph operations, interview state)     │
│  └── Spawns subagent for extraction (heavy LLM work)         │
└────────────────────┬─────────────────────────────────────────┘
                     │
┌────────────────────▼─────────────────────────────────────────┐
│  Tempyr MCP Server (Rust binary, deterministic)              │
│  ├── Interview session state machine                         │
│  ├── Gap detection engine (schema-driven)                    │
│  ├── Graph search / traversal / validation                   │
│  ├── Node file I/O                                           │
│  └── Embedding + index queries                               │
└──────────────────────────────────────────────────────────────┘
```

### Why this split

| Concern | Handled by | Rationale |
|---------|-----------|-----------|
| Conversation flow, tone, phrasing | Claude Code main agent | LLMs are best at natural conversation |
| When to ask what, gap priorities | MCP server (Rust) | Deterministic, testable, debuggable |
| Structured extraction from answers | Subagent (isolated context) | Prevents context pollution in main conversation |
| Graph I/O, validation, indexing | MCP server (Rust) | Must be 100% reliable, no LLM in the loop |
| Post-commit validation | Hook (deterministic) | Must run every time, no exceptions |
| Interview guidance for Claude | Skill (on-demand) | Loaded only during interviews, not every session |

### Design principles from current best practices (March 2026)

1. **CLAUDE.md stays under 50 project-specific instructions.** Claude Code's system prompt already consumes ~50 instruction slots. The interview logic lives in the skill and MCP server, not CLAUDE.md.

2. **Skills for guidance, MCP for actions, hooks for guarantees.** Skills are advisory (Claude follows ~80% of the time). MCP tool calls are deterministic. Hooks are 100% guaranteed execution. Match the mechanism to the reliability requirement.

3. **Progressive disclosure everywhere.** The skill loads only during interviews. The MCP server returns only the context needed for the current question. The subagent processes one answer at a time in isolated context.

4. **Fix the context, not the conversation.** When the interview produces bad results, update the skill or the gap detection logic — don't re-prompt.

---

## 2. Claude Code Configuration

### 2.1 CLAUDE.md (project root)

Keep this minimal. Only rules that apply to ALL sessions, not just interviews.

```markdown
# Tempyr Knowledge Graph

## Project
Tempyr is a file-based knowledge graph with hybrid retrieval.
Graph nodes: `graph/` directory, organized by type subdirectory.
Schema: `.tempyr/schema.toml` — defines valid node types, edges, fields.

## MCP Server
The Tempyr MCP server provides graph operations. Always prefer MCP tools
over direct file manipulation for graph nodes:
- `graph_search` / `graph_vsearch` / `graph_context` for finding nodes
- `graph_add_node` / `graph_add_edge` for mutations
- `graph_validate` after any graph changes
- `interview_*` tools for guided node creation

## Critical Rules
- NEVER manually edit node IDs — use `tempyr rename`
- Edge lists in YAML frontmatter MUST be sorted alphabetically by target
- Run `graph_validate` after any manual file edits to graph/ directory
- When creating features or epics, USE the interview flow — it ensures
  completeness and discovers relevant existing context
- When compacting, preserve: current interview session state, list of
  tentative nodes, and active gap list
```

That's ~15 instructions. Lean enough to be reliably followed alongside Claude Code's system prompt.

### 2.2 Interview Skill

File: `.claude/skills/tempyr-interview/SKILL.md`

```yaml
---
name: tempyr-interview
description: >
  Guides the user through creating graph nodes via structured interview.
  Activate when the user wants to: add a feature, create an epic, plan a
  project, capture requirements, do a brain dump, or create a PRD/TDD.
  Keywords: interview, new feature, brain dump, PRD, TDD, requirements.
allowed-tools:
  - mcp__tempyr__interview_start
  - mcp__tempyr__interview_answer
  - mcp__tempyr__interview_show
  - mcp__tempyr__interview_commit
  - mcp__tempyr__interview_adjust
  - mcp__tempyr__interview_resume
  - mcp__tempyr__graph_search
  - mcp__tempyr__graph_context
  - mcp__tempyr__graph_traverse
  - mcp__tempyr__graph_get_node
---
```

````markdown
# Tempyr Interview Skill

You are conducting a structured interview to create knowledge graph nodes.
The MCP server handles state, gap detection, and node proposals. Your job
is the CONVERSATION — phrasing questions naturally, handling ambiguity,
and presenting proposals clearly.

## Core behavior

### Starting an interview

When the user describes something they want to build/plan/capture:
1. Call `interview_start` with their input as `brain_dump`
2. The server returns: tentative nodes, existing context, initial gaps
3. Present what the server found in the existing graph FIRST
4. Show the tentative nodes it created from the brain dump
5. Ask the first 2-3 questions from the gap list

### Processing answers

When the user answers a question (or gives additional context):
1. Call `interview_answer` with their response
2. The server returns: new/modified nodes, filled gaps, next gaps
3. Show what was created/linked (compact format, not full YAML)
4. Ask next 2-3 questions

### How to present tentative nodes

Use this compact format — NOT full YAML:

```
Here's what I've added:
+ constraint: P99 replay load < 2s (from your latency requirement)
+ decision: separate ingestion pipeline (status: proposed)
→ linked to: comp-event-pipeline (existing), constraint-p99-latency (existing)
```

### How to phrase questions

The MCP server returns structured gap descriptions, NOT pre-written questions.
You must phrase them naturally using the context provided.

Server returns:
```json
{
  "gap_type": "missing_success_metric",
  "priority": "required",
  "context": "Feature has no success metrics. Parent epic uses MTTR reduction.",
  "existing_related": ["metric-mttr-reduction"],
  "question_type": "open",
  "suggested_angle": "Ask whether epic-level metric applies or needs its own"
}
```

You generate something like:
"The observability epic tracks MTTR reduction — does that cover session replay
too, or does this feature need its own success metric? Maybe something like
replay adoption rate or time-to-diagnosis?"

### Question rules

- NEVER ask more than 3 questions per turn
- NEVER ask questions the graph already answers — check `existing_related`
- For CLOSED gaps (yes/no, confirm/deny): phrase as confirmation
- For OPEN gaps (describe, explain): ask one focused question
- For FORCED-CHOICE gaps (pick an option): present 2-3 concrete options
- For IMPLICATION gaps (surface what user hasn't considered): frame as
  "have you thought about X?" with a specific number or consequence

### Phase transitions

The server manages phases internally. When a phase transition occurs,
the server response includes `phase_changed: true`. Acknowledge the
shift naturally:

"Good — I have a clear picture of who this is for and what success looks
like. Let me ask about the technical side now."

Do NOT announce phase names ("entering Technical phase"). The user
experiences a conversation, not a state machine.

### Handling tangents and corrections

If the user:
- Brings up something off-topic: still call `interview_answer` — the
  server will extract relevant nodes and ignore the rest
- Wants to correct something: call `interview_adjust` with the changes
- Wants to skip ahead: call `interview_answer` with "user wants to
  skip to technical/decomposition/review"
- Dumps a wall of text: still call `interview_answer` — the server's
  extraction handles multi-topic answers

### Review phase

When the server returns `phase: review`:
1. Call `interview_show` to get the full tentative state
2. Present a structured summary organized by node type
3. Ask: "Anything to add, change, or remove before I commit?"
4. On approval, call `interview_commit`
5. Mention that they can now run `tempyr render prd <id>` or
   `tempyr render tdd <id>` to see document views

### When NOT to interview

If the user just wants to quickly add a note or insight:
- Use `graph_add_node` directly, skip the interview
- The interview is for features, epics, and multi-node creation
````

### 2.3 Extraction Subagent

File: `.claude/agents/tempyr-extractor.md`

```yaml
---
name: tempyr-extractor
description: >
  Extracts structured graph nodes from natural language input.
  Used by the interview skill for processing brain dumps and answers.
model: claude-opus-4-6
skills:
  - tempyr-extraction-schema
allowed-tools:
  - mcp__tempyr__graph_search
  - mcp__tempyr__graph_vsearch
---
```

````markdown
# Tempyr Extraction Agent

You extract structured information from natural language to create
knowledge graph nodes. You receive context about the current interview
state and the user's answer, and return structured JSON.

## Input format

You receive:
- The question that was asked (or "brain_dump" for initial input)
- The user's answer text
- Current tentative graph state (existing proposed nodes)
- Relevant existing graph nodes (from search)
- The schema excerpt for valid node types and edge types

## Output format

Return ONLY valid JSON, no markdown fences, no preamble:

```json
{
  "new_nodes": [
    {
      "id": "constraint-p99-latency",
      "node_type": "constraint",
      "status": "active",
      "title": "P99 Replay Latency Under 2 Seconds",
      "body": "Session replay playback must load within 2 seconds at P99...",
      "confidence": 0.9
    }
  ],
  "new_edges": [
    {
      "source": "feat-session-replay",
      "target": "constraint-p99-latency",
      "edge_type": "constrained_by",
      "source_type": "explicit"
    }
  ],
  "modified_nodes": [
    {
      "id": "feat-session-replay",
      "body_append": "\n## Latency Requirements\nPlayback must be under 2s at P99."
    }
  ],
  "potential_duplicates": [
    {
      "proposed_id": "persona-sre",
      "existing_id": "persona-platform-eng",
      "similarity_reason": "Both describe on-call engineers focused on reliability"
    }
  ]
}
```

## Rules

- Generate slugs as lowercase-kebab-case with type prefix
- Confidence scores: 0.9+ for explicitly stated facts, 0.6-0.8 for
  inferences, below 0.6 for guesses (include anyway, flag them)
- Check potential_duplicates by comparing proposed titles against the
  existing nodes provided in context
- source_type on edges: "explicit" if user stated the relationship,
  "inferred" if you derived it, "inherited" if parent node has it
- Body content should be concise prose, not YAML or structured data
- If the answer doesn't contain extractable graph content (e.g., "yes"
  or "sounds good"), return empty arrays
````

### 2.4 Validation Hook

File: `.claude/settings.json` (relevant excerpt)

```json
{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "mcp__tempyr__graph_add_node|mcp__tempyr__graph_add_edge|mcp__tempyr__interview_commit",
        "hooks": [
          {
            "type": "command",
            "command": "tempyr validate --json --quiet"
          }
        ]
      }
    ]
  }
}
```

This ensures graph validation runs after every mutation — node creation, edge addition, or interview commit. Deterministic, 100% execution, no LLM involvement.

---

## 3. MCP Server Interview Tools — Detailed Specification

### 3.1 `interview_start`

**Purpose**: Initialize a new interview session from a brain dump or idea description.

**Parameters**:
```
brain_dump: string     # The user's raw input
root_type: string      # "feature" (default), "epic", "component"
```

**Internal flow**:

```
1. Generate session ID (UUIDv4)

2. SEARCH existing graph for related context
   a. Hybrid search using brain_dump text (BM25 + vector, top 20)
   b. Extract potential entity references from brain_dump
      (capitalized phrases, quoted terms, kebab-case identifiers)
   c. For each reference, exact-match against existing node IDs and titles
   d. Collect all matched existing nodes as graph_context

3. EXTRACT structured content from brain_dump
   a. Call the extraction subagent with:
      - brain_dump text
      - matched existing nodes (summaries only, not full bodies)
      - schema excerpt for root_type
   b. Receive: proposed nodes, edges, potential duplicates

4. DEDUPLICATE proposed nodes
   For each proposed node:
   a. Fuzzy match title against all existing node titles (Levenshtein < 3)
   b. If vector index exists, cosine similarity against existing embeddings
   c. If similarity > 0.85, flag as potential duplicate — propose linking
      to existing node instead of creating new one

5. CREATE tentative root node
   a. Generate slug from title (lowercase, kebab-case, type-prefixed)
   b. Set status to "draft"
   c. Add edges to matched existing nodes

6. RUN gap analysis (see §3.6)
   a. Load schema for root_type
   b. For each allowed_edge type, check if a tentative or existing link covers it
   c. For missing required relationships, create Gap entries
   d. For missing recommended relationships, create lower-priority Gaps
   e. Assign each Gap to the appropriate interview phase

7. PERSIST session to .tempyr/sessions/<session_id>.json

8. RETURN to Claude Code:
   {
     session_id: string,
     root_node: TentativeNode,
     tentative_nodes: TentativeNode[],
     tentative_edges: TentativeEdge[],
     graph_context: ExistingNodeSummary[],  // id, title, type, 1-line summary
     potential_duplicates: DuplicateCandidate[],
     gaps: Gap[],                           // sorted by phase then priority
     next_questions: Gap[3],                // top 3 from current phase
     phase: "discovery" | "product" | ...,
     progress: { filled: number, total: number, percentage: number }
   }
```

### 3.2 `interview_answer`

**Purpose**: Process a user's answer, update session state, advance the interview.

**Parameters**:
```
session_id: string
answer: string
```

**Internal flow**:

```
1. LOAD session from .tempyr/sessions/<session_id>.json

2. EXTRACT structured content from answer
   a. Call extraction subagent with:
      - The Gap(s) that prompted this answer (the questions asked)
      - The answer text
      - Current tentative nodes (summaries)
      - Relevant existing nodes from graph_context
      - Schema excerpt for relevant node types
   b. Receive: new nodes, edges, modifications, potential duplicates

3. DEDUPLICATE (same as interview_start step 4)

4. MERGE into session state
   a. Add new tentative nodes
   b. Add new tentative edges
   c. Apply modifications to existing tentative nodes
   d. Record QAPair: { question gaps, answer text, nodes produced, timestamp }

5. UPDATE gap analysis
   a. Mark gaps as filled if a corresponding node/edge now exists
   b. Re-run gap analysis with updated state (new gaps may emerge)
   c. Check phase transition conditions (see §3.5)

6. PERSIST updated session

7. RETURN:
   {
     new_nodes: TentativeNode[],       // created from this answer
     modified_nodes: TentativeNode[],  // changed by this answer
     new_edges: TentativeEdge[],
     filled_gaps: Gap[],               // gaps resolved by this answer
     potential_duplicates: DuplicateCandidate[],
     next_questions: Gap[3],           // top 3 remaining from current phase
     phase: string,
     phase_changed: boolean,           // true if phase just advanced
     progress: { filled, total, percentage }
   }
```

### 3.3 `interview_show`

**Purpose**: Return the full tentative graph state for review.

**Parameters**:
```
session_id: string
```

**Returns**:
```
{
  root_node: TentativeNode,
  tentative_nodes_by_type: {
    "feature": TentativeNode[],
    "persona": TentativeNode[],
    "metric": TentativeNode[],
    ...
  },
  tentative_edges: TentativeEdge[],
  linked_existing_nodes: ExistingNodeSummary[],
  remaining_gaps: Gap[],
  qa_history: QAPair[],
  phase: string,
  progress: { filled, total, percentage }
}
```

### 3.4 `interview_commit`

**Purpose**: Write all tentative nodes as files and update the graph.

**Parameters**:
```
session_id: string
```

**Internal flow**:

```
1. LOAD session
2. For each tentative node:
   a. Generate YAML frontmatter from node fields + edges
   b. Sort edge list alphabetically by target
   c. Write .md file to graph/<type>/<id>.md
   d. For each edge, write the reverse edge to the target file
      (create reverse edge entry, re-sort target's edge list, rewrite)
3. Run graph_validate on all affected files
4. Update SQLite index incrementally (if index exists)
5. Delete session file from .tempyr/sessions/
6. RETURN:
   {
     files_created: string[],    // paths relative to graph/
     files_modified: string[],   // existing files that got reverse edges
     validation_warnings: string[],
     node_count: number,
     edge_count: number
   }
```

### 3.5 Phase Transition Logic

Phases advance automatically when conditions are met. The user can also force advancement by saying "skip to technical" or "let's review."

```
DISCOVERY → PRODUCT
  Conditions (ALL required):
    - Root node has a body with at least 2 sentences
    - At least 1 existing graph node is linked as context
  Fallback: If no existing context found after 2 turns, advance anyway
    (the graph is probably empty / this is a new domain)

PRODUCT → TECHNICAL
  Conditions (ALL required):
    - At least 1 persona linked (existing or tentative)
    - At least 1 success metric exists (existing or tentative)
    - Root node body includes a problem statement
  Optional but tracked:
    - Constraints (recommended, don't block)
    - Risks (recommended, don't block)

TECHNICAL → DECOMPOSITION
  Conditions (ANY sufficient):
    - At least 1 component or API surface linked
    - At least 1 technical decision exists
    - User explicitly says "let's break this into tasks"
  Fallback: If user answers 3+ technical questions, advance
    (some features don't have complex architecture)

DECOMPOSITION → REVIEW
  Conditions (ANY sufficient):
    - At least 1 task exists
    - User says "let's review" / "that's everything" / "commit"
  Fallback: Auto-advance after 2 turns in this phase
    (task decomposition can always be refined later)
```

### 3.6 Gap Detection Engine

The gap detector is the brain of the interview. It examines the schema, the current tentative graph, and existing linked nodes to determine what's missing.

**Gap structure**:
```rust
pub struct Gap {
    pub id: String,                    // unique gap identifier
    pub gap_type: GapType,
    pub priority: GapPriority,         // required | recommended | optional
    pub phase: InterviewPhase,
    pub node_type_needed: Option<String>,
    pub edge_type_needed: Option<String>,
    pub context: String,               // WHY this gap matters, with specifics
    pub existing_related: Vec<String>, // IDs of nodes that might fill this gap
    pub question_type: QuestionType,   // closed | open | forced_choice | implication
    pub suggested_angle: String,       // hint for Claude on how to approach
    pub filled: bool,
    pub filled_by: Option<String>,     // QAPair or node ID that filled it
}
```

**Gap types and their question mappings**:

```
GAP TYPE                  PHASE          PRIORITY     QUESTION TYPE
─────────────────────────────────────────────────────────────────
missing_persona           product        required     closed (if parent has one)
                                                      open (if no candidates)
missing_success_metric    product        required     open
unclear_problem           product        required     open
missing_constraint        product        recommended  implication
missing_risk              product        recommended  implication
no_technical_decision     technical      recommended  forced_choice
missing_component         technical      recommended  open
missing_dependency        technical      optional     closed
missing_api_surface       technical      optional     open
no_task_decomposition     decomposition  required     open
unresolved_question       any            required     open
missing_owner             any            optional     closed
```

**Gap generation algorithm**:

```
FOR each allowed_edge in schema[root_type].allowed_edges:
  target_type = allowed_edge.target
  edge_type = allowed_edge.type

  # Check if this relationship is covered
  covered = false
  FOR each tentative_edge in session.tentative_edges:
    IF tentative_edge.edge_type == edge_type:
      covered = true
      BREAK
  FOR each existing_edge in graph_context_edges:
    IF existing_edge.edge_type == edge_type:
      covered = true
      BREAK

  IF NOT covered:
    # Determine priority based on target type
    priority = MATCH target_type:
      "persona" => required (in product phase)
      "metric" => required (in product phase)
      "constraint" => recommended
      "risk" => recommended
      "decision" => recommended (in technical phase)
      "task" => required (in decomposition phase)
      _ => optional

    # Find existing nodes that COULD fill this gap
    candidates = search_existing_graph(type=target_type, related_to=root_node)

    # Determine question type
    question_type = MATCH:
      candidates.len() == 1 => closed  ("Is X the right persona here?")
      candidates.len() > 1  => forced_choice ("Which of these: X, Y, Z?")
      priority == recommended => implication ("Have you considered...?")
      _ => open ("What does success look like?")

    # Build context string with specifics
    context = build_context_string(root_node, target_type, candidates)

    CREATE Gap { ... }

# ALSO check for content-level gaps in the root node body
IF root_node.body does not contain "problem" OR "why":
  CREATE Gap { gap_type: unclear_problem, ... }
IF root_node.body.len() < 100 characters:
  CREATE Gap { gap_type: insufficient_detail, ... }
```

**Context-aware question angle generation**:

The `suggested_angle` field is NOT generated by an LLM — it's constructed deterministically from the gap context:

```rust
fn build_suggested_angle(gap: &Gap, session: &InterviewSession) -> String {
    match gap.gap_type {
        GapType::MissingPersona => {
            if let Some(parent_personas) = get_parent_personas(session) {
                format!(
                    "Parent {} serves {}. Ask if same audience or different.",
                    session.root_node.parent_id(),
                    parent_personas.join(", ")
                )
            } else {
                "Ask who will use this and what their primary goal is.".into()
            }
        }
        GapType::MissingConstraint => {
            // Implication-style: surface something the user may not have considered
            let node_body = &session.root_node.body;
            if node_body.contains("data") || node_body.contains("storage") {
                "User mentioned data. Ask about volume/cost/retention constraints.".into()
            } else if node_body.contains("latency") || node_body.contains("performance") {
                "User mentioned performance. Ask for specific P99/throughput targets.".into()
            } else {
                "Ask about technical, business, or regulatory constraints.".into()
            }
        }
        GapType::NoTechnicalDecision { ref topic } => {
            let existing_decisions = get_related_decisions(session, topic);
            if !existing_decisions.is_empty() {
                format!(
                    "Related decisions exist: {}. Ask if they apply or need a new decision.",
                    existing_decisions.join(", ")
                )
            } else {
                format!("No existing decisions for '{}'. Present 2-3 options.", topic)
            }
        }
        // ... etc
    }
}
```

---

## 4. Conversation Flow Examples

### 4.1 Happy path — feature with existing context

```
USER: "I want to add session replay to the observability platform"

[Claude calls interview_start(brain_dump="I want to add session replay
to the observability platform", root_type="feature")]

[Server returns: found epic-observability-v2, persona-platform-eng,
existing decision-storage-backend. Created tentative feat-session-replay.
Gaps: missing_success_metric (required), missing_constraint (recommended)]

CLAUDE: "I found your existing observability epic — it targets platform
engineers and already has an MTTR reduction metric. I've linked this
new feature to that epic. Here's what I've started:

+ feature: Session replay for observability (draft)
→ linked to: epic-observability-v2, persona-platform-eng (both existing)

Two things I need: Does the epic's MTTR reduction metric cover this
feature, or does session replay need its own success measure? And do you
have specific performance targets — like how fast a replay should load?"

USER: "MTTR is the right metric but we should also track adoption.
Replays should load under 2 seconds."

[Claude calls interview_answer(session_id, answer)]

[Server returns: created metric-replay-adoption, constraint-p99-latency.
Gaps filled: missing_success_metric, missing_constraint.
Phase changed: product → technical]

CLAUDE: "Got it. Here's what I've added:

+ metric: Replay adoption rate (status: proposed)
+ constraint: P99 replay load < 2s
→ linked: metric-mttr-reduction (existing, inherited from epic)

Good — I have a clear picture of the product side. Now the technical
side: you have an existing event ingestion pipeline for metrics. Does
session replay piggyback on that, or does it need its own data path?"
```

### 4.2 Cold start — empty graph

```
USER: "I'm building a co-op desert survival game in Bevy"

[Server returns: no existing context found. Created tentative
feat-desert-survival. Many gaps across all phases.]

CLAUDE: "Starting fresh — no existing nodes in the graph yet.
I've created the initial feature node. Here's what I have:

+ feature: Co-op desert survival game (draft)

I need to understand the basics first. Who's the target player —
is this for hardcore survival fans, casual co-op players, or
something else? And in one sentence, what's the core experience
you want players to have?"
```

### 4.3 User goes off-rails

```
CLAUDE: "What technical components does the replay system need?"

USER: "Oh wait, I just realized we also need to handle PII.
Some of our users are in the EU and session replays might capture
personal data in form fields. This could be a GDPR issue."

[Claude calls interview_answer with the PII/GDPR response]

[Server extracts: risk-pii-in-replays, constraint-gdpr-compliance.
Returns these as new nodes, continues with technical gaps.]

CLAUDE: "Good catch — that's important. I've captured it:

+ risk: PII in session replays (status: identified)
+ constraint: GDPR compliance for EU users

Those are linked to the feature. Now back to the technical side —
what components does the replay system need?"
```

### 4.4 User wants to skip ahead

```
CLAUDE: "Who is the target user for this feature?"

USER: "Can we skip the product stuff? I already know what I'm
building, I just need to get the technical design documented."

[Claude calls interview_answer with "user wants to skip to technical"]

[Server force-advances to technical phase, marks product gaps as
deferred, adds a meta-gap: "product_gaps_deferred"]

CLAUDE: "No problem — I'll mark the product questions as deferred.
You can come back to them later with `tempyr interview refine`.
Let's talk architecture. What's the main technical challenge here?"
```

---

## 5. Progressive Disclosure Strategy

### 5.1 Context loading for interview

The interview skill uses progressive disclosure at every level:

**Session start**: The MCP server returns `ExistingNodeSummary[]` — just id, title, type, and a 1-line summary. NOT full node bodies. Claude references these in conversation. If it needs the full body of a specific node, it calls `graph_get_node` on demand.

**Gap presentation**: Only the top 3 gaps are returned per turn as `next_questions`. The full gap list is available via `interview_show` but never loaded unprompted.

**Tentative node display**: Claude shows the compact `+ type: title` format, not YAML frontmatter. Full details only on `interview_show` during review.

**Extraction context**: The subagent receives only the relevant slice of graph context for the current answer — not the entire tentative state. The MCP server pre-filters to nodes within 2 hops of the discussed topic.

### 5.2 Context management across long interviews

A 15-turn interview can consume significant context. Mitigations:

1. **Session state lives in the MCP server**, not Claude's context. If the conversation is compacted, no interview state is lost — Claude just calls `interview_show` to reload.

2. **The CLAUDE.md includes a compaction instruction**: "When compacting, preserve: current interview session ID, active phase, and last 2 QA pairs."

3. **Each `interview_answer` response is self-contained** — it includes enough context for Claude to continue without referencing earlier turns. The `next_questions` include `context` and `existing_related` so Claude doesn't need to remember what was said 10 turns ago.

4. **The extraction subagent runs in isolated context** — its work doesn't accumulate in the main conversation.

---

## 6. Error Handling and Recovery

### 6.1 Extraction failures

If the extraction subagent returns invalid JSON or errors:
- The MCP server returns the answer as-is with `extraction_failed: true`
- Claude asks the user to rephrase or be more specific
- The gap that prompted the question remains unfilled

### 6.2 Session recovery

If a Claude Code session ends mid-interview:
- Session state is persisted to disk after every `interview_answer`
- User runs `tempyr interview list` to see active sessions
- `interview_resume` returns the full current state
- Claude picks up naturally: "Looks like we were working on session
  replay — we'd covered the product side and were starting on
  technical design. Want to continue from there?"

### 6.3 Commit failures

If `interview_commit` fails (e.g., file write error, validation failure):
- No partial writes — commit is atomic (write all or none)
- Session is NOT deleted — user can fix and retry
- Validation errors are returned with specific file paths and issues

### 6.4 Graph conflicts

If between `interview_start` and `interview_commit`, someone else has
modified a node that the interview wants to link to:
- The commit detects the conflict via file modification timestamps
- Returns the conflict details
- Claude presents: "The persona-platform-eng node was modified since
  we started. Want me to re-read it and check if the edge still makes
  sense?"

---

## 7. Testing Strategy

### 7.1 Gap detection unit tests

The gap detector is deterministic Rust code. Test exhaustively:

```rust
#[test]
fn test_feature_missing_persona_creates_gap() {
    let schema = load_test_schema();
    let session = create_session_with_root("feature");
    // No persona linked
    let gaps = detect_gaps(&schema, &session);
    assert!(gaps.iter().any(|g| g.gap_type == GapType::MissingPersona));
    assert_eq!(gaps[0].priority, GapPriority::Required);
    assert_eq!(gaps[0].phase, InterviewPhase::Product);
}

#[test]
fn test_inherited_persona_fills_gap() {
    let schema = load_test_schema();
    let mut session = create_session_with_root("feature");
    // Link parent epic that has a persona
    session.graph_context.push(existing_node("epic-obs", vec![
        edge("serves", "persona-platform-eng")
    ]));
    session.tentative_edges.push(edge("child_of", "epic-obs"));
    let gaps = detect_gaps(&schema, &session);
    // Persona gap should exist but with lower priority (inherited, needs confirm)
    let persona_gap = gaps.iter().find(|g| g.gap_type == GapType::MissingPersona);
    assert!(persona_gap.is_some());
    assert_eq!(persona_gap.unwrap().question_type, QuestionType::Closed);
}
```

### 7.2 Phase transition tests

```rust
#[test]
fn test_product_to_technical_transition() {
    let mut session = create_session_in_phase(InterviewPhase::Product);
    // Missing: persona + metric → should NOT transition
    assert!(!should_advance_phase(&session));

    // Add persona
    session.tentative_nodes.push(node("persona-eng", "persona"));
    session.tentative_edges.push(edge("serves", "persona-eng"));
    assert!(!should_advance_phase(&session)); // still missing metric

    // Add metric
    session.tentative_nodes.push(node("metric-adoption", "metric"));
    session.tentative_edges.push(edge("measured_by", "metric-adoption"));
    assert!(should_advance_phase(&session)); // NOW should transition
}
```

### 7.3 Integration tests (MCP tools)

Test the full `interview_start` → `interview_answer` × N → `interview_commit` flow using a test graph directory with known seed nodes. Verify that committed files match expected YAML frontmatter and body content.

### 7.4 Conversation tests (manual)

Cannot be automated — the LLM behavior is non-deterministic. Instead, maintain a set of test scenarios with expected outcomes:

| Scenario | Input | Expected behavior |
|----------|-------|-------------------|
| Feature with rich parent epic | "add session replay to obs" | Links to epic, inherits persona, asks about metrics |
| Cold start, empty graph | "build a survival game" | No context found, starts from scratch, asks who/why |
| User skips phases | "skip to technical" | Defers product gaps, advances phase |
| User contradicts earlier answer | "actually it's for SREs not platform engineers" | Updates tentative persona link |
| Wall-of-text brain dump | 500-word description | Extracts multiple nodes across types |
| Minimal answer | "yes" | Fills closed gap, moves to next question |

---

## 8. Implementation Order

This spec is designed to be built incrementally, with each stage producing a usable system:

**Stage 1: Skeleton** (days 1-3)
- InterviewSession struct and serialization
- `interview_start`: creates root node, returns it (no extraction, no gap detection)
- `interview_commit`: writes files
- Skill file with basic instructions
- Test: can create a feature node through conversation

**Stage 2: Gap detection** (days 4-6)
- Schema-driven gap analysis
- Phase definitions and transition logic
- `interview_answer`: records QA pairs, updates gaps (no extraction yet — Claude fills nodes directly based on gaps)
- Test: gaps close as edges are added, phases advance

**Stage 3: Extraction subagent** (days 7-10)
- Extraction agent definition
- `interview_answer` calls extraction, merges results
- Duplicate detection (fuzzy title match, vector similarity if index exists)
- Test: natural language answers produce structured nodes

**Stage 4: Context awareness** (days 11-14)
- `interview_start` searches existing graph for context
- Gap detection uses existing graph (inherited personas, related decisions)
- Suggested angle generation uses graph context
- Test: second feature in same epic has shorter interview

**Stage 5: Polish** (week 3)
- `interview_adjust` for mid-interview corrections
- `interview_resume` for session recovery
- `interview_show` with full formatted output for review phase
- Compaction-safe instructions in CLAUDE.md
- Edge case handling (conflicts, extraction failures)

---

## 9. Open Design Decisions

Resolve during implementation, not before:

1. **Subagent vs inline extraction**: The spec calls for a subagent for extraction. If subagent spawning overhead is too high for responsive conversation, fall back to inline LLM calls from the MCP server with the Anthropic API directly. The subagent pattern is cleaner but the inline pattern is faster.

2. **Extraction model**: Sonnet for extraction (cheaper, fast, good enough for structured JSON) vs Opus (more accurate but slower and more expensive). Start with Sonnet, upgrade if extraction quality is insufficient.

3. **Gap priority tuning**: The initial priority assignments (§3.6) are guesses. After 10 real interviews, review which gaps users skip vs engage with and adjust priorities.

4. **Maximum interview length**: Should there be a hard cap on turns? The spec doesn't impose one, but if interviews routinely exceed 20 turns, the gap detection is probably too granular. Add a `--quick` flag to `interview_start` that only asks required gaps.

5. **Re-interview / refine flow**: The spec mentions `tempyr interview refine <node_id>` for re-running gap analysis on committed nodes. The exact UX for this (new session? Edit existing nodes in place?) should be determined after using the basic interview for a few weeks.
