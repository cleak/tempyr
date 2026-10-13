---
name: tempyr-eval
description: >
  Run the Tempyr interview-skill eval in this sandbox: simulated users on fixed
  scenarios, blind judging, aggregate metrics, and paired transcripts for the
  owner. Use when the user runs /tempyr-eval or asks to evaluate the interview skill.
---

# Tempyr interview eval

This project is a throwaway sandbox created by `evals/interview/setup.sh` in the tempyr
repo. The workflow script is `.claude/workflows/tempyr-interview-eval.js` in this
directory (the Workflow tool only loads scripts from the working directory). Beside the
sandbox are `../harness/` (rubric, critic, scenarios, and skill **arms** under
`arms/<name>/SKILL.md`) and `../results/`. Each run interviews simulated users with each
arm, judges every transcript blind, and writes results. Interview sessions are never
committed, so the fixture graph stays clean.

## 1. Preflight

1. Confirm the working directory has `.tempyr/` and
   `.claude/workflows/tempyr-interview-eval.js`, and that `../harness/` has `rubric.md`,
   `scenarios/` and `arms/`. If not, stop and tell the user to run
   `evals/interview/setup.sh` from the tempyr repo and start Claude Code in the sandbox
   it prints.
2. Load `mcp__tempyr__graph_list` with ToolSearch and call it. Expect the fixture nodes
   (`analytics-dashboard-b4c5d6`, `eu-data-residency-e1d2c3`,
   `notification-preferences-a7b8c9`, `workspace-member-f1e2d3`). If the tempyr server
   isn't connected or lists other nodes, stop and report it. The MCP server must be
   running from this directory.

## 2. Build the run

The user can override any of these defaults in their request:
- **Arms**: every directory under `../harness/arms/` (normally `baseline` and
  `candidate`).
- **Scenarios**: every directory under `../harness/scenarios/` (D1–D6).
- **Runs per scenario**: 1 (a quick pass: 6 runs per arm). If the user asks for the
  **full** suite, use each scenario's `meta.json` `k` (16 runs per arm). An explicit
  `k` override applies only to scenarios whose `meta.json` `k` is greater than 1.
- **Max user turns per run**: 16.

For each scenario, read `meta.json` only. Build the args with **absolute** paths:

```json
{
  "rubricPath": "<abs>/harness/rubric.md",
  "criticPath": "<abs>/harness/critic.md",
  "maxUserTurns": 16,
  "arms": [{ "name": "baseline", "skillPath": "<abs>/harness/arms/baseline/SKILL.md", "killPass": false }],
  "scenarios": [{
    "id": "D1", "title": "...", "k": 3, "plantedFlaws": 3, "scores": ["..."],
    "brainDump": "<meta.brain_dump>",
    "personaPath": "<abs>/harness/scenarios/D1/persona.md",
    "judgePath": "<abs>/harness/scenarios/D1/judge.md"
  }]
}
```

`plantedFlaws` is `meta.planted_flaws`; omit it when the scenario has none. Set an
arm's `killPass` to true when its `SKILL.md` has a "Kill-pass" section and
`../harness/critic.md` exists; the harness then runs the critic at the Technical
transition. Omit `criticPath` if `critic.md` is missing.

Never read `persona.md` or `judge.md` yourself, and never put their contents in the
args. The isolation between interviewer, simulated user and judge depends on it.

Tell the user how many runs that is and what it will cost. Measured runs took
9–13 minutes and 0.6–1M subagent tokens. Runs inside a workflow go in parallel, so wall
time is closer to the slowest few runs than to the sum. Then call the **Workflow**
tool once **per arm**, each time with `scriptPath` set to the absolute path of
`.claude/workflows/tempyr-interview-eval.js` in this directory and `args` containing
just that arm. A workflow can spawn at most 1000 agents, and one arm of the full suite
stays under that. Each call returns `{summary, runs}`. Merge them: concatenate the
`runs`, and key `summary` by arm.

## 3. Write results

Create `../results/<YYYYMMDD-HHMMSS>/` (get the timestamp from the shell) containing:

- `result.json`: the merged results, plus the contents of each arm's `SOURCE` file.
- `transcripts/<arm>-<scenario>-r<n>.md`: each transcript (`INTERVIEWER:` / `USER:`
  blocks), followed by the judge's JSON.
- `summary.md`: one table per metric, with a column per arm and the provisional
  target beside it. Every target is a judgment call to revisit after the first
  baseline.

  | Metric | Provisional target (candidate) |
  |---|---|
  | planted_flaw_recall | ≥ min(0.95, max(baseline + 0.30, 0.60)) (capped so a strong baseline leaves a reachable target) |
  | load_bearing_push_rate | ≥ 0.50 |
  | invented_objection_runs_D2 | ≤ 1 |
  | self_answer_runs_D3 | ≤ baseline and ≤ 1 |
  | relitigation_runs | ≤ 1 |
  | divergence_runs_with_accept | ≥ 2/3 of divergence_runs |
  | divergence_precision | ≥ 0.60 |
  | median_user_turns_to_approval | light (D1) ≤ 7, D2 ≤ 8, others ≤ 12 |
  | proxy_abandonment | ≤ baseline + 0.15 |
  | contradiction_surfaced_D5 | true |
  | kill_pass_surfaced (arms with a kill-pass) | ≥ 8/9 of runs where it returned an objection |

  Below the table, add the diagnostics, labelled as diagnostics rather than targets:
  push-or-no-push compliance, no-push rate by scenario, premise n/unsure rate, and end
  reasons. Finish with a short "what stood out" paragraph drawn from the judges'
  `notes`.

- `pairs/` (only when both `baseline` and `candidate` ran): the owner's paired test.
  - Take up to 8 pairs in this order: D1 runs 1–3, D4 runs 1–3, D6 runs 1–2 (only the
    runs that exist; a quick pass gives 3 pairs). A pair is the same scenario and run
    number across the two arms.
  - For pair `NN`, write `pairs/pair-NN.md` with the brain dump and two transcripts
    labelled **Transcript A** and **Transcript B**. Baseline is A in odd-numbered pairs
    and B in even-numbered ones.
  - Normalize formatting so the arm isn't given away by labels:
    - Drop leading `Recorded:` / `Push:` labels, keeping the text after them.
    - Delete any sentence that starts with "No push" (including variants like "No push
      on X: …").
    - Delete the opener's level line ("I'll push at <level> …") and the "(proposed — answer
      or ignore …)" note. Rewrite `[pN]` references as plain words ("the first
      proposal", "the proposals") so no empty brackets or "(, , )" fragments remain.
  - The pairs are formatting-normalized, not blind: the arms differ in substance (e.g.
    numbered premises), which is what the owner is judging. Say so in the pair file.
  - End each pair file with:
    1. Which pushed more usefully on load-bearing claims? (A / B / tie)
    2. Which surfaced a relevant option the user had not raised? (A / B / tie)
  - Write the arm mapping only to `pairs/KEY.md`.

## 4. Report

Show the user the summary table, the results path, and any failed, turn-capped or
no-session runs. If pairs were written, ask the user to answer the pair files before
opening `pairs/KEY.md`. The gate is ≥ 6/8 on each question over the full suite's 8 pairs
(a quick pass is only a preview); with 8 pairs, about 14% of
arms would pass by chance alone, so treat the result as directional.
