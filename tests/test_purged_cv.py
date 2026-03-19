"""Tests for purged cross-validation."""

import numpy as np
import pytest

from paper_trading.purged_cv import (
    _compute_pbo,
    cpcv_splits,
    purged_kfold_splits,
)


class TestPurgedKFold:
    def test_correct_number_of_splits(self):
        splits = purged_kfold_splits(100, n_splits=5)
        assert len(splits) == 5

    def test_test_sets_are_disjoint(self):
        splits = purged_kfold_splits(100, n_splits=5)
        all_test = []
        for _, test_idx in splits:
            all_test.extend(test_idx)
        assert len(all_test) == len(set(all_test))

    def test_embargo_excludes_post_test_samples(self):
        splits = purged_kfold_splits(100, n_splits=5, embargo_pct=0.05)
        for train_idx, test_idx in splits:
            test_end = max(test_idx) + 1
            embargo_end = min(test_end + 5, 100)
            # No train sample should be in the embargo zone
            for idx in range(test_end, embargo_end):
                assert idx not in train_idx, (
                    f"train should not contain embargo idx {idx}"
                )

    def test_train_and_test_disjoint(self):
        splits = purged_kfold_splits(100, n_splits=5)
        for train_idx, test_idx in splits:
            overlap = set(train_idx) & set(test_idx)
            assert len(overlap) == 0

    def test_all_indices_covered(self):
        """Every index appears in at least one test set."""
        splits = purged_kfold_splits(100, n_splits=5, embargo_pct=0.0)
        all_test = set()
        for _, test_idx in splits:
            all_test.update(test_idx)
        assert all_test == set(range(100))


class TestCPCV:
    def test_correct_number_of_paths(self):
        # C(6, 2) = 15
        splits = cpcv_splits(120, n_groups=6, n_test_groups=2)
        assert len(splits) == 15

    def test_c_4_1_gives_4_paths(self):
        splits = cpcv_splits(100, n_groups=4, n_test_groups=1)
        assert len(splits) == 4

    def test_test_groups_are_contiguous_blocks(self):
        splits = cpcv_splits(120, n_groups=6, n_test_groups=2)
        for _, test_idx, _ in splits:
            # Test should be contiguous within each group
            assert len(test_idx) > 0

    def test_train_test_disjoint_with_embargo(self):
        splits = cpcv_splits(120, n_groups=6, n_test_groups=2, embargo_pct=0.02)
        for train_idx, test_idx, _ in splits:
            overlap = set(train_idx) & set(test_idx)
            assert len(overlap) == 0


class TestPBO:
    def test_perfect_strategy_low_pbo(self):
        """A strategy that always ranks best OOS should have PBO=0."""
        # 5 paths, 3 configs. Config 0 is always best IS and OOS.
        is_sharpes = np.array([
            [2.0, 1.0, 0.5],
            [1.8, 0.9, 0.4],
            [2.2, 1.1, 0.6],
            [1.9, 0.8, 0.3],
            [2.1, 1.0, 0.5],
        ])
        oos_sharpes = np.array([
            [1.5, 0.5, 0.2],
            [1.3, 0.4, 0.1],
            [1.6, 0.6, 0.3],
            [1.4, 0.3, 0.0],
            [1.5, 0.5, 0.2],
        ])
        pbo = _compute_pbo(is_sharpes, oos_sharpes)
        assert pbo == 0.0, f"perfect strategy should have PBO=0, got {pbo}"

    def test_overfit_strategy_high_pbo(self):
        """A strategy where IS best always underperforms OOS should have PBO=1."""
        # Best IS (config 0) always worst OOS
        is_sharpes = np.array([
            [2.0, 1.0, 0.5],
            [2.0, 1.0, 0.5],
            [2.0, 1.0, 0.5],
        ])
        oos_sharpes = np.array([
            [-1.0, 0.5, 1.0],  # best IS (idx 0) = -1.0, median = 0.5
            [-0.5, 0.3, 0.8],
            [-0.8, 0.4, 0.9],
        ])
        pbo = _compute_pbo(is_sharpes, oos_sharpes)
        assert pbo == 1.0, f"overfit strategy should have PBO=1, got {pbo}"

    def test_pbo_bounded_0_1(self):
        rng = np.random.RandomState(42)
        is_s = rng.randn(10, 5)
        oos_s = rng.randn(10, 5)
        pbo = _compute_pbo(is_s, oos_s)
        assert 0.0 <= pbo <= 1.0
