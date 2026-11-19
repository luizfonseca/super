//! Time fields, all UTC: `date` (the date and time), `weekday` (the day of the week) and `time`
//! (the time of day)

use serde_json::Value;

use super::{list, unsupported};
use crate::rules::{
    compile::{Field, Test},
    schema,
};

const WINDOWS: &[&str] = &["gte", "gt", "lt", "lte", "between"];

pub(in crate::rules) const DATE: schema::Field = schema::Field {
    name: "date",
    named: false,
    description: "The date and time now, UTC (2026-11-21, 2026-11-21T00:00)",
    operators: WINDOWS,
};
pub(in crate::rules) const WEEKDAY: schema::Field = schema::Field {
    name: "weekday",
    named: false,
    description: "The day of the week, UTC (mon, tue...)",
    operators: schema::EQUALITY,
};
pub(in crate::rules) const TIME: schema::Field = schema::Field {
    name: "time",
    named: false,
    description: "The time of day, UTC (HH:MM)",
    operators: WINDOWS,
};

/// A test of `operator` with `value`, on the time field `field`
pub(super) fn parse(field: &Field, operator: &str, value: &Value) -> Result<Test, String> {
    match field {
        Field::Date => window("date", operator, value, date, u64::MAX),
        Field::Time => window("time", operator, value, time_of_day, DAY),
        _ => {
            let days = match operator {
                "eq" | "neq" => weekday(value)?,
                "in" | "not_in" => list(value)?
                    .iter()
                    .try_fold(0, |days, day| weekday(day).map(|day| days | day))?,
                _ => return Err(unsupported("weekday", "`eq`, `neq`, `in` and `not_in`")),
            };
            let negate = matches!(operator, "neq" | "not_in");
            Ok(Test::Weekdays { days, negate })
        }
    }
}

/// What a time field reads of `now` (Unix seconds, UTC): the seconds themselves (`date`), the
/// seconds since midnight (`time`), or the day of the week, Monday being 0 (`weekday`):
/// 1970-01-01 was a Thursday
pub(super) fn read(field: &Field, now: u64) -> u64 {
    match field {
        Field::Time => now % DAY,
        Field::Weekday => (now / DAY + 3) % 7,
        _ => now,
    }
}

/// Whether `test` holds for `value` (as [`read`] gives it): a window (wrapping past midnight
/// when `until` is before `from`), or days of the week
pub(super) fn holds(test: &Test, value: u64) -> bool {
    match test {
        Test::Window { from, until } if from < until => *from <= value && value < *until,
        Test::Window { from, until } => value >= *from || value < *until,
        Test::Weekdays { days, negate } => ((u64::from(*days) >> value) & 1 == 1) != *negate,
        _ => false,
    }
}

/// Seconds in a day
pub(in crate::rules) const DAY: u64 = 86_400;

/// A value as written, in seconds: its start, and the length of what it names (a day for
/// `2026-11-21`, a minute for `02:00`, a second for `02:00:00`)
type Moment = (u64, u64);

/// The window of an operator on `field`, whose values `read` gives (`max`: the end of what's
/// read). A value covers what it names: `lte` includes all of it, `gt` starts after it. Only
/// `between` wraps past midnight: a window that would be empty is an error.
fn window(
    field: &str,
    operator: &str,
    value: &Value,
    read: fn(&Value) -> Result<Moment, String>,
    max: u64,
) -> Result<Test, String> {
    let end = |(start, length): Moment| start.saturating_add(length);
    let (from, until) = match operator {
        "gte" => (read(value)?.0, max),
        "gt" => (end(read(value)?), max),
        "lt" => (0, read(value)?.0),
        "lte" => (0, end(read(value)?)),
        "between" => return between(field, value, read),
        _ => return Err(unsupported(field, "`gte`, `gt`, `lt`, `lte` and `between`")),
    };
    if until <= from {
        return Err("never holds (nothing is in it)".to_string());
    }
    Ok(Test::Window { from, until })
}

