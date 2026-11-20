//! openCypher's temporal functions with a dotted name: `date.truncate` and
//! the other truncations, `duration.between` and its `inMonths`, `inDays`
//! and `inSeconds` forms, and the `transaction`, `statement` and `realtime`
//! clocks of each temporal type.
//!
//! A value is read as its local fields: its date, its time of day and its
//! offset, each when it has one. `localdatetime` is a [`Timestamp`],
//! `localtime` a [`Time`] without an offset, `datetime` a [`ZonedDatetime`]
//! and `time` a [`Time`] with one; a zoned type without a zone is at +00:00,
//! the default zone. A function answers what it cannot compute (a unit or a
//! key it does not know, a named time zone) with no value.

use grafeo_common::types::{Date, Duration, PropertyKey, Time, Timestamp, Value, ZonedDatetime};
use std::collections::BTreeMap;

const NANOS_PER_MICROSECOND: i64 = 1_000;
const NANOS_PER_MILLISECOND: i64 = 1_000_000;
const NANOS_PER_SECOND: i64 = 1_000_000_000;
const NANOS_PER_MINUTE: i64 = 60 * NANOS_PER_SECOND;
const NANOS_PER_HOUR: i64 = 60 * NANOS_PER_MINUTE;
const NANOS_PER_DAY: i64 = 24 * NANOS_PER_HOUR;

/// The temporal type a function gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// `date`
    Date,
    /// `time`, with an offset.
    Time,
    /// `localtime`
    LocalTime,
    /// `datetime`, with an offset.
    Datetime,
    /// `localdatetime`
    LocalDatetime,
}

impl Kind {
    fn has_date(self) -> bool {
        matches!(self, Self::Date | Self::Datetime | Self::LocalDatetime)
    }

    fn is_zoned(self) -> bool {
        matches!(self, Self::Time | Self::Datetime)
    }
}

/// What `duration.between` and its forms measure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Measure {
    /// `duration.between`: months, then days, then the time left.
    Duration,
    /// `duration.inMonths`: whole months.
    Months,
    /// `duration.inDays`: whole days.
    Days,
    /// `duration.inSeconds`: the time between, in seconds and nanoseconds.
    Seconds,
}

/// The local fields of a temporal value.
#[derive(Debug, Clone, Copy)]
struct Fields {
    date: Option<Date>,
    /// The time of day, in nanoseconds.
    nanos: Option<i64>,
    /// The offset from UTC, in seconds.
    offset: Option<i32>,
}

fn fields(value: &Value) -> Option<Fields> {
    let nanos = |time: Time| i64::try_from(time.as_nanos()).ok();
    Some(match value {
        Value::Date(date) => Fields {
            date: Some(*date),
            nanos: None,
            offset: None,
        },
        Value::Time(time) => Fields {
            date: None,
            nanos: Some(nanos(*time)?),
            offset: time.offset_seconds(),
        },
        Value::Timestamp(timestamp) => Fields {
            date: Some(timestamp.to_date()),
            nanos: Some(nanos(timestamp.to_time())?),
            offset: None,
        },
        Value::ZonedDatetime(zoned) => Fields {
            date: Some(zoned.to_local_date()),
            nanos: Some(nanos(zoned.to_local_time())?),
            offset: Some(zoned.offset_seconds()),
        },
        _ => return None,
    })
}

/// The value of `kind` with these local fields; `None` for a datetime more
/// than about 292,000 years from 1970, which a [`Timestamp`]'s count of
/// microseconds does not reach.
fn value_of(kind: Kind, date: Date, nanos: i64, offset: i32) -> Option<Value> {
    const MAX_DAYS: i64 = i64::MAX / (NANOS_PER_DAY / NANOS_PER_MICROSECOND) - 1;
    if matches!(kind, Kind::Datetime | Kind::LocalDatetime)
        && i64::from(date.as_days()).abs() > MAX_DAYS
    {
        return None;
    }
    let time = Time::from_nanos(u64::try_from(nanos).ok()?)?;
    Some(match kind {
        Kind::Date => Value::Date(date),
        Kind::Time => Value::Time(time.with_offset(offset)),
        Kind::LocalTime => Value::Time(time),
        Kind::Datetime => Value::ZonedDatetime(ZonedDatetime::from_date_time(
            date,
            time.with_offset(offset),
        )?),
        Kind::LocalDatetime => Value::Timestamp(Timestamp::from_date_time(date, time)),
    })
}

