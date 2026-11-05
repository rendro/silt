//! The decoder: a document decoded as its parser reads it, a value of
//! the type made of the text part by part, with no tree of the document
//! in between (a `DeserializeSeed` of the type, over the parser of the
//! text). One decoder for every format: JSON text (`json.parse`, a
//! postgres `json` or `jsonb` cell), TOML text (`toml.parse`,
//! `toml.parse_list`, `toml.parse_map`), and a `json.Value`
//! (`json.decode`, `T.decode`).
//!
//! What a program sees:
//!
//! - broken text anywhere wins over a value of the wrong type before
//!   it: the error of a part is kept, and the text is read to its end;
//! - of a key written twice in an object the last value counts, and its
//!   error with it: an earlier one's error is dropped;
//! - a record's error is that of the first field it declares that has
//!   one, a map's that of the first key in sorted order, a list's that
//!   of the first element that has one (the decoder's order: fields as
//!   the type declares them, keys sorted, elements in order);
//! - a JSON number is an `Int` by its digits, whatever its form; a TOML
//!   number by its kind;
//! - a TOML date or time literal is a datetime, which the date and time
//!   types read from its text (the parser gives it as a table of one
//!   key, its mark, [`DATETIME`]).
//!
//! A part of a type with a written `json.Decode` impl is the impl's: a
//! document with such parts is read twice ([`Parts`], `vm::decode`),
//! first to find them, the json.Value of each, in the decoder's order;
//! then, once the impls gave their values, to put those in their
//! places.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use serde::de::value::{MapDeserializer, SeqDeserializer};
use serde::de::{self, DeserializeSeed, IntoDeserializer, MapAccess, SeqAccess, Visitor};

use super::{
    At, Error, Hole, I64_MAX_PLUS_ONE, Numbers, Reader, Rules, decodable_enum, decodable_record,
    descriptor, field_record_type, invalid_date_message, invalid_time_message, parse_date,
    parse_datetime, parse_time, shown, unsupported_field_type_message, variant_field_message,
};
use crate::builtins::registry::decoding::Leaf;
use crate::builtins::time::{make_date, make_datetime, make_time};
use crate::builtins::{json, toml};
use crate::typeinfo::{FieldType, Shape, Tag, TypeInfo, bv};
use crate::value::{Float, Value};

/// What a part of a document decodes into: its value, or the decoder's
/// error of it. (An error of the text is the parser's.)
pub(crate) type Outcome = Result<Value, Error>;

/// What a reading does at the parts of a document of a type with a
/// written `json.Decode` impl, each numbered in the order the reading
/// meets it (the same in both readings of a document).
pub(crate) struct Parts {
    /// How many parts the reading met.
    met: Cell<usize>,
    /// Finding (the first reading): each part found, with its number,
    /// in the decoder's order (each record, map, tuple and variant puts
    /// its own parts in that order when it ends), up to the first error
    /// of the document.
    found: RefCell<Vec<Found>>,
    /// Supplying (the second reading): what the impls gave, by number;
    /// `None` for a part no impl was called for.
    given: Option<Vec<Option<Result<Value, Value>>>>,
    /// Whether every variant's fields are kept as read, decoded when
    /// its table ends: so a JSON text that gives a variant's name or
    /// fields twice is read (the last of each counts).
    keep_fields: Cell<bool>,
    /// Whether the reading makes the value; a first reading of a
    /// document a second reading will make the value of builds nothing
    /// but the parts' `json.Value`s, finding the first error all the
    /// same.
    build: bool,
}

/// A part found by the first reading.
pub(crate) struct Found {
    /// The number of the part.
    pub(crate) number: usize,
    pub(crate) hole: Hole,
}

impl Parts {
    /// The first reading's: finding the parts.
    pub(crate) fn finding() -> Parts {
        Parts {
            met: Cell::new(0),
            found: RefCell::new(Vec::new()),
            given: None,
            keep_fields: Cell::new(false),
            build: true,
        }
    }

    /// The first reading's of a document whose value a second reading
    /// makes: finding the parts, building nothing else.
    pub(crate) fn only_finding() -> Parts {
        Parts {
            build: false,
            ..Parts::finding()
        }
    }

    /// Whether the reading made the value of the document.
    pub(crate) fn built(&self) -> bool {
        self.build
    }

    /// The second reading's: supplying, in the places of the parts
    /// `numbers` (in the decoder's order, as found), what the impls gave
    /// for them, `given`, in that order (up to the first `Err`).
    /// `keep_fields`: as the first reading kept the variants' fields.
    pub(crate) fn supplying(
        numbers: &[usize],
        given: Vec<Result<Value, Value>>,
        keep_fields: bool,
    ) -> Parts {
        let mut by_number = vec![None; numbers.iter().max().map_or(0, |n| n + 1)];
        for (&number, given) in numbers.iter().zip(given) {
            by_number[number] = Some(given);
        }
        Parts {
            given: Some(by_number),
            keep_fields: Cell::new(keep_fields),
            ..Parts::finding()
        }
    }

    /// The parts found, in the decoder's order, and whether the variants'
    /// fields were kept.
    pub(crate) fn into_found(self) -> (Vec<Found>, bool) {
        (self.found.into_inner(), self.keep_fields.get())
    }

    /// As before a reading again from the start.
    fn restart(&self) {
        self.met.set(0);
        self.found.borrow_mut().clear();
    }

    /// Where the parts found from now on begin.
    fn mark(&self) -> usize {
        self.found.borrow().len()
    }

    /// The parts found from `start` on in the order of `ranges` (each a
    /// range of them, as they were found), the others dropped.
    fn reorder(&self, start: usize, ranges: impl IntoIterator<Item = Range<usize>>) {
        let mut found = self.found.borrow_mut();
        if found.len() == start {
            return;
        }
        let mut found_since: Vec<Option<Found>> = found.drain(start..).map(Some).collect();
        for range in ranges {
            for part in &mut found_since[range.start - start..range.end - start] {
                found.extend(part.take());
            }
        }
    }
}

/// Whether a written impl decodes a part of a value of `expected` (a
/// record or enum type, at any depth, generic ones at their arguments),
/// as `r` sees the impls and decoders of `rules` the types. (Past so
/// many types it says so, to be safe.)
pub(crate) fn reaches_written(r: &Reader, rules: &Rules, expected: &FieldType) -> bool {
    fn reaches(r: &Reader, rules: &Rules, ty: &FieldType, seen: &mut Vec<FieldType>) -> bool {
        let (id, args) = match ty {
            FieldType::Option(inner) | FieldType::List(inner) | FieldType::Map(inner) => {
                return reaches(r, rules, inner, seen);
            }
            FieldType::Tuple(elems) => {
                return elems.iter().any(|elem| reaches(r, rules, elem, seen));
            }
            FieldType::Record(id) | FieldType::Enum(id) => (id, &[][..]),
            FieldType::Applied(id, args) => (id, args.as_slice()),
            _ => return false,
        };
        if r.impls
            .as_ref()
            .is_some_and(|impls| (impls.written)(*id, args).is_some())
        {
            return true;
        }
        if seen.contains(ty) {
            return false;
        }
        if seen.len() > 256 {
            return true;
        }
        seen.push(ty.clone());
        let Ok(info) = field_record_type(r, rules.caller, *id) else {
            return false;
        };
        let fields: Vec<&FieldType> = match &info.shape {
            Shape::Enum(_) => (info.variants().iter()).flat_map(|v| &v.fields).collect(),
            _ => info.fields().iter().map(|(_, field)| field).collect(),
        };
        (fields.into_iter()).any(|field| reaches(r, rules, &field.instantiated(args), seen))
    }
    reaches(r, rules, expected, &mut Vec::new())
}

/// The JSON text `text` decoded into a value of `expected` by `r`, its
/// parts of a type with a written impl as `parts` says: the decoder's
/// outcome, or the parser's error.
pub(crate) fn json_text(
    r: &Reader,
    expected: &FieldType,
    text: &[u8],
    parts: &Parts,
) -> Result<Outcome, serde_json::Error> {
    loop {
        let at = Cell::new(0);
        let restart = Cell::new(false);
        let kept = Cell::new(None);
        let cx = Cx {
            r,
            rules: &json::RULES,
            format: Format::Json,
            digits: Digits::Text(json::Numerals::new(text, &at)),
            marked: None,
            parts,
            restart: &restart,
            kept: &kept,
        };
        let mut de = serde_json::Deserializer::from_slice(text);
        let read = Seed {
            cx: &cx,
            expected,
            at: &At::Top,
        }
        .deserialize(&mut de)
        .and_then(|decoded| de.end().map(|()| decoded));
        if !restart.get() {
            return read;
        }
        // A variant's name or fields given twice: read again, every
        // variant's fields kept.
        parts.restart();
        parts.keep_fields.set(true);
    }
}

