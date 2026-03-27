//! Schedule checking — determines whether the current local time falls within
//! a configured active-hours window.

use chrono::{Datelike, Local, NaiveTime, Weekday};

use crate::config::Schedule;

/// Returns `true` if the current local time is within the active window defined
/// by `schedule`.  Returns `true` (always active) when no constraints are set.
pub fn is_within_active_hours(schedule: &Schedule) -> bool {
    if schedule.active_hours.is_none() && schedule.active_days.is_none() {
        return true;
    }

    let now = Local::now();

    // Check active days first (cheap).
    if let Some(ref days) = schedule.active_days {
        let day_str = weekday_str(now.weekday());
        if !days.iter().any(|d| d.to_lowercase() == day_str) {
            return false;
        }
    }

    // Check active hours window.
    if let Some(ref hours) = schedule.active_hours
        && let Some((start, end)) = parse_active_hours(hours)
    {
        let current = now.time();
        let within = if start <= end {
            // Normal range e.g. 09:00-18:00
            current >= start && current < end
        } else {
            // Overnight range e.g. 22:00-06:00: active if >= 22:00 OR < 06:00
            current >= start || current < end
        };
        if !within {
            return false;
        }
    }

    true
}

fn weekday_str(weekday: Weekday) -> &'static str {
    match weekday {
        Weekday::Mon => "mon",
        Weekday::Tue => "tue",
        Weekday::Wed => "wed",
        Weekday::Thu => "thu",
        Weekday::Fri => "fri",
        Weekday::Sat => "sat",
        Weekday::Sun => "sun",
    }
}

fn parse_active_hours(s: &str) -> Option<(NaiveTime, NaiveTime)> {
    let (start_str, end_str) = s.split_once('-')?;
    let start = NaiveTime::parse_from_str(start_str.trim(), "%H:%M").ok()?;
    let end = NaiveTime::parse_from_str(end_str.trim(), "%H:%M").ok()?;
    Some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule(hours: Option<&str>, days: Option<Vec<&str>>) -> Schedule {
        Schedule {
            active_hours: hours.map(str::to_string),
            active_days: days.map(|v| v.into_iter().map(str::to_string).collect()),
        }
    }

    // ── parse_active_hours ──────────────────────────────────────────────────

    #[test]
    fn test_parse_normal_range() {
        let (s, e) = parse_active_hours("09:00-18:00").unwrap();
        assert_eq!(s, NaiveTime::from_hms_opt(9, 0, 0).unwrap());
        assert_eq!(e, NaiveTime::from_hms_opt(18, 0, 0).unwrap());
    }

    #[test]
    fn test_parse_overnight_range() {
        let (s, e) = parse_active_hours("22:00-06:00").unwrap();
        assert_eq!(s, NaiveTime::from_hms_opt(22, 0, 0).unwrap());
        assert_eq!(e, NaiveTime::from_hms_opt(6, 0, 0).unwrap());
    }

    #[test]
    fn test_parse_invalid_returns_none() {
        assert!(parse_active_hours("not-valid").is_none());
        assert!(parse_active_hours("25:00-26:00").is_none());
        assert!(parse_active_hours("0900-1800").is_none());
    }

    // ── is_within_active_hours ──────────────────────────────────────────────

    #[test]
    fn test_empty_schedule_always_active() {
        assert!(is_within_active_hours(&Schedule::default()));
        assert!(is_within_active_hours(&schedule(None, None)));
    }

    #[test]
    fn test_normal_range_inside() {
        // 10:30 is inside 09:00-18:00 — but we can't control Local::now() in tests.
        // So we test the logic directly via parse_active_hours + manual check.
        let (start, end) = parse_active_hours("09:00-18:00").unwrap();
        let t = NaiveTime::from_hms_opt(10, 30, 0).unwrap();
        assert!(t >= start && t < end);
    }

    #[test]
    fn test_normal_range_outside_before() {
        let (start, end) = parse_active_hours("09:00-18:00").unwrap();
        let t = NaiveTime::from_hms_opt(8, 0, 0).unwrap();
        assert!(!(t >= start && t < end));
    }

    #[test]
    fn test_normal_range_outside_after() {
        let (start, end) = parse_active_hours("09:00-18:00").unwrap();
        let t = NaiveTime::from_hms_opt(19, 0, 0).unwrap();
        assert!(!(t >= start && t < end));
    }

    #[test]
    fn test_overnight_range_active_before_midnight() {
        let (start, end) = parse_active_hours("22:00-06:00").unwrap();
        // 23:00 is active (>= 22:00)
        let t = NaiveTime::from_hms_opt(23, 0, 0).unwrap();
        assert!(t >= start || t < end);
    }

    #[test]
    fn test_overnight_range_active_after_midnight() {
        let (start, end) = parse_active_hours("22:00-06:00").unwrap();
        // 03:00 is active (< 06:00)
        let t = NaiveTime::from_hms_opt(3, 0, 0).unwrap();
        assert!(t >= start || t < end);
    }

    #[test]
    fn test_overnight_range_inactive_midday() {
        let (start, end) = parse_active_hours("22:00-06:00").unwrap();
        // 12:00 is NOT active
        let t = NaiveTime::from_hms_opt(12, 0, 0).unwrap();
        assert!(!(t >= start || t < end));
    }

    #[test]
    fn test_weekday_str_roundtrip() {
        let days = [
            (Weekday::Mon, "mon"),
            (Weekday::Tue, "tue"),
            (Weekday::Wed, "wed"),
            (Weekday::Thu, "thu"),
            (Weekday::Fri, "fri"),
            (Weekday::Sat, "sat"),
            (Weekday::Sun, "sun"),
        ];
        for (wd, expected) in days {
            assert_eq!(weekday_str(wd), expected);
        }
    }

    #[test]
    fn test_days_filter_accepts_configured_day() {
        // We can't control today's weekday, but we can verify that when all 7
        // days are listed the schedule is never blocked by the day filter.
        let all_days = schedule(
            None,
            Some(vec!["mon", "tue", "wed", "thu", "fri", "sat", "sun"]),
        );
        assert!(is_within_active_hours(&all_days));
    }

    #[test]
    fn test_days_filter_rejects_empty_list() {
        // An empty active_days list means no days are active.
        let s = Schedule {
            active_hours: None,
            active_days: Some(vec![]),
        };
        assert!(!is_within_active_hours(&s));
    }
}
