# Interview eval rubric (judge)

You are grading one interview transcript produced by an AI interviewer running a Tempyr
interview skill against a simulated user. You do not know which skill version produced it;
grade only what is in the transcript and the session snapshot.

Inputs you get:
- the transcript (interviewer and user turns, numbered);
- the scenario's judge notes (planted flaws and scenario checks);
- the interview `session_id` — call `interview_show` with `{session_id, full: true}` to see
  the recorded answers (`qa_history`) and the tentative nodes and edges. (If `full` isn't
  supported, call it without.)

Grade strictly from evidence. Quote short phrases from the transcript as evidence.

## Definitions

**Turn.** A user turn is one simulated-user message. A *post-answer interviewer turn* is
any interviewer message after the first.

**Push.** One challenge in an interviewer turn that quotes or directly targets something
the user said: a specificity push ("what number?"), an evidence probe ("when did this last
happen?"), an objection (stating a risk or weakness), an alternatives set, or a reframe
question. Plain coverage questions ("who are the users?") are not pushes.

**Load-bearing push.** A push that targets a claim the plan depends on, *and* that leads to
an outcome: the user changed the plan, confirmed with new specifics, or explicitly deferred
it as an open question/risk. Pushes the user ignored, or that targeted cosmetic details,
are not load-bearing.

**"No push".** An explicit statement that the answer is solid ("No push: …"). Count
separately.

**Planted flaw surfaced.** The interviewer raised the flaw clearly enough that the user
engaged with it (per the judge notes' criterion). Record the user turn it surfaced on.
List **every** planted flaw from the judge notes in `flaws`, surfaced or not.

**Invented major objection.** An objection that is false, treats a decided or mandated
choice as open, or demands evidence that doesn't apply (e.g. user-pain evidence for an
audit mandate).

**Self-answer incident.** A recorded answer in `qa_history` containing content the user
did not say, or a node/field asserting a specific value the user never gave that is not
marked proposed or `needs:`.

**Re-litigation.** Re-raising something the user already settled (answered premise,
decided decision, rebutted objection) without new information.

**Repeat question.** Asking for information the user already gave.

**Divergence item.** An alternative option or reframe the interviewer offered. It's
*non-obvious* if it isn't simply a restatement or the default approach in the user's own
framing. It's *accepted* if the user adopted it or kept it for consideration with interest.

**Premises.** If the opener lists numbered premises, count them and how many the user
answered n / unsure.

## Output

Fill every field of the output schema. Use `null` for checks that don't apply to this
scenario. Keep `notes` to 2–4 sentences on what most helped or hurt.
