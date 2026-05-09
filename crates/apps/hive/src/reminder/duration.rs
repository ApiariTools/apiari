//! Human-friendly duration parsing.
//!
//! Supports formats like `30m`, `2h`, `1d`, `1h30m`, `2d12h`.

use chrono::Duration;

/// Parse a human-friendly duration string into a `chrono::Duration`.
///
/// Supported unit suffixes:
/// - `s` — seconds
/// - `m` — minutes
/// - `h` — hours
/// - `d` — days
///
/// Units can be combined: `1h30m`, `2d12h`, `1d2h30m`.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty duration string".into());
    }

    let mut total_secs: i64 = 0;
    let mut digits = String::new();
    let mut found_any = false;

    for ch in s.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else {
            if digits.is_empty() {
                return Err(format!("unexpected '{ch}' without a preceding number"));
            }
            let n: i64 = digits
                .parse()
                .map_err(|_| format!("number too large: {digits}"))?;
            digits.clear();

            let multiplier = match ch {
                's' => 1,
                'm' => 60,
                'h' => 3600,
                'd' => 86400,
                _ => return Err(format!("unknown unit '{ch}', expected s/m/h/d")),
            };
            total_secs += n * multiplier;
            found_any = true;
        }
    }

    if !digits.is_empty() {
        return Err(format!("trailing digits without unit: {digits}"));
    }

    if !found_any {
        return Err("no duration units found".into());
    }

    if total_secs == 0 {
        return Err("duration must be greater than zero".into());
    }

    Ok(Duration::seconds(total_secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minutes() {
        assert_eq!(parse_duration("30m").unwrap(), Duration::minutes(30));
    }

    #[test]
    fn test_parse_hours() {
        assert_eq!(parse_duration("2h").unwrap(), Duration::hours(2));
    }

    #[test]
    fn test_parse_days() {
        assert_eq!(parse_duration("1d").unwrap(), Duration::days(1));
    }

    #[test]
    fn test_parse_seconds() {
        assert_eq!(parse_duration("90s").unwrap(), Duration::seconds(90));
    }

    #[test]
    fn test_parse_compound_hm() {
        assert_eq!(parse_duration("1h30m").unwrap(), Duration::minutes(90));
    }

    #[test]
    fn test_parse_compound_dh() {
        assert_eq!(parse_duration("2d12h").unwrap(), Duration::hours(60));
    }

    #[test]
    fn test_parse_compound_dhm() {
        let expected = Duration::days(1) + Duration::hours(2) + Duration::minutes(30);
        assert_eq!(parse_duration("1d2h30m").unwrap(), expected);
    }

    #[test]
    fn test_parse_large() {
        assert_eq!(parse_duration("365d").unwrap(), Duration::days(365));
    }

    #[test]
    fn test_parse_empty_fails() {
        assert!(parse_duration("").is_err());
    }

    #[test]
    fn test_parse_whitespace_only_fails() {
        assert!(parse_duration("   ").is_err());
    }

    #[test]
    fn test_parse_no_suffix_fails() {
        let err = parse_duration("30").unwrap_err();
        assert!(err.contains("trailing digits"), "got: {err}");
    }

    #[test]
    fn test_parse_bad_suffix_fails() {
        let err = parse_duration("30x").unwrap_err();
        assert!(err.contains("unknown unit"), "got: {err}");
    }

    #[test]
    fn test_parse_zero_fails() {
        let err = parse_duration("0m").unwrap_err();
        assert!(err.contains("greater than zero"), "got: {err}");
    }

    #[test]
    fn test_parse_leading_suffix_fails() {
        assert!(parse_duration("m30").is_err());
    }

    #[test]
    fn test_parse_with_whitespace_trimmed() {
        assert_eq!(parse_duration("  30m  ").unwrap(), Duration::minutes(30));
    }
}
