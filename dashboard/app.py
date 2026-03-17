"""
OpenQuant Dashboard — Streamlit app for strategy analysis and data validation.

Usage:
    streamlit run dashboard/app.py

Reads from SQLite journal (data/journal/*.db) and Alpaca market data.
All data queries live in dashboard/queries.py (UI-agnostic, reusable for Grafana).
"""

import os
import sqlite3
from datetime import datetime, timedelta, timezone
from pathlib import Path

import numpy as np
import pandas as pd
import plotly.express as px
import plotly.graph_objects as go
import streamlit as st
from dotenv import load_dotenv

import sys
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from dashboard import queries as Q

load_dotenv()

ROOT = Path(__file__).resolve().parent.parent
JOURNAL_DIR = ROOT / "data" / "journal"


# ---------------------------------------------------------------------------
# UI-only helpers (not in queries.py — these depend on Streamlit or Alpaca SDK)
# ---------------------------------------------------------------------------

def find_journals() -> list[Path]:
    if not JOURNAL_DIR.exists():
        return []
    return sorted(JOURNAL_DIR.glob("*.db"), key=lambda p: p.stat().st_mtime, reverse=True)


def fetch_alpaca_bars(symbol: str, days: int = 7) -> pd.DataFrame:
    try:
        from alpaca.data.historical import StockHistoricalDataClient, CryptoHistoricalDataClient
        from alpaca.data.requests import StockBarsRequest, CryptoBarsRequest
        from alpaca.data.timeframe import TimeFrame
        from alpaca.data.enums import DataFeed

        start = datetime.now(timezone.utc) - timedelta(days=days)
        is_crypto = "/" in symbol
        if is_crypto:
            client = CryptoHistoricalDataClient()
            req = CryptoBarsRequest(symbol_or_symbols=symbol, timeframe=TimeFrame.Minute, start=start)
            bars = client.get_crypto_bars(req)
        else:
            client = StockHistoricalDataClient(os.environ["ALPACA_API_KEY"], os.environ["ALPACA_SECRET_KEY"])
            req = StockBarsRequest(symbol_or_symbols=symbol, timeframe=TimeFrame.Minute, start=start, feed=DataFeed.IEX)
            bars = client.get_stock_bars(req)
        bar_key = symbol if symbol in bars.data else symbol.replace("/", "")
        bar_list = bars.data.get(bar_key, [])
        if not bar_list:
            return pd.DataFrame()
        return pd.DataFrame([{
            "timestamp": int(b.timestamp.timestamp() * 1000), "datetime": b.timestamp,
            "open": float(b.open), "high": float(b.high), "low": float(b.low),
            "close": float(b.close), "volume": float(b.volume),
        } for b in bar_list])
    except Exception as e:
        st.error(f"Could not fetch Alpaca data: {e}")
        return pd.DataFrame()


# ---------------------------------------------------------------------------
# Page config
# ---------------------------------------------------------------------------
st.set_page_config(page_title="OpenQuant Dashboard", layout="wide")
st.title("OpenQuant Dashboard")

tabs = st.tabs([
    "Strategy Performance", "Signal Explorer", "Price Chart",
    "Signal Decay", "Time-of-Day", "Regime Analysis",
    "Feature vs Return", "Drawdown Anatomy", "Strategy Conflicts",
    "Execution Quality", "Data Validation",
])
(tab_strategy, tab_signals, tab_price,
 tab_decay, tab_tod, tab_regime,
 tab_feat_ret, tab_drawdown, tab_conflicts,
 tab_exec, tab_data_check) = tabs


# ---------------------------------------------------------------------------
# Sidebar: journal picker
# ---------------------------------------------------------------------------
journals = find_journals()
if not journals:
    st.sidebar.error("No journal files found in data/journal/")
    st.stop()

journal_path = st.sidebar.selectbox("Journal", journals, format_func=lambda p: p.name)
conn = sqlite3.connect(str(journal_path))

bars_df = Q.load_bars(conn)
# datetime already added by Q.load_bars
symbols = sorted(bars_df["symbol"].unique())
selected_symbols = st.sidebar.multiselect("Symbols", symbols, default=symbols)
min_dt = bars_df["datetime"].min()
max_dt = bars_df["datetime"].max()
st.sidebar.caption(f"Data: {min_dt:%Y-%m-%d} to {max_dt:%Y-%m-%d}")
bars_df = bars_df[bars_df["symbol"].isin(selected_symbols)]

decisions_df = Q.load_decisions(conn)
decisions_df = decisions_df[decisions_df["symbol"].isin(selected_symbols)]

features_df = Q.load_features(conn)
features_df = features_df[features_df["symbol"].isin(selected_symbols)]

trades_df = Q.load_trades(conn)
fills_df = Q.load_fills(conn)