/// The offset a time zone such as `'+01:00'` or `'Z'` names; a named zone
/// such as `'Europe/Stockholm'` is not known yet.
pub(super) fn offset_of_zone(zone: &str) -> Option<i32> {
    Time::parse(&format!("00:00:00{zone}"))?.offset_seconds()
}

/// The day of the week, from 0 (Monday) to 6 (Sunday).
fn weekday(date: Date) -> i32 {
    // 1970-01-01 was a Thursday.
    (date.as_days() + 3).rem_euclid(7)
}

/// The first day of the ISO week-based year `date` is in: the Monday of the
/// week of its January 4th.
fn week_year_start(date: Date) -> Option<Date> {
    let thursday = Date::from_days(date.as_days() + 3 - weekday(date));
    let january_4 = Date::from_ymd(thursday.year(), 1, 4)?;
    Some(Date::from_days(january_4.as_days() - weekday(january_4)))
}

/// The unit a truncation keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Unit {
    Millennium,
    Century,
    Decade,
    Year,
    WeekYear,
    Quarter,
    Month,
    Week,
    Day,
    Hour,
    Minute,
    Second,
    Millisecond,
    Microsecond,
}

impl Unit {
    fn parse(text: &str) -> Option<Self> {
        const UNITS: [(&str, Unit); 14] = [
            ("millennium", Unit::Millennium),
            ("century", Unit::Century),
            ("decade", Unit::Decade),
            ("year", Unit::Year),
            ("weekYear", Unit::WeekYear),
            ("quarter", Unit::Quarter),
            ("month", Unit::Month),
            ("week", Unit::Week),
            ("day", Unit::Day),
            ("hour", Unit::Hour),
            ("minute", Unit::Minute),
            ("second", Unit::Second),
            ("millisecond", Unit::Millisecond),
            ("microsecond", Unit::Microsecond),
        ];
        UNITS
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(text))
            .map(|&(_, unit)| unit)
    }
}

