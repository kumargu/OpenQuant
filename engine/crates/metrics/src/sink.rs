//! JSONL file sink — writes metric snapshots as one JSON object per line.
//!
//! File rotation: new file per UTC day (`metrics-YYYY-MM-DD.jsonl`).
//! Retention: configurable, default 7 days (caller manages cleanup).

use std::path::PathBuf;

use chrono::Utc;
use serde::Serialize;
use tokio::fs;
use tokio::io::AsyncWriteExt;

use crate::recorder::MetricsSnapshot;

/// Async JSONL file writer with daily rotation.
pub struct MetricsSink {
    dir: PathBuf,
    current_date: String,
    file: Option<fs::File>,
}

#[derive(Serialize)]
struct JsonlLine<'a> {
    ts: String,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    counters: &'a [crate::recorder::MetricEntry],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    gauges: &'a [crate::recorder::MetricEntry],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    histograms: &'a [crate::recorder::HistogramEntry],
}

impl MetricsSink {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            current_date: String::new(),
            file: None,
        }
    }

    /// Write a metrics snapshot as a single JSONL line.
    pub async fn write_snapshot(&mut self, snapshot: &MetricsSnapshot) {
        // Skip empty snapshots
        if snapshot.counters.is_empty()
            && snapshot.gauges.is_empty()
            && snapshot.histograms.is_empty()
        {
            return;
        }

        let now = Utc::now();
        let date = now.format("%Y-%m-%d").to_string();

        // Rotate file on new day
        if date != self.current_date {
            self.file = None;
            self.current_date = date.clone();
        }

        // Open file if needed
        if self.file.is_none() {
            if let Err(e) = fs::create_dir_all(&self.dir).await {
                eprintln!("[metrics] failed to create dir {:?}: {e}", self.dir);
                return;
            }
            let path = self.dir.join(format!("metrics-{date}.jsonl"));
            match fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .await
            {
                Ok(f) => self.file = Some(f),
                Err(e) => {
                    eprintln!("[metrics] failed to open {path:?}: {e}");
                    return;
                }
            }
        }

        let line = JsonlLine {
            ts: now.to_rfc3339(),
            counters: &snapshot.counters,
            gauges: &snapshot.gauges,
            histograms: &snapshot.histograms,
        };

        let mut json = match serde_json::to_string(&line) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("[metrics] serialize error: {e}");
                return;
            }
        };
        json.push('\n');

        if let Some(file) = &mut self.file
            && let Err(e) = file.write_all(json.as_bytes()).await
        {
            eprintln!("[metrics] write error: {e}");
            self.file = None; // re-open on next write
        }
    }
}
