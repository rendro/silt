//! `toml.*` builtin functions: parse TOML documents into typed silt records
//! and serialize silt values into TOML text.
//!
//! The API mirrors the `json` module in `src/builtins/json.rs`, with each
//! fallible call returning a typed `TomlError`. See
//! `module.rs::builtin_error_enum_variants_with_arity` for Phase 0
//! background:
//! - `toml.parse(T: Type, s: String) -> Result(T, TomlError)` — parse a top-
//!   level TOML table into a record, or a variant with fields, of type `T`.
//! - `toml.parse_list(T: Type, s: String) -> Result(List(T), TomlError)` —
//!   parse a document whose top-level table has one key, an array: a
//!   `[[items]]` array of tables for a record type, `xs = [1, 2]` for `Int`.
//! - `toml.parse_map(V: Type, s: String) -> Result(Map(String, V), TomlError)`
//!   — parse a top-level table as a `Map(String, V)`.
//! - `toml.stringify(v) -> Result(String, TomlError)` — serialize a silt
//!   value to compact TOML. Unlike `json.stringify` this is fallible because
//!   TOML requires a table at the top level.
//! - `toml.pretty(v) -> Result(String, TomlError)` — serialize a silt value
//!   to TOML as `stringify` does, except that an array of plain values is
//!   written one element per line.
//!
//! ## TOML-specific types
//!
//! TOML's native date/time variants (Offset Date-Time, Local Date-Time,
//! Local Date, Local Time) are translated to strings using the same ISO 8601
//! shapes that `json.parse` already accepts for `Date`, `Time`, and `DateTime`
//! fields — so a `Date` field in the record happily receives a bare TOML
//! `1979-05-27`, a `DateTime` field receives `1979-05-27T07:32:00Z`, and a
//! `Time` field receives `07:32:00`. The one typed decoder (`decode.rs`)
//! reads a document of either module, so the two stay aligned. The encoder
//! writes a `Date`, `Time` and `DateTime` as such a literal
//! (`literal`), so each reads back as the value it was.
//!
//! ## Semantics
//!
//! - Parse errors pass through the `toml` crate's error text (`format!("{e}")`).
//! - Missing `Option` fields default to `None`.
//! - Missing required fields return `Err(...)`.
//! - Type mismatches return an `Err(...)` naming the field.
//! - Integer overflow: TOML integers are `i64` per spec, which matches silt's
//!   `Int` exactly; no demotion risk.

use std::sync::Arc;

use chrono::{NaiveDate, NaiveTime};

use super::registry::decoding::Family;
use crate::typeinfo::bv;
use crate::value::Value;
use crate::vm::{Source, Step, Vm, VmError};

use super::data::{self, Data, Format};
use super::decode::{self, Decoder, Numbers, Rules, seed, shown};
use super::typed::{Type, builtins};
use crate::typeinfo::FieldType;

// ── TomlError helpers ────────────────────────────────────────────────
//
// Phase 1 of the stdlib error redesign: every fallible toml.* call now
// surfaces a typed `TomlError` variant wrapped in `Err(...)` instead of
// a bare `Err(String)`. Mirrors `json_*_err` in `src/builtins/json.rs`.
//
// `TomlError` variants, each with the byte offset of where it is and
// that byte's line and column (`decode::place`):
//   Syntax(message, byte_offset, line, column)
//   TypeMismatch(expected, actual, byte_offset, line, column)
//   MissingField(name, byte_offset, line, column)
//   Other(message, byte_offset, line, column)
//   UnknownField(path, expected_keys, byte_offset, line, column)

fn toml_err_wrap(inner: Value) -> Value {
    Value::variant(bv::ERR, [inner])
}

/// Classify a `toml::de::Error` into one of the `TomlError` variants.
/// The `toml` crate exposes a `span()` method that yields a byte range
/// into the source; we take the start as the offset. For errors that
/// don't have a span (extremely rare in practice) we fall back to 0.
pub(crate) fn toml_de_error_to_variant(err: &::toml::de::Error, text: &str) -> Value {
    let start = err.span().map_or(0, |s| s.start);
    syntax(err, text, start, text, start)
}

/// The `Syntax` error of `err`, an error at the byte `at` of `text`
/// that is the byte `offset` of the text the program gave, `s` (`text`
/// is `s` with literals marked, [`marking`]): its line and column are
/// counted in `s`.
fn syntax(err: &::toml::de::Error, text: &str, at: usize, s: &str, offset: usize) -> Value {
    let (line, column) = decode::line_column(s.as_bytes(), offset);
    let [offset, line, column] = decode::place(offset, line, column);
    Value::variant(
        bv::TOML_SYNTAX,
        [
            Value::string(toml_reason(err.message(), text.get(at..).unwrap_or(""))),
            offset,
            line,
            column,
        ],
    )
}

