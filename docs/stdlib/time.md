---
title: "time"
section: "Standard Library"
order: 14
---

# time

Dates, times, instants, durations, formatting, parsing, and arithmetic. All values are immutable. Nanosecond precision throughout.

## Types

```silt
type Instant {
  epoch_ns: Int,
} -- point on the UTC timeline (ns since Unix epoch)

type Date {
  year: Int,
  month: Int,
  day: Int,
} -- calendar date, no time or zone

type Time {
  hour: Int,
  minute: Int,
  second: Int,
  ns: Int,
} -- wall clock time, no date or zone

type DateTime {
  date: Date,
  time: Time,
} -- date + time, no zone

type Duration {
  ns: Int,
} -- fixed elapsed time in nanoseconds

type Weekday {
  Monday,
  Tuesday,
  Wednesday,
  Thursday,
  Friday,
  Saturday,
  Sunday,
}
```

`Date`, `Time`, and `DateTime` display as ISO 8601 in string interpolation.
`Duration` displays in human-readable form (`2h30m15s`, `500ms`, `42ns`).
Comparison operators (`<`, `>`, `==`) work correctly on all time types.

## Errors

`time.date`, `time.time`, `time.parse`, `time.parse_date`, `time.format` and
`time.format_date` return `Result(T, TimeError)`. The enum is intentionally
small — calendar validation, a text that does not match its pattern and a
pattern that is no format are the structural failure modes:

| Variant | Fields | Meaning |
|---------|--------|---------|
| `ParseFormat(msg)` | `String` | the text did not match a pattern that could have given the value |
| `OutOfRange(msg)` | `String` | field out of valid range (e.g. `month=13`) |
| `InvalidFormat(msg)` | `String` | the pattern is at fault, whatever the value or the text: an unknown specifier, or one the type has nothing for (a time of day for a `Date`, a time zone), in a format and in its parse alike; for a parse also fields that are not enough for the value (no time of day for a `DateTime`, no date, an empty pattern) |
| `Other(msg)` | `String` | unclassified failure |

`TimeError` implements the built-in `Error` trait, so `e.message()`
yields a rendered string when variant branching isn't needed.

## Summary

| Name | Signature | Description |
|------|-----------|-------------|
| `now` | `() -> Instant` | Current UTC time as nanosecond epoch |
| `today` | `() -> Date` | Current local date |
| `date` | `(Int, Int, Int) -> Result(Date, TimeError)` | Validated date from year, month, day |
| `time` | `(Int, Int, Int) -> Result(Time, TimeError)` | Validated time from hour, min, sec (ns=0) |
| `datetime` | `(Date, Time) -> DateTime` | Combine date and time (infallible) |
| `to_datetime` | `(Instant, Int) -> DateTime` | Convert instant to local datetime with UTC offset in minutes |
| `to_instant` | `(DateTime, Int) -> Instant` | Convert local datetime to instant with UTC offset in minutes |
| `format` | `(DateTime, String) -> Result(String, TimeError)` | Format datetime with strftime pattern |
| `format_date` | `(Date, String) -> Result(String, TimeError)` | Format date with strftime pattern |
| `parse` | `(String, String) -> Result(DateTime, TimeError)` | Parse string into datetime with strftime pattern |
| `parse_date` | `(String, String) -> Result(Date, TimeError)` | Parse string into date with strftime pattern |
| `add_days` | `(Date, Int) -> Date` | Add/subtract days from a date |
| `add_months` | `(Date, Int) -> Date` | Add/subtract months, clamping to end-of-month |
| `add` | `(Instant, Duration) -> Instant` | Add duration to an instant |
| `since` | `(Instant, Instant) -> Duration` | Signed duration between two instants (to − from) |
| `hours` | `(Int) -> Duration` | Create duration from hours |
| `minutes` | `(Int) -> Duration` | Create duration from minutes |
| `seconds` | `(Int) -> Duration` | Create duration from seconds |
| `ms` | `(Int) -> Duration` | Create duration from milliseconds |
| `micros` | `(Int) -> Duration` | Create duration from microseconds |
| `nanos` | `(Int) -> Duration` | Create duration from nanoseconds |
| `weekday` | `(Date) -> Weekday` | Day of the week |
| `days_between` | `(Date, Date) -> Int` | Signed number of days between two dates |
| `days_in_month` | `(Int, Int) -> Int` | Days in month for given year and month |
| `is_leap_year` | `(Int) -> Bool` | Check if a year is a leap year |
| `sleep` | `(Duration) -> ()` | Fiber-aware sleep |