/// The JSON text `text` of a postgres cell decoded into a value of
/// `expected` by `r`, which does at the parts of a type with a written
/// impl what the reader of the row says: finding them, after the parts
/// of the cells before (`Holes::Collect`), or putting in the next of
/// what the impls gave (`Holes::Supply`), taking one at each part in
/// the decoder's order up to an impl's `Err`. `Err` of the parser's
/// error where the text is no document.
#[cfg(feature = "postgres")]
pub(crate) fn json_cell(
    r: &Reader,
    expected: &FieldType,
    text: &[u8],
) -> Result<Outcome, serde_json::Error> {
    use super::Holes;
    let Some(impls) = &r.impls else {
        return json_text(r, expected, text, &Parts::finding());
    };
    let finding = Parts::finding();
    let decoded = json_text(r, expected, text, &finding)?;
    let (found, keep_fields) = finding.into_found();
    let mut holes = impls.holes.borrow_mut();
    match &mut *holes {
        Holes::Collect(holes) => {
            holes.extend(found.into_iter().map(|found| found.hole));
            Ok(decoded)
        }
        Holes::Supply(values) => {
            let numbers: Vec<usize> = found.iter().map(|found| found.number).collect();
            let mut given = Vec::new();
            for _ in &numbers {
                let Some(value) = values.next() else { break };
                let failed = value.is_err();
                given.push(value);
                if failed {
                    break;
                }
            }
            drop(holes);
            let supplying = Parts::supplying(&numbers, given, keep_fields);
            json_text(r, expected, text, &supplying)
        }
    }
}

/// The `json.Value` `v` decoded into a value of `expected` by `r`, its
/// parts of a type with a written impl as `parts` says (`json.decode`,
/// `T.decode`): read as the JSON document `json.parse` read it from, a
/// `json.Int` an integer and a `json.Float` a float whatever its value
/// (so no `Int`); or `Err` of the message of a value nested past the
/// encoder's limit.
pub(crate) fn json_value(
    r: &Reader,
    expected: &FieldType,
    v: &Value,
    parts: &Parts,
) -> Result<Outcome, String> {
    let restart = Cell::new(false);
    let kept = Cell::new(None);
    let cx = Cx {
        r,
        rules: &json::RULES,
        format: Format::Json,
        digits: Digits::Read,
        marked: None,
        parts,
        restart: &restart,
        kept: &kept,
    };
    let read = Seed {
        cx: &cx,
        expected,
        at: &At::Top,
    }
    .deserialize(JsonValueDe {
        value: v.clone(),
        level: 0,
    });
    // (A `json.Value` has each key of an object once: the one error of
    // its reading is its depth.)
    read.map_err(|e| e.to_string())
}

/// A `json.Value` `level` levels deep in the one decoded, read as the
/// JSON of it: `json.Int(n)` as an integer, a `json.Float` as a float
/// (as `json.parse` holds a number of a text), an object's `variant`
/// first (so that a variant's fields are decoded as they are read, not
/// kept: no other part of a decoding depends on the order of the
/// members of an object that has each key once). A value nested more
/// than the encoder writes is an error, of its message.
struct JsonValueDe {
    value: Value,
    level: usize,
}

impl<'de> de::Deserializer<'de> for JsonValueDe {
    type Error = de::value::Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        use crate::builtins::data::MAX_ENCODE_DEPTH;
        if self.level > MAX_ENCODE_DEPTH {
            return Err(de::Error::custom(format_args!(
                "json.decode: the value is nested more than {MAX_ENCODE_DEPTH} levels deep"
            )));
        }
        let level = self.level + 1;
        let Value::Variant(json) = &self.value else {
            return Err(de::Error::custom(
                "json.decode: a value that is no json.Value",
            ));
        };
        match json.fields() {
            [] => visitor.visit_unit(),
            [Value::Bool(b)] => visitor.visit_bool(*b),
            [Value::Int(n)] => visitor.visit_i64(*n),
            [Value::Float(f)] => visitor.visit_f64(f.get()),
            [Value::String(s)] => visitor.visit_str(s),
            [Value::List(items)] => {
                let items = items.iter().map(|value| JsonValueDe { value, level });
                let mut seq = SeqDeserializer::new(items);
                let read = visitor.visit_seq(&mut seq)?;
                seq.end()?;
                Ok(read)
            }
            [Value::Map(members)] => {
                fn key(key: &Value) -> Cow<'_, str> {
                    match key {
                        Value::String(key) => Cow::Borrowed(key),
                        other => Cow::Owned(other.to_string()),
                    }
                }
                let mut entries: Vec<(Cow<str>, JsonValueDe)> = Vec::with_capacity(members.len());
                for (name, value) in members.iter() {
                    let member = (
                        key(name),
                        JsonValueDe {
                            value: value.clone(),
                            level,
                        },
                    );
                    match member.0 == "variant" {
                        true => entries.insert(0, member),
                        false => entries.push(member),
                    }
                }
                let mut map = MapDeserializer::new(entries.into_iter());
                let read = visitor.visit_map(&mut map)?;
                map.end()?;
                Ok(read)
            }
            _ => Err(de::Error::custom(
                "json.decode: a value that is no json.Value",
            )),
        }
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map struct enum identifier ignored_any
    }
}

impl<'de> IntoDeserializer<'de, de::value::Error> for JsonValueDe {
    type Deserializer = Self;

    fn into_deserializer(self) -> Self {
        self
    }
}

/// What the top of a TOML document is decoded as.
pub(crate) enum Top<'t> {
    /// A value of the type (`toml.parse`).
    One(&'t FieldType),
    /// The list of values of the type that is the array under its one
    /// key (`toml.parse_list`); a document of more keys or none is the
    /// error `Other` of what the function says of how many it has.
    List(&'t FieldType, &'t dyn Fn(usize) -> String),
    /// A map of values of the type (`toml.parse_map`).
    Map(&'t FieldType),
}

/// The TOML text `text` decoded by `r` as `top` says, its parts of a
/// type with a written impl as `parts` says: the decoder's outcome, or
/// the parser's error. A string that starts with `marked` stands for
/// the date or time literal after it (`toml::document`).
pub(crate) fn toml_text(
    r: &Reader,
    top: &Top,
    text: &str,
    marked: &'static str,
    parts: &Parts,
) -> Result<Outcome, ::toml::de::Error> {
    // (A reading that ended in the parser's error is read again.)
    parts.restart();
    let restart = Cell::new(false);
    let kept = Cell::new(None);
    let cx = Cx {
        r,
        rules: &toml::RULES,
        format: Format::Toml,
        digits: Digits::Read,
        marked: Some(marked),
        parts,
        restart: &restart,
        kept: &kept,
    };
    let de = ::toml::Deserializer::new(text);
    let map;
    match top {
        Top::One(expected) => Seed {
            cx: &cx,
            expected,
            at: &At::Top,
        }
        .deserialize(de),
        Top::Map(element) => {
            map = FieldType::Map(Box::new((*element).clone()));
            let seed = Seed {
                cx: &cx,
                expected: &map,
                at: &At::Top,
            };
            Only { seed, table: true }.deserialize(de)
        }
        Top::List(element, keys) => ListUnderItsKey {
            cx: &cx,
            list: &FieldType::List(Box::new((*element).clone())),
            keys: *keys,
        }
        .deserialize(de),
    }
}

/// What a reading reads with.
#[derive(Clone, Copy)]
struct Cx<'c> {
    r: &'c Reader<'c>,
    rules: &'static Rules,
    format: Format,
    digits: Digits<'c>,
    /// The start of a TOML string that stands for a date or time
    /// literal.
    marked: Option<&'static str>,
    parts: &'c Parts,
    /// Set where a JSON text gives a variant's name or fields twice
    /// while its fields are decoded as they are read.
    restart: &'c Cell<bool>,
    /// What was kept of a part that is read again ([`Capture`]), handed
    /// over as it is.
    kept: &'c Cell<Option<Content>>,
}

impl Cx<'_> {
    /// The string `s` of the document: a TOML date or time literal where
    /// it is marked as one.
    fn string<'s>(&self, s: &'s str) -> Scalar<'s> {
        match self.marked.and_then(|marked| s.strip_prefix(marked)) {
            Some(literal) => Scalar::Datetime(literal),
            None => Scalar::String(s),
        }
    }

    /// The error that stops the reading, to read again with the
    /// variants' fields kept.
    fn for_a_restart<E: de::Error>(&self) -> E {
        self.restart.set(true);
        E::custom("a variant's key is given twice")
    }
}

/// What a reading does at a part of a type with a written impl it
/// meets.
enum Met {
    /// Find it: its number.
    Find(usize),
    /// Put in what the impl gave.
    Given(Outcome),
}

/// The format of a document, where formats differ in what a decoder
/// reads.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Json,
    Toml,
}

impl Format {
    /// What the format calls the kind of `scalar` in a type mismatch.
    fn kind(self, scalar: Scalar) -> &'static str {
        match (self, scalar) {
            (_, Scalar::Null) => "null",
            (_, Scalar::Bool(_)) => "boolean",
            (Format::Json, Scalar::Int(_) | Scalar::Float(_)) => "number",
            (Format::Toml, Scalar::Int(_)) => "integer",
            (Format::Toml, Scalar::Float(_)) => "float",
            (_, Scalar::String(_)) => "string",
            (_, Scalar::Datetime(_)) => "datetime",
        }
    }

    /// What the format calls the kind of a table (JSON's object).
    fn table(self) -> &'static str {
        match self {
            Format::Json => "object",
            Format::Toml => "table",
        }
    }
}

/// The one key of the table that the TOML parser gives a date or time
/// literal as, the value its text (`toml_datetime`'s, which is no
/// dependency of silt's own).
pub(crate) const DATETIME: &str = "$__toml_private_datetime";