# ---------------------------------------------------------------------------
# Tab 1: Strategy Performance
# ---------------------------------------------------------------------------
with tab_strategy:
    st.header("Strategy Performance")
    st.caption("How each strategy is performing across all selected symbols. Signals = entry/exit triggers the engine detected.")

    total_bars = len(bars_df)
    total_signals = len(decisions_df[decisions_df["signal_fired"] == 1])
    signal_rate = total_signals / total_bars * 100 if total_bars > 0 else 0

    col1, col2, col3, col4 = st.columns(4)
    col1.metric("Total Bars", f"{total_bars:,}")
    col2.metric("Signals Fired", f"{total_signals:,}")
    col3.metric("Signal Rate", f"{signal_rate:.2f}%")
    col4.metric("Symbols", len(selected_symbols))

    st.subheader("Signal Breakdown by Strategy")
    strat_metrics = Q.strategy_summary(conn, selected_symbols)
    if not strat_metrics.empty:
        st.dataframe(strat_metrics, use_container_width=True, hide_index=True)
    else:
        st.info("No signals found in this journal.")

    if not trades_df.empty:
        st.subheader("Round-Trip Trade Metrics")
        trade_metrics = Q.trade_summary(conn)
        st.dataframe(trade_metrics, use_container_width=True, hide_index=True)

        st.subheader("Cumulative P&L")
        trades_df["cum_pnl"] = trades_df["pnl"].cumsum()
        fig = go.Figure()
        fig.add_trace(go.Scatter(
            y=trades_df["cum_pnl"], mode="lines+markers", name="Cumulative P&L",
            line=dict(color="green" if trades_df["cum_pnl"].iloc[-1] > 0 else "red"),
        ))
        fig.update_layout(height=400, yaxis_title="P&L ($)", xaxis_title="Trade #")
        st.plotly_chart(fig, use_container_width=True)
    else:
        st.info("No round-trip trades. Run live with `--journal` to capture fills.")

    st.subheader("Signals by Symbol")
    sig_by_sym = decisions_df[decisions_df["signal_fired"] == 1].groupby(["symbol", "signal_side"]).size().unstack(fill_value=0)
    if not sig_by_sym.empty:
        fig = go.Figure()
        for side in sig_by_sym.columns:
            fig.add_trace(go.Bar(name=side, x=sig_by_sym.index, y=sig_by_sym[side]))
        fig.update_layout(barmode="group", height=350, yaxis_title="Count")
        st.plotly_chart(fig, use_container_width=True)


# ---------------------------------------------------------------------------
# Tab 2: Signal Explorer
# ---------------------------------------------------------------------------
with tab_signals:
    st.header("Signal Explorer")
    st.caption("Drill into individual signals — their scores, timing, and which passed risk gates.")

    signals = decisions_df[decisions_df["signal_fired"] == 1].copy()
    if signals.empty:
        st.info("No signals in this journal.")
    else:
        st.subheader("Score Distribution")
        fig = go.Figure()
        fig.add_trace(go.Histogram(x=signals["signal_score"], nbinsx=30, name="Signal Scores"))
        fig.update_layout(height=300, xaxis_title="Score", yaxis_title="Count")
        st.plotly_chart(fig, use_container_width=True)

        st.subheader("Signal Scores Over Time")
        fig = go.Figure()
        for sym in selected_symbols:
            sym_sigs = signals[signals["symbol"] == sym]
            if not sym_sigs.empty:
                fig.add_trace(go.Scatter(
                    x=sym_sigs["datetime"], y=sym_sigs["signal_score"],
                    mode="markers", name=sym,
                    marker=dict(size=8, symbol=[
                        "triangle-up" if s == "buy" else "triangle-down" for s in sym_sigs["signal_side"]
                    ]),
                ))
        fig.update_layout(height=400, yaxis_title="Score", xaxis_title="Time")
        st.plotly_chart(fig, use_container_width=True)

        st.subheader("Recent Signals")
        display_cols = ["datetime", "symbol", "signal_side", "signal_score", "signal_reason", "risk_passed", "qty_approved", "close"]
        available_cols = [c for c in display_cols if c in signals.columns]
        st.dataframe(signals[available_cols].tail(50).sort_values("datetime", ascending=False),
                     use_container_width=True, hide_index=True)


