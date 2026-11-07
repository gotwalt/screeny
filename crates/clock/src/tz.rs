//! Local time from a POSIX `TZ` string, such as `PST8PDT,M3.2.0,M11.1.0`.
//!
//! The string carries the whole rule, summer time included, in a few dozen
//! bytes, so the device needs no zone database and never needs updating when
//! the date comes round. The settings page writes it for you from what the
//! browser knows.
//!
//! The rule forms are `Mm.w.d[/time]`, with `time` possibly negative or past 24
//! hours. POSIX's `Jn` and `n` day-of-year forms are refused: nothing in current
//! use needs them and refusing is better than being an hour out twice a year.

/// Longest `TZ` string kept, in bytes. Long enough for any zone in the database.
pub const MAX_TZ_LEN: usize = 48;

/// "The `w`th weekday `d` of month `m`", at `time` seconds after local midnight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rule {
    month: u8,
    /// 1..=5, and 5 means the last one.
    week: u8,
    /// 0 is Sunday.
    day: u8,
    time: i32,
}

/// A parsed `TZ` string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tz {
    /// Seconds east of UTC, standard time.
    std: i32,
    /// Seconds east of UTC in summer time, with the start and end rules.
    dst: Option<(i32, Rule, Rule)>,
}

impl Tz {
    pub const UTC: Tz = Tz { std: 0, dst: None };

    /// `None` for anything this cannot follow exactly.
    pub fn parse(s: &str) -> Option<Tz> {
        let mut p = Parser(s.as_bytes());
        p.name()?;
        let std = -p.hms()?;
        if p.0.is_empty() {
            return Some(Tz { std, dst: None });
        }
        p.name()?;
        let dst = if p.0.first() == Some(&b',') { std + 3600 } else { -p.hms()? };
        p.eat(b',')?;
        let start = p.rule()?;
        p.eat(b',')?;
        let end = p.rule()?;
        p.0.is_empty().then_some(Tz { std, dst: Some((dst, start, end)) })
    }

    /// Seconds east of UTC at `unix`.
    pub fn offset(&self, unix: i64) -> i32 {
        let Some((dst, start, end)) = self.dst else {
            return self.std;
        };
        let year = civil(unix + self.std as i64).0;
        // A rule's time is on the clock in force just before it.
        let from = rule_unix(year, start) - self.std as i64;
        let to = rule_unix(year, end) - dst as i64;
        let summer = if from <= to { unix >= from && unix < to } else { unix >= from || unix < to };
        if summer {
            dst
        } else {
            self.std
        }
    }

    /// `unix` as seconds on the wall.
    pub fn local(&self, unix: i64) -> i64 {
        unix + self.offset(unix) as i64
    }
}

struct Parser<'a>(&'a [u8]);

impl Parser<'_> {
    fn eat(&mut self, b: u8) -> Option<()> {
        let (first, rest) = self.0.split_first()?;
        (*first == b).then(|| self.0 = rest)
    }

    fn name(&mut self) -> Option<()> {
        if self.0.first() == Some(&b'<') {
            let end = self.0.iter().position(|b| *b == b'>')?;
            self.0 = &self.0[end + 1..];
            return Some(());
        }
        let n = self.0.iter().take_while(|b| b.is_ascii_alphabetic()).count();
        self.0 = &self.0[n..];
        (n >= 3).then_some(())
    }

    fn number(&mut self) -> Option<i32> {
        let n = self.0.iter().take_while(|b| b.is_ascii_digit()).count();
        if n == 0 || n > 3 {
            return None;
        }
        let v = self.0[..n].iter().fold(0i32, |v, b| v * 10 + (*b - b'0') as i32);
        self.0 = &self.0[n..];
        Some(v)
    }

    /// `[+-]hh[:mm[:ss]]`, in seconds.
    fn hms(&mut self) -> Option<i32> {
        let sign = match self.0.first() {
            Some(b'-') => {
                self.0 = &self.0[1..];
                -1
            }
            Some(b'+') => {
                self.0 = &self.0[1..];
                1
            }
            _ => 1,
        };
        let mut seconds = self.number()? * 3600;
        let mut unit = 60;
        while unit > 0 && self.0.first() == Some(&b':') {
            self.0 = &self.0[1..];
            seconds += self.number()? * unit;
            unit /= 60;
        }
        Some(sign * seconds)
    }

    fn rule(&mut self) -> Option<Rule> {
        self.eat(b'M')?;
        let month = self.number()?;
        self.eat(b'.')?;
        let week = self.number()?;
        self.eat(b'.')?;
        let day = self.number()?;
        if !(1..=12).contains(&month) || !(1..=5).contains(&week) || !(0..=6).contains(&day) {
            return None;
        }
        let time = if self.0.first() == Some(&b'/') {
            self.0 = &self.0[1..];
            self.hms()?
        } else {
            2 * 3600
        };
        Some(Rule { month: month as u8, week: week as u8, day: day as u8, time })
    }
}

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

