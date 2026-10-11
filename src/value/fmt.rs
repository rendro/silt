//! The text of a value.
//!
//! One formatter writes every text of a value, in one of two ways
//! ([`Mode`]): as it is shown (`println`, interpolation: a string is
//! its text, a date is `2024-03-09`, a builtin error is its message,
//! and a record or variant of a type with a `Display` impl the program
//! wrote is what the impl gave), and as it is inspected (`io.inspect`,
//! `assert_eq`'s message, Rust's `{:?}`: the value as a program would
//! write it, a string in quotes).
//!
//! The formatter goes through a value with a stack of its own for what
//! it has begun and not yet closed: a value nested a million levels
//! deep is written like any other.

use std::collections::{btree_map, btree_set};
use std::fmt;

use super::list::Elements;
use super::{List, Record, TooLong, Value};
use crate::typeinfo::{FieldType, ty};

impl Value {
    /// Whether the value can be written out: `Err` if a list in it
    /// cannot ([`List::writable`]). Showing a value to a program
    /// (`println`, interpolation, `io.inspect`) asks this first, and
    /// the error is the program's.
    pub fn writable(&self) -> Result<(), TooLong> {
        let mut pending = vec![self];
        while let Some(value) = pending.pop() {
            match value {
                Value::List(items) => {
                    items.writable()?;
                    if let Elements::Items(items) = items.elements() {
                        pending.extend(items);
                    }
                }
                Value::Tuple(items) => pending.extend(items.iter()),
                Value::Variant(variant) => pending.extend(variant.fields()),
                Value::Set(items) => pending.extend(items.iter()),
                Value::Map(entries) => {
                    for (k, v) in entries.iter() {
                        pending.push(k);
                        pending.push(v);
                    }
                }
                Value::Record(record) => pending.extend(record.fields()),
                _ => {}
            }
        }
        Ok(())
    }

    /// Format a value in silt syntax, suitable for `io.inspect`.
    ///
    /// Unlike `Display` (which prints bare strings for user output),
    /// this produces the silt-source representation: strings are
    /// quoted, collections use silt syntax, etc.
    pub fn format_silt(&self) -> String {
        Inspected(self).to_string()
    }
}

/// What writes a value in place of the formatter: for a value of a type
/// with a `Display` impl a program wrote, what the impl gave. `None`
/// for any other value, which the formatter writes itself.
pub type Written<'a> = &'a dyn Fn(&Value, &mut fmt::Formatter<'_>) -> Option<fmt::Result>;

/// A value as it is shown (`println`, interpolation), with `written`
/// asked first at each record and variant inside it.
pub struct Shown<'a>(pub &'a Value, pub Written<'a>);

impl fmt::Display for Shown<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write(self.0, f, Mode::Shown(self.1))
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write(self, f, Mode::Shown(&|_, _| None))
    }
}

/// A value as it is inspected.
struct Inspected<'a>(&'a Value);

impl fmt::Display for Inspected<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write(self.0, f, Mode::Inspected)
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write(self, f, Mode::Inspected)
    }
}

impl fmt::Debug for List {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&Value::List(self.clone()), f)
    }
}

impl fmt::Debug for Record {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&Value::Record(self.clone()), f)
    }
}

// ── The formatter ──────────────────────────────────────────────────

/// The two ways a value is written.
#[derive(Clone, Copy)]
enum Mode<'a> {
    /// As `println` writes it; what the `Display` impls of the program
    /// gave is asked for first at each record and variant.
    Shown(Written<'a>),
    /// As `io.inspect` writes it.
    Inspected,
}

/// A value the formatter has begun and not yet closed: its parts yet
/// to be written, and what closes it.
struct Open<'a> {
    parts: Parts<'a>,
    /// Whether a part of it is written: the next comes after `, `.
    begun: bool,
    close: &'static str,
}

