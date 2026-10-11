#!/usr/bin/env bash
# Build the interview-eval workspace (outside the repo).
#
# Usage: evals/interview/setup.sh [--dir <path>] [--baseline <git-ref>] [--candidate <git-ref>]
#   --dir        workspace root (default: $TEMPYR_EVAL_DIR or ~/.tempyr-eval/interview)
#   --baseline   ref whose interview skill is the "baseline" arm
#                (default: the commit before the "skeptical design partner" rewrite)
#   --candidate  ref whose interview skill is the "candidate" arm
#                (default: the working tree's .claude/skills/tempyr-interview/SKILL.md)
#
# Layout:
#   <dir>/sandbox  tempyr project + its own git repo; start Claude Code here. Rebuilt.
#   <dir>/harness  workflow.js, rubric, critic, scenarios, skill arms. Rebuilt.
#   <dir>/results  eval results. Kept across rebuilds.
# The workspace lives outside the repo so agents don't pick up this repo's CLAUDE.md
# or skills, journal events stay out of its journal, and the scenario files sit
# outside the interviewer's working directory.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(git -C "$here" rev-parse --show-toplevel)"
root="${TEMPYR_EVAL_DIR:-$HOME/.tempyr-eval/interview}"
baseline_ref=""
candidate_ref=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dir) root="$2"; shift 2 ;;
    --baseline) baseline_ref="$2"; shift 2 ;;
    --candidate) candidate_ref="$2"; shift 2 ;;
    -h|--help) sed -n '2,17p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

skill_path=".claude/skills/tempyr-interview/SKILL.md"

# Print the interview skill at a git ref. MSYS_NO_PATHCONV stops Git Bash on Windows
# from rewriting the "ref:path" argument into a Windows path.
show_skill() {
  (cd "$repo" && MSYS_NO_PATHCONV=1 git show "$1:$skill_path")
}

if [[ -z "$baseline_ref" ]]; then
  rewrite="$(cd "$repo" && git log --reverse --format=%H -S"skeptical design partner" -- "$skill_path" | head -n1)"
  baseline_ref="${rewrite:+$rewrite^}"
  baseline_ref="${baseline_ref:-origin/master}"
fi

if ! command -v tempyr >/dev/null 2>&1; then
  echo "tempyr is not on PATH. Build it from this repo first, e.g.:" >&2
  echo "  cargo install --path crates/tempyr-cli --locked" >&2
  exit 1
fi
# The candidate skill relies on interview_answer's question param, interview_show
# {full}, and journal_search tag filters; the same build added `journal search --tag`.
if ! tempyr journal search --help 2>/dev/null | grep -q -- "--tag"; then
  echo "The tempyr on PATH predates the interview-phase changes (no 'journal search --tag')." >&2
  echo "Reinstall from this repo: cargo install --path crates/tempyr-cli --locked" >&2
  exit 1
fi

sandbox="$root/sandbox"
harness="$root/harness"
rm -rf "$sandbox" "$harness"
mkdir -p "$sandbox" "$harness/arms/baseline" "$harness/arms/candidate" "$root/results"

cp -R "$here/template/." "$sandbox/"
cp "$repo/schema/default-schema.toml" "$sandbox/.tempyr/schema.toml"

cp -R "$here/scenarios" "$harness/scenarios"
cp "$here/rubric.md" "$here/workflow.js" "$harness/"
# The kill-pass critic's instructions; the harness runs it for arms whose skill has a kill-pass.
if [[ -f "$repo/.claude/agents/tempyr-critic.md" ]]; then
  cp "$repo/.claude/agents/tempyr-critic.md" "$harness/critic.md"
fi

show_skill "$baseline_ref" > "$harness/arms/baseline/SKILL.md"
(cd "$repo" && MSYS_NO_PATHCONV=1 git rev-parse "$baseline_ref") > "$harness/arms/baseline/SOURCE"
if [[ -n "$candidate_ref" ]]; then
  show_skill "$candidate_ref" > "$harness/arms/candidate/SKILL.md"
  (cd "$repo" && MSYS_NO_PATHCONV=1 git rev-parse "$candidate_ref") > "$harness/arms/candidate/SOURCE"
else
  cp "$repo/$skill_path" "$harness/arms/candidate/SKILL.md"
  {
    git -C "$repo" rev-parse HEAD
    git -C "$repo" diff --quiet HEAD -- "$skill_path" || echo "(plus uncommitted changes)"
  } > "$harness/arms/candidate/SOURCE"
fi

(
  cd "$sandbox"
  git init -q
  git config core.autocrlf false
  git add -A
  git -c user.name=tempyr-eval -c user.email=tempyr-eval@localhost commit -q -m "eval sandbox"
  tempyr validate
  tempyr index rebuild --skip-embeddings >/dev/null
)

echo
echo "Eval workspace ready: $root"
echo "  baseline:  $(head -n1 "$harness/arms/baseline/SOURCE")"
echo "  candidate: $(tr '\n' ' ' < "$harness/arms/candidate/SOURCE")"
echo
echo "Next:"
echo "  cd \"$sandbox\" && claude"
echo "  then run /tempyr-eval"
