//! Date-based lock file for once-daily execution.
//!
//! Creates a lock file `data/pair_picker_YYYYMMDD.lock` to ensure the pair picker
//! runs at most once per day. The Python runner can check for this file before
//! starting the trading engine and trigger pair_picker if missing.
//!
//! Test lock files use a separate prefix to avoid interfering with production runs.

use chrono::{NaiveDate, Utc};
use std::fs;
use std::path::{Path, PathBuf};

const LOCK_PREFIX: &str = "pair_picker_";
const LOCK_SUFFIX: &str = ".lock";
const TEST_LOCK_PREFIX: &str = "pair_picker_test_";

/// Check if the pair picker has already run today.
pub fn has_run_today(data_dir: &Path) -> bool {
    let today = Utc::now().format("%Y%m%d").to_string();
    lock_path(data_dir, &today).exists()
}

/// Check if the pair picker has run for a specific date.
pub fn has_run_for_date(data_dir: &Path, date: NaiveDate) -> bool {
    let date_str = date.format("%Y%m%d").to_string();
    lock_path(data_dir, &date_str).exists()
}

/// Create lock file for today, indicating successful completion.
pub fn create_lock(data_dir: &Path) -> std::io::Result<PathBuf> {
    let today = Utc::now().format("%Y%m%d").to_string();
    create_lock_for_date(data_dir, &today, false)
}

/// Create lock file for a specific date string.
pub fn create_lock_for_date(
    data_dir: &Path,
    date_str: &str,
    test_mode: bool,
) -> std::io::Result<PathBuf> {
    let path = if test_mode {
        test_lock_path(data_dir, date_str)
    } else {
        lock_path(data_dir, date_str)
    };
    fs::create_dir_all(data_dir)?;
    fs::write(&path, format!("completed at {}\n", Utc::now().to_rfc3339()))?;
    Ok(path)
}

/// Remove a test lock file.
pub fn remove_test_lock(data_dir: &Path, date_str: &str) -> std::io::Result<()> {
    let path = test_lock_path(data_dir, date_str);
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

/// Clean up old lock files (keep last 7 days).
pub fn cleanup_old_locks(data_dir: &Path) -> std::io::Result<usize> {
    let mut removed = 0;
    let today = Utc::now().date_naive();

    if let Ok(entries) = fs::read_dir(data_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with(LOCK_PREFIX)
                && name_str.ends_with(LOCK_SUFFIX)
                && !name_str.starts_with(TEST_LOCK_PREFIX)
            {
                // Extract date from filename
                let date_part = &name_str[LOCK_PREFIX.len()..name_str.len() - LOCK_SUFFIX.len()];
                if let Ok(lock_date) = NaiveDate::parse_from_str(date_part, "%Y%m%d") {
                    let age = today.signed_duration_since(lock_date).num_days();
                    if age > 7 {
                        fs::remove_file(entry.path())?;
                        removed += 1;
                    }
                }
            }
        }
    }

    Ok(removed)
}

fn lock_path(data_dir: &Path, date_str: &str) -> PathBuf {
    data_dir.join(format!("{LOCK_PREFIX}{date_str}{LOCK_SUFFIX}"))
}

fn test_lock_path(data_dir: &Path, date_str: &str) -> PathBuf {
    data_dir.join(format!("{TEST_LOCK_PREFIX}{date_str}{LOCK_SUFFIX}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_lock_lifecycle() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        // No lock initially
        assert!(!has_run_for_date(dir, Utc::now().date_naive()));

        // Create lock
        let path = create_lock(dir).unwrap();
        assert!(path.exists());
        assert!(has_run_today(dir));

        // Idempotent: creating again is fine
        create_lock(dir).unwrap();
        assert!(has_run_today(dir));
    }

    #[test]
    fn test_test_mode_separate() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let date = "20260321";

        // Test lock doesn't affect production check
        create_lock_for_date(dir, date, true).unwrap();
        assert!(!has_run_for_date(
            dir,
            NaiveDate::from_ymd_opt(2026, 3, 21).unwrap()
        ));

        // Production lock works
        create_lock_for_date(dir, date, false).unwrap();
        assert!(has_run_for_date(
            dir,
            NaiveDate::from_ymd_opt(2026, 3, 21).unwrap()
        ));

        // Can remove test lock independently
        remove_test_lock(dir, date).unwrap();
    }

    #[test]
    fn test_cleanup_old_locks() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        // Create locks for various dates
        create_lock_for_date(dir, "20260101", false).unwrap();
        create_lock_for_date(dir, "20260102", false).unwrap();
        // Today's lock
        let today = Utc::now().format("%Y%m%d").to_string();
        create_lock_for_date(dir, &today, false).unwrap();

        let removed = cleanup_old_locks(dir).unwrap();
        assert!(removed >= 2, "removed={removed}");
        // Today's lock should remain
        assert!(has_run_today(dir));
    }
}
