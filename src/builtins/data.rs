//! A silt value as data: the one layer that `json.stringify`,
//! `toml.stringify` and the parameters of a postgres statement write a
//! value from.
//!
//! [`data`] turns a value into [`Data`]: `Some(x)` is `x`, a `json.Value`
//! the JSON it holds, a record a table of its fields in the order its
//! type declares them, a map a table sorted by its keys' shown text, a
//! variant the table `{fields, variant}`, a date and a time their
//! fields. It refuses every value the format it writes for has no form
//! for, with one wording per refusal, wherever in the value it is. Each
//! format then writes the data in its own forms: json its numbers and
//! strings, toml its literals and its tables, postgres the text the
//! server reads.
//!
//! A record or variant of a type the program wrote a `json.Encode`
//! impl for is written as the `json.Value` its `encode` gives, wherever
//! it is in the value. The impl is silt code, which the VM runs
//! (`vm::encode`): [`data_with_holes`] leaves a hole for each such part,
//! and once the impls have run [`fill`] writes what they gave there.

use std::sync::Arc;

use crate::typeinfo::{bv, ty};
use crate::value::{Record, Value};
use crate::vm::VmError;

/// The deepest a value may be nested for a writer: each element of a
/// list or a tuple, field of a record or variant, and value of a map is
/// a level further in than what holds them (the content of a `Some` is
/// not: it is written in its place; nor is a `json.Value`'s, but for
/// toml, which counts it as it counts a variant's). The writers go
/// down a value on the native stack, and one nested without bound would
/// overflow it: a value at the bound takes some 5 MiB of it in a debug
/// build, a third of the least a thread that runs silt code has
/// (`scheduler::WORKER_STACK_BYTES`). (A parser stops far sooner:
/// `json.parse` at 127 levels of a document, `toml.parse` at 79.)
pub(crate) const MAX_ENCODE_DEPTH: usize = 1000;

/// The format a value is written in: which values have a form in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Format {
    Json,
    /// TOML has no null: a `None` member of a table is left out, and a
    /// `None` anywhere else, or a `()`, has no form.
    Toml,
    /// The parameter of a statement: `Bytes` has a form at its place and
    /// in an array (`bytea`'s hex text), not in the JSON of a record, a
    /// map, a tuple, a variant.
    Postgres,
}

/// A value as data.
#[derive(Debug)]
pub(crate) enum Data {
    /// `None`, `json.Null`.
    Null,
    Unit,
    Int(i64),
    Float(f64),
    Bool(bool),
    String(Arc<str>),
    /// Only for [`Format::Postgres`], at a parameter's place or in an
    /// array.
    Bytes(#[cfg_attr(not(feature = "postgres"), allow(dead_code))] Arc<Vec<u8>>),
    // (Boxed, as each is rare and larger than the rest: a node is 32
    // bytes, a list of three million Ints some 96 MB of them.)
    Date(Box<Date>),
    Time(Box<Time>),
    DateTime(Box<(Date, Time)>),
    /// A list, the fields of a variant.
    Array(Vec<Data>),
    /// A tuple: an array to json and toml, JSON to postgres where a list
    /// is an array.
    Tuple(Vec<Data>),
    /// A record, a map, a variant with fields, a `Duration`, an
    /// `Instant`.
    Table(Vec<(String, Data)>),
    /// A `json.Value` at a postgres parameter's place or in an array: the
    /// JSON it holds, which the server gets as JSON text. (For json and
    /// toml, and inside JSON, a `json.Value` is what it holds.)
    Json(Box<Data>),
    /// The place of the `k`th part a written `json.Encode` impl writes,
    /// until [`fill`] writes it.
    Hole(usize),
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<Data>() == 32);

/// A `Date`'s fields. Shown as the text of a `Date`, the one place it
/// is decided (a `Date` value displays it too): `2026-10-19`,
/// `-043-03-15`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Date {
    pub(crate) year: i64,
    pub(crate) month: i64,
    pub(crate) day: i64,
}

/// A `Time`'s fields. Shown as the text of a `Time`, the one place it
/// is decided (a `Time` value displays it too): `12:30:00`, with a
/// fraction of a second only when it has one, `12:30:00.250000000`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Time {
    pub(crate) hour: i64,
    pub(crate) minute: i64,
    pub(crate) second: i64,
    pub(crate) ns: i64,
}

