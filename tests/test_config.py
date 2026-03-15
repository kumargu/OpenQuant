"""Tests for TOML config loading."""

import tempfile
from pathlib import Path

from paper_trading.config import engine_kwargs, load_toml


def test_load_missing_file():
    """Missing TOML file returns empty dict."""
    kw = engine_kwargs("/nonexistent/openquant.toml")
    assert kw == {}


def test_load_default_toml():
    """Repo-root openquant.toml loads correctly."""
    kw = engine_kwargs()
    assert kw["buy_z_threshold"] == -2.2
    assert kw["sell_z_threshold"] == 2.0
    assert kw["min_relative_volume"] == 1.2
    assert kw["max_position_notional"] == 10_000.0
    assert kw["max_daily_loss"] == 500.0
    assert kw["stop_loss_atr_mult"] == 2.5
    assert kw["max_hold_bars"] == 100
    assert kw["trend_filter"] is True
    assert kw["metrics_enabled"] is True


def test_load_custom_toml():
    """Custom TOML with non-default values."""
    content = b"""
[signal]
buy_z_threshold = -3.0
sell_z_threshold = 2.5

[risk]
max_position_notional = 5000.0

[exit]
stop_loss_atr_mult = 3.0
max_hold_bars = 50

[metrics]
enabled = false
"""
    with tempfile.NamedTemporaryFile(suffix=".toml", delete=False) as f:
        f.write(content)
        f.flush()
        kw = engine_kwargs(f.name)

    assert kw["buy_z_threshold"] == -3.0
    assert kw["sell_z_threshold"] == 2.5
    assert kw["max_position_notional"] == 5000.0
    assert kw["stop_loss_atr_mult"] == 3.0
    assert kw["max_hold_bars"] == 50
    assert kw["metrics_enabled"] is False
    # Keys not in partial TOML should not appear
    assert "min_relative_volume" not in kw


def test_symbol_overrides():
    """Per-symbol overrides are passed through."""
    content = b"""
[signal]
buy_z_threshold = -2.2

[symbol_overrides.BTCUSD]
buy_z_threshold = -2.5
stop_loss_atr_mult = 3.0

[symbol_overrides.ETHUSD]
min_relative_volume = 1.5
"""
    with tempfile.NamedTemporaryFile(suffix=".toml", delete=False) as f:
        f.write(content)
        f.flush()
        kw = engine_kwargs(f.name)

    assert kw["buy_z_threshold"] == -2.2
    assert kw["symbol_overrides"]["BTCUSD"]["buy_z_threshold"] == -2.5
    assert kw["symbol_overrides"]["BTCUSD"]["stop_loss_atr_mult"] == 3.0
    assert kw["symbol_overrides"]["ETHUSD"]["min_relative_volume"] == 1.5


def test_merge_cli_overrides():
    """CLI args override TOML values, None args are skipped."""
    from types import SimpleNamespace
    from paper_trading.config import merge_cli_overrides

    toml_kw = {"buy_z_threshold": -2.2, "max_hold_bars": 100}
    cli = SimpleNamespace(
        max_position=None,
        max_daily_loss=None,
        buy_z=-3.0,  # override
        sell_z=None,
        min_vol=None,
        stop_loss=None,
        stop_loss_atr=None,
        max_hold=50,  # override
        take_profit=None,
        no_trend_filter=False,
    )
    merged = merge_cli_overrides(toml_kw, cli)
    assert merged["buy_z_threshold"] == -3.0  # CLI wins
    assert merged["max_hold_bars"] == 50  # CLI wins


def test_merge_cli_trend_filter():
    """--no-trend-filter sets trend_filter=False."""
    from types import SimpleNamespace
    from paper_trading.config import merge_cli_overrides

    toml_kw = {"trend_filter": True}
    cli = SimpleNamespace(
        max_position=None, max_daily_loss=None, buy_z=None,
        sell_z=None, min_vol=None, stop_loss=None, stop_loss_atr=None,
        max_hold=None, take_profit=None, no_trend_filter=True,
    )
    merged = merge_cli_overrides(toml_kw, cli)
    assert merged["trend_filter"] is False
