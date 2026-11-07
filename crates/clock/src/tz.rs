//! Local time: US Pacific. These panels live in San Francisco, so the rule is
//! written down here rather than configured (the owner, 2026-11-07).
//!
//! Summer time runs from 02:00 PST on the second Sunday of March to 02:00 PDT
//! on the first Sunday of November.

const PST: i64 = -8 * 3600;
const PDT: i64 = -7 * 3600;

/// Days since 1970-01-01 of a civil date (Howard Hinnant's algorithm).
fn days(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil year of a count of seconds since 1970.
fn year(seconds: i64) -> i64 {
    let z = seconds.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let march_based = yoe + era * 400;
    // `doy` counts from 1 March, so January and February are the next year.
    if doy >= 306 {
        march_based + 1
    } else {
        march_based
    }
}

/// The `nth` Sunday of `month`, as days since 1970.
fn sunday(y: i64, month: u32, nth: i64) -> i64 {
    let first = days(y, month, 1);
    let weekday = (first + 4).rem_euclid(7); // 1970-01-01 was a Thursday.
    first + (-weekday).rem_euclid(7) + (nth - 1) * 7
}

/// Seconds east of UTC in San Francisco at `unix`.
pub fn offset(unix: i64) -> i64 {
    let y = year(unix + PST);
    let start = sunday(y, 3, 2) * 86_400 + 2 * 3600 - PST;
    let end = sunday(y, 11, 1) * 86_400 + 2 * 3600 - PDT;
    if (start..end).contains(&unix) {
        PDT
    } else {
        PST
    }
}

/// `unix` as seconds on a San Francisco wall.
pub fn local(unix: i64) -> i64 {
    unix + offset(unix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i64, m: u32, d: u32, hh: i64, mm: i64) -> i64 {
        days(y, m, d) * 86_400 + hh * 3600 + mm * 60
    }

    #[test]
    fn the_clocks_change_on_the_right_seconds() {
        // 8 March 2026, 02:00 PST is 10:00 UTC.
        assert_eq!(offset(at(2026, 3, 8, 9, 59)), PST);
        assert_eq!(offset(at(2026, 3, 8, 10, 0)), PDT);
        // 1 November 2026, 02:00 PDT is 09:00 UTC.
        assert_eq!(offset(at(2026, 11, 1, 8, 59)), PDT);
        assert_eq!(offset(at(2026, 11, 1, 9, 0)), PST);
        // 2028 is a leap year: 12 March and 5 November.
        assert_eq!(offset(at(2028, 3, 12, 10, 0)), PDT);
        assert_eq!(offset(at(2028, 11, 5, 8, 59)), PDT);
        assert_eq!(offset(at(2028, 11, 5, 9, 0)), PST);
    }

    #[test]
    fn new_year_is_on_the_right_side_of_the_year() {
        assert_eq!(offset(at(2027, 1, 1, 4, 0)), PST);
        assert_eq!(offset(at(2026, 12, 31, 12, 0)), PST);
        assert_eq!(year(at(2026, 1, 1, 0, 0)), 2026);
        assert_eq!(year(at(2026, 2, 28, 0, 0)), 2026);
        assert_eq!(year(at(2026, 12, 31, 23, 59)), 2026);
    }
}
