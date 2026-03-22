#!/bin/bash
# OpenQuant Runner — build, run, manage the trading engine
#
# Usage:
#   ./run.sh pairs          # run pairs trading (default)
#   ./run.sh single         # run single-symbol trading
#   ./run.sh test           # run integration test config
#   ./run.sh build          # build only (no run)
#   ./run.sh clean          # clean build artifacts + logs
#   ./run.sh logs           # tail the engine log
#   ./run.sh summary        # show P&L summary from last run
#   ./run.sh status         # show git commit, config, last run

set -e

ROOT="$(cd "$(dirname "$0")" && pwd)"
ENGINE="$ROOT/engine"
BINARY="$ENGINE/target/release/openquant-runner"
DATA="$ROOT/data"
JOURNAL="$DATA/journal"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

build() {
    echo -e "${YELLOW}Clean building openquant-runner (release)...${NC}"
    cd "$ENGINE" && cargo clean -p openquant-runner 2>/dev/null
    cargo build -p openquant-runner --release 2>&1 | tail -3
    echo -e "${GREEN}Build complete${NC}"
}

run_engine() {
    local config="$1"
    local config_path="$ROOT/config/${config}.toml"

    if [ ! -f "$config_path" ]; then
        echo -e "${RED}Config not found: $config_path${NC}"
        exit 1
    fi

    if [ ! -f "$BINARY" ]; then
        echo -e "${YELLOW}Binary not found, building...${NC}"
        build
    fi

    mkdir -p "$JOURNAL"

    # Clean previous run output (trade_results appends)
    rm -f "$DATA/trade_results.json" "$DATA/order_intents.json"

    echo -e "${GREEN}Running: $config mode${NC}"
    echo -e "  Config: config/${config}.toml"
    echo -e "  Data:   $DATA"
    echo -e "  Log:    $JOURNAL/engine.log"
    echo ""

    cd "$ROOT"
    RUST_LOG=info "$BINARY" backtest \
        --config "$config_path" \
        --data-dir "$DATA" \
        --output-dir "$DATA" \
        --warmup-bars 0 2>/dev/null

    echo ""
    # Show summary
    summary
}

clean() {
    echo -e "${YELLOW}Cleaning...${NC}"
    rm -f "$DATA/trade_results.json" "$DATA/order_intents.json"
    rm -f "$JOURNAL/engine.log"
    cd "$ENGINE" && cargo clean 2>/dev/null
    echo -e "${GREEN}Cleaned build artifacts, logs, and output files${NC}"
}

logs() {
    if [ ! -f "$JOURNAL/engine.log" ]; then
        echo -e "${RED}No engine.log found${NC}"
        exit 1
    fi
    tail -f "$JOURNAL/engine.log"
}

summary() {
    if [ ! -f "$JOURNAL/engine.log" ]; then
        echo -e "${RED}No engine.log found — run the engine first${NC}"
        exit 1
    fi

    local last_summary
    last_summary=$(grep "P&L summary" "$JOURNAL/engine.log" | tail -1)

    if [ -z "$last_summary" ]; then
        echo -e "${RED}No P&L summary in log${NC}"
        exit 1
    fi

    local run_id=$(echo "$last_summary" | grep -o 'run_id="[^"]*"' | cut -d'"' -f2)
    local trades=$(echo "$last_summary" | grep -o 'total_trades=[0-9]*' | cut -d= -f2)
    local pnl=$(echo "$last_summary" | grep -o 'dollar_pnl="[^"]*"' | cut -d'"' -f2)
    local per_day=$(echo "$last_summary" | grep -o 'dollar_per_day="[^"]*"' | cut -d'"' -f2)
    local win_rate=$(echo "$last_summary" | grep -o 'win_rate="[^"]*"' | cut -d'"' -f2)
    local days=$(echo "$last_summary" | grep -o 'trading_days=[0-9]*' | cut -d= -f2)

    echo -e "${GREEN}═══════════════════════════════════════${NC}"
    echo -e "${GREEN}  P&L Summary (run: $run_id)${NC}"
    echo -e "${GREEN}═══════════════════════════════════════${NC}"
    echo -e "  Trades:      $trades"
    echo -e "  Win rate:    $win_rate"
    echo -e "  Total P&L:   \$$pnl"
    echo -e "  Per day:     ${GREEN}\$$per_day/day${NC}"
    echo -e "  Days:        $days"
    echo -e "${GREEN}═══════════════════════════════════════${NC}"
}

status() {
    echo -e "${YELLOW}OpenQuant Status${NC}"
    echo "  Git commit: $(git -C "$ROOT" rev-parse --short HEAD)"
    echo "  Branch:     $(git -C "$ROOT" branch --show-current)"
    echo "  Configs:    $(ls "$ROOT"/config/*.toml 2>/dev/null | xargs -n1 basename | tr '\n' ' ')"
    echo "  Pairs:      $(python3 -c "import json; d=json.load(open('$DATA/active_pairs.json')); print(', '.join(f\"{p['leg_a']}/{p['leg_b']}\" for p in d['pairs']))" 2>/dev/null || echo 'none')"
    echo "  Binary:     $([ -f "$BINARY" ] && echo 'built' || echo 'not built')"

    if [ -f "$JOURNAL/engine.log" ]; then
        local runs=$(grep -c "RUN START" "$JOURNAL/engine.log" 2>/dev/null || echo 0)
        local last_run=$(grep "RUN START" "$JOURNAL/engine.log" | tail -1 | grep -o 'run_id="[^"]*"' | cut -d'"' -f2)
        echo "  Log runs:   $runs (last: $last_run)"
    else
        echo "  Log:        no runs yet"
    fi
}

# Main
case "${1:-pairs}" in
    pairs)    build && run_engine pairs ;;
    single)   build && run_engine single ;;
    test)     build && run_engine test ;;
    build)    build ;;
    clean)    clean ;;
    logs)     logs ;;
    summary)  summary ;;
    status)   status ;;
    *)
        echo "Usage: ./run.sh {pairs|single|test|build|clean|logs|summary|status}"
        exit 1
        ;;
esac
