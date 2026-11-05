//! Decoding a document as its parser reads it: a value of the type is
//! made of the text, part by part, with no tree of the document in
//! between (a `DeserializeSeed` of the type, over the parser of the
//! text). One decoder for every format: JSON text (`json.parse`), TOML
//! text (`toml.parse`, `toml.parse_list`, `toml.parse_map`), and a JSON
//! document already read (`json.decode` of a `json.Value`, a postgres
//! `json` or `jsonb` cell).
//!
//! What a program sees is what the walk of the tree ([`value`]) gives
//! of the same document, and that walk stays the referee until every
//! decoder reads this way:
//!
//! - broken text anywhere wins over a value of the wrong type before
//!   it: the error of a part is kept, and the text is read to its end;
//! - of a key written twice in an object the last value counts, and its
//!   error with it: an earlier one's error is dropped;
//! - a record's error is that of the first field it declares that has
//!   one, a map's that of the first key in sorted order, a list's that
//!   of the first element that has one;
//! - a JSON number is an `Int` by its digits, as [`parse_document`]
//!   reads it; a TOML number by its kind;
//! - a TOML date or time literal is a datetime, which the date and time
//!   types read from its text (the parser gives it as a table of one
//!   key, its mark, [`DATETIME`]).
//!
//! A part of a type with a written `json.Decode` impl stops the reading
//! (`Ok(None)`): such a document is decoded by the walk, which reads it
//! twice (`vm::decode`).
//!
//! [`value`]: super::value
//! [`parse_document`]: crate::builtins::json::parse_document

use std::borrow::Cow;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;

use serde::de::value::{MapDeserializer, SeqDeserializer};
use serde::de::{
    self, DeserializeSeed, IgnoredAny, IntoDeserializer, MapAccess, SeqAccess, Visitor,
};

use super::{
    At, Error, I64_MAX_PLUS_ONE, Numbers, Reader, Rules, decodable_enum, decodable_record,
    field_record_type, invalid_date_message, invalid_time_message, parse_date, parse_datetime,
    parse_time, shown, unsupported_field_type_message, variant_field_message,
};
use crate::builtins::registry::decoding::Leaf;
use crate::builtins::time::{make_date, make_datetime, make_time};
use crate::builtins::{json, toml};
use crate::typeinfo::{FieldType, Shape, Tag, TypeInfo, bv};
use crate::value::{Float, Value};

/// What a part of a document decodes into: its value, or the decoder's
/// error of it. (An error of the text is the parser's.)
pub(crate) type Outcome = Result<Value, Error>;

/// What a reading of a text gives: `Ok(Some(..))` of what the document
/// decodes into, `Ok(None)` where the document is the walk's (a part of
/// a type with a written impl, or a TOML table the walk reads as no
/// table of the text, [`open`]), `Err` of the parser's error where
/// the text is no document.
pub(crate) type Read<E> = Result<Option<Outcome>, E>;

/// The JSON text `text` decoded into a value of `expected` by `r`.
pub(crate) fn json_text(r: &Reader, expected: &FieldType, text: &[u8]) -> Read<serde_json::Error> {
    let at = Cell::new(0);
    let walk = Cell::new(false);
    let kept = Cell::new(None);
    let cx = Cx {
        r,
        rules: &json::RULES,
        format: Format::Json,
        digits: Digits::Text(json::Node::new(text, &at)),
        marked: None,
        walk: &walk,
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
    match read {
        _ if walk.get() => Ok(None),
        Ok(decoded) => Ok(Some(decoded)),
        Err(e) => Err(e),
    }
}

/// The JSON document `doc`, already read (as [`parse_document`] reads
/// one), decoded into a value of `expected` by `r`: `None` where the
/// document is the walk's.
///
/// [`parse_document`]: crate::builtins::json::parse_document
pub(crate) fn json_document(
    r: &Reader,
    expected: &FieldType,
    doc: &serde_json::Value,
) -> Option<Outcome> {
    let walk = Cell::new(false);
    let kept = Cell::new(None);
    let cx = Cx {
        r,
        rules: &json::RULES,
        format: Format::Json,
        digits: Digits::Read,
        marked: None,
        walk: &walk,
        kept: &kept,
    };
    let read = Seed {
        cx: &cx,
        expected,
        at: &At::Top,
    }
    .deserialize(ValueDe(doc));
    match read {
        Ok(decoded) if !walk.get() => Some(decoded),
        _ => None,
    }
}

/// A JSON document already read, read as serde_json reads it, but for
/// the order of an object's members: its `variant` comes first, so that
/// a variant's fields are decoded as they are read, not kept. (No other
/// part of a decoding depends on the order of the members of an object
/// that has each key once.)
#[derive(Clone, Copy)]
struct ValueDe<'d>(&'d serde_json::Value);

