"""
Combinatorial Purged Cross-Validation (CPCV) for backtest validation.

Detects overfitting by computing the Probability of Backtest Overfitting
(PBO): the chance that the best in-sample parameter set underperforms
out-of-sample relative to the median.

Implements:
  - Purged K-Fold CV (de Prado, 2018)
  - Combinatorial Purged CV with PBO (Bailey et al., 2017)

Usage:
  python -m paper_trading.purged_cv --symbol BTC/USD --days 30
  python -m paper_trading.purged_cv --symbol AAPL --days 90 --n-groups 8
"""

from __future__ import annotations

import argparse
from itertools import combinations

import numpy as np

from paper_trading.benchmark import fetch_bars


def purged_kfold_splits(
    n: int, n_splits: int = 5, embargo_pct: float = 0.01
) -> list[tuple[list[int], list[int]]]:
    """Generate purged k-fold train/test index splits.

    Purging removes train samples adjacent to the test set boundaries.
    Embargo adds a buffer after each test set to prevent lookahead leakage
    from overlapping feature windows.

    Args:
        n: total number of observations
        n_splits: number of folds
        embargo_pct: fraction of n to use as post-test embargo buffer
    """
    embargo_size = max(int(n * embargo_pct), 1)
    fold_size = n // n_splits
    splits = []

    for i in range(n_splits):
        test_start = i * fold_size
        test_end = min((i + 1) * fold_size, n)
        embargo_end = min(test_end + embargo_size, n)

        train_idx = list(range(0, test_start)) + list(range(embargo_end, n))
        test_idx = list(range(test_start, test_end))
        splits.append((train_idx, test_idx))

    return splits


def cpcv_splits(
    n: int,
    n_groups: int = 6,
    n_test_groups: int = 2,
    embargo_pct: float = 0.01,
) -> list[tuple[list[int], list[int], tuple[int, ...]]]:
    """Generate Combinatorial Purged CV splits.

    Partitions data into n_groups contiguous blocks, then generates
    all C(n_groups, n_test_groups) train/test combinations with purging.

    Returns list of (train_idx, test_idx, test_group_ids).
    """
    group_size = n // n_groups
    embargo_size = max(int(n * embargo_pct), 1)

    groups = []
    for i in range(n_groups):
        start = i * group_size
        end = min((i + 1) * group_size, n) if i < n_groups - 1 else n
        groups.append(list(range(start, end)))

    splits = []
    for test_combo in combinations(range(n_groups), n_test_groups):
        test_set = set()
        for g in test_combo:
            test_set.update(groups[g])

        # Embargo: exclude samples within embargo_size after each test group end
        embargo_set = set()
        for g in test_combo:
            group_end = groups[g][-1] + 1 if groups[g] else 0
            for j in range(group_end, min(group_end + embargo_size, n)):
                embargo_set.add(j)

        train_idx = [
            i for i in range(n) if i not in test_set and i not in embargo_set
        ]
        test_idx = sorted(test_set)
        splits.append((train_idx, test_idx, test_combo))

    return splits


