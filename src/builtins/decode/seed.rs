//! Decoding a document as its parser reads it: a value of the type is
//! made of the text, part by part, with no tree of the document in
//! between (a `DeserializeSeed` of the type, over the parser of the
//! text).
//!
//! What a program sees is what the walk of the tree ([`value`]) gives
//! of the same text, and that walk stays the referee until every
//! decoder reads this way:
//!
//! - broken text anywhere wins over a value of the wrong type before
//!   it: the error of a part is kept, and the text is read to its end;
//! - of a key written twice in an object the last value counts, and its
//!   error with it: an earlier one's error is dropped;
//! - a record's error is that of the first field it declares that has
//!   one, a map's that of the first key in sorted order, a list's that
//!   of the first element that has one;
//! - a number is an `Int` by its digits, as [`parse_document`] reads
//!   it.
//!
//! A part of a type with a written `json.Decode` impl stops the reading
//! ([`Read::Written`]): such a document is decoded by the walk, which
//! reads it twice (`vm::decode`).
//!
//! [`value`]: super::value
//! [`parse_document`]: crate::builtins::json::parse_document

use std::borrow::Cow;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::sync::Arc;

use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};

use super::{
    At, Document, Error, I64_MAX_PLUS_ONE, Numbers, Reader, Rules, decodable_enum,
    decodable_record, field_record_type, invalid_date_message, invalid_time_message, parse_date,
    parse_datetime, parse_time, shown, unsupported_field_type_message, variant_field_message,
};
use crate::builtins::json;
use crate::builtins::registry::decoding::Leaf;
use crate::builtins::time::{make_date, make_datetime, make_time};
use crate::typeinfo::{FieldType, Shape, Tag, TypeInfo, bv};
use crate::value::{Float, Value};

/// What a part of a document decodes into: its value, or the decoder's
/// error of it. (An error of the text is the parser's.)
pub(crate) type Outcome = Result<Value, Error>;

/// What a reading of a text gives.
pub(crate) enum Read {
    /// The text is a document: the value it decodes into, or why it is
    /// none.
    Decoded(Outcome),
    /// The text is no document.
    Text(serde_json::Error),
    /// A part of the document is of a type with a written impl: the
    /// reading stopped there.
    Written,
}

/// The JSON text `text` decoded into a value of `expected` by `r`.
pub(crate) fn json_text(r: &Reader, expected: &FieldType, text: &[u8]) -> Read {
    let at = Cell::new(0);
    let written = Cell::new(false);
    let cx = Cx {
        r,
        rules: &json::RULES,
        digits: Digits::Text(json::Node::new(text, &at)),
        written: &written,
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
        _ if written.get() => Read::Written,
        Ok(decoded) => Read::Decoded(decoded),
        Err(e) => Read::Text(e),
    }
}

/// What a reading reads with.
#[derive(Clone, Copy)]
struct Cx<'c> {
    r: &'c Reader<'c>,
    rules: &'static Rules,
    digits: Digits<'c>,
    /// Set where a part of a type with a written impl is met.
    written: &'c Cell<bool>,
}

/// Where a reading takes the numbers of a document from.
#[derive(Clone, Copy)]
enum Digits<'t> {
    /// From the JSON text: the parser gives every number that is no
    /// integer literal of 64 bits as an `f64`, so each number's digits
    /// are read again, in the order the parser gives the numbers.
    Text(json::Node<'t>),
    /// From a document already read (`parse_document`'s), whose numbers
    /// are held as they decode.
    Read,
}

impl Digits<'_> {
    /// Past the number the parser gave as an integer.
    fn integer(self) {
        if let Digits::Text(node) = self {
            node.next_number();
        }
    }

    /// The number the parser gave as the `f64` `f`: an `Int` when its
    /// digits are a whole number in `Int`'s range. (One that is no
    /// finite `f64` is held as no number, as `parse_document` holds it.)
    fn float(self, f: f64) -> Scalar<'static> {
        if let Digits::Text(node) = self
            && let Some(n) = json::exact_int(node.next_number())
        {
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
}