/// Where a reading takes the numbers of a document from.
#[derive(Clone, Copy)]
enum Digits<'t> {
    /// From the JSON text: the parser gives every number that is no
    /// integer literal of 64 bits as an `f64`, so each number's digits
    /// are read again, in the order the parser gives the numbers.
    Text(json::Numerals<'t>),
    /// From the parser as it gives them: a TOML document, whose numbers
    /// are an integer or a float by their kind, or a `json.Value` or
    /// what was kept of a document, whose numbers are held as they
    /// decode.
    Read,
}

impl Digits<'_> {
    /// Past the number the parser gave as an integer.
    fn integer(self) {
        if let Digits::Text(node) = self {
            node.next_number();
        }
    }

    /// The number the parser gave as the `f64` `f`: of a JSON text, an
    /// `Int` when its digits are a whole number in `Int`'s range, and
    /// one that is no finite `f64` is held as no number.
    fn float(self, f: f64) -> Scalar<'static> {
        let Digits::Text(node) = self else {
            return Scalar::Float(f);
        };
        if let Some(n) = json::exact_int(node.next_number()) {
            return Scalar::Int(n);
        }
        match f.is_finite() {
            true => Scalar::Float(f),
            false => Scalar::Null,
        }
    }
}

/// A value of a document that holds no other.
#[derive(Clone, Copy)]
enum Scalar<'s> {
    Null,
    Bool(bool),
    /// A number that is an `Int`.
    Int(i64),
    /// Any other number.
    Float(f64),
    String(&'s str),
    /// A TOML date or time literal, as its text.
    Datetime(&'s str),
}

impl Scalar<'_> {
    /// The integer the parser gave, held as an `Int` when it is one.
    fn unsigned(n: u64) -> Scalar<'static> {
        match i64::try_from(n) {
            Ok(n) => Scalar::Int(n),
            Err(_) => Scalar::Float(n as f64),
        }
    }

    /// The `json.Value` that holds it.
    fn json(self) -> Value {
        match self {
            Scalar::Null => Value::variant(bv::JSON_NULL, []),
            Scalar::Bool(b) => Value::variant(bv::JSON_BOOL, [Value::Bool(b)]),
            Scalar::Int(n) => Value::variant(bv::JSON_INT, [Value::Int(n)]),
            Scalar::Float(f) => match Float::new(f) {
                Some(f) => Value::variant(bv::JSON_FLOAT, [Value::Float(f)]),
                None => Value::variant(bv::JSON_NULL, []),
            },
            // (A format with such literals decodes no `json.Value`.)
            Scalar::String(s) | Scalar::Datetime(s) => {
                Value::variant(bv::JSON_STRING, [Value::String(s.into())])
            }
        }
    }
}

/// What the first key of a table says the table is.
enum Opened<'de> {
    /// A TOML date or time literal, of this text.
    Datetime(String),
    /// A table: its entries, the first key read.
    Table(Option<Cow<'de, str>>),
}

/// The table `map` opened: under TOML, a table whose first key is
/// [`DATETIME`] is the literal whose text its value is, read as
/// `toml::Value` reads one: the value must be a string of a datetime
/// (else the parser's error), and what follows is not read.
fn open<'de, A: MapAccess<'de>>(cx: &Cx, map: &mut A) -> Result<Opened<'de>, A::Error> {
    let first = map.next_key_seed(Key)?;
    if cx.format != Format::Toml || first.as_deref() != Some(DATETIME) {
        return Ok(Opened::Table(first));
    }
    Ok(Opened::Datetime(map.next_value_seed(DatetimeText)?))
}

/// The value of [`DATETIME`]: the text of the datetime, as the `toml`
/// crate reads it.
struct DatetimeText;

impl<'de> DeserializeSeed<'de> for DatetimeText {
    type Value = String;

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<String, D::Error> {
        d.deserialize_str(self)
    }
}

impl<'de> Visitor<'de> for DatetimeText {
    type Value = String;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("string containing a datetime")
    }

    fn visit_str<E: de::Error>(self, s: &str) -> Result<String, E> {
        match s.parse::<::toml::value::Datetime>() {
            Ok(datetime) => Ok(datetime.to_string()),
            Err(e) => Err(de::Error::custom(e)),
        }
    }
}

/// The entries of a table, its first key read.
struct Entries<'de, A> {
    first: Option<Cow<'de, str>>,
    map: A,
}

impl<'de, A: MapAccess<'de>> Entries<'de, A> {
    /// The next key.
    fn key(&mut self) -> Result<Option<Cow<'de, str>>, A::Error> {
        match self.first.take() {
            Some(key) => Ok(Some(key)),
            None => self.map.next_key_seed(Key),
        }
    }

    /// The value of the key read last.
    fn value<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, A::Error> {
        self.map.next_value_seed(seed)
    }

    /// Every entry read, decoded into nothing.
    fn skip(mut self, skip: Skip<'_>) -> Result<(), A::Error> {
        while self.key()?.is_some() {
            self.value(skip)?;
        }
        Ok(())
    }
}

