"""Tests for tearsheet generation (equity curve → returns conversion)."""

import pandas as pd
import pytest

from paper_trading.tearsheet import equity_curve_to_returns, trades_to_returns


class TestEquityCurveToReturns:
    def test_basic_conversion(self):
        """Equity curve converts to correct returns series."""
        # Starting capital 100k, cumulative P&L: 0, 100, 300
        equity_curve = [0.0, 100.0, 300.0]
        timestamps = [1000, 2000, 3000]  # ms

        returns = equity_curve_to_returns(equity_curve, timestamps, 100_000.0)

        assert len(returns) == 3
        assert returns.iloc[0] == 0.0  # first bar has no prior
        assert abs(returns.iloc[1] - 100.0 / 100_000.0) < 1e-10
        assert abs(returns.iloc[2] - 200.0 / 100_100.0) < 1e-10

    def test_empty_inputs(self):
        """Empty inputs return empty series."""
        assert equity_curve_to_returns([], []).empty
        assert equity_curve_to_returns([1.0], []).empty
        assert equity_curve_to_returns([], [1000]).empty

    def test_flat_equity(self):
        """No P&L produces zero returns."""
        equity_curve = [0.0, 0.0, 0.0]
        timestamps = [1000, 2000, 3000]

        returns = equity_curve_to_returns(equity_curve, timestamps, 100_000.0)

        assert all(r == 0.0 for r in returns)

    def test_negative_pnl(self):
        """Losses produce negative returns."""
        equity_curve = [0.0, -500.0]
        timestamps = [1000, 2000]

        returns = equity_curve_to_returns(equity_curve, timestamps, 100_000.0)

        assert returns.iloc[1] < 0.0

    def test_datetime_index(self):
        """Returns series has UTC DatetimeIndex."""
        equity_curve = [0.0, 100.0]
        timestamps = [1_700_000_000_000, 1_700_000_060_000]  # ~2023 timestamps

        returns = equity_curve_to_returns(equity_curve, timestamps)

        assert isinstance(returns.index, pd.DatetimeIndex)
        assert str(returns.index.tz) == "UTC"

    def test_duplicate_timestamps_deduped(self):
        """Duplicate timestamps keep first occurrence only."""
        equity_curve = [0.0, 100.0, 200.0]
        timestamps = [1000, 2000, 2000]  # duplicate

        returns = equity_curve_to_returns(equity_curve, timestamps)

        assert len(returns) == 2  # deduped


class TestTradesToReturns:
    def test_basic_trades(self):
        """Trade P&L maps to correct bar returns."""
        trades = [
            {"exit_time": 2000, "pnl": 50.0},
            {"exit_time": 3000, "pnl": -30.0},
        ]
        timestamps = [1000, 2000, 3000]

        returns = trades_to_returns(trades, timestamps, 100_000.0)

        assert abs(returns.iloc[1] - 50.0 / 100_000.0) < 1e-10
        assert abs(returns.iloc[2] - (-30.0 / 100_000.0)) < 1e-10
        assert returns.iloc[0] == 0.0  # no trade closed here

    def test_multiple_trades_same_bar(self):
        """Multiple trades closing on same bar sum their P&L."""
        trades = [
            {"exit_time": 2000, "pnl": 100.0},
            {"exit_time": 2000, "pnl": 50.0},
        ]
        timestamps = [1000, 2000]

        returns = trades_to_returns(trades, timestamps, 100_000.0)

        assert abs(returns.iloc[1] - 150.0 / 100_000.0) < 1e-10

    def test_empty_inputs(self):
        """Empty inputs return empty series."""
        assert trades_to_returns([], []).empty
        assert trades_to_returns([], [1000]).empty
        assert trades_to_returns([{"exit_time": 1, "pnl": 1}], []).empty

    def test_no_bars_zero_returns(self):
        """Bars with no trade closings have zero return."""
        trades = [{"exit_time": 3000, "pnl": 100.0}]
        timestamps = [1000, 2000, 3000]

        returns = trades_to_returns(trades, timestamps, 100_000.0)

        assert returns.iloc[0] == 0.0
        assert returns.iloc[1] == 0.0
        assert returns.iloc[2] > 0.0
