#!/usr/bin/env python3
from __future__ import annotations

import argparse
import re
from collections import Counter
from pathlib import Path


HEADER_RE = re.compile(r"([a-z_]+)=([-0-9.]+)")
TARGET_RE = re.compile(
    r"date=(?P<date>\d{4}-\d{2}-\d{2}).*leadership_mode=\"(?P<mode>[^\"]+)\""
)
FAILED_RE = re.compile(r"date=(?P<date>\d{4}-\d{2}-\d{2})")
SYMBOL_RE = re.compile(r"symbol=\"([A-Z.]+)\"")
SECTOR_RE = re.compile(r"leadership_sectors_active=\{([^}]*)\}")


def parse_header(report_path: Path) -> dict[str, float]:
    first = report_path.read_text().splitlines()[0]
    return {k: float(v) for k, v in HEADER_RE.findall(first)}


def parse_equity_rows(report_path: Path) -> list[tuple[str, float, float, float, float]]:
    rows: list[tuple[str, float, float, float, float]] = []
    peak = None
    for line in report_path.read_text().splitlines()[3:]:
        if not line.strip():
            continue
        date, equity_s, pnl_s = line.split("\t")
        equity = float(equity_s)
        pnl = float(pnl_s)
        peak = equity if peak is None else max(peak, equity)
        drawdown = 0.0 if peak <= 0 else (peak - equity) / peak
        rows.append((date, equity, pnl, drawdown, peak))
    return rows


def parse_log(log_path: Path) -> dict[str, object]:
    mode_by_date: dict[str, str] = {}
    sectors_by_date: dict[str, str] = {}
    failed_orders_by_date: Counter[str] = Counter()
    failed_symbols: Counter[str] = Counter()
    divergences_by_date: Counter[str] = Counter()
    replacement_lines_by_date: dict[str, str] = {}
    fatal_line = None

    for line in log_path.read_text().splitlines():
        if "target notionals summary" in line:
            m = TARGET_RE.search(line)
            if m:
                date = m.group("date")
                mode_by_date[date] = m.group("mode")
                sectors = SECTOR_RE.search(line)
                if sectors:
                    sectors_by_date[date] = sectors.group(1).replace('"', "")
        if (
            "leadership overlay transformed baseline basket portfolio" in line
            or "leadership overlay transformed basket-only basket portfolio" in line
        ):
            m = FAILED_RE.search(line)
            if m and m.group("date") not in replacement_lines_by_date:
                replacement_lines_by_date[m.group("date")] = line
        if "ORDER FAILED" in line:
            m = FAILED_RE.search(line)
            if m:
                failed_orders_by_date[m.group("date")] += 1
            symbol = SYMBOL_RE.search(line)
            if symbol:
                failed_symbols[symbol.group(1)] += 1
        if "BROKER DIVERGENCE" in line:
            m = FAILED_RE.search(line)
            if m:
                divergences_by_date[m.group("date")] += 1
        if "basket replay failed; report TSV not written" in line:
            fatal_line = line

    return {
        "mode_by_date": mode_by_date,
        "sectors_by_date": sectors_by_date,
        "failed_orders_by_date": failed_orders_by_date,
        "failed_symbols": failed_symbols,
        "divergences_by_date": divergences_by_date,
        "replacement_lines_by_date": replacement_lines_by_date,
        "fatal_line": fatal_line,
    }


def summarize_run(run_dir: Path, top_n: int) -> str:
    report_path = run_dir / "report.tsv"
    log_path = run_dir / "replay.log"
    if not log_path.exists():
        raise FileNotFoundError(f"missing log: {log_path}")

    log = parse_log(log_path)
    have_report = report_path.exists()
    header = parse_header(report_path) if have_report else {}
    rows = parse_equity_rows(report_path) if have_report else []

    mode_counts = Counter(log["mode_by_date"].values())
    worst_pnl = sorted(rows, key=lambda row: row[2])[:top_n] if rows else []
    worst_dd = sorted(rows, key=lambda row: row[3], reverse=True)[:top_n] if rows else []

    lines = [
        f"run: {run_dir.name}",
        (
            "stats: "
            + (
                f"cum_return={header.get('cum_return', 0.0):+.2%} "
                f"sharpe={header.get('sharpe', 0.0):.2f} "
                f"max_dd={header.get('max_dd', 0.0):.2%} "
                f"n_days={int(header.get('n_days', 0))}"
                if have_report
                else "report_missing"
            )
        ),
        "mode_days: "
        + ", ".join(
            f"{mode}={count}" for mode, count in sorted(mode_counts.items())
        ),
    ]
    if log["fatal_line"]:
        lines.append(f"fatal: {log['fatal_line']}")

    if worst_pnl:
        lines.append("worst_daily_pnl:")
        for date, equity, pnl, drawdown, peak in worst_pnl:
            mode = log["mode_by_date"].get(date, "unknown")
            sectors = log["sectors_by_date"].get(date, "-")
            failed = log["failed_orders_by_date"][date]
            divs = log["divergences_by_date"][date]
            lines.append(
                f"  {date} pnl={pnl:+.2f} equity={equity:.2f} dd={drawdown:.2%} "
                f"mode={mode} sectors={sectors or '-'} failed_orders={failed} divergences={divs}"
            )

        lines.append("worst_drawdown_dates:")
        for date, equity, pnl, drawdown, peak in worst_dd:
            mode = log["mode_by_date"].get(date, "unknown")
            sectors = log["sectors_by_date"].get(date, "-")
            failed = log["failed_orders_by_date"][date]
            divs = log["divergences_by_date"][date]
            lines.append(
                f"  {date} dd={drawdown:.2%} equity={equity:.2f} peak={peak:.2f} pnl={pnl:+.2f} "
                f"mode={mode} sectors={sectors or '-'} failed_orders={failed} divergences={divs}"
            )

    top_failed = log["failed_symbols"].most_common(8)
    if top_failed:
        lines.append(
            "top_failed_symbols: "
            + ", ".join(f"{symbol}={count}" for symbol, count in top_failed)
        )

    replacement_lines = log["replacement_lines_by_date"]
    if replacement_lines:
        lines.append("overlay_replacement_samples:")
        for date, *_rest in worst_pnl[: min(3, len(worst_pnl))]:
            sample = replacement_lines.get(date)
            if sample:
                lines.append(f"  {date} {sample}")

    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Summarize worst drawdown days from quiet basket replay outputs."
    )
    parser.add_argument("run_dirs", nargs="+", help="Replay run directories under data/replay/")
    parser.add_argument("--top", type=int, default=5, help="Number of worst days to print")
    args = parser.parse_args()

    blocks = []
    for run_dir in args.run_dirs:
        blocks.append(summarize_run(Path(run_dir), args.top))
    print("\n\n".join(blocks))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
