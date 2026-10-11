export const meta = {
  name: 'tempyr-interview-eval',
  description: 'Run interview-skill arms against simulated users on fixed scenarios, judge each transcript, and aggregate metrics',
  whenToUse: 'Invoked by the /tempyr-eval skill from the eval sandbox project',
  phases: [
    { title: 'Interview', detail: 'interviewer and simulated user alternate, one fresh agent per turn' },
    { title: 'Judge', detail: 'blind rubric grading of each transcript plus session snapshot' },
  ],
}

// args: {
//   rubricPath, arms: [{name, skillPath}],
//   scenarios: [{id, title, k, brainDump, personaPath, judgePath, scores: [..]}],
//   maxUserTurns
// }
const A = args || {}
const ARMS = A.arms || []
const SCENARIOS = A.scenarios || []
const MAX_USER_TURNS = A.maxUserTurns || 16

const INTERVIEWER_SCHEMA = {
  type: 'object',
  properties: {
    message: { type: 'string', description: 'Exactly what you would show the user this turn' },
    session_id: { type: 'string' },
    phase: { type: 'string', description: 'Interview phase reported by the server after this turn' },
    done: {
      type: 'boolean',
      description: 'True only when the user approved committing and you have given the final review; you did NOT call interview_commit',
    },
  },
  required: ['message', 'session_id', 'phase', 'done'],
}

const USER_SCHEMA = {
  type: 'object',
  properties: {
    reply: { type: 'string' },
    gave_up: { type: 'boolean' },
  },
  required: ['reply', 'gave_up'],
}

const INT = { type: 'integer', minimum: 0 }
const JUDGE_SCHEMA = {
  type: 'object',
  properties: {
    flaws: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          id: { type: 'string' },
          surfaced: { type: 'boolean' },
          user_turn: { type: ['integer', 'null'] },
          evidence: { type: 'string' },
        },
        required: ['id', 'surfaced', 'user_turn', 'evidence'],
      },
    },
    pushes: {
      type: 'object',
      properties: {
        count: INT,
        load_bearing: INT,
        no_push_count: INT,
        post_answer_turns: INT,
        turns_with_push_or_no_push: INT,
      },
      required: ['count', 'load_bearing', 'no_push_count', 'post_answer_turns', 'turns_with_push_or_no_push'],
    },
    invented_major_objections: INT,
    self_answer_incidents: INT,
    relitigation_incidents: INT,
    repeat_question_incidents: INT,
    divergence: {
      type: 'object',
      properties: {
        shown: INT,
        accepted_non_obvious: INT,
        relevant_non_obvious: INT,
        rejected_irrelevant: INT,
      },
      required: ['shown', 'accepted_non_obvious', 'relevant_non_obvious', 'rejected_irrelevant'],
    },
    premises: {
      type: 'object',
      properties: { count: INT, n_or_unsure: INT },
      required: ['count', 'n_or_unsure'],
    },
    contradiction_surfaced: { type: ['boolean', 'null'] },
    reached_review: { type: 'boolean' },
    scenario_checks: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          check: { type: 'string' },
          passed: { type: ['boolean', 'null'] },
          evidence: { type: 'string' },
        },
        required: ['check', 'passed', 'evidence'],
      },
    },
    notes: { type: 'string' },
  },
  required: [
    'flaws', 'pushes', 'invented_major_objections', 'self_answer_incidents',
    'relitigation_incidents', 'repeat_question_incidents', 'divergence', 'premises',
    'contradiction_surfaced', 'reached_review', 'scenario_checks', 'notes',
  ],
}

function renderTranscript(transcript) {
  if (!transcript.length) return '(no messages yet)'
  let userTurn = 0
  return transcript
    .map((m) => {
      if (m.who === 'user') {
        userTurn += 1
        return `USER (turn ${userTurn}):\n${m.text}`
      }
      return `INTERVIEWER:\n${m.text}`
    })
    .join('\n\n---\n\n')
}

const MCP_TOOLS =
  'mcp__tempyr__interview_start,mcp__tempyr__interview_answer,mcp__tempyr__interview_show,' +
  'mcp__tempyr__interview_adjust,mcp__tempyr__interview_add_node,mcp__tempyr__interview_add_edge,' +
  'mcp__tempyr__graph_search,mcp__tempyr__graph_list,mcp__tempyr__graph_context,' +
  'mcp__tempyr__graph_traverse,mcp__tempyr__graph_get_node,mcp__tempyr__journal_search'