/// The reason of a `toml` syntax error in silt's words, from the
/// crate's message: on one line (the crate writes what it expected on a
/// second one: "invalid table header\nduplicate key `a` in document
/// root"), with no reason left empty (a control character in a
/// comment), and a number too large worded as `int.parse` words it.
/// `rest` is the text from where the error is: an empty reason there,
/// with nothing but white space left, is a value missing at the end
/// (`a = `); a string the crate looked for where nothing but white space
/// or a comment is left on the line, a value missing at the end of the
/// line (`a = ⏎`).
fn toml_reason(message: &str, rest: &str) -> String {
    let lines: Vec<&str> = (message.lines())
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let line_rest = rest.split('\n').next().unwrap_or("").trim();
    match lines.as_slice() {
        [] if rest.trim().is_empty() => "a value is missing at the end of the text".to_string(),
        ["invalid string", ..] if line_rest.is_empty() || line_rest.starts_with('#') => {
            "a value is missing at the end of the line".to_string()
        }
        [] => "a character that is not allowed here".to_string(),
        ["number too large to fit in target type"] => "number too large".to_string(),
        ["number too small to fit in target type"] => "number too small".to_string(),
        lines => lines.join(": "),
    }
}

/// Build a full `Err(TomlError)` from a `toml::de::Error`.
pub(crate) fn toml_de_result_err(err: &::toml::de::Error, text: &str) -> Value {
    toml_err_wrap(toml_de_error_to_variant(err, text))
}

/// Build `Err(Other(msg))` for ad-hoc failures (unknown type,
/// serialization errors, document-shape violations, etc.).
pub(crate) fn toml_unknown_err<S: Into<Arc<str>>>(msg: S) -> Value {
    let [offset, line, column] = decode::NO_PLACE;
    toml_err_wrap(Value::variant(
        bv::TOML_OTHER,
        [Value::string(msg), offset, line, column],
    ))
}

/// How `TomlError` shows the variant `tag` with `fields`:
/// `None` if they are no variant of it.
pub(crate) fn error_text(tag: &str, fields: &[Value]) -> Option<String> {
    use super::json::at_place;
    Some(match (tag, fields) {
        (
            "Syntax",
            [
                Value::String(m, _),
                Value::Int(_),
                Value::Int(line),
                Value::Int(column),
            ],
        ) => {
            format!("toml syntax error{}: {m}", at_place(*line, *column))
        }
        (
            "TypeMismatch",
            [
                Value::String(exp, _),
                Value::String(act, _),
                Value::Int(_),
                Value::Int(line),
                Value::Int(column),
            ],
        ) => {
            let at = at_place(*line, *column);
            format!("toml type mismatch{at}: expected {exp}, got {act}")
        }
        (
            "MissingField",
            [
                Value::String(n, _),
                Value::Int(_),
                Value::Int(line),
                Value::Int(column),
            ],
        ) => {
            format!("toml missing field{}: {n}", at_place(*line, *column))
        }
        // (Its place is in fields of its own: the message is the text.)
        (
            "Other",
            [
                Value::String(m, _),
                Value::Int(_),
                Value::Int(_),
                Value::Int(_),
            ],
        ) => m.to_string(),
        (
            "UnknownField",
            [
                Value::String(n, _),
                Value::List(expected),
                Value::Int(_),
                Value::Int(line),
                Value::Int(column),
            ],
        ) => {
            let at = at_place(*line, *column);
            format!("toml unknown field{at}: {n}, {}", expected_keys(expected))
        }
        _ => return None,
    })
}

/// What an `UnknownField` says of the keys its table takes: `expected
/// match or account`, `expected a, b or c`, `expected no key`.
fn expected_keys(keys: &crate::value::List) -> String {
    let keys: Vec<String> = keys.iter().map(|key| key.to_string()).collect();
    match keys.as_slice() {
        [] => "expected no key".to_string(),
        [one] => format!("expected {one}"),
        [rest @ .., last] => format!("expected {} or {last}", rest.join(", ")),
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

/// `data` as TOML: a member of a table that is null (a `None`, a
/// `json.Null`) is left out, as TOML has no null, and reads back as
/// `None`; a date and a time are TOML's own literals ([`literal`]).
/// (Data written as TOML holds no other null, no `()` and no `Bytes`:
/// [`data::data`] refuses them.) The writer puts the plain keys of a
/// table before its sub-tables, as TOML has them.
struct Toml<'d>(&'d Data);

impl serde::Serialize for Toml<'_> {
    fn serialize<S: serde::Serializer>(&self, to: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{Error, SerializeMap, SerializeSeq};
        match self.0 {
            Data::Int(n) => to.serialize_i64(*n),
            Data::Float(f) => to.serialize_f64(*f),
            Data::Bool(b) => to.serialize_bool(*b),
            Data::String(s) => to.serialize_str(s),
            Data::Date(_) | Data::Time(_) | Data::DateTime(..) => match literal(self.0) {
                Ok(literal) => literal.serialize(to),
                Err(text) => to.serialize_str(&text),
            },
            Data::Array(items) | Data::Tuple(items) => {
                let mut seq = to.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(&Toml(item))?;
                }
                seq.end()
            }
            Data::Table(members) => {
                let mut map = to.serialize_map(None)?;
                for (key, value) in members {
                    if !matches!(value, Data::Null) {
                        map.serialize_entry(key, &Toml(value))?;
                    }
                }
                map.end()
            }
            Data::Json(content) => Toml(content).serialize(to),
            Data::Null | Data::Unit | Data::Bytes(_) => {
                Err(S::Error::custom("TOML has no form for the value"))
            }
            Data::Hole(_) => Err(S::Error::custom(data::UNFILLED)),
        }
    }
}

