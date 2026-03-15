//! JSONL recorder — implements `metrics::Recorder` and periodically
//! flushes aggregated metrics to disk via a Tokio task.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use metrics::{
    Counter, Gauge, Histogram, Key, KeyName, Metadata, Recorder, SharedString, Unit,
};
use metrics_util::registry::{AtomicStorage, Registry};

use crate::sink::MetricsSink;

/// Global handle to the flush task so we can shut it down.
static FLUSH_HANDLE: OnceLock<Arc<FlushState>> = OnceLock::new();

struct FlushState {
    shutdown_tx: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    join_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

/// A recorder that stores metrics in an atomic registry and periodically
/// snapshots + flushes them to a JSONL file.
pub(crate) struct JsonlRecorder {
    registry: Arc<Registry<Key, AtomicStorage>>,
    descriptions: Mutex<HashMap<String, String>>,
}

impl JsonlRecorder {
    fn new() -> Self {
        Self {
            registry: Arc::new(Registry::new(AtomicStorage)),
            descriptions: Mutex::new(HashMap::new()),
        }
    }

    /// Snapshot all metrics. Counter values are read (not reset — counters are monotonic).
    /// Gauges return current value. Histograms return summary stats.
    pub(crate) fn snapshot(&self) -> MetricsSnapshot {
        let mut counters = Vec::new();
        let mut gauges = Vec::new();
        let mut histograms = Vec::new();

        // Counters
        let counter_handles = self.registry.get_counter_handles();
        for (key, counter) in counter_handles {
            let value = counter.load(Ordering::Relaxed);
            counters.push(MetricEntry {
                name: key.name().to_string(),
                labels: key_labels(&key),
                value: value as f64,
            });
        }

        // Gauges
        let gauge_handles = self.registry.get_gauge_handles();
        for (key, gauge) in gauge_handles {
            let bits = gauge.load(Ordering::Relaxed);
            let value = f64::from_bits(bits);
            gauges.push(MetricEntry {
                name: key.name().to_string(),
                labels: key_labels(&key),
                value,
            });
        }

        // Histograms — we use AtomicStorage which gives us AtomicBuckets
        // For simplicity, we record count only (full percentile tracking
        // requires a custom storage or periodic drain — future enhancement)
        let histogram_handles = self.registry.get_histogram_handles();
        for (key, hist) in histogram_handles {
            let mut count = 0u64;
            let mut sum = 0.0f64;
            let mut min = f64::MAX;
            let mut max = f64::MIN;

            hist.clear_with(|values| {
                for &v in values {
                    count += 1;
                    sum += v;
                    if v < min {
                        min = v;
                    }
                    if v > max {
                        max = v;
                    }
                }
            });

            if count > 0 {
                histograms.push(HistogramEntry {
                    name: key.name().to_string(),
                    labels: key_labels(&key),
                    count,
                    sum,
                    min,
                    max,
                    mean: sum / count as f64,
                });
            }
        }

        MetricsSnapshot {
            counters,
            gauges,
            histograms,
        }
    }
}

fn key_labels(key: &Key) -> HashMap<String, String> {
    key.labels()
        .map(|l| (l.key().to_string(), l.value().to_string()))
        .collect()
}

impl Recorder for JsonlRecorder {
    fn describe_counter(&self, key: KeyName, _unit: Option<Unit>, description: SharedString) {
        if let Ok(mut descs) = self.descriptions.lock() {
            descs.insert(key.as_str().to_string(), description.to_string());
        }
    }

    fn describe_gauge(&self, key: KeyName, _unit: Option<Unit>, description: SharedString) {
        if let Ok(mut descs) = self.descriptions.lock() {
            descs.insert(key.as_str().to_string(), description.to_string());
        }
    }

    fn describe_histogram(&self, key: KeyName, _unit: Option<Unit>, description: SharedString) {
        if let Ok(mut descs) = self.descriptions.lock() {
            descs.insert(key.as_str().to_string(), description.to_string());
        }
    }

    fn register_counter(&self, key: &Key, _metadata: &Metadata<'_>) -> Counter {
        self.registry
            .get_or_create_counter(key, |c| Counter::from_arc(c.clone()))
    }

