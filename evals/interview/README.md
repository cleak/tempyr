# Interview eval harness

This harness measures the `tempyr-interview` skill. It checks whether the skill challenges
the user, surfaces things they hadn't considered, and avoids inventing objections,
without wearing them out. This is stage S0 of `docs/interview-challenge-research.md`.

The eval runs inside a single Claude Code session. The Workflow tool orchestrates
subagents, so the eval uses your subscription and the real skill, tools and MCP server.
It needs no SDK and no API keys.

## Run it

```bash
# 1. Install a tempyr built from this repo (the sandbox's MCP server uses it)
cargo install --path crates/tempyr-cli --locked

# 2. Build the eval workspace (default: ~/.tempyr-eval/interview, or $TEMPYR_EVAL_DIR / --dir).
#    The baseline arm is the skill before the "skeptical design partner" rewrite;
#    the candidate arm is your working tree. Override with --baseline / --candidate <ref>.
evals/interview/setup.sh

# 3. Start Claude Code in the sandbox it prints and run the eval
cd ~/.tempyr-eval/interview/sandbox && claude
/tempyr-eval                                 # e.g. "/tempyr-eval only candidate, D1 and D4, k=1"
```

The workspace sits outside the repo. That way agents don't inherit this repo's
`CLAUDE.md` or skills, and interview journal events stay out of this repo's journal.
Its layout:

| Path | Contents | On rebuild |
|---|---|---|
| `sandbox/` | A tempyr project and its own git repo; start Claude Code here | Rebuilt |
| `harness/` | `workflow.js`, `rubric.md`, the critic, `scenarios/`, `arms/` | Rebuilt |
| `results/<timestamp>/` | `summary.md`, `transcripts/`, and the blind `pairs/` | Kept |

Answer the pairs before you open `pairs/KEY.md`.

A full run is 32 runs: 2 arms × D1–D4 and D6 at k=3, plus D5 at k=1. The eval skill runs one
workflow per arm, which keeps each under the Workflow tool's 1000-agent limit. Expect
roughly 30–60 minutes.

## How it works

- **Sandbox.** It contains a 4-node fixture graph and a structural-only index. Search works
  without an embedding key, but `graph_context` does not, so the interviewer isn't offered it.
- **Runs.** Each run alternates two kinds of fresh subagents:
  - An interviewer that reads its arm's `SKILL.md` and drives the real `interview_*`
    tools.
  - A simulated user that reads only its persona card (hidden facts, style, patience) and
    sees only the conversation.

  The workflow script relays messages verbatim. Session state lives in the MCP server, so a
  fresh interviewer can pick up each turn.
- **Commits.** Runs never commit. When the user approves, the interviewer stops, and the judge
  inspects the session through `interview_show {full: true}`.
- **Judging.** A blind judge per run reads `rubric.md` and the scenario's `judge.md`.
  `workflow.js` then aggregates the metrics deterministically. Recall is measured against
  each scenario's declared planted-flaw count.

## Scenarios

| ID | What it tests |
|---|---|
| D1 | Vague hobby idea, solution-first; light level and "go wilder" |
| D2 | Sound, decided audit-mandate plan; the negative control for invented objections |
| D3 | Self-answer bait: obvious limits and alternatives the interviewer could fill in itself |
| D4 | Open product idea with a hidden better framing (divergence) |
| D5 | Contradicts a constraint in the fixture graph |
| D6 | Overlaps an existing fixture feature; the best move is to extend it (graph lens) |

Each scenario has three files, kept separate on purpose:

| File | Read by |
|---|---|
| `meta.json` | The interviewer: brain dump, run count, planted-flaw count, score keys |
| `persona.md` | Simulated user only |
| `judge.md` | Judge only |

Skill examples must never share a domain with a scenario. All targets in `summary.md` are
provisional judgment calls; reset them after the first baseline run.
