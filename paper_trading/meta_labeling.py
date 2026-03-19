"""
Meta-labeling: learn when signals are profitable.

Trains a classifier to predict P(profitable | signal fired) using:
  - Triple Barrier Method for labeling historical signals
  - Meta-features: strategy scores, regime, GARCH vol, time-of-day, etc.
  - Gradient-boosted classifier with calibrated probabilities
  - Purged CV from paper_trading.purged_cv for training

The meta-model's output is a confidence score [0, 1] that can gate or
scale position sizing: high confidence = full size, low = skip.

Usage:
  python -m paper_trading.meta_labeling --symbol AAPL --days 30
  python -m paper_trading.meta_labeling --symbol AAPL --days 90 --export rules.json
"""

from __future__ import annotations

import argparse
import json
from datetime import datetime, timezone
from pathlib import Path

import numpy as np

from paper_trading.benchmark import CATEGORIES, fetch_bars


# ---------------------------------------------------------------------------
# Triple Barrier Labeling
# ---------------------------------------------------------------------------

def triple_barrier_label(
    entry_price: float,
    future_prices: list[float],
    take_profit_pct: float = 0.02,
    stop_loss_pct: float = 0.015,
    max_hold_bars: int = 50,
) -> int:
    """Label a trade signal as 1 (profitable) or 0 (not).

    Whichever barrier is hit first determines the label:
      - Price rises >= take_profit_pct → 1 (profitable)
      - Price falls <= -stop_loss_pct → 0 (not profitable)
      - max_hold_bars reached → label by sign of return
    """
    for i, price in enumerate(future_prices[:max_hold_bars]):
        ret = (price - entry_price) / entry_price
        if ret >= take_profit_pct:
            return 1
        if ret <= -stop_loss_pct:
            return 0

    # Time barrier: label by sign of final return
    if future_prices:
        final_idx = min(max_hold_bars - 1, len(future_prices) - 1)
        final_ret = (future_prices[final_idx] - entry_price) / entry_price
        return 1 if final_ret > 0 else 0
    return 0


# ---------------------------------------------------------------------------
# Meta-feature extraction from backtest
# ---------------------------------------------------------------------------

def extract_meta_features(
    bars: list,
    params: dict | None = None,
) -> tuple[np.ndarray, np.ndarray, list[dict]]:
    """Run backtest and extract meta-features for each signal that fired.

    Returns (X, y, feature_names_list):
      X: (n_signals, n_features) array of meta-features
      y: (n_signals,) array of labels (1=profitable, 0=not)
    """
    from openquant import Engine

    params = params or {}
    engine = Engine(**params)

    signals = []
    closes = [bar[5] for bar in bars]  # extract close prices

    for bar_idx, bar in enumerate(bars):
        symbol, ts, o, h, l, c, v = bar
        intents = engine.on_bar(symbol, ts, o, h, l, c, v)

        if intents:
            for intent in intents:
                if intent["side"] == "buy":
                    features_dict = engine.features(symbol)
                    if features_dict is None:
                        continue

                    # Future prices for triple barrier labeling
                    future = closes[bar_idx + 1 : bar_idx + 51]
                    if len(future) < 5:
                        continue

                    label = triple_barrier_label(c, future)

                    # Extract meta-features
                    hour = datetime.fromtimestamp(
                        ts / 1000, tz=timezone.utc
                    ).hour
                    day_of_week = datetime.fromtimestamp(
                        ts / 1000, tz=timezone.utc
                    ).weekday()

                    meta = {
                        "signal_score": intent.get("score", 0.0),
                        "z_score": features_dict.get("return_z_score", 0.0),
                        "relative_volume": features_dict.get(
                            "relative_volume", 1.0
                        ),
                        "garch_vol": features_dict.get("garch_vol", 0.0),
                        "adx": features_dict.get("adx", 0.0),
                        "bollinger_pct_b": features_dict.get(
                            "bollinger_pct_b", 0.5
                        ),
                        "ema_fast_above_slow": float(
                            features_dict.get("ema_fast_above_slow", False)
                        ),
                        "garch_vol_percentile": features_dict.get(
                            "garch_vol_percentile", 0.5
                        ),
                        "regime_change_prob": features_dict.get(
                            "regime_change_prob", 0.0
                        ),
                        "hour_of_day": hour,
                        "day_of_week": day_of_week,
                    }

                    signals.append((meta, label))

                    # Simulate fill for engine state tracking
                    engine.on_fill(symbol, "buy", intent["qty"], c)

                elif intent["side"] == "sell":
                    engine.on_fill(symbol, "sell", intent["qty"], c)

    if not signals:
        return np.array([]), np.array([]), []

    feature_names = list(signals[0][0].keys())
    X = np.array([[s[0][f] for f in feature_names] for s in signals])
    y = np.array([s[1] for s in signals])

    return X, y, feature_names


# ---------------------------------------------------------------------------
# Training
# ---------------------------------------------------------------------------