    fn register_gauge(&self, key: &Key, _metadata: &Metadata<'_>) -> Gauge {
        self.registry
            .get_or_create_gauge(key, |g| Gauge::from_arc(g.clone()))
    }

    fn register_histogram(&self, key: &Key, _metadata: &Metadata<'_>) -> Histogram {
        self.registry
            .get_or_create_histogram(key, |h| Histogram::from_arc(h.clone()))
    }
}

// ---------------------------------------------------------------------------
// Snapshot types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
pub struct MetricEntry {
    pub name: String,
    pub labels: HashMap<String, String>,
    pub value: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct HistogramEntry {
    pub name: String,
    pub labels: HashMap<String, String>,
    pub count: u64,
    pub sum: f64,
    pub min: f64,
    pub max: f64,
    pub mean: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MetricsSnapshot {
    pub counters: Vec<MetricEntry>,
    pub gauges: Vec<MetricEntry>,
    pub histograms: Vec<HistogramEntry>,
}

// ---------------------------------------------------------------------------
// Install / shutdown
// ---------------------------------------------------------------------------

/// Install the JSONL metrics recorder globally.
///
/// Starts a Tokio task that flushes metrics to `{dir}/metrics-YYYY-MM-DD.jsonl`
/// every `flush_interval`.
///
/// Returns `Err` if:
/// - A recorder is already installed
/// - No Tokio runtime is available (must be called from an async context or with an active runtime)
/// - `flush_interval` is zero
pub fn install(dir: &str, flush_interval: Duration) -> Result<(), String> {
    if flush_interval.is_zero() {
        return Err("flush_interval must be non-zero".into());
    }

    // Verify a Tokio runtime is available before installing the global recorder.
    // tokio::spawn panics without a runtime, which would leave the recorder half-initialized.
    let handle = tokio::runtime::Handle::try_current()
        .map_err(|_| "no Tokio runtime available — install metrics from within a Tokio context")?;

    let recorder = Arc::new(JsonlRecorder::new());
    let recorder_for_flush = Arc::clone(&recorder);

    // Install as global recorder
    metrics::set_global_recorder(RecorderWrapper(Arc::clone(&recorder)))
        .map_err(|e| format!("failed to install metrics recorder: {e}"))?;

    // Start flush task on the current runtime
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let metrics_dir = PathBuf::from(dir);

    let join_handle = handle.spawn(async move {
        let mut sink = MetricsSink::new(metrics_dir);
        let mut interval = tokio::time::interval(flush_interval);
        let mut shutdown_rx = shutdown_rx;

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let snapshot = recorder_for_flush.snapshot();
                    sink.write_snapshot(&snapshot).await;
                }
                _ = &mut shutdown_rx => {
                    // Final flush
                    let snapshot = recorder_for_flush.snapshot();
                    sink.write_snapshot(&snapshot).await;
                    break;
                }
            }
        }
    });

    let state = Arc::new(FlushState {
        shutdown_tx: Mutex::new(Some(shutdown_tx)),
        join_handle: Mutex::new(Some(join_handle)),
    });
    let _ = FLUSH_HANDLE.set(state);

    Ok(())
}

/// Shut down the metrics flush task, writing one final snapshot.
pub async fn shutdown() {
    if let Some(state) = FLUSH_HANDLE.get() {
        // Send shutdown signal (drop MutexGuard before await)
        if let Ok(mut tx) = state.shutdown_tx.lock()
            && let Some(tx) = tx.take()
        {
            let _ = tx.send(());
        }
        // Extract JoinHandle before awaiting (no MutexGuard held across await)
        let jh = state
            .join_handle
            .lock()
            .ok()
            .and_then(|mut g| g.take());
        if let Some(jh) = jh {
            let _ = jh.await;
        }
    }
}

/// Create a recorder for testing (not installed globally).
#[cfg(test)]
pub(crate) fn test_recorder() -> Arc<JsonlRecorder> {
    Arc::new(JsonlRecorder::new())
}

/// Wrapper so we can pass Arc<JsonlRecorder> to set_global_recorder.
struct RecorderWrapper(Arc<JsonlRecorder>);

