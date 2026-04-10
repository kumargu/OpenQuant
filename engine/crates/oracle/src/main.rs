//! Oracle — brute-force pair replay over the foundation dataset.
//!
//! Runs the real `PairState::on_price` from `openquant_core::pairs` on every
//! ordered pair (A < B lexicographically) in the universe, over the eval
//! period, in intraday rolling z-score mode. Classifies each pair as
//! WINNER / AVERAGE / LOSER / INACTIVE per `autoresearch/ORACLE_SPEC.md`.
//!
//! This is the ground truth that every picker experiment is measured against.
//! It imports strategy code directly from `openquant-core` — there is no
//! parallel strategy implementation anywhere in the oracle.
//!
//! Usage:
//!     oracle \
//!         --bars-dir ~/quant-data/bars/v1_sp500_2025-2026_1min \
//!         --start 2025-09-01 --end 2025-11-30 \
//!         --output ~/quant-data/oracle/v1_bars/v1_strategy_z30_entry2_exit0_cost5bps

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use arrow::array::{
    Array, Float64Array, Int32Array, Int64Array, StringArray, TimestampMicrosecondArray,
};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use chrono::{Datelike, NaiveDate, TimeZone, Utc};
use clap::Parser;
use openquant_core::pairs::{PairConfig, PairPosition, PairState, PairsTradingConfig};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use tracing::info;

// ── CLI ──

#[derive(Parser, Debug)]
#[command(about = "Brute-force pair replay oracle (Mode A)")]
struct Cli {
    /// Directory containing per-symbol parquet files (one file per symbol).
    #[arg(long)]
    bars_dir: PathBuf,

    /// Eval period start (YYYY-MM-DD, UTC, inclusive).
    #[arg(long)]
    start: String,

    /// Eval period end (YYYY-MM-DD, UTC, inclusive).
    #[arg(long)]
    end: String,

    /// Output directory for verdicts.parquet + MANIFEST.json.
    #[arg(long)]
    output: PathBuf,

    /// Rolling window size in intraday bars (minutes). Default 30.
    #[arg(long, default_value_t = 30)]
    intraday_bars: usize,

    /// Entry z-score threshold. Default 2.0.
    #[arg(long, default_value_t = 2.0)]
    entry_z: f64,

    /// Exit z-score threshold. Default 0.0 (exit at mean crossing).
    #[arg(long, default_value_t = 0.0)]
    exit_z: f64,

    /// Notional per leg in USD. Default 5000 (matches ORACLE_SPEC v1).
    #[arg(long, default_value_t = 5000.0)]
    notional_per_leg: f64,

    /// Round-trip cost in basis points. Default 10 (5 bps per side).
    #[arg(long, default_value_t = 10.0)]
    cost_bps: f64,

    /// Optional symbol filter — comma-separated list to restrict the universe
    /// (useful for smoke testing). If omitted, every symbol in bars_dir is used.
    #[arg(long)]
    symbols: Option<String>,

    /// Optional symbol exclusion — comma-separated list to REMOVE from the
    /// universe (e.g. symbols with unadjusted corporate actions).
    #[arg(long)]
    exclude: Option<String>,

    /// Maximum number of pairs to evaluate (for smoke testing). 0 = no limit.
    #[arg(long, default_value_t = 0)]
    max_pairs: usize,

    /// Number of worker threads for pair evaluation. 0 = rayon default.
    #[arg(long, default_value_t = 0)]
    threads: usize,
}

// ── Data structures ──

#[derive(Debug, Clone)]
struct Bar {
    ts_us: i64, // microseconds since epoch (UTC)
    close: f64,
}

#[derive(Debug)]
struct JoinedBar {
    ts_us: i64,
    close_a: f64,
    close_b: f64,
}

#[derive(Debug, Clone, Copy)]
enum TradeDir {
    Long,
    Short,
}

#[derive(Debug)]
struct OpenTrade {
    entry_ts_us: i64,
    entry_price_a: f64,
    entry_price_b: f64,
    qty_a: f64,
    qty_b: f64,
    direction: TradeDir,
}

