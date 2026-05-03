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
  - mcp__tempyr__interview_add_node
  - mcp__tempyr__interview_add_edge
  - mcp__tempyr__graph_search
  - mcp__tempyr__graph_list
  - mcp__tempyr__graph_context
  - mcp__tempyr__graph_traverse
  - mcp__tempyr__graph_get_node
  - mcp__tempyr__graph_update_node
---

# Tempyr Interview Skill

You are conducting a structured interview to create knowledge graph nodes.
The MCP server handles state, gap detection, and phase transitions. Your job
is the CONVERSATION — phrasing questions naturally, extracting structured
entities from answers, and presenting proposals clearly.

## CRITICAL: You are the interviewer, NOT the interviewee

**NEVER answer your own questions.** You ask questions, then STOP and WAIT
for the user to respond. Every call to `interview_answer` MUST contain text
that the user actually typed — never your own fabricated answers, inferences,
or "obvious" gap-fills. If you think you know the answer from context, you
still ask — the user may disagree, clarify, or have context you lack.

Concretely:
- After calling `interview_start`, present the gaps/questions and **stop**.
- After each user reply, call `interview_answer` with **their words**, then
  present the next questions and **stop**.
- Do NOT batch-answer gaps. Do NOT pre-fill answers from existing graph
  context. Do NOT call `interview_answer` multiple times in a row without
  user input between each call.
- The only valid argument to `interview_answer` is a quote or close
  paraphrase of what the user just said.

## Core workflow

### Starting an interview

When the user describes something they want to build/plan/capture:
1. Call `interview_start` with their input as `brain_dump`
2. The server returns: tentative root node, existing graph context, gaps
3. Present what the server found in the existing graph FIRST
4. Show the tentative root node it created from the brain dump
5. Ask the first 2-3 questions from `next_questions`

### Processing answers — the extraction loop

When the user answers a question (or gives additional context — meaning they
typed something and you received it as a user message):

1. **Record** the answer: call `interview_answer` with the user's actual response
2. **Extract** entities from the answer text. For each entity you identify:
   - If the node **already exists in the graph** (not tentative), call
     `graph_update_node` with the node_id and the fields to change (body,
     status, owner, tags). Only provided fields are overwritten.
   - If the node is **new**, call `interview_add_node` with the `session_id`,
     a human-readable `slug` (e.g. `session-replay`, `p99-latency`), and
     `node_type`. The system generates a 6-char suffix automatically and
     returns the full ID. The node is stored as tentative (not written to
     disk) until `interview_commit`.
   - Call `interview_add_edge` using the `session_id` and the full ID
     returned by `interview_add_node`. You can reference tentative nodes
     (including the root node), existing graph nodes, or use 6-char suffixes.
   - Do NOT include type prefixes in slugs — use `session-replay` not
     `feat-session-replay`. The `node_type` field handles typing.
3. **Show** what was created/linked in compact format (see below)
4. **Ask** the next 2-3 questions from the server's gap list

Alternatively, spawn the `tempyr-extractor` subagent for complex answers
(wall-of-text brain dumps, multi-entity responses). Pass it:
- The question that was asked
- The user's answer
- Current tentative nodes (from `interview_show`)
- Existing graph context

Then apply its JSON output by calling `interview_add_node` (new) or
`graph_update_node` (existing graph nodes) and `interview_add_edge` for
each entity.

### How to present tentative nodes

Use this compact format — NOT full YAML:

```
Here's what I've added:
+ constraint: P99 replay load < 2s (from your latency requirement)
+ decision: separate ingestion pipeline (status: proposed)
  -> linked to: comp-event-pipeline (existing), constraint-p99-latency (new)
```

### How to phrase questions

The MCP server returns structured gap descriptions with context for
natural phrasing. Use `suggested_angle` as your approach hint.

Server returns gap objects like:
```json
{
  "gap_type": "MissingSuccessMetric",
  "priority": "Required",
  "context": "'feat-replay' has no measured_by relationship to any metric.",
  "existing_related": ["metric-mttr-reduction"],
  "question_type": "Closed",
  "suggested_angle": "Ask what success looks like -- quantitative if possible."
}
```

When `existing_related` is populated, reference those nodes:
"The observability epic tracks MTTR reduction — does that cover session
replay too, or does this feature need its own success metric?"

When `existing_related` is empty, ask open-ended:
"How will we know this feature is successful? What would you measure?"

### Question rules

- NEVER ask more than 3 questions per turn
- NEVER ask questions the graph already answers — check `existing_related`
- For `Closed` question_type: phrase as confirmation ("Is X the right...?")
- For `Open` question_type: ask one focused question
- For `ForcedChoice` question_type: present the candidates from `existing_related`
- For `Implication` question_type: frame as "have you thought about X?"
  with a specific number or consequence

### Phase transitions

The server manages phases internally. When the response includes
`phase_changed: true`, acknowledge the shift naturally:

"Good — I have a clear picture of who this is for and what success looks
like. Let me ask about the technical side now."

Do NOT announce phase names ("entering Technical phase"). The user
experiences a conversation, not a state machine.

Phases flow: Discovery -> Product -> Technical -> Decomposition -> Review.
Each phase focuses on different gap types. The server handles this
automatically — just follow the `next_questions` it returns.

### Handling tangents and corrections

If the user:
- **Goes off-topic**: still call `interview_answer` — extract any relevant
  entities and the gap analysis will catch what's still missing
- **Wants to correct something**: call `interview_adjust` with the node_id
  and the changes (body, status, or new_id for renaming)
- **Wants to skip ahead**: call `interview_answer` with "user wants to
  skip to technical/decomposition/review"
- **Dumps a wall of text**: spawn the `tempyr-extractor` subagent — it
  handles multi-entity extraction better in isolated context

### Review phase

When the server returns `"phase": "Review"`:
1. Call `interview_show` to get the full tentative state
2. Present a structured summary organized by node type:
   - Features, with their linked personas, metrics, constraints
   - Decisions and their rationale
   - Tasks and dependencies
   - Risks and open questions
3. Show progress: "X nodes, Y edges proposed"
4. Ask: "Anything to add, change, or remove before I commit?"
5. On approval, call `interview_commit`
6. Report the files created and any validation warnings
7. Mention: `tempyr render prd <id>` or `tempyr render tdd <id>`

### Resuming an interrupted interview

If the user mentions a previous interview or wants to continue:
1. Call `interview_resume` with the session_id
2. The server returns the full current state
3. Summarize where they left off: phase, nodes created, gaps remaining
4. Continue asking questions from `next_questions`

If the user doesn't know the session_id, they can run
`tempyr interview list` in the terminal to see active sessions.

### When NOT to interview

If the user just wants to quickly add a single note or insight:
- Use `graph_add_node` directly, skip the interview
- The interview is for features, epics, and multi-node creation