/// `kind.truncate(unit, value, overrides)`: `value` truncated to `unit` as
/// a value of `kind`, with the fields `overrides` names set after. A date is
/// at midnight, and a zoned type keeps the value's offset, +00:00 without
/// one; `timezone` sets the offset and keeps the local fields. A date type
/// truncates to a day at the finest, a time type to a day at the coarsest
/// (midnight).
pub(super) fn truncate(
    kind: Kind,
    unit: &str,
    value: &Value,
    overrides: &BTreeMap<PropertyKey, Value>,
) -> Option<Value> {
    let unit = Unit::parse(unit)?;
    let input = fields(value)?;
    let (mut date, mut nanos) = if kind.has_date() {
        if unit > Unit::Day && kind == Kind::Date {
            return None;
        }
        (input.date?, input.nanos.unwrap_or(0))
    } else {
        if unit < Unit::Day {
            return None;
        }
        (Date::from_days(0), input.nanos?)
    };
    let mut offset = input.offset.unwrap_or(0);

    let (year, month, _) = date.to_ymd();
    let start_of_year = |year: i32| Date::from_ymd(year, 1, 1);
    match unit {
        Unit::Millennium => date = start_of_year(year.div_euclid(1000) * 1000)?,
        Unit::Century => date = start_of_year(year.div_euclid(100) * 100)?,
        Unit::Decade => date = start_of_year(year.div_euclid(10) * 10)?,
        Unit::Year => date = start_of_year(year)?,
        Unit::WeekYear => date = week_year_start(date)?,
        Unit::Quarter => date = Date::from_ymd(year, (month - 1) / 3 * 3 + 1, 1)?,
        Unit::Month => date = Date::from_ymd(year, month, 1)?,
        Unit::Week => date = Date::from_days(date.as_days() - weekday(date)),
        Unit::Day => {}
        Unit::Hour => nanos -= nanos % NANOS_PER_HOUR,
        Unit::Minute => nanos -= nanos % NANOS_PER_MINUTE,
        Unit::Second => nanos -= nanos % NANOS_PER_SECOND,
        Unit::Millisecond => nanos -= nanos % NANOS_PER_MILLISECOND,
        Unit::Microsecond => nanos -= nanos % NANOS_PER_MICROSECOND,
    }
    if unit <= Unit::Day {
        nanos = 0;
    }

    // The fields `overrides` sets, each an integer but the zone.
    let field = |key: &str| -> Option<Option<i64>> {
        match overrides.get(&PropertyKey::from(key)) {
            None => Some(None),
            Some(Value::Int64(number)) => Some(Some(*number)),
            Some(_) => None,
        }
    };
    const KEYS: [&str; 11] = [
        "year",
        "month",
        "day",
        "dayOfWeek",
        "hour",
        "minute",
        "second",
        "millisecond",
        "microsecond",
        "nanosecond",
        "timezone",
    ];
    if overrides.keys().any(|key| !KEYS.contains(&key.as_str())) {
        return None;
    }
    let (year, month, day) = (field("year")?, field("month")?, field("day")?);
    let day_of_week = field("dayOfWeek")?;
    if !kind.has_date()
        && (year.is_some() || month.is_some() || day.is_some() || day_of_week.is_some())
    {
        return None;
    }
    if year.is_some() || month.is_some() || day.is_some() {
        let (y, m, d) = date.to_ymd();
        let year = year.map_or(Some(y), |year| i32::try_from(year).ok())?;
        let month = month.map_or(Some(m), |month| u32::try_from(month).ok())?;
        let day = day.map_or(Some(d), |day| u32::try_from(day).ok())?;
        date = Date::from_ymd(year, month, day)?;
    }
    if let Some(day_of_week) = day_of_week {
        if !(1..=7).contains(&day_of_week) {
            return None;
        }
        let day_of_week = i32::try_from(day_of_week).ok()?;
        date = Date::from_days(date.as_days() - weekday(date) + day_of_week - 1);
    }
    let time_fields = [
        ("hour", NANOS_PER_HOUR, 24),
        ("minute", NANOS_PER_MINUTE, 60),
        ("second", NANOS_PER_SECOND, 60),
        ("millisecond", NANOS_PER_MILLISECOND, 1_000),
        ("microsecond", NANOS_PER_MICROSECOND, 1_000),
        ("nanosecond", 1, 1_000),
    ];
    if kind == Kind::Date && time_fields.iter().any(|(key, ..)| field(key) != Some(None)) {
        return None;
    }
    for (key, size, count) in time_fields {
        let Some(number) = field(key)? else {
            continue;
        };
        // A nanosecond alone may give the whole fraction of a second after
        // a truncation to a second or coarser.
        let count = if key == "nanosecond" && nanos % NANOS_PER_SECOND == 0 {
            NANOS_PER_SECOND
        } else {
            count
        };
        if !(0..count).contains(&number) {
            return None;
        }
        let current = (nanos / size) % count;
        nanos += (number - current) * size;
    }
    match overrides.get(&PropertyKey::from("timezone")) {
        None => {}
        Some(Value::String(zone)) if kind.is_zoned() => offset = offset_of_zone(zone)?,
        Some(_) => return None,
    }
    value_of(kind, date, nanos, offset)
}

/// A date and a time of day as one count of nanoseconds since 1970-01-01.
fn instant(date: Date, nanos: i64) -> i128 {
    i128::from(date.as_days()) * i128::from(NANOS_PER_DAY) + i128::from(nanos)
}

/// The date and time of day of a count of nanoseconds since 1970-01-01.
fn split(instant: i128) -> Option<(Date, i64)> {
    let day = i128::from(NANOS_PER_DAY);
    Some((
        Date::from_days(i32::try_from(instant.div_euclid(day)).ok()?),
        i64::try_from(instant.rem_euclid(day)).ok()?,
    ))
}

