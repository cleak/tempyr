# Making the Interview Challenge the User — Research & Proposal

- **Created**: 2026-09-26 (v5, revised after critic rounds 1–5)
- **Status**: In progress — S1-R Rust (#54), S1 skill rewrite (this PR), schema delta, eval harness, and `spar` in follow-up PRs
- **Scope**:
  - `tempyr-interview` (gaps, phases, session)
  - `tempyr-core` (frontmatter, ops)
  - `tempyr-mcp`
  - schema
  - the interview skill (Claude, `.agents`, Codex)
  - the extractor
  - a new `spar` command and critic agent
  - journal auto-emit
- **Question**: The interview doesn't challenge enough or think outside the box. What do current best practices say, and should we use a critic panel?

---

## 1. TL;DR

1. **A critic panel isn't the biggest lever.** Three things make the interview uncritical:
   - A gap closes as soon as a node of the right type *exists*.
   - Phases auto-advance on node existence, before any pushback.
   - The skill has no stance.

   The first fix the owner will *feel* is **S1** (~5 d): a self-contained skill rewrite (Appendix B) plus a gate-free Rust patch covering phase timing, full-content reads, and schema edges.
2. **Yes to critics, no to debate.** Evidence from reasoning benchmarks (mostly homogeneous or small models):
   - Multi-agent debate mostly reduces to majority voting.
   - Agents conform: majority adoption reached up to 85.5% in 7–8B models.
   - Dense communication accelerates premature convergence.

   So critics are independent, run in a fresh context that holds **settled items** but no transcript, are aggregated in Rust, and surface one objection at a time. They live in an on-demand **`spar`** command over any node, committed or tentative. The interview runs one **kill-pass** in the background at Product→Technical. Two parts are hypotheses, not findings: defining critics by lens rather than persona, and using a cross-model critic.
3. **"Outside the box" ships in S1, user-first and grounded in the graph.** In order:
   1. Ask what the user already considered.
   2. Check the graph and journal for overlapping work and past dead-ends ("extend feat-X?").
   3. Always offer one option from a lens the user didn't cover.
   4. Pose reframes as questions.
   5. Offer "go wilder" on demand.
4. **Provenance becomes load-bearing only once Rust quality gates exist.** The origin model (S2o) is built only when a gate needs it. Gates then close only on content the user stated or confirmed.
5. **Burden is a first-class constraint.**
   - Challenge levels are inferred and announced. Epics get standard; mandates get a "closure" focus.
   - At most one push per turn.
   - The user decides on preferences.
   - "Wrap up" commits from any phase.
   - Guardrails cover turns-to-commit and abandonment.
   - A paired owner comparison (formatting-normalized, directional) tests the complaint directly.
6. **Stages** (engineer-days; S2o, S2b, S4, and S5 are gated). Every numeric target is a provisional judgment call, reset after S0.

   | Stage | Scope | Effort |
   |---|---|---|
   | S0 | Baseline | ~2 d |
   | S1 | Skill rewrite + gate-free Rust | ~5 d |
   | S3a | Skill-only `spar` + kill-pass | ~2 d |
   | S2a | Core fixes + session state | ~6 d |
   | S3b | `spar` hardening | ~5 d |
   | S2o | Origin model | ~5 d |
   | S2b | Quality gaps (scoped by S1) | ~8 d |
   | S4 | Depth | ~8 d |
   | S5 | Divergence engine + panel | ~7 d |

---

## 2. Diagnosis of the current implementation

| Where | Problem | Effect |
|---|---|---|
| `gaps.rs:158-167` | A gap is skipped if *any* node of the target type exists, even one unlinked to the root. The `graph_context` prefix check (`starts_with("persona-")`) is dead since hybrid IDs dropped type prefixes. | One vague persona or "improve UX" metric closes the gap. |
| `gaps.rs:56-70`, `152-208`, `356-396` | Every gap is an *existence* gap, generated only from the root's `allowed_edges`, and each carries a full template `suggested_question`. | Assumptions, alternatives, evidence, and vague metrics go unchecked. The server scripts the questions. |
| `gaps.rs:191,219` | The gap id / dedup key is `(gap_type, node_type_needed)`. | There's no way to express a gap on a specific node. |
| `phases.rs:66-127`; `proposer.rs:65,172`; `handler.rs:2036` | `interview_start` and every `add_node` run the phase check. Discovery ends after 1 answer when context exists. Product, Technical, and Decomposition end as soon as a matching node exists. | Phases advance *before* any push, and the interview converges once the template is filled. |
| `handler.rs:1748-1754`; `session.rs:72-78` | The recorded "question" is the top-3 suggestions concatenated. `QAPair` has no `gap_id`. | The system can't tell which gap or premise an answer addressed. |
| `handler.rs:2033`, `proposer.rs:115` | `source_qa = answered.len()`, one past the answer just recorded. | Provenance links to the wrong user text. |
| `handler.rs:750,760`; `handler.rs:1303-1327, 95-97` | `interview_show` returns 3-line previews for non-root nodes. `graph_traverse` returns only `{node_id, depth, type, title}`, and its `edge_types` filter is dead. | No tool exposes the full text a critic needs. |
| `node.rs:10-27`; `session.rs:449-486`; `ops.rs:13-31,201-259,526` | `NodeFrontmatter` has fixed keys and no extras. Commit writes only owner and tags. Edge ops re-serialize the whole node. Commit only creates files, never patches them. | Structured data can't reach disk. |
| `handler.rs:183-212, 1932` | `add_node` and `adjust` accept no `fields`. `adjust` reanalyzes without the graph. | |
| `handler.rs:2001-2108`; `auto_emit/interview.rs:41-83` | MCP `add_node` and `add_edge` emit nothing, although CLAUDE.md says they do. | No data for measurement. |
| `session.rs:399`; `init.rs:171,550` | Commit deletes the session file. Only `.env.local` is gitignored. | No history for baselines, and archived answers would be committed if kept. |
| `SKILL.md:28-30, 41, 57-59, 82, 95-104, 132, 134, 145-146, 152-153, 162-163` | Rules push toward scribing (full list in Appendix B), plus a skip hack that sends non-user text to `interview_answer`. | The default is an agreeable scribe. |
| `default-schema.toml` | `epic` lacks `has_risk` and `has_question`. `decision` lacks `has_risk`. `risk_for` can only target a feature. There's no `assumption` type. Projects hold a copy of the schema (`init.rs:136`), and `ops::find_type_directory` is hardcoded. | Challenge output about epics and decisions can't be linked. Commit drops those edges as warnings. |
| `tempyr-interview.codex.SKILL.md` | A 28-line stub. | |

---

## 3. What the research says (condensed; verified across five audit rounds)

### 3.1 Critic panels and debate

- **Debate ≈ voting.** *Debate or Vote* (NeurIPS 2025 spotlight, 2508.17536): majority voting explains most of debate's gains on reasoning benchmarks. Among homogeneous agents, debate is a martingale.
- **Debate often loses to simpler baselines.** *Stop Overvaluing MAD* (2502.08788): debate often underperforms CoT and self-consistency. The paper recommends **model heterogeneity**.
- **Conformity.** *Cost of Consensus* (2605.00914), on 7–8B models with GSM-Hard and MMLU-Hard:
  - majority adoption up to 85.5%
  - vulnerability up to 70% in one model
  - 2.1–3.4× the tokens

  *Diversity Collapse* (2604.18005): "group-size scaling yields diminishing returns and dense communication topologies accelerate premature convergence."
- **Personas vs lenses.**
  - Persona labels don't improve *factual accuracy*: Zheng et al., "When 'A Helpful Assistant' Is Not Really Helpful" (EMNLP Findings 2024); Wharton *Playing Pretend* (2512.05858).
  - ChatEval (2308.07201): diverse roles beat identical ones in debate-based evaluation (60.0 vs 53.8). Identical roles did no better than a single agent.
  - Varied personas raise idea diversity (Terwiesch et al. 2026, 2607.27553).
  - **No study compares task-lens critics with persona critics.** Our choice of lenses is a hypothesis.
- **Sycophancy and self-preference.**
  - ELEPHANT (ICLR 2026): models preserve users' face 45 percentage points more than humans do.
  - Kim & Khashabi (EMNLP Findings 2025): models flip under follow-up rebuttal, but judge correctly when both sides are shown side by side.
  - Panickssery et al. (NeurIPS 2024, 2404.13076): LLM evaluators favor their own generations.
- **HCI.**
  - Chiang et al. (IUI 2024): a devil's advocate arguing against the *AI's* recommendation showed potential to promote appropriate reliance. The authors also tested challenges to the group majority, but the benefit was reported for AI-targeted objections.
  - Drosos/Sarkar (2501.17247, n=24): provocations restored critical thinking about AI suggestions.
  - Applying these findings to challenging a user's own plan is extrapolation.
  - Buçinca et al. (CSCW 2021, 2102.09692): friction-heavy designs got the lowest ratings **but reduced overreliance the most.**
- **Practitioner warning** (Anthropic Claude Code best practices): "a reviewer prompted to find gaps will usually report some, even when the work is sound". Flag only issues that affect correctness or stated requirements.
- **Verbalized confidence is overconfident** (Xiong et al., ICLR 2024, 2306.13063).

### 3.2 Requirements elicitation

- **LLMREI** (RE'25, 2507.02564): the structured least-to-most prompt produced more generic template questions than zero-shot. The lesson is to avoid scripted question lists. Structure also dropped off when users signalled they were short on time.

  | Question type | Least-to-most | Zero-shot |
  |---|---|---|
  | Parameterized (generic template) | 28.9% | 12.8% |
  | Context-deepening | 32% | 44% |
  | Deepening + enhancing | 43% | 60% |

- **Mistake-guided follow-ups were preferred 68% of the time** (87/128, p≈4e-8; Shen/Singhal/Breaux, RE'25, 2507.02858). The mistakes include:
  - missing tacit assumptions
  - not considering alternatives
  - not clarifying contradictions
  - asking generic questions
- **ReqElicitGym** (2602.18306): the best model reached only 32% of implicit requirements. Models favor probing over clarifying.
- **OntoAgent** (2605.05828): +33% on implicit requirements (whole system).
- **BED-LLM** (ICLR 2026, 2508.21184): +37pp on average with expected-information-gain question selection (20Q and preference tasks).
- **Probes** (CHI 2025, 2503.08582, N=64; a chatbot survey about people's own tech-stress experiences):
  - The **idiographic** probe performed best overall: "can you give me a recent example of when you experienced this?"
  - The descriptive probe also did well: "describe in detail what you were doing when…"
  - The explanatory "why" probe was less relevant and specific, and was rated repetitive.
  - For a builder we combine the first two: "When did this last happen? Walk me through it." A mandate also counts as evidence.
- **Depth vs breadth** (2608.01640, quasi-experiment): an AI script + tracking + follow-up workflow produced 3.43 vs 1.15 follow-ups per topic, but covered fewer topics (9.6 vs 14.5).

### 3.3 Divergent thinking

- **Verbalized Sampling** (2510.01171): mode collapse comes from typicality bias. Asking the model to generate k responses with probabilities gave 1.6–2.1× more diversity.
- **Artificial Hivemind** (NeurIPS 2025 D&B best paper, 2510.22954): different model families give strikingly similar answers.
  - *Our inference, untested:* generic lens prompts may converge too, though direction-first stratification (below) suggests explicit directions help.
  - So we try **grounding divergence in the project's own graph** first. This is a hypothesis, and eval dump D6 exercises it.
  - Pooling ideas across vendors raised idea diversity (Terwiesch 2026). Model heterogeneity improved debate *accuracy* (2502.08788), not diversity.
- **Direction-first stratification** (2605.30150) gave the best diversity/quality/compute frontier.
- **Timing matters** (CHI 2025, 2502.06197, N=60): early LLM use reduced the number of original ideas. So diverge after the brain dump, and ask what the user considered first.
- **Reframing.**
  - LLM help gave no improvement in problem-frame quality (CHI 2025, 2503.01631, N=280).
  - Pail (CHI 2025, 2503.06911, exploratory, n=11): users rarely sought new framings unless prompted, and ignored content under overload.
  - Hence: explicit reframe prompts, and one push per turn.

### 3.4 What shipping tools do

| Tool | Mechanic worth stealing |
|---|---|
| **GitHub Spec Kit `/clarify`** | Clear/Partial/Missing taxonomy. **Impact × Uncertainty** ranking. **≤5 questions per clarify pass**, one at a time. **Recommended: option + reasoning**. Coverage table. |
| **BMAD Advanced Elicitation** | At pauses, a menu of 5 methods (pre-mortem, inversion, assumption audit, reframe, subtraction…), each result accepted or discarded. Warns against quizzing in discovery. Counter-metrics. |
| **gstack `office-hours`** | "Take a position… state what evidence would change it." Banned hedge phrases. "Push once, then push again." A numbered **premise challenge**. Minimal/ideal/lateral alternatives. A **cross-model second opinion**. |
| **Matt Pocock `grill-me`** | "Facts are your job; decisions are the user's." "A session with no pushback from you is a session you didn't need." |
| **Anthropic `product-brainstorming`** | "A thinking partner who only validates is not a thinking partner." Opposite and subtractive options. **The assumption that kills the idea**, plus the cheapest test for it. |
| **pm-skills `strategy-red-team`** | Separates load-bearing from cosmetic claims. Steelman first, then attack. "Fails if ___". "Never invent a weakness." |
| **Codex Plan Mode** | Explores facts instead of asking about them. 2–4 options with a recommended default. Unanswered defaults become a **recorded assumption**. |
| **Kiro Analyze Requirements** ([blog](https://kiro.dev/blog/deep-spec-analysis/)) | Finds contradictions and gaps using SMT. Each finding is framed as **A = keep as-is / B = a specific change**. |
| **Productboard Spark** | Evidence-grounded pushback. Tempyr has the graph and journal for this. |
| **Tessl task evals** | Generates scenarios, runs them with and without the skill, and scores against generated criteria. |

ChatPRD: mechanics unpublished; reviewers find its output generic.

---

## 4. Design

### 4.0 Alternatives considered

| Option | Verdict |
|---|---|
| **A. Gap-driven Q&A, hardened** | **Chosen spine.** |
| **B. Draft-then-attack** | An optional skill-only arm in S1 if budget allows; otherwise an S4 experiment. The S1 opener premises are a "B-lite". |
| **C. Design-tree frontier** | Rejected as the spine: its question burden is uncapped. We adopt its facts-vs-decisions rule. |
| **D. Persona debate panel** | Rejected (§3.1). |
| **E. On-demand `spar` over the graph** | **Adopted as the home for critics.** |

### 4.1 Principles

1. **User decides.**
   - Hold a position only on *factual* claims.
   - On preferences and decisions the user's word is final. Record their rationale, state dissent **once**, and never re-raise it.
   - "Wrap up" works from any phase.
2. **At most one push per turn.**
   - A push quotes the user's own words. It can be an objection, a specificity push, an evidence probe, alternatives, or a reframe.
   - Say "No push: \<reason\>" only where pushback was plausibly expected. Purely logistical answers need neither.
   - These labels describe the shape of a turn; they are not literal headers.
3. **Mark weak answers; don't hold them back.**
   - Extract immediately.
   - If an answer fails the level's bar, the node gets a visible `needs:` marker and becomes the turn's push.
   - Refine it with `interview_adjust`. Review lists anything still marked.
   - An ignored push counts as declined.
4. **User ideas before AI ideas, grounded in the graph.**
   - Ask what the user considered, then check the graph and journal for overlap and dead-ends, then offer one *uncovered* lens.
   - Pose reframes as questions.
   - Never re-open settled items (answered premises, decided decisions, rebuttals), whether in the interview or in a critic.
5. **Grounded pushback.** Look facts up instead of asking. Cite them.
6. **Provenance before gates (S2o+).** Rust quality gates close only on user-stated or user-confirmed content (§A.2). *Limit:* Rust can't verify that answer text is really the user's. The evals and a journal flag detect self-answering.
7. **Rust decides when; the LLM supplies content (S2a+).**

### 4.2 Challenge levels and focus

**Setting the level.** The level is **inferred** from the brain dump, **announced** in the opener, and changed whenever the user says "lighter" or "harder". A persistent default can be set with `.tempyr/config.toml` `[interview] level = "hard"`.
- **Default:** standard, *including epics*.
- **Light:** hobby, prototype, spike, side project, or "for me / just me" wording, or a note/insight root.
- **Hard:** irreversible or launch wording, or when the user asks for it.

**Mandate-closure focus** is orthogonal to level. It triggers on compliance, audit, or policy wording.
- Premises test whether the plan fully closes the mandate.
- No alternatives for mandated choices.
- The mandate itself satisfies the evidence requirement.
- The kill-pass lens becomes "the assumption under which this fails to close the mandate".
- No accept-streak callouts.

**Decided decisions** get a *plain batched question* rather than a push: "X / Y — anything you ruled out? one line each". This applies at every level unless the user asked for "harder".

| Behavior | light | standard | hard |
|---|---|---|---|
| Opener premises | 1–2, stakes-aware | 3 | 5 |
| Take a position on | decisions | decisions, metrics, personas | every substantive answer |
| Pushes per topic | 1 | 2 | 2 + evidence probe |
| Session push budget¹ (enforced from S2a) | 3 | 6 | 10 |
| Alternatives per open decision | step 0 only | step 0 → graph lens → 1 uncovered lens (minimal/ideal/lateral if step 0 was thin) | same, with ≥2 considered |
| Metric bar | "done when…" is enough | number, threshold, or date | + a guardrail metric |
| Reframes (product roots) | off | 1, at Discovery→Product | 2 (+ framing audit in S5) |
| Kill-pass | at Product→Technical | at Product→Technical | + pre-Review sweep, + irreversible decisions |
| Deferred items land as | root `## Open questions` section | `open_question` nodes | `open_question` nodes |
| Target user turns to commit¹ | ≤7 | ≤12 | ≤20 |

"Thin" means fewer than 2 options, or all options from one lens.

¹ Judgment calls, reset after S0. Spec Kit caps a clarify pass at 5 questions; ours is a longer interview, so this is a separate call.

### 4.3 Opening turn

The opener is a single message:
1. Graph context: related nodes, superseded decisions, dead-ends.
2. The root.
3. One line: "I'll push at *standard* — say lighter/harder, 'go wilder', or 'wrap up' anytime", plus the focus if any.
4. Numbered premises. Each one names an assumption and what would make it false, and is answered y / n / unsure ("1y 2n 3?"). The most load-bearing premise gets an evidence question instead. These are exempt from the 3-question cap.

The opener asks no gap questions. When the reply is recorded, the premise list is passed as `question`.

### 4.4 Stages

#### S0: Baseline (~2 d)

- **Runner:** in-session subagents orchestrated by the Workflow tool, launched by a `/tempyr-eval` skill inside a sandbox tempyr project (`evals/interview/`). The sandbox is its own git repo with a fixture graph, so runs never touch a real graph or journal.
  - The interviewer is a fresh subagent per turn. It reads its arm's skill file and drives the real tempyr MCP tools; session state carries across turns.
  - The **simulated user** is a separate subagent with a persona card, hidden facts, and a **fixed patience budget**. Giving up counts as *proxy abandonment*. It rejects irrelevant options and gives a reason.
  - The workflow script relays messages verbatim and aggregates scores deterministically.
  - Runs never commit: the judge inspects the session via `interview_show {full: true}`.
  - Runs on the subscription, not API billing.
- **Dumps.** D1, D3, and D4 each carry **exactly 3** planted flaws, scored for recall. D2, D5, and D6 carry scenario-specific checks only. Skill examples must never share a domain with any dump.

  | Dump | Scenario | Level |
  |---|---|---|
  | D1 | Vague hobby idea, solution-first. The simulated user says "go wilder" once. | light |
  | D2 | Well-reasoned mandate plan (SSO for an audit finding). Passes if ≤1 run contains an invented objection. | standard, mandate focus |
  | D3 | Invites self-answering | standard |
  | D4 | Open product idea; the user privately holds a better framing | standard |
  | D5 | Contradicts a constraint in a fixture graph | — |
  | D6 | Overlaps committed fixture nodes; the best alternative is extending an existing feature. Tests the graph lens and node reuse. | standard |

- **Baseline:** the current skill, **pinned by git SHA**, on D1–D4 and D6 at k=3, and D5 at k=1.
- **Judge:** Claude with a blind rubric; the owner spot-checks 20%.
- **Owner signal:**
  - The primary gate signal is a **paired-transcript test**: baseline vs candidate, same dump and seed, 8 randomized pairs, about 1 hour.
  - Formatting is normalized first (labels stripped, opener collapsed), so the candidate isn't recognizable at a glance.
  - Questions are comparative:
    - "Which pushed more usefully on load-bearing claims?"
    - "Which surfaced a relevant option the simulated user had not raised?"
  - Pass: ≥6/8 on each. With no true preference there is a ≈14% chance of passing each by luck, so treat it as a directional signal, not significance.
  - Real-session ratings stay secondary at every n. At n≥6, a median at or below baseline triggers investigation, not rollback.
- **Rust:**
  - Archive committed sessions instead of deleting them (`session.rs:399`).
  - `tempyr init`, `doctor`, and `schema upgrade` make sure `.gitignore` covers `.tempyr/sessions/` and `.tempyr/spar/`.

#### S1: Skill rewrite + gate-free Rust (~5 d)

**Skill (~2 d; runs in parallel with S0).** Apply **Appendix B** to the Claude and `.agents` skills, with Codex parity.
- The owner uses the new skill on real work from about day 2. S0 pins the old skill by SHA, so the baseline is unaffected.
- Budget: ≤100 lines added, including the level digest and 3 examples; about +55 net of deletions (the deletions remove ~45 lines).
- In S1 the skill enforces one push per turn and the per-level *bars*. Session push totals wait until S2a supplies `budget_remaining`, and accept-streak callouts wait until S2b adds an `accept_streak` session counter, since the model can’t reliably count across a long transcript.
- *Optional:* a skill-only draft-then-attack arm (B).

**S1-R: Rust, no quality gates (~3 d).**
- **Phase liveness.**
  - `interview_start` no longer advances.
  - **Transitions are evaluated only in `interview_answer`.** Drop `try_advance_phase` from the reanalysis done by `add_node`, `add_edge`, and `adjust` (`proposer.rs:172`, reached via `handler.rs:1932, 2036, 2089`; also update the "may advance the phase" tool descriptions at `handler.rs:1728, 1993`). A phase can then change only at the start of a turn, after the user has replied to the push on the previous turn's nodes.
  - Each transition fires when there is ≥1 answer in the phase *and* its node condition holds, **or** when answers-in-phase ≥ N. Node conditions ignore nodes whose body contains a `needs:` line.

    | Transition | Node condition | N |
    |---|---|---|
    | Discovery | none | 1 |
    | Product | persona + metric + problem | 4 |
    | Technical | decision or component | 3 |
    | Decomposition | task | 2 |

  - That means at least 4 answers before Review, and no answer is ever forced.
  - Update the tests at `phases.rs:195-402`, `proposer.rs:362-381`, and `proposer.rs:485-530`. Add tests for "no advance from `add_node`" and "`needs:` nodes don't satisfy node conditions".
  - `interview_commit` returns a warning listing nodes that still have `^needs:` lines.
- **Reads.**
  - `interview_answer` accepts `question?`: the exact question or premise list that was asked. The `AnswerRecorded` journal event carries it.
  - `journal_search` gains `tags: [..]` and `references: [node_id]` filters, reusing `push_filters` over `entry_tags` and `entry_refs`. Today the query goes straight into FTS `MATCH`, so `disposition:rebut` can't be expressed as a query string.
  - `interview_show {full: true}` returns full bodies and fields.
  - `graph_traverse {include_body: true}` returns bodies and edges, and the dead `edge_types` filter gets implemented.
- **Gap text.** `suggested_question` and `suggested_angle` become angle hints instead of scripted questions (a string-only change).
- **Additive schema delta.**
  - `has_risk` and `has_question` on `epic`; `has_risk` on `decision` (it already has `has_question`).
  - `risk_for → epic|decision`; `question_for → epic`.
  - Epic roots gain the `MissingRisk` and `UnresolvedQuestion` root gaps, so re-baseline any epic runs.
- **`tempyr schema upgrade`.**
  - Applies **named, versioned deltas** (e.g. `2026-10-interview-edges`) via `toml_edit`, which preserves comments and layout.
  - Keys on the (node_type, edge_type, target) triple.
  - Records applied deltas in `.tempyr/config.toml`, so each delta applies once and edges the user removed are never re-added.
  - `--dry-run` (diff) is the default; `--apply` writes.
- **Journal.** Fix the missing MCP `add_node` and `add_edge` emits.

This is safe without provenance: S1 adds no quality gates, so there is nothing to game.

**Gate: S1 → scope of S2b.**
- The paired test must score ≥6/8 on both questions.
  - If it falls short, do one skill iteration of ≤2 days and re-test.
  - If it still falls short, ship the S1 skill only if the judge metrics pass. Otherwise revert the skill; S1-R stays either way.
- A divergence miss is re-checked after S3a (the kill-pass) before it is used to scope S2b.
- S2b builds only the parts whose §4.5 metrics S1 fails.

#### S3a: Skill-only `spar` + kill-pass (~2 d; needs S1-R reads)

- **`/tempyr-spar <node>`.** Spawns the `tempyr-critic` subagent with a packet built as follows:
  - **Committed nodes:** from `graph_traverse(include_body)`.
  - **Tentative nodes:** from `interview_show(full)`.
  - **Settled context:** focus, level, decided flags, and premise answers (from the recorded `question` + answer). Also prior rebuttals, found with `journal_search{tags: ["disposition:rebut"], references: [<id>]}` (filter-only, or the title quoted as an FTS phrase, since the query goes raw into `MATCH`) and `journal_blame <node file>`.
  - Never the transcript. Settled items are out of scope for the critic.
  - `needs:` lines and generated blocks are stripped from the packet.
- **Showing and resolving objections.**
  - The parent checks `quoted_claim` (normalization in §A.1) and shows the top 3 as a numbered batch.
  - The user replies in one line, e.g. "1a 2d 3r launchd handles it":
    - `a` = accept: preview "+ risk: … → risk_for X", then write on confirm.
    - `c` = change: preview a body edit, applied via `graph_update_node` (committed) or `interview_adjust` (tentative).
    - `r` = rebut, `d` = dismiss.
  - Resolutions are journaled.
- **Extractor.** Gains `weakest_claim {quote, kind, why}`: a push candidate written from a fresh context.
- **Kill-pass.**
  - Launched in the background when the phase changes into Technical.
  - Gets an *outside-view* packet: problem, personas, metrics, constraints, and settled items, with no solution nodes.
  - Returns the assumption that kills the idea and the cheapest test for it.
  - Surfaces as the push on the **first turn after it returns, in any phase**.
  - If it is still pending at wrap-up or Review, wait up to ~30s, then list it at Review.
  - Push priority when several sources compete: an in-progress `needs:` thread, then the kill-pass, then divergence, then a reframe.
  - At hard, there is also an announced pre-Review sweep ("running a red-team pass, ~30s"), and a kill-pass launches whenever the user marks a decision irreversible.
- **Gate → S3b.** Evaluated at k=3. Go to S3b if any of these hold:
  - invalid-quote or duplicate rate >20%
  - re-litigation above the §4.5 threshold
  - the kill-pass surfaces in fewer than 8/9 runs

#### S2a: Core fixes + session state (~6 d; committed)

- **Frontmatter extras.**
  - `NodeFrontmatter` gains `#[serde(flatten)] extra: BTreeMap<String, serde_yaml::Value>`.
  - **Reserved keys** (`id, type, status, created, updated, owner, tags, edges`) route to their named fields and are never written to `extra`. A round-trip test covers a colliding key.
  - `create_node_file` and `update_node` take `extra`.
- **Generated sections.**
  - Sections derived from extras (e.g. alternatives) are wrapped in `<!-- tempyr:gen alternatives -->…<!-- /tempyr:gen -->`.
  - They are regenerated byte-stably inside `ops::update_node` on **any** body or extras write, so a body-only update can't drop them. Only the marked block is replaced.
  - Marked blocks are stripped from patch bodies, and `base_body_hash` is computed with them stripped.
  - `tempyr validate` warns when extras and body drift apart.
  - S1-era sections are migrated only on request: `tempyr migrate extras`.
- **Node fields.**
  - `TentativeNode.fields` becomes `BTreeMap<String, serde_json::Value>` (backward-compatible).
  - `add_node` and `adjust` take `fields` and `source_answer`.
  - Fix the `source_qa` off-by-one.
- **Tentative patches on existing nodes** (§A.1), with key-level preflight.
- **Commit.**
  - Validate everything, then write.
  - The gate lives in `InterviewSession::commit`.
  - CLI: `tempyr interview confirm|defer`.
- **Session state.**
  - `level` and `focus`; `interview_set_level` reads the config default.
  - `premises: Vec<{id, text, state}>`.
    - Premises are registered on the premise-reply call as `interview_answer{question, premises: [text], confirms, rejects}`, because the premises are written after `interview_start` returns graph context.
    - In S2a, `confirms`/`rejects` accept premise ids only. `adopts` and `as:` arrive in S2o.
  - `interview_answer` accepts `push?: {kind, target_node, quote}`, and the `challenge_slot` response field echoes it until S2b ranks it. It also carries pending `spar`/kill-pass objections.
  - All reanalyze paths pass the graph.

#### S3b: `spar` hardening (~5 d; needs S2a)

- Adds `spar_packet`, `spar_submit`, and `spar_resolve`, with Rust validation and ranking (§A.1).
- Storage per §A.6, and the CLI `tempyr spar packet|list|resolve`.
- Interview integration through `challenge_slot`.
- Provenance-weighted ranking only if S2o exists.
- **Gate → S4.** Evaluated on the full harness: all dumps, k=5, and a cross-family judge where available. Go to S4 if any of these hold:
  - recall below min(95%, max(baseline + 30pp, 60%))
  - divergence misses its target
  - the paired test scores below 6/8

#### S2o: Origin model (~5 d; built only when S2b quality gates or S4 are triggered)

- The §A.2 table, with `origin` **required** on `add_node`.
- `confirms`, `adopts`, and `rejects {as: consider|drop, why_not}`.
- The persisted `provenance` extra.
- `field_lint`.
- `graph_update_node` removed from the skill's allowed tools.
- A warn-only numerals flag.

#### S2b: Quality gaps (~8 d; needs S2o; only the parts S1 fails)

- **Gap keys.** Per-node key `(gap_type, node_type_needed, target_node)`, plus a second detection pass over tentative nodes.
- **Filling rules.**
  - A gap is filled only via an edge to the root.
  - Quality predicates apply to tentative nodes only (§A.3).
  - Committed nodes satisfy existence gaps unless they carry `provenance`.
  - Remove the dead prefix check.
- **Gaps:** `DecisionWithoutAlternatives`, `VagueMetric`, `PremisesUnconfirmed`.
- **Phases and deferral.**
  - Phase readiness is derived from the gaps.
  - Follow-ups are counted per emitted `challenge_slot`.
  - A gap auto-defers once its push budget is exhausted.
  - `interview_defer(gap_ids | "all", up_to_phase?)`.
- **Session:** an `accept_streak` counter (consecutive accepted recommendations) that surfaces in `challenge_slot`, enabling the accept-streak callout.
- **Journal:** `GapDeferred` and `ChallengeLevelSet` events.
- **Tests:** rewrite `gaps.rs:476-515`; add per-root-type defer tests.

#### S4: Depth (~8 d)

- **Prerequisites:**
  - S2o (~5 d), plus the S2b core: gap keys, the tentative pass, `interview_defer`, and derived readiness (~6 d). Build whichever of these is missing first.
  - `ops::find_type_directory` reads the schema instead of a hardcoded list.
- **Schema:**
  - `assumption` type with `importance` and `evidence`.
  - `assumes`↔`assumed_by` and `validates`↔`validated_by` edges.
  - Field typing in `validate.rs`.
- **Remaining gaps:** `UnevidencedProblem`, `UnvalidatedAssumption`, `NoCounterMetric`/`NoNonGoals` (hard only), and structural `Contradiction`.
- **Question scoring:** priority × (1 + downstream edges) × uncertainty proxy − saturation.
- **Coverage view** in `interview_show`.
- **Arm B experiment**, if A missed its gate.

#### S5: Divergence engine + full panel (~7 d; only if S4 leaves divergence or recall short)

- **Lens-stratified alternatives:**
  1. A planning call picks lenses the user didn't cover.
  2. Generate one alternative per lens with Verbalized Sampling plus a tail pass.
  3. Dedupe by embedding and show 3.
- **Framing audit** (hard only).
- **Three-lens `spar` panel:** pre-mortem, load-bearing assumptions, and outside view (no solution nodes).
- **Optional cross-model critic** via Codex.

### 4.5 Measurement

All targets are provisional and reset after S0. N = dumps × k.

| Metric | Dumps | N (k=3 / k=5) | Provisional pass |
|---|---|---|---|
| **Load-bearing push rate**: the judge rates whether a push targets a load-bearing claim *and* leads to a changed, confirmed, or deferred node | D1, D3, D4, D6 | all pushes | ≥50% of pushes |
| Planted-flaw recall | D1, D3, D4 (9 flaws) | 27 / 45 opportunities | ≥ min(95%, max(baseline + 30pp, 60%)) |
| Contradiction surfaced | D5 | 1 / 5 runs | surfaced in ≥ (N−1) runs |
| Invented major objections | D2 | 3 / 5 runs | ≤1 run |
| Self-answer / self-fill incidents | D3 | 3 / 5 runs | ≤ baseline and ≤1 run |
| Re-litigation of settled items | D2, D4 | 6 / 10 runs | ≤1 run |
| **Divergence**: accepted non-obvious alternative or reframe (excluding exempt and mandated decisions) | D4, D6 | 6 / 10 runs | ≥1 in ≥2/3 of runs |
| Divergence precision: judge rates each shown item relevant *and* non-obvious | D4, D6 (+ D1 go-wilder output) | shown items | ≥60%; ≤1 rejected as irrelevant per run |
| Repeat-question incidents | D1–D4, D6 | 15 / 25 runs | ≤1 |
| Kill-pass surfaced (S3a+) | D1, D3, D4 | 9 / 15 runs | ≥8/9; ≤1 miss in 15 |
| Origin mislabels (S2o+) | all | nodes | ≤5% |
| **Guardrail:** median user turns to commit | per level | runs | §4.2 table; D2 ≤8 |
| **Guardrail:** proxy abandonment | D1–D4, D6 | runs | ≤ baseline + 15pp |
| **Owner paired test** (normalized formatting) | 8 pairs drawn from D1, D4, D6 | 8 | ≥6/8 on both questions (≈14% chance-pass each; directional only) |
| Owner real-session ratings | field | ≥3 per stage | secondary at every n; at n≥6, a median ≤ baseline means investigate |
| Real-world abandonment | journal (Started without Committed) | ≥20 sessions | ≤ baseline + 10pp |

These are reported as **diagnostics**, not targets:
- Push-or-No-push compliance.
- The No-push rate per dump. It should be high on D2 and low on D1 and D3.
- The premise n/unsure rate per dump. A rate near 0 suggests the premises are too easy to agree with.

**Kill and rollback.**
- Abandonment is the only standalone kill trigger: proxy abandonment above baseline + 15pp, or real-world abandonment above baseline + 15pp after 20 sessions.
- Between +10 and +15pp, investigate but don't roll back.
- A high dismiss rate triggers rollback only if recall or confirmed graph changes are also low (per Buçinca).
- The skill is a managed file, so rollback is one release.
- **Caving signal:** a high accept rate combined with low recall.

## 5. Dependencies

```
S1-skill ∥ S0 (baseline pinned by SHA) ─► S1-R (answer-only transitions, full reads, question param,
                                               journal tag/ref filters, gap text, additive edges, schema upgrade)
        ├─► S3a skill-only spar + kill-pass
        └─► S2a core fixes + session state (level/focus/premises/push/slot)
                ├─► S3b spar hardening (+ provenance ranking if S2o exists)
                └─► [S1 gate] S2o origin model ─► S2b quality gaps
        [S3b gate] ─► (S2o + S2b core, built if missing) ─► S4 depth ─► [S4 gate] S5
```

**Unsafe order:** Rust *quality gates* before S2o provenance and S2a extras. Gates would reward self-fill and drop data at commit.

## 6. Risks

- **Fatigue.** Mitigated by:
  - levels (epics default to standard) and mandate focus
  - plain questions for decided decisions
  - never forcing an answer
  - one push per turn and push budgets
  - "user decides" and wrap-up from any phase
  - turn and abandonment guardrails
- **Timidity, or compliance theater.** Mitigated by:
  - measuring load-bearing pushes, not label counts
  - the graph lens and the uncovered-lens rule
  - go-wilder
  - a configurable default level
  - the paired owner test
- **Manufactured doubt.** Mitigated by:
  - `quoted_claim` validation
  - "No push" and "no objection" as valid outcomes
  - passing settled context to critics
  - D2
  - dismiss tracking
- **Self-fill passing as rigor.** Mitigated by:
  - S2o provenance and the persisted `provenance` extra
  - `confirmed_at_commit` earning no credit
  - the numerals flag
  - D3
- **Lost user input.** Mitigated by extract-then-mark (no chat-only holds), session archiving, and listing marked nodes at Review.
- **Data loss on disk.** Mitigated by:
  - reserved-key routing
  - generated-section markers
  - key-level patch preflight with read-modify-write
  - validate-then-write commits
  - versioned schema deltas
- **Unknown skill adherence.** The "~80%" in `tempyr-interview-spec.md` is an unsourced estimate; the harness will measure actual adherence.
- **Harness cost.** Runs on subscription usage as in-session subagents; small k early, full harness only at later gates.

## 7. Unverified or weak evidence

- OntoAgent's component attribution was checked at abstract level only.
- BMAD's `methods.csv` returned 404; behavior was confirmed from its docs.
- ChatPRD's internals are unknown.
- **No study tests critic panels, or lens vs persona critics, inside a requirements interview.** The debate evidence comes from reasoning benchmarks and the HCI evidence from critiques of AI outputs. This design is extrapolation; S0–S3 exist to test it.

---

## Appendix A — Implementation contract

### A.1 MCP tools, patches, commit

| Tool | Change | Stage |
|---|---|---|
| `interview_answer` | `question?` | S1-R |
| `interview_answer` | S2a: `premises: [text]`, `confirms`/`rejects` on premise ids, `push?: {kind, target_node, quote}`, `gap_id?`; the response adds `challenge_slot?` and `budget_remaining`. S2o: `adopts: [id]` and `rejects: [{id, as: consider\|drop, why_not?}]` on any proposal. | S2a/S2o |
| `journal_search` | `tags: [..]`, `references: [node_id]` filters | S1-R |
| `interview_show` | `{full: true}` | S1-R |
| `graph_traverse` | `{include_body: true}`; implement `edge_types` | S1-R |
| `interview_add_node`, `interview_adjust` | `fields` and `source_answer?` (S2a). `origin`, **required** on add (S2o). `adjust` on an existing id creates a **tentative patch**. `field_lint` (S2o). | S2a/S2o |
| `interview_defer` | `gap_ids \| "all"`, `up_to_phase?`, `reason`. Target rule in §A.4. | S2b |
| `interview_set_level` | `level`, `focus?`. Default from `.tempyr/config.toml`. | S2a |
| `spar_packet` | Takes `(root_node, hops, lens, session_id?)`. Returns a snapshot: nodes (with origin/provenance where present), 1-hop neighbors, journal `dead_end` hits, and **settled** context. **Never the transcript.** For the outside-view lens, solution nodes are stripped. | S3b |
| `spar_submit` | `(packet_id, critic, objections[])`. **Called by the parent only.** | S3b |
| `spar_resolve` | `(objection_id, accept\|rebut\|dismiss\|change, rationale)` | S3b |

**`spar_submit` validation and ranking**
- **Drop** an objection if:
  - its target isn't in the packet;
  - its `quoted_claim` fails to match (see below); or
  - it targets a settled item.
- **`quoted_claim` matching:**
  - Normalize CRLF→LF, collapse whitespace, fold smart quotes, and compare case-insensitively.
  - The quote must be a substring of the node's body or fields, ≥15 characters, and not wholly inside the title.
  - `needs:` lines and `tempyr:gen` blocks are excluded from matching.
- **Dedupe** with strsim against existing risk/assumption nodes and journaled rebuttals on the same target. S5 switches to embeddings.
- **Rank** by severity × (1 + downstream edges) × (1.5 if `provenance`, S2o only) × (1.5 if it overlaps an open gap). Critic confidence is only a tie-breaker.

**Tentative patch**
- Records `base_body_hash`, `base_status`, and the base value of each patched extra key.
- Covers body, status, and extras. Edges stay in `tentative_edges`.
- Preflight blocks only if one of the *patched* keys changed; edge-only changes to hub nodes don't block.
- Applied as read-modify-write onto the current file.
- On conflict, `CommitBlocked::StalePatch` returns the diff plus an `interview_adjust` rebase hint.

**Commit**
1. Preflight all patches.
2. Validate everything.
3. Write.
4. Mark any open checkpoints `abandoned` and journal them.

### A.2 Origin model (S2o)

| Content | `origin` |
|---|---|
| Root body from the brain dump | `user_stated` |
| Node or field added with `source_answer` | `user_stated` |
| Premise, default, lens alternative, draft, critic output | `agent_proposed` |
| Accepted via `confirms` or `adopts` | `user_confirmed` |
| Rejected with the user's reason (`as: consider`) | `user_confirmed` + `why_not`; counts as *considered* |
| Rejected as irrelevant (`as: drop`) | removed; logged |
| Bulk-confirmed at commit | `confirmed_at_commit`; no gap or coverage credit |
| Committed node | `committed`. Satisfies existence gaps; exempt from quality gaps unless it carries `provenance`. |
| Legacy session node | `legacy`; treated as `user_stated` |

- **Persistence.**
  - Field values persist to extras. Field-level origin stays in the session and journal.
  - Non-user nodes persist a reserved extra, `provenance: agent_proposed | confirmed_at_commit`. If it's omitted, the node is user-derived.
  - `provenance` can't be set through `fields`. It's cleared only by `confirms`/`adopts` or a spar `accept`.
- **`[pN]` proposals.**
  - Each is shown once. The first time, it carries the note "(proposed — answer or ignore; ignored ones are listed at Review)".
  - Proposals still open at commit default to "keep as open question": a new `open_question` quotes the proposal and is linked per §A.4, and the original is dropped.

### A.3 Deterministic predicates (S2b/S4; tentative nodes only)

| Gap | Predicate | Stage |
|---|---|---|
| `PremisesUnconfirmed` | a session premise is still in state `open` | S2b |
| `DecisionWithoutAlternatives` | fewer *considered* alternatives (user-origin, adopted or `consider`) than the level requires: light = step 0 answered; standard = 1; hard = 2. `do_nothing` counts only if user-origin. Decided decisions are exempt. | S2b |
| `VagueMetric` | `target` is missing, not user-origin, or not parseable as a number, threshold, or date. At light, `done_when` is accepted. | S2b |
| `UnevidencedProblem` | `evidence` ∉ {incident, data, mandate} | S4 |
| `UnvalidatedAssumption` | `importance=high` ∧ `evidence ∈ {none, anecdotal}` ∧ `untested` | S4 |
| `NoCounterMetric` (hard) | has `measured_by` edges, none with `kind=guardrail` | S4 |
| `NoNonGoals` (hard) | `non_goals` is empty | S4 |
| `Contradiction` | structural only: `depends_on` a superseded decision, or conflicting statuses | S4 |

The extractor fills fields; Rust only checks them. The LLM still does extraction only.

### A.4 Schema delta and defer targets

- **S1-R delta `2026-10-interview-edges` (additive):**
  - `epic` gains `has_risk` and `has_question`.
  - `decision` gains `has_risk`; it already has `has_question`.
  - `risk` gains `risk_for → epic|decision`.
  - `open_question` gains `question_for → epic`.
  - The reverse pairs already exist in `[edge_types]`.
- **S4:**
  - `assumption` type, with statuses `untested` / `validated` / `invalidated`.
  - `assumes`↔`assumed_by` and `validates`↔`validated_by` edges.
  - A `field_types` table checked by `validate.rs`.
  - A `classify_edge_gap` arm for `assumes`.
  - `templates/tdd.toml` sections for generated blocks.
- **Deferred:** `considered` edges and a `rejected` status. Linear would show them as blockers.
- **Missing types or fields:** gaps become no-ops when the project schema lacks them.
- **Defer target rule.** Try each in order:
  1. Link to `target_node`, if a question edge to it is valid.
  2. Otherwise link to the root, if that's valid.
  3. Otherwise append to the target's `## Open questions` body section.

  At light, deferrals always go to the root's body section.

### A.5 Files, journal, parity

- **Managed files** (`managed.rs` MANAGED_FILES):
  - `.claude/agents/tempyr-critic.md`: one agent, with the lens passed in.
  - The `/tempyr-spar` skill.
  - `tempyr-extractor.md`, which gains `weakest_claim` (S3a) and `origin`/`fields` (S2a/S2o).
- **Skill allowed-tools.**
  - S1 adds `journal_search` and `journal_blame`. `graph_traverse` is already allowed.
  - `graph_update_node` stays in S1, but only for committed-node edits the user explicitly asks for, and only after commit (Appendix B). It is removed in S2o.
- **Mirrors.** Keep `.agents/skills` in sync, with Codex parity. Codex's `spar` fallback is a sequential critique that reads only the packet.
- **Journal events.**
  - `GapDeferred` and `ChallengeLevelSet`.
  - `spar` `ObjectionRaised` and `ObjectionResolved`, each with the full payload. Every resolution entry sets:
    - `references: [target_id]`
    - `tags: [objection:<id>, disposition:<…>]`
    - `files: [node path]` when the target is committed

    This lets `journal_search{tags, references}` and `journal_blame` find it.

### A.6 Spar storage and lifecycle (S3b)

- **Packet storage.** `.tempyr/spar/<packet_id>.json`, per-worktree like sessions and gitignored. Each file holds a snapshot of the packet content, and `quoted_claim` is validated against that snapshot.
- **Resolutions.** Journaled with the full payload (§A.5). Rebuttals travel via journal refs, and dedupe uses `journal_search{tags: ["disposition:rebut"], references: [target]}`.
- **Renames.** A tentative rename (`adjust new_id`) remaps targets in the session's open packets.
- **Cleanup.** Packets are dropped only if a `session_id` was given and that session is gone.
- **CLI.** `tempyr spar packet <node>` prints a packet for any agent; `tempyr spar list|resolve` handles the rest. Tempyr has no LLM client, so `/tempyr-spar` is the primary UX.

---

## Appendix B — S1 skill diff (Claude skill; mirror to `.agents`, Codex parity)

**Delete / replace in SKILL.md**

| Lines | Current | Action |
|---|---|---|
| :28-30 | "Your job is the CONVERSATION — phrasing questions naturally…" | Replace with "Your role" below. |
| :37-38 | "If you think you know the answer from context, you still ask…" | Replace with "If the graph states a fact, show it with its node id so the user can correct it; if the code suggests something, put it to the user as a question or premise. Ask only about preferences and unknowns." |
| :41 | "After calling `interview_start`, present the gaps/questions and **stop**." | Change to "present the opener and **stop**." |
| :44-45 | "Do NOT pre-fill answers from existing graph context." | Change to "Never put graph facts into `interview_answer`." |
| :57-59 | Starting-an-interview steps 3–5 | Replace with "Opening turn" below. |
| :67-70, :91-93 | `graph_update_node` for extracted entities that already exist | Replace with "Existing nodes" below. |
| :82 | "Ask the next 2-3 questions from the server's gap list" | Delete. |
| :95-104 | Compact node format | Merge into "Recorded". |
| :106-128 | Canned phrasing and example questions | Collapse to "Use `suggested_angle` and `existing_related` as hints; cite related nodes by id." |
| :132 | "NEVER ask more than 3 questions per turn" | Append "(the opener's premise list is exempt)". |
| :134-138 | Per-`question_type` phrasing rules | Delete. |
| :142-146 | "acknowledge the shift naturally:" plus the scripted phase-change line | Delete. |
| :152-153 | "just follow the `next_questions` it returns" | Delete. |
| :162-163 | Skip hack (non-user text in `interview_answer`) | Replace with "Review" below. |
| :169-180 | Review steps 1–7 | Replace with "Review" below. |
| :188 | "Continue asking questions from `next_questions`" | Change to "Summarize state and continue the open thread." |

**Add** (allowed-tools gains `journal_search` and `journal_blame`):

```markdown
## Your role
You are a skeptical design partner, not a scribe. Help the user find the weakest
parts of their idea and the options they haven't considered — then they decide.
Your views, premises, and alternatives go in YOUR message; only the user's words
go in `interview_answer` (pass the exact question you asked as `question`).
The server's gaps are a coverage checklist, not a script.

## Opening turn
Show related graph context, the root, one line — "I'll push at <level>: say
lighter/harder, 'go wilder', or 'wrap up' anytime" (+ focus if any) — and
numbered premises. Each premise names an assumption the plan depends on and what
would make it false; the user replies y / n / unsure ("1y 2n 3?"). Put an
evidence question on the most load-bearing one instead of y/n. No gap questions.

## Levels
- light (hobby/side project/"just me"): 1–2 premises; 1 push per topic; a "done when…" is enough
  for a metric; step 0 only for alternatives; no reframes.
- standard (default, incl. epics): 3 premises; up to 2 pushes per topic; metrics need a
  number/threshold/date; step 0 → graph → one uncovered lens; product roots
  get one reframe (Discovery→Product).
- hard (on request, irreversible, launch): 5 premises; 2 pushes + an evidence probe
  per topic; + guardrail metric; ≥2
  alternatives considered per open decision; reframes at Discovery→Product and
  Product→Technical.
Compliance/audit/policy → mandate focus: test whether the plan fully closes the
mandate; no alternatives for mandated choices.

## Each turn
- Recorded: what you captured (a compact `+ type: title` list if >2 items).
- At most ONE push, quoting the user's words: specificity, evidence ("When did
  this last happen? Walk me through it."), strongest objection (steelman first;
  say what would change your mind), alternatives, or a reframe question.
  Priority when several apply: an unfinished `needs:` thread > kill-pass result >
  divergence > reframe. Where they'd expect pushback but the answer is solid:
  "No push: <reason>" — vary the reason. Logistical answers need neither.
- At most 2 questions: a follow-up on the user's words, or the highest-impact
  open gap phrased specifically for this project — never a generic question.
These describe the shape, not literal headers; write naturally.

## Weak answers
Extract immediately. If an answer fails the level's bar (metric without a
threshold, persona without a goal, open decision with nothing considered), add
the node with a line `needs: <what's missing>` in its body and make it this
turn's push. Refine with `interview_adjust` and remove the line when resolved.
If the user moves on, treat it as declined. If a phase change arrives
mid-thread, finish the thread first.

## Existing nodes
Don't edit committed nodes mid-interview. Link to them with `interview_add_edge`
and note intended changes in the root body under `## Changes to existing nodes`.
"Extend <id>" means: add the schema-valid edge from the root to <id> (`depends_on` for
feature→feature, `parent_of` for epic→feature) plus that note. Use
`graph_update_node` only after commit, and only if the user explicitly asks.

## Stance
- Facts: hold your position unless given new evidence. Preferences/decisions:
  the user's word is final — record their reason, state dissent once in one line,
  never re-raise. Never re-open settled items (answered premises, decided
  decisions, rebuttals).
- No empty agreement ("Great idea", "That could work").
- Avoid: two topics in one question; asking the user to design the solution when
  the gap is the problem; moving past a contradiction.

## Divergence
- Open decisions: "What else did you consider, and why not?" Then check
  `graph_search`/`journal_search` for overlapping nodes or dead-ends — if found,
  offer "extend <id>?" or "this was tried: <dead_end>". Then (standard+) also
  offer ONE option from a lens they didn't cover, with a one-line "fails if";
  if they gave <2 options or all from one lens, offer minimal / ideal / lateral.
  Lenses: build·buy·don't, 1/10th budget, 10× scale, remove the need, what
  substitutes do, boring option, subtract something. They mark each adopt /
  consider (why not) / drop; record under `## Alternatives considered`.
- Agent proposals show as `[pN]`, numbered across the session. First time, add
  "(proposed — answer or ignore; ignored ones are listed at Review)".
- Decided decisions: one plain batched question, not a push: "X / Y — anything
  you ruled out? one line each."
- "go wilder": run the best-fit of pre-mortem / inversion / assumption audit /
  reframe / subtraction now and name two others as optional next picks. It
  doesn't count as your push.

## Review
When the server returns Review, or the user says "wrap up" / "commit" /
"enough" from any phase:
1. If a push is still worth spending, use it on the highest-impact open
   decision or `needs:` node first.
2. `interview_show {full: true}`, then summarize by type (features with
   personas/metrics/constraints, decisions with rationale and alternatives,
   tasks, risks, open questions) and "X nodes, Y edges".
3. List nodes still marked `needs:` and unanswered `[pN]`. For each, the user
   resolves it now or it becomes an open question (root `## Open questions` at
   light, or if no valid edge exists; `## Risks` likewise) — remove the
   `needs:` line when converting.
4. On approval, `interview_commit`; report files and warnings; mention
   `tempyr render prd|tdd <id>`.
Never send non-user text to `interview_answer`.
```

**Examples** (included in the skill; their domains must not overlap any eval dump)

1. **Opener, standard.** Brain dump: "offline mode for our recipe app".
   - **AI:** Related: recipe-sync-a1b2c3 (sync conflicts are an open question there). + feature "offline mode". I'll push at *standard* — say lighter/harder, 'go wilder', or 'wrap up' anytime.
     1. Users lose recipes mid-cook when signal drops — false if support tickets don't mention it. When did someone last hit this?
     2. Read-only offline is enough — false if people edit while cooking.
     3. Sync conflicts are rare — false if households share recipe boxes.

2. **Open decision, standard.**
   - **User:** "We'll store on-call rotations in Postgres; considered a spreadsheet."
   - **AI:** + decision "rotations in Postgres" (considered: spreadsheet). This overlaps pagerduty-sync-d4e5f6 — extend it instead? [p1]. One lens you didn't cover, *remove the need*: derive rotations from PagerDuty schedules and store nothing [p2] — fails if you need rotation history PagerDuty doesn't keep. (proposed — answer or ignore; ignored ones are listed at Review)

3. **Restraint, mandate focus.**
   - **User:** "Retention: delete chat logs after 90 days, per the records policy."
   - **AI:** + constraint "chat logs ≤90 days". No push: it's the policy verbatim, with a clear owner. Does the 90 days include backups, or only the primary store?