/// The parts of a value yet to be written, in the order they are
/// written in.
enum Parts<'a> {
    /// The items of a tuple, the fields of a variant, the elements a
    /// list holds.
    Values(std::slice::Iter<'a, Value>),
    /// The fields of a record, in the order its type declares them
    /// (an anonymous record's in name order), each after its name.
    Fields(
        std::slice::Iter<'a, (String, FieldType)>,
        std::slice::Iter<'a, Value>,
    ),
    /// The entries of a map, a key and then its value: the entries
    /// left, and the value of the key last written.
    Entries(btree_map::Iter<'a, Value, Value>, Option<&'a Value>),
    /// The members of a set.
    Members(btree_set::Iter<'a, Value>),
}

/// A part of a value, as the formatter comes to it.
enum Part<'a> {
    Value(&'a Value),
    /// A record's field: its name and its value.
    Field(&'a str, &'a Value),
    /// A key of a map; its value is the next part.
    Key(&'a Value),
    /// The value of the key before it.
    OfKey(&'a Value),
}

impl<'a> Parts<'a> {
    fn next(&mut self) -> Option<Part<'a>> {
        match self {
            Parts::Values(values) => values.next().map(Part::Value),
            Parts::Fields(names, values) => {
                let value = values.next()?;
                let (name, _) = names.next()?;
                Some(Part::Field(name, value))
            }
            Parts::Entries(entries, value) => match value.take() {
                Some(value) => Some(Part::OfKey(value)),
                None => {
                    let (key, of_key) = entries.next()?;
                    *value = Some(of_key);
                    Some(Part::Key(key))
                }
            },
            Parts::Members(members) => members.next().map(Part::Value),
        }
    }
}

/// Write `value`.
fn write<'a>(value: &'a Value, f: &mut fmt::Formatter<'_>, mode: Mode<'_>) -> fmt::Result {
    // What is begun and not closed, the innermost last. (No memory is
    // asked for a value without parts.)
    let mut open: Vec<Open<'a>> = Vec::new();
    if let Some(begun) = begin(value, f, mode)? {
        open.push(begun);
    }
    while let Some(innermost) = open.last_mut() {
        let Some(part) = innermost.parts.next() else {
            f.write_str(innermost.close)?;
            open.pop();
            continue;
        };
        if !matches!(part, Part::OfKey(_)) && std::mem::replace(&mut innermost.begun, true) {
            f.write_str(", ")?;
        }
        let value = match part {
            Part::Value(value) => value,
            Part::Field(name, value) => {
                f.write_str(name)?;
                f.write_str(": ")?;
                value
            }
            // (A key that is a string is in quotes however the map is
            // written: `#{"a": 1}`.)
            Part::Key(Value::String(key)) => {
                write!(f, "\"{key}\"")?;
                continue;
            }
            Part::Key(key) => key,
            Part::OfKey(value) => {
                f.write_str(": ")?;
                value
            }
        };
        if let Some(begun) = begin(value, f, mode)? {
            open.push(begun);
        }
    }
    Ok(())
}

/// Write `value` if it has no parts, and what a value with parts
/// begins with: its parts are then to be written, and what closes it.
fn begin<'a>(
    value: &'a Value,
    f: &mut fmt::Formatter<'_>,
    mode: Mode<'_>,
) -> Result<Option<Open<'a>>, fmt::Error> {
    let shown = match mode {
        Mode::Shown(written) => {
            if matches!(value, Value::Record(..) | Value::Variant(..))
                && let Some(done) = written(value, f)
            {
                return done.map(|()| None);
            }
            true
        }
        Mode::Inspected => false,
    };
    let (parts, close) = match value {
        Value::Int(n) => return write!(f, "{n}").map(|()| None),
        Value::Float(n) => return write!(f, "{n}").map(|()| None),
        Value::Bool(b) => return write!(f, "{b}").map(|()| None),
        Value::Unit => return f.write_str("()").map(|()| None),
        Value::String(s) => {
            return match shown {
                true => f.write_str(s),
                false => write!(f, "\"{s}\""),
            }
            .map(|()| None);
        }
        Value::List(xs) => match xs.elements() {
            Elements::Items(items) => {
                f.write_str("[")?;
                (Parts::Values(items.iter()), "]")
            }
            Elements::Ints(lo, hi) => return write_ints(f, xs, lo, hi).map(|()| None),
        },
        Value::Tuple(items) => {
            f.write_str("(")?;
            // (A tuple of one item is written as a program writes it.)
            let close = match items.len() {
                1 => ",)",
                _ => ")",
            };
            (Parts::Values(items.iter()), close)
        }
        Value::Map(entries) => {
            f.write_str("#{")?;
            (Parts::Entries(entries.iter(), None), "}")
        }
        Value::Set(members) => {
            f.write_str("#[")?;
            (Parts::Members(members.iter()), "]")
        }
        Value::Record(record) => {
            if shown && write_time(record, f)? {
                return Ok(None);
            }
            // (An anonymous record is written without a name.)
            if !record.ty().is_anon() {
                f.write_str(&record.ty().name)?;
                f.write_str(" ")?;
            }
            f.write_str("{")?;
            let names = record.ty().fields().iter();
            (Parts::Fields(names, record.fields().iter()), "}")
        }
        Value::Variant(variant) => {
            // A builtin error is shown as its message, the text its
            // `message()` gives: one text for both.
            if shown && let Some(message) = crate::builtins::error_text(variant) {
                return f.write_str(&message).map(|()| None);
            }
            f.write_str(variant.name())?;
            if variant.fields().is_empty() {
                return Ok(None);
            }
            f.write_str("(")?;
            (Parts::Values(variant.fields().iter()), ")")
        }
        Value::VmClosure(_) | Value::BuiltinFn(_) | Value::HostFn(_) if !shown => {
            return f.write_str("<fn>").map(|()| None);
        }
        Value::VmClosure(c) => return write!(f, "<fn:{}>", c.function.name()).map(|()| None),
        Value::BuiltinFn(id) => return write!(f, "<builtin:{id}>").map(|()| None),
        Value::HostFn(h) => return write!(f, "<host:{}>", h.name).map(|()| None),
        Value::VariantConstructor(tag) => return write!(f, "<constructor:{tag}>").map(|()| None),
        Value::TypeDescriptor(ty) => return write!(f, "<type:{}>", ty.name).map(|()| None),
        Value::PrimitiveDescriptor(name) => return write!(f, "<type:{name}>").map(|()| None),
        Value::Channel(ch) => return write!(f, "<channel:{}>", ch.id()).map(|()| None),
        Value::Handle(h) => return write!(f, "<handle:{}>", h.id).map(|()| None),
        Value::Bytes(b) => return f.write_str(&format_bytes_preview(b)).map(|()| None),
        Value::TcpListener(t) => return write!(f, "<tcp-listener:{}>", t.id).map(|()| None),
        Value::TcpStream(t) => return write!(f, "<tcp-stream:{}>", t.id).map(|()| None),
    };
    Ok(Some(Open {
        parts,
        begun: false,
        close,
    }))
}