/// The whole months or days (`months`) from one date and time to another, as
/// java.time counts them: a last day that is not complete does not count.
fn whole_units(from: (Date, i64), to: (Date, i64), months: bool) -> i64 {
    let (start, start_time) = from;
    let (mut end, end_time) = to;
    if end > start && end_time < start_time {
        end = Date::from_days(end.as_days() - 1);
    } else if end < start && end_time > start_time {
        end = Date::from_days(end.as_days() + 1);
    }
    if months {
        let packed = |date: Date| {
            let (year, month, day) = date.to_ymd();
            (i64::from(year) * 12 + i64::from(month) - 1) * 32 + i64::from(day)
        };
        (packed(end) - packed(start)) / 32
    } else {
        i64::from(end.as_days()) - i64::from(start.as_days())
    }
}

/// `date` plus `months` months, on the last day of the month when the month
/// is shorter.
fn add_months(date: Date, months: i64) -> Option<Date> {
    let (year, month, day) = date.to_ymd();
    let index = i64::from(year) * 12 + i64::from(month) - 1 + months;
    let year = i32::try_from(index.div_euclid(12)).ok()?;
    let month = u32::try_from(index.rem_euclid(12) + 1).ok()?;
    (day.saturating_sub(3)..=day)
        .rev()
        .find_map(|day| Date::from_ymd(year, month, day))
}

/// The duration from `from` to `to`, as `measure` counts it. Before it is
/// counted, a value without a time of day is at midnight next to one with
/// it (and always for `inSeconds`), a value without a date is on the other's
/// date, and a local value is in the other's zone. Two zoned values are
/// compared in `from`'s offset.
pub(super) fn between(measure: Measure, from: &Value, to: &Value) -> Option<Value> {
    let (mut from, mut to) = (fields(from)?, fields(to)?);
    if measure == Measure::Seconds || from.nanos.is_some() != to.nanos.is_some() {
        from.nanos.get_or_insert(0);
        to.nanos.get_or_insert(0);
    }
    if from.date.is_none() {
        from.date = to.date;
    }
    if to.date.is_none() {
        to.date = from.date;
    }
    if from.offset.is_none() {
        from.offset = to.offset;
    }
    if to.offset.is_none() {
        to.offset = from.offset;
    }
    let start_offset = i128::from(from.offset.unwrap_or(0)) * i128::from(NANOS_PER_SECOND);
    let end_offset = i128::from(to.offset.unwrap_or(0)) * i128::from(NANOS_PER_SECOND);
    let (Some(start_date), Some(end_date)) = (from.date, to.date) else {
        // Two times of day: no months or days between them.
        let nanos = match measure {
            Measure::Months | Measure::Days => 0,
            Measure::Duration | Measure::Seconds => i64::try_from(
                (i128::from(to.nanos?) - end_offset) - (i128::from(from.nanos?) - start_offset),
            )
            .ok()?,
        };
        return Some(Value::Duration(Duration::new(0, 0, nanos)));
    };
    let start = (start_date, from.nanos.unwrap_or(0));
    // The end in the start's offset.
    let end = split(instant(end_date, to.nanos.unwrap_or(0)) - end_offset + start_offset)?;
    let nanos_between =
        |from: (Date, i64)| i64::try_from(instant(end.0, end.1) - instant(from.0, from.1)).ok();
    let duration = match measure {
        Measure::Months => Duration::new(whole_units(start, end, true), 0, 0),
        Measure::Days => Duration::new(0, whole_units(start, end, false), 0),
        Measure::Seconds => Duration::new(0, 0, nanos_between(start)?),
        Measure::Duration => {
            let months = whole_units(start, end, true);
            let start = (add_months(start.0, months)?, start.1);
            let days = whole_units(start, end, false);
            let start = (
                Date::from_days(start.0.as_days().checked_add(i32::try_from(days).ok()?)?),
                start.1,
            );
            Duration::new(months, days, nanos_between(start)?)
        }
    };
    Some(Value::Duration(duration))
}

