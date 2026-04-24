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

pub fn previous_trading_day(mut day: NaiveDate) -> NaiveDate {
    loop {
        day = day.pred_opt().expect("date before supported range");
        if is_trading_day(day) {
            return day;
        }
    }
}

pub fn latest_completed_trading_day_utc(dt_utc: DateTime<Utc>, grace_min: u32) -> NaiveDate {
    let today = trading_day_utc(dt_utc);
    if is_trading_day(today) && is_after_close_grace_utc(dt_utc, grace_min) {
        today
    } else {
        previous_trading_day(today)
    }
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
    fn test_is_trading_day_handles_holiday_and_weekend() {
        let holiday = NaiveDate::from_ymd_opt(2026, 12, 25).unwrap();
        let weekday = NaiveDate::from_ymd_opt(2026, 12, 24).unwrap();
        let weekend = NaiveDate::from_ymd_opt(2026, 12, 26).unwrap();

        assert!(!is_trading_day(holiday));
        assert!(is_trading_day(weekday));
        assert!(!is_trading_day(weekend));
    }

    #[test]
    fn test_latest_completed_trading_day_before_and_after_close() {
        let intraday = Utc.with_ymd_and_hms(2026, 12, 24, 18, 0, 0).unwrap();
        let after_close = Utc.with_ymd_and_hms(2026, 12, 24, 22, 30, 0).unwrap();

        assert_eq!(
            latest_completed_trading_day_utc(intraday, 2),
            NaiveDate::from_ymd_opt(2026, 12, 23).unwrap()
        );
        assert_eq!(
            latest_completed_trading_day_utc(after_close, 2),
            NaiveDate::from_ymd_opt(2026, 12, 24).unwrap()
        );
    }
}
