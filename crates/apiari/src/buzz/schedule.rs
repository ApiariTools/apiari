//! Schedule checking — determines whether the current local time falls within
//! a configured active-hours window.

use chrono::{Datelike, Local, NaiveDateTime, NaiveTime, Weekday};
use tracing::warn;

use crate::config::Schedule;

/// Valid lowercase day abbreviations accepted in `active_days`.
const VALID_DAYS: &[&str] = &["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/// Validate a schedule at configuration-load time and emit `warn!` for any
/// malformed fields.  Call this once per watcher/workspace at startup — not
/// in the poll hot path.
///
/// Malformed constraints are silently ignored at runtime (the constraint is
/// treated as absent) so that a misconfigured schedule does not break polling
/// entirely.  This function is the single place where the user is notified.
pub fn warn_if_invalid(schedule: &Schedule) {
    if let Some(ref hours) = schedule.active_hours
        && parse_active_hours(hours).is_none()
    {
        warn!(
            "schedule.active_hours {:?} is malformed (expected HH:MM-HH:MM); \
             hours constraint will be ignored",
            hours
        );
    }
    if let Some(ref days) = schedule.active_days {
        for day in days {
            if !VALID_DAYS.iter().any(|&v| day.eq_ignore_ascii_case(v)) {
                warn!(
                    "schedule.active_days contains unknown day {:?} \
                     (expected one of mon/tue/wed/thu/fri/sat/sun); entry will never match",
                    day
                );
            }
        }
    }
}

/// Returns `true` if the current local time is within the active window defined
/// by `schedule`.  Returns `true` (always active) when no constraints are set.
pub fn is_within_active_hours(schedule: &Schedule) -> bool {
    is_within_active_hours_at(schedule, Local::now().naive_local())
}

/// Inner implementation — accepts an injected `now` so tests can use fixed times.
fn is_within_active_hours_at(schedule: &Schedule, now: NaiveDateTime) -> bool {
    if schedule.active_hours.is_none() && schedule.active_days.is_none() {
        return true;
    }

    // Determine whether we're in the post-midnight tail of an overnight window.
    // For active_days purposes the "logical day" for such times is the *previous*
    // calendar day — e.g. 01:00 Saturday during a "22:00-06:00 fri" window should
    // still be considered Friday.
    let is_overnight_tail = if let Some(ref hours) = schedule.active_hours
        && let Some((start, end)) = parse_active_hours(hours)
        && start > end
    {
        now.time() < end
    } else {
        false
    };

    // Check active days.  Day strings are compared case-insensitively without
    // allocation; malformed entries silently never match.
    if let Some(ref days) = schedule.active_days {
        let logical_day = if is_overnight_tail {
            now.weekday().pred()
        } else {
            now.weekday()
        };
        let day_str = weekday_str(logical_day);
        if !days.iter().any(|d| d.eq_ignore_ascii_case(day_str)) {
            return false;
        }
    }

    // Check active hours window.  Malformed strings are silently ignored (treated as
    // "no hours constraint") — `warn_if_invalid` is called at startup to surface them.
    if let Some(ref hours) = schedule.active_hours
        && let Some((start, end)) = parse_active_hours(hours)
    {
        let current = now.time();
        // When start == end (e.g. "00:00-00:00"), start <= end is true and the window
        // `current >= start && current < end` is empty — the schedule is never active.
        // This is intentional: an equal-endpoint range means "always inactive".
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

pub(crate) fn parse_active_hours(s: &str) -> Option<(NaiveTime, NaiveTime)> {
    let (start_str, end_str) = s.split_once('-')?;
    let start = NaiveTime::parse_from_str(start_str.trim(), "%H:%M").ok()?;
    let end = NaiveTime::parse_from_str(end_str.trim(), "%H:%M").ok()?;
    Some((start, end))
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::*;

    fn schedule(hours: Option<&str>, days: Option<Vec<&str>>) -> Schedule {
        Schedule {
            active_hours: hours.map(str::to_string),
            active_days: days.map(|v| v.into_iter().map(str::to_string).collect()),
        }
    }

    /// Build a NaiveDateTime for a known weekday + time (2024-01-08 = Monday).
    fn at(weekday_offset_from_monday: u32, h: u32, m: u32) -> NaiveDateTime {
        // 2024-01-08 is a Monday; adding days gives Tue, Wed, …, Sun.
        NaiveDate::from_ymd_opt(2024, 1, 8 + weekday_offset_from_monday)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }

    fn mon(h: u32, m: u32) -> NaiveDateTime {
        at(0, h, m)
    }
    fn wed(h: u32, m: u32) -> NaiveDateTime {
        at(2, h, m)
    }
    fn fri(h: u32, m: u32) -> NaiveDateTime {
        at(4, h, m)
    }
    fn sat(h: u32, m: u32) -> NaiveDateTime {
        at(5, h, m)
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

    // ── is_within_active_hours_at ───────────────────────────────────────────

    #[test]
    fn test_empty_schedule_always_active() {
        assert!(is_within_active_hours_at(&Schedule::default(), mon(10, 0)));
        assert!(is_within_active_hours_at(&schedule(None, None), sat(0, 0)));
    }

    #[test]
    fn test_normal_range_inside() {
        let s = schedule(Some("09:00-18:00"), None);
        assert!(is_within_active_hours_at(&s, mon(9, 0)));
        assert!(is_within_active_hours_at(&s, mon(12, 30)));
        assert!(is_within_active_hours_at(&s, mon(17, 59)));
    }

    #[test]
    fn test_normal_range_outside_before() {
        let s = schedule(Some("09:00-18:00"), None);
        assert!(!is_within_active_hours_at(&s, mon(8, 59)));
        assert!(!is_within_active_hours_at(&s, mon(0, 0)));
    }

    #[test]
    fn test_normal_range_outside_after() {
        let s = schedule(Some("09:00-18:00"), None);
        assert!(!is_within_active_hours_at(&s, mon(18, 0)));
        assert!(!is_within_active_hours_at(&s, mon(23, 0)));
    }

    #[test]
    fn test_overnight_range_active_before_midnight() {
        let s = schedule(Some("22:00-06:00"), None);
        assert!(is_within_active_hours_at(&s, mon(22, 0)));
        assert!(is_within_active_hours_at(&s, mon(23, 30)));
    }

    #[test]
    fn test_overnight_range_active_after_midnight() {
        let s = schedule(Some("22:00-06:00"), None);
        assert!(is_within_active_hours_at(&s, mon(0, 0)));
        assert!(is_within_active_hours_at(&s, mon(3, 0)));
        assert!(is_within_active_hours_at(&s, mon(5, 59)));
    }

    #[test]
    fn test_overnight_range_inactive_midday() {
        let s = schedule(Some("22:00-06:00"), None);
        assert!(!is_within_active_hours_at(&s, mon(6, 0)));
        assert!(!is_within_active_hours_at(&s, mon(12, 0)));
        assert!(!is_within_active_hours_at(&s, mon(21, 59)));
    }

    #[test]
    fn test_overnight_days_pre_midnight_uses_current_day() {
        // 22:30 Friday is within "22:00-06:00" and the logical day is Friday.
        let s = schedule(Some("22:00-06:00"), Some(vec!["fri"]));
        assert!(is_within_active_hours_at(&s, fri(22, 30)));
    }

    #[test]
    fn test_overnight_days_post_midnight_uses_previous_day() {
        // 01:00 Saturday is the post-midnight tail of a "22:00-06:00 fri" window.
        // The logical day for this time is Friday, so it should be active.
        let s = schedule(Some("22:00-06:00"), Some(vec!["fri"]));
        assert!(is_within_active_hours_at(&s, sat(1, 0)));
    }

    #[test]
    fn test_overnight_days_post_midnight_excluded_day() {
        // 01:00 Saturday is the post-midnight tail, logical day = Friday.
        // If only Saturday is configured, the window should NOT match.
        let s = schedule(Some("22:00-06:00"), Some(vec!["sat"]));
        assert!(!is_within_active_hours_at(&s, sat(1, 0)));
    }

    #[test]
    fn test_equal_start_end_is_never_active() {
        // "00:00-00:00": start == end means the window is empty — always inactive.
        let s = schedule(Some("00:00-00:00"), None);
        assert!(!is_within_active_hours_at(&s, mon(0, 0)));
        assert!(!is_within_active_hours_at(&s, mon(12, 0)));
        assert!(!is_within_active_hours_at(&s, mon(23, 59)));
    }

    #[test]
    fn test_weekday_filter_active_on_configured_days() {
        let s = schedule(Some("09:00-18:00"), Some(vec!["mon", "wed"]));
        assert!(is_within_active_hours_at(&s, mon(10, 0)));
        assert!(is_within_active_hours_at(&s, wed(10, 0)));
    }

    #[test]
    fn test_weekday_filter_inactive_on_excluded_days() {
        let s = schedule(
            Some("09:00-18:00"),
            Some(vec!["mon", "tue", "wed", "thu", "fri"]),
        );
        // Saturday is excluded
        assert!(!is_within_active_hours_at(&s, sat(10, 0)));
    }

    #[test]
    fn test_days_only_no_hours_weekday_active() {
        let s = schedule(None, Some(vec!["mon"]));
        assert!(is_within_active_hours_at(&s, mon(3, 0)));
        assert!(is_within_active_hours_at(&s, mon(23, 59)));
    }

    #[test]
    fn test_days_only_no_hours_weekend_inactive() {
        let s = schedule(None, Some(vec!["mon", "tue", "wed", "thu", "fri"]));
        assert!(!is_within_active_hours_at(&s, sat(12, 0)));
    }

    #[test]
    fn test_days_filter_rejects_empty_list() {
        let s = Schedule {
            active_hours: None,
            active_days: Some(vec![]),
        };
        assert!(!is_within_active_hours_at(&s, mon(10, 0)));
    }

    #[test]
    fn test_malformed_hours_treated_as_unrestricted() {
        // When hours is malformed, the hours constraint is silently ignored at runtime
        // (warn_if_invalid logs a warning at startup). Days constraint still applies.
        let s = schedule(Some("9am-5pm"), Some(vec!["mon"]));
        // Monday — days pass; malformed hours → hours ignored → active
        assert!(is_within_active_hours_at(&s, mon(10, 0)));
    }

    #[test]
    fn test_active_days_case_insensitive() {
        // Day names are matched case-insensitively.
        let s = schedule(None, Some(vec!["Mon", "WED", "FRI"]));
        assert!(is_within_active_hours_at(&s, mon(10, 0)));
        assert!(is_within_active_hours_at(&s, wed(10, 0)));
        assert!(!is_within_active_hours_at(&s, sat(10, 0)));
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

    // ── is_within_active_hours (public API, uses Local::now()) ─────────────

    #[test]
    fn test_is_within_active_hours_empty_schedule_always_active() {
        // An unconstrained schedule is always active regardless of Local::now().
        assert!(is_within_active_hours(&Schedule::default()));
    }

    #[test]
    fn test_is_within_active_hours_all_days_no_hours() {
        // All 7 days configured, no hours constraint — always active.
        let s = schedule(
            None,
            Some(vec!["mon", "tue", "wed", "thu", "fri", "sat", "sun"]),
        );
        assert!(is_within_active_hours(&s));
    }

    // ── warn_if_invalid ─────────────────────────────────────────────────────

    #[test]
    fn test_warn_if_invalid_valid_schedule_no_panic() {
        warn_if_invalid(&schedule(Some("09:00-18:00"), Some(vec!["mon", "fri"])));
    }

    #[test]
    fn test_warn_if_invalid_malformed_hours_no_panic() {
        // Should log a warning but not panic.
        warn_if_invalid(&schedule(Some("not-a-time"), None));
    }

    #[test]
    fn test_warn_if_invalid_unknown_day_no_panic() {
        // Should log a warning but not panic.
        warn_if_invalid(&schedule(None, Some(vec!["monday", "xyz"])));
    }
}