def run_cpcv(
    bars: list,
    n_groups: int = 6,
    n_test_groups: int = 2,
    param_grid: dict | None = None,
) -> dict:
    """Run CPCV over parameter grid and compute PBO.

    Args:
        bars: list of bar tuples (symbol, timestamp, O, H, L, C, V)
        n_groups: number of contiguous time blocks
        n_test_groups: number of blocks held out per split
        param_grid: dict of param_name -> list of values to sweep.
                    If None, uses default grid of key parameters.

    Returns dict with:
        pbo: Probability of Backtest Overfitting (0-1)
        n_paths: number of CPCV paths
        n_configs: number of parameter configurations tested
        best_config: best in-sample configuration
        oos_sharpes: out-of-sample Sharpe per config per path
    """
    from openquant import backtest

    n = len(bars)

    if param_grid is None:
        param_grid = {
            "buy_z_threshold": [-2.5, -2.2, -2.0, -1.8],
            "sell_z_threshold": [1.5, 1.8, 2.0, 2.5],
            "stop_loss_atr_mult": [2.0, 2.5, 3.0],
        }

    # Generate all parameter combinations
    param_names = list(param_grid.keys())
    param_values = list(param_grid.values())
    configs = [
        dict(zip(param_names, combo))
        for combo in _product(*param_values)
    ]
    n_configs = len(configs)

    # Generate CPCV splits
    splits = cpcv_splits(n, n_groups, n_test_groups)
    n_paths = len(splits)

    print(f"CPCV: {n_paths} paths × {n_configs} configs = {n_paths * n_configs} backtests")

    # Matrix: (n_paths, n_configs) of OOS Sharpe ratios
    oos_sharpes = np.zeros((n_paths, n_configs))
    is_sharpes = np.zeros((n_paths, n_configs))

    for path_idx, (train_idx, test_idx, _combo) in enumerate(splits):
        train_bars = [bars[i] for i in train_idx]
        test_bars = [bars[i] for i in test_idx]

        for cfg_idx, cfg in enumerate(configs):
            # In-sample: run on train
            if train_bars:
                is_result = backtest(train_bars, **cfg)
                is_sharpes[path_idx, cfg_idx] = is_result.get("sharpe_approx", 0.0)

            # Out-of-sample: run on test
            if test_bars:
                oos_result = backtest(test_bars, **cfg)
                oos_sharpes[path_idx, cfg_idx] = oos_result.get("sharpe_approx", 0.0)

        if (path_idx + 1) % 5 == 0 or path_idx == n_paths - 1:
            print(f"  path {path_idx + 1}/{n_paths} done")

    # Compute PBO
    pbo = _compute_pbo(is_sharpes, oos_sharpes)

    # Find best IS config (by average IS Sharpe across paths)
    avg_is = is_sharpes.mean(axis=0)
    best_idx = int(avg_is.argmax())
    best_config = configs[best_idx]
    best_is_sharpe = avg_is[best_idx]
    best_oos_sharpe = oos_sharpes[:, best_idx].mean()

    return {
        "pbo": pbo,
        "n_paths": n_paths,
        "n_configs": n_configs,
        "best_config": best_config,
        "best_is_sharpe": best_is_sharpe,
        "best_oos_sharpe": best_oos_sharpe,
        "oos_sharpes": oos_sharpes,
        "configs": configs,
    }


def _compute_pbo(
    is_sharpes: np.ndarray, oos_sharpes: np.ndarray
) -> float:
    """Compute Probability of Backtest Overfitting.

    PBO = fraction of paths where the best in-sample config
    performs below the median out-of-sample.
    """
    n_paths = is_sharpes.shape[0]
    if n_paths == 0:
        return 0.5

    overfit_count = 0
    for path in range(n_paths):
        # Find best in-sample config for this path
        best_is_idx = int(is_sharpes[path].argmax())
        # Check if its OOS performance is below median OOS
        oos_median = float(np.median(oos_sharpes[path]))
        if oos_sharpes[path, best_is_idx] < oos_median:
            overfit_count += 1

    return overfit_count / n_paths


def _product(*iterables):
    """Cartesian product (like itertools.product but returns lists)."""
    from itertools import product
    return list(product(*iterables))


def print_report(result: dict):
    """Print CPCV results."""
    pbo = result["pbo"]
    print(f"\n{'='*60}")
    print("COMBINATORIAL PURGED CROSS-VALIDATION")
    print(f"{'='*60}")
    print(f"\nPaths: {result['n_paths']} | Configs: {result['n_configs']}")
    print(f"\nBest in-sample config: {result['best_config']}")
    print(f"  IS Sharpe:  {result['best_is_sharpe']:+.3f}")
    print(f"  OOS Sharpe: {result['best_oos_sharpe']:+.3f}")
    print(f"\nPBO = {pbo:.1%}")

    if pbo < 0.3:
        verdict = "PASS — low probability of overfitting"
    elif pbo < 0.5:
        verdict = "MARGINAL — some overfitting risk, proceed with caution"
    else:
        verdict = "FAIL — parameters likely overfit, do not trust backtest results"

    print(f"\n**Verdict: {verdict}**")


def main():
    parser = argparse.ArgumentParser(
        description="Combinatorial Purged Cross-Validation"
    )
    parser.add_argument("--symbol", "-s", default="BTC/USD")
    parser.add_argument("--days", "-d", type=int, default=30)
    parser.add_argument("--timeframe", "-t", default="1Min")
    parser.add_argument("--n-groups", type=int, default=6)
    parser.add_argument("--n-test-groups", type=int, default=2)
    args = parser.parse_args()

    print(f"Fetching {args.symbol} ({args.days}d)...")
    bars = fetch_bars(args.symbol, args.days, args.timeframe)
    if not bars:
        print("No data.")
        return

    result = run_cpcv(
        bars,
        n_groups=args.n_groups,
        n_test_groups=args.n_test_groups,
    )
    print_report(result)


if __name__ == "__main__":
    main()
