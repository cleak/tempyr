#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: bash install.sh [--install-root PATH] [--no-path-update]

Installs Tempyr from this checkout with cargo into a Tempyr-owned install root.

Options:
  --install-root PATH  Override the install root.
  --no-path-update     Skip shell profile updates.
  -h, --help           Show this help text.
EOF
}

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "install.sh currently supports Linux only. Use install.ps1 on Windows." >&2
  exit 1
fi

INSTALL_ROOT="${TEMPYR_INSTALL_ROOT:-${XDG_DATA_HOME:-$HOME/.local/share}/tempyr}"
SKIP_PATH_UPDATE=0

while (($# > 0)); do
  case "$1" in
    --install-root)
      if (($# < 2)); then
        echo "--install-root requires a path." >&2
        exit 1
      fi
      INSTALL_ROOT="$2"
      shift 2
      ;;
    --no-path-update)
      SKIP_PATH_UPDATE=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo is required but was not found in PATH." >&2
  exit 1
fi

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
CRATE_PATH="$SCRIPT_DIR/crates/tempyr-cli"
BIN_DIR="$INSTALL_ROOT/bin"
TARGET_BIN="$BIN_DIR/tempyr"
INSTALL_OUTPUT=""
PATH_BLOCK_START="# >>> tempyr >>>"
PATH_BLOCK_END="# <<< tempyr <<<"

if [[ ! -f "$CRATE_PATH/Cargo.toml" ]]; then
  echo "Could not find crates/tempyr-cli/Cargo.toml relative to $SCRIPT_DIR." >&2
  exit 1
fi

run_cargo_install() {
  local log_file
  log_file="$(mktemp)"
  set +e
  cargo install \
    --path "$CRATE_PATH" \
    --root "$INSTALL_ROOT" \
    --locked \
    --force \
    --bin tempyr 2>&1 | tee "$log_file"
  local status=${PIPESTATUS[0]}
  set -e
  INSTALL_OUTPUT="$(cat "$log_file")"
  rm -f "$log_file"
  return "$status"
}

preflight_locked_target() {
  [[ -e "$TARGET_BIN" ]] || return 0

  local -a pids
  mapfile -t pids < <(find_matching_pids "$TARGET_BIN")
  if ((${#pids[@]} == 0)); then
    return 0
  fi

  stop_matching_processes "$TARGET_BIN" "${pids[@]}"
}

output_indicates_lock_error() {
  local output="$1"
  grep -Eq 'Text file busy|Device or resource busy|resource busy' <<<"$output"
}

find_matching_pids() {
  local target="$1"
  local resolved_target resolved proc pid

  if [[ ! -e "$target" ]]; then
    return 0
  fi

  resolved_target="$(readlink -f -- "$target")"
  for proc in /proc/[0-9]*/exe; do
    [[ -L "$proc" ]] || continue
    pid="${proc#/proc/}"
    pid="${pid%/exe}"
    resolved="$(readlink -f -- "$proc" 2>/dev/null || true)"
    if [[ "$resolved" == "$resolved_target" ]]; then
      printf '%s\n' "$pid"
    fi
  done
}

stop_matching_processes() {
  local target="$1"
  shift
  local -a pids remaining
  local pid

  pids=("$@")
  if ((${#pids[@]} == 0)); then
    return 1
  fi

  echo "Detected a locked Tempyr install at $target. Stopping matching processes: ${pids[*]}" >&2
  kill -TERM "${pids[@]}" 2>/dev/null || true

  for ((attempt = 0; attempt < 30; attempt++)); do
    remaining=()
    for pid in "${pids[@]}"; do
      if kill -0 "$pid" 2>/dev/null; then
        remaining+=("$pid")
      fi
    done

    if ((${#remaining[@]} == 0)); then
      return 0
    fi

    pids=("${remaining[@]}")
    sleep 0.5
  done

  echo "Some Tempyr processes did not exit after SIGTERM. Sending SIGKILL to: ${pids[*]}" >&2
  kill -KILL "${pids[@]}" 2>/dev/null || true
  sleep 1

  remaining=()
  for pid in "${pids[@]}"; do
    if kill -0 "$pid" 2>/dev/null; then
      remaining+=("$pid")
    fi
  done

  if ((${#remaining[@]} > 0)); then
    echo "Failed to stop Tempyr processes: ${remaining[*]}" >&2
    return 2
  fi

  return 0
}

kill_matching_processes() {
  local target="$1"
  local -a pids

  mapfile -t pids < <(find_matching_pids "$target")
  stop_matching_processes "$target" "${pids[@]}"
}

handle_failed_process_stop() {
  local target="$1"
  local status="$2"

  if [[ "$status" -eq 1 ]]; then
    echo "The install target appears busy, but no matching Tempyr processes were found at $target." >&2
    return
  fi

  echo "The install target appears busy, and matching Tempyr processes could not be stopped at $target." >&2
}

upsert_path_block() {
  local rc_file="$1"
  local tmp_file

  mkdir -p "$(dirname "$rc_file")"
  touch "$rc_file"
  tmp_file="$(mktemp)"

  awk -v start="$PATH_BLOCK_START" -v end="$PATH_BLOCK_END" '
    $0 == start { skipping = 1; next }
    $0 == end { skipping = 0; next }
    !skipping { print }
  ' "$rc_file" > "$tmp_file"

  mv "$tmp_file" "$rc_file"
  printf '\n%s\nexport PATH="%s:$PATH"\n%s\n' \
    "$PATH_BLOCK_START" \
    "$BIN_DIR" \
    "$PATH_BLOCK_END" >> "$rc_file"
}

ensure_path_persistence() {
  upsert_path_block "$HOME/.profile"

  case "${SHELL##*/}" in
    bash)
      upsert_path_block "$HOME/.bashrc"
      ;;
    zsh)
      upsert_path_block "$HOME/.zshrc"
      ;;
  esac

  case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *)
      export PATH="$BIN_DIR:$PATH"
      ;;
  esac
}

preflight_locked_target

run_cargo_install || {
  if [[ -e "$TARGET_BIN" ]] && output_indicates_lock_error "$INSTALL_OUTPUT"; then
    if kill_matching_processes "$TARGET_BIN"; then
      echo "Retrying cargo install after stopping matching Tempyr processes..." >&2
      run_cargo_install
    else
      handle_failed_process_stop "$TARGET_BIN" "$?"
      exit 1
    fi
  else
    exit 1
  fi
}

if ((SKIP_PATH_UPDATE == 0)); then
  ensure_path_persistence
fi

echo
echo "Tempyr installed to $TARGET_BIN"
if ((SKIP_PATH_UPDATE == 0)); then
  echo "Added $BIN_DIR to PATH for future shells."
fi
