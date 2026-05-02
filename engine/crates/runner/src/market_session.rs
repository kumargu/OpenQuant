use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone, Timelike, Utc};
use chrono_tz::America::New_York;
use nyse_holiday_cal::HolidayCal;

const RTH_OPEN: NaiveTime = NaiveTime::from_hms_opt(9, 30, 0).expect("valid open");
const RTH_CLOSE: NaiveTime = NaiveTime::from_hms_opt(16, 0, 0).expect("valid close");

fn session_minute(local: DateTime<chrono_tz::Tz>) -> u32 {
    local.time().hour() * 60 + local.time().minute()
}

pub fn trading_day_utc(dt_utc: DateTime<Utc>) -> NaiveDate {
    dt_utc.with_timezone(&New_York).date_naive()
}

pub fn is_rth_utc(dt_utc: DateTime<Utc>) -> bool {
    let local = dt_utc.with_timezone(&New_York);
    let minute = session_minute(local);
    let open = RTH_OPEN.hour() * 60 + RTH_OPEN.minute();
    let close = RTH_CLOSE.hour() * 60 + RTH_CLOSE.minute();
    (open..close).contains(&minute)
}

pub fn is_after_close_grace_utc(dt_utc: DateTime<Utc>, grace_min: u32) -> bool {
    let local = dt_utc.with_timezone(&New_York);
    session_minute(local) >= (RTH_CLOSE.hour() * 60 + RTH_CLOSE.minute() + grace_min)
}

pub fn is_trading_day(day: NaiveDate) -> bool {
    match day.is_busday() {
        Ok(open) => open,
        Err(_) => {
            tracing::warn!(
                date = %day,
                "NYSE holiday calendar out of range; falling back to weekday trading-day check"
            );
            !day.is_weekend()
        }
    }
}

/// True when `dt_utc`'s NY-local minute-of-day is at or before
/// `RTH_CLOSE − offset_min`. Used by the basket replay path to enforce
/// `[runner].decision_offset_minutes_before_close`: bars opening past
/// the cutoff are dropped so the engine, the simulated broker, and
/// the walk-forward fit all see the same per-day snapshot.
///
/// `offset_min = 0` is byte-identical to "any RTH bar" — every RTH bar
/// has open minute < `RTH_CLOSE` (= 960), so `<= 960 − 0` accepts all.
/// `offset_min = 15` accepts bars opening at or before 15:45 ET.
///
/// Caller should still gate on `is_rth_utc(dt_utc)` first; this helper
/// only enforces the cutoff. NY-local conversion handles DST.
pub fn is_open_at_or_before_cutoff_utc(dt_utc: DateTime<Utc>, offset_min: u32) -> bool {
    let local = dt_utc.with_timezone(&New_York);
    let minute = session_minute(local);
    let close_min = RTH_CLOSE.hour() * 60 + RTH_CLOSE.minute();
    let cutoff = close_min.saturating_sub(offset_min);
    minute <= cutoff
}

pub fn close_timestamp_utc_for_day(day: NaiveDate) -> i64 {
    let local = New_York
        .from_local_datetime(&day.and_time(RTH_CLOSE))
        .single()
        .expect("unambiguous cash close");
    local.with_timezone(&Utc).timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rth_detection_handles_dst() {
        let summer_open = Utc.with_ymd_and_hms(2026, 7, 1, 13, 30, 0).unwrap();
        let winter_open = Utc.with_ymd_and_hms(2026, 1, 15, 14, 30, 0).unwrap();
        let winter_pre_open = Utc.with_ymd_and_hms(2026, 1, 15, 14, 29, 0).unwrap();

        assert!(is_rth_utc(summer_open));
        assert!(is_rth_utc(winter_open));
        assert!(!is_rth_utc(winter_pre_open));
    }

    #[test]
    fn test_close_timestamp_handles_dst() {
        let summer_day = NaiveDate::from_ymd_opt(2026, 7, 1).unwrap();
        let winter_day = NaiveDate::from_ymd_opt(2026, 1, 15).unwrap();

        assert_eq!(
            DateTime::<Utc>::from_timestamp_millis(close_timestamp_utc_for_day(summer_day))
                .unwrap()
                .time(),
            NaiveTime::from_hms_opt(20, 0, 0).unwrap()
        );
        assert_eq!(
            DateTime::<Utc>::from_timestamp_millis(close_timestamp_utc_for_day(winter_day))
                .unwrap()
                .time(),
            NaiveTime::from_hms_opt(21, 0, 0).unwrap()
        );
    }

    #[test]
    fn test_cutoff_default_offset_zero_accepts_every_rth_bar() {
        // Open minute 9:30 (first), 12:00 (mid), 15:59 (last RTH bar).
        let summer_open = Utc.with_ymd_and_hms(2025, 7, 15, 13, 30, 0).unwrap();
        let summer_mid = Utc.with_ymd_and_hms(2025, 7, 15, 16, 0, 0).unwrap();
        let summer_last = Utc.with_ymd_and_hms(2025, 7, 15, 19, 59, 0).unwrap();
        for dt in [summer_open, summer_mid, summer_last] {
            assert!(
                is_open_at_or_before_cutoff_utc(dt, 0),
                "offset=0 must accept every RTH bar, including {dt}"
            );
        }
    }

    #[test]
    fn test_cutoff_t15m_keeps_through_15_45_drops_after() {
        // 15:45 ET = 19:45 UTC in summer (EDT). 15:46 = 19:46.
        let at_cutoff = Utc.with_ymd_and_hms(2025, 7, 15, 19, 45, 0).unwrap();
        let one_after = Utc.with_ymd_and_hms(2025, 7, 15, 19, 46, 0).unwrap();
        let last_rth = Utc.with_ymd_and_hms(2025, 7, 15, 19, 59, 0).unwrap();
        assert!(is_open_at_or_before_cutoff_utc(at_cutoff, 15));
        assert!(!is_open_at_or_before_cutoff_utc(one_after, 15));
        assert!(!is_open_at_or_before_cutoff_utc(last_rth, 15));
    }

    #[test]
    fn test_cutoff_handles_dst_winter() {
        // Winter ET: 15:45 ET = 20:45 UTC.
        let at_cutoff = Utc.with_ymd_and_hms(2026, 1, 15, 20, 45, 0).unwrap();
        let one_after = Utc.with_ymd_and_hms(2026, 1, 15, 20, 46, 0).unwrap();
        assert!(is_open_at_or_before_cutoff_utc(at_cutoff, 15));
        assert!(!is_open_at_or_before_cutoff_utc(one_after, 15));
    }

    #[test]
    fn test_is_trading_day_handles_holiday_and_weekend() {
        let holiday = NaiveDate::from_ymd_opt(2026, 12, 25).unwrap();
        let weekday = NaiveDate::from_ymd_opt(2026, 12, 24).unwrap();
        let weekend = NaiveDate::from_ymd_opt(2026, 12, 26).unwrap();

        assert!(!is_trading_day(holiday));
        assert!(is_trading_day(weekday));
        assert!(!is_trading_day(weekend));
    }
}