#[derive(Debug, Clone)]
struct ClosedTrade {
    net_bps: f64,
    hold_minutes: i64,
    month: String, // "YYYY-MM" of entry
}

#[derive(Debug, Serialize, Deserialize)]
struct PairVerdict {
    leg_a: String,
    leg_b: String,
    total_trades: i32,
    winning_trades: i32,
    win_pct: f64,
    total_net_bps: i64,
    avg_net_bps: f64,
    median_hold_minutes: f64,
    max_hold_minutes: i64,
    /// JSON-encoded {"YYYY-MM": bps_int, ...}
    monthly_bps_json: String,
    classification: String,
}

// ── Classification thresholds (from ORACLE_SPEC.md v1) ──
const CLASSIFY_MIN_TRADES: i32 = 20;
const CLASSIFY_WINNER_BPS: i64 = 300;
const CLASSIFY_WINNER_PROFIT_MONTHS_PCT: f64 = 0.60;

// ── Main ──

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("oracle=info")),
        )
        .init();

    let cli = Cli::parse();

    if cli.threads > 0 {
        rayon::ThreadPoolBuilder::new()
            .num_threads(cli.threads)
            .build_global()
            .expect("rayon thread pool init");
    }

    let start_date = NaiveDate::parse_from_str(&cli.start, "%Y-%m-%d")
        .expect("start must be YYYY-MM-DD");
    let end_date = NaiveDate::parse_from_str(&cli.end, "%Y-%m-%d")
        .expect("end must be YYYY-MM-DD");
    // inclusive end: convert to end of day UTC
    let start_us = Utc
        .from_utc_datetime(&start_date.and_hms_opt(0, 0, 0).unwrap())
        .timestamp_micros();
    let end_us = Utc
        .from_utc_datetime(&end_date.and_hms_opt(23, 59, 59).unwrap())
        .timestamp_micros();

    info!(
        bars_dir = %cli.bars_dir.display(),
        start = cli.start.as_str(),
        end = cli.end.as_str(),
        intraday_bars = cli.intraday_bars,
        entry_z = cli.entry_z,
        exit_z = cli.exit_z,
        notional_per_leg = cli.notional_per_leg,
        cost_bps = cli.cost_bps,
        "oracle: starting"
    );

    // 1. Discover symbols
    let mut symbols = discover_symbols(&cli.bars_dir, cli.symbols.as_deref());
    if let Some(exclude) = cli.exclude.as_deref() {
        let excluded: std::collections::HashSet<&str> = exclude.split(',').collect();
        let before = symbols.len();
        symbols.retain(|s| !excluded.contains(s.as_str()));
        info!(
            excluded = before - symbols.len(),
            "oracle: applied --exclude filter"
        );
    }
    info!(n_symbols = symbols.len(), "oracle: universe loaded");

    // 2. Preload all symbols' bars for the eval period
    let t_load = Instant::now();
    let bars_by_symbol: HashMap<String, Vec<Bar>> = symbols
        .par_iter()
        .map(|sym| {
            let bars = load_symbol_bars(&cli.bars_dir.join(format!("{sym}.parquet")), start_us, end_us);
            (sym.clone(), bars)
        })
        .collect();
    info!(
        elapsed_sec = t_load.elapsed().as_secs_f32(),
        "oracle: bars loaded into memory"
    );

    // 3. Generate all unordered pairs (A, B) with A < B
    let mut pair_list: Vec<(String, String)> = Vec::new();
    for i in 0..symbols.len() {
        for j in (i + 1)..symbols.len() {
            pair_list.push((symbols[i].clone(), symbols[j].clone()));
        }
    }
    if cli.max_pairs > 0 && pair_list.len() > cli.max_pairs {
        pair_list.truncate(cli.max_pairs);
    }
    info!(n_pairs = pair_list.len(), "oracle: pairs enumerated");

    // 4. Shared trading config
    let trading = PairsTradingConfig {
        entry_z: cli.entry_z,
        exit_z: cli.exit_z,
        stop_z: 100.0,   // disable stop-loss — oracle spec has no stop
        lookback: cli.intraday_bars,
        max_hold_bars: 0, // no max-hold timer
        min_hold_bars: 1, // minimum 1 bar as per spec
        notional_per_leg: cli.notional_per_leg,
        last_entry_hour: 23,
        force_close_minute: 15 * 60 + 58, // 15:58 ET — EOD exit as per spec
        cost_bps: cli.cost_bps,
        tz_offset_hours: -5,
        max_concurrent_pairs: 0, // unlimited (oracle evaluates pairs independently)
        max_drift_z: 0.0,
        spread_trend_gate: 0,
        intraday_entries: false,
        intraday_confirm_bars: 0,
        intraday_entry_z: 0.0,
        max_daily_entries: 0, // unlimited
        intraday_rolling_bars: cli.intraday_bars,
    };

    // 5. Evaluate each pair in parallel
    let t_eval = Instant::now();
    let verdicts: Vec<PairVerdict> = pair_list
        .par_iter()
        .map(|(a, b)| {
            let bars_a = bars_by_symbol.get(a).unwrap();
            let bars_b = bars_by_symbol.get(b).unwrap();
            evaluate_pair(a, b, bars_a, bars_b, &trading)
        })
        .collect();
    info!(
        elapsed_sec = t_eval.elapsed().as_secs_f32(),
        "oracle: evaluation complete"
    );

    // 6. Summary
    let mut c_winner = 0usize;
    let mut c_average = 0usize;
    let mut c_loser = 0usize;
    let mut c_inactive = 0usize;
    for v in &verdicts {
        match v.classification.as_str() {
            "WINNER" => c_winner += 1,
            "AVERAGE" => c_average += 1,
            "LOSER" => c_loser += 1,
            _ => c_inactive += 1,
        }
    }
    info!(
        winner = c_winner,
        average = c_average,
        loser = c_loser,
        inactive = c_inactive,
        "oracle: classification summary"
    );

    // 7. Write verdicts parquet + manifest
    fs::create_dir_all(&cli.output).expect("create output dir");
    let verdicts_path = cli.output.join("verdicts.parquet");
    write_verdicts_parquet(&verdicts, &verdicts_path);
    info!(path = %verdicts_path.display(), "oracle: verdicts written");

    let manifest = serde_json::json!({
        "version": "v1_bars/v1_strategy_z30_entry2_exit0_cost5bps",
        "built_at": chrono::Utc::now().to_rfc3339(),
        "bars_dir": cli.bars_dir.display().to_string(),
        "eval_period": {
            "start": cli.start,
            "end": cli.end,
        },
        "strategy": {
            "mode": "intraday_rolling",
            "intraday_bars": cli.intraday_bars,
            "entry_z": cli.entry_z,
            "exit_z": cli.exit_z,
            "stop_z_disabled": true,
            "notional_per_leg": cli.notional_per_leg,
            "cost_bps_round_trip": cli.cost_bps,
            "max_entries_per_pair_per_day": 1,
            "eod_exit_minute": 15 * 60 + 58,
            "session": "regular (13:30-20:00 UTC)",
        },
        "classification": {
            "min_trades": CLASSIFY_MIN_TRADES,
            "winner_total_bps": CLASSIFY_WINNER_BPS,
            "winner_profit_months_pct": CLASSIFY_WINNER_PROFIT_MONTHS_PCT,
        },
        "n_symbols": symbols.len(),
        "n_pairs": pair_list.len(),
        "n_winner": c_winner,
        "n_average": c_average,
        "n_loser": c_loser,
        "n_inactive": c_inactive,
        "notes": [
            "Immutable: never mutate. If strategy or thresholds change, bump version.",
            "Strategy runs via openquant_core::pairs::PairState::on_price — no parallel implementation.",
        ],
    });
    let manifest_path = cli.output.join("MANIFEST.json");
    fs::write(&manifest_path, serde_json::to_string_pretty(&manifest).unwrap())
        .expect("write manifest");
    info!(path = %manifest_path.display(), "oracle: manifest written");
}

