#!/usr/bin/env bash
# Build the interview-eval sandbox at evals/interview/.run/sandbox.
#
# Usage: evals/interview/setup.sh [--baseline <git-ref>] [--candidate <git-ref>]
#   --baseline   ref whose interview skill is the "baseline" arm (default: origin/master)
#   --candidate  ref whose interview skill is the "candidate" arm
#                (default: the working tree's .claude/skills/tempyr-interview/SKILL.md)
#
# The sandbox is its own git repo so interview journal events stay out of the tempyr
# repo's journal, and its own tempyr project so eval sessions never touch a real graph.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(git -C "$here" rev-parse --show-toplevel)"
baseline_ref="origin/master"
candidate_ref=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --baseline) baseline_ref="$2"; shift 2 ;;
    --candidate) candidate_ref="$2"; shift 2 ;;
    -h|--help) sed -n '2,11p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

skill_path=".claude/skills/tempyr-interview/SKILL.md"

# Print the interview skill at a git ref. MSYS_NO_PATHCONV stops Git Bash on Windows
# from rewriting the "ref:path" argument into a Windows path.
show_skill() {
  (cd "$repo" && MSYS_NO_PATHCONV=1 git show "$1:$skill_path")
}

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

run="$here/.run/sandbox"
rm -rf "$run"
mkdir -p "$run"

cp -R "$here/template/." "$run/"
cp -R "$here/scenarios" "$run/scenarios"
cp "$here/rubric.md" "$here/workflow.js" "$run/"
cp "$repo/schema/default-schema.toml" "$run/.tempyr/schema.toml"

mkdir -p "$run/arms/baseline" "$run/arms/candidate"
show_skill "$baseline_ref" > "$run/arms/baseline/SKILL.md"
git -C "$repo" rev-parse "$baseline_ref" > "$run/arms/baseline/SOURCE"
if [[ -n "$candidate_ref" ]]; then
  show_skill "$candidate_ref" > "$run/arms/candidate/SKILL.md"
  git -C "$repo" rev-parse "$candidate_ref" > "$run/arms/candidate/SOURCE"
else
  cp "$repo/$skill_path" "$run/arms/candidate/SKILL.md"
  {
    git -C "$repo" rev-parse HEAD
    git -C "$repo" diff --quiet HEAD -- "$skill_path" || echo "(plus uncommitted changes)"
  } > "$run/arms/candidate/SOURCE"
fi

(
  cd "$run"
  git init -q
  git config core.autocrlf false
  git add -A
  git -c user.name=tempyr-eval -c user.email=tempyr-eval@localhost commit -q -m "eval sandbox"
  tempyr validate
  tempyr index rebuild --skip-embeddings >/dev/null
)

echo
echo "Sandbox ready: $run"
echo "  baseline:  $(head -n1 "$run/arms/baseline/SOURCE")"
echo "  candidate: $(tr '\n' ' ' < "$run/arms/candidate/SOURCE")"
echo
echo "Next:"
echo "  cd \"$run\" && claude"
echo "  then run /tempyr-eval"