# ---------------------------------------------------------------------------
# Tab 3: Price Chart
# ---------------------------------------------------------------------------
with tab_price:
    st.header("Price Chart")
    st.caption("Candlestick chart with buy/sell signal overlay. Like Google Finance but with your trades marked.")

    chart_symbol = st.selectbox("Symbol", selected_symbols, key="price_sym")
    sym_bars = bars_df[bars_df["symbol"] == chart_symbol].copy()

    if sym_bars.empty:
        st.info(f"No data for {chart_symbol}")
    else:
        sym_bars = sym_bars.set_index("datetime").sort_index()
        if len(sym_bars) > 2000:
            sym_bars_r = sym_bars.resample("5min").agg({"open": "first", "high": "max", "low": "min", "close": "last", "volume": "sum"}).dropna()
        else:
            sym_bars_r = sym_bars

        fig = go.Figure(data=[go.Candlestick(
            x=sym_bars_r.index, open=sym_bars_r["open"], high=sym_bars_r["high"],
            low=sym_bars_r["low"], close=sym_bars_r["close"], name=chart_symbol,
        )])
        sym_signals = decisions_df[(decisions_df["symbol"] == chart_symbol) & (decisions_df["signal_fired"] == 1)]
        if not sym_signals.empty:
            buys = sym_signals[sym_signals["signal_side"] == "buy"]
            sells = sym_signals[sym_signals["signal_side"] == "sell"]
            if not buys.empty:
                fig.add_trace(go.Scatter(x=buys["datetime"], y=buys["close"], mode="markers", name="BUY",
                                        marker=dict(symbol="triangle-up", size=12, color="lime")))
            if not sells.empty:
                fig.add_trace(go.Scatter(x=sells["datetime"], y=sells["close"], mode="markers", name="SELL",
                                        marker=dict(symbol="triangle-down", size=12, color="red")))

        first_c = sym_bars_r["close"].iloc[0]
        last_c = sym_bars_r["close"].iloc[-1]
        change_pct = (last_c - first_c) / first_c * 100
        fig.update_layout(height=500, title=f"{chart_symbol}  ${last_c:.2f}  ({change_pct:+.2f}%)",
                          xaxis_rangeslider_visible=False, yaxis_title="Price ($)")
        st.plotly_chart(fig, use_container_width=True)

        fig_vol = go.Figure(data=[go.Bar(x=sym_bars_r.index, y=sym_bars_r["volume"], marker_color="steelblue", opacity=0.6)])
        fig_vol.update_layout(height=200, yaxis_title="Volume", margin=dict(t=10))
        st.plotly_chart(fig_vol, use_container_width=True)

        st.subheader("Indicator Overlay")
        sym_feats = features_df[features_df["symbol"] == chart_symbol].set_index("datetime").sort_index()
        if not sym_feats.empty:
            indicator = st.selectbox("Indicator", [
                "return_z_score", "relative_volume", "adx", "bollinger_pct_b",
                "ema_fast", "ema_slow", "sma_20", "sma_50", "atr",
            ])
            if indicator in sym_feats.columns:
                fig_ind = go.Figure()
                fig_ind.add_trace(go.Scatter(x=sym_feats.index, y=sym_feats[indicator], mode="lines", name=indicator, line=dict(color="orange")))
                fig_ind.update_layout(height=250, yaxis_title=indicator, margin=dict(t=10))
                st.plotly_chart(fig_ind, use_container_width=True)


# ---------------------------------------------------------------------------
# Tab 4: Signal Decay Curve
# ---------------------------------------------------------------------------
with tab_decay:
    st.header("Signal Decay Analysis")
    st.caption(
        "Do higher-score signals actually produce better returns? "
        "If this chart is flat, our scoring formula isn't adding value — we're trading noise."
    )

    signals = decisions_df[decisions_df["signal_fired"] == 1].copy()
    if signals.empty or "signal_score" not in signals.columns:
        st.info("Need signals with scores to analyze decay.")
    else:
        # For each buy signal, compute forward return (next 10, 20, 50 bars)
        signals["strategy"] = signals["signal_reason"].apply(Q.extract_strategy)
        buy_sigs = signals[signals["signal_side"] == "buy"].copy()

        if buy_sigs.empty:
            st.info("No buy signals to analyze.")
        else:
            forward_returns = []
            for _, sig in buy_sigs.iterrows():
                sym_bars_after = bars_df[(bars_df["symbol"] == sig["symbol"]) & (bars_df["timestamp"] > sig["timestamp"])].head(50)
                if len(sym_bars_after) >= 10:
                    ret_10 = (sym_bars_after.iloc[9]["close"] - sig["close"]) / sig["close"] * 100
                    ret_20 = (sym_bars_after.iloc[min(19, len(sym_bars_after)-1)]["close"] - sig["close"]) / sig["close"] * 100 if len(sym_bars_after) >= 20 else None
                    ret_50 = (sym_bars_after.iloc[min(49, len(sym_bars_after)-1)]["close"] - sig["close"]) / sig["close"] * 100 if len(sym_bars_after) >= 50 else None
                    forward_returns.append({
                        "score": sig["signal_score"], "strategy": sig["strategy"],
                        "symbol": sig["symbol"], "ret_10": ret_10, "ret_20": ret_20, "ret_50": ret_50,
                    })

            if forward_returns:
                fr_df = pd.DataFrame(forward_returns)

                # Bucket by score
                fr_df["score_bucket"] = pd.cut(fr_df["score"], bins=5, labels=False)
                bucket_stats = fr_df.groupby("score_bucket").agg(
                    avg_score=("score", "mean"),
                    avg_ret_10=("ret_10", "mean"),
                    avg_ret_20=("ret_20", "mean"),
                    count=("score", "count"),
                ).reset_index()

                fig = go.Figure()
                fig.add_trace(go.Bar(x=bucket_stats["avg_score"].round(2).astype(str), y=bucket_stats["avg_ret_10"],
                                     name="10-bar return %", marker_color="steelblue"))
                if "avg_ret_20" in bucket_stats.columns:
                    fig.add_trace(go.Bar(x=bucket_stats["avg_score"].round(2).astype(str), y=bucket_stats["avg_ret_20"],
                                         name="20-bar return %", marker_color="orange"))
                fig.update_layout(height=400, xaxis_title="Avg Signal Score (bucket)", yaxis_title="Avg Forward Return %",
                                  barmode="group", title="Higher score should = higher return. If flat, scoring is broken.")
                st.plotly_chart(fig, use_container_width=True)

                # Scatter: score vs return
                fig2 = px.scatter(fr_df, x="score", y="ret_10", color="strategy", hover_data=["symbol"],
                                  title="Score vs 10-bar Forward Return (each dot = one buy signal)",
                                  labels={"ret_10": "10-bar Return %", "score": "Signal Score"})
                fig2.add_hline(y=0, line_dash="dash", line_color="gray")
                fig2.update_layout(height=400)
                st.plotly_chart(fig2, use_container_width=True)
            else:
                st.info("Not enough forward data to compute returns.")


