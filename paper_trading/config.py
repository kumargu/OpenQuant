"""
Load engine configuration from openquant.toml.

Reads the TOML file once and exposes a flat dict of kwargs that can be
unpacked directly into the Rust engine constructor or backtest() call.
CLI flags take precedence over TOML values.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

try:
    import tomllib  # Python 3.11+
except ModuleNotFoundError:
    import tomli as tomllib  # type: ignore[no-redef]


_DEFAULT_PATH = Path(__file__).resolve().parent.parent / "openquant.toml"


def load_toml(path: str | Path | None = None) -> dict[str, Any]:
    """Return the raw parsed TOML as a nested dict."""
    p = Path(path) if path else _DEFAULT_PATH
    if not p.exists():
        return {}
    with open(p, "rb") as f:
        return tomllib.load(f)


def engine_kwargs(path: str | Path | None = None) -> dict[str, Any]:
    """Flatten TOML sections into kwargs accepted by the Rust Engine / backtest().

    Returns a dict like::

        {
            "buy_z_threshold": -2.2,
            "max_position_notional": 10000.0,
            ...
            "symbol_overrides": {"BTCUSD": {"buy_z_threshold": -2.5}},
        }
    """
    raw = load_toml(path)
    if not raw:
        return {}

    kw: dict[str, Any] = {}

    # [signal] — only keys accepted by the Rust Engine / backtest() PyO3 API
    sig = raw.get("signal", {})
    for key in ("buy_z_threshold", "sell_z_threshold", "min_relative_volume",
                "trend_filter"):
        if key in sig:
            kw[key] = sig[key]
    # min_score is Rust-internal (not exposed via PyO3 yet)

    # [risk] — only keys accepted by the Rust Engine / backtest() PyO3 API
    risk = raw.get("risk", {})
    for key in ("max_position_notional", "max_daily_loss"):
        if key in risk:
            kw[key] = risk[key]
    # min_reward_cost_ratio and estimated_cost_bps are Rust-internal

    # [exit]
    ex = raw.get("exit", {})
    for key in ("stop_loss_pct", "stop_loss_atr_mult", "max_hold_bars",
                "take_profit_pct"):
        if key in ex:
            kw[key] = ex[key]

    # [data]
    data = raw.get("data", {})
    if "max_bar_age_seconds" in data:
        kw["max_bar_age_seconds"] = data["max_bar_age_seconds"]

    # [metrics]
    metrics = raw.get("metrics", {})
    if "enabled" in metrics:
        kw["metrics_enabled"] = metrics["enabled"]

    # [symbol_overrides.*]
    overrides = raw.get("symbol_overrides", {})
    if overrides:
        kw["symbol_overrides"] = dict(overrides)

    return kw


def merge_cli_overrides(toml_kw: dict[str, Any], cli_args: Any) -> dict[str, Any]:
    """Overlay CLI args on top of TOML defaults.

    Only overrides values where the CLI arg was explicitly provided
    (not left at its argparse default).
    """
    merged = dict(toml_kw)

    cli_map = {
        "max_position": "max_position_notional",
        "max_daily_loss": "max_daily_loss",
        "buy_z": "buy_z_threshold",
        "sell_z": "sell_z_threshold",
        "min_vol": "min_relative_volume",
        "stop_loss": "stop_loss_pct",
        "stop_loss_atr": "stop_loss_atr_mult",
        "max_hold": "max_hold_bars",
        "take_profit": "take_profit_pct",
        "no_trend_filter": "_no_trend_filter",
    }

    for cli_name, kw_name in cli_map.items():
        val = getattr(cli_args, cli_name.replace("-", "_"), None)
        if val is not None:
            if kw_name == "_no_trend_filter":
                if val:  # --no-trend-filter was passed
                    merged["trend_filter"] = False
            else:
                merged[kw_name] = val

    return merged
