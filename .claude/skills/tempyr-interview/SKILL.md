---
name: tempyr-interview
description: >
  Guides the user through creating graph nodes via structured interview.
  Activate when the user wants to: add a feature, create an epic, plan a
  project, capture requirements, do a brain dump, or create a PRD/TDD.
  Keywords: interview, new feature, brain dump, PRD, TDD, requirements.
allowed-tools:
  - mcp__graphforge__interview_start
  - mcp__graphforge__interview_answer
  - mcp__graphforge__interview_show
  - mcp__graphforge__interview_commit
  - mcp__graphforge__interview_adjust
  - mcp__graphforge__interview_resume
  - mcp__graphforge__graph_search
  - mcp__graphforge__graph_context
  - mcp__graphforge__graph_traverse
  - mcp__graphforge__graph_get_node
  - mcp__graphforge__graph_add_node
  - mcp__graphforge__graph_add_edge
---

# Tempyr Interview Skill

You are conducting a structured interview to create knowledge graph nodes.
The MCP server handles state, gap detection, and phase transitions. Your job
is the CONVERSATION — phrasing questions naturally, extracting structured
entities from answers, and presenting proposals clearly.

## Core workflow

### Starting an interview

When the user describes something they want to build/plan/capture:
1. Call `interview_start` with their input as `brain_dump`
2. The server returns: tentative root node, existing graph context, gaps
3. Present what the server found in the existing graph FIRST
4. Show the tentative root node it created from the brain dump
5. Ask the first 2-3 questions from `next_questions`

### Processing answers — the extraction loop

When the user answers a question (or gives additional context):

1. **Record** the answer: call `interview_answer` with their response
2. **Extract** entities from the answer text. For each entity you identify:
   - Call `graph_add_node` with the extracted id, node_type, and body
   - Call `graph_add_edge` to link it to the root or other tentative nodes
   - Use type-prefixed kebab-case IDs (e.g., `persona-platform-eng`, `constraint-p99-latency`)
3. **Show** what was created/linked in compact format (see below)
4. **Ask** the next 2-3 questions from the server's gap list

Alternatively, spawn the `tempyr-extractor` subagent for complex answers
(wall-of-text brain dumps, multi-entity responses). Pass it:
- The question that was asked
- The user's answer
- Current tentative nodes (from `interview_show`)
- Existing graph context

Then apply its JSON output by calling `graph_add_node`/`graph_add_edge`
for each extracted entity.

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
7. Mention: `graphforge render prd <id>` or `graphforge render tdd <id>`

### Resuming an interrupted interview

If the user mentions a previous interview or wants to continue:
1. Call `interview_resume` with the session_id
2. The server returns the full current state
3. Summarize where they left off: phase, nodes created, gaps remaining
4. Continue asking questions from `next_questions`

If the user doesn't know the session_id, they can run
`graphforge interview list` in the terminal to see active sessions.

### When NOT to interview

If the user just wants to quickly add a single note or insight:
- Use `graph_add_node` directly, skip the interview
- The interview is for features, epics, and multi-node creation
