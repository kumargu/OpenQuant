#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
db="${1:-}"
view="${2:-summary}"

if [[ -z "$db" ]]; then
  db="$(find "$repo_root/data/journal" -maxdepth 1 -type f -name 'basket_live*.sqlite3' | sort | tail -n 1)"
fi

if [[ -z "$db" || ! -f "$db" ]]; then
  echo "basket journal DB not found" >&2
  exit 1
fi

case "$view" in
  summary)
    sqlite3 -header -column "$db" "
      SELECT 'runs' AS table_name, COUNT(*) AS rows FROM basket_runs
      UNION ALL SELECT 'session_closes', COUNT(*) FROM basket_session_closes
      UNION ALL SELECT 'order_events', COUNT(*) FROM basket_order_events
      UNION ALL SELECT 'picker_decisions', COUNT(*) FROM basket_picker_decisions;

      SELECT run_id, started_at_utc, execution_mode, startup_phase, symbols,
             baskets, capital, leverage, n_active_baskets, broker_positions,
             last_processed_trading_day
        FROM basket_runs
       ORDER BY started_at_utc DESC
       LIMIT 10;
    "
    ;;
  sessions)
    sqlite3 -header -column "$db" "
      SELECT trading_day, status, closes_received, symbols_expected,
             active_baskets, admitted_baskets, excluded_baskets,
             ROUND(gross_notional, 2) AS gross_notional,
             ROUND(gross_cap, 2) AS gross_cap,
             target_positions, buy_orders, sell_orders,
             accepted_orders, failed_orders,
             ROUND(target_gross, 2) AS target_gross,
             ROUND(actual_gross, 2) AS actual_gross,
             ROUND(divergence_pct, 4) AS divergence_pct,
             error_text
        FROM basket_session_closes
       ORDER BY trading_day DESC, id DESC
       LIMIT 50;
    "
    ;;
  orders)
    sqlite3 -header -column "$db" "
      SELECT trading_day, seq, symbol, side, requested_qty,
             ROUND(intended_notional, 2) AS intended_notional,
             reason, basket_id, broker_order_id, broker_status,
             submission_status, error_text, created_at_utc
        FROM basket_order_events
       ORDER BY id DESC
       LIMIT 100;
    "
    ;;
  picker)
    sqlite3 -header -column "$db" "
      SELECT trading_day, picker_id, mode, reason,
             active_sectors_json, active_symbols_json,
             ROUND(leadership_short_conflict_ratio, 4) AS short_conflict,
             ROUND(strategy_return_20d, 4) AS ret20d,
             ROUND(strategy_drawdown_20d, 4) AS dd20d,
             ROUND(baseline_scale_if_sleeve, 4) AS baseline_scale,
             ROUND(sleeve_leverage_scale, 4) AS sleeve_scale,
             created_at_utc
        FROM basket_picker_decisions
       ORDER BY id DESC
       LIMIT 100;
    "
    ;;
  *)
    echo "usage: $0 [journal.sqlite3] [summary|sessions|orders|picker]" >&2
    exit 2
    ;;
esac