impl<'de> de::Deserializer<'de> for ValueDe<'de> {
    type Error = de::value::Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        use serde_json::Value as J;
        match self.0 {
            J::Null => visitor.visit_unit(),
            J::Bool(b) => visitor.visit_bool(*b),
            J::Number(n) => match (n.as_u64(), n.as_i64(), n.as_f64()) {
                (Some(n), _, _) => visitor.visit_u64(n),
                (None, Some(n), _) => visitor.visit_i64(n),
                (None, None, Some(f)) => visitor.visit_f64(f),
                // (A number serde_json holds is an `i64`, a `u64` or an
                // `f64`.)
                (None, None, None) => visitor.visit_f64(f64::NAN),
            },
            J::String(s) => visitor.visit_borrowed_str(s),
            J::Array(items) => {
                let mut seq = SeqDeserializer::new(items.iter().map(ValueDe));
                let read = visitor.visit_seq(&mut seq)?;
                seq.end()?;
                Ok(read)
            }
            J::Object(members) => {
                let name = members.get_key_value("variant");
                let rest = members.iter().filter(|(key, _)| *key != "variant");
                let entries = name.into_iter().chain(rest);
                let mut map = MapDeserializer::new(
                    entries.map(|(key, value)| (key.as_str(), ValueDe(value))),
                );
                let read = visitor.visit_map(&mut map)?;
                map.end()?;
                Ok(read)
            }
        }
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map struct enum identifier ignored_any
    }
}

impl<'de> IntoDeserializer<'de, de::value::Error> for ValueDe<'de> {
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