/// What a part of a document is decoded as, by its type.
enum Want<'s> {
    /// `None` for `null`, else `Some` of the type.
    Option(&'s FieldType),
    List(&'s FieldType),
    Map(&'s FieldType),
    Tuple(&'s [FieldType]),
    JsonValue,
    /// A record type at its type arguments.
    Record(Arc<TypeInfo>, &'s [FieldType]),
    /// An enum type at its type arguments.
    Variant(Arc<TypeInfo>, &'s [FieldType]),
    /// A string, a number, a boolean, a date or a time.
    Scalar,
    /// No value of the type, whatever the part is.
    Fails(Error),
}

/// The decoding of a part of a document, `at` a path, into a value of
/// `expected`.
#[derive(Clone, Copy)]
struct Seed<'s> {
    cx: &'s Cx<'s>,
    expected: &'s FieldType,
    at: &'s At<'s>,
}

impl<'s> Seed<'s> {
    /// The decoding of a part `at` a path into a value of `expected`.
    fn of<'p>(&'p self, expected: &'p FieldType, at: &'p At<'p>) -> Seed<'p> {
        Seed {
            cx: self.cx,
            expected,
            at,
        }
    }

    /// The reading of a part that is decoded into nothing.
    fn skip(&self) -> Skip<'s> {
        Skip(*self.cx)
    }

    /// The slot of the `decode` the program wrote for the type of the
    /// part, if it wrote one.
    fn written(&self) -> Option<u16> {
        let (id, args) = match self.expected {
            FieldType::Record(id) | FieldType::Enum(id) => (id, &[][..]),
            FieldType::Applied(id, args) => (id, args.as_slice()),
            _ => return None,
        };
        let impls = self.cx.r.impls.as_ref()?;
        (impls.written)(*id, args)
    }

    /// The part of a type with a written impl the reading meets: its
    /// number to find it by, or what the impl gave for it, as the
    /// format's error with the part's path where it gave an `Err`.
    fn met(&self) -> Met {
        let parts = self.cx.parts;
        let number = parts.met.get();
        parts.met.set(number + 1);
        let Some(given) = &parts.given else {
            return Met::Find(number);
        };
        Met::Given(match given.get(number).cloned().flatten() {
            Some(Ok(value)) => Ok(value),
            Some(Err(error)) => Err(Error::Given((self.cx.rules.from_impl)(
                &error,
                &self.at.to_string(),
            ))),
            None => Ok(Value::Unit),
        })
    }

    /// The part numbered `number`, of the impl in `slot`, found with
    /// its `json.Value`: a stand-in until the impl gives its value; or,
    /// where it has none (a TOML float that is no number), the error.
    fn found(&self, number: usize, slot: u16, json: Result<Value, Error>) -> Outcome {
        let json = json?;
        let ty = match self.expected {
            FieldType::Applied(..) => descriptor(self.cx.r, self.expected),
            _ => Value::Unit,
        };
        let hole = Hole { json, slot, ty };
        self.cx
            .parts
            .found
            .borrow_mut()
            .push(Found { number, hole });
        Ok(Value::Unit)
    }

    /// Whether the reading makes the value.
    fn build(&self) -> bool {
        self.cx.parts.build
    }

    /// `Some` of `value`, where the reading makes the value.
    fn some(&self, value: Value) -> Value {
        match self.build() {
            true => Value::variant(bv::SOME, [value]),
            false => Value::Unit,
        }
    }

    /// What the part is decoded as.
    fn want(&self) -> Want<'s> {
        let rules = self.cx.rules;
        let rt = self.cx.r;
        let unsupported = || Want::Fails(Error::Unsupported(shown(rt, self.expected)));
        match self.expected {
            FieldType::Option(inner) => Want::Option(inner),
            FieldType::List(inner) => Want::List(inner),
            FieldType::Map(inner) => Want::Map(inner),
            FieldType::Tuple(elems) => Want::Tuple(elems),
            FieldType::String
            | FieldType::Int
            | FieldType::Float
            | FieldType::Bool
            | FieldType::Date
            | FieldType::Time
            | FieldType::DateTime => Want::Scalar,
            FieldType::Unsupported(declared) => Want::Fails(Error::Unsupported(declared.clone())),
            FieldType::JsonValue if rules.family.decodes(Leaf::JsonValue) => Want::JsonValue,
            FieldType::JsonValue => Want::Fails(Error::Unsupported("json.Value".into())),
            FieldType::Bytes => Want::Fails(Error::Unsupported("Bytes".into())),
            FieldType::Record(id) => {
                let ty = match field_record_type(rt, rules.caller, *id) {
                    Ok(ty) => ty,
                    Err(e) => return Want::Fails(e.into()),
                };
                match decodable_record(rules.caller, &ty) {
                    Ok(_) => Want::Record(ty, &[]),
                    Err(e) => Want::Fails(e.into()),
                }
            }
            FieldType::Enum(id) if rules.family.decodes_enums() => {
                match field_record_type(rt, rules.caller, *id) {
                    Ok(ty) => Want::Variant(ty, &[]),
                    Err(e) => Want::Fails(e.into()),
                }
            }
            FieldType::Enum(_) => unsupported(),
            // A generic type's fields, each type parameter its argument.
            FieldType::Applied(id, args) => {
                let ty = match field_record_type(rt, rules.caller, *id) {
                    Ok(ty) => ty,
                    Err(e) => return Want::Fails(e.into()),
                };
                match &ty.shape {
                    Shape::Enum(_) if rules.family.decodes_enums() && decodable_enum(&ty) => {
                        Want::Variant(ty, args)
                    }
                    Shape::Enum(_) => unsupported(),
                    _ => match decodable_record(rules.caller, &ty) {
                        Ok(_) => Want::Record(ty, args),
                        Err(e) => Want::Fails(e.into()),
                    },
                }
            }
            // (A type parameter no type argument gave: refused before.)
            FieldType::Param(_) | FieldType::KeyedMap(..) => unsupported(),
        }
    }

    /// The value of the part that is `scalar`.
    fn scalar<E: de::Error>(self, scalar: Scalar) -> Result<Outcome, E> {
        if let Some(slot) = self.written() {
            return Ok(match self.met() {
                Met::Find(number) => self.found(number, slot, self.json().scalar(scalar)),
                Met::Given(outcome) => outcome,
            });
        }
        // (The most of a document: no type to look up.)
        if let FieldType::String
        | FieldType::Int
        | FieldType::Float
        | FieldType::Bool
        | FieldType::Date
        | FieldType::Time
        | FieldType::DateTime = self.expected
        {
            return Ok(self.primitive(scalar));
        }
        let rules = self.cx.rules;
        let at = self.at;
        let kind = self.cx.format.kind(scalar);
        let mismatch = |expected: &'static str| Error::mismatch(expected, kind, at);
        Ok(match self.want() {
            Want::Option(_) if matches!(scalar, Scalar::Null) => Ok(Value::variant(bv::NONE, [])),
            Want::Option(inner) => self
                .of(inner, at)
                .scalar(scalar)?
                .map(|value| self.some(value)),
            Want::Fails(e) => Err(e),
            Want::JsonValue if !self.build() => Ok(Value::Unit),
            Want::JsonValue => self.json().scalar(scalar),
            Want::List(_) => Err(mismatch("List")),
            Want::Map(_) => Err(mismatch("Map")),
            Want::Tuple(_) => Err(mismatch("Tuple")),
            Want::Record(..) => Err(mismatch(rules.table)),
            Want::Variant(ty, _) => match scalar {
                Scalar::String(name) => bare_variant(&ty, name, at),
                _ => Err(mismatch(rules.variant)),
            },
            Want::Scalar => self.primitive(scalar),
        })
    }

    /// The value of the primitive type, date or time `expected` that the
    /// part `scalar` is.
    fn primitive(self, scalar: Scalar) -> Outcome {
        let at = self.at;
        let kind = self.cx.format.kind(scalar);
        let mismatch = |expected: &'static str| Error::mismatch(expected, kind, at);
        match self.expected {
            // (A datetime literal is no string, as a float is no `Int`.)
            FieldType::String => match scalar {
                Scalar::String(_) if !self.build() => Ok(Value::Unit),
                Scalar::String(s) => Ok(Value::String(s.into())),
                _ => Err(mismatch("String")),
            },
            // A number is an `Int` when it is held as one; else, where the
            // format tells numbers apart by their value, one with a
            // fraction or out of range is no `Int`, and nothing is cut
            // off or saturated.
            FieldType::Int => match scalar {
                Scalar::Int(n) => Ok(Value::Int(n)),
                Scalar::Float(f) if self.cx.rules.numbers == Numbers::Values => {
                    let what = match f.abs() < I64_MAX_PLUS_ONE {
                        true => "a number with a fraction",
                        false => "a number out of Int's range",
                    };
                    Err(Error::mismatch("Int", what, at))
                }
                _ => Err(mismatch("Int")),
            },
            // (An integer is a `Float` too. A `Float` is finite, so TOML's
            // `nan` and `inf` do not decode.)
            FieldType::Float => match scalar {
                Scalar::Int(n) => Ok(Value::Float(Float::from(n))),
                Scalar::Float(f) => match Float::new(f) {
                    Some(f) => Ok(Value::Float(f)),
                    None => Err(Error::mismatch("Float", "a non-finite float", at)),
                },
                _ => Err(mismatch("Float")),
            },
            FieldType::Bool => match scalar {
                Scalar::Bool(b) => Ok(Value::Bool(b)),
                _ => Err(mismatch("Bool")),
            },
            // A date and a time are a string of their text, or a literal of
            // the format; a text that is none has the one message either
            // way.
            FieldType::Date => match scalar {
                Scalar::String(s) | Scalar::Datetime(s) => parse_date(s)
                    .map(make_date)
                    .map_err(|e| Error::other(invalid_date_message(s, e), at)),
                _ => Err(mismatch("time.Date")),
            },
            FieldType::Time => match scalar {
                Scalar::String(s) | Scalar::Datetime(s) => parse_time(s)
                    .map(make_time)
                    .map_err(|e| Error::other(invalid_time_message(s, &e), at)),
                _ => Err(mismatch("time.Time")),
            },
            FieldType::DateTime => match scalar {
                Scalar::String(s) | Scalar::Datetime(s) => parse_datetime(s)
                    .map(make_datetime)
                    .map_err(|e| Error::other(e, at)),
                _ => Err(mismatch("time.DateTime")),
            },
            _ => unreachable!("only a primitive type, a date or a time is decoded so"),
        }
    }

    /// The value of the part that is an array, read by `seq`.
    fn array<'de, A: SeqAccess<'de>>(self, mut seq: A) -> Result<Outcome, A::Error> {
        if let Some(slot) = self.written() {
            return Ok(match self.met() {
                Met::Find(number) => self.found(number, slot, self.json().array(seq)?),
                Met::Given(outcome) => {
                    while seq.next_element_seed(self.skip())?.is_some() {}
                    outcome
                }
            });
        }
        let rules = self.cx.rules;
        let at = self.at;
        let mismatch = |expected: &'static str| Error::mismatch(expected, "array", at);
        let failed = match self.want() {
            Want::Option(inner) => {
                let some = self.of(inner, at).array(seq)?;
                return Ok(some.map(|value| self.some(value)));
            }
            // The elements in order, up to the first that is no value of
            // the type.
            Want::List(inner) => {
                let mut values = Vec::with_capacity(room(&seq));
                let mut index = 0;
                let build = self.build();
                while let Some(decoded) = seq.next_element_seed(self.of(inner, &at.index(index)))? {
                    match decoded {
                        Ok(value) if build => values.push(value),
                        Ok(_) => {}
                        Err(e) => {
                            while seq.next_element_seed(self.skip())?.is_some() {}
                            return Ok(Err(e));
                        }
                    }
                    index += 1;
                }
                return Ok(Ok(match build {
                    true => Value::list(values),
                    false => Value::Unit,
                }));
            }
            // An array of as many elements as the tuple has, which it
            // takes before any element is decoded.
            Want::Tuple(elems) => {
                let start = self.cx.parts.mark();
                let mut values = Vec::with_capacity(elems.len());
                let mut error = None;
                let mut count = 0;
                loop {
                    let read = match elems.get(count) {
                        Some(elem) if error.is_none() => {
                            seq.next_element_seed(self.of(elem, &at.index(count)))?
                        }
                        _ => seq
                            .next_element_seed(self.skip())?
                            .map(|()| Ok(Value::Unit)),
                    };
                    match read {
                        Some(Ok(value)) if error.is_none() => values.push(value),
                        Some(Ok(_)) => {}
                        Some(Err(e)) => error = Some(e),
                        None => break,
                    }
                    count += 1;
                }
                // (A tuple of the wrong length decodes none of its
                // elements.)
                if count != elems.len() {
                    self.cx.parts.reorder(start, []);
                }
                return Ok(match (count == elems.len(), error) {
                    (false, _) => Err(Error::other(
                        format_args!(
                            "expected an array of {} elements for a tuple, got {count}",
                            elems.len()
                        ),
                        at,
                    )),
                    (true, Some(e)) => Err(e),
                    (true, None) if !self.build() => Ok(Value::Unit),
                    (true, None) => Ok(Value::tuple(values)),
                });
            }
            Want::JsonValue if !self.build() => {
                while seq.next_element_seed(self.skip())?.is_some() {}
                return Ok(Ok(Value::Unit));
            }
            Want::JsonValue => return self.json().array(seq),
            Want::Fails(e) => e,
            Want::Map(_) => mismatch("Map"),
            Want::Record(..) => mismatch(rules.table),
            Want::Variant(..) => mismatch(rules.variant),
            Want::Scalar => mismatch(scalar_name(self.expected)),
        };
        while seq.next_element_seed(self.skip())?.is_some() {}
        Ok(Err(failed))
    }

    /// The value of the part that is a table, read by `map`.
    fn object<'de, A: MapAccess<'de>>(self, mut map: A) -> Result<Outcome, A::Error> {
        match open(self.cx, &mut map)? {
            Opened::Datetime(text) => self.scalar(Scalar::Datetime(&text)),
            Opened::Table(first) => self.table(Entries { first, map }),
        }
    }

    /// The value of the part that is a table of the entries `entries`,
    /// of a type that a table is a value of.
    fn table<'de, A: MapAccess<'de>>(self, entries: Entries<'de, A>) -> Result<Outcome, A::Error> {
        if let Some(slot) = self.written() {
            return Ok(match self.met() {
                Met::Find(number) => self.found(number, slot, self.json().table(entries)?),
                Met::Given(outcome) => {
                    entries.skip(self.skip())?;
                    outcome
                }
            });
        }
        let at = self.at;
        let mismatch =
            |expected: &'static str| Error::mismatch(expected, self.cx.format.table(), at);
        let failed = match self.want() {
            Want::Option(inner) => {
                let some = self.of(inner, at).table(entries)?;
                return Ok(some.map(|value| self.some(value)));
            }
            Want::Map(inner) => return self.map(entries, inner),
            Want::Record(ty, args) => return self.record(entries, &ty, args),
            Want::Variant(ty, args) => return self.variant(entries, &ty, args),
            Want::JsonValue if !self.build() => {
                entries.skip(self.skip())?;
                return Ok(Ok(Value::Unit));
            }
            Want::JsonValue => return self.json().table(entries),
            Want::Fails(e) => e,
            Want::List(_) => mismatch("List"),
            Want::Tuple(_) => mismatch("Tuple"),
            Want::Scalar => mismatch(scalar_name(self.expected)),
        };
        entries.skip(self.skip())?;
        Ok(Err(failed))
    }

    /// The map of values of `inner` that the table of `entries` is: of
    /// the keys that are no value of the type, the first in sorted order
    /// is the error.
    fn map<'de, A: MapAccess<'de>>(
        self,
        mut entries: Entries<'de, A>,
        inner: &FieldType,
    ) -> Result<Outcome, A::Error> {
        let parts = self.cx.parts;
        let start = parts.mark();
        let mut read: Vec<(Arc<str>, Outcome, Range<usize>)> = Vec::new();
        while let Some(key) = entries.key()? {
            let from = parts.mark();
            let decoded = entries.value(self.of(inner, &self.at.key(&key)))?;
            read.push((key.into(), decoded, from..parts.mark()));
        }
        let all_ok = read.iter().all(|(_, decoded, _)| decoded.is_ok());
        if !self.build() && all_ok && parts.mark() == start {
            return Ok(Ok(Value::Unit));
        }
        if parts.mark() == start && all_ok {
            let read = read.into_iter().filter_map(|(key, decoded, _)| {
                decoded.ok().map(|value| (Value::String(key), value))
            });
            return Ok(Ok(Value::map(read)));
        }
        // (Of a key written twice the last value counts, its error and
        // its parts too.)
        let mut sorted = BTreeMap::new();
        for (key, decoded, found) in read {
            sorted.insert(key, (decoded, found));
        }
        let mut values = Vec::with_capacity(sorted.len());
        let mut ranges = Vec::with_capacity(sorted.len());
        let mut error = None;
        for (key, (decoded, found)) in sorted {
            ranges.push(found);
            match decoded {
                Ok(value) => values.push((Value::String(key), value)),
                Err(e) => {
                    error = Some(e);
                    break;
                }
            }
        }
        parts.reorder(start, ranges);
        Ok(match error {
            Some(e) => Err(e),
            None if !self.build() => Ok(Value::Unit),
            None => Ok(Value::map(values)),
        })
    }

    /// The record of type `ty` at its type arguments `args` that the
    /// table of `entries` is: of the fields that are no value of their
    /// type or missing, the first the type declares is the error.
    fn record<'de, A: MapAccess<'de>>(
        self,
        mut entries: Entries<'de, A>,
        ty: &Arc<TypeInfo>,
        args: &[FieldType],
    ) -> Result<Outcome, A::Error> {
        let fields = ty.fields();
        let parts = self.cx.parts;
        let start = parts.mark();
        let mut read: Vec<Option<(Outcome, Range<usize>)>> =
            (0..fields.len()).map(|_| None).collect();
        while let Some(key) = entries.key()? {
            match fields.iter().position(|(name, _)| *name == *key) {
                Some(k) => {
                    let (name, field) = &fields[k];
                    let field = field.instantiated(args);
                    let at = self.at.key(name);
                    let from = parts.mark();
                    let decoded = entries.value(self.of(&field, &at))?;
                    read[k] = Some((decoded, from..parts.mark()));
                }
                None => entries.value(self.skip())?,
            }
        }
        // (The parts found in the fields, in the order the type declares
        // them, where there are any.)
        let found_any = parts.mark() != start;
        let mut values = Vec::with_capacity(fields.len());
        let mut ranges = Vec::new();
        let mut error = None;
        for ((name, field), read) in fields.iter().zip(read) {
            let decoded = match read {
                Some((decoded, found)) => {
                    if found_any {
                        ranges.push(found);
                    }
                    decoded
                }
                // (TOML has no null: a key it leaves out is a `None`.)
                None => match &*field.instantiated(args) {
                    FieldType::Option(_) => Ok(Value::variant(bv::NONE, [])),
                    _ => Err(Error::MissingField(self.at.key(name).to_string())),
                },
            };
            match decoded {
                Ok(value) if self.build() => values.push(value),
                Ok(_) => {}
                Err(Error::Unsupported(declared)) => {
                    let message = unsupported_field_type_message(&ty.name, name, &declared);
                    error = Some(Error::Other(message));
                    break;
                }
                Err(e) => {
                    error = Some(e);
                    break;
                }
            }
        }
        if found_any {
            parts.reorder(start, ranges);
        }
        Ok(match error {
            Some(e) => Err(e),
            None if !self.build() => Ok(Value::Unit),
            None => Ok(Value::record(ty.clone(), values)),
        })
    }

    /// The variant of the enum type `ty` at its type arguments `args`
    /// that the table of `entries` is, as `json.stringify` writes one
    /// with fields: `{"fields":[...],"variant":"Name"}`, other keys
    /// ignored. The fields are decoded as they are read where the name
    /// came before them; where they come first they are kept as read
    /// (once: a variant in them is read from what is kept, which it
    /// shares) and decoded once the name is read. A JSON text that gives
    /// either key twice is read again with every variant's fields kept
    /// ([`Parts::keep_fields`]), the last of each counting.
    fn variant<'de, A: MapAccess<'de>>(
        self,
        mut entries: Entries<'de, A>,
        ty: &Arc<TypeInfo>,
        args: &[FieldType],
    ) -> Result<Outcome, A::Error> {
        let cx = self.cx;
        let at = self.at;
        let keep = cx.parts.keep_fields.get();
        let mut name = None;
        let mut fields = None;
        while let Some(key) = entries.key()? {
            match &*key {
                "variant" if !keep && name.is_some() => return Err(cx.for_a_restart()),
                "fields" if !keep && fields.is_some() => return Err(cx.for_a_restart()),
                "variant" => name = Some(entries.value(Capture::kept(*cx))?),
                "fields" => {
                    fields = Some(match (keep, named(cx, ty, name.as_ref(), at)) {
                        (false, Ok(tag)) => Fields::Read(entries.value(self.fields(&tag, args))?),
                        // (The name's error is the variant's.)
                        (false, Err(_)) if name.is_some() => {
                            entries.value(self.skip())?;
                            Fields::Passed
                        }
                        _ => Fields::Kept(entries.value(Capture::kept(*cx))?),
                    })
                }
                _ => entries.value(self.skip())?,
            }
        }
        let tag = match named(cx, ty, name.as_ref(), at) {
            Ok(tag) => tag,
            Err(e) => return Ok(Err(e)),
        };
        Ok(match fields {
            Some(Fields::Read(decoded)) => decoded,
            Some(Fields::Kept(kept)) => {
                // The fields, read again from what was kept.
                let cx = Cx {
                    digits: Digits::Read,
                    ..*cx
                };
                let seed = Seed { cx: &cx, ..self };
                let kept = ContentDe {
                    content: &kept,
                    kept: cx.kept,
                };
                (seed.fields(&tag, args).deserialize(kept))
                    .map_err(<A::Error as de::Error>::custom)?
            }
            Some(Fields::Passed) => unreachable!("a name that is no variant's is the error"),
            None => self.fields(&tag, args).count(0, Vec::new(), None),
        })
    }

    /// The decoding of the `fields` of the variant `tag` at the type
    /// arguments `args`.
    fn fields<'f>(&'f self, tag: &'f Tag, args: &'f [FieldType]) -> FieldsSeed<'f> {
        FieldsSeed {
            seed: *self,
            tag,
            args,
        }
    }
}

