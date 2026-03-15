//! Dual-runtime architecture: hot path (sync) + data path (Tokio).
//!
//! ```text
//!  ┌──────────────────────┐     channel      ┌──────────────────────┐
//!  │   Trading Runtime    │ ──────────────>   │    Data Runtime      │
//!  │   (synchronous)      │   BarRecord,      │    (Tokio async)     │
//!  │                      │   FillRecord      │                      │
//!  │  - on_bar()          │                   │  - SQLite writes     │
//!  │  - feature compute   │                   │  - benchmark runs    │
//!  │  - signal eval       │                   │  - analytics queries │
//!  │  - risk gates        │                   │  - dashboard serving │
//!  │                      │                   │                      │
//!  │  Zero-alloc hot path │                   │  Can tolerate I/O    │
//!  └──────────────────────┘                   └──────────────────────┘
//! ```
//!
//! The trading hot path remains synchronous and deterministic.
//! The data runtime handles all I/O-bound work asynchronously.

use std::path::Path;

use crate::writer::{self, JournalHandle};

/// The data runtime — owns a Tokio runtime and the journal writer.
pub struct DataRuntime {
    runtime: tokio::runtime::Runtime,
    handle: Option<JournalHandle>,
    writer_task: Option<tokio::task::JoinHandle<()>>,
}

impl DataRuntime {
    /// Create and start the data runtime with a journal writer.
    pub fn new(journal_path: &Path, channel_buffer: usize) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("oq-data")
            .enable_all()
            .build()
            .expect("failed to create data runtime");

        let (handle, writer_task) =
            runtime.block_on(async { writer::start(journal_path, channel_buffer) });

        Self {
            runtime,
            handle: Some(handle),
            writer_task: Some(writer_task),
        }
    }

    /// Access the underlying Tokio runtime (for installing metrics, etc.).
    pub fn runtime(&self) -> &tokio::runtime::Runtime {
        &self.runtime
    }

    /// Get a clone of the journal handle for sending records.
    pub fn journal(&self) -> JournalHandle {
        self.handle.as_ref().expect("runtime not started").clone()
    }

    /// Shut down the data runtime gracefully, waiting for all writes to flush.
    pub fn shutdown(mut self) {
        if let Some(handle) = self.handle.take() {
            let writer_task = self.writer_task.take();
            self.runtime.block_on(async {
                handle.shutdown().await;
                // Wait for writer to finish flushing all queued records
                if let Some(task) = writer_task {
                    let _ = task.await;
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer::BarRecord;
    use openquant_core::features::FeatureValues;
    use rusqlite::Connection;

    #[test]
    fn data_runtime_starts_and_stops() {
        let tmp = std::env::temp_dir().join("test_runtime.db");
        let _ = std::fs::remove_file(&tmp);

        let rt = DataRuntime::new(&tmp, 64);
        let journal = rt.journal();

        journal.log_bar(BarRecord {
            symbol: "TEST".to_string(),
            timestamp: 1000,
            open: 100.0,
            high: 101.0,
            low: 99.0,
            close: 100.5,
            volume: 500.0,
            features: FeatureValues::default(),
            signal_fired: false,
            signal_side: None,
            signal_score: None,
            signal_reason: None,
            risk_passed: None,
            risk_rejection: None,
            qty_approved: None,
            engine_version: "test".to_string(),
        });

        rt.shutdown();

        // Verify data was persisted
        let conn = Connection::open(&tmp).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM bars", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);

        let _ = std::fs::remove_file(&tmp);
    }
}