/// Write the list `xs` of the Ints from `lo` to `hi`, which holds no
/// element. Of a list that cannot be written out ([`List::writable`])
/// its first three elements, `...` and its last element are written: a
/// program is refused the text of such a list ([`Value::writable`]),
/// and a host that formats the value all the same gets this.
fn write_ints(f: &mut fmt::Formatter<'_>, xs: &List, lo: i64, hi: i64) -> fmt::Result {
    f.write_str("[")?;
    match xs.writable() {
        Ok(()) => {
            for n in lo..=hi {
                if n > lo {
                    f.write_str(", ")?;
                }
                write!(f, "{n}")?;
            }
        }
        Err(_) => write!(f, "{lo}, {}, {}, ..., {hi}", lo + 1, lo + 2)?,
    }
    f.write_str("]")
}

/// Show `record` if it is a builtin record of time, which is shown as
/// its text (`2024-03-09`, `07:30:05`, `1.500s`), and say so.
fn write_time(record: &Record, f: &mut fmt::Formatter<'_>) -> Result<bool, fmt::Error> {
    match record.type_id() {
        ty::DATE => {
            let y = val_i64(record.get("year"));
            let m = val_i64(record.get("month"));
            let d = val_i64(record.get("day"));
            write!(f, "{y:04}-{m:02}-{d:02}")?;
        }
        ty::TIME => {
            let h = val_i64(record.get("hour"));
            let m = val_i64(record.get("minute"));
            let s = val_i64(record.get("second"));
            let ns = val_i64(record.get("ns"));
            if ns > 0 {
                write!(f, "{h:02}:{m:02}:{s:02}.{ns:09}")?;
            } else {
                write!(f, "{h:02}:{m:02}:{s:02}")?;
            }
        }
        ty::DATE_TIME => match (record.get("date"), record.get("time")) {
            (Some(date), Some(time)) => write!(f, "{date}T{time}")?,
            _ => f.write_str("DateTime {}")?,
        },
        ty::DURATION => fmt_duration(f, val_i64(record.get("ns")))?,
        _ => return Ok(false),
    }
    Ok(true)
}