/// `between = [from, until]`: `until` after `from`, except for times of day, which wrap
fn between(
    field: &str,
    value: &Value,
    read: fn(&Value) -> Result<Moment, String>,
) -> Result<Test, String> {
    let Some([from_value, until_value]) = value.as_array().map(Vec::as_slice) else {
        return Err("must be [from, until]".to_string());
    };
    let (from, until) = (read(from_value)?.0, read(until_value)?.0);
    let wraps = field == "time" && until < from;
    if until <= from && !wraps {
        return Err(format!("{until_value} isn't after {from_value}"));
    }
    Ok(Test::Window { from, until })
}

/// A date and time, UTC, in Unix seconds: `2026-11-21` (a day), `2026-11-21T00:00` (a minute)
/// or `2026-11-21T00:00:00` (a second), optionally ending in `Z`
fn date(value: &Value) -> Result<Moment, String> {
    let written = value
        .as_str()
        .ok_or("must be a date, such as \"2026-11-21\"")?;
    let utc = written.strip_suffix('Z').unwrap_or(written);
    let (day, clock) = match utc.split_once('T') {
        Some((day, clock)) => (day, Some(clock)),
        None => (utc, None),
    };
    if clock.is_some_and(|clock| clock.contains(['+', '-'])) {
        return Err(format!(
            "{written:?}: times are UTC (end with `Z`, or nothing)"
        ));
    }
    let invalid = || {
        format!("{written:?} isn't a date (2026-11-21, 2026-11-21T00:00 or 2026-11-21T00:00:00)")
    };
    let (seconds, length) = match clock {
        None => (0, DAY),
        Some(clock) => seconds_of_day(clock).ok_or_else(invalid)?,
    };
    let midnight = calendar_day(day).ok_or_else(invalid)?;
    let start = u64::try_from(midnight.midnight().assume_utc().unix_timestamp())
        .ok()
        .and_then(|midnight| midnight.checked_add(seconds))
        .ok_or_else(invalid)?;
    Ok((start, length))
}

/// `YYYY-MM-DD`, a day that exists
fn calendar_day(day: &str) -> Option<time::Date> {
    let mut parts = day.split('-');
    let (year, month, day) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || year.len() != 4 || month.len() != 2 || day.len() != 2 {
        return None;
    }
    let month = time::Month::try_from(month.parse::<u8>().ok()?).ok()?;
    time::Date::from_calendar_date(year.parse().ok()?, month, day.parse().ok()?).ok()
}

/// A time of day, `HH:MM` (a minute) or `HH:MM:SS` (a second), in seconds since midnight
fn time_of_day(value: &Value) -> Result<Moment, String> {
    let written = value
        .as_str()
        .ok_or("must be a time of day, such as \"02:00\"")?;
    seconds_of_day(written)
        .ok_or_else(|| format!("{written:?} isn't a time of day (HH:MM or HH:MM:SS)"))
}

/// `HH:MM` (a minute) or `HH:MM:SS` (a second), in seconds since midnight
fn seconds_of_day(clock: &str) -> Option<Moment> {
    let mut parts = clock.split(':');
    let (hours, minutes) = (two_digits(parts.next()?)?, two_digits(parts.next()?)?);
    let (seconds, length) = match parts.next() {
        Some(seconds) => (two_digits(seconds)?, 1),
        None => (0, 60),
    };
    if parts.next().is_some() || hours > 23 || minutes > 59 || seconds > 59 {
        return None;
    }
    Some((hours * 3600 + minutes * 60 + seconds, length))
}

fn two_digits(part: &str) -> Option<u64> {
    let digits = part.len() == 2 && part.bytes().all(|byte| byte.is_ascii_digit());
    digits.then(|| part.parse().ok()).flatten()
}

