---
name: tempyr-spar
description: Use when the user wants to red-team, pre-mortem, or stress-test a Tempyr graph node (e.g. /tempyr-spar <node>) with an independent critic, then accept, rebut, dismiss, or change each objection.
---

# Tempyr Spar

Challenge a part of the graph that nobody has challenged yet. An independent critic
(the `tempyr-critic` subagent) reviews the nodes in a fresh context; you validate
its output, show the strongest objections, and the user decides what happens to each.

Usage: `/tempyr-spar <node> [hops N] [lens kill-pass|pre-mortem|assumptions|outside-view|panel]`
Defaults: 1 hop, `kill-pass`. `panel` runs pre-mortem, assumptions, and outside-view.

## 1. Build the packet

1. Resolve the node, then call `graph_traverse` with
   `{node_id, depth: hops, include_body: true}`. If the user is in an interview and
   names a tentative node, use `interview_show {session_id, full: true}` instead.
2. For `kill-pass` and `outside-view`, drop decision, component, task, and
   api_surface nodes. These lenses judge the problem, not the chosen solution.
3. Gather **settled** items:
   - prior rebuttals and dismissals: `journal_search` with an empty query,
     `tags: ["disposition:rebut"]` (then `["disposition:dismiss"]`), and
     `references: [<packet node ids>]`. You can also run `journal_blame` on the
     target's file.
   - decisions with status `decided`, and any premise answers in an active
     interview.
4. Set `focus: mandate` if the nodes describe a compliance, audit, or policy
   requirement.

Never include conversation transcript in the packet.

## 2. Run the critic

Spawn `tempyr-critic` with `lens`, `focus`, `packet` (JSON), and `settled`. For
`panel`, spawn one per lens in parallel. Never show one critic another's output.
If the subagent isn't available (e.g. in Codex), run each critique yourself as a
separate pass that sees only the packet and settled items, following
`.claude/agents/tempyr-critic.md` or `.agents/skills/tempyr-spar/critic.md`.

## 3. Validate before showing anything

Drop an objection if any of these holds:
- Its `target_node` isn't in the packet.
- Its `quoted_claim` isn't found in the target's body. Compare after normalizing:
  CRLF→LF, whitespace collapsed, curly quotes straightened, case-insensitive. It
  must be at least 15 characters, not only the title, and not inside a `needs:`
  line or a `<!-- tempyr:gen -->` block.
- It argues against a settled item.
- It duplicates an existing risk or open question linked to the target, or another
  objection.

Rank the rest by severity, then by how many edges the target has.

## 4. Show the top 3

Number them. For each, show:
- the target id
- the quoted claim
- "fails if …"
- the cheapest test, if any
- the proposed change

Then add one line on what's well-reasoned. Ask for dispositions in a single line,
for example `1a 2r launchd handles it 3d`:
- `a`: accept
- `r <reason>`: rebut
- `d`: dismiss as noise
- `c <change>`: change the plan

`more` shows the next three. If nothing survives validation, say the plan holds up
under this lens and list what's well-reasoned.

## 5. Apply each disposition

- **accept**: preview the change, then write it on confirmation.
  - A risk becomes a `risk` node (`status: identified`) linked with `has_risk` /
    `risk_for`.
  - An assumption becomes an `open_question` (`status: open`) linked with
    `has_question` /
    `question_for`.
  - An alternative goes under `## Alternatives considered` in the target's body.
  - If no valid edge exists for the target's type, append to its body under
    `## Risks` or `## Open questions` instead.
  - Committed nodes use `graph_add_node` / `graph_add_edge`. Tentative interview
    nodes use `interview_add_node` / `interview_add_edge`.
- **change**: preview the body edit, then apply it with `graph_update_node`
  (committed) or `interview_adjust` (tentative).
- **rebut / dismiss**: record only. Never raise that objection again.

Journal every disposition with `journal_log`:
- `kind: "finding"`
- `summary: "spar <lens> objection <disposition>: <short claim>"`
- `detail`: the quoted claim, "fails if", and the user's reason
- `tags: ["spar", "objection:<target>-<n>", "disposition:<accept|rebut|dismiss|change>"]`
- `references: [<target>]`
- `files: [<the target's node file>]` when it's committed

This journal record is what keeps later runs from re-raising the same objection.
