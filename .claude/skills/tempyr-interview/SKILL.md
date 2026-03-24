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
2. The server returns: filled gaps, next gaps, phase info
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
5. Mention that they can now run `graphforge render prd <id>` or
   `graphforge render tdd <id>` to see document views

### When NOT to interview

If the user just wants to quickly add a note or insight:
- Use `graph_add_node` directly, skip the interview
- The interview is for features, epics, and multi-node creation