/// The year, month and day of a count of seconds since 1970.
pub fn civil(seconds: i64) -> (i64, u32, u32) {
    let z = seconds.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
}

/// When a rule falls in `year`, in seconds since 1970 on its own clock.
fn rule_unix(year: i64, r: Rule) -> i64 {
    let month = r.month as u32;
    let first = days(year, month, 1);
    let weekday = (first + 4).rem_euclid(7); // 1970-01-01 was a Thursday.
    let mut day = first + (r.day as i64 - weekday).rem_euclid(7) + (r.week as i64 - 1) * 7;
    let next = if month == 12 { days(year + 1, 1, 1) } else { days(year, month + 1, 1) };
    while day >= next {
        day -= 7;
    }
    day * 86_400 + r.time as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    const LA: &str = "PST8PDT,M3.2.0,M11.1.0";

    fn at(y: i64, m: u32, d: u32, hh: i64, mm: i64) -> i64 {
        days(y, m, d) * 86_400 + hh * 3600 + mm * 60
    }

    #[test]
    fn utc_and_fixed_zones() {
        assert_eq!(Tz::parse("UTC0").unwrap().offset(0), 0);
        assert_eq!(Tz::parse("<+0530>-5:30").unwrap().offset(0), 19_800);
        assert_eq!(Tz::parse("JST-9").unwrap().local(0), 9 * 3600);
    }

    #[test]
    fn los_angeles_changes_on_the_right_seconds_2026() {
        let tz = Tz::parse(LA).unwrap();
        // 8 March 2026, 02:00 PST is 10:00 UTC.
        assert_eq!(tz.offset(at(2026, 3, 8, 9, 59)), -8 * 3600);
        assert_eq!(tz.offset(at(2026, 3, 8, 10, 0)), -7 * 3600);
        // 1 November 2026, 02:00 PDT is 09:00 UTC.
        assert_eq!(tz.offset(at(2026, 11, 1, 8, 59)), -7 * 3600);
        assert_eq!(tz.offset(at(2026, 11, 1, 9, 0)), -8 * 3600);
    }

    #[test]
    fn the_southern_hemisphere_wraps_the_year() {
        let tz = Tz::parse("AEST-10AEDT,M10.1.0,M4.1.0/3").unwrap();
        assert_eq!(tz.offset(at(2026, 1, 15, 0, 0)), 11 * 3600);
        assert_eq!(tz.offset(at(2026, 7, 15, 0, 0)), 10 * 3600);
        assert_eq!(tz.offset(at(2026, 12, 15, 0, 0)), 11 * 3600);
    }

    #[test]
    fn europe_changes_at_one_utc() {
        let tz = Tz::parse("GMT0BST,M3.5.0/1,M10.5.0").unwrap();
        // Last Sunday of March 2026 is the 29th.
        assert_eq!(tz.offset(at(2026, 3, 29, 0, 59)), 0);
        assert_eq!(tz.offset(at(2026, 3, 29, 1, 0)), 3600);
        assert_eq!(tz.offset(at(2026, 10, 25, 0, 59)), 3600);
        assert_eq!(tz.offset(at(2026, 10, 25, 1, 0)), 0);
    }

    #[test]
    fn a_rule_it_cannot_follow_is_refused() {
        for bad in ["", "P", "PST", "PST8PDT", "PST8PDT,J60,J300", "PST8PDT,M13.1.0,M1.1.0", "PST8PDT,M3.2.0"] {
            assert_eq!(Tz::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn civil_dates_round_trip() {
        for s in (-1_000_000_000i64..4_000_000_000).step_by(86_400 * 37) {
            let (y, m, d) = civil(s);
            assert_eq!(days(y, m, d), s.div_euclid(86_400));
        }
    }
}