/// The value of `kind` a clock that reads `instant` gives, in `zone`
/// (`'+01:00'`), or at +00:00, the default zone, without one.
pub(super) fn clock(kind: Kind, instant: Timestamp, zone: Option<&Value>) -> Option<Value> {
    let offset = match zone {
        None => 0,
        Some(Value::Null) => return Some(Value::Null),
        Some(Value::String(zone)) => offset_of_zone(zone)?,
        Some(_) => return None,
    };
    if kind == Kind::Datetime {
        return Some(Value::ZonedDatetime(ZonedDatetime::from_timestamp_offset(
            instant, offset,
        )));
    }
    let local = instant.add_micros(i64::from(offset) * 1_000_000);
    let nanos = i64::try_from(local.to_time().as_nanos()).ok()?;
    value_of(kind, local.to_date(), nanos, offset)
}

/// `datetime.fromepoch(seconds, nanoseconds)`: the instant that long after
/// 1970-01-01T00:00:00Z, at +00:00.
pub(super) fn from_epoch(seconds: i64, nanoseconds: i64) -> Option<Value> {
    let micros = seconds
        .checked_mul(1_000_000)?
        .checked_add(nanoseconds.div_euclid(NANOS_PER_MICROSECOND))?;
    Some(Value::ZonedDatetime(ZonedDatetime::from_timestamp_offset(
        Timestamp::from_micros(micros),
        0,
    )))
}

/// The text openCypher's `toString()` gives a temporal value, as java.time
/// writes it: a time of day without its seconds when they and their
/// fraction are zero (`12:00`, `1984-10-11T00:00Z`), and a duration with the
/// sign on each component (`PT-1M-0.5S`, where ISO writes `-PT1M0.5S`).
/// `None` for a value that is no temporal.
pub(super) fn opencypher_text(value: &Value) -> Option<String> {
    // Display writes `HH:MM:SS` first in a time and after the `T` of a
    // datetime.
    let without_zero_seconds = |text: String, time: Time, start: usize| {
        if time.second() == 0 && time.nanosecond() == 0 {
            let mut text = text;
            text.replace_range(start + 5..start + 8, "");
            text
        } else {
            text
        }
    };
    Some(match value {
        Value::Date(date) => date.to_string(),
        Value::Time(time) => without_zero_seconds(time.to_string(), *time, 0),
        Value::Timestamp(timestamp) => {
            let text = timestamp.to_local_text();
            let start = text.find('T')? + 1;
            without_zero_seconds(text, timestamp.to_time(), start)
        }
        Value::ZonedDatetime(zoned) => {
            let text = zoned.to_string();
            let start = text.find('T')? + 1;
            without_zero_seconds(text, zoned.to_local_time(), start)
        }
        Value::Duration(duration) => duration_text(duration),
        _ => return None,
    })
}