## `time.now`

```
time.now() -> Instant
```

Returns the current UTC time as nanoseconds since the Unix epoch (1970-01-01T00:00:00Z).

```silt
import time

fn main() {
  let t = time.now()
  println(t.epoch_ns) -- 1775501213453369259
}
```


## `time.today`

```
time.today() -> Date
```

Returns the current date in the system's local timezone.

```silt
import time

fn main() {
  println(time.today()) -- 2026-04-06
}
```


## `time.date`

```
time.date(year: Int, month: Int, day: Int) -> Result(Date, TimeError)
```

Creates a validated `Date`. Returns `Err(OutOfRange(msg))` for every
part that makes no date, a negative one or one too large for any date
included.

```silt
import time

fn main() {
  println(time.date(2024, 3, 15)) -- Ok(2024-03-15)
  println(time.date(2024, 2, 29)) -- Ok(2024-02-29)  (leap year)
  println(time.date(2024, 13, 1)) -- Err(time out of range: invalid date: 2024-13-1)
}
```


## `time.time`

```
time.time(hour: Int, min: Int, sec: Int) -> Result(Time, TimeError)
```

Creates a validated `Time` with `ns` set to 0. Returns `Err(OutOfRange(msg))`
for every part that makes no time of day, a negative one included.

```silt
import time

fn main() {
  println(time.time(14, 30, 0)) -- Ok(14:30:00)
  println(time.time(25, 0, 0)) -- Err(time out of range: invalid time: 25:0:0)
}
```


## `time.datetime`

```
time.datetime(date: Date, time: Time) -> DateTime
```

Combines a `Date` and `Time` into a `DateTime`. Infallible since both inputs are already validated.

```silt
import time

fn main() -> Result(Unit, time.TimeError) {
  let d = time.date(2024, 6, 15)?
  let t = time.time(9, 30, 0)?
  println(time.datetime(d, t)) -- 2024-06-15T09:30:00
  Ok(())
}
```


## `time.to_datetime`

```
time.to_datetime(instant: Instant, offset_minutes: Int) -> DateTime
```

Converts an `Instant` to a `DateTime` by applying a UTC offset in minutes.

```silt
import time

fn main() {
  let now = time.now()
  let tokyo = now |> time.to_datetime(540) -- UTC+9:00
  let india = now |> time.to_datetime(330) -- UTC+5:30
  println(tokyo)
  println(india)
}
```


## `time.to_instant`

```
time.to_instant(datetime: DateTime, offset_minutes: Int) -> Instant
```

Converts a local `DateTime` to an `Instant` by subtracting the UTC offset.

```silt
import time

fn main() -> Result(Unit, time.TimeError) {
  let dt = time.datetime(time.date(2024, 1, 1)?, time.time(0, 0, 0)?)
  let instant = time.to_instant(dt, 0)
  println(instant.epoch_ns)
  Ok(())
}
```


## `time.format`

```
time.format(datetime: DateTime, pattern: String) -> Result(String, TimeError)
```