/// Shared rendering for `Bytes` values: short hex preview + length.
/// Truncates to the first 32 bytes with an ellipsis to keep output
/// readable for large buffers (e.g. tcp.read(conn, 4096)).
fn format_bytes_preview(b: &[u8]) -> String {
    const PREVIEW: usize = 32;
    if b.len() <= PREVIEW {
        let hex: Vec<String> = b.iter().map(|byte| format!("{byte:02x}")).collect();
        format!("bytes({}, length: {})", hex.join(" "), b.len())
    } else {
        let hex: Vec<String> = b[..PREVIEW]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        format!("bytes({} …, length: {})", hex.join(" "), b.len())
    }
}

/// Extract an i64 from an optional Value reference.
fn val_i64(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Int(n)) => *n,
        _ => 0,
    }
}

/// Format a duration in nanoseconds as a human-readable string.
fn fmt_duration(f: &mut fmt::Formatter<'_>, total_ns: i64) -> fmt::Result {
    if total_ns < 0 {
        write!(f, "-")?;
    }
    let ns = total_ns.unsigned_abs();
    if ns == 0 {
        write!(f, "0s")
    } else if ns >= 3_600_000_000_000 {
        let h = ns / 3_600_000_000_000;
        let m = (ns % 3_600_000_000_000) / 60_000_000_000;
        let s = (ns % 60_000_000_000) / 1_000_000_000;
        if m > 0 && s > 0 {
            write!(f, "{h}h{m}m{s}s")
        } else if m > 0 {
            write!(f, "{h}h{m}m")
        } else {
            write!(f, "{h}h")
        }
    } else if ns >= 60_000_000_000 {
        let m = ns / 60_000_000_000;
        let s = (ns % 60_000_000_000) / 1_000_000_000;
        if s > 0 {
            write!(f, "{m}m{s}s")
        } else {
            write!(f, "{m}m")
        }
    } else if ns >= 1_000_000_000 {
        let s = ns / 1_000_000_000;
        let ms = (ns % 1_000_000_000) / 1_000_000;
        if ms > 0 {
            write!(f, "{s}.{ms:03}s")
        } else {
            write!(f, "{s}s")
        }
    } else if ns >= 1_000_000 {
        write!(f, "{}ms", ns / 1_000_000)
    } else if ns >= 1_000 {
        write!(f, "{}us", ns / 1_000)
    } else {
        write!(f, "{ns}ns")
    }
}
