#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 3 ]]; then
  cat <<'EOF'
Usage:
  scripts/run_basket_replay_quiet.sh <name> <start> <end> [extra replay args...]

Example:
  scripts/run_basket_replay_quiet.sh 2026_ytd_baseline 2026-01-02 2026-04-30

  scripts/run_basket_replay_quiet.sh 2026_ytd_classifier 2026-01-02 2026-04-30 \
    --leadership-overlay-sectors faang,chips \
    --leadership-mode replace-with-long-only \
    --leadership-ret5d-threshold 0.02 \
    --leadership-breadth5d-threshold 0.56 \
    --leadership-long-only-leverage 4.0
EOF
  exit 2
fi

name="$1"
start="$2"
end="$3"
shift 3

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
runner="$repo_root/engine/target/debug/openquant-runner"
out_dir="$repo_root/data/replay/$name"
tsv_path="$out_dir/report.tsv"
log_path="$out_dir/replay.log"

mkdir -p "$out_dir"

if [[ ! -x "$runner" ]]; then
  echo "runner binary not found at $runner" >&2
  echo "build it first: cargo build -p openquant-runner" >&2
  exit 1
fi

RUST_LOG=info "$runner" replay --engine basket \
  --start "$start" \
  --end "$end" \
  --capital 10000 \
  --n-active-baskets 5 \
  --report-tsv "$tsv_path" \
  "$@" \
  >"$log_path" 2>&1

if [[ ! -f "$tsv_path" ]]; then
  echo "replay completed without report: $tsv_path" >&2
  exit 1
fi

stats_line="$(head -n 1 "$tsv_path")"
cum_return="$(printf '%s\n' "$stats_line" | sed -n 's/.*cum_return=\([-0-9.]*\).*/\1/p')"
sharpe="$(printf '%s\n' "$stats_line" | sed -n 's/.*sharpe=\([-0-9.]*\).*/\1/p')"
max_dd="$(printf '%s\n' "$stats_line" | sed -n 's/.*max_dd=\([-0-9.]*\).*/\1/p')"
n_days="$(printf '%s\n' "$stats_line" | sed -n 's/.*n_days=\([0-9]*\).*/\1/p')"
orders="$(grep -c 'emitting orders' "$log_path" || true)"
on_events="$(grep -c 'classifier switched ON' "$log_path" || true)"
off_events="$(grep -c 'classifier switched OFF' "$log_path" || true)"
active_days="$(grep -c 'leadership_mode="replace_with_long_only"' "$log_path" || true)"
inactive_days="$(grep -c 'leadership_mode="replace_with_long_only_inactive"' "$log_path" || true)"
failed_orders="$(awk '
  /submitted basket orders/ {
    if (match($0, /failed_orders=[0-9]+/)) {
      value = substr($0, RSTART + 14, RLENGTH - 14)
      sum += value + 0
    }
  }
  END { print sum + 0 }
' "$log_path")"

cat <<EOF
replay: $name
window: $start -> $end
report: $tsv_path
log: $log_path
cum_return: $cum_return
sharpe: $sharpe
max_dd: $max_dd
n_days: $n_days
order_days: $orders
failed_orders: $failed_orders
overlay_on_events: $on_events
overlay_off_events: $off_events
overlay_active_days: $active_days
overlay_inactive_days: $inactive_days
EOF