# ---------------------------------------------------------------------------
# Tab 5: Time-of-Day Heatmap
# ---------------------------------------------------------------------------
with tab_tod:
    st.header("Time-of-Day Heatmap")
    st.caption(
        "When do signals fire and when do they work? Most alpha is concentrated in specific windows "
        "(open, close, post-lunch). If we're trading during dead zones, we're burning capital on noise."
    )

    signals = decisions_df[decisions_df["signal_fired"] == 1].copy()
    if signals.empty:
        st.info("No signals to analyze.")
    else:
        signals["hour"] = signals["datetime"].dt.hour
        signals["strategy"] = signals["signal_reason"].apply(Q.extract_strategy)

        # Signal count heatmap: hour x strategy
        heatmap_data = signals.groupby(["hour", "strategy"]).size().unstack(fill_value=0)
        fig = px.imshow(heatmap_data.T, aspect="auto", color_continuous_scale="YlOrRd",
                        labels=dict(x="Hour (UTC)", y="Strategy", color="Signal Count"),
                        title="Signal Frequency by Hour — bright spots are when each strategy is most active")
        fig.update_layout(height=350)
        st.plotly_chart(fig, use_container_width=True)

        # Score by hour
        score_by_hour = signals.groupby("hour")["signal_score"].agg(["mean", "count"]).reset_index()
        fig2 = go.Figure()
        fig2.add_trace(go.Bar(x=score_by_hour["hour"], y=score_by_hour["count"], name="Count", yaxis="y", marker_color="steelblue", opacity=0.5))
        fig2.add_trace(go.Scatter(x=score_by_hour["hour"], y=score_by_hour["mean"], name="Avg Score", yaxis="y2", line=dict(color="red", width=3)))
        fig2.update_layout(
            height=350, title="Signal count (bars) vs average score (line) — high count + high score = prime trading hours",
            yaxis=dict(title="Count"), yaxis2=dict(title="Avg Score", overlaying="y", side="right"),
            xaxis=dict(title="Hour (UTC)", dtick=1),
        )
        st.plotly_chart(fig2, use_container_width=True)

        # If we have trades, show win rate by hour
        if not trades_df.empty:
            trades_with_hour = trades_df.copy()
            # Approximate hour from entry fill
            fills_with_ts = fills_df.copy()
            if not fills_with_ts.empty and "timestamp" in fills_with_ts.columns:
                fills_with_ts["hour"] = fills_with_ts["timestamp"].apply(lambda t: Q._ts_to_dt(t).hour if pd.notna(t) else None)
                st.info("Win rate by hour requires fill timestamps in journal.")