/// What a variant's `fields` are while its table is read.
enum Fields {
    /// Decoded as they were read, the name before them.
    Read(Outcome),
    /// Kept as read, the name after them.
    Kept(Content),
    /// Passed by: the name before them is no variant's.
    Passed,
}

/// The variant of the enum type `ty` that the kept `variant` of its
/// table names (`at` a path), one with fields; or the error of the
/// name, in this order: missing, no string, no variant's, or one
/// of no fields.
fn named(cx: &Cx, ty: &Arc<TypeInfo>, name: Option<&Content>, at: &At) -> Result<Tag, Error> {
    let at_name = at.key("variant");
    let name = match name {
        Some(name) => match name.scalar(cx) {
            Some(Scalar::String(name)) => name,
            _ => return Err(Error::mismatch("String", name.kind(cx), &at_name)),
        },
        None => return Err(Error::MissingField(at_name.to_string())),
    };
    let tag = Tag::named(ty, name).ok_or_else(|| no_variant(ty, name, &at_name))?;
    // (A variant without fields is its name, never a table.)
    match ty.variants()[tag.ordinal() as usize].fields.is_empty() {
        true => Err(Error::mismatch("String", cx.format.table(), at)),
        false => Ok(tag),
    }
}

/// The decoding of the `fields` of the variant `tag` of a table `at` a
/// path of its seed, at the type arguments `args`: an array of as many
/// elements as the variant has fields, which it takes before any
/// element is decoded, each decoded as the variant declares it.
struct FieldsSeed<'f> {
    seed: Seed<'f>,
    tag: &'f Tag,
    args: &'f [FieldType],
}