/// `data`, a date, a time of day or a datetime, as a TOML literal
/// (`2026-10-19`, `12:30:00.25`, `2026-10-19T12:30:00`), or, for one
/// that no literal holds (a year outside 0000-9999, fields that are no
/// day or time of the calendar), `Err` of the text silt shows of it,
/// which toml writes as a string. `toml.parse` reads a literal and the
/// text alike.
fn literal(data: &Data) -> Result<::toml::value::Datetime, std::string::String> {
    use ::toml::value::Datetime;
    let date = |date: &crate::value::Date| {
        let year = u16::try_from(date.year).ok().filter(|year| *year <= 9999)?;
        let (month, day) = (u8::try_from(date.month).ok()?, u8::try_from(date.day).ok()?);
        NaiveDate::from_ymd_opt(year.into(), month.into(), day.into())?;
        Some(::toml::value::Date { year, month, day })
    };
    let time = |time: &crate::value::Time| {
        let hour = u8::try_from(time.hour).ok()?;
        let minute = u8::try_from(time.minute).ok()?;
        let second = u8::try_from(time.second).ok()?;
        // (Below a second: chrono takes one more for a leap second.)
        let nanosecond = u32::try_from(time.ns)
            .ok()
            .filter(|ns| *ns < 1_000_000_000)?;
        NaiveTime::from_hms_nano_opt(hour.into(), minute.into(), second.into(), nanosecond)?;
        Some(::toml::value::Time {
            hour,
            minute,
            second,
            nanosecond,
        })
    };
    let (literal, text) = match data {
        Data::Date(d) => (date(d).map(|d| (Some(d), None)), d.to_string()),
        Data::Time(t) => (time(t).map(|t| (None, Some(t))), t.to_string()),
        Data::DateTime(at) => {
            let (d, t) = &**at;
            (
                date(d).zip(time(t)).map(|(d, t)| (Some(d), Some(t))),
                format!("{d}T{t}"),
            )
        }
        _ => (None, std::string::String::new()),
    };
    literal
        .map(|(date, time)| Datetime {
            date,
            time,
            offset: None,
        })
        .ok_or(text)
}

/// `data` as the document `caller` writes. TOML's top level must be a
/// table: a value that is no record, no map (a scalar, an array, a
/// `None`) is an error naming what it is.
fn top_level(caller: &str, data: Data) -> Result<Data, VmError> {
    let kind = match &data {
        Data::Table(_) => return Ok(data),
        Data::Array(_) | Data::Tuple(_) => "array",
        Data::Int(_) => "integer",
        Data::Float(_) => "float",
        Data::Bool(_) => "boolean",
        Data::String(_) => "string",
        Data::Date(_) | Data::Time(_) | Data::DateTime(..) => match literal(&data) {
            Ok(_) => "datetime",
            Err(_) => "string",
        },
        Data::Null | Data::Unit | Data::Bytes(_) | Data::Json(_) | Data::Hole(_) => "null",
    };
    Err(VmError::new(format!(
        "{caller}: top-level value must be a table/record, got {kind}"
    )))
}

// ── Decoding a document ─────────────────────────────────────────────

/// How a TOML document decodes: TOML says which kind a number is, so a
/// float is no `Int` whatever its value; a date and a time are TOML's
/// literals or strings; TOML has no null (a key left out is a `None`);
/// a `json.Value` is decoded as a written `json.Decode` impl is given
/// its part of the document.
pub(crate) const RULES: Rules = Rules {
    caller: "toml.parse",
    table: "table",
    variant: "String or table",
    numbers: Numbers::Kinds,
    family: Family::Toml,
    errors: [
        bv::TOML_TYPE_MISMATCH,
        bv::TOML_MISSING_FIELD,
        bv::TOML_OTHER,
    ],
    offsets: true,
    unknown: Some(bv::TOML_UNKNOWN_FIELD),
    from_impl,
};

/// The `TomlError` of the `JsonError` `error` that a written
/// `json.Decode` impl gave for a part `at` a path: the variant of the
/// same name, the path added as json's decoder adds it
/// ([`decode::at_part`]). A `Syntax`, whose offset is in the impl's
/// JSON text and not in the TOML document, is an `Other` of json's
/// message of it ("json syntax error at line 1, column 4: ...").
fn from_impl(error: &Value, at: &str) -> Value {
    let error = decode::at_part(error, at);
    let Value::Variant(json) = &error else {
        return error;
    };
    let fields = json.fields().to_vec();
    let tag = match () {
        _ if json.is(bv::JSON_TYPE_MISMATCH) => bv::TOML_TYPE_MISMATCH,
        _ if json.is(bv::JSON_MISSING_FIELD) => bv::TOML_MISSING_FIELD,
        _ if json.is(bv::JSON_OTHER) => bv::TOML_OTHER,
        _ if json.is(bv::JSON_SYNTAX) => {
            let message =
                super::json::error_text(json.name(), &fields).unwrap_or_else(|| error.to_string());
            let [offset, line, column] = decode::NO_PLACE;
            return Value::variant(
                bv::TOML_OTHER,
                [Value::string(message), offset, line, column],
            );
        }
        _ => return error,
    };
    Value::variant(tag, fields)
}

// ── The functions ───────────────────────────────────────────────────

static PARSE: Decoder = Decoder::new("toml.parse");
static PARSE_LIST: Decoder = Decoder::new("toml.parse_list");
static PARSE_MAP: Decoder = Decoder::new("toml.parse_map");