# ---------------------------------------------------------------------------
# Tab 6: Regime Analysis
# ---------------------------------------------------------------------------
with tab_regime:
    st.header("Regime Analysis")
    st.caption(
        "Markets alternate between trending (ADX > 25), ranging (ADX < 15), and volatile (ATR spikes) regimes. "
        "Each strategy works best in a specific regime. If momentum fires in a ranging market, that's a bug."
    )

    regime_sym = st.selectbox("Symbol", selected_symbols, key="regime_sym")
    sym_feats = features_df[features_df["symbol"] == regime_sym].copy()

    if sym_feats.empty or "adx" not in sym_feats.columns:
        st.info(f"No feature data for {regime_sym}.")
    else:
        sym_feats = sym_feats.set_index("datetime").sort_index()

        # Classify regime
        sym_feats["regime"] = "ranging"
        sym_feats.loc[sym_feats["adx"] > 25, "regime"] = "trending"
        sym_feats.loc[sym_feats["adx"] > 40, "regime"] = "strong-trend"

        # Color map
        regime_colors = {"ranging": "yellow", "trending": "green", "strong-trend": "blue"}

        # Price chart colored by regime
        fig = go.Figure()
        for regime, color in regime_colors.items():
            mask = sym_feats["regime"] == regime
            if mask.any():
                fig.add_trace(go.Scatter(
                    x=sym_feats.index[mask], y=sym_feats["close"][mask],
                    mode="markers", name=regime, marker=dict(color=color, size=3),
                ))

        # Overlay signals
        sym_sigs = decisions_df[(decisions_df["symbol"] == regime_sym) & (decisions_df["signal_fired"] == 1)]
        if not sym_sigs.empty:
            for _, sig in sym_sigs.iterrows():
                fig.add_annotation(
                    x=sig["datetime"], y=sig["close"],
                    text="B" if sig["signal_side"] == "buy" else "S",
                    showarrow=True, arrowhead=2, arrowsize=1, arrowwidth=1,
                    font=dict(size=10, color="lime" if sig["signal_side"] == "buy" else "red"),
                )

        fig.update_layout(height=450, title=f"{regime_sym} — Price colored by regime (yellow=ranging, green=trending, blue=strong-trend)",
                          yaxis_title="Price ($)")
        st.plotly_chart(fig, use_container_width=True)

        # ADX chart
        fig_adx = go.Figure()
        fig_adx.add_trace(go.Scatter(x=sym_feats.index, y=sym_feats["adx"], mode="lines", name="ADX", line=dict(color="purple")))
        fig_adx.add_hline(y=25, line_dash="dash", line_color="green", annotation_text="Trending threshold")
        fig_adx.add_hline(y=15, line_dash="dash", line_color="red", annotation_text="Ranging threshold")
        fig_adx.update_layout(height=250, yaxis_title="ADX", title="ADX trend strength — above 25 = trend, below 15 = range")
        st.plotly_chart(fig_adx, use_container_width=True)

        # Regime distribution
        regime_counts = sym_feats["regime"].value_counts()
        col1, col2, col3 = st.columns(3)
        col1.metric("Ranging", f"{regime_counts.get('ranging', 0):,} bars ({regime_counts.get('ranging', 0)/len(sym_feats)*100:.0f}%)")
        col2.metric("Trending", f"{regime_counts.get('trending', 0):,} bars ({regime_counts.get('trending', 0)/len(sym_feats)*100:.0f}%)")
        col3.metric("Strong Trend", f"{regime_counts.get('strong-trend', 0):,} bars ({regime_counts.get('strong-trend', 0)/len(sym_feats)*100:.0f}%)")

        # Signal firing by regime
        if not sym_sigs.empty:
            st.subheader("Signals per Regime")
            sig_times = sym_sigs.set_index("datetime")
            sig_regimes = []
            for dt in sig_times.index:
                nearest = sym_feats.index.get_indexer([dt], method="nearest")[0]
                if 0 <= nearest < len(sym_feats):
                    sig_regimes.append(sym_feats.iloc[nearest]["regime"])
                else:
                    sig_regimes.append("unknown")
            sig_times["regime"] = sig_regimes
            sig_times["strategy"] = sig_times["signal_reason"].apply(Q.extract_strategy)
            regime_sig = sig_times.groupby(["regime", "strategy"]).size().unstack(fill_value=0)
            st.dataframe(regime_sig, use_container_width=True)