Formats a `DateTime` using a strftime pattern. The pattern may hold every
specifier of a date or of a time of day (`%Y %m %d %H %M %S %f %A %a %B %b`
and `%%` among them): whatever a `DateTime` has a value for.
A pattern is text like any other: one that is no pattern of a `DateTime`
(an unknown specifier such as `%Q`, or a time zone specifier such as `%z`: a
`DateTime` has no time zone) is `Err(InvalidFormat(why))`, not a failure of
the program. `time.parse` refuses the same patterns.

```silt
import time

fn main() -> Result(Unit, time.TimeError) {
  let dt = time.datetime(time.date(2024, 12, 25)?, time.time(18, 0, 0)?)
  println(time.format(dt, "%A, %B %d, %Y at %H:%M")?)
  -- Wednesday, December 25, 2024 at 18:00
  println(time.format(dt, "%Q"))
  -- Err(time format error: invalid format specifier in '%Q')
  Ok(())
}
```


## `time.format_date`

```
time.format_date(date: Date, pattern: String) -> Result(String, TimeError)
```

Formats a `Date` using strftime patterns. A pattern that is no pattern of a
date is `Err(InvalidFormat(why))`: an unknown specifier, or one of a time of
day (`%H`) or a time zone, which a date has not. `time.parse_date` refuses the
same patterns.

```silt
import time

fn main() -> Result(Unit, time.TimeError) {
  let d = time.date(2024, 6, 15)?
  println(time.format_date(d, "%d/%m/%Y")?) -- 15/06/2024
  Ok(())
}
```


## `time.parse`

```
time.parse(s: String, pattern: String) -> Result(DateTime, TimeError)
```

Parses a string into a `DateTime` using a strftime pattern.

A pattern that `time.format` refuses, `time.parse` refuses too: an unknown
specifier, or one of a time zone (`%z`), is `Err(InvalidFormat(why))`; no
offset is read and dropped. The pattern also has to give a date and a time
of day. One that cannot, whatever the text, is `Err(InvalidFormat(why))`: a
pattern without a time of day (`"%Y-%m-%d"`: use `time.parse_date`),
without a date, with a 12-hour clock and no `%p`, or an empty one. That
is decided from the pattern alone. A text that does not match a pattern
that could give a `DateTime` is `Err(ParseFormat(msg))`, and one that
matches with a field that is no such day or time `Err(OutOfRange(msg))`. A
second 60, a leap second, is one:
`time.parse("2016-12-31 23:59:60", "%Y-%m-%d %H:%M:%S")` is
`Err(OutOfRange("invalid datetime '2016-12-31 23:59:60': a leap second (second 60) is not read"))`.

```silt
import time

fn main() {
  let dt = time.parse("2024-07-04 12:00:00", "%Y-%m-%d %H:%M:%S")
  println(dt) -- Ok(2024-07-04T12:00:00)
}
```


## `time.parse_date`

```
time.parse_date(s: String, pattern: String) -> Result(Date, TimeError)
```

Parses a string into a `Date` using a strftime pattern. A pattern that
`time.format_date` refuses, `time.parse_date` refuses too: one with a
specifier of a time of day (`"%Y-%m-%d %H:%M"`: use `time.parse`, no time is
read and dropped) or of a time zone is `Err(InvalidFormat(why))`. So is a
pattern whose fields do not make a date (`"%m.%Y"`, an empty one), whatever
the text, as in `time.parse`.

```silt
import time

fn main() {
  let d = time.parse_date("2024-07-04", "%Y-%m-%d")
  println(d) -- Ok(2024-07-04)
}
```


## `time.add_days`

```
time.add_days(date: Date, days: Int) -> Date
```

Adds (or subtracts, if negative) days from a date.

```silt
import time

fn main() -> Result(Unit, time.TimeError) {
  let d = time.date(2024, 1, 1)?
  println(d |> time.add_days(90)) -- 2024-03-31
  println(d |> time.add_days(-1)) -- 2023-12-31
  Ok(())
}
```


## `time.add_months`

```
time.add_months(date: Date, months: Int) -> Date
```

