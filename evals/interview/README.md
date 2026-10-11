# Interview eval harness

Measures the `tempyr-interview` skill: does it challenge the user, surface what they
hadn't considered, and avoid inventing objections, without wearing them out? This is
stage S0 of `docs/interview-challenge-research.md`.

Everything runs inside one Claude Code session with subagents, orchestrated by the
Workflow tool, so it uses your subscription and the real skill, tools, and MCP server.
No SDK or API keys are needed.

## Run it

```bash
# 1. Install a tempyr built from this repo (the sandbox's MCP server uses it)
cargo install --path crates/tempyr-cli --locked

# 2. Build the sandbox: baseline skill from origin/master, candidate from your working tree
evals/interview/setup.sh                     # or: --baseline <ref> --candidate <ref>

# 3. Start Claude Code in the sandbox and run the eval
cd evals/interview/.run/sandbox && claude
/tempyr-eval                                 # e.g. "/tempyr-eval only candidate, D1 and D4, k=1"
```

Results land in `evals/interview/.run/sandbox/results/<timestamp>/`:

| File | Contents |
|---|---|
| `summary.md` | Metrics per arm against the provisional targets |
| `transcripts/` | Every conversation plus the judge's verdict |
| `pairs/` | Your blind paired comparison (answer the pairs before opening `KEY.md`) |

A full run is 32 runs (2 arms × D1–D4 and D6 at k=3, plus D5 at k=1). That's a few hundred subagent calls and takes roughly 30–60 minutes, depending on how many run in parallel.

## How it works

- **Sandbox.** `setup.sh` copies `template/` into `.run/sandbox` and copies the default schema in. It puts the two skill arms in `arms/`, builds a structural-only index, and makes the sandbox its own git repo. The tempyr MCP server started there sees only the fixture graph, and journal events stay out of this repo's journal.
- **One run.** Each run alternates two kinds of fresh subagents:
  - an interviewer that reads its arm's `SKILL.md` and drives the real `interview_*` tools, and
  - a simulated user that reads only its persona card (hidden facts, style, patience) and sees only the conversation.

  The workflow script relays messages verbatim. Session state lives in the MCP server, so a fresh interviewer per turn can pick it up.
- **No commits.** Runs never commit. When the user approves, the interviewer stops, and the judge inspects the session with `interview_show {full: true}`.
- **Judge.** A blind judge per run reads `rubric.md` and the scenario's `judge.md`. It can't see which arm produced the transcript. `workflow.js` aggregates the scores deterministically.

## Scenarios

| ID | What it tests |
|---|---|
| D1 | Vague hobby idea, solution-first; light level and "go wilder" |
| D2 | Sound, decided audit-mandate plan. The negative control: no invented objections |
| D3 | Self-answer bait: obvious limits and alternatives the interviewer could fill in itself |
| D4 | Open product idea with a hidden better framing (divergence) |
| D5 | Contradicts a constraint in the fixture graph |
| D6 | Overlaps an existing fixture feature, where the best move is to extend it (graph lens) |

Each scenario has three files, and the separation matters:

| File | Who reads it |
|---|---|
| `meta.json` | The brain dump the interviewer sees |
| `persona.md` | The simulated user only |
| `judge.md` | The judge only |

Skill examples must never share a domain with a scenario.

All targets in `summary.md` are provisional judgment calls; reset them after the first
baseline run.