function interviewerPrompt(arm, sc, seed, sessionId, transcript) {
  const start = sessionId
    ? `The interview is in progress with session_id \`${sessionId}\`. The user's latest reply is the last USER message below. Handle it exactly as the skill says (record it with interview_answer first, extract, then write your reply).`
    : `This is the very first turn. The user's brain dump is:\n\n"""\n${sc.brainDump}\n"""\n\nStart the interview as the skill says (interview_start with the brain dump, then your opening message).`
  return `You are the main Claude Code agent running an interview skill, inside an automated evaluation (scenario ${sc.id}, run ${seed + 1}).

Read the skill file at \`${arm.skillPath}\` and follow it exactly, as if it had been loaded for this conversation. You can't talk to the user directly: you see the conversation so far below, and your return value's \`message\` is shown to them.

Tools: load the tempyr MCP tools with ToolSearch, query "select:${MCP_TOOLS}".

Evaluation rules (these override the skill only where they conflict):
- Never call \`interview_commit\`. When the user approves committing and you have given your final review, return \`done: true\` instead.
- Don't spawn subagents; do any extraction yourself.
- Don't read files other than the skill file. Don't mention that this is an evaluation.

${start}

Conversation so far:

${renderTranscript(transcript)}

Return: \`message\` (exactly what you'd show the user), \`session_id\`, \`phase\` (as reported by the server), and \`done\`.`
}

function userPrompt(sc, transcript) {
  return `You are role-playing the user in a product-design conversation with an AI interviewer. Read your persona card at \`${sc.personaPath}\` and stay in character.

Rules:
- Reply only to the interviewer's latest message, in your persona's style and length.
- Reveal hidden facts only when a question gets at them; never state the persona card's framing outright.
- Never invent facts beyond your card; say you don't know instead.
- Follow the card's behaviour rules (e.g. when to say "go wilder", when to approve, when to give up).
- If you give up, set \`gave_up: true\` and make \`reply\` start with "GIVE_UP:".
- Don't read any file other than your persona card.

Conversation so far (you are USER):

${renderTranscript(transcript)}`
}

function judgePrompt(sc, run) {
  return `Grade one interview transcript. Read the rubric at \`${A.rubricPath}\` and this scenario's judge notes at \`${sc.judgePath}\`.

Then load \`mcp__tempyr__interview_show\` with ToolSearch ("select:mcp__tempyr__interview_show") and call it with session_id \`${run.sessionId || '(none)'}\` and \`full: true\` to see recorded answers and tentative nodes. If the session id is missing or the call fails, grade from the transcript alone and say so in notes.

Run ended by: ${run.endReason}.

Transcript:

${renderTranscript(run.transcript)}`
}

async function runInterview(item) {
  const { arm, sc, seed } = item
  const tag = `${arm.name}/${sc.id}/r${seed + 1}`
  const transcript = []
  let sessionId = null
  let endReason = 'turn_cap'
  let lastPhase = null
  let userTurns = 0

  let turn = await agent(interviewerPrompt(arm, sc, seed, null, transcript), {
    label: `${tag} interviewer t0`,
    phase: 'Interview',
    schema: INTERVIEWER_SCHEMA,
  })
  if (!turn) return { ...item, transcript, sessionId, endReason: 'interviewer_failed', userTurns, lastPhase }
  sessionId = turn.session_id
  lastPhase = turn.phase
  transcript.push({ who: 'interviewer', text: turn.message })

  while (userTurns < MAX_USER_TURNS) {
    const user = await agent(userPrompt(sc, transcript), {
      label: `${tag} user t${userTurns + 1}`,
      phase: 'Interview',
      schema: USER_SCHEMA,
      effort: 'low',
    })
    if (!user) {
      endReason = 'user_failed'
      break
    }
    userTurns += 1
    transcript.push({ who: 'user', text: user.reply })
    if (user.gave_up) {
      endReason = 'gave_up'
      break
    }

    turn = await agent(interviewerPrompt(arm, sc, seed, sessionId, transcript), {
      label: `${tag} interviewer t${userTurns}`,
      phase: 'Interview',
      schema: INTERVIEWER_SCHEMA,
    })
    if (!turn) {
      endReason = 'interviewer_failed'
      break
    }
    sessionId = turn.session_id || sessionId
    lastPhase = turn.phase
    transcript.push({ who: 'interviewer', text: turn.message })
    if (turn.done) {
      endReason = 'approved'
      break
    }
  }

  log(`${tag}: ${endReason} after ${userTurns} user turn(s)`)
  return { ...item, transcript, sessionId, endReason, userTurns, lastPhase }
}

async function judgeRun(run) {
  const verdict = await agent(judgePrompt(run.sc, run), {
    label: `${run.arm.name}/${run.sc.id}/r${run.seed + 1} judge`,
    phase: 'Judge',
    schema: JUDGE_SCHEMA,
  })
  return { ...run, judge: verdict }
}

// ---- aggregation (deterministic) ----

function median(xs) {
  if (!xs.length) return null
  const s = [...xs].sort((a, b) => a - b)
  const mid = Math.floor(s.length / 2)
  return s.length % 2 ? s[mid] : (s[mid - 1] + s[mid]) / 2
}