/// `value` as TOML text, for `toml.<name>`, `pretty` or not:
/// `Ok(text)`, or the `TomlError` of a value that TOML has no form for.
/// (A part of a type with a written `json.Encode` impl is written as
/// the `json.Value` the impl gives, which is the program's code: a
/// step.)
fn written(vm: &mut Vm, name: &str, value: &Value, pretty: bool) -> Result<Step, VmError> {
    let caller = format!("toml.{name}");
    vm.encode(&caller.clone(), Format::Toml, value, move |_, data| {
        Ok(Step::Done(text(&caller, data, pretty)?))
    })
}

/// The fewest bytes the text of `data` takes: its strings and keys, and
/// a byte for each other part. The TOML writer writes into a `String`
/// that cannot be stopped, so a text that would be longer than a string
/// may be is refused on this count before it is written (the data can
/// hold one long string many times), and on its length after.
fn least_len(data: &Data) -> usize {
    match data {
        Data::String(s) => s.len(),
        Data::Array(items) | Data::Tuple(items) => items
            .iter()
            .fold(1, |len, item| len.saturating_add(least_len(item))),
        Data::Table(entries) => entries.iter().fold(1, |len, (key, value)| {
            len.saturating_add(key.len())
                .saturating_add(least_len(value))
        }),
        _ => 1,
    }
}

/// The TOML text `caller` writes of `data`, `pretty` or not:
/// `Ok(text)`, or the `TomlError` of a value that TOML has no form for
/// (the error the writer gave in place of the data, if it gave one).
/// A text longer than a string may be is a runtime error.
fn text(caller: &str, data: Result<Data, VmError>, pretty: bool) -> Result<Value, VmError> {
    let data = match data.and_then(|d| top_level(caller, d)) {
        Ok(data) => data,
        Err(e) => return Ok(toml_unknown_err(e.message)),
    };
    let least = least_len(&data);
    if least > crate::value::MAX_TEXT_LEN {
        return Err(super::common::too_long_text(caller, least));
    }
    match toml_text(&data, pretty) {
        Ok(s) if s.len() > crate::value::MAX_TEXT_LEN => {
            Err(super::common::too_long_text(caller, s.len()))
        }
        Ok(s) => Ok(Value::variant(bv::OK, [Value::string(s)])),
        Err(e) => Ok(toml_unknown_err(format!("{caller}: {e}"))),
    }
}

/// The TOML text of `data`, a table, as the `toml` crate writes a
/// document (each value that holds tables as a `[table]` or
/// `[[array]]` of its own), but for two things: a Float is written as
/// `json.stringify` writes it (`1e+300`, not 301 digits), and in the
/// `pretty` form an array of two or more values has a line for each,
/// indented one level deeper than the array it is in (the crate
/// indents every one by four spaces, an array in an array too). An
/// array in an inline table stays on its line: an inline table is one
/// line in TOML.
fn toml_text(data: &Data, pretty: bool) -> Result<String, ::toml_edit::ser::Error> {
    use serde::Serialize;
    use toml_edit::visit_mut::VisitMut;
    let value = Toml(data).serialize(toml_edit::ser::ValueSerializer::new())?;
    let mut table = toml_edit::Item::Value(value)
        .into_table()
        .map_err(|_| toml_edit::ser::Error::UnsupportedType(None))?;
    Layout {
        pretty,
        ..Layout::default()
    }
    .visit_table_mut(&mut table);
    Ok(toml_edit::DocumentMut::from(table).to_string())
}

/// How [`toml_text`] lays a document out.
#[derive(Default)]
struct Layout {
    pretty: bool,
    /// In a value: no table in it becomes a `[table]` of its own.
    in_value: bool,
    /// In an inline table: every array in it stays on one line.
    inline: bool,
    /// The arrays around, each on lines of its own.
    depth: usize,
}

impl toml_edit::visit_mut::VisitMut for Layout {
    fn visit_item_mut(&mut self, node: &mut toml_edit::Item) {
        let outer = self.in_value;
        if !outer {
            let item = std::mem::take(node);
            let item = item.into_table().map_or_else(|i| i, toml_edit::Item::Table);
            let item =
                (item.into_array_of_tables()).map_or_else(|i| i, toml_edit::Item::ArrayOfTables);
            self.in_value = item.is_value();
            *node = item;
        }
        toml_edit::visit_mut::visit_item_mut(self, node);
        self.in_value = outer;
    }

    fn visit_table_mut(&mut self, node: &mut toml_edit::Table) {
        node.decor_mut().clear();
        // (An empty table may mean something: it is written.)
        if !node.is_empty() {
            node.set_implicit(true);
        }
        toml_edit::visit_mut::visit_table_mut(self, node);
    }

    fn visit_value_mut(&mut self, node: &mut toml_edit::Value) {
        if let toml_edit::Value::Float(f) = node
            && let Some(written) = json_float(*f.value())
            && let Ok(value) = written.parse::<toml_edit::Value>()
        {
            *node = value;
        }
        node.decor_mut().clear();
        toml_edit::visit_mut::visit_value_mut(self, node);
    }

    fn visit_inline_table_mut(&mut self, node: &mut toml_edit::InlineTable) {
        let outer = std::mem::replace(&mut self.inline, true);
        toml_edit::visit_mut::visit_inline_table_mut(self, node);
        self.inline = outer;
    }