/// A day of the week (`mon`...`sun`, ignoring case), as its bit: Monday's is bit 0
fn weekday(value: &Value) -> Result<u8, String> {
    const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
    let written = value.as_str().ok_or("must be a day, such as \"mon\"")?;
    DAYS.iter()
        .zip(0u8..)
        .find(|(day, _)| written.eq_ignore_ascii_case(day))
        .map(|(_, index)| 1 << index)
        .ok_or_else(|| format!("{written:?} isn't a day (mon, tue, wed, thu, fri, sat, sun)"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::rules::{
        compile::tests::{err, rules, THEN},
        eval::tests::{matching, rule, TestRequest},
    };

    #[test]
    fn test_time_values_and_errors() {
        let ok = |condition: serde_json::Value| {
            rules(&[json!({ "name": "a", "and": condition, "then": THEN() })]).is_ok()
        };
        assert!(ok(json!({ "date": { "gte": "2026-11-21" } })));
        assert!(ok(json!({ "date": { "lt": "2026-11-21T00:00" } })));
        assert!(ok(
            json!({ "date": { "between": ["2026-11-21T00:00:00Z", "2026-11-28T00:00"] } })
        ));
        // Wraps past midnight
        assert!(ok(json!({ "time": { "between": ["22:00", "02:00"] } })));
        assert!(ok(json!({ "time": { "lte": "23:59:59" } })));
        assert!(ok(json!({ "weekday": { "in": ["Mon", "fri"] } })));

        let error = |condition: serde_json::Value| {
            err(json!({ "name": "a", "and": condition, "then": THEN() }))
        };
        assert_eq!(
            error(json!({ "date": { "gte": "2026-11-21T00:00+01:00" } })),
            "rule \"a\": and.date.gte: \"2026-11-21T00:00+01:00\": times are UTC (end with `Z`, \
             or nothing)"
        );
        assert_eq!(
            error(json!({ "date": { "gte": "21/11/2026" } })),
            "rule \"a\": and.date.gte: \"21/11/2026\" isn't a date (2026-11-21, 2026-11-21T00:00 \
             or 2026-11-21T00:00:00)"
        );
        assert_eq!(
            error(json!({ "date": { "between": ["2026-12-01", "2026-11-27"] } })),
            "rule \"a\": and.date.between: \"2026-11-27\" isn't after \"2026-12-01\""
        );
        assert_eq!(
            error(json!({ "date": { "between": ["2026-12-01"] } })),
            "rule \"a\": and.date.between: must be [from, until]"
        );
        assert_eq!(
            error(json!({ "time": { "gte": "25:00" } })),
            "rule \"a\": and.time.gte: \"25:00\" isn't a time of day (HH:MM or HH:MM:SS)"
        );
        assert_eq!(
            error(json!({ "weekday": { "eq": "monday" } })),
            "rule \"a\": and.weekday.eq: \"monday\" isn't a day (mon, tue, wed, thu, fri, sat, sun)"
        );
        assert_eq!(
            error(json!({ "date": { "eq": "2026-11-21" } })),
            "rule \"a\": and.date.eq: isn't an operator of date (it takes `gte`, `gt`, `lt`, `lte` \
             and `between`)"
        );
        assert_eq!(
            error(json!({ "weekday": { "gte": "mon" } })),
            "rule \"a\": and.weekday.gte: isn't an operator of weekday (it takes `eq`, `neq`, `in` \
             and `not_in`)"
        );
        assert_eq!(
            error(json!({ "path": { "gte": "/a" } })),
            "rule \"a\": and.path.gte: compares dates and times, which this field isn't"
        );
        // Windows that never hold, rather than ones wrapping to hold all day
        assert_eq!(
            error(json!({ "time": { "lt": "00:00" } })),
            "rule \"a\": and.time.lt: never holds (nothing is in it)"
        );
        assert_eq!(
            error(json!({ "time": { "gt": "23:59:59" } })),
            "rule \"a\": and.time.gt: never holds (nothing is in it)"
        );
        assert_eq!(
            error(json!({ "time": { "between": ["02:00", "02:00"] } })),
            "rule \"a\": and.time.between: \"02:00\" isn't after \"02:00\""
        );
    }

    /// 2026-11-27T00:00:00Z, a Friday
    const BLACK_FRIDAY: u64 = 1_795_737_600;

    #[test]
    fn test_window_edges() {
        let rules = [rule(
            "bf",
            json!({ "date": { "between": ["2026-11-27", "2026-12-01"] } }),
        )];
        let at = |s: u64| matching(&rules, &TestRequest::new("GET", "a.test", "/").at(s));
        // 2026-11-26T23:59:59
        assert!(at(1_795_737_599).is_empty());
        // From: included
        assert_eq!(at(BLACK_FRIDAY), ["bf"]);
        // 2026-11-30T23:59:59
        assert_eq!(at(1_796_083_199), ["bf"]);
        // Until: excluded
        assert!(at(1_796_083_200).is_empty());
    }

    #[test]
    fn test_values_cover_the_unit_they_name() {
        // A day: `lte` includes all of it, `gt` starts after it
        let rules = [
            rule("until the 30th", json!({ "date": { "lte": "2026-11-30" } })),
            rule("after the 27th", json!({ "date": { "gt": "2026-11-27" } })),
        ];
        let at = |s: u64| matching(&rules, &TestRequest::new("GET", "a.test", "/").at(s));
        assert_eq!(at(BLACK_FRIDAY + 86_399), ["until the 30th"]);
        assert_eq!(
            at(BLACK_FRIDAY + 86_400),
            ["until the 30th", "after the 27th"]
        );
        // 2026-11-30T23:59:59, then 2026-12-01T00:00:00
        assert_eq!(at(1_796_083_199), ["until the 30th", "after the 27th"]);
        assert_eq!(at(1_796_083_200), ["after the 27th"]);

        // A minute, a second
        let rules = [
            rule("minute", json!({ "time": { "lte": "02:00" } })),
            rule("second", json!({ "time": { "lte": "02:00:00" } })),
        ];
        let at = |s: u64| {
            let request = TestRequest::new("GET", "a.test", "/").at(BLACK_FRIDAY + 7200 + s);
            matching(&rules, &request)
        };
        assert_eq!(at(0), ["minute", "second"]);
        assert_eq!(at(1), ["minute"]);
        assert_eq!(at(59), ["minute"]);
        assert!(at(60).is_empty());
    }

    #[test]
    fn test_time_windows_wrap_past_midnight() {
        let rules = [rule(
            "night",
            json!({ "time": { "between": ["22:00", "02:00"] } }),
        )];
        let at = |hour: u64| {
            let request = TestRequest::new("GET", "a.test", "/").at(BLACK_FRIDAY + hour * 3600);
            matching(&rules, &request)
        };
        assert_eq!(at(23), ["night"]);
        assert_eq!(at(1), ["night"]);
        assert_eq!(at(22), ["night"]);
        assert!(at(2).is_empty());
        assert!(at(12).is_empty());
    }

    #[test]
    fn test_weekdays() {
        let rules = [
            rule("fri", json!({ "weekday": { "eq": "fri" } })),
            rule("weekend", json!({ "weekday": { "in": ["sat", "sun"] } })),
            rule("not monday", json!({ "weekday": { "neq": "mon" } })),
        ];
        let at = |s: u64| matching(&rules, &TestRequest::new("GET", "a.test", "/").at(s));
        assert_eq!(at(BLACK_FRIDAY), ["fri", "not monday"]);
        // Sunday 23:59:59, then Monday 00:00:00
        assert_eq!(at(1_795_996_799), ["weekend", "not monday"]);
        assert!(at(1_795_996_800).is_empty());
        // 1970-01-01, a Thursday
        assert_eq!(at(0), ["not monday"]);
    }

    #[test]
    fn test_time_fields_combine() {
        let rules = [rule(
            "backups",
            json!({ "and": {
                "weekday": { "in": ["mon", "tue", "wed", "thu", "fri"] },
                "time": { "between": ["02:00", "04:00"] },
            } }),
        )];
        let at = |s: u64| matching(&rules, &TestRequest::new("GET", "a.test", "/").at(s));
        assert_eq!(at(BLACK_FRIDAY + 3 * 3600), ["backups"]);
        assert!(at(BLACK_FRIDAY + 5 * 3600).is_empty());
        // Saturday 03:00
        assert!(at(BLACK_FRIDAY + 86_400 + 3 * 3600).is_empty());
    }
}
