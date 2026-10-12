//! The `time.*` builtin functions.

use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Weekday};

use super::typed::{Arg, builtins};
use crate::defs::TypeId;
use crate::runtime::sync::Wait;
use crate::typeinfo::{bv, ty};
use crate::value::{Record, Value};
use crate::vm::{Step, VmError};

/// Compute (year, month, day) from Unix epoch seconds.
/// Uses Howard Hinnant's civil_from_days algorithm (public domain).
#[cfg(not(feature = "local-clock"))]
fn civil_from_epoch_secs(secs: i64) -> (i32, u32, u32) {
    let z = secs.div_euclid(86400) + 719468;
    let era = z.div_euclid(146097);
    let doe = (z - era * 146097) as u32; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year [0, 365]
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

/// Whether `year` is a leap year.
fn leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

/// The number of days in the month (1-12) of the year.
fn days_in(year: i32, month: u32) -> u32 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 if leap(year) => 29,
        2 => 28,
        _ => 31,
    }
}

/// `Err(TimeError)` of a chrono `ParseError`: `InvalidFormat` for a
/// pattern that is no format or whose fields are not enough for the
/// value (what [`fits`] and [`enough`] have said before the text was
/// read, in nearly every case), `OutOfRange` for a field that is out
/// of its range, `ParseFormat` for text that does not match the
/// pattern.
fn time_parse_err(err: chrono::ParseError) -> Value {
    use chrono::format::ParseErrorKind;
    let msg = err.to_string();
    let variant = match err.kind() {
        ParseErrorKind::BadFormat | ParseErrorKind::NotEnough => bv::TIME_INVALID_FORMAT,
        ParseErrorKind::OutOfRange => bv::TIME_OUT_OF_RANGE,
        _ => bv::TIME_PARSE_FORMAT,
    };
    Value::variant(
        bv::ERR,
        vec![Value::variant(variant, vec![Value::String(msg.into())])],
    )
}

/// `Err(InvalidFormat(why))`: the pattern of a format is none.
fn invalid_format_err(why: String) -> Value {
    Value::variant(
        bv::ERR,
        vec![Value::variant(
            bv::TIME_INVALID_FORMAT,
            vec![Value::String(why.into())],
        )],
    )
}

/// Build `Err(OutOfRange(msg))` for explicit range rejections from
/// `time.date` / `time.time`.
fn time_out_of_range_err(msg: String) -> Value {
    Value::variant(
        bv::ERR,
        vec![Value::variant(
            bv::TIME_OUT_OF_RANGE,
            vec![Value::String(msg.into())],
        )],
    )
}

/// What `TimeError`'s `message` says of the variant `tag` with `fields`:
/// `None` if they are no variant of it.
pub(crate) fn error_text(tag: &str, fields: &[Value]) -> Option<String> {
    Some(match (tag, fields) {
        ("ParseFormat", [Value::String(m)]) => format!("time parse error: {m}"),
        ("OutOfRange", [Value::String(m)]) => format!("time out of range: {m}"),
        ("InvalidFormat", [Value::String(m)]) => format!("time format error: {m}"),
        ("Other", [Value::String(m)]) => m.to_string(),
        _ => return None,
    })
}

// ── Time helpers ────────────────────────────────────────────────────

/// Build a Silt `Date` record Value from chrono NaiveDate.
pub(crate) fn make_date(d: NaiveDate) -> Value {
    Value::builtin_record(
        ty::DATE,
        [
            ("year", Value::Int(d.year() as i64)),
            ("month", Value::Int(d.month() as i64)),
            ("day", Value::Int(d.day() as i64)),
        ],
    )
}

/// Build a Silt `Time` record Value from chrono NaiveTime.
pub(crate) fn make_time(t: NaiveTime) -> Value {
    Value::builtin_record(
        ty::TIME,
        [
            ("hour", Value::Int(t.hour() as i64)),
            ("minute", Value::Int(t.minute() as i64)),
            ("second", Value::Int(t.second() as i64)),
            ("ns", Value::Int(t.nanosecond() as i64)),
        ],
    )
}

/// Build a Silt `DateTime` record Value from chrono NaiveDateTime.
pub(crate) fn make_datetime(dt: NaiveDateTime) -> Value {
    Value::builtin_record(
        ty::DATE_TIME,
        [
            ("date", make_date(dt.date())),
            ("time", make_time(dt.time())),
        ],
    )
}

/// Build a Silt `Instant` record Value.
fn make_instant(epoch_ns: i64) -> Value {
    Value::builtin_record(ty::INSTANT, [("epoch_ns", Value::Int(epoch_ns))])
}

/// Build a Silt `Duration` record Value.
fn make_duration(ns: i64) -> Value {
    Value::builtin_record(ty::DURATION, [("ns", Value::Int(ns))])
}

// ── The arguments ───────────────────────────────────────────────────

/// `value`, if it is a record of the builtin type `ty`.
fn record(value: &Value, ty: TypeId) -> Option<&Record> {
    match value {
        Value::Record(record) if record.type_id() == ty => Some(record),
        _ => None,
    }
}