# ---------------------------------------------------------------------------
# Tab 7: Feature vs Return
# ---------------------------------------------------------------------------
with tab_feat_ret:
    st.header("Feature vs Forward Return")
    st.caption(
        "Does this feature actually predict future returns? Plot feature value vs next-N-bar return. "
        "If mean-reversion works, negative z-scores should be followed by positive returns (upward slope on left side)."
    )

    feat_sym = st.selectbox("Symbol", selected_symbols, key="feat_sym")
    feature_name = st.selectbox("Feature", [
        "return_z_score", "relative_volume", "adx", "bollinger_pct_b",
        "ema_fast_above_slow", "trend_up", "close_location",
    ], key="feat_name")
    forward_bars = st.selectbox("Forward return window", [5, 10, 20, 50], index=1, key="feat_fwd")

    sym_feats = features_df[features_df["symbol"] == feat_sym].copy()
    sym_bars_local = bars_df[bars_df["symbol"] == feat_sym].sort_values("timestamp").copy()

    if sym_feats.empty or feature_name not in sym_feats.columns:
        st.info(f"No {feature_name} data for {feat_sym}.")
    else:
        # Compute forward returns
        sym_bars_local["fwd_return"] = sym_bars_local["close"].shift(-forward_bars) / sym_bars_local["close"] - 1
        sym_bars_local["fwd_return_pct"] = sym_bars_local["fwd_return"] * 100

        merged = pd.merge(
            sym_feats[["bar_id", feature_name]],
            sym_bars_local[["id", "fwd_return_pct"]].rename(columns={"id": "bar_id"}),
            on="bar_id",
        ).dropna()

        if merged.empty:
            st.info("Not enough data to compute forward returns.")
        else:
            # Scatter
            fig = px.scatter(merged, x=feature_name, y="fwd_return_pct", opacity=0.3,
                             title=f"{feature_name} vs {forward_bars}-bar forward return — look for a slope (predictive power)",
                             labels={"fwd_return_pct": f"{forward_bars}-bar Forward Return %"})
            fig.add_hline(y=0, line_dash="dash", line_color="gray")
            fig.update_layout(height=450)
            st.plotly_chart(fig, use_container_width=True)

            # Bucketed average
            merged["bucket"] = pd.qcut(merged[feature_name], q=10, duplicates="drop")
            bucket_stats = merged.groupby("bucket")["fwd_return_pct"].agg(["mean", "count"]).reset_index()
            bucket_stats["bucket_str"] = bucket_stats["bucket"].astype(str)

            fig2 = go.Figure()
            fig2.add_trace(go.Bar(x=bucket_stats["bucket_str"], y=bucket_stats["mean"], marker_color=[
                "green" if v > 0 else "red" for v in bucket_stats["mean"]
            ]))
            fig2.update_layout(height=350, title=f"Avg {forward_bars}-bar return by {feature_name} decile — green bars = profitable zones",
                               xaxis_title=f"{feature_name} (decile)", yaxis_title="Avg Forward Return %")
            st.plotly_chart(fig2, use_container_width=True)

            # Correlation
            corr = merged[feature_name].corr(merged["fwd_return_pct"])
            st.metric(f"Correlation ({feature_name} vs {forward_bars}-bar return)", f"{corr:.4f}",
                      help="Close to 0 = no prediction. Negative = feature inversely predicts. > 0.05 = potentially useful.")


# ---------------------------------------------------------------------------
# Tab 8: Drawdown Anatomy
# ---------------------------------------------------------------------------
with tab_drawdown:
    st.header("Drawdown Anatomy")
    st.caption(
        "Break down each losing period by which strategy caused the losses. "
        "Was it momentum buying into a reversal? Mean-rev catching a falling knife? Tells you what to fix."
    )

    if trades_df.empty:
        st.info("Need round-trip trades in journal for drawdown analysis. Run live with `--journal`.")
    else:
        trades_local = trades_df.copy()
        trades_local["cum_pnl"] = trades_local["pnl"].cumsum()
        trades_local["peak"] = trades_local["cum_pnl"].cummax()
        trades_local["drawdown"] = trades_local["cum_pnl"] - trades_local["peak"]
        trades_local["strategy"] = trades_local["exit_reason"].apply(Q.extract_strategy)

        # Equity curve + drawdown
        fig = go.Figure()
        fig.add_trace(go.Scatter(y=trades_local["cum_pnl"], mode="lines", name="Equity", line=dict(color="blue")))
        fig.add_trace(go.Scatter(y=trades_local["drawdown"], mode="lines", name="Drawdown", fill="tozeroy",
                                 line=dict(color="red"), opacity=0.3))
        fig.update_layout(height=400, title="Equity curve (blue) and drawdown (red fill) — flat red = money being lost",
                          yaxis_title="$")
        st.plotly_chart(fig, use_container_width=True)

        # P&L by strategy (who's making/losing money)
        strat_pnl = trades_local.groupby("strategy")["pnl"].sum().sort_values()
        fig2 = go.Figure(data=[go.Bar(
            x=strat_pnl.values, y=strat_pnl.index, orientation="h",
            marker_color=["green" if v > 0 else "red" for v in strat_pnl.values],
        )])
        fig2.update_layout(height=300, title="Total P&L by exit strategy — red bars are bleeding money",
                           xaxis_title="P&L ($)")
        st.plotly_chart(fig2, use_container_width=True)

        # Losing streaks
        trades_local["is_loss"] = trades_local["pnl"] < 0
        trades_local["streak_id"] = (trades_local["is_loss"] != trades_local["is_loss"].shift()).cumsum()
        loss_streaks = trades_local[trades_local["is_loss"]].groupby("streak_id").agg(
            length=("pnl", "count"), total_loss=("pnl", "sum"),
            strategies=("strategy", lambda x: ", ".join(x.unique())),
        ).sort_values("total_loss")

        st.subheader("Worst Losing Streaks")
        st.caption("Consecutive losses grouped — shows which strategy combos produce losing runs.")
        st.dataframe(loss_streaks.head(10), use_container_width=True)


