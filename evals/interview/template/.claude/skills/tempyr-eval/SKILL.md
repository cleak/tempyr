---
name: tempyr-eval
description: >
  Run the Tempyr interview-skill eval in this sandbox: simulated users on fixed
  scenarios, blind judging, aggregate metrics, and blind paired transcripts for the
  owner. Use when the user runs /tempyr-eval or asks to evaluate the interview skill.
---

# Tempyr interview eval

This project is a throwaway sandbox created by `evals/interview/setup.sh`. Each run
interviews simulated users with one or more skill **arms** (`arms/<name>/SKILL.md`),
judges every transcript blind, and writes results under `results/`. Interview sessions
are never committed, so the fixture graph stays clean.

## 1. Preflight

1. Confirm the working directory is the sandbox: it has `.tempyr/`, `arms/`,
   `scenarios/`, `rubric.md` and `workflow.js`. If not, stop and tell the user to run
   `evals/interview/setup.sh` from the tempyr repo and start Claude Code in
   `evals/interview/.run/sandbox`.
2. Load `mcp__tempyr__graph_list` with ToolSearch and call it. Expect the fixture nodes
   (`analytics-dashboard-b4c5d6`, `eu-data-residency-e1d2c3`,
   `notification-preferences-a7b8c9`, `workspace-member-f1e2d3`). If the tempyr server
   isn't connected or lists other nodes, stop and report — the MCP server must be
   running from this directory.

## 2. Build the run

Defaults; the user may override in their request:
- arms: every directory under `arms/` (normally `baseline` and `candidate`)
- scenarios: every directory under `scenarios/` (D1–D6)
- runs per scenario: each scenario's `meta.json` `k` (an override applies to scenarios
  with `k > 1`)
- max user turns per run: 16

For each scenario read `scenarios/<id>/meta.json`. Build the Workflow `args` with
**absolute** paths:

```json
{
  "rubricPath": "<abs>/rubric.md",
  "maxUserTurns": 16,
  "arms": [{ "name": "baseline", "skillPath": "<abs>/arms/baseline/SKILL.md" }],
  "scenarios": [{
    "id": "D1", "title": "...", "k": 3, "scores": ["..."],
    "brainDump": "<meta.brain_dump>",
    "personaPath": "<abs>/scenarios/D1/persona.md",
    "judgePath": "<abs>/scenarios/D1/judge.md"
  }]
}
```

Don't read `persona.md` or `judge.md` yourself, and never put their contents in the
args. Isolation between interviewer, simulated user and judge depends on it.

Tell the user how many runs that is and roughly how long it takes (about 1–3 minutes
per run, several running in parallel), then call the **Workflow** tool with
`scriptPath` = the absolute path of `workflow.js` and those `args`.

## 3. Write results

Create `results/<YYYYMMDD-HHMMSS>/` (get the timestamp from the shell) containing:

- `result.json`: the workflow's full return value.
- `transcripts/<arm>-<scenario>-r<n>.md`: each transcript (`INTERVIEWER:` / `USER:`
  blocks), followed by the judge's JSON.
- `summary.md`: one table per metric with a column per arm, next to the provisional
  target. Every target is a judgment call to revisit after the first baseline.

  | Metric | Provisional target (candidate) |
  |---|---|
  | planted_flaw_recall | ≥ max(baseline + 0.30, 0.60) |
  | load_bearing_push_rate | ≥ 0.50 |
  | invented_objection_runs_D2 | ≤ 1 |
  | self_answer_runs_D3 | ≤ baseline and ≤ 1 |
  | relitigation_runs | ≤ 1 |
  | divergence_runs_with_accept | ≥ 2/3 of divergence_runs |
  | divergence_precision | ≥ 0.60 |
  | median_user_turns_to_approval | light (D1) ≤ 7, D2 ≤ 8, others ≤ 12 |
  | proxy_abandonment | ≤ baseline + 0.15 |
  | contradiction_surfaced_D5 | true |

  Add the diagnostics (push-or-no-push compliance, no-push rate by scenario, premise
  n/unsure rate, end reasons) below, labelled as diagnostics rather than targets. Also
  include a short "what stood out" paragraph drawn from the judges' `notes`.

- `pairs/` (only when both `baseline` and `candidate` ran): the owner's paired test.
  Take up to 8 pairs, in this order: D1 runs 1–3, D4 runs 1–3, D6 runs 1–2. A pair
  matches the same scenario and run number across the two arms. For pair `NN`, write
  `pairs/pair-NN.md` holding the brain dump plus two transcripts labelled
  **Transcript A** and **Transcript B**. Put baseline as A in odd-numbered pairs and as
  B in even-numbered ones. Normalize formatting so the version isn't obvious:
  - Drop leading `Recorded:`, `Push:` and `No push:` labels (keep the text after
    them).
  - Replace an opener line like "I'll push at <level> …" with "(level line)".

  End each pair file with the two questions:
  1. Which pushed more usefully on load-bearing claims? (A / B / tie)
  2. Which surfaced a relevant option the user had not raised? (A / B / tie)

  Write the arm mapping to `pairs/KEY.md` only.

## 4. Report

Show the user the summary table, the run's results path and any failed or turn-capped
runs. If pairs were written, ask them to answer the pair files **before** opening
`pairs/KEY.md`. The gate is ≥ 6/8 on each question. With 8 pairs, about 14% of runs
would pass by luck alone, so treat it as directional.