/// The `Int` field `name` of a record.
fn int(fields: &Record, name: &str) -> Option<i64> {
    i64::take(fields.get(name)?)
}

/// `n` as the `what` of a date (its year), for `name`: an error if it
/// is no `i32`. (A cast would truncate: the year `u32::MAX + 1999`
/// would be 1999.)
fn as_i32(name: &str, what: &str, n: i64) -> Result<i32, VmError> {
    i32::try_from(n).map_err(|_| VmError::new(format!("{name}: {what} {n} out of range for i32")))
}

/// The same for the parts that are a `u32` (month, day, hour, minute,
/// second, nanosecond).
fn as_u32(name: &str, what: &str, n: i64) -> Result<u32, VmError> {
    u32::try_from(n).map_err(|_| VmError::new(format!("{name}: {what} {n} out of range for u32")))
}

/// A `Date` argument: its fields, which a program can set to what is
/// no date of the calendar ([`Date::naive`]).
#[derive(Clone, Copy)]
struct Date {
    year: i64,
    month: i64,
    day: i64,
}

impl<'a> Arg<'a> for Date {
    fn take(value: &'a Value) -> Option<Self> {
        let fields = record(value, ty::DATE)?;
        Some(Date {
            year: int(fields, "year")?,
            month: int(fields, "month")?,
            day: int(fields, "day")?,
        })
    }
}

impl Date {
    fn naive(self) -> Result<NaiveDate, VmError> {
        let y = as_i32("time", "year", self.year)?;
        let m = as_u32("time", "month", self.month)?;
        let d = as_u32("time", "day", self.day)?;
        NaiveDate::from_ymd_opt(y, m, d)
            .ok_or_else(|| VmError::new(format!("invalid date: {y}-{m}-{d}")))
    }
}

/// A `Time` argument: its fields, like a [`Date`]'s.
#[derive(Clone, Copy)]
struct Time {
    hour: i64,
    minute: i64,
    second: i64,
    ns: i64,
}

impl<'a> Arg<'a> for Time {
    fn take(value: &'a Value) -> Option<Self> {
        let fields = record(value, ty::TIME)?;
        Some(Time {
            hour: int(fields, "hour")?,
            minute: int(fields, "minute")?,
            second: int(fields, "second")?,
            ns: int(fields, "ns")?,
        })
    }
}

impl Time {
    fn naive(self) -> Result<NaiveTime, VmError> {
        let h = as_u32("time", "hour", self.hour)?;
        let m = as_u32("time", "minute", self.minute)?;
        let s = as_u32("time", "second", self.second)?;
        let ns = as_u32("time", "ns", self.ns)?;
        NaiveTime::from_hms_nano_opt(h, m, s, ns)
            .ok_or_else(|| VmError::new(format!("invalid time: {h}:{m}:{s}.{ns}")))
    }
}

/// A `DateTime` argument.
#[derive(Clone, Copy)]
struct DateTime {
    date: Date,
    time: Time,
}

impl<'a> Arg<'a> for DateTime {
    fn take(value: &'a Value) -> Option<Self> {
        let fields = record(value, ty::DATE_TIME)?;
        Some(DateTime {
            date: Date::take(fields.get("date")?)?,
            time: Time::take(fields.get("time")?)?,
        })
    }
}

impl DateTime {
    fn naive(self) -> Result<NaiveDateTime, VmError> {
        Ok(NaiveDateTime::new(self.date.naive()?, self.time.naive()?))
    }
}

/// An `Instant` argument: its nanoseconds since the epoch.
#[derive(Clone, Copy)]
struct Instant(i64);

impl<'a> Arg<'a> for Instant {
    fn take(value: &'a Value) -> Option<Self> {
        int(record(value, ty::INSTANT)?, "epoch_ns").map(Instant)
    }
}

impl Instant {
    /// The date and time of the instant in UTC.
    fn utc(self) -> Result<NaiveDateTime, VmError> {
        // Rust `i64 % i64` carries the sign of the dividend, so for a
        // negative instant whose magnitude isn't a multiple of 1e9 the
        // remainder is negative; casting to `u32` wraps it to a huge
        // value and chrono then rejects the instant. With
        // div_euclid/rem_euclid the remainder is always in
        // `[0, 1_000_000_000)` and seconds round toward negative
        // infinity, which matches chrono's own expectations.
        let secs = self.0.div_euclid(1_000_000_000);
        let nanos = self.0.rem_euclid(1_000_000_000) as u32;
        chrono::DateTime::from_timestamp(secs, nanos)
            .map(|at| at.naive_utc())
            .ok_or_else(|| VmError::new("instant out of range".into()))
    }

    /// The instant of a date and time in UTC.
    fn of_utc(at: NaiveDateTime) -> Result<Value, VmError> {
        at.and_utc()
            .timestamp_nanos_opt()
            .map(make_instant)
            .ok_or_else(|| VmError::new("datetime out of range for nanosecond epoch".into()))
    }
}