    fn visit_array_mut(&mut self, node: &mut toml_edit::Array) {
        let lines = self.pretty && !self.inline && node.len() > 1;
        let depth = self.depth;
        self.depth += usize::from(lines);
        toml_edit::visit_mut::visit_array_mut(self, node);
        self.depth = depth;
        match lines {
            true => {
                let indent = "    ".repeat(depth + 1);
                for item in node.iter_mut() {
                    item.decor_mut().set_prefix(format!("\n{indent}"));
                }
                node.set_trailing(format!("\n{}", "    ".repeat(depth)));
                node.set_trailing_comma(true);
            }
            false => {
                node.set_trailing("");
                node.set_trailing_comma(false);
            }
        }
    }
}

/// The text `json.stringify` writes of the Float `f` where it is not
/// the crate's (an exponent: `1e+300`, `1e-7`); None where it is.
fn json_float(f: f64) -> Option<String> {
    let text = serde_json::to_string(&f).ok()?;
    (f.is_finite() && text.contains('e')).then_some(text)
}

/// The TOML text `s` decoded by `r` as `top` says, read once
/// (`decode::seed`), its parts of a type with a written impl as `parts`
/// says: what the document decodes into, or `Err` of the call's result
/// where the text is no document.
///
/// A date or time literal that is no day or time of the calendar
/// (`1979-02-29`, `24:00:00`) is valid TOML in its form, but the `toml`
/// crate refuses it while parsing. So that it reads as such a text does
/// in quotes, with the one message of a date or time that is none
/// (and as a literal: a datetime, for a `String`), the literal is
/// written as a marked string ([`marked`]) and the document read
/// again; the marked string decodes as the literal would. Only a text
/// silt's own reader refuses too is marked, and only so many: what is
/// left is the crate's error, at its place in `s`. (A document whose
/// marked strings are more than the ones written here, which no
/// document can be, is refused with the first error.)
pub(crate) fn read_text(
    r: &decode::Reader,
    top: &seed::Top,
    s: &str,
    parts: &seed::Parts,
) -> Result<seed::Outcome, Value> {
    let read = |text: &str| seed::toml_text(r, top, text, marked(), parts);
    // (The marked strings, counted where any was written.)
    let marked_in = |_: &seed::Outcome, text: &str| {
        ::toml::from_str::<::toml::Value>(text).map_or(usize::MAX, |doc| marked_strings(&doc))
    };
    // An error's value found in the text read, at its place in `s`.
    let located = |mut outcome: seed::Outcome, text: &str, in_s: &dyn Fn(usize) -> usize| {
        if let Err(e) = &mut outcome {
            e.locate(|path, key| in_s(offset_at(text, path, key)));
            e.count_lines(s.as_bytes());
        }
        outcome
    };
    marking(s, read, marked_in, located)
}

