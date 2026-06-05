use std::str::FromStr;
use std::sync::OnceLock;

use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use nyse_holiday_cal::HolidayCal;

#[derive(Debug, Clone, Copy)]
enum MarketCalendar {
    Nyse,
    WeekdaysOnly,
}

#[derive(Debug, Clone)]
struct MarketSessionConfig {
    tz: Tz,
    open: NaiveTime,
    close: NaiveTime,
    calendar: MarketCalendar,
}

static MARKET_SESSION: OnceLock<MarketSessionConfig> = OnceLock::new();

fn session_config() -> &'static MarketSessionConfig {
    MARKET_SESSION.get_or_init(|| {
        let profile = std::env::var("OPENQUANT_MARKET_PROFILE").unwrap_or_else(|_| "us".into());
        let profile = profile.to_ascii_lowercase();
        let (default_tz, default_open, default_close, default_calendar) = match profile.as_str() {
            "india" | "nse" => ("Asia/Kolkata", "09:15", "15:30", "weekdays"),
            _ => ("America/New_York", "09:30", "16:00", "nyse"),
        };

        let tz_name = std::env::var("OPENQUANT_MARKET_TZ").unwrap_or_else(|_| default_tz.into());
        let open = std::env::var("OPENQUANT_MARKET_OPEN").unwrap_or_else(|_| default_open.into());
        let close =
            std::env::var("OPENQUANT_MARKET_CLOSE").unwrap_or_else(|_| default_close.into());
        let calendar =
            std::env::var("OPENQUANT_MARKET_CALENDAR").unwrap_or_else(|_| default_calendar.into());

        MarketSessionConfig {
            tz: Tz::from_str(&tz_name).unwrap_or(chrono_tz::UTC),
            open: parse_time(&open, 9, 30),
            close: parse_time(&close, 16, 0),
            calendar: match calendar.to_ascii_lowercase().as_str() {
                "weekdays" | "weekday" | "weekday_only" => MarketCalendar::WeekdaysOnly,
                _ => MarketCalendar::Nyse,
            },
        }
    })
}

fn parse_time(raw: &str, fallback_hour: u32, fallback_minute: u32) -> NaiveTime {
    NaiveTime::parse_from_str(raw, "%H:%M").unwrap_or_else(|_| {
        NaiveTime::from_hms_opt(fallback_hour, fallback_minute, 0).expect("valid fallback time")
    })
}

fn session_minute(local: DateTime<Tz>) -> u32 {
    local.time().hour() * 60 + local.time().minute()
}

pub fn trading_day_utc(dt_utc: DateTime<Utc>) -> NaiveDate {
    dt_utc.with_timezone(&session_config().tz).date_naive()
}

pub fn is_rth_utc(dt_utc: DateTime<Utc>) -> bool {
    let cfg = session_config();
    let local = dt_utc.with_timezone(&cfg.tz);
    let minute = session_minute(local);
    let open = cfg.open.hour() * 60 + cfg.open.minute();
    let close = cfg.close.hour() * 60 + cfg.close.minute();
    (open..close).contains(&minute)
}

pub fn is_after_close_grace_utc(dt_utc: DateTime<Utc>, grace_min: u32) -> bool {
    let cfg = session_config();
    let local = dt_utc.with_timezone(&cfg.tz);
    session_minute(local) >= (cfg.close.hour() * 60 + cfg.close.minute() + grace_min)
}

pub fn is_at_or_after_close_decision_utc(dt_utc: DateTime<Utc>, minutes_before_close: u32) -> bool {
    let cfg = session_config();
    let local = dt_utc.with_timezone(&cfg.tz);
    let minute = session_minute(local);
    let close = cfg.close.hour() * 60 + cfg.close.minute();
    let decision = close.saturating_sub(minutes_before_close);
    minute >= decision && minute < close
}

pub fn minutes_from_open_utc(dt_utc: DateTime<Utc>) -> Option<u32> {
    let cfg = session_config();
    let local = dt_utc.with_timezone(&cfg.tz);
    let minute = session_minute(local);
    let open = cfg.open.hour() * 60 + cfg.open.minute();
    let close = cfg.close.hour() * 60 + cfg.close.minute();
    if !(open..close).contains(&minute) {
        return None;
    }
    Some(minute - open)
}

pub fn is_trading_day(day: NaiveDate) -> bool {
    match session_config().calendar {
        MarketCalendar::Nyse => match day.is_busday() {
            Ok(open) => open,
            Err(_) => {
                tracing::warn!(
                    date = %day,
                    "holiday calendar out of range; falling back to weekday trading-day check"
                );
                !day.is_weekend()
            }
        },
        MarketCalendar::WeekdaysOnly => !day.is_weekend(),
    }
}

pub fn close_timestamp_utc_for_day(day: NaiveDate) -> i64 {
    let cfg = session_config();
    let local = cfg
        .tz
        .from_local_datetime(&day.and_time(cfg.close))
        .single()
        .expect("unambiguous cash close");
    local.with_timezone(&Utc).timestamp_millis()
}

pub fn open_datetime_utc_for_day(day: NaiveDate) -> DateTime<Utc> {
    let cfg = session_config();
    let local = cfg
        .tz
        .from_local_datetime(&day.and_time(cfg.open))
        .single()
        .expect("unambiguous cash open");
    local.with_timezone(&Utc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rth_detection_handles_us_dst_defaults() {
        let summer_open = Utc.with_ymd_and_hms(2026, 7, 1, 13, 30, 0).unwrap();
        let winter_open = Utc.with_ymd_and_hms(2026, 1, 15, 14, 30, 0).unwrap();
        let winter_pre_open = Utc.with_ymd_and_hms(2026, 1, 15, 14, 29, 0).unwrap();

        assert!(is_rth_utc(summer_open));
        assert!(is_rth_utc(winter_open));
        assert!(!is_rth_utc(winter_pre_open));
    }

    #[test]
    fn test_close_timestamp_handles_us_dst_defaults() {
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
    fn test_pre_close_decision_window_handles_us_dst_defaults() {
        let before_decision = Utc.with_ymd_and_hms(2026, 7, 1, 19, 44, 0).unwrap();
        let at_decision = Utc.with_ymd_and_hms(2026, 7, 1, 19, 45, 0).unwrap();
        let before_close = Utc.with_ymd_and_hms(2026, 7, 1, 19, 59, 0).unwrap();
        let at_close = Utc.with_ymd_and_hms(2026, 7, 1, 20, 0, 0).unwrap();

        assert!(!is_at_or_after_close_decision_utc(before_decision, 15));
        assert!(is_at_or_after_close_decision_utc(at_decision, 15));
        assert!(is_at_or_after_close_decision_utc(before_close, 15));
        assert!(!is_at_or_after_close_decision_utc(at_close, 15));
    }

    #[test]
    fn test_open_datetime_handles_us_dst_defaults() {
        let summer_day = NaiveDate::from_ymd_opt(2026, 7, 1).unwrap();
        let winter_day = NaiveDate::from_ymd_opt(2026, 1, 15).unwrap();

        assert_eq!(
            open_datetime_utc_for_day(summer_day).time(),
            NaiveTime::from_hms_opt(13, 30, 0).unwrap()
        );
        assert_eq!(
            open_datetime_utc_for_day(winter_day).time(),
            NaiveTime::from_hms_opt(14, 30, 0).unwrap()
        );
    }
}