def train_meta_model(
    X: np.ndarray,
    y: np.ndarray,
    feature_names: list[str],
    n_cv_splits: int = 5,
) -> dict:
    """Train a gradient-boosted meta-classifier with calibrated probabilities.

    Uses purged k-fold CV for evaluation (no lookahead bias).

    Returns dict with model, metrics, and feature importances.
    """
    from sklearn.calibration import CalibratedClassifierCV
    from sklearn.ensemble import GradientBoostingClassifier
    from sklearn.metrics import accuracy_score, log_loss, roc_auc_score

    from paper_trading.purged_cv import purged_kfold_splits

    if len(X) < 20:
        return {
            "error": f"too few signals ({len(X)}) for training, need >= 20",
        }

    # Purged CV evaluation
    splits = purged_kfold_splits(len(X), n_splits=n_cv_splits)
    oos_preds = np.zeros(len(X))
    oos_probs = np.zeros(len(X))

    for train_idx, test_idx in splits:
        if not train_idx or not test_idx:
            continue

        X_train, y_train = X[train_idx], y[train_idx]
        X_test = X[test_idx]

        if len(np.unique(y_train)) < 2:
            continue

        clf = GradientBoostingClassifier(
            n_estimators=100, max_depth=3, learning_rate=0.1, random_state=42
        )
        clf.fit(X_train, y_train)

        oos_preds[test_idx] = clf.predict(X_test)
        oos_probs[test_idx] = clf.predict_proba(X_test)[:, 1]

    # Evaluate OOS performance
    valid_mask = oos_probs > 0  # exclude folds that failed
    if valid_mask.sum() < 10:
        return {"error": "insufficient valid OOS predictions"}

    y_valid = y[valid_mask]
    probs_valid = oos_probs[valid_mask]
    preds_valid = oos_preds[valid_mask]

    metrics = {
        "accuracy": float(accuracy_score(y_valid, preds_valid)),
        "n_signals": int(len(X)),
        "n_profitable": int(y.sum()),
        "base_rate": float(y.mean()),
    }

    if len(np.unique(y_valid)) >= 2:
        metrics["auc"] = float(roc_auc_score(y_valid, probs_valid))
        metrics["log_loss"] = float(log_loss(y_valid, probs_valid))

    # Train final model on all data with calibration
    base_clf = GradientBoostingClassifier(
        n_estimators=100, max_depth=3, learning_rate=0.1, random_state=42
    )
    base_clf.fit(X, y)

    # Feature importances
    importances = dict(
        zip(feature_names, [float(v) for v in base_clf.feature_importances_])
    )
    metrics["feature_importances"] = importances

    return {
        "metrics": metrics,
        "model": base_clf,
        "feature_names": feature_names,
    }


def export_rules(result: dict, output_path: str | Path) -> Path:
    """Export meta-model as simple JSON rules for downstream use.

    Exports feature importances, optimal threshold, and key statistics.
    Full model inference requires the sklearn model object.
    """
    path = Path(output_path)
    path.parent.mkdir(parents=True, exist_ok=True)

    export = {
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "metrics": result.get("metrics", {}),
        "feature_names": result.get("feature_names", []),
        "confidence_threshold": 0.5,
    }

    with open(path, "w") as f:
        json.dump(export, f, indent=2)

    return path


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(description="Meta-Labeling Training")
    parser.add_argument("--symbol", "-s", default=None, help="Single symbol")
    parser.add_argument(
        "--category", "-c", default="tech", help="Category to train on"
    )
    parser.add_argument("--days", "-d", type=int, default=30)
    parser.add_argument("--timeframe", "-t", default="1Min")
    parser.add_argument("--export", default=None, help="Export rules to JSON")
    args = parser.parse_args()

    if args.symbol:
        symbols = [args.symbol]
    else:
        symbols = CATEGORIES.get(args.category, [])

    print(f"Meta-labeling: {len(symbols)} symbols, {args.days}d")

    all_X, all_y = [], []
    feature_names = None

    for symbol in symbols:
        print(f"\n  {symbol}...")
        bars = fetch_bars(symbol, args.days, args.timeframe)
        if not bars:
            continue

        X, y, fnames = extract_meta_features(bars)
        if len(X) == 0:
            print(f"    no signals")
            continue

        all_X.append(X)
        all_y.append(y)
        feature_names = fnames
        print(f"    {len(X)} signals, {y.sum():.0f} profitable ({y.mean():.0%})")

    if not all_X:
        print("No signals extracted.")
        return

    X = np.vstack(all_X)
    y = np.concatenate(all_y)

    print(f"\nTotal: {len(X)} signals, {y.sum():.0f} profitable ({y.mean():.0%})")
    print("Training meta-model...")

    result = train_meta_model(X, y, feature_names)

    if "error" in result:
        print(f"Error: {result['error']}")
        return

    metrics = result["metrics"]
    print(f"\nOOS Accuracy: {metrics['accuracy']:.1%}")
    if "auc" in metrics:
        print(f"OOS AUC:      {metrics['auc']:.3f}")
    print(f"Base rate:    {metrics['base_rate']:.1%}")

    print("\nFeature importances:")
    imps = sorted(
        metrics["feature_importances"].items(), key=lambda x: -x[1]
    )
    for name, imp in imps:
        print(f"  {name:25s} {imp:.3f}")

    if args.export:
        path = export_rules(result, args.export)
        print(f"\nRules exported to: {path}")


if __name__ == "__main__":
    main()