impl Scalar<'_> {
    /// What the format calls its kind in a type mismatch.
    fn kind(self) -> &'static str {
        match self {
            Scalar::Null => "null",
            Scalar::Bool(_) => "boolean",
            Scalar::Int(_) | Scalar::Float(_) => "number",
            Scalar::String(_) => "string",
        }
    }

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
            Scalar::String(s) => Value::variant(bv::JSON_STRING, [Value::String(s.into())]),
        }
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
        Skip(self.cx.digits)
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
            true => {
                self.cx.written.set(true);
                Err(E::custom("a part of a type with a written decode"))
            }
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
        let rules = self.cx.rules;
        let at = self.at;
        let mismatch = |expected: &'static str| Error::mismatch(expected, scalar.kind(), at);
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
        let mismatch = |expected: &'static str| Error::mismatch(expected, scalar.kind(), at);
        match self.expected {
            FieldType::String => match scalar {
                Scalar::String(s) => Ok(Value::String(s.into())),
                _ => Err(mismatch("String")),
            },
            // A number is an `Int` when it is held as one; else one with
            // a fraction or out of range is no `Int`, and nothing is cut
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
            FieldType::Date => match scalar {
                Scalar::String(s) => parse_date(s)
                    .map(make_date)
                    .map_err(|e| Error::other(invalid_date_message(s, e), at)),
                _ => Err(mismatch("time.Date")),
            },
            FieldType::Time => match scalar {
                Scalar::String(s) => parse_time(s)
                    .map(make_time)
                    .map_err(|e| Error::other(invalid_time_message(s, &e), at)),
                _ => Err(mismatch("time.Time")),
            },
            FieldType::DateTime => match scalar {
                Scalar::String(s) => parse_datetime(s)
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
                let mut values = Vec::new();
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
            Want::JsonValue => return Ok(Ok(JsonSeed(self.cx.digits).array(seq)?)),
            Want::Fails(e) => e,
            Want::Map(_) => mismatch("Map"),
            Want::Record(..) => mismatch(rules.table),
            Want::Variant(..) => mismatch(rules.variant),
            Want::Scalar => mismatch(scalar_name(self.expected)),
        };
        while seq.next_element_seed(self.skip())?.is_some() {}
        Ok(Err(failed))
    }

    /// The value of the part that is an object, read by `map`.
    fn object<'de, A: MapAccess<'de>>(self, mut map: A) -> Result<Outcome, A::Error> {
        let at = self.at;
        let mismatch = |expected: &'static str| Error::mismatch(expected, "object", at);
        let failed = match self.want() {
            Want::Option(inner) => {
                let some = self.of(inner, at);
                some.unwritten()?;
                let some = some.object(map)?;
                return Ok(some.map(|value| Value::variant(bv::SOME, [value])));
            }
            Want::Map(inner) => return self.map(map, inner),
            Want::Record(ty, args) => return self.record(map, &ty, args),
            Want::Variant(ty, args) => return self.variant(map, &ty, args),
            Want::JsonValue => return Ok(Ok(JsonSeed(self.cx.digits).object(map)?)),
            Want::Fails(e) => e,
            Want::List(_) => mismatch("List"),
            Want::Tuple(_) => mismatch("Tuple"),
            Want::Scalar => mismatch(scalar_name(self.expected)),
        };
        while map
            .next_entry_seed(PhantomData::<IgnoredAny>, self.skip())?
            .is_some()
        {}
        Ok(Err(failed))
    }

    /// The map of values of `inner` that the object `map` reads is: of
    /// the keys that are no value of the type, the first in sorted
    /// order is the error.
    fn map<'de, A: MapAccess<'de>>(
        self,
        mut map: A,
        inner: &FieldType,
    ) -> Result<Outcome, A::Error> {
        let mut entries: Vec<(Arc<str>, Outcome)> = Vec::new();
        while let Some(key) = map.next_key_seed(Key)? {
            let decoded = map.next_value_seed(self.of(inner, &self.at.key(&key)))?;
            entries.push((key.into(), decoded));
        }
        if entries.iter().all(|(_, decoded)| decoded.is_ok()) {
            let entries = entries
                .into_iter()
                .filter_map(|(key, decoded)| decoded.ok().map(|value| (Value::String(key), value)));
            return Ok(Ok(Value::map(entries)));
        }
        // (Of a key written twice the last value counts, its error too.)
        let mut sorted = BTreeMap::new();
        for (key, decoded) in entries {
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
    /// object `map` reads is: of the fields that are no value of their
    /// type or missing, the first the type declares is the error.
    fn record<'de, A: MapAccess<'de>>(
        self,
        mut map: A,
        ty: &Arc<TypeInfo>,
        args: &[FieldType],
    ) -> Result<Outcome, A::Error> {
        let fields = ty.fields();
        let mut read: Vec<Option<Outcome>> = (0..fields.len()).map(|_| None).collect();
        while let Some(key) = map.next_key_seed(Key)? {
            match fields.iter().position(|(name, _)| *name == *key) {
                Some(k) => {
                    let (name, field) = &fields[k];
                    let field = field.instantiated(args);
                    let at = self.at.key(name);
                    read[k] = Some(map.next_value_seed(self.of(&field, &at))?);
                }
                None => map.next_value_seed(self.skip())?,
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
                None => match &*field.instantiated(args) {
                    FieldType::Option(_) => Value::variant(bv::NONE, []),
                    _ => return Ok(Err(Error::MissingField(self.at.key(name).to_string()))),
                },
            });
        }
        Ok(Ok(Value::record(ty.clone(), values)))
    }

    /// The variant of the enum type `ty` at its type arguments `args`
    /// that the object `map` reads is, as `json.stringify` writes one
    /// with fields: `{"fields":[...],"variant":"Name"}`, other keys
    /// ignored. The two may come in either order: they are kept as they
    /// are read, and decoded once the object ends.
    fn variant<'de, A: MapAccess<'de>>(
        self,
        mut map: A,
        ty: &Arc<TypeInfo>,
        args: &[FieldType],
    ) -> Result<Outcome, A::Error> {
        let at = self.at;
        let mut name = None;
        let mut items = None;
        while let Some(key) = map.next_key_seed(Key)? {
            match &*key {
                "variant" => name = Some(map.next_value_seed(Kept(self.cx.digits))?),
                "fields" => items = Some(map.next_value_seed(Kept(self.cx.digits))?),
                _ => map.next_value_seed(self.skip())?,
            }
        }
        let at_name = at.key("variant");
        let name = match &name {
            Some(serde_json::Value::String(name)) => name,
            Some(name) => return Ok(Err(Error::mismatch("String", name.kind(), &at_name))),
            None => return Ok(Err(Error::MissingField(at_name.to_string()))),
        };
        let Some(tag) = Tag::named(ty, name) else {
            return Ok(Err(no_variant(ty, name, &at_name)));
        };
        let declared = &ty.variants()[tag.ordinal() as usize].fields;
        // (A variant without fields is its name, never an object.)
        if declared.is_empty() {
            return Ok(Err(Error::mismatch("String", "object", at)));
        }
        let at_fields = at.key("fields");
        let items = match &items {
            Some(serde_json::Value::Array(items)) => &items[..],
            Some(items) => return Ok(Err(Error::mismatch("array", items.kind(), &at_fields))),
            None => &[],
        };
        if items.len() != declared.len() {
            let message = format_args!(
                "variant `{name}` of `{}` has {}, got {}",
                ty.name,
                count(declared.len()),
                items.len()
            );
            return Ok(Err(Error::other(message, &at_fields)));
        }
        // The fields, read again from what was kept.
        let cx = Cx {
            digits: Digits::Read,
            ..*self.cx
        };
        let mut fields = Vec::with_capacity(items.len());
        for (index, (item, field)) in items.iter().zip(declared).enumerate() {
            let field = field.instantiated(args);
            let seed = Seed {
                cx: &cx,
                expected: &field,
                at: &at_fields.index(index),
            };
            match seed
                .deserialize(item)
                .map_err(<A::Error as de::Error>::custom)?
            {
                Ok(value) => fields.push(value),
                // (A field no decoder fills was refused before the
                // document was read; this is the fallback.)
                Err(Error::Unsupported(declared)) => {
                    let message = variant_field_message(&ty.name, name, &declared);
                    return Ok(Err(Error::Other(message)));
                }
                Err(e) => return Ok(Err(e)),
            }
        }
        Ok(Ok(Value::variant(tag, fields)))
    }
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
        f.write_str("a JSON value")
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
        self.scalar(Scalar::String(s))
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

/// The key of a member of an object.
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
struct Skip<'t>(Digits<'t>);