// ── Symbol discovery ──

fn discover_symbols(bars_dir: &Path, filter: Option<&str>) -> Vec<String> {
    let mut symbols: Vec<String> = fs::read_dir(bars_dir)
        .expect("read bars dir")
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|s| s.to_str()) == Some("parquet") {
                path.file_stem().and_then(|s| s.to_str()).map(|s| s.to_string())
            } else {
                None
            }
        })
        .collect();
    symbols.sort();

    if let Some(f) = filter {
        let wanted: std::collections::HashSet<&str> = f.split(',').collect();
        symbols.retain(|s| wanted.contains(s.as_str()));
    }

    symbols
}

// ── Parquet loading ──

fn load_symbol_bars(path: &Path, start_us: i64, end_us: i64) -> Vec<Bar> {
    let file = fs::File::open(path).unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .unwrap_or_else(|e| panic!("parquet reader {}: {e}", path.display()));
    let reader = builder.build().unwrap();

    let mut bars: Vec<Bar> = Vec::new();
    for batch_res in reader {
        let batch: RecordBatch = batch_res.unwrap();
        let ts = batch
            .column_by_name("timestamp")
            .expect("timestamp column")
            .as_any()
            .downcast_ref::<TimestampMicrosecondArray>()
            .expect("timestamp[us]");
        let close = batch
            .column_by_name("close")
            .expect("close column")
            .as_any()
            .downcast_ref::<Float64Array>()
            .expect("close f64");

        for i in 0..batch.num_rows() {
            let t = ts.value(i);
            if t < start_us || t > end_us {
                continue;
            }
            // Regular session filter: 13:30 - 20:00 UTC
            let secs_of_day = ((t / 1_000_000) % 86_400 + 86_400) % 86_400;
            let mins_of_day = secs_of_day / 60;
            if !(13 * 60 + 30..20 * 60).contains(&mins_of_day) {
                continue;
            }
            bars.push(Bar {
                ts_us: t,
                close: close.value(i),
            });
        }
    }
    bars.sort_by_key(|b| b.ts_us);
    bars
}