/// A `Duration` argument: its nanoseconds.
#[derive(Clone, Copy)]
pub(crate) struct Duration(pub(crate) i64);

impl<'a> Arg<'a> for Duration {
    fn take(value: &'a Value) -> Option<Self> {
        int(record(value, ty::DURATION)?, "ns").map(Duration)
    }
}

/// The duration of `n` units of `per_unit` nanoseconds, for `name`.
fn duration_of(name: &str, n: i64, per_unit: i64) -> Result<Value, VmError> {
    let ns = n.checked_mul(per_unit).ok_or_else(|| {
        VmError::new(format!(
            "time arithmetic overflow: {name}({n}) exceeds i64 nanoseconds"
        ))
    })?;
    Ok(make_duration(ns))
}

/// `Ok(value)`.
fn ok(value: Value) -> Value {
    Value::variant(bv::OK, vec![value])
}

/// The type a pattern is of: what its format writes and its parse
/// reads.
#[derive(Clone, Copy)]
enum Of {
    Date,
    DateTime,
}

impl Of {
    fn name(self) -> &'static str {
        match self {
            Of::Date => "Date",
            Of::DateTime => "DateTime",
        }
    }
}

/// A moment that has every field, for asking chrono what a pattern
/// takes.
fn moment() -> Option<NaiveDateTime> {
    NaiveDate::from_ymd_opt(2001, 2, 3).and_then(|date| date.and_hms_milli_opt(16, 5, 6, 789))
}

/// Whether `pattern` is a pattern of the type `of`: every specifier in
/// it is one chrono knows, and one a value of the type has. `Err` with
/// the reason: an unknown specifier, one of a time of day for a `Date`,
/// one of a time zone (no value of silt has one).
///
/// This is the one answer for both directions: a format and its parse
/// take the same specifiers. It is a list of what a type TAKES, and the
/// list is chrono's own: a type takes a specifier if a value of it
/// writes that specifier. So nothing reaches chrono's formatter that it
/// cannot write, and a specifier chrono comes to know later is refused
/// or taken by the same question. (Chrono's `to_string` panics on a
/// specifier the value cannot write; `write!` gives the error.)
fn fits(pattern: &str, of: Of) -> Result<(), String> {
    use chrono::format::{Item, StrftimeItems};
    use std::fmt::Write;

    let invalid = || format!("invalid format specifier in '{pattern}'");
    let Some(moment) = moment() else {
        return Err(invalid());
    };
    fn writes(shown: impl std::fmt::Display) -> bool {
        write!(String::new(), "{shown}").is_ok()
    }
    for item in StrftimeItems::new(pattern) {
        if matches!(item, Item::Error) {
            return Err(invalid());
        }
        let one = || std::iter::once(&item);
        // What a date writes, every value has.
        if writes(moment.date().format_with_items(one())) {
            continue;
        }
        let of_a_time = writes(moment.format_with_items(one()));
        let of_a_zone = !of_a_time && writes(moment.and_utc().format_with_items(one()));
        let what = of.name();
        return Err(match (of, of_a_time, of_a_zone) {
            (Of::DateTime, true, _) => continue,
            (Of::Date, true, _) => {
                format!("a specifier of a time of day in '{pattern}' is not valid for a {what}")
            }
            (_, false, true) => format!(
                "a specifier of a time zone in '{pattern}' is not valid for a {what}: \
                 a {what} has no time zone"
            ),
            // (One that chrono reads and does not write.)
            (_, false, false) => invalid(),
        });
    }
    Ok(())
}

/// Whether the fields of `pattern`, which [`fits`] the type, are enough
/// to read a value of it: `Err` with the reason for a pattern by which
/// no text is such a value (no time of day for a `DateTime`, no date,
/// nothing at all). That is the pattern's fault and is said of the
/// pattern alone, before any text is looked at. Only a parse asks: a
/// format writes the fields it is given, however few.
///
/// Which fields are enough is chrono's to say (a year with a month
/// and a day, or with a day of the year, or a week and a weekday; a
/// 12-hour clock only with its half of the day; a timestamp alone),
/// so it is asked: the pattern writes a moment that has every field,
/// and reads its own text back. Only "not enough" counts; a pattern
/// that cannot read what it wrote for another reason is left to the
/// text.
fn enough(pattern: &str, of: Of) -> Result<(), String> {
    use chrono::format::ParseErrorKind;
    use std::fmt::Write;

    let what = of.name();
    if pattern.is_empty() {
        return Err(format!(
            "the pattern is empty: no text read by it is a {what}"
        ));
    }
    let Some(moment) = moment() else {
        return Ok(());
    };
    let mut text = String::new();
    let written = match of {
        Of::Date => write!(text, "{}", moment.date().format(pattern)),
        Of::DateTime => write!(text, "{}", moment.format(pattern)),
    };
    if written.is_err() {
        return Ok(());
    }
    let lacks = |result: Option<chrono::ParseError>| {
        result.is_some_and(|error| error.kind() == ParseErrorKind::NotEnough)
    };
    let no_date = lacks(NaiveDate::parse_from_str(&text, pattern).err());
    match of {
        Of::Date if no_date => Err(format!(
            "the fields of '{pattern}' do not make a date: no text read by it is a {what}"
        )),
        Of::Date => Ok(()),
        Of::DateTime => {
            if !lacks(NaiveDateTime::parse_from_str(&text, pattern).err()) {
                return Ok(());
            }
            let missing = match no_date {
                true => "a date",
                false => "a time of day",
            };
            Err(format!(
                "the fields of '{pattern}' do not make {missing}: no text read by it is a {what}"
            ))
        }
    }
}

