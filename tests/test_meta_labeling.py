"""Tests for meta-labeling: triple barrier + feature extraction."""

import numpy as np
import pytest

from paper_trading.meta_labeling import triple_barrier_label, train_meta_model


class TestTripleBarrier:
    def test_take_profit_hit(self):
        """Price rises above TP → label 1."""
        label = triple_barrier_label(
            100.0, [101.0, 102.0, 103.0], take_profit_pct=0.02
        )
        assert label == 1

    def test_stop_loss_hit(self):
        """Price falls below SL → label 0."""
        label = triple_barrier_label(
            100.0, [99.0, 98.0, 97.0], stop_loss_pct=0.015
        )
        assert label == 0

    def test_tp_hit_before_sl(self):
        """TP hit first even though SL would be hit later."""
        label = triple_barrier_label(
            100.0,
            [101.0, 102.5, 97.0],  # TP at 102.5, SL would be at 97
            take_profit_pct=0.02,
            stop_loss_pct=0.04,
        )
        assert label == 1

    def test_time_barrier_positive(self):
        """Neither TP nor SL hit, small positive return → label 1."""
        label = triple_barrier_label(
            100.0,
            [100.5] * 50,  # 0.5% gain, below 2% TP
            take_profit_pct=0.02,
            stop_loss_pct=0.015,
            max_hold_bars=50,
        )
        assert label == 1

    def test_time_barrier_negative(self):
        """Neither TP nor SL hit, small negative return → label 0."""
        label = triple_barrier_label(
            100.0,
            [99.5] * 50,  # -0.5% loss, above -1.5% SL
            take_profit_pct=0.02,
            stop_loss_pct=0.015,
            max_hold_bars=50,
        )
        assert label == 0

    def test_empty_future_prices(self):
        """No future data → label 0."""
        label = triple_barrier_label(100.0, [])
        assert label == 0

    def test_immediate_tp(self):
        """First bar hits TP."""
        label = triple_barrier_label(
            100.0, [103.0], take_profit_pct=0.02
        )
        assert label == 1

    def test_max_hold_respects_limit(self):
        """Only looks at max_hold_bars future prices."""
        prices = [100.5] * 10 + [105.0]  # TP at bar 10
        label = triple_barrier_label(
            100.0,
            prices,
            take_profit_pct=0.04,
            stop_loss_pct=0.02,
            max_hold_bars=5,  # only look at first 5 bars
        )
        # At max_hold (bar 5): price=100.5, ret=0.5% > 0 → label 1
        assert label == 1


class TestTrainMetaModel:
    def test_rejects_too_few_signals(self):
        X = np.random.randn(5, 3)
        y = np.array([1, 0, 1, 0, 1])
        result = train_meta_model(X, y, ["a", "b", "c"])
        assert "error" in result

    def test_trains_with_sufficient_data(self):
        rng = np.random.RandomState(42)
        n = 100
        X = rng.randn(n, 5)
        # Simple pattern: positive feature 0 → profitable
        y = (X[:, 0] > 0).astype(int)
        names = [f"feat_{i}" for i in range(5)]

        result = train_meta_model(X, y, names, n_cv_splits=3)

        assert "error" not in result
        assert result["metrics"]["accuracy"] > 0.5
        assert result["metrics"]["n_signals"] == n
        assert "feature_importances" in result["metrics"]

    def test_feature_importances_sum_to_one(self):
        rng = np.random.RandomState(42)
        X = rng.randn(100, 4)
        y = (X[:, 0] > 0).astype(int)
        names = ["a", "b", "c", "d"]

        result = train_meta_model(X, y, names, n_cv_splits=3)
        if "error" in result:
            pytest.skip("training failed")

        imps = result["metrics"]["feature_importances"]
        total = sum(imps.values())
        assert abs(total - 1.0) < 0.01, f"importances sum to {total}"