function ratio(num, den) {
  return den ? Math.round((num / den) * 1000) / 1000 : null
}

function summarize(runs) {
  const byArm = {}
  for (const arm of ARMS) {
    const rs = runs.filter((r) => r && r.arm.name === arm.name)
    const judged = rs.filter((r) => r.judge)
    const scored = (key) => judged.filter((r) => (r.sc.scores || []).includes(key))
    const sum = (list, f) => list.reduce((acc, r) => acc + (f(r) || 0), 0)

    const recallRuns = scored('recall')
    const flaws = recallRuns.flatMap((r) => r.judge.flaws || [])
    const pushRuns = judged.filter((r) => ['D1', 'D3', 'D4', 'D6'].includes(r.sc.id))
    const divRuns = scored('divergence')
    const precisionRuns = judged.filter((r) => ['D1', 'D4', 'D6'].includes(r.sc.id))
    const abandonRuns = rs.filter((r) => (r.sc.scores || []).includes('abandonment'))

    const turnsByScenario = {}
    for (const sc of SCENARIOS) {
      turnsByScenario[sc.id] = median(
        rs.filter((r) => r.sc.id === sc.id && r.endReason === 'approved').map((r) => r.userTurns),
      )
    }

    byArm[arm.name] = {
      runs: rs.length,
      judged: judged.length,
      planted_flaw_recall: ratio(flaws.filter((f) => f.surfaced).length, flaws.length),
      load_bearing_push_rate: ratio(
        sum(pushRuns, (r) => r.judge.pushes.load_bearing),
        sum(pushRuns, (r) => r.judge.pushes.count),
      ),
      invented_objection_runs_D2: scored('invented_objections').filter(
        (r) => r.judge.invented_major_objections > 0,
      ).length,
      self_answer_runs_D3: scored('self_answer').filter((r) => r.judge.self_answer_incidents > 0)
        .length,
      relitigation_runs: scored('relitigation').filter((r) => r.judge.relitigation_incidents > 0)
        .length,
      divergence_runs_with_accept: divRuns.filter((r) => r.judge.divergence.accepted_non_obvious > 0)
        .length,
      divergence_runs: divRuns.length,
      divergence_precision: ratio(
        sum(precisionRuns, (r) => r.judge.divergence.relevant_non_obvious),
        sum(precisionRuns, (r) => r.judge.divergence.shown),
      ),
      repeat_question_incidents: sum(judged, (r) => r.judge.repeat_question_incidents),
      contradiction_surfaced_D5: scored('contradiction').map((r) => r.judge.contradiction_surfaced),
      median_user_turns_to_approval: turnsByScenario,
      proxy_abandonment: ratio(
        abandonRuns.filter((r) => r.endReason === 'gave_up').length,
        abandonRuns.length,
      ),
      diagnostics: {
        push_or_no_push_compliance: ratio(
          sum(judged, (r) => r.judge.pushes.turns_with_push_or_no_push),
          sum(judged, (r) => r.judge.pushes.post_answer_turns),
        ),
        no_push_rate_by_scenario: Object.fromEntries(
          SCENARIOS.map((sc) => {
            const list = judged.filter((r) => r.sc.id === sc.id)
            return [
              sc.id,
              ratio(
                sum(list, (r) => r.judge.pushes.no_push_count),
                sum(list, (r) => r.judge.pushes.post_answer_turns),
              ),
            ]
          }),
        ),
        premise_n_or_unsure_rate: ratio(
          sum(judged, (r) => r.judge.premises.n_or_unsure),
          sum(judged, (r) => r.judge.premises.count),
        ),
        end_reasons: rs.reduce((acc, r) => {
          acc[r.endReason] = (acc[r.endReason] || 0) + 1
          return acc
        }, {}),
      },
    }
  }
  return byArm
}

// ---- run ----

const items = []
for (const arm of ARMS) {
  for (const sc of SCENARIOS) {
    for (let seed = 0; seed < (sc.k || 1); seed++) items.push({ arm, sc, seed })
  }
}
log(`${items.length} runs: ${ARMS.length} arm(s) x ${SCENARIOS.length} scenario(s), max ${MAX_USER_TURNS} user turns each`)

const runs = await pipeline(items, runInterview, judgeRun)
const done = runs.filter(Boolean)
if (done.length < items.length) log(`${items.length - done.length} run(s) failed and were dropped`)

return {
  summary: summarize(done),
  runs: done.map((r) => ({
    arm: r.arm.name,
    scenario: r.sc.id,
    run: r.seed + 1,
    session_id: r.sessionId,
    end_reason: r.endReason,
    user_turns: r.userTurns,
    last_phase: r.lastPhase,
    transcript: r.transcript,
    judge: r.judge,
  })),
}