// ── Per-pair evaluation ──

fn evaluate_pair(
    leg_a: &str,
    leg_b: &str,
    bars_a: &[Bar],
    bars_b: &[Bar],
    trading: &PairsTradingConfig,
) -> PairVerdict {
    // Inner-join on timestamp
    let joined = inner_join_bars(bars_a, bars_b);

    if joined.len() < trading.intraday_rolling_bars + 10 {
        return empty_verdict(leg_a, leg_b);
    }

    let config = PairConfig {
        leg_a: leg_a.to_string(),
        leg_b: leg_b.to_string(),
        alpha: 0.0,
        beta: 1.0, // log-spread = ln(A) - ln(B); oracle uses raw log-spread
        kappa: 0.0,
        max_hold_bars: 0,
        lookback_bars: 0,
    };

    let mut state = PairState::for_pair(&config, trading);
    let mut closed: Vec<ClosedTrade> = Vec::new();
    let mut open: Option<OpenTrade> = None;

    for bar in &joined {
        // Feed both legs
        let mut intents = state.on_price(leg_a, bar.close_a, &config, trading, bar.ts_us / 1000);
        intents.extend(state.on_price(leg_b, bar.close_b, &config, trading, bar.ts_us / 1000));

        if intents.is_empty() {
            continue;
        }

        // Detect entry / exit via state.position() transitions
        let pos = state.position();

        match (&open, pos) {
            (None, PairPosition::LongSpread) | (None, PairPosition::ShortSpread) => {
                // Entry: record from intents (they carry qty)
                let direction = if pos == PairPosition::LongSpread {
                    TradeDir::Long
                } else {
                    TradeDir::Short
                };
                let qty_a = intents
                    .iter()
                    .find(|i| i.symbol == leg_a)
                    .map(|i| i.qty)
                    .unwrap_or(0.0);
                let qty_b = intents
                    .iter()
                    .find(|i| i.symbol == leg_b)
                    .map(|i| i.qty)
                    .unwrap_or(0.0);
                open = Some(OpenTrade {
                    entry_ts_us: bar.ts_us,
                    entry_price_a: bar.close_a,
                    entry_price_b: bar.close_b,
                    qty_a,
                    qty_b,
                    direction,
                });
            }
            (Some(ot), PairPosition::Flat) => {
                // Exit
                let pnl_leg_a = match ot.direction {
                    TradeDir::Long => (bar.close_a - ot.entry_price_a) * ot.qty_a,
                    TradeDir::Short => (ot.entry_price_a - bar.close_a) * ot.qty_a,
                };
                let pnl_leg_b = match ot.direction {
                    TradeDir::Long => (ot.entry_price_b - bar.close_b) * ot.qty_b,
                    TradeDir::Short => (bar.close_b - ot.entry_price_b) * ot.qty_b,
                };
                let gross_pnl = pnl_leg_a + pnl_leg_b;
                let notional = trading.notional_per_leg * 2.0;
                let gross_bps = if notional > 0.0 {
                    (gross_pnl / notional) * 10_000.0
                } else {
                    0.0
                };
                let net_bps = gross_bps - trading.cost_bps;
                let hold_minutes = (bar.ts_us - ot.entry_ts_us) / 60_000_000;
                let entry_secs = ot.entry_ts_us / 1_000_000;
                let entry_dt = Utc.timestamp_opt(entry_secs, 0).single().unwrap();
                let month = format!("{:04}-{:02}", entry_dt.year(), entry_dt.month());
                closed.push(ClosedTrade {
                    net_bps,
                    hold_minutes,
                    month,
                });
                open = None;
            }
            _ => {
                // No transition (same position or entry-then-exit same bar)
            }
        }
    }

    build_verdict(leg_a, leg_b, &closed)
}