/// The `Int` field `name` of `record`, or 0. (A record of a builtin
/// type of time holds an `Int` in each field.)
fn int(record: &Record, name: &str) -> i64 {
    match record.get(name) {
        Some(Value::Int(n)) => *n,
        _ => 0,
    }
}

impl Date {
    /// The fields of `record`, a `Date`.
    pub(crate) fn of(record: &Record) -> Date {
        Date {
            year: int(record, "year"),
            month: int(record, "month"),
            day: int(record, "day"),
        }
    }
}

impl Time {
    /// The fields of `record`, a `Time`.
    pub(crate) fn of(record: &Record) -> Time {
        Time {
            hour: int(record, "hour"),
            minute: int(record, "minute"),
            second: int(record, "second"),
            ns: int(record, "ns"),
        }
    }
}

impl std::fmt::Display for Date {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Date { year, month, day } = self;
        write!(f, "{year:04}-{month:02}-{day:02}")
    }
}

impl std::fmt::Display for Time {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Time {
            hour,
            minute,
            second,
            ns,
        } = self;
        write!(f, "{hour:02}:{minute:02}:{second:02}")?;
        match *ns > 0 {
            true => write!(f, ".{ns:09}"),
            false => Ok(()),
        }
    }
}

/// What a format says of a hole of data that was never filled (a bug
/// of silt: [`fill`] fills every hole before data is written).
pub(crate) const UNFILLED: &str =
    "internal error: a part a json.Encode impl writes was not written";

/// What is said of a value of the kind `kind` that a format has no
/// form for.
pub(crate) fn unsupported(kind: &str) -> String {
    format!("unsupported value kind {kind}")
}

/// The error of `caller` for `v`, a value the format it writes has no
/// form for: it names the kind of the value. (To a program every
/// function is an `Fn`.)
fn refused(caller: &str, v: &Value) -> VmError {
    let kind = match v.is_fn() {
        true => "Fn",
        false => v.kind(),
    };
    VmError::new(format!("{caller}: {}", unsupported(kind)))
}

/// `v` as data in `format`, for `caller` (`json.stringify`), or the
/// error of the first value in it that `format` has no form for: a set,
/// a function (the constructor of a variant among them), a channel, a
/// handle, `Bytes` where the format has none, a value nested deeper
/// than [`MAX_ENCODE_DEPTH`], a list too long to write out; for toml
/// also `()` and a `None` in an array. Every record and variant is
/// written by its structure.
pub(crate) fn data(caller: &str, format: Format, v: &Value) -> Result<Data, VmError> {
    Writer::new(caller, format, None).data(v, 0, Place::Value)
}

/// A record or variant in a value whose type has a written
/// `json.Encode` impl: the value, the global slot of its type's
/// `encode`, and where in the value it is.
pub(crate) struct Part {
    pub(crate) value: Value,
    pub(crate) slot: u16,
    level: usize,
    place: Place,
}

/// `v` as data, as [`data`] writes it, but with a hole in the place of
/// each record or variant that `written` gives the slot of an `encode`
/// for; and those parts, the `k`th the one of `Data::Hole(k)`.
pub(crate) fn data_with_holes(
    caller: &str,
    format: Format,
    v: &Value,
    written: &dyn Fn(&Value) -> Option<u16>,
) -> Result<(Data, Vec<Part>), VmError> {
    let writer = Writer::new(caller, format, Some(written));
    let data = writer.data(v, 0, Place::Value)?;
    Ok((data, writer.parts.into_inner()))
}

/// Write in each hole of `data` the `json.Value` the `encode` of its
/// part gave (`encoded`, in the order of `parts`), where the part is,
/// in `format`: as deep as it is, so that the depth counts on.
pub(crate) fn fill(
    caller: &str,
    format: Format,
    data: &mut Data,
    parts: &[Part],
    encoded: &[Value],
) -> Result<(), VmError> {
    match data {
        Data::Hole(k) => {
            let (part, value) = (&parts[*k], &encoded[*k]);
            *data = Writer::new(caller, format, None).data(value, part.level, part.place)?;
        }
        Data::Array(items) | Data::Tuple(items) => {
            for item in items {
                fill(caller, format, item, parts, encoded)?;
            }
        }
        Data::Table(members) => {
            for (_, member) in members {
                fill(caller, format, member, parts, encoded)?;
            }
        }
        Data::Json(content) => fill(caller, format, content, parts, encoded)?,
        _ => {}
    }
    Ok(())
}

