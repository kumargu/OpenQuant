#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
session="${VESTED_TMUX_SESSION:-openquant-vested-paper}"

if [[ $# -gt 0 && "${1:0:1}" != "-" ]]; then
  session="$1"
  shift
fi

if ! command -v tmux >/dev/null 2>&1; then
  echo "tmux is required to keep the Vested paper runner alive" >&2
  exit 1
fi

if tmux has-session -t "$session" 2>/dev/null; then
  echo "tmux session already exists: $session" >&2
  echo "attach with: tmux attach -t $session" >&2
  exit 1
fi

runner="$repo_root/engine/target/debug/openquant-runner"
log_dir="$repo_root/data/paper/vested_model"
log_path="$log_dir/engine.log"
mkdir -p "$log_dir"

(
  cd "$repo_root/engine"
  cargo build -p openquant-runner
)

runner_args=(paper --engine basket --paper-vested "$@")
printf -v runner_cmd '%q ' "$runner" "${runner_args[@]}"
printf -v log_path_q '%q' "$log_path"
printf -v repo_root_q '%q' "$repo_root"

cmd="cd $repo_root_q && RUST_LOG=\${RUST_LOG:-info} $runner_cmd 2>&1 | tee -a $log_path_q"
tmux new-session -d -s "$session" -c "$repo_root" "bash -lc '$cmd'"

cat <<EOF
started Vested paper runner
session: $session
attach: tmux attach -t $session
log: $log_path
EOF