impl FieldsSeed<'_> {
    /// What `count` elements, of which `values` were decoded up to the
    /// first `error`, give.
    fn count(&self, count: usize, values: Vec<Value>, error: Option<Error>) -> Outcome {
        let ty = self.tag.ty();
        let variant = &ty.variants()[self.tag.ordinal() as usize];
        let declared = variant.fields.len();
        let at_fields = self.seed.at.key("fields");
        if count != declared {
            let message = format_args!(
                "variant `{}` of `{}` has {}, got {count}",
                variant.name,
                ty.name,
                self::count(declared),
            );
            return Err(Error::other(message, &at_fields));
        }
        match error {
            // (A field no decoder fills was refused before the document
            // was read; this is the fallback.)
            Some(Error::Unsupported(declared)) => Err(Error::Other(variant_field_message(
                &ty.name,
                &variant.name,
                &declared,
            ))),
            Some(e) => Err(e),
            None if !self.seed.build() => Ok(Value::Unit),
            None => Ok(Value::variant(self.tag.clone(), values)),
        }
    }

    fn not(&self, kind: &'static str) -> Outcome {
        Err(Error::mismatch("array", kind, &self.seed.at.key("fields")))
    }

    fn scalar<E>(&self, scalar: Scalar) -> Result<Outcome, E> {
        Ok(self.not(self.seed.cx.format.kind(scalar)))
    }
}

impl<'de> DeserializeSeed<'de> for FieldsSeed<'_> {
    type Value = Outcome;

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Outcome, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for FieldsSeed<'_> {
    type Value = Outcome;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a variant's fields")
    }

    fn visit_bool<E: de::Error>(self, b: bool) -> Result<Outcome, E> {
        self.scalar(Scalar::Bool(b))
    }

    fn visit_i64<E: de::Error>(self, n: i64) -> Result<Outcome, E> {
        self.seed.cx.digits.integer();
        self.scalar(Scalar::Int(n))
    }

    fn visit_u64<E: de::Error>(self, n: u64) -> Result<Outcome, E> {
        self.seed.cx.digits.integer();
        self.scalar(Scalar::unsigned(n))
    }

    fn visit_f64<E: de::Error>(self, f: f64) -> Result<Outcome, E> {
        self.scalar(self.seed.cx.digits.float(f))
    }

    fn visit_str<E: de::Error>(self, s: &str) -> Result<Outcome, E> {
        self.scalar(self.seed.cx.string(s))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Outcome, E> {
        self.scalar(Scalar::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Outcome, A::Error> {
        let declared = &self.tag.ty().variants()[self.tag.ordinal() as usize].fields;
        let at_fields = self.seed.at.key("fields");
        let start = self.seed.cx.parts.mark();
        let mut values = Vec::with_capacity(declared.len());
        let mut error = None;
        let mut count = 0;
        loop {
            let read = match declared.get(count) {
                Some(field) if error.is_none() => {
                    let field = field.instantiated(self.args);
                    seq.next_element_seed(self.seed.of(&field, &at_fields.index(count)))?
                }
                _ => seq
                    .next_element_seed(self.seed.skip())?
                    .map(|()| Ok(Value::Unit)),
            };
            match read {
                Some(Ok(value)) if error.is_none() => values.push(value),
                Some(Ok(_)) => {}
                Some(Err(e)) => error = Some(e),
                None => break,
            }
            count += 1;
        }
        // (Fields of the wrong count decode none of them.)
        if count != declared.len() {
            self.seed.cx.parts.reorder(start, []);
        }
        Ok(self.count(count, values, error))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Outcome, A::Error> {
        let cx = self.seed.cx;
        let first = match open(cx, &mut map)? {
            Opened::Datetime(text) => return self.scalar(Scalar::Datetime(&text)),
            Opened::Table(first) => first,
        };
        Entries { first, map }.skip(self.seed.skip())?;
        Ok(self.not(cx.format.table()))
    }
}

/// The room to make for the elements of the array `seq` reads: as many
/// as it says it has. (Only a document already read says: the parser of
/// a JSON text does not, and TOML's and a parsed document's say how
/// many they hold.)
fn room<'de, A: SeqAccess<'de>>(seq: &A) -> usize {
    seq.size_hint().unwrap_or(0)
}

/// The variant of the enum type `ty` whose name is the string `name`,
/// `at` a path: one without fields.
fn bare_variant(ty: &Arc<TypeInfo>, name: &str, at: &At) -> Outcome {
    let tag = Tag::named(ty, name).ok_or_else(|| no_variant(ty, name, at))?;
    let fields = &ty.variants()[tag.ordinal() as usize].fields;
    if !fields.is_empty() {
        let message = format_args!(
            "variant `{name}` of `{}` has {}, got 0",
            ty.name,
            count(fields.len())
        );
        return Err(Error::other(message, at));
    }
    Ok(Value::variant(tag, []))
}

/// The error of a variant name `name`, `at` a path, that the enum type
/// `ty` has no variant of.
fn no_variant(ty: &TypeInfo, name: &str, at: &At) -> Error {
    Error::other(format_args!("`{}` has no variant `{name}`", ty.name), at)
}

/// `n` fields, in words.
fn count(n: usize) -> String {
    match n {
        1 => "1 field".to_string(),
        n => format!("{n} fields"),
    }
}

/// What a type mismatch says a part of the primitive type, date or time
/// `expected` should be.
fn scalar_name(expected: &FieldType) -> &'static str {
    match expected {
        FieldType::String => "String",
        FieldType::Int => "Int",
        FieldType::Float => "Float",
        FieldType::Bool => "Bool",
        FieldType::Date => "time.Date",
        FieldType::Time => "time.Time",
        FieldType::DateTime => "time.DateTime",
        _ => unreachable!("only a primitive type, a date or a time is decoded so"),
    }
}

impl<'de> DeserializeSeed<'de> for Seed<'_> {
    type Value = Outcome;

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Outcome, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Seed<'_> {
    type Value = Outcome;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a document")
    }

    fn visit_bool<E: de::Error>(self, b: bool) -> Result<Outcome, E> {
        self.scalar(Scalar::Bool(b))
    }

    fn visit_i64<E: de::Error>(self, n: i64) -> Result<Outcome, E> {
        self.cx.digits.integer();
        self.scalar(Scalar::Int(n))
    }

    fn visit_u64<E: de::Error>(self, n: u64) -> Result<Outcome, E> {
        self.cx.digits.integer();
        self.scalar(Scalar::unsigned(n))
    }

    fn visit_f64<E: de::Error>(self, f: f64) -> Result<Outcome, E> {
        self.scalar(self.cx.digits.float(f))
    }

    fn visit_str<E: de::Error>(self, s: &str) -> Result<Outcome, E> {
        self.scalar(self.cx.string(s))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Outcome, E> {
        self.scalar(Scalar::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Outcome, A::Error> {
        self.array(seq)
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Outcome, A::Error> {
        self.object(map)
    }
}

/// The TOML document read as the list under its one key, each element
/// `<key>[i]` (`toml.parse_list`).
struct ListUnderItsKey<'s> {
    cx: &'s Cx<'s>,
    /// The list type.
    list: &'s FieldType,
    /// What the error of a document of more keys or none says.
    keys: &'s dyn Fn(usize) -> String,
}

impl<'de> DeserializeSeed<'de> for ListUnderItsKey<'_> {
    type Value = Outcome;

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Outcome, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for ListUnderItsKey<'_> {
    type Value = Outcome;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a TOML document")
    }

    // (TOML's top level is a table.)
    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Outcome, A::Error> {
        let mut map = map;
        let first = match open(self.cx, &mut map)? {
            Opened::Datetime(_) => return Ok(Err(Error::mismatch("table", "datetime", &At::Top))),
            Opened::Table(first) => first,
        };
        let mut entries = Entries { first, map };
        let mut decoded = None;
        let mut keys = 0;
        while let Some(key) = entries.key()? {
            match keys {
                0 => {
                    let at = At::Top.key(&key);
                    let seed = Seed {
                        cx: self.cx,
                        expected: self.list,
                        at: &at,
                    };
                    decoded = Some(entries.value(Only { seed, table: false })?);
                }
                _ => entries.value(Skip(*self.cx))?,
            }
            keys += 1;
        }
        Ok(match (keys, decoded) {
            (1, Some(decoded)) => decoded,
            // (A document of more keys or none decodes none of them.)
            _ => {
                self.cx.parts.reorder(0, []);
                Err(Error::Other((self.keys)(keys)))
            }
        })
    }
}

/// The decoding of a part that is an array, or a `table`, into the
/// list or map type of its seed: anything else is the type mismatch of
/// no such part, an `array` or the format's table.
struct Only<'s> {
    seed: Seed<'s>,
    table: bool,
}