/// The TOML text `text` decoded by `r` as `top` says. A string that
/// starts with `marked` stands for the date or time literal after it
/// (`toml::document`).
pub(crate) fn toml_text(
    r: &Reader,
    top: &Top,
    text: &str,
    marked: &'static str,
) -> Read<::toml::de::Error> {
    let walk = Cell::new(false);
    let kept = Cell::new(None);
    let cx = Cx {
        r,
        rules: &toml::RULES,
        format: Format::Toml,
        digits: Digits::Read,
        marked: Some(marked),
        walk: &walk,
        kept: &kept,
    };
    let de = ::toml::Deserializer::new(text);
    let map;
    let read = match top {
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
    };
    match read {
        _ if walk.get() => Ok(None),
        Ok(decoded) => Ok(Some(decoded)),
        Err(e) => Err(e),
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
    /// Set where the document is found to be the walk's.
    walk: &'c Cell<bool>,
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

    /// The error that stops the reading: the document is the walk's.
    fn for_the_walk<E: de::Error>(&self) -> E {
        self.walk.set(true);
        E::custom("the document is decoded by the walk")
    }
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
    Text(json::Node<'t>),
    /// From the parser as it gives them: a TOML document, whose numbers
    /// are an integer or a float by their kind, or a document already
    /// read, whose numbers are held as they decode.
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
    /// one that is no finite `f64` is held as no number, as
    /// `parse_document` holds it.
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
/// [`DATETIME`] is the literal whose text its value is, as `toml::Value`
/// reads one; one that holds more, or no datetime, is the walk's.
fn open<'de, A: MapAccess<'de>>(cx: &Cx, map: &mut A) -> Result<Opened<'de>, A::Error> {
    let first = map.next_key_seed(Key)?;
    if cx.format != Format::Toml || first.as_deref() != Some(DATETIME) {
        return Ok(Opened::Table(first));
    }
    let text = map.next_value_seed(Capture(*cx))?;
    let more = map.next_key_seed(PhantomData::<IgnoredAny>)?;
    match (text, more) {
        (Content::String(text), None) => match text.parse::<::toml::value::Datetime>() {
            Ok(datetime) => Ok(Opened::Datetime(datetime.to_string())),
            Err(_) => Err(cx.for_the_walk()),
        },
        _ => Err(cx.for_the_walk()),
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

    /// `Err` where a written impl decodes a value of the type: the
    /// reading stops.
    fn unwritten<E: de::Error>(&self) -> Result<(), E> {
        let (id, args) = match self.expected {
            FieldType::Record(id) | FieldType::Enum(id) => (id, &[][..]),
            FieldType::Applied(id, args) => (id, args.as_slice()),
            _ => return Ok(()),
        };
        match self.cx.r.written(*id, args) {
            true => Err(self.cx.for_the_walk()),
            false => Ok(()),
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
            Want::Option(inner) => {
                let some = self.of(inner, at);
                some.unwritten()?;
                some.scalar(scalar)?
                    .map(|value| Value::variant(bv::SOME, [value]))
            }
            Want::Fails(e) => Err(e),
            Want::JsonValue => Ok(scalar.json()),
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
        let rules = self.cx.rules;
        let at = self.at;
        let mismatch = |expected: &'static str| Error::mismatch(expected, "array", at);
        let failed = match self.want() {
            Want::Option(inner) => {
                let some = self.of(inner, at);
                some.unwritten()?;
                let some = some.array(seq)?;
                return Ok(some.map(|value| Value::variant(bv::SOME, [value])));
            }
            // The elements in order, up to the first that is no value of
            // the type.
            Want::List(inner) => {
                let mut values = Vec::with_capacity(room(&seq));
                let mut index = 0;
                while let Some(decoded) = seq.next_element_seed(self.of(inner, &at.index(index)))? {
                    match decoded {
                        Ok(value) => values.push(value),
                        Err(e) => {
                            while seq.next_element_seed(self.skip())?.is_some() {}
                            return Ok(Err(e));
                        }
                    }
                    index += 1;
                }
                return Ok(Ok(Value::list(values)));
            }
            // An array of as many elements as the tuple has, which it
            // takes before any element is decoded.
            Want::Tuple(elems) => {
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
                return Ok(match (count == elems.len(), error) {
                    (false, _) => Err(Error::other(
                        format_args!(
                            "expected an array of {} elements for a tuple, got {count}",
                            elems.len()
                        ),
                        at,
                    )),
                    (true, Some(e)) => Err(e),
                    (true, None) => Ok(Value::tuple(values)),
                });
            }
            Want::JsonValue => return Ok(Ok(JsonSeed(*self.cx).array(seq)?)),
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
        let at = self.at;
        let mismatch =
            |expected: &'static str| Error::mismatch(expected, self.cx.format.table(), at);
        let failed = match self.want() {
            Want::Option(inner) => {
                let some = self.of(inner, at);
                some.unwritten()?;
                let some = some.table(entries)?;
                return Ok(some.map(|value| Value::variant(bv::SOME, [value])));
            }
            Want::Map(inner) => return self.map(entries, inner),
            Want::Record(ty, args) => return self.record(entries, &ty, args),
            Want::Variant(ty, args) => return self.variant(entries, &ty, args),
            Want::JsonValue => return Ok(Ok(JsonSeed(*self.cx).table(entries)?)),
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
        let mut read: Vec<(Arc<str>, Outcome)> = Vec::new();
        while let Some(key) = entries.key()? {
            let decoded = entries.value(self.of(inner, &self.at.key(&key)))?;
            read.push((key.into(), decoded));
        }
        if read.iter().all(|(_, decoded)| decoded.is_ok()) {
            let read = read
                .into_iter()
                .filter_map(|(key, decoded)| decoded.ok().map(|value| (Value::String(key), value)));
            return Ok(Ok(Value::map(read)));
        }
        // (Of a key written twice the last value counts, its error too.)
        let mut sorted = BTreeMap::new();
        for (key, decoded) in read {
            sorted.insert(key, decoded);
        }
        let mut values = Vec::with_capacity(sorted.len());
        for (key, decoded) in sorted {
            match decoded {
                Ok(value) => values.push((Value::String(key), value)),
                Err(e) => return Ok(Err(e)),
            }
        }
        Ok(Ok(Value::map(values)))
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
        let mut read: Vec<Option<Outcome>> = (0..fields.len()).map(|_| None).collect();
        while let Some(key) = entries.key()? {
            match fields.iter().position(|(name, _)| *name == *key) {
                Some(k) => {
                    let (name, field) = &fields[k];
                    let field = field.instantiated(args);
                    let at = self.at.key(name);
                    read[k] = Some(entries.value(self.of(&field, &at))?);
                }
                None => entries.value(self.skip())?,
            }
        }
        let mut values = Vec::with_capacity(fields.len());
        for ((name, field), read) in fields.iter().zip(read) {
            values.push(match read {
                Some(Ok(value)) => value,
                Some(Err(Error::Unsupported(declared))) => {
                    let message = unsupported_field_type_message(&ty.name, name, &declared);
                    return Ok(Err(Error::Other(message)));
                }
                Some(Err(e)) => return Ok(Err(e)),
                // (TOML has no null: a key it leaves out is a `None`.)
                None => match &*field.instantiated(args) {
                    FieldType::Option(_) => Value::variant(bv::NONE, []),
                    _ => return Ok(Err(Error::MissingField(self.at.key(name).to_string()))),
                },
            });
        }
        Ok(Ok(Value::record(ty.clone(), values)))
    }

    /// The variant of the enum type `ty` at its type arguments `args`
    /// that the table of `entries` is, as `json.stringify` writes one
    /// with fields: `{"fields":[...],"variant":"Name"}`, other keys
    /// ignored. The fields are decoded as they are read where the name
    /// came before them; where they come first they are kept as read
    /// (once: a variant in them is read from what is kept, which it
    /// shares) and decoded once the name is read. A table that gives
    /// either key twice is the walk's.
    fn variant<'de, A: MapAccess<'de>>(
        self,
        mut entries: Entries<'de, A>,
        ty: &Arc<TypeInfo>,
        args: &[FieldType],
    ) -> Result<Outcome, A::Error> {
        let cx = self.cx;
        let at = self.at;
        let mut name = None;
        let mut fields = None;
        while let Some(key) = entries.key()? {
            match &*key {
                "variant" if name.is_some() => return Err(cx.for_the_walk()),
                "fields" if fields.is_some() => return Err(cx.for_the_walk()),
                "variant" => name = Some(entries.value(Capture(*cx))?),
                "fields" => {
                    fields = Some(match named(cx, ty, name.as_ref(), at) {
                        Ok(tag) => Fields::Read(entries.value(self.fields(&tag, args))?),
                        // (The name's error is the variant's.)
                        Err(_) if name.is_some() => {
                            entries.value(self.skip())?;
                            Fields::Passed
                        }
                        Err(_) => Fields::Kept(entries.value(Capture(*cx))?),
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
/// name, as the walk takes it: missing, no string, no variant's, or one
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
        // A record or enum type with a written impl: the impl's.
        self.unwritten()?;
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
            _ => Err(Error::Other((self.keys)(keys))),
        })
    }
}

/// The decoding of a part that is an array, or a `table`, into the
/// list or map type of its seed: anything else is the type mismatch of
/// no such part, as the walk's [`list`] and [`map`] say it.
///
/// [`list`]: super::list
/// [`map`]: super::map
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
    Array(Rc<[Content]>),
    /// A table's entries in the order read, a key written twice too.
    Table(Rc<[(String, Content)]>),
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
struct Capture<'t>(Cx<'t>);

impl<'de> DeserializeSeed<'de> for Capture<'_> {
    type Value = Content;

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Content, D::Error> {
        #[cfg(test)]
        tests::CAPTURED.with(|captured| captured.set(captured.get() + 1));
        d.deserialize_newtype_struct(KEPT, self)
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
        self.0.digits.integer();
        Ok(Content::Int(n))
    }

    fn visit_u64<E: de::Error>(self, n: u64) -> Result<Content, E> {
        self.0.digits.integer();
        Ok(Content::of(Scalar::unsigned(n)))
    }

    fn visit_f64<E: de::Error>(self, f: f64) -> Result<Content, E> {
        Ok(Content::of(self.0.digits.float(f)))
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
        Ok(self.0.kept.take().unwrap_or(Content::Null))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Content, A::Error> {
        let mut items = Vec::with_capacity(room(&seq));
        while let Some(item) = seq.next_element_seed(self)? {
            items.push(item);
        }
        Ok(Content::Array(items.into()))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Content, A::Error> {
        let first = match open(&self.0, &mut map)? {
            Opened::Datetime(text) => return Ok(Content::Datetime(text)),
            Opened::Table(first) => first,
        };
        let mut entries = Entries { first, map };
        let mut members = Vec::new();
        while let Some(key) = entries.key()? {
            members.push((key.into_owned(), entries.value(self)?));
        }
        Ok(Content::Table(members.into()))
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

/// The reading of a part into the `json.Value` that holds it.
#[derive(Clone, Copy)]
struct JsonSeed<'t>(Cx<'t>);

impl JsonSeed<'_> {
    /// The `json.Array` of the array `seq` reads.
    fn array<'de, A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::with_capacity(room(&seq));
        while let Some(item) = seq.next_element_seed(self)? {
            items.push(item);
        }
        Ok(Value::variant(bv::JSON_ARRAY, [Value::list(items)]))
    }

    /// The `json.Object` of the table of `entries`. (Of a key written
    /// twice the last value counts.)
    fn table<'de, A: MapAccess<'de>>(
        self,
        mut entries: Entries<'de, A>,
    ) -> Result<Value, A::Error> {
        let mut members = Vec::new();
        while let Some(key) = entries.key()? {
            members.push((Value::String(key.into()), entries.value(self)?));
        }
        Ok(Value::variant(bv::JSON_OBJECT, [Value::map(members)]))
    }
}

impl<'de> DeserializeSeed<'de> for JsonSeed<'_> {
    type Value = Value;

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for JsonSeed<'_> {
    type Value = Value;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E: de::Error>(self, b: bool) -> Result<Value, E> {
        Ok(Scalar::Bool(b).json())
    }

    fn visit_i64<E: de::Error>(self, n: i64) -> Result<Value, E> {
        self.0.digits.integer();
        Ok(Scalar::Int(n).json())
    }

    fn visit_u64<E: de::Error>(self, n: u64) -> Result<Value, E> {
        self.0.digits.integer();
        Ok(Scalar::unsigned(n).json())
    }

    fn visit_f64<E: de::Error>(self, f: f64) -> Result<Value, E> {
        Ok(self.0.digits.float(f).json())
    }

    fn visit_str<E: de::Error>(self, s: &str) -> Result<Value, E> {
        Ok(self.0.string(s).json())
    }

    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Scalar::Null.json())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Value, A::Error> {
        self.array(seq)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let first = match open(&self.0, &mut map)? {
            Opened::Datetime(text) => return Ok(Scalar::Datetime(&text).json()),
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
    fn kept(read: impl FnOnce() -> Option<Outcome>) -> usize {
        CAPTURED.with(|captured| captured.set(0));
        let decoded = read().expect("the reading decodes the document");
        assert!(decoded.is_ok(), "{:?}", decoded.err());
        CAPTURED.with(Cell::get)
    }

    /// A variant's fields that come before its name are kept once: a
    /// variant nested in them is handed its part of what is kept, not
    /// kept again (the parts kept grew with the depth times the size of
    /// the document); a document already read keeps only the names.
    #[test]
    fn nested_variants_are_kept_once() {
        let (vm, e, r) = runtime();
        let reader = Reader::new(&vm.runtime);
        const NUMBERS: usize = 1_000;
        for levels in [1, 10, 20] {
            let text = nested(NUMBERS, levels, false);
            let from_text = kept(|| json_text(&reader, &e, text.as_bytes()).expect("JSON"));
            assert!(
                from_text <= NUMBERS + 10 * levels,
                "{levels} levels: {from_text} parts kept"
            );
            let doc: serde_json::Value = serde_json::from_str(&text).expect("JSON");
            let from_doc = kept(|| json_document(&reader, &e, &doc));
            // (Each variant's name alone.)
            assert_eq!(from_doc, levels + 1, "{levels} levels of a document read");
            let toml = format!("e = {}\n", nested(NUMBERS, levels.min(10), true));
            let top = Top::One(&r);
            let from_toml = kept(|| toml_text(&reader, &top, &toml, "\u{0}").expect("TOML"));
            assert!(
                from_toml <= NUMBERS + 10 * levels,
                "{levels} levels of TOML: {from_toml} parts kept"
            );
        }
    }
}