fn duration_text(duration: &Duration) -> String {
    use std::fmt::Write as _;
    let mut text = String::from("P");
    let (months, days, nanos) = (duration.months(), duration.days(), duration.nanos());
    for (amount, unit) in [(months / 12, 'Y'), (months % 12, 'M'), (days, 'D')] {
        if amount != 0 {
            let _ = write!(text, "{amount}{unit}");
        }
    }
    if nanos != 0 {
        text.push('T');
        let hours = nanos / NANOS_PER_HOUR;
        let minutes = nanos % NANOS_PER_HOUR / NANOS_PER_MINUTE;
        let seconds = nanos % NANOS_PER_MINUTE / NANOS_PER_SECOND;
        let fraction = nanos % NANOS_PER_SECOND;
        for (amount, unit) in [(hours, 'H'), (minutes, 'M')] {
            if amount != 0 {
                let _ = write!(text, "{amount}{unit}");
            }
        }
        if seconds != 0 || fraction != 0 {
            let sign = if nanos < 0 { "-" } else { "" };
            let _ = write!(text, "{sign}{}", seconds.unsigned_abs());
            if fraction != 0 {
                let digits = format!("{:09}", fraction.unsigned_abs());
                let _ = write!(text, ".{}", digits.trim_end_matches('0'));
            }
            text.push('S');
        }
    }
    if text == "P" {
        text.push_str("T0S");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opencypher_text_drops_zero_seconds_and_signs_each_component() {
        let text = |value: Value| opencypher_text(&value).unwrap();
        assert_eq!(text(local_time("12:00")), "12:00");
        assert_eq!(text(local_time("12:00:00.5")), "12:00:00.5");
        assert_eq!(
            text(Value::Time(Time::parse("12:00:00+01:00").unwrap())),
            "12:00+01:00"
        );
        assert_eq!(
            text(local_datetime("1984-10-11", "12:31")),
            "1984-10-11T12:31"
        );
        assert_eq!(
            text(local_datetime("1984-10-11", "12:31:14")),
            "1984-10-11T12:31:14"
        );
        assert_eq!(text(zoned("1984-10-11", "00:00Z")), "1984-10-11T00:00Z");
        assert_eq!(text(date("1984-10-11")), "1984-10-11");
        let duration = |months, days, nanos| Value::Duration(Duration::new(months, days, nanos));
        assert_eq!(
            text(duration(0, 0, -400 * NANOS_PER_MILLISECOND)),
            "PT-0.4S"
        );
        assert_eq!(
            text(duration(0, 0, -(NANOS_PER_MINUTE + NANOS_PER_MILLISECOND))),
            "PT-1M-0.001S"
        );
        assert_eq!(text(duration(-20, -27, 0)), "P-1Y-8M-27D");
        assert_eq!(text(duration(0, 1, -NANOS_PER_MILLISECOND)), "P1DT-0.001S");
        assert_eq!(
            text(duration(149, 14, 16 * NANOS_PER_HOUR)),
            "P12Y5M14DT16H"
        );
        assert_eq!(text(duration(0, 0, 0)), "PT0S");
        assert_eq!(opencypher_text(&Value::Int64(1)), None);
    }

    fn date(text: &str) -> Value {
        Value::Date(Date::parse(text).unwrap())
    }

    fn local_time(text: &str) -> Value {
        Value::Time(Time::parse(text).unwrap())
    }

    fn local_datetime(date: &str, time: &str) -> Value {
        Value::Timestamp(Timestamp::from_date_time(
            Date::parse(date).unwrap(),
            Time::parse(time).unwrap(),
        ))
    }

    fn zoned(date: &str, time: &str) -> Value {
        Value::ZonedDatetime(
            ZonedDatetime::from_date_time(Date::parse(date).unwrap(), Time::parse(time).unwrap())
                .unwrap(),
        )
    }

    fn map(entries: &[(&str, Value)]) -> BTreeMap<PropertyKey, Value> {
        entries
            .iter()
            .map(|(key, value)| (PropertyKey::from(*key), value.clone()))
            .collect()
    }

    fn duration(months: i64, days: i64, nanos: i64) -> Option<Value> {
        Some(Value::Duration(Duration::new(months, days, nanos)))
    }

    #[test]
    fn a_date_truncates_to_each_unit_down_to_a_day() {
        let d = date("1984-11-11");
        for (unit, expected) in [
            ("millennium", "1000-01-01"),
            ("century", "1900-01-01"),
            ("decade", "1980-01-01"),
            ("year", "1984-01-01"),
            ("quarter", "1984-10-01"),
            ("month", "1984-11-01"),
            ("week", "1984-11-05"),
            ("day", "1984-11-11"),
        ] {
            assert_eq!(
                truncate(Kind::Date, unit, &d, &BTreeMap::new()),
                Some(date(expected)),
                "{unit}"
            );
        }
        assert_eq!(truncate(Kind::Date, "hour", &d, &BTreeMap::new()), None);
        assert_eq!(
            truncate(Kind::Date, "fortnight", &d, &BTreeMap::new()),
            None
        );
    }

    #[test]
    fn a_week_year_starts_on_the_monday_of_its_january_4th() {
        let none = BTreeMap::new();
        assert_eq!(
            truncate(Kind::Date, "weekYear", &date("1984-02-01"), &none),
            Some(date("1984-01-02"))
        );
        // 1984-01-01 is a Sunday of the last week of 1983.
        assert_eq!(
            truncate(Kind::Date, "weekYear", &date("1984-01-01"), &none),
            Some(date("1983-01-03"))
        );
    }

    #[test]
    fn overrides_set_fields_after_the_truncation() {
        let d = date("1984-10-11");
        assert_eq!(
            truncate(
                Kind::Date,
                "millennium",
                &d,
                &map(&[("day", Value::Int64(2))])
            ),
            Some(date("1000-01-02"))
        );
        assert_eq!(
            truncate(
                Kind::Date,
                "week",
                &d,
                &map(&[("dayOfWeek", Value::Int64(2))])
            ),
            Some(date("1984-10-09"))
        );
        let t = local_time("12:31:14.645876123");
        let nanosecond = map(&[("nanosecond", Value::Int64(2))]);
        assert_eq!(
            truncate(Kind::LocalTime, "millisecond", &t, &nanosecond),
            Some(local_time("12:31:14.645000002"))
        );
        assert_eq!(
            truncate(Kind::LocalTime, "microsecond", &t, &nanosecond),
            Some(local_time("12:31:14.645876002"))
        );
        assert_eq!(
            truncate(
                Kind::LocalTime,
                "second",
                &t,
                &map(&[("nanosecond", Value::Int64(5_000))])
            ),
            Some(local_time("12:31:14.000005"))
        );
        assert_eq!(
            truncate(Kind::Date, "year", &d, &map(&[("hour", Value::Int64(1))])),
            None
        );
        assert_eq!(
            truncate(Kind::Date, "year", &d, &map(&[("days", Value::Int64(1))])),
            None
        );
    }

    #[test]
    fn a_zoned_truncation_keeps_the_offset_or_takes_the_new_zone() {
        let at_minus_one = zoned("1984-10-11", "12:31:14-01:00");
        assert_eq!(
            truncate(Kind::Time, "hour", &at_minus_one, &BTreeMap::new()),
            Some(Value::Time(Time::parse("12:00-01:00").unwrap()))
        );
        assert_eq!(
            truncate(
                Kind::Datetime,
                "hour",
                &at_minus_one,
                &map(&[("timezone", Value::from("+01:00"))])
            ),
            Some(zoned("1984-10-11", "12:00+01:00"))
        );
        // A local value is at +00:00, and a named zone is not known yet.
        assert_eq!(
            truncate(Kind::Datetime, "day", &date("1984-10-11"), &BTreeMap::new()),
            Some(zoned("1984-10-11", "00:00Z"))
        );
        assert_eq!(
            truncate(
                Kind::Datetime,
                "day",
                &date("1984-10-11"),
                &map(&[("timezone", Value::from("Europe/Stockholm"))])
            ),
            None
        );
        assert_eq!(
            truncate(Kind::LocalTime, "hour", &at_minus_one, &BTreeMap::new()),
            Some(local_time("12:00"))
        );
    }

    #[test]
    fn a_datetime_beyond_a_timestamp_is_no_value() {
        let d = local_datetime("1984-10-11", "12:00");
        let far = map(&[("year", Value::Int64(300_000))]);
        assert_eq!(truncate(Kind::LocalDatetime, "year", &d, &far), None);
        assert_eq!(
            truncate(Kind::Date, "year", &d, &far),
            Some(Value::Date(Date::from_ymd(300_000, 1, 1).unwrap()))
        );
    }

    #[test]
    fn a_time_truncates_from_a_day_down() {
        let t = local_datetime("1984-10-11", "12:31:14");
        assert_eq!(
            truncate(Kind::LocalTime, "day", &t, &BTreeMap::new()),
            Some(local_time("00:00"))
        );
        assert_eq!(
            truncate(Kind::LocalTime, "month", &t, &BTreeMap::new()),
            None
        );
        assert_eq!(
            truncate(
                Kind::LocalTime,
                "hour",
                &date("1984-10-11"),
                &BTreeMap::new()
            ),
            None
        );
    }

    #[test]
    fn between_counts_months_then_days_then_the_rest() {
        let hour = NANOS_PER_HOUR;
        assert_eq!(
            between(Measure::Duration, &date("1984-10-11"), &date("2015-06-24")),
            duration(30 * 12 + 8, 13, 0)
        );
        assert_eq!(
            between(
                Measure::Duration,
                &local_datetime("2018-01-01", "12:00"),
                &local_datetime("2018-01-02", "10:00")
            ),
            duration(0, 0, 22 * hour)
        );
        assert_eq!(
            between(
                Measure::Duration,
                &zoned("2014-07-21", "21:40:36.143+02:00"),
                &date("2015-06-24")
            ),
            duration(
                11,
                2,
                2 * hour + 19 * NANOS_PER_MINUTE + 23_857 * NANOS_PER_MILLISECOND
            )
        );
        // In the start's offset: 21:40:32.142+01:00 is 22:40:32.142+02:00.
        assert_eq!(
            between(
                Measure::Duration,
                &zoned("2014-07-21", "21:40:36.143+02:00"),
                &zoned("2015-07-21", "21:40:32.142+01:00")
            ),
            duration(
                12,
                0,
                59 * NANOS_PER_MINUTE + 55_999 * NANOS_PER_MILLISECOND
            )
        );
    }

    #[test]
    fn a_missing_part_comes_from_the_other_value() {
        let hour = NANOS_PER_HOUR;
        let minute = NANOS_PER_MINUTE;
        assert_eq!(
            between(Measure::Duration, &date("1984-10-11"), &local_time("16:30")),
            duration(0, 0, 16 * hour + 30 * minute)
        );
        // Both zoned: 14:30Z to 16:30+01:00 is an hour.
        assert_eq!(
            between(
                Measure::Seconds,
                &Value::Time(Time::parse("14:30Z").unwrap()),
                &Value::Time(Time::parse("16:30+01:00").unwrap())
            ),
            duration(0, 0, hour)
        );
        // A local time is in the other's zone.
        assert_eq!(
            between(
                Measure::Seconds,
                &local_time("14:30"),
                &Value::Time(Time::parse("16:30+01:00").unwrap())
            ),
            duration(0, 0, 2 * hour)
        );
        assert_eq!(
            between(Measure::Months, &date("1984-10-11"), &local_time("16:30")),
            duration(0, 0, 0)
        );
    }

    #[test]
    fn whole_months_and_days_round_toward_zero() {
        assert_eq!(
            between(
                Measure::Months,
                &date("2018-07-21"),
                &zoned("2016-07-21", "21:40:32.142+01:00")
            ),
            duration(-23, 0, 0)
        );
        assert_eq!(
            between(
                Measure::Days,
                &local_datetime("2015-07-21", "21:40:32.142"),
                &date("2015-06-24")
            ),
            duration(0, -27, 0)
        );
        assert_eq!(
            between(
                Measure::Seconds,
                &local_time("12:34:56.3"),
                &local_time("12:34:54.7")
            ),
            duration(0, 0, -1_600 * NANOS_PER_MILLISECOND)
        );
    }

    #[test]
    fn a_month_added_past_a_short_month_ends_on_its_last_day() {
        let jan_31 = Date::parse("2015-01-31").unwrap();
        assert_eq!(add_months(jan_31, 1), Date::parse("2015-02-28"));
        assert_eq!(add_months(jan_31, -2), Date::parse("2014-11-30"));
    }

    #[test]
    fn a_clock_reads_its_instant_in_a_zone() {
        let instant = Timestamp::from_date_time(
            Date::parse("2024-03-15").unwrap(),
            Time::parse("23:30").unwrap(),
        );
        assert_eq!(clock(Kind::Date, instant, None), Some(date("2024-03-15")));
        assert_eq!(
            clock(Kind::Date, instant, Some(&Value::from("+01:00"))),
            Some(date("2024-03-16"))
        );
        assert_eq!(
            clock(Kind::Datetime, instant, Some(&Value::from("+01:00"))),
            Some(zoned("2024-03-16", "00:30+01:00"))
        );
        assert_eq!(
            clock(Kind::Date, instant, Some(&Value::Null)),
            Some(Value::Null)
        );
        assert_eq!(
            clock(Kind::Date, instant, Some(&Value::from("Europe/Paris"))),
            None
        );
    }

    #[test]
    fn an_epoch_instant_is_at_utc() {
        assert_eq!(
            from_epoch(237_821_673, 987_000_000),
            Some(zoned("1977-07-15", "13:34:33.987Z"))
        );
    }
}