fn inner_join_bars(a: &[Bar], b: &[Bar]) -> Vec<JoinedBar> {
    let mut out = Vec::with_capacity(a.len().min(b.len()));
    let mut j = 0usize;
    for ba in a {
        while j < b.len() && b[j].ts_us < ba.ts_us {
            j += 1;
        }
        if j < b.len() && b[j].ts_us == ba.ts_us {
            out.push(JoinedBar {
                ts_us: ba.ts_us,
                close_a: ba.close,
                close_b: b[j].close,
            });
        }
    }
    out
}

fn empty_verdict(leg_a: &str, leg_b: &str) -> PairVerdict {
    PairVerdict {
        leg_a: leg_a.to_string(),
        leg_b: leg_b.to_string(),
        total_trades: 0,
        winning_trades: 0,
        win_pct: 0.0,
        total_net_bps: 0,
        avg_net_bps: 0.0,
        median_hold_minutes: 0.0,
        max_hold_minutes: 0,
        monthly_bps_json: "{}".to_string(),
        classification: "INACTIVE".to_string(),
    }
}

fn build_verdict(leg_a: &str, leg_b: &str, trades: &[ClosedTrade]) -> PairVerdict {
    if trades.is_empty() {
        return empty_verdict(leg_a, leg_b);
    }
    let n = trades.len() as i32;
    let wins = trades.iter().filter(|t| t.net_bps > 0.0).count() as i32;
    let total_bps: f64 = trades.iter().map(|t| t.net_bps).sum();
    let avg_bps = total_bps / trades.len() as f64;

    let mut holds: Vec<i64> = trades.iter().map(|t| t.hold_minutes).collect();
    holds.sort();
    let median_hold = holds[holds.len() / 2] as f64;
    let max_hold = *holds.last().unwrap_or(&0);

    // Monthly breakdown
    let mut monthly: HashMap<String, i64> = HashMap::new();
    for t in trades {
        *monthly.entry(t.month.clone()).or_insert(0) += t.net_bps.round() as i64;
    }
    let monthly_bps_json = serde_json::to_string(&monthly).unwrap_or_else(|_| "{}".into());

    // Classification
    let classification = classify(n, total_bps.round() as i64, &monthly);

    PairVerdict {
        leg_a: leg_a.to_string(),
        leg_b: leg_b.to_string(),
        total_trades: n,
        winning_trades: wins,
        win_pct: wins as f64 / n as f64,
        total_net_bps: total_bps.round() as i64,
        avg_net_bps: avg_bps,
        median_hold_minutes: median_hold,
        max_hold_minutes: max_hold,
        monthly_bps_json,
        classification,
    }
}