/// The byte offset in the TOML text `text`, a document, of the value at
/// `path`: of a table, its header (`[a]`, `[[a]]`), the `{` of an inline
/// one, or the key of one written by dotted keys (`a.b = 1`); as far as
/// the path leads in the text, 0 for the document. Where `key`, the key
/// at the end of the path is meant, and found where it is written.
fn offset_at(text: &str, path: &[decode::Seg], key: bool) -> usize {
    use toml_edit::{Item, Table, Value as V};
    /// A part of the document.
    #[derive(Clone, Copy)]
    enum Node<'d> {
        Item(&'d Item),
        Table(&'d Table),
        Value(&'d V),
    }
    let Ok(doc) = toml_edit::ImDocument::parse(text) else {
        return 0;
    };
    let mut node = Node::Table(doc.as_table());
    let mut at = 0;
    for (k, seg) in path.iter().enumerate() {
        let node_table = match node {
            Node::Table(table) | Node::Item(Item::Table(table)) => Some(table),
            _ => None,
        };
        let value = match node {
            Node::Value(value) | Node::Item(Item::Value(value)) => Some(value),
            _ => None,
        };
        let next = match (seg, node_table, value, node) {
            (decode::Seg::Key(key), Some(table), _, _) => table
                .get_key_value(key)
                .map(|(key, item)| (Node::Item(item), key.span())),
            (decode::Seg::Key(key), _, Some(V::InlineTable(table)), _) => table
                .get_key_value(key)
                .map(|(key, item)| (Node::Item(item), key.span())),
            (decode::Seg::Index(index), _, Some(V::Array(array)), _) => {
                array.get(*index).map(|value| (Node::Value(value), None))
            }
            (decode::Seg::Index(index), _, _, Node::Item(Item::ArrayOfTables(tables))) => {
                tables.get(*index).map(|table| (Node::Table(table), None))
            }
            _ => None,
        };
        let Some((next, key_span)) = next else {
            break;
        };
        if key
            && k + 1 == path.len()
            && let Some(span) = key_span
        {
            return span.start;
        }
        let own = match next {
            Node::Item(Item::Table(table)) | Node::Table(table) => table.span(),
            Node::Item(Item::Value(value)) | Node::Value(value) => value.span(),
            Node::Item(Item::ArrayOfTables(tables)) => tables.span(),
            Node::Item(Item::None) => None,
        };
        if let Some(span) = own.or(key_span) {
            at = span.start;
        }
        node = next;
    }
    at
}

/// A TOML text read again, decoded as `toml.parse` (`list`: `None`),
/// `toml.parse_list` (`Some` of the element type as its error shows it)
/// or `toml.parse_map` (`map`) decodes it.
struct Text {
    text: String,
    list: Option<String>,
    map: bool,
}

impl Source for Text {
    fn read(
        &self,
        r: &decode::Reader,
        expected: &FieldType,
        parts: &seed::Parts,
    ) -> Result<seed::Outcome, Value> {
        let keys = |n: usize| not_one_key(self.list.as_deref().unwrap_or_default(), n);
        let top = match (&self.list, self.map) {
            (Some(_), _) => seed::Top::List(expected, &keys),
            (None, true) => seed::Top::Map(expected),
            (None, false) => seed::Top::One(expected),
        };
        read_text(r, &top, &self.text, parts)
    }
}

/// What `parse` reads of the TOML text `s`, its date and time literals
/// that are none marked as [`document`] says, or the result of a text
/// that is none. `marked_in` counts the marked strings of what was read
/// of a text.
fn marking<T>(
    s: &str,
    mut parse: impl FnMut(&str) -> Result<T, ::toml::de::Error>,
    marked_in: impl Fn(&T, &str) -> usize,
    finish: impl FnOnce(T, &str, &dyn Fn(usize) -> usize) -> T,
) -> Result<T, Value> {
    const MOST: usize = 64;
    let mut text = std::borrow::Cow::Borrowed(s);
    // Where each marked literal ends in `text`, and how many bytes it
    // grew by: an offset in `text` less the growth before it is the
    // offset in `s`.
    let mut grown: Vec<(usize, usize)> = Vec::new();
    let mut first = None;
    loop {
        match parse(&text) {
            Ok(doc) => {
                return match first {
                    Some(e) if marked_in(&doc, &text) != grown.len() => {
                        Err(toml_de_result_err(&e, &text))
                    }
                    _ => {
                        let in_s = |at: usize| {
                            let growth: usize = (grown.iter())
                                .filter(|(end, _)| *end <= at)
                                .map(|(_, by)| by)
                                .sum();
                            at.saturating_sub(growth)
                        };
                        Ok(finish(doc, &text, &in_s))
                    }
                };
            }
            Err(e) => {
                let at = e.span().map_or(0, |span| span.start);
                let literal = (grown.len() < MOST)
                    .then(|| none_literal_at(&text, at))
                    .flatten();
                let Some(range) = literal else {
                    let growth: usize = grown
                        .iter()
                        .filter(|(end, _)| *end <= at)
                        .map(|(_, by)| by)
                        .sum();
                    return Err(toml_err_wrap(syntax(
                        &e,
                        &text,
                        at,
                        s,
                        at.saturating_sub(growth),
                    )));
                };
                first.get_or_insert(e);
                let source = marked().replace('\u{0}', "\\u0000");
                let marked = format!("\"{source}{}\"", &text[range.clone()]);
                let by = marked.len() - range.len();
                let mut next = String::with_capacity(text.len() + by);
                next.push_str(&text[..range.start]);
                next.push_str(&marked);
                next.push_str(&text[range.end..]);
                grown.push((range.start + marked.len(), by));
                text = std::borrow::Cow::Owned(next);
            }
        }
    }
}

/// The start of a string that stands for a date or time literal that
/// is none ([`document`]): it holds a number drawn once per process, so
/// no document can know it.
fn marked() -> &'static str {
    static MARKED: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    MARKED.get_or_init(|| {
        use std::hash::{BuildHasher, Hasher};
        let drawn = || {
            std::collections::hash_map::RandomState::new()
                .build_hasher()
                .finish()
        };
        format!("\u{0}silt:datetime:{:016x}{:016x}\u{0}", drawn(), drawn())
    })
}

/// How many strings of `doc` are marked ([`marked`]).
fn marked_strings(doc: &::toml::Value) -> usize {
    match doc {
        ::toml::Value::String(s) => usize::from(s.starts_with(marked())),
        ::toml::Value::Array(items) => items.iter().map(marked_strings).sum(),
        ::toml::Value::Table(table) => table.values().map(marked_strings).sum(),
        _ => 0,
    }
}

/// Where the comment of the TOML line `line` starts: its first `#`
/// outside a string.
fn comment_in(line: &str) -> Option<usize> {
    let mut quote = None;
    let mut escaped = false;
    for (at, c) in line.char_indices() {
        match (quote, c) {
            (Some('"'), '\\') if !escaped => {
                escaped = true;
                continue;
            }
            (Some(q), c) if c == q && !escaped => quote = None,
            (None, '"' | '\'') => quote = Some(c),
            (None, '#') => return Some(at),
            _ => {}
        }
        escaped = false;
    }
    None
}

/// The date or time literal of `text` that the parser's error at `at`
/// is in, if it is one in a value's place whose text silt's reader
/// refuses as a date, a time and a datetime alike: one that is no day
/// or time of the calendar.
fn none_literal_at(text: &str, at: usize) -> Option<std::ops::Range<usize>> {
    static LITERAL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"^(?:[0-9]{4}-[0-9]{2}-[0-9]{2}(?:[Tt ][0-9]{2}:[0-9]{2}(?::[0-9]{2}(?:\.[0-9]+)?)?(?:[Zz]|[+-][0-9]{2}:[0-9]{2})?)?|[0-9]{2}:[0-9]{2}(?::[0-9]{2}(?:\.[0-9]+)?)?)",
        )
        .expect("the pattern of a literal")
    });
    let bytes = text.as_bytes();
    let part = |b: u8| b.is_ascii_digit() || b"-:.+TtZz ".contains(&b);
    let mut start = at.min(bytes.len());
    while start > 0 && part(bytes[start - 1]) {
        start -= 1;
    }
    while start < at && bytes[start] == b' ' {
        start += 1;
    }
    const SPACE: [char; 4] = [' ', '\t', '\r', '\n'];
    let mut before = text[..start].trim_end_matches(SPACE);
    // (A comment at the end of a line before the literal is no part of
    // what comes before it.)
    loop {
        let line = before.rfind('\n').map_or(0, |at| at + 1);
        let Some(comment) = comment_in(&before[line..]) else {
            break;
        };
        before = before[..line + comment].trim_end_matches(SPACE);
    }
    if !before.ends_with(['=', '[', ',']) {
        return None;
    }
    let end = start + LITERAL.find(&text[start..])?.end();
    let ends = bytes.get(end).is_none_or(|b| b" \t\r\n,]}#".contains(b));
    let literal = &text[start..end];
    let none = decode::parse_date(literal).is_err()
        && decode::parse_time(literal).is_err()
        && decode::parse_datetime(literal).is_err();
    (at <= end && ends && none).then_some(start..end)
}