Adds (or subtracts) months from a date. Clamps to the last valid day of the target month.

```silt
import time

fn main() -> Result(Unit, time.TimeError) {
  let d = time.date(2024, 1, 31)?
  println(d |> time.add_months(1)) -- 2024-02-29 (leap year, clamped)
  println(d |> time.add_months(2)) -- 2024-03-31
  Ok(())
}
```


## `time.add`

```
time.add(instant: Instant, duration: Duration) -> Instant
```

Adds a duration to an instant.

```silt
import time

fn main() {
  let t = time.now()
  let later = t |> time.add(time.hours(2))
  println(time.since(t, later)) -- 2h
}
```


## `time.since`

```
time.since(from: Instant, to: Instant) -> Duration
```

Returns the signed duration from `from` to `to` (computed as `to.epoch_ns − from.epoch_ns`).

```silt
import time

fn main() {
  let start = time.now()
  time.sleep(time.ms(100))
  let elapsed = time.since(start, time.now())
  println(elapsed) -- 100ms
}
```


## `time.hours`, `time.minutes`, `time.seconds`, `time.ms`, `time.micros`, `time.nanos`

```
time.hours(n: Int) -> Duration
time.minutes(n: Int) -> Duration
time.seconds(n: Int) -> Duration
time.ms(n: Int) -> Duration
time.micros(n: Int) -> Duration
time.nanos(n: Int) -> Duration
```

Duration constructor functions. All units return a `Duration` with
nanosecond precision; they differ only in the multiplier applied to
their `Int` argument. `time.nanos` is the raw form (no multiplication).
Overflowing the `Int` range (`i64::MAX` nanoseconds ≈ 292 years) is
surfaced as a runtime error rather than a silent wrap.

```silt
import time

fn main() {
  println(time.hours(1)) -- 1h
  println(time.minutes(30)) -- 30m
  println(time.seconds(5)) -- 5s
  println(time.ms(500)) -- 500ms
  println(time.micros(250)) -- 250us
  println(time.nanos(42)) -- 42ns
}
```


## `time.weekday`

```
time.weekday(date: Date) -> Weekday
```

Returns the day of the week. Pattern-match on the result for exhaustive handling.

```silt
import time

fn main() {
  let day = time.today() |> time.weekday
  match day {
    time.Monday -> println("start of the week")
    time.Friday -> println("almost weekend")
    time.Saturday | time.Sunday -> println("weekend!")
    _ -> println("midweek")
  }
}
```


## `time.days_between`

```
time.days_between(from: Date, to: Date) -> Int
```

Returns the signed number of days between two dates.

```silt
import time

fn main() -> Result(Unit, time.TimeError) {
  let a = time.date(2024, 1, 1)?
  let b = time.date(2024, 12, 31)?
  println(time.days_between(a, b)) -- 365
  Ok(())
}
```


## `time.days_in_month`

```
time.days_in_month(year: Int, month: Int) -> Int
```

Returns the number of days in the given month of any year. A month
outside `1..=12` is a runtime error.

```silt
import time

fn main() {
  println(time.days_in_month(2024, 2)) -- 29 (leap year)
  println(time.days_in_month(2023, 2)) -- 28
}
```


## `time.is_leap_year`

```
time.is_leap_year(year: Int) -> Bool
```

Returns true if the year is a leap year, by the Gregorian rule, for any
year.

```silt
import time

fn main() {
  println(time.is_leap_year(2024)) -- true
  println(time.is_leap_year(1900)) -- false (divisible by 100)
  println(time.is_leap_year(2000)) -- true (divisible by 400)
}
```


## `time.sleep`

```
time.sleep(duration: Duration) -> ()
```

Blocks the current task for the given duration. Other tasks continue running.

```silt
import time

fn main() {
  let before = time.now()
  time.sleep(time.ms(100))
  let elapsed = time.since(before, time.now())
  println(elapsed) -- ~100ms
}
```