fn classify(n_trades: i32, total_bps: i64, monthly: &HashMap<String, i64>) -> String {
    if n_trades < CLASSIFY_MIN_TRADES {
        return "INACTIVE".to_string();
    }
    if total_bps <= 0 {
        return "LOSER".to_string();
    }
    let n_months = monthly.len() as f64;
    let profit_months = monthly.values().filter(|&&v| v > 0).count() as f64;
    let profit_pct = if n_months > 0.0 { profit_months / n_months } else { 0.0 };

    if total_bps >= CLASSIFY_WINNER_BPS && profit_pct >= CLASSIFY_WINNER_PROFIT_MONTHS_PCT {
        "WINNER".to_string()
    } else {
        "AVERAGE".to_string()
    }
}

// ── Parquet writer ──

fn write_verdicts_parquet(verdicts: &[PairVerdict], path: &Path) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("leg_a", DataType::Utf8, false),
        Field::new("leg_b", DataType::Utf8, false),
        Field::new("total_trades", DataType::Int32, false),
        Field::new("winning_trades", DataType::Int32, false),
        Field::new("win_pct", DataType::Float64, false),
        Field::new("total_net_bps", DataType::Int64, false),
        Field::new("avg_net_bps", DataType::Float64, false),
        Field::new("median_hold_minutes", DataType::Float64, false),
        Field::new("max_hold_minutes", DataType::Int64, false),
        Field::new("monthly_bps_json", DataType::Utf8, false),
        Field::new("classification", DataType::Utf8, false),
    ]));

    let leg_a = StringArray::from_iter_values(verdicts.iter().map(|v| v.leg_a.as_str()));
    let leg_b = StringArray::from_iter_values(verdicts.iter().map(|v| v.leg_b.as_str()));
    let total_trades = Int32Array::from_iter_values(verdicts.iter().map(|v| v.total_trades));
    let winning_trades = Int32Array::from_iter_values(verdicts.iter().map(|v| v.winning_trades));
    let win_pct = Float64Array::from_iter_values(verdicts.iter().map(|v| v.win_pct));
    let total_bps = Int64Array::from_iter_values(verdicts.iter().map(|v| v.total_net_bps));
    let avg_bps = Float64Array::from_iter_values(verdicts.iter().map(|v| v.avg_net_bps));
    let median_hold =
        Float64Array::from_iter_values(verdicts.iter().map(|v| v.median_hold_minutes));
    let max_hold = Int64Array::from_iter_values(verdicts.iter().map(|v| v.max_hold_minutes));
    let monthly =
        StringArray::from_iter_values(verdicts.iter().map(|v| v.monthly_bps_json.as_str()));
    let clazz = StringArray::from_iter_values(verdicts.iter().map(|v| v.classification.as_str()));

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(leg_a),
            Arc::new(leg_b),
            Arc::new(total_trades),
            Arc::new(winning_trades),
            Arc::new(win_pct),
            Arc::new(total_bps),
            Arc::new(avg_bps),
            Arc::new(median_hold),
            Arc::new(max_hold),
            Arc::new(monthly),
            Arc::new(clazz),
        ],
    )
    .expect("build RecordBatch");

    let file = fs::File::create(path).expect("create verdicts.parquet");
    let props = WriterProperties::builder()
        .set_compression(Compression::SNAPPY)
        .build();
    let mut writer = ArrowWriter::try_new(file, schema, Some(props)).expect("arrow writer");
    writer.write(&batch).expect("write batch");
    writer.close().expect("close writer");
}