/// `f` of each of `items`, or its first error. (Collecting a `Result`
/// would grow the vector by doubling: it is made the size it ends.)
fn each<I: ExactSizeIterator, T>(
    items: I,
    mut f: impl FnMut(I::Item) -> Result<T, VmError>,
) -> Result<Vec<T>, VmError> {
    let mut all = Vec::with_capacity(items.len());
    for item in items {
        all.push(f(item)?);
    }
    Ok(all)
}

/// Where in the value being written a value is, as far as the format
/// tells them apart.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Place {
    /// At the top, or in a list at the top (a postgres parameter's place
    /// or an array).
    Value,
    /// A member of a table.
    Member,
    /// An element of an array that is no list at the top (a tuple, the
    /// fields of a variant, a list inside one of them).
    Element,
}

struct Writer<'c> {
    caller: &'c str,
    format: Format,
    /// The slot of the `encode` of a record or variant's type, if the
    /// program wrote one: such a part is a hole (`data_with_holes`).
    written: Option<&'c dyn Fn(&Value) -> Option<u16>>,
    /// The parts of the holes so far.
    parts: std::cell::RefCell<Vec<Part>>,
}

impl<'c> Writer<'c> {
    fn new(
        caller: &'c str,
        format: Format,
        written: Option<&'c dyn Fn(&Value) -> Option<u16>>,
    ) -> Writer<'c> {
        Writer {
            caller,
            format,
            written,
            parts: std::cell::RefCell::new(Vec::new()),
        }
    }

    /// `v`, which is `level` levels inside the value and at `place`.
    fn data(&self, v: &Value, level: usize, place: Place) -> Result<Data, VmError> {
        // `Some(x)` is written as `x` is, and so is a `json.Value` as the
        // JSON it holds (`json.Int(1)` as `1`). A `Some` is no level of
        // its own; for toml the content of a `json.Value` is a level
        // further in, as the content of a variant is.
        let (mut v, mut level) = (v, level);
        let mut json = false;
        while let Value::Variant(variant) = v
            && (variant.is(bv::SOME) || variant.of(ty::JSON_VALUE))
            && let [content] = variant.fields()
        {
            if variant.of(ty::JSON_VALUE) {
                json = true;
                level += usize::from(self.format == Format::Toml);
            }
            v = content;
        }
        if level > MAX_ENCODE_DEPTH {
            return Err(VmError::new(format!(
                "{}: the value is nested more than {MAX_ENCODE_DEPTH} levels deep",
                self.caller
            )));
        }
        if let Value::Variant(variant) = v {
            json |= variant.is(bv::JSON_NULL);
        }
        // A part a written `json.Encode` impl writes.
        if matches!(v, Value::Record(_) | Value::Variant(_))
            && let Some(slot) = self.written.and_then(|written| written(v))
        {
            let mut parts = self.parts.borrow_mut();
            parts.push(Part {
                value: v.clone(),
                slot,
                level,
                place,
            });
            return Ok(Data::Hole(parts.len() - 1));
        }
        // Postgres sends a `json.Value` at a parameter's place, or in an
        // array, as JSON text.
        if json && self.format == Format::Postgres && place == Place::Value {
            return Ok(Data::Json(Box::new(self.content(
                v,
                level,
                Place::Member,
            )?)));
        }
        self.content(v, level, place)
    }

    /// `v`, which is no `Some` and no `json.Value` with content.
    fn content(&self, v: &Value, level: usize, place: Place) -> Result<Data, VmError> {
        // A list keeps the place of a list at the top; inside anything
        // else its elements are elements.
        let element = match place {
            Place::Value => Place::Value,
            _ => Place::Element,
        };
        let inner = |v: &Value, place| self.data(v, level + 1, place);
        Ok(match v {
            Value::Int(n) => Data::Int(*n),
            Value::Float(f) => Data::Float(f.get()),
            Value::Bool(b) => Data::Bool(*b),
            Value::String(s) => Data::String(s.clone()),
            Value::Bytes(bytes) if self.format == Format::Postgres && place == Place::Value => {
                Data::Bytes(bytes.clone())
            }
            Value::List(xs) => {
                xs.writable()?;
                Data::Array(each(xs.iter(), |x| inner(&x, element))?)
            }
            Value::Map(m) => {
                let entries: Result<Vec<_>, VmError> = m
                    .iter()
                    .map(|(k, v)| {
                        let key = match k {
                            Value::String(s) => s.to_string(),
                            other => other.to_string(),
                        };
                        Ok((key, inner(v, Place::Member)?))
                    })
                    .collect();
                // Sorted by key; of a key shown twice the last value
                // counts.
                let members: std::collections::BTreeMap<_, _> = entries?.into_iter().collect();
                Data::Table(members.into_iter().collect())
            }
            Value::Tuple(vs) => Data::Tuple(each(vs.iter(), |v| inner(v, Place::Element))?),
            Value::Record(record) if record.type_id() == ty::DATE => {
                Data::Date(Box::new(Date::of(record)))
            }
            Value::Record(record) if record.type_id() == ty::TIME => {
                Data::Time(Box::new(Time::of(record)))
            }
            Value::Record(record) if record.type_id() == ty::DATE_TIME => {
                match (record.get("date"), record.get("time")) {
                    (Some(Value::Record(date)), Some(Value::Record(time))) => {
                        Data::DateTime(Box::new((Date::of(date), Time::of(time))))
                    }
                    // (A `DateTime` holds a `Date` and a `Time`.)
                    _ => return Err(refused(self.caller, v)),
                }
            }
            Value::Record(record) => Data::Table(each(record.named(), |(k, v)| {
                Ok((k.to_string(), inner(v, Place::Member)?))
            })?),
            Value::Variant(variant) if variant.is(bv::NONE) || variant.is(bv::JSON_NULL) => {
                // (A `None` at the top is no table: toml says so.)
                if self.format == Format::Toml && place != Place::Member && level > 0 {
                    let null = match variant.is(bv::NONE) {
                        true => "None",
                        false => "json.Null",
                    };
                    return Err(VmError::new(format!(
                        "{}: TOML has no null: a {null} in an array",
                        self.caller
                    )));
                }
                Data::Null
            }
            // A variant without fields is its name; one with fields the
            // table of them and its name (`fields` before `variant`, as
            // the keys of a table sort).
            Value::Variant(variant) if variant.fields().is_empty() => {
                Data::String(variant.name().into())
            }
            Value::Variant(variant) => {
                let items = each(variant.fields().iter(), |v| inner(v, Place::Element))?;
                Data::Table(vec![
                    ("fields".into(), Data::Array(items)),
                    ("variant".into(), Data::String(variant.name().into())),
                ])
            }
            Value::Unit if self.format == Format::Toml => {
                return Err(VmError::new(format!(
                    "{}: TOML cannot represent Unit",
                    self.caller
                )));
            }
            Value::Unit => Data::Unit,
            _ => return Err(refused(self.caller, v)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Float;
    use std::collections::BTreeSet;

    /// The checker's `json.Encode` of a builtin type is the writer's: a
    /// type that is no record or enum has the trait exactly when json
    /// has a form for its values, each type the builtin registry
    /// declares has it (by its structure, which the checker walks), and
    /// a handle has none.
    #[test]
    fn a_builtin_type_has_json_encode_when_the_writer_has_a_json_form_for_it() {
        let impls = crate::typechecker::__trait_init_fingerprint_check_program();
        let encode: BTreeSet<&str> = impls
            .iter()
            .filter_map(|stamp| stamp.strip_prefix("Encode:"))
            .collect();
        let samples = [
            ("Int", Value::Int(1)),
            ("Float", Value::Float(Float::new(1.5).expect("finite"))),
            ("Bool", Value::Bool(true)),
            ("String", Value::String("s".into())),
            ("Unit", Value::Unit),
            ("List", Value::list(vec![Value::Int(1)])),
            ("Tuple", Value::tuple(vec![Value::Int(1)])),
            ("Map", Value::map([(Value::Int(1), Value::Int(2))])),
            ("Set", Value::set([Value::Int(1)])),
            ("Bytes", Value::Bytes(Arc::new(vec![1]))),
        ];
        for (name, value) in samples {
            let written = data("json.stringify", Format::Json, &value).is_ok();
            assert_eq!(encode.contains(name), written, "{name}");
        }
        assert!(!encode.contains("Channel"));
        let registry = crate::builtins::registry::registry();
        for (_, ty) in registry.types() {
            assert!(encode.contains(ty.name), "{}", ty.name);
        }
        let opaque = (registry.modules.iter())
            .flat_map(|module| module.opaque.iter())
            .chain(crate::builtins::registry::PRELUDE_OPAQUE);
        for (name, _) in opaque {
            assert!(!encode.contains(name), "{name}");
        }
    }
}