impl Only<'_> {
    fn not<E>(&self, kind: &'static str) -> Result<Outcome, E> {
        let expected = match self.table {
            true => self.seed.cx.rules.table,
            false => "array",
        };
        Ok(Err(Error::mismatch(expected, kind, self.seed.at)))
    }

    fn scalar<E>(&self, scalar: Scalar) -> Result<Outcome, E> {
        self.not(self.seed.cx.format.kind(scalar))
    }
}

impl<'de> DeserializeSeed<'de> for Only<'_> {
    type Value = Outcome;

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Outcome, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Only<'_> {
    type Value = Outcome;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("an array or a table")
    }

    fn visit_bool<E: de::Error>(self, b: bool) -> Result<Outcome, E> {
        self.scalar(Scalar::Bool(b))
    }

    fn visit_i64<E: de::Error>(self, n: i64) -> Result<Outcome, E> {
        self.scalar(Scalar::Int(n))
    }

    fn visit_f64<E: de::Error>(self, f: f64) -> Result<Outcome, E> {
        self.scalar(Scalar::Float(f))
    }

    fn visit_str<E: de::Error>(self, s: &str) -> Result<Outcome, E> {
        self.scalar(self.seed.cx.string(s))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Outcome, A::Error> {
        if !self.table {
            return self.seed.array(seq);
        }
        while seq.next_element_seed(Skip(*self.seed.cx))?.is_some() {}
        self.not("array")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Outcome, A::Error> {
        let cx = self.seed.cx;
        let first = match open(cx, &mut map)? {
            Opened::Datetime(_) => return self.not("datetime"),
            Opened::Table(first) => first,
        };
        let entries = Entries { first, map };
        if self.table {
            return self.seed.table(entries);
        }
        entries.skip(Skip(*cx))?;
        self.not(cx.format.table())
    }
}

/// The key of a member of a table.
struct Key;

impl<'de> DeserializeSeed<'de> for Key {
    type Value = Cow<'de, str>;

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_str(self)
    }
}

impl<'de> Visitor<'de> for Key {
    type Value = Cow<'de, str>;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a key")
    }

    fn visit_borrowed_str<E: de::Error>(self, s: &'de str) -> Result<Self::Value, E> {
        Ok(Cow::Borrowed(s))
    }

    fn visit_str<E: de::Error>(self, s: &str) -> Result<Self::Value, E> {
        Ok(Cow::Owned(s.to_owned()))
    }

    fn visit_string<E: de::Error>(self, s: String) -> Result<Self::Value, E> {
        Ok(Cow::Owned(s))
    }
}

/// The reading of a part that is decoded into nothing, which may still
/// be broken text.
#[derive(Clone, Copy)]
struct Skip<'t>(Cx<'t>);

impl<'de> DeserializeSeed<'de> for Skip<'_> {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Skip<'_> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a document")
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<(), E> {
        self.0.digits.integer();
        Ok(())
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<(), E> {
        self.0.digits.integer();
        Ok(())
    }

    fn visit_f64<E: de::Error>(self, f: f64) -> Result<(), E> {
        self.0.digits.float(f);
        Ok(())
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<(), E> {
        Ok(())
    }

    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while seq.next_element_seed(self)?.is_some() {}
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        match open(&self.0, &mut map)? {
            Opened::Datetime(_) => Ok(()),
            Opened::Table(first) => Entries { first, map }.skip(self),
        }
    }
}

/// A part of a document kept as it is read, to be decoded later (a
/// variant's `variant`, and its `fields` where they come first). An
/// array or a table is shared: a part of it is kept again without a
/// copy ([`ContentDe`]).
#[derive(Clone)]
enum Content {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    /// A TOML date or time literal, as its text.
    Datetime(String),
    Array(Rc<Vec<Content>>),
    /// A table's entries in the order read, a key written twice too.
    Table(Rc<Vec<(String, Content)>>),
}

impl Content {
    /// It as a value that holds no other.
    fn scalar<'c>(&'c self, cx: &Cx) -> Option<Scalar<'c>> {
        Some(match self {
            Content::Null => Scalar::Null,
            Content::Bool(b) => Scalar::Bool(*b),
            Content::Int(n) => Scalar::Int(*n),
            Content::Float(f) => Scalar::Float(*f),
            Content::String(s) => cx.string(s),
            Content::Datetime(s) => Scalar::Datetime(s),
            Content::Array(_) | Content::Table(_) => return None,
        })
    }

    /// What the format calls its kind in a type mismatch.
    fn kind(&self, cx: &Cx) -> &'static str {
        match self {
            Content::Array(_) => "array",
            Content::Table(_) => cx.format.table(),
            scalar => cx.format.kind(scalar.scalar(cx).expect("a scalar")),
        }
    }

    fn of(scalar: Scalar) -> Content {
        match scalar {
            Scalar::Null => Content::Null,
            Scalar::Bool(b) => Content::Bool(b),
            Scalar::Int(n) => Content::Int(n),
            Scalar::Float(f) => Content::Float(f),
            Scalar::String(s) => Content::String(s.to_owned()),
            Scalar::Datetime(s) => Content::Datetime(s.to_owned()),
        }
    }
}

/// The name under which [`Capture`] asks for a part: of what was kept,
/// the part itself is handed over ([`Cx::kept`]), of a parser's text the
/// part is read.
const KEPT: &str = "$__silt_kept";

/// The reading of a part into the [`Content`] that keeps it.
#[derive(Clone, Copy)]
struct Capture<'t> {
    cx: Cx<'t>,
    /// Whether it is the part kept, not a part of it: only the part kept
    /// is asked for under [`KEPT`] (what was kept is handed over whole).
    whole: bool,
}

impl<'t> Capture<'t> {
    /// The reading of the part `cx` reads, kept.
    fn kept(cx: Cx<'t>) -> Capture<'t> {
        Capture { cx, whole: true }
    }

    /// The reading of a part of what is kept.
    fn inner(self) -> Capture<'t> {
        Capture {
            whole: false,
            ..self
        }
    }
}

impl<'de> DeserializeSeed<'de> for Capture<'_> {
    type Value = Content;

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Content, D::Error> {
        #[cfg(test)]
        tests::CAPTURED.with(|captured| captured.set(captured.get() + 1));
        match self.whole {
            true => d.deserialize_newtype_struct(KEPT, self),
            false => d.deserialize_any(self),
        }
    }
}

impl<'de> Visitor<'de> for Capture<'_> {
    type Value = Content;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a document")
    }

    fn visit_newtype_struct<D: de::Deserializer<'de>>(self, d: D) -> Result<Content, D::Error> {
        d.deserialize_any(self)
    }

    fn visit_bool<E: de::Error>(self, b: bool) -> Result<Content, E> {
        Ok(Content::Bool(b))
    }

    fn visit_i64<E: de::Error>(self, n: i64) -> Result<Content, E> {
        self.cx.digits.integer();
        Ok(Content::Int(n))
    }

    fn visit_u64<E: de::Error>(self, n: u64) -> Result<Content, E> {
        self.cx.digits.integer();
        Ok(Content::of(Scalar::unsigned(n)))
    }

    fn visit_f64<E: de::Error>(self, f: f64) -> Result<Content, E> {
        Ok(Content::of(self.cx.digits.float(f)))
    }

    // (A marked string is kept as it is: its reading decides.)
    fn visit_str<E: de::Error>(self, s: &str) -> Result<Content, E> {
        Ok(Content::String(s.to_owned()))
    }

    fn visit_string<E: de::Error>(self, s: String) -> Result<Content, E> {
        Ok(Content::String(s))
    }

    // (Or what was kept, handed over.)
    fn visit_unit<E: de::Error>(self) -> Result<Content, E> {
        Ok(self.cx.kept.take().unwrap_or(Content::Null))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Content, A::Error> {
        let mut items = Vec::with_capacity(room(&seq));
        while let Some(item) = seq.next_element_seed(self.inner())? {
            items.push(item);
        }
        Ok(Content::Array(Rc::new(items)))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Content, A::Error> {
        let first = match open(&self.cx, &mut map)? {
            Opened::Datetime(text) => return Ok(Content::Datetime(text)),
            Opened::Table(first) => first,
        };
        let mut entries = Entries { first, map };
        let mut members = Vec::new();
        while let Some(key) = entries.key()? {
            members.push((key.into_owned(), entries.value(self.inner())?));
        }
        Ok(Content::Table(Rc::new(members)))
    }
}

/// What was kept, read as its parser gave it: a TOML date or time
/// literal as the table of its mark. A part [`Capture`] asks for is
/// handed over in `kept`, shared, not read.
#[derive(Clone, Copy)]
struct ContentDe<'a> {
    content: &'a Content,
    kept: &'a Cell<Option<Content>>,
}