# ---------------------------------------------------------------------------
# Tab 9: Strategy Conflicts
# ---------------------------------------------------------------------------
with tab_conflicts:
    st.header("Strategy Conflict Timeline")
    st.caption(
        "When do strategies disagree? If they always agree, the combiner adds nothing. "
        "If they disagree often and the winner is random, we need better conflict resolution."
    )

    # We can detect conflicts by looking at bars where signals fired
    # and checking if different strategies would have given different signals
    signals = decisions_df[decisions_df["signal_fired"] == 1].copy()

    if signals.empty:
        st.info("No signals to analyze conflicts.")
    else:
        signals["strategy"] = signals["signal_reason"].apply(Q.extract_strategy)

        # Group by bar (timestamp + symbol) to find bars with multiple signals
        # Since combiner picks one, we look at bars where signal_reason mentions different strategies over time
        # Approximate: look at consecutive signals for same symbol with different strategies
        signals = signals.sort_values(["symbol", "timestamp"])
        signals["prev_strategy"] = signals.groupby("symbol")["strategy"].shift(1)
        signals["prev_side"] = signals.groupby("symbol")["signal_side"].shift(1)
        signals["conflict"] = (signals["strategy"] != signals["prev_strategy"]) & signals["prev_strategy"].notna()
        signals["side_flip"] = (signals["signal_side"] != signals["prev_side"]) & signals["prev_side"].notna()

        conflicts = signals[signals["conflict"]]
        side_flips = signals[signals["side_flip"]]

        col1, col2, col3 = st.columns(3)
        col1.metric("Strategy Switches", f"{len(conflicts):,}",
                     help="Times the winning strategy changed from one bar to the next")
        col2.metric("Side Flips (Buy<->Sell)", f"{len(side_flips):,}",
                     help="Times direction flipped — frequent flips = whipsawing")
        col3.metric("Total Signals", f"{len(signals):,}")

        # Strategy transition matrix
        st.subheader("Strategy Transition Matrix")
        st.caption("How often does strategy A hand off to strategy B? Off-diagonal = strategy switching.")
        if not conflicts.empty:
            trans = conflicts.groupby(["prev_strategy", "strategy"]).size().unstack(fill_value=0)
            fig = px.imshow(trans, text_auto=True, color_continuous_scale="Blues",
                            labels=dict(x="To Strategy", y="From Strategy", color="Count"),
                            title="Strategy handoff frequency — dark cells = common transitions")
            fig.update_layout(height=400)
            st.plotly_chart(fig, use_container_width=True)

        # Flip rate over time
        st.subheader("Whipsaw Rate Over Time")
        st.caption("Side flips per hour. High flip rate = strategies are fighting each other.")
        if not side_flips.empty:
            side_flips["hour_bucket"] = side_flips["datetime"].dt.floor("1h")
            flip_rate = side_flips.groupby("hour_bucket").size().reset_index(name="flips")
            fig = go.Figure()
            fig.add_trace(go.Bar(x=flip_rate["hour_bucket"], y=flip_rate["flips"], marker_color="orange"))
            fig.update_layout(height=300, yaxis_title="Side Flips / Hour", xaxis_title="Time")
            st.plotly_chart(fig, use_container_width=True)