impl Recorder for RecorderWrapper {
    fn describe_counter(&self, key: KeyName, unit: Option<Unit>, desc: SharedString) {
        self.0.describe_counter(key, unit, desc);
    }
    fn describe_gauge(&self, key: KeyName, unit: Option<Unit>, desc: SharedString) {
        self.0.describe_gauge(key, unit, desc);
    }
    fn describe_histogram(&self, key: KeyName, unit: Option<Unit>, desc: SharedString) {
        self.0.describe_histogram(key, unit, desc);
    }
    fn register_counter(&self, key: &Key, meta: &Metadata<'_>) -> Counter {
        self.0.register_counter(key, meta)
    }
    fn register_gauge(&self, key: &Key, meta: &Metadata<'_>) -> Gauge {
        self.0.register_gauge(key, meta)
    }
    fn register_histogram(&self, key: &Key, meta: &Metadata<'_>) -> Histogram {
        self.0.register_histogram(key, meta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use metrics::{Key, Label, Metadata};

    fn meta() -> Metadata<'static> {
        Metadata::new(module_path!(), metrics::Level::INFO, None)
    }

    #[test]
    fn counter_snapshot() {
        let rec = test_recorder();
        let key = Key::from_parts(
            "engine.bars_processed",
            vec![Label::new("symbol", "BTCUSD")],
        );
        let counter = rec.register_counter(&key, &meta());
        counter.increment(42);
        counter.increment(8);

        let snap = rec.snapshot();
        assert_eq!(snap.counters.len(), 1);
        assert_eq!(snap.counters[0].name, "engine.bars_processed");
        assert_eq!(snap.counters[0].value, 50.0);
        assert_eq!(snap.counters[0].labels["symbol"], "BTCUSD");
    }

    #[test]
    fn gauge_snapshot() {
        let rec = test_recorder();
        let key = Key::from_name("journal.channel.pending");
        let gauge = rec.register_gauge(&key, &meta());
        gauge.set(17.0);

        let snap = rec.snapshot();
        assert_eq!(snap.gauges.len(), 1);
        assert_eq!(snap.gauges[0].name, "journal.channel.pending");
        assert_eq!(snap.gauges[0].value, 17.0);
    }

    #[test]
    fn histogram_snapshot() {
        let rec = test_recorder();
        let key = Key::from_name("engine.on_bar.duration_ns");
        let hist = rec.register_histogram(&key, &meta());
        hist.record(50.0);
        hist.record(100.0);
        hist.record(150.0);

        let snap = rec.snapshot();
        assert_eq!(snap.histograms.len(), 1);
        let h = &snap.histograms[0];
        assert_eq!(h.name, "engine.on_bar.duration_ns");
        assert_eq!(h.count, 3);
        assert!((h.sum - 300.0).abs() < 1e-10);
        assert!((h.min - 50.0).abs() < 1e-10);
        assert!((h.max - 150.0).abs() < 1e-10);
        assert!((h.mean - 100.0).abs() < 1e-10);
    }

    #[test]
    fn empty_snapshot() {
        let rec = test_recorder();
        let snap = rec.snapshot();
        assert!(snap.counters.is_empty());
        assert!(snap.gauges.is_empty());
        assert!(snap.histograms.is_empty());
    }

    #[test]
    fn histogram_clears_on_snapshot() {
        let rec = test_recorder();
        let key = Key::from_name("test.hist");
        let hist = rec.register_histogram(&key, &meta());
        hist.record(10.0);

        let snap1 = rec.snapshot();
        assert_eq!(snap1.histograms[0].count, 1);

        // Second snapshot should be empty (histogram drained)
        let snap2 = rec.snapshot();
        assert!(snap2.histograms.is_empty());
    }

    #[test]
    fn multiple_labels() {
        let rec = test_recorder();
        let key1 = Key::from_parts(
            "signal.fired",
            vec![Label::new("symbol", "BTCUSD"), Label::new("side", "buy")],
        );
        let key2 = Key::from_parts(
            "signal.fired",
            vec![
                Label::new("symbol", "ETHUSD"),
                Label::new("side", "sell"),
            ],
        );

        rec.register_counter(&key1, &meta()).increment(3);
        rec.register_counter(&key2, &meta()).increment(1);

        let snap = rec.snapshot();
        assert_eq!(snap.counters.len(), 2);
    }
}