/// The message of `toml.parse_list` of a document of `keys` top-level
/// keys, not one; `shown` is its element type.
fn not_one_key(shown: &str, keys: usize) -> String {
    format!(
        "toml.parse_list({shown}): expected a document with exactly one top-level key, an array, found {keys} keys"
    )
}

// (A part of a type with a written `json.Decode` impl is decoded by the
// impl, which is the program's code: the decoders are steps.)
builtins! {
    fn parse(vm, s: &str, ty: Type) -> Result<Step, VmError> {
        let expected = decode::target(&PARSE, ty)?.element();
        let read = |r: &decode::Reader, expected: &FieldType, parts: &seed::Parts| {
            read_text(r, &seed::Top::One(expected), s, parts)
        };
        let source = || -> Box<dyn Source> {
            Box::new(Text { text: s.to_string(), list: None, map: false })
        };
        vm.decode_read(PARSE.name, &RULES, expected, read, source)
    }

    fn parse_list(vm, s: &str, ty: Type) -> Result<Step, VmError> {
        let element = decode::target(&PARSE_LIST, ty)?.element();
        let shown = shown(&vm.runtime, &element);
        let keys = |n: usize| not_one_key(&shown, n);
        let read = |r: &decode::Reader, element: &FieldType, parts: &seed::Parts| {
            read_text(r, &seed::Top::List(element, &keys), s, parts)
        };
        let source = || -> Box<dyn Source> {
            Box::new(Text { text: s.to_string(), list: Some(shown.clone()), map: false })
        };
        vm.decode_read(PARSE_LIST.name, &RULES, element, read, source)
    }

    fn parse_map(vm, s: &str, ty: Type) -> Result<Step, VmError> {
        let element = decode::target(&PARSE_MAP, ty)?.element();
        let read = |r: &decode::Reader, element: &FieldType, parts: &seed::Parts| {
            read_text(r, &seed::Top::Map(element), s, parts)
        };
        let source = || -> Box<dyn Source> {
            Box::new(Text { text: s.to_string(), list: None, map: true })
        };
        vm.decode_read(PARSE_MAP.name, &RULES, element, read, source)
    }

    fn stringify(vm, value: &Value) -> Result<Step, VmError> {
        written(vm, "stringify", value, false)
    }

    fn pretty(vm, value: &Value) -> Result<Step, VmError> {
        written(vm, "pretty", value, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A field whose type holds `Bytes` has no decoder, whatever the
    /// document holds there, an empty array too, or nothing.
    #[test]
    fn a_field_that_holds_bytes_is_never_decoded() {
        use crate::defs::{DefId, TypeId};
        let vm = crate::vm::Vm::new(crate::vm::HostIo::process());
        let fields = vec![("b".to_string(), FieldType::List(Box::new(FieldType::Bytes)))];
        let ty = crate::typeinfo::TypeInfo::new_record(TypeId(DefId(9000)), "B", fields.clone());
        for text in ["b = []", "b = [1]", ""] {
            // As toml.parse decodes it: the type is refused before the
            // document is read.
            let target = decode::Target::Record(&ty);
            let reader = decode::Reader::new(&vm.runtime);
            let decoded = decode::refused(&reader, &RULES, &target).unwrap_or_else(|| {
                let element = target.element();
                let top = seed::Top::One(&element);
                match read_text(&reader, &top, text, &seed::Parts::finding()) {
                    Ok(decoded) => decode::result(&RULES, decoded),
                    Err(result) => result,
                }
            });
            assert_eq!(
                decoded.to_string(),
                "Err(cannot decode field `b` of `B`: a value of type List(Bytes) cannot be decoded)",
                "{text}"
            );
        }
    }
    use crate::builtins::data::MAX_ENCODE_DEPTH;

    /// An error has the line and the character column of its byte, and
    /// an unknown key's names the keys its table takes: the shown text
    /// says the line and the column, not the byte.
    #[test]
    fn an_error_says_its_line_and_column() {
        let text = |name: &str, fields: Vec<Value>| error_text(name, &fields).expect(name);
        let place = |offset, line, column| decode::place(offset, line, column).to_vec();
        let keys = Value::list(vec![Value::string("match"), Value::string("account")]);
        let unknown = [
            vec![Value::string("rules[0].macth"), keys],
            place(182, 10, 1),
        ]
        .concat();
        assert_eq!(
            text("UnknownField", unknown),
            "toml unknown field at line 10, column 1: rules[0].macth, expected match or account"
        );
        let one = Value::list(vec![Value::string("title")]);
        let unknown = [vec![Value::string("x"), one], place(0, 1, 1)].concat();
        assert_eq!(
            text("UnknownField", unknown),
            "toml unknown field at line 1, column 1: x, expected title"
        );
        let three = ["a", "b", "c"].map(Value::string).to_vec();
        let unknown = [vec![Value::string("x"), Value::list(three)], place(0, 1, 1)].concat();
        assert_eq!(
            text("UnknownField", unknown),
            "toml unknown field at line 1, column 1: x, expected a, b or c"
        );
        let unknown = [
            vec![Value::string("x"), Value::list(vec![])],
            place(0, 1, 1),
        ]
        .concat();
        assert_eq!(
            text("UnknownField", unknown),
            "toml unknown field at line 1, column 1: x, expected no key"
        );
        let syntax = [vec![Value::string("bad")], place(9, 2, 4)].concat();
        assert_eq!(
            text("Syntax", syntax),
            "toml syntax error at line 2, column 4: bad"
        );
        // (An error of no place in a text shows none.)
        let mismatch = [
            vec![Value::string("Int"), Value::string("string at a")],
            place(0, 0, 0),
        ];
        assert_eq!(
            text("TypeMismatch", mismatch.concat()),
            "toml type mismatch: expected Int, got string at a"
        );
        // (The old shape, without a line and a column, is no TomlError.)
        assert_eq!(
            error_text("Syntax", &[Value::string("bad"), Value::Int(9)]),
            None
        );
    }

    /// The offset of a value is where it starts; of a table, its header,
    /// the `{` of an inline one, the key of a dotted one; of the document,
    /// 0; of a path that leads out of the text, as far as it leads.
    #[test]
    fn a_value_is_found_where_it_starts() {
        use decode::Seg::{Index, Key};
        let key = |k: &str| Key(k.to_string());
        let text =
            "a = 1\n[t]\nx = \"s\"\n[[r]]\nn = 1\n[[r]]\nn = [1, 2]\nd.e = 3\ni = { j = 4 }\n";
        let at = |path: &[decode::Seg]| offset_at(text, path, false);
        assert_eq!(at(&[]), 0);
        assert_eq!(at(&[key("a")]), 4);
        assert_eq!(at(&[key("t")]), 6);
        assert_eq!(at(&[key("t"), key("x")]), 14);
        assert_eq!(at(&[key("r"), Index(1)]), 30);
        assert_eq!(at(&[key("r"), Index(1), key("n"), Index(1)]), 44);
        assert_eq!(at(&[key("r"), Index(1), key("n"), Index(9)]), 40);
        // (The keys after `[[r]]` are the last element's.)
        let last = |k: &str| [key("r"), Index(1), key(k)];
        assert_eq!(at(&last("d")), text.find("d.e").unwrap_or(0));
        assert_eq!(at(&last("i")), text.find('{').unwrap_or(0));
        let j = [key("r"), Index(1), key("i"), key("j")];
        assert_eq!(at(&j), text.find('4').unwrap_or(0));
        assert_eq!(at(&[key("none")]), 0);
        // A key, meant as itself, is where it is written.
        assert_eq!(offset_at(text, &[key("t"), key("x")], true), 10);
        assert_eq!(
            offset_at(text, &last("d"), true),
            text.find("d.e").unwrap_or(0)
        );
    }

    /// The deepest value the encoder writes (a map in a list in a map,
    /// and so on) is written on a stack of 16 MiB, the least a thread
    /// that runs silt code has (`scheduler::WORKER_STACK_BYTES` where
    /// pointers have 32 bits), in a debug build too; and a value one
    /// level deeper is refused with a `Other`.
    #[test]
    fn the_deepest_value_is_written_on_the_smallest_stack() {
        let (written, refused) = std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                // `levels` levels above the Int: a map at the even
                // ones, a list of it at the odd ones.
                let nested = |levels: usize| {
                    (0..levels)
                        .rev()
                        .fold(Value::Int(1), |inner, level| match level % 2 {
                            0 => Value::map([(Value::string("k"), inner)]),
                            _ => Value::list(vec![inner]),
                        })
                };
                let written = |caller: &str, value: &Value, pretty| {
                    text(caller, data::data(caller, Format::Toml, value), pretty)
                        .expect("no longer than a string may be")
                };
                let text = written("toml.stringify", &nested(MAX_ENCODE_DEPTH), false);
                let pretty = written("toml.pretty", &nested(MAX_ENCODE_DEPTH), true);
                let refused = written("toml.pretty", &nested(MAX_ENCODE_DEPTH + 1), true);
                (
                    [text, pretty].map(|v| v.to_string().len()),
                    format!("{refused:?}"),
                )
            })
            .expect("a thread")
            .join()
            .expect("the thread ends");
        assert!(
            written.iter().all(|len| *len > MAX_ENCODE_DEPTH),
            "{written:?}"
        );
        assert_eq!(
            refused,
            format!(
                "Err(Other(\"toml.pretty: the value is nested more than \
                 {MAX_ENCODE_DEPTH} levels deep\", 0, 0, 0))"
            )
        );
    }
}