impl<'de> DeserializeSeed<'de> for Skip<'_> {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Skip<'_> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<(), E> {
        self.0.integer();
        Ok(())
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<(), E> {
        self.0.integer();
        Ok(())
    }

    fn visit_f64<E: de::Error>(self, f: f64) -> Result<(), E> {
        self.0.float(f);
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
        while map
            .next_entry_seed(PhantomData::<IgnoredAny>, self)?
            .is_some()
        {}
        Ok(())
    }
}

/// The reading of a part that is kept as a document (`parse_document`'s)
/// to be decoded later.
#[derive(Clone, Copy)]
struct Kept<'t>(Digits<'t>);

impl<'de> DeserializeSeed<'de> for Kept<'_> {
    type Value = serde_json::Value;

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        match self.0 {
            Digits::Text(node) => node.deserialize(d),
            Digits::Read => serde::Deserialize::deserialize(d),
        }
    }
}

/// The reading of a part into the `json.Value` that holds it.
#[derive(Clone, Copy)]
struct JsonSeed<'t>(Digits<'t>);

impl JsonSeed<'_> {
    /// The `json.Array` of the array `seq` reads.
    fn array<'de, A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(self)? {
            items.push(item);
        }
        Ok(Value::variant(bv::JSON_ARRAY, [Value::list(items)]))
    }

    /// The `json.Object` of the object `map` reads. (Of a key written
    /// twice the last value counts.)
    fn object<'de, A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut members = Vec::new();
        while let Some((key, value)) = map.next_entry_seed(Key, self)? {
            members.push((Value::String(key.into()), value));
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
        self.0.integer();
        Ok(Scalar::Int(n).json())
    }

    fn visit_u64<E: de::Error>(self, n: u64) -> Result<Value, E> {
        self.0.integer();
        Ok(Scalar::unsigned(n).json())
    }

    fn visit_f64<E: de::Error>(self, f: f64) -> Result<Value, E> {
        Ok(self.0.float(f).json())
    }

    fn visit_str<E: de::Error>(self, s: &str) -> Result<Value, E> {
        Ok(Scalar::String(s).json())
    }

    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Scalar::Null.json())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Value, A::Error> {
        self.array(seq)
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Value, A::Error> {
        self.object(map)
    }
}