/// `Ok(text)` of what `shown` writes, a value formatted by a pattern
/// that [`fits`] it.
fn written(shown: impl std::fmt::Display, pattern: &str) -> Value {
    use std::fmt::Write;

    let mut text = String::new();
    match write!(text, "{shown}") {
        Ok(()) => ok(Value::String(text.into())),
        Err(_) => invalid_format_err(format!("invalid format specifier in '{pattern}'")),
    }
}

// ── The functions ───────────────────────────────────────────────────

builtins! {
    fn sleep(vm, duration: Duration) -> Result<Step, VmError> {
        let Ok(nanos @ 1..) = u64::try_from(duration.0) else {
            return Ok(Step::Done(Value::Unit));
        };
        let duration = std::time::Duration::from_nanos(nanos);
        // The program's own thread asks the clock to sleep (`Clock::sleep`).
        if !vm.spawned {
            vm.runtime.io.sleep(duration);
            return Ok(Step::Done(Value::Unit));
        }
        // A task waits for the clock to reach the end of the sleep.
        let end =
            vm.runtime.io.deadline_after(duration).ok_or_else(|| {
                VmError::new("cannot start a timer: the duration is out of range".into())
            })?;
        if let Some(failure) = vm.runtime.io.clock_failure() {
            return Err(VmError::new(failure));
        }
        let wait = Wait::new(Vec::new()).deadline(Some(end));
        Ok(vm.park("time.sleep", wait, |_, _| Ok(Step::Done(Value::Unit))))
    }

    fn now(vm) -> Result<Value, VmError> {
        // Millisecond resolution.
        let epoch_ns = i64::try_from(vm.runtime.io.now().as_millis())
            .ok()
            .and_then(|ms| ms.checked_mul(1_000_000))
            .ok_or_else(|| {
                VmError::new("time.now: epoch milliseconds * 1_000_000 overflows i64".into())
            })?;
        Ok(make_instant(epoch_ns))
    }

    fn today(vm) -> Result<Value, VmError> {
        let out_of_range = || VmError::new("time.today: date out of range".into());
        let now = vm.runtime.io.now();
        let secs = i64::try_from(now.as_secs()).map_err(|_| out_of_range())?;
        // The date of the host clock's time: in the local time zone
        // where the build knows it, in UTC otherwise.
        #[cfg(feature = "local-clock")]
        {
            use chrono::TimeZone;
            let local = chrono::Local
                .timestamp_opt(secs, now.subsec_nanos())
                .single()
                .ok_or_else(out_of_range)?;
            Ok(make_date(local.date_naive()))
        }
        #[cfg(not(feature = "local-clock"))]
        {
            let (y, m, d) = civil_from_epoch_secs(secs);
            let date = NaiveDate::from_ymd_opt(y, m, d).ok_or_else(out_of_range)?;
            Ok(make_date(date))
        }
    }

    fn date(year: i64, month: i64, day: i64) -> Result<Value, VmError> {
        let y = as_i32("time.date", "year", year)?;
        let m = as_u32("time.date", "month", month)?;
        let d = as_u32("time.date", "day", day)?;
        Ok(match NaiveDate::from_ymd_opt(y, m, d) {
            Some(date) => ok(make_date(date)),
            None => time_out_of_range_err(format!("invalid date: {year}-{month}-{day}")),
        })
    }

    fn time(hour: i64, min: i64, sec: i64) -> Result<Value, VmError> {
        let h = as_u32("time.time", "hour", hour)?;
        let m = as_u32("time.time", "minute", min)?;
        let s = as_u32("time.time", "second", sec)?;
        Ok(match NaiveTime::from_hms_opt(h, m, s) {
            Some(time) => ok(make_time(time)),
            None => time_out_of_range_err(format!("invalid time: {hour}:{min}:{sec}")),
        })
    }

    fn datetime(date: Date, time: Time) -> Result<Value, VmError> {
        Ok(make_datetime(NaiveDateTime::new(date.naive()?, time.naive()?)))
    }

    fn to_datetime(instant: Instant, offset_minutes: i64) -> Result<Value, VmError> {
        let utc = instant.utc()?;
        // `chrono::Duration::minutes(i64)` panics when the value is
        // outside a roughly `i64::MAX / 60000` window. Use the
        // fallible constructor so a pathological offset surfaces as
        // a clean VmError rather than a builtin panic.
        let offset = chrono::Duration::try_minutes(offset_minutes).ok_or_else(|| {
            VmError::new(format!(
                "time.to_datetime: offset {offset_minutes} minutes out of range"
            ))
        })?;
        // Even a valid chrono::Duration can still push the naive
        // datetime past chrono's ±262143-year range; `NaiveDateTime
        // + Duration` panics on overflow, so use the checked form.
        // (In practice `Instant.epoch_ns` is an i64, so the combined
        // epoch-ns + i32-minute-offset input cannot reach chrono's
        // ±262143-year boundary from Silt user code — we keep the
        // check as defence in depth against future Instant
        // widenings or chrono internal assumption changes.)
        let local = utc.checked_add_signed(offset).ok_or_else(|| {
            VmError::new("time.to_datetime: datetime + offset out of range".into())
        })?;
        Ok(make_datetime(local))
    }

    fn to_instant(datetime: DateTime, offset_minutes: i64) -> Result<Value, VmError> {
        let local = datetime.naive()?;
        let offset = chrono::Duration::try_minutes(offset_minutes).ok_or_else(|| {
            VmError::new(format!(
                "time.to_instant: offset {offset_minutes} minutes out of range"
            ))
        })?;
        // `NaiveDateTime - Duration` panics on overflow (chrono's
        // valid range is ±262143 years). Use the checked form so a
        // pathological offset/datetime combination surfaces as a
        // clean VmError.
        let utc = local.checked_sub_signed(offset).ok_or_else(|| {
            VmError::new("time.to_instant: datetime - offset out of range".into())
        })?;
        Instant::of_utc(utc)
    }

    // A pattern is text like any other: one that is no pattern of the
    // type (an unknown specifier, one of a time of day for a date, one
    // of a time zone) is `Err(InvalidFormat(why))`, for a format and
    // for its parse alike ([`fits`]).
    fn format(datetime: DateTime, pattern: &str) -> Result<Value, VmError> {
        let datetime = datetime.naive()?;
        Ok(match fits(pattern, Of::DateTime) {
            Ok(()) => written(datetime.format(pattern), pattern),
            Err(why) => invalid_format_err(why),
        })
    }

    fn format_date(date: Date, pattern: &str) -> Result<Value, VmError> {
        let date = date.naive()?;
        Ok(match fits(pattern, Of::Date) {
            Ok(()) => written(date.format(pattern), pattern),
            Err(why) => invalid_format_err(why),
        })
    }

    // A parse also needs fields enough for the value ([`enough`]): a
    // pattern without them is `Err(InvalidFormat(why))` whatever the
    // text. `ParseFormat` is a text that does not match a pattern that
    // could give the value.
    fn parse(s: &str, pattern: &str) -> Value {
        if let Err(why) = fits(pattern, Of::DateTime).and_then(|()| enough(pattern, Of::DateTime)) {
            return invalid_format_err(why);
        }
        match NaiveDateTime::parse_from_str(s, pattern) {
            Ok(datetime) => ok(make_datetime(datetime)),
            Err(e) => time_parse_err(e),
        }
    }

    fn parse_date(s: &str, pattern: &str) -> Value {
        if let Err(why) = fits(pattern, Of::Date).and_then(|()| enough(pattern, Of::Date)) {
            return invalid_format_err(why);
        }
        // Parse as NaiveDateTime with a dummy time appended, then extract the date.
        let padded = format!("{s}T00:00:00");
        let padded_fmt = format!("{pattern}T%H:%M:%S");
        match NaiveDateTime::parse_from_str(&padded, &padded_fmt) {
            Ok(datetime) => ok(make_date(datetime.date())),
            // Fallback: try direct NaiveDate parse (works on native)
            Err(_) => match NaiveDate::parse_from_str(s, pattern) {
                Ok(date) => ok(make_date(date)),
                Err(e) => time_parse_err(e),
            },
        }
    }

    fn add_days(date: Date, days: i64) -> Result<Value, VmError> {
        let date = date.naive()?;
        // chrono::Duration::days panics when `days * 86_400_000` overflows
        // i64 milliseconds (i.e. for inputs beyond roughly ±106_751_991 days).
        // We reject such values up front so the panic can never escape
        // the builtin. Further, `NaiveDate::checked_add_signed` returns
        // None for out-of-range dates (chrono's valid range spans ±262_143
        // years).  In both failure modes we produce a clean VmError.
        const MAX_DAYS: i64 = 100_000_000; // safely below chrono's panic threshold
        if days.unsigned_abs() > MAX_DAYS as u64 {
            return Err(VmError::new(format!(
                "time arithmetic overflow: time.add_days days={days} out of range"
            )));
        }
        let delta = chrono::Duration::days(days);
        let result = date.checked_add_signed(delta).ok_or_else(|| {
            VmError::new(format!(
                "time arithmetic overflow: time.add_days result out of range for {date} + {days} days"
            ))
        })?;
        Ok(make_date(result))
    }

    fn add_months(date: Date, months: i64) -> Result<Value, VmError> {
        let date = date.naive()?;
        // Calculate target year and month using checked arithmetic so
        // extreme `months` inputs (e.g. i64::MAX) don't panic in debug
        // builds or silently wrap in release builds.
        let total_months = (date.year() as i64)
            .checked_mul(12)
            .and_then(|y| y.checked_add(date.month() as i64 - 1))
            .and_then(|m| m.checked_add(months))
            .ok_or_else(|| {
                VmError::new(format!(
                    "time arithmetic overflow: time.add_months months={months} out of range"
                ))
            })?;
        let target_year = total_months.div_euclid(12);
        let target_year = i32::try_from(target_year).map_err(|_| {
            VmError::new(format!(
                "time arithmetic overflow: time.add_months target year {target_year} out of i32 range"
            ))
        })?;
        let target_month = (total_months.rem_euclid(12) + 1) as u32;
        // Clamp day to last valid day of target month
        let target_day = date.day().min(days_in(target_year, target_month));
        let result = NaiveDate::from_ymd_opt(target_year, target_month, target_day)
            .ok_or_else(|| {
                VmError::new(format!(
                    "add_months overflow: {target_year}-{target_month}-{target_day}"
                ))
            })?;
        Ok(make_date(result))
    }

    fn add(instant: Instant, duration: Duration) -> Result<Value, VmError> {
        let result = instant.0.checked_add(duration.0).ok_or_else(|| {
            VmError::new("time arithmetic overflow: time.add instant + duration".into())
        })?;
        Ok(make_instant(result))
    }

    fn since(from: Instant, to: Instant) -> Result<Value, VmError> {
        let result = to.0.checked_sub(from.0).ok_or_else(|| {
            VmError::new("time arithmetic overflow: time.since to - from".into())
        })?;
        Ok(make_duration(result))
    }

    fn hours(n: i64) -> Result<Value, VmError> {
        duration_of("time.hours", n, 3_600_000_000_000)
    }

    fn minutes(n: i64) -> Result<Value, VmError> {
        duration_of("time.minutes", n, 60_000_000_000)
    }

    fn seconds(n: i64) -> Result<Value, VmError> {
        duration_of("time.seconds", n, 1_000_000_000)
    }

    fn ms(n: i64) -> Result<Value, VmError> {
        duration_of("time.ms", n, 1_000_000)
    }

    fn micros(n: i64) -> Result<Value, VmError> {
        duration_of("time.micros", n, 1_000)
    }

    fn nanos(n: i64) -> Result<Value, VmError> {
        duration_of("time.nanos", n, 1)
    }

    fn weekday(date: Date) -> Result<Value, VmError> {
        let day = match date.naive()?.weekday() {
            Weekday::Mon => bv::MONDAY,
            Weekday::Tue => bv::TUESDAY,
            Weekday::Wed => bv::WEDNESDAY,
            Weekday::Thu => bv::THURSDAY,
            Weekday::Fri => bv::FRIDAY,
            Weekday::Sat => bv::SATURDAY,
            Weekday::Sun => bv::SUNDAY,
        };
        Ok(Value::variant(day, vec![]))
    }

    fn days_between(from: Date, to: Date) -> Result<i64, VmError> {
        let (from, to) = (from.naive()?, to.naive()?);
        Ok(to.signed_duration_since(from).num_days())
    }

    fn days_in_month(year: i64, month: i64) -> Result<i64, VmError> {
        let y = as_i32("time.days_in_month", "year", year)?;
        let m = as_u32("time.days_in_month", "month", month)?;
        // A month outside 1..=12 is refused, as `time.date` and
        // `time.time` refuse each part that is none.
        if !(1..=12).contains(&m) {
            return Err(VmError::new(format!(
                "time.days_in_month: month {month} out of range (must be 1..=12)"
            )));
        }
        Ok(days_in(y, m) as i64)
    }

    fn is_leap_year(year: i64) -> Result<bool, VmError> {
        Ok(leap(as_i32("time.is_leap_year", "year", year)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::{Step, Vm};

    /// What a parse says of `pattern` before it looks at a text.
    fn reads(pattern: &str, of: Of) -> Result<(), String> {
        fits(pattern, of).and_then(|()| enough(pattern, of))
    }

    /// The value a builtin of the module gives for `args`.
    fn call(builtin: crate::builtins::typed::TypedCall, args: &[Value]) -> Value {
        let mut vm = Vm::new(crate::vm::HostIo::process());
        match builtin(&mut vm, args).expect("the arguments are the builtin's") {
            Ok(Step::Done(value)) => value,
            _ => panic!("the builtin gives a value"),
        }
    }

    fn text(s: &str) -> Value {
        Value::String(s.into())
    }

    /// Whether `result` is `Err(InvalidFormat(_))`.
    fn invalid_format(result: &Value) -> bool {
        let Value::Variant(result) = result else {
            return false;
        };
        result.is(bv::ERR)
            && matches!(result.fields(), [Value::Variant(error)] if error.is(bv::TIME_INVALID_FORMAT))
    }

    /// The text of `result` if it is `Ok` of one.
    fn ok_text(result: &Value) -> Option<String> {
        match result {
            Value::Variant(result) if result.is(bv::OK) => match result.fields() {
                [Value::String(text)] => Some(text.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    /// The specifiers of a date: every value has what they name.
    const OF_A_DATE: [&str; 30] = [
        "%Y", "%C", "%y", "%q", "%m", "%b", "%B", "%h", "%d", "%e", "%a", "%A", "%w", "%u", "%U",
        "%W", "%G", "%g", "%V", "%j", "%D", "%x", "%F", "%v", "%t", "%n", "%%", "%-d", "%_d",
        "%0d",
    ];
    /// The specifiers of a time of day: a `DateTime` has what they
    /// name, a `Date` has not.
    const OF_A_TIME: [&str; 22] = [
        "%H", "%k", "%I", "%l", "%P", "%p", "%M", "%S", "%f", "%.f", "%.3f", "%.6f", "%.9f", "%3f",
        "%6f", "%9f", "%R", "%T", "%X", "%r", "%c", "%s",
    ];
    /// The specifiers of a time zone, `%#z` among them, which chrono
    /// reads and cannot write: no value of silt has one.
    const OF_A_ZONE: [&str; 8] = ["%z", "%:z", "%::z", "%:::z", "%#z", "%Z", "%+", "%:::z %Z"];
    /// What chrono does not know.
    const UNKNOWN: [&str; 7] = ["%Q", "%E", "%O", "%L", "%N", "%J", "%"];

    /// A format and its parse take the same specifiers: for every
    /// specifier chrono knows and some it does not, behind a pattern
    /// with fields enough for the value, both say `InvalidFormat` or
    /// neither does, and a type takes exactly the specifiers of what it
    /// has. No specifier makes chrono's formatter panic (`%3f` for a
    /// date and `%#z` for either type once did).
    #[test]
    fn a_format_and_its_parse_take_the_same_specifiers() {
        let moment = moment().expect("a moment");
        let types: [(
            Of,
            &str,
            Value,
            crate::builtins::typed::TypedCall,
            crate::builtins::typed::TypedCall,
        ); 2] = [
            (
                Of::Date,
                "%Y-%m-%d",
                make_date(moment.date()),
                format_date,
                parse_date,
            ),
            (
                Of::DateTime,
                "%Y-%m-%d %H:%M:%S",
                make_datetime(moment),
                format,
                parse,
            ),
        ];
        for (of, enough, value, format, parse) in types {
            let takes_times = matches!(of, Of::DateTime);
            let groups = [
                (&OF_A_DATE[..], true),
                (&OF_A_TIME[..], takes_times),
                (&OF_A_ZONE[..], false),
                (&UNKNOWN[..], false),
            ];
            for (specifiers, taken) in groups {
                for specifier in specifiers {
                    let pattern = format!("{enough} {specifier}");
                    let formatted = call(format, &[value.clone(), text(&pattern)]);
                    let written = ok_text(&formatted);
                    let read = written.clone().unwrap_or_else(|| "x".to_string());
                    let parsed = call(parse, &[text(&read), text(&pattern)]);
                    let what = format!("'{pattern}' for a {}", of.name());
                    assert_eq!(
                        written.is_some(),
                        taken,
                        "the format of {what}: {formatted}"
                    );
                    assert_eq!(invalid_format(&formatted), !taken, "{what}: {formatted}");
                    assert_eq!(
                        invalid_format(&parsed),
                        !taken,
                        "the parse of {what}: {parsed}"
                    );
                }
            }
        }
    }

    /// The one difference between a format and its parse: a pattern
    /// whose specifiers the type takes, with fields too few to read a
    /// value. The format writes what it is given; the parse says that
    /// no text read by the pattern is the value.
    #[test]
    fn a_pattern_with_too_few_fields_is_written_and_not_read() {
        let moment = moment().expect("a moment");
        let date = make_date(moment.date());
        let datetime = make_datetime(moment);
        for (pattern, written) in [("%Y", "2001"), ("%m.%Y", "02.2001"), ("", "")] {
            let formatted = call(format_date, &[date.clone(), text(pattern)]);
            assert_eq!(ok_text(&formatted).as_deref(), Some(written), "{pattern}");
            let parsed = call(parse_date, &[text(written), text(pattern)]);
            assert!(invalid_format(&parsed), "'{pattern}': {parsed}");
        }
        for (pattern, written) in [("%Y-%m-%d", "2001-02-03"), ("%H:%M", "16:05"), ("", "")] {
            let formatted = call(format, &[datetime.clone(), text(pattern)]);
            assert_eq!(ok_text(&formatted).as_deref(), Some(written), "{pattern}");
            let parsed = call(parse, &[text(written), text(pattern)]);
            assert!(invalid_format(&parsed), "'{pattern}': {parsed}");
        }
    }

    /// A pattern is refused before any text is read exactly when no
    /// text read by it can be the value: for each pattern here that is
    /// let through, a text is given that it reads; for each that is
    /// refused, chrono reads no text of it, whatever it holds.
    #[test]
    fn a_pattern_is_refused_when_no_text_read_by_it_is_the_value() {
        let datetimes = [
            ("%Y-%m-%d %H:%M:%S", "2024-02-29 13:05:09"),
            ("%Y-%m-%dT%H:%M:%S%.f", "2024-02-29T13:05:09.25"),
            ("%Y%m%d%H%M%S", "20240229130509"),
            ("%F %T", "2024-02-29 13:05:09"),
            ("%D %R", "02/29/24 13:05"),
            ("%d/%m/%y %I:%M %p", "29/02/24 01:05 PM"),
            ("%Y-%j %H:%M", "2024-060 13:05"),
            ("%G-W%V-%u %H:%M:%S", "2024-W09-4 13:05:09"),
            ("%s", "1709211909"),
            ("%c", "Thu Feb 29 13:05:09 2024"),
            (
                "on %A, %d %B %Y at %H:%M",
                "on Thursday, 29 February 2024 at 13:05",
            ),
        ];
        for (pattern, text) in datetimes {
            assert_eq!(reads(pattern, Of::DateTime), Ok(()), "{pattern}");
            assert!(
                NaiveDateTime::parse_from_str(text, pattern).is_ok(),
                "{pattern} reads {text}"
            );
        }
        let dates = [
            ("%d.%m.%Y", "29.02.2024"),
            ("%F", "2024-02-29"),
            ("%Y-%j", "2024-060"),
            ("%b %e %Y", "Feb 29 2024"),
            ("%x", "02/29/24"),
            ("%G-W%V-%a", "2024-W09-Thu"),
        ];
        for (pattern, text) in dates {
            assert_eq!(reads(pattern, Of::Date), Ok(()), "{pattern}");
            assert!(
                NaiveDate::parse_from_str(text, pattern).is_ok(),
                "{pattern} reads {text}"
            );
        }
        // No time of day, no date, half a date, a 12-hour clock without
        // its half of the day, a year of two digits' century alone,
        // nothing but text, nothing.
        let not_enough = chrono::format::ParseErrorKind::NotEnough;
        let no_datetime = [
            ("%Y-%m-%d", "2024-02-29", "a time of day"),
            ("%H:%M:%S", "13:05:09", "a date"),
            ("%Y-%m %H:%M", "2024-02 13:05", "a date"),
            ("%Y-%m-%d %I:%M", "2024-02-29 01:05", "a time of day"),
            ("%Y-%m-%d %H", "2024-02-29 13", "a time of day"),
            ("at noon", "at noon", "a date"),
        ];
        for (pattern, text, missing) in no_datetime {
            let why = reads(pattern, Of::DateTime).expect_err(pattern);
            assert!(why.contains(missing) && why.contains(pattern), "{why}");
            let read = NaiveDateTime::parse_from_str(text, pattern);
            assert_eq!(read.map_err(|e| e.kind()), Err(not_enough), "{pattern}");
        }
        for (pattern, text) in [("%Y-%m", "2024-02"), ("%Y", "2024"), ("%C", "20")] {
            let why = reads(pattern, Of::Date).expect_err(pattern);
            assert!(why.contains("a date") && why.contains(pattern), "{why}");
            let read = NaiveDate::parse_from_str(text, pattern);
            assert_eq!(read.map_err(|e| e.kind()), Err(not_enough), "{pattern}");
        }
        for of in [Of::DateTime, Of::Date] {
            let why = reads("", of).expect_err("the empty pattern");
            assert!(why.starts_with("the pattern is empty"), "{why}");
            let why = reads("%Y-%Q", of).expect_err("an unknown specifier");
            assert_eq!(why, "invalid format specifier in '%Y-%Q'");
        }
    }

    /// The records the module builds have the fields of their types,
    /// in the types' order (`Value::builtin_record` says so in a debug
    /// build), and are read back as what they were built from.
    #[test]
    fn the_records_of_the_module_have_their_types_fields() {
        let day = NaiveDate::from_ymd_opt(2024, 3, 9).expect("a date");
        let clock = NaiveTime::from_hms_nano_opt(7, 30, 5, 42).expect("a time");
        let date = make_date(day);
        let time = make_time(clock);
        let both = make_datetime(day.and_time(clock));
        assert_eq!(date.to_string(), "2024-03-09");
        assert_eq!(time.to_string(), "07:30:05.000000042");
        assert_eq!(both.to_string(), "2024-03-09T07:30:05.000000042");
        assert_eq!(make_instant(7).to_string(), "Instant {epoch_ns: 7}");
        assert_eq!(make_duration(1_500_000_000).to_string(), "1.500s");

        let read = Date::take(&date).expect("a Date");
        assert_eq!((read.year, read.month, read.day), (2024, 3, 9));
        let read = Time::take(&time).expect("a Time");
        assert_eq!(
            (read.hour, read.minute, read.second, read.ns),
            (7, 30, 5, 42)
        );
        let Value::Record(record) = &both else {
            panic!("a record");
        };
        assert_eq!(record.fields(), [date, time]);
        // (A record of another type is none of these.)
        assert!(Date::take(&both).is_none());
    }
}