# ---------------------------------------------------------------------------
# Tab 10: Execution Quality
# ---------------------------------------------------------------------------
with tab_exec:
    st.header("Execution Quality")
    st.caption(
        "Compare the price when we decided to trade vs the price we actually got. "
        "The difference is slippage — money left on the table every single trade."
    )

    if fills_df.empty:
        st.info("No fills in journal. Run live with `--journal` to capture execution data.")
    else:
        fills_local = fills_df.copy()
        if "bar_close" in fills_local.columns and "fill_price" in fills_local.columns:
            fills_local["slippage_bps"] = (fills_local["fill_price"] - fills_local["bar_close"]) / fills_local["bar_close"] * 10000

            col1, col2, col3 = st.columns(3)
            col1.metric("Total Fills", f"{len(fills_local):,}")
            col2.metric("Avg Slippage", f"{fills_local['slippage_bps'].mean():.2f} bps")
            col3.metric("Worst Slippage", f"{fills_local['slippage_bps'].abs().max():.2f} bps")

            # Slippage distribution
            fig = go.Figure()
            fig.add_trace(go.Histogram(x=fills_local["slippage_bps"], nbinsx=30, name="Slippage"))
            fig.add_vline(x=0, line_dash="dash", line_color="gray")
            fig.update_layout(height=350, xaxis_title="Slippage (bps)", yaxis_title="Count",
                              title="Slippage distribution — centered on 0 is good, skew right = paying too much")
            st.plotly_chart(fig, use_container_width=True)

            # Slippage by symbol
            if "symbol" in fills_local.columns:
                slip_by_sym = fills_local.groupby("symbol")["slippage_bps"].agg(["mean", "std", "count"]).round(2)
                st.subheader("Slippage by Symbol")
                st.dataframe(slip_by_sym, use_container_width=True)

            # Slippage over time
            if "timestamp" in fills_local.columns:
                fills_local["datetime"] = fills_local["timestamp"].apply(Q._ts_to_dt)
                fig2 = go.Figure()
                fig2.add_trace(go.Scatter(x=fills_local["datetime"], y=fills_local["slippage_bps"],
                                          mode="markers", marker=dict(size=5, color="red")))
                fig2.add_hline(y=0, line_dash="dash", line_color="gray")
                fig2.update_layout(height=300, title="Slippage per fill over time — look for degradation patterns",
                                   yaxis_title="Slippage (bps)")
                st.plotly_chart(fig2, use_container_width=True)
        else:
            st.info("Fill data missing price columns for slippage analysis.")


# ---------------------------------------------------------------------------
# Tab 11: Data Validation
# ---------------------------------------------------------------------------
with tab_data_check:
    st.header("Data Validation")
    st.caption("Compare our journal bars against fresh Alpaca data to verify we're trading on correct prices.")

    val_symbol = st.selectbox("Symbol", ["AAPL", "NVDA", "TSLA", "AMD", "SPY", "MSFT", "GOOG"], key="val_sym")
    val_days = st.selectbox("Days", [1, 3, 7], index=0, key="val_days")

    if st.button("Fetch & Compare"):
        with st.spinner("Fetching Alpaca data..."):
            alpaca_bars = fetch_alpaca_bars(val_symbol, days=val_days)
        if alpaca_bars.empty:
            st.warning("No Alpaca data returned.")
        else:
            our_bars = bars_df[bars_df["symbol"] == val_symbol].copy()
            if our_bars.empty:
                st.warning(f"No journal data for {val_symbol}.")
            else:
                merged = pd.merge(
                    our_bars[["timestamp", "close", "volume"]].rename(columns={"close": "our_close", "volume": "our_volume"}),
                    alpaca_bars[["timestamp", "close", "volume"]].rename(columns={"close": "alpaca_close", "volume": "alpaca_volume"}),
                    on="timestamp", how="inner",
                )
                if merged.empty:
                    st.warning("No overlapping timestamps.")
                else:
                    merged["datetime"] = merged["timestamp"].apply(Q._ts_to_dt)
                    merged["price_diff"] = merged["our_close"] - merged["alpaca_close"]
                    merged["price_diff_pct"] = (merged["price_diff"] / merged["alpaca_close"]) * 100

                    col1, col2, col3, col4 = st.columns(4)
                    col1.metric("Matched Bars", f"{len(merged):,}")
                    col2.metric("Avg Price Diff", f"{merged['price_diff_pct'].mean():.4f}%")
                    col3.metric("Max Price Diff", f"{merged['price_diff_pct'].abs().max():.4f}%")
                    col4.metric("Exact Matches", f"{(merged['price_diff'] == 0).sum():,}")

                    fig = go.Figure()
                    fig.add_trace(go.Scatter(x=merged["datetime"], y=merged["our_close"], mode="lines", name="Our Data", line=dict(color="blue")))
                    fig.add_trace(go.Scatter(x=merged["datetime"], y=merged["alpaca_close"], mode="lines", name="Alpaca", line=dict(color="orange", dash="dot")))
                    fig.update_layout(height=400, title=f"{val_symbol} Price Comparison", yaxis_title="Price ($)")
                    st.plotly_chart(fig, use_container_width=True)

                    fig_diff = go.Figure()
                    fig_diff.add_trace(go.Scatter(x=merged["datetime"], y=merged["price_diff_pct"], mode="lines", name="Diff %", line=dict(color="red")))
                    fig_diff.add_hline(y=0, line_dash="dash", line_color="gray")
                    fig_diff.update_layout(height=250, title="Price Difference (%) — should be flat at zero", yaxis_title="Diff %")
                    st.plotly_chart(fig_diff, use_container_width=True)

                    st.subheader("Largest Discrepancies")
                    worst = merged.nlargest(10, "price_diff_pct")[["datetime", "our_close", "alpaca_close", "price_diff", "price_diff_pct"]]
                    st.dataframe(worst, use_container_width=True, hide_index=True)

conn.close()