impl<'de> de::Deserializer<'de> for ContentDe<'de> {
    type Error = de::value::Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let kept = self.kept;
        match self.content {
            Content::Null => visitor.visit_unit(),
            Content::Bool(b) => visitor.visit_bool(*b),
            Content::Int(n) => visitor.visit_i64(*n),
            Content::Float(f) => visitor.visit_f64(*f),
            Content::String(s) => visitor.visit_borrowed_str(s),
            Content::Datetime(s) => {
                let mut map = MapDeserializer::new(std::iter::once((DATETIME, s.as_str())));
                let read = visitor.visit_map(&mut map)?;
                map.end()?;
                Ok(read)
            }
            Content::Array(items) => {
                let items = items.iter().map(|content| ContentDe { content, kept });
                let mut seq = SeqDeserializer::new(items);
                let read = visitor.visit_seq(&mut seq)?;
                seq.end()?;
                Ok(read)
            }
            Content::Table(members) => {
                let entries = (members.iter())
                    .map(|(key, content)| (key.as_str(), ContentDe { content, kept }));
                let mut map = MapDeserializer::new(entries);
                let read = visitor.visit_map(&mut map)?;
                map.end()?;
                Ok(read)
            }
        }
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        match name {
            KEPT => {
                self.kept.set(Some(self.content.clone()));
                visitor.visit_unit()
            }
            _ => visitor.visit_newtype_struct(self),
        }
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct seq tuple
        tuple_struct map struct enum identifier ignored_any
    }
}

impl<'de> IntoDeserializer<'de, de::value::Error> for ContentDe<'de> {
    type Deserializer = Self;

    fn into_deserializer(self) -> Self {
        self
    }
}

/// The reading of a part, `at` a path, into the `json.Value` that holds
/// it: a written impl's part, or a part of type `json.Value`. A TOML
/// float that is no number has none: a type mismatch at its own path
/// (the first in the decoder's order: a table's keys sorted).
#[derive(Clone, Copy)]
struct JsonSeed<'s> {
    cx: &'s Cx<'s>,
    at: &'s At<'s>,
}

impl<'s> Seed<'s> {
    /// The reading of the part into its `json.Value`.
    fn json(&self) -> JsonSeed<'s> {
        JsonSeed {
            cx: self.cx,
            at: self.at,
        }
    }
}

impl JsonSeed<'_> {
    /// The `json.Value` of `scalar`.
    fn scalar(&self, scalar: Scalar) -> Outcome {
        match scalar {
            Scalar::Float(f) if !f.is_finite() => {
                Err(Error::mismatch("json.Value", "a non-finite float", self.at))
            }
            scalar => Ok(scalar.json()),
        }
    }

    /// The `json.Array` of the array `seq` reads.
    fn array<'de, A: SeqAccess<'de>>(self, mut seq: A) -> Result<Outcome, A::Error> {
        let mut items = Vec::with_capacity(room(&seq));
        let mut error = None;
        let mut index = 0;
        loop {
            let at = self.at.index(index);
            let item = JsonSeed {
                cx: self.cx,
                at: &at,
            };
            match seq.next_element_seed(item)? {
                Some(Ok(item)) => items.push(item),
                Some(Err(e)) => {
                    error.get_or_insert(e);
                }
                None => break,
            }
            index += 1;
        }
        Ok(match error {
            Some(e) => Err(e),
            None => Ok(Value::variant(bv::JSON_ARRAY, [Value::list(items)])),
        })
    }

    /// The `json.Object` of the table of `entries`. (Of a key written
    /// twice the last value counts.)
    fn table<'de, A: MapAccess<'de>>(
        self,
        mut entries: Entries<'de, A>,
    ) -> Result<Outcome, A::Error> {
        let mut members = Vec::new();
        let mut failed = false;
        while let Some(key) = entries.key()? {
            let at = self.at.key(&key);
            let member = entries.value(JsonSeed {
                cx: self.cx,
                at: &at,
            })?;
            failed |= member.is_err();
            members.push((Arc::<str>::from(key), member));
        }
        if failed {
            let sorted: BTreeMap<_, _> = members.into_iter().collect();
            let first = sorted.into_values().find_map(Result::err);
            return Ok(Err(first.expect("a member has no json.Value")));
        }
        let members = members
            .into_iter()
            .map(|(key, member)| (Value::String(key), member.expect("no member failed")));
        Ok(Ok(Value::variant(bv::JSON_OBJECT, [Value::map(members)])))
    }
}

impl<'de> DeserializeSeed<'de> for JsonSeed<'_> {
    type Value = Outcome;

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Outcome, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for JsonSeed<'_> {
    type Value = Outcome;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E: de::Error>(self, b: bool) -> Result<Outcome, E> {
        Ok(self.scalar(Scalar::Bool(b)))
    }

    fn visit_i64<E: de::Error>(self, n: i64) -> Result<Outcome, E> {
        self.cx.digits.integer();
        Ok(self.scalar(Scalar::Int(n)))
    }

    fn visit_u64<E: de::Error>(self, n: u64) -> Result<Outcome, E> {
        self.cx.digits.integer();
        Ok(self.scalar(Scalar::unsigned(n)))
    }

    fn visit_f64<E: de::Error>(self, f: f64) -> Result<Outcome, E> {
        Ok(self.scalar(self.cx.digits.float(f)))
    }

    fn visit_str<E: de::Error>(self, s: &str) -> Result<Outcome, E> {
        Ok(self.scalar(self.cx.string(s)))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Outcome, E> {
        Ok(self.scalar(Scalar::Null))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Outcome, A::Error> {
        self.array(seq)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Outcome, A::Error> {
        let first = match open(self.cx, &mut map)? {
            Opened::Datetime(text) => return Ok(self.scalar(Scalar::Datetime(&text))),
            Opened::Table(first) => first,
        };
        self.table(Entries { first, map })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typeinfo::FieldType;

    thread_local! {
        /// How many parts the readings of this thread kept.
        pub(super) static CAPTURED: Cell<usize> = const { Cell::new(0) };
    }

    /// A runtime that knows `type E { Wrap(E), Leaf(List(Int)) }` and
    /// `type R { e: E }`, and the field types of the two.
    fn runtime() -> (crate::vm::Vm, FieldType, FieldType) {
        let source = "type E { Wrap(E), Leaf(List(Int)) }\ntype R { e: E }\n";
        let program = crate::session::testing::compile_decls_str(source)
            .unwrap_or_else(|e| panic!("the types compile: {}", e[0].message));
        let vm = crate::vm::Vm::new(crate::vm::HostIo::process());
        for info in program.types.iter() {
            vm.runtime.types.lock().insert(info.clone());
        }
        let id = |name: &str| {
            (program.types.iter())
                .find(|info| info.name == name)
                .expect("the type is described")
                .id
        };
        (vm, FieldType::Enum(id("E")), FieldType::Record(id("R")))
    }

    /// The variant `Leaf` of `numbers` numbers in `levels` `Wrap`s, each
    /// variant's name after its fields (`toml`: as an inline table).
    fn nested(numbers: usize, levels: usize, toml: bool) -> String {
        let (eq, comma) = match toml {
            true => (" = ", ", "),
            false => (":", ","),
        };
        let key = |key: &str| match toml {
            true => key.to_string(),
            false => format!("\"{key}\""),
        };
        let variant = |name: &str, fields: String| {
            let (fields_key, variant_key) = (key("fields"), key("variant"));
            format!("{{{fields_key}{eq}[{fields}]{comma}{variant_key}{eq}\"{name}\"}}")
        };
        let list: Vec<String> = (0..numbers).map(|n| n.to_string()).collect();
        let mut doc = variant("Leaf", format!("[{}]", list.join(comma)));
        for _ in 0..levels {
            doc = variant("Wrap", doc);
        }
        doc
    }

    /// How many parts `read` kept, and that it decoded a variant.
    fn kept(read: impl FnOnce() -> Outcome) -> usize {
        CAPTURED.with(|captured| captured.set(0));
        let decoded = read();
        assert!(decoded.is_ok(), "{:?}", decoded.err());
        CAPTURED.with(Cell::get)
    }

    /// A variant's fields that come before its name are kept once: a
    /// variant nested in them is handed its part of what is kept, not
    /// kept again (the parts kept grew with the depth times the size of
    /// the document); a `json.Value` keeps only the names.
    #[test]
    fn nested_variants_are_kept_once() {
        let (vm, e, r) = runtime();
        let reader = Reader::new(&vm.runtime);
        const NUMBERS: usize = 1_000;
        for levels in [1, 10, 20] {
            let text = nested(NUMBERS, levels, false);
            let parts = Parts::finding();
            let from_text = kept(|| json_text(&reader, &e, text.as_bytes(), &parts).expect("JSON"));
            assert!(
                from_text <= NUMBERS + 10 * levels,
                "{levels} levels: {from_text} parts kept"
            );
            let value = json_text(&reader, &FieldType::JsonValue, text.as_bytes(), &parts)
                .expect("JSON")
                .expect("a json.Value");
            let from_value = kept(|| json_value(&reader, &e, &value, &parts).expect("not deep"));
            // (Each variant's name alone.)
            assert_eq!(from_value, levels + 1, "{levels} levels of a json.Value");
            let toml = format!("e = {}\n", nested(NUMBERS, levels.min(10), true));
            let top = Top::One(&r);
            let from_toml =
                kept(|| toml_text(&reader, &top, &toml, "\u{0}", &parts).expect("TOML"));
            assert!(
                from_toml <= NUMBERS + 10 * levels,
                "{levels} levels of TOML: {from_toml} parts kept"
            );
        }
    }
}
