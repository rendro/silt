use std::fmt;
use std::sync::Arc;

use super::{Float, Value};
use crate::typeinfo::bv;
use crate::vm::VmError;

// ── Host functions ─────────────────────────────────────────────────

/// The Rust side of a host function: it takes the call's arguments and
/// gives its result.
pub type HostImpl = Arc<dyn Fn(&[Value]) -> Result<Value, VmError> + Send + Sync>;

/// A function of a host module, as the VM calls it.
pub struct HostFn {
    /// The function's name, qualified by its module (`mylib.double`).
    pub name: String,
    pub call: HostImpl,
    /// What its signature says it returns: each result is checked
    /// against it.
    pub returns: HostShape,
    /// The type and row variables of the return type, by their index in
    /// `HostShape::Var` and `HostShape::Record::row`, with where the
    /// arguments of a call show each.
    pub vars: Vec<HostVar>,
    /// The number of parameters its signature declares: a call with
    /// another number of arguments shows no variable.
    pub arity: usize,
}

/// A type variable (`a`) or row variable (`...r`) of a host function's
/// return type, with the places among its parameters where an argument
/// shows the type a call gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostVar {
    pub name: String,
    /// The places, in the order the parameters are written.
    pub sources: Vec<HostSource>,
}

/// A place among a host function's parameters where the type of a
/// variable shows: the parameter `param` (its name `name`), then the
/// parts `path` of its value. A `type a` parameter is the type itself
/// (`of_type`). For a row variable, the place is a record, and `written`
/// are the fields its type writes: the others are the row's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSource {
    pub param: usize,
    pub name: String,
    pub path: Vec<HostStep>,
    pub of_type: bool,
    pub written: Vec<String>,
}

/// One step into a value, toward the place of a variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostStep {
    /// An element of a list or a set.
    Element,
    /// A key, or a value, of a map.
    Key,
    Value,
    /// The value of a `Some`, an `Ok`, an `Err`.
    Some,
    Ok,
    Err,
    /// The `n`th part of a tuple.
    Part(usize),
    /// The field of a record.
    Field(String),
}

/// What a call's arguments show of a variable: a value of the type, or
/// the type itself (the value of a `type a` parameter), and where.
#[derive(Debug, Clone)]
pub struct Witness<'a> {
    pub value: Value,
    pub source: &'a HostSource,
}

/// Why a result is not of the type a host function's signature returns:
/// its shape, or the type a variable has at the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostFault {
    Shape,
    Var(usize),
}

/// The shape of the values of a type a host function's signature
/// names, as far as a value shows it: a type whose values are not told
/// apart here admits anything, and so does a variable that the call's
/// arguments do not show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostShape {
    Any,
    /// `Never`, which no value is of: a function that returns it can
    /// only fail.
    Never,
    Int,
    Float,
    Bool,
    String,
    Bytes,
    Unit,
    List(Box<HostShape>),
    Set(Box<HostShape>),
    Map(Box<HostShape>, Box<HostShape>),
    Option(Box<HostShape>),
    Result(Box<HostShape>, Box<HostShape>),
    Tuple(Vec<HostShape>),
    /// A channel; what it carries is not shown by the value.
    Channel(Box<HostShape>),
    /// An anonymous record, its fields as written, open where it has a
    /// row variable (`{name: String, ...r}`): the index of `r` among the
    /// function's variables, and its name.
    Record {
        fields: Vec<(String, HostShape)>,
        row: Option<(usize, String)>,
    },
    /// The type variable of that index among the function's variables,
    /// and its name.
    Var(usize, String),
}

impl HostFn {
    /// What the arguments `args` of a call show of each of the
    /// function's variables: the first value found at one of its places,
    /// if any.
    pub fn witnesses(&self, args: &[Value]) -> Vec<Option<Witness<'_>>> {
        if args.len() != self.arity {
            return vec![None; self.vars.len()];
        }
        (self.vars.iter())
            .map(|var| {
                var.sources.iter().find_map(|source| {
                    let arg = args.get(source.param)?;
                    let value = match source.of_type {
                        true => matches!(
                            arg,
                            Value::TypeDescriptor(_) | Value::PrimitiveDescriptor(_)
                        )
                        .then(|| arg.clone())?,
                        false => found_at(arg, &source.path)?,
                    };
                    Some(Witness { value, source })
                })
            })
            .collect()
    }
}

/// The first value at the place `path` of `value`.
fn found_at(value: &Value, path: &[HostStep]) -> Option<Value> {
    let Some((step, rest)) = path.split_first() else {
        return Some(value.clone());
    };
    match (step, value) {
        (HostStep::Element, Value::List(items)) => {
            items.iter().find_map(|item| found_at(&item, rest))
        }
        (HostStep::Element, Value::Set(items)) => {
            items.iter().find_map(|item| found_at(item, rest))
        }
        (HostStep::Key, Value::Map(entries)) => {
            entries.iter().find_map(|(key, _)| found_at(key, rest))
        }
        (HostStep::Value, Value::Map(entries)) => {
            entries.iter().find_map(|(_, value)| found_at(value, rest))
        }
        (HostStep::Some, Value::Variant(variant)) if variant.is(bv::SOME) => {
            found_at(variant.fields().first()?, rest)
        }
        (HostStep::Ok, Value::Variant(variant)) if variant.is(bv::OK) => {
            found_at(variant.fields().first()?, rest)
        }
        (HostStep::Err, Value::Variant(variant)) if variant.is(bv::ERR) => {
            found_at(variant.fields().first()?, rest)
        }
        (HostStep::Part(n), Value::Tuple(parts)) => found_at(parts.get(*n)?, rest),
        (HostStep::Field(name), Value::Record(record)) => found_at(record.get(name)?, rest),
        _ => None,
    }
}

/// How many pairs of parts `same_type` compares at most. A check costs
/// at most this many steps whatever the size of the values, and the
/// first parts of a result are where a function that returns the wrong
/// type shows it: a result of another kind, record or enum shows it at
/// the first pair, a list of another element type at its first
/// elements. 64 pairs cover a small record, tuple or list whole and two
/// or three levels of a nested one, at a cost of a few hundred
/// nanoseconds a call.
pub const SAME_TYPE_PARTS: usize = 64;

/// Whether `value` could be of the type of `witness`: they differ in
/// nothing both show, among the first `SAME_TYPE_PARTS` pairs of their
/// parts compared. Past those, and for a part that only one of them
/// shows (the elements of an empty list, the payload of another
/// variant), nothing shows a difference: the value passes. A value that
/// is the witness itself, or shares its parts, passes at once.
pub fn same_type(witness: &Value, value: &Value) -> bool {
    let mut budget = SAME_TYPE_PARTS;
    let mut work: Vec<(Value, Value)> = vec![(witness.clone(), value.clone())];
    // (What may still be added to the work, within the budget.)
    let room = |budget: usize, work: &Vec<(Value, Value)>| budget.saturating_sub(work.len());
    let descriptor =
        |v: &Value| matches!(v, Value::TypeDescriptor(_) | Value::PrimitiveDescriptor(_));
    while let Some((witness, value)) = work.pop() {
        if budget == 0 {
            return true;
        }
        budget -= 1;
        match (&witness, &value) {
            (Value::Int(_), Value::Int(_))
            | (Value::Float(_), Value::Float(_))
            | (Value::Bool(_), Value::Bool(_))
            | (Value::String(_), Value::String(_))
            | (Value::Bytes(_), Value::Bytes(_))
            | (Value::Unit, Value::Unit)
            | (Value::Channel(_), Value::Channel(_))
            | (Value::Handle(_), Value::Handle(_))
            | (Value::TcpListener(_), Value::TcpListener(_))
            | (Value::TcpStream(_), Value::TcpStream(_)) => {}
            (Value::Unit, Value::Tuple(parts)) | (Value::Tuple(parts), Value::Unit) => {
                if !parts.is_empty() {
                    return false;
                }
            }
            (Value::List(a), Value::List(b)) => {
                if a.shares(b) {
                    continue;
                }
                if let (Some(a), Some(b)) = (a.first(), b.first()) {
                    work.push((a, b));
                }
            }
            (Value::Set(a), Value::Set(b)) => {
                if a.shares(b) {
                    continue;
                }
                if let (Some(a), Some(b)) = (a.iter().next(), b.iter().next()) {
                    work.push((a.clone(), b.clone()));
                }
            }
            (Value::Map(a), Value::Map(b)) => {
                if a.shares(b) {
                    continue;
                }
                if let (Some((ak, av)), Some((bk, bv))) = (a.iter().next(), b.iter().next()) {
                    work.push((ak.clone(), bk.clone()));
                    work.push((av.clone(), bv.clone()));
                }
            }
            (Value::Tuple(a), Value::Tuple(b)) => {
                if a.len() != b.len() {
                    return false;
                }
                if shares(a, b) {
                    continue;
                }
                let room = room(budget, &work);
                work.extend(
                    (a.iter().zip(b.iter()).take(room)).map(|(a, b)| (a.clone(), b.clone())),
                );
            }
            (Value::Record(a), Value::Record(b)) => {
                if a.type_id() != b.type_id() {
                    return false;
                }
                if shares(a.fields(), b.fields()) {
                    continue;
                }
                // (Anonymous records share one id: their fields are
                // their type, each name counted as a part compared.)
                if !std::sync::Arc::ptr_eq(a.ty(), b.ty())
                    && a.type_id() == crate::typeinfo::ty::ANON_RECORD
                {
                    let names = a.fields().len().max(b.fields().len());
                    if names > budget {
                        return true;
                    }
                    budget -= names;
                    if !a
                        .named()
                        .map(|(name, _)| name)
                        .eq(b.named().map(|(name, _)| name))
                    {
                        return false;
                    }
                }
                let room = room(budget, &work);
                work.extend(
                    (a.fields().iter().zip(b.fields()).take(room))
                        .map(|(a, b)| (a.clone(), b.clone())),
                );
            }
            (Value::Variant(a), Value::Variant(b)) => {
                if a.type_id() != b.type_id() {
                    return false;
                }
                // (The fields of another variant are not compared.)
                if a.ordinal() != b.ordinal() || shares(a.fields(), b.fields()) {
                    continue;
                }
                let room = room(budget, &work);
                work.extend(
                    (a.fields().iter().zip(b.fields()).take(room))
                        .map(|(a, b)| (a.clone(), b.clone())),
                );
            }
            (a, b) if a.is_fn() && b.is_fn() => {}
            (a, b) if descriptor(a) && descriptor(b) => {}
            (Value::Dict(_), _) | (_, Value::Dict(_)) => {}
            _ => return false,
        }
    }
    true
}

/// Whether the parts `a` and `b` are one, shared: those of one value.
fn shares(a: &[Value], b: &[Value]) -> bool {
    !a.is_empty() && std::ptr::eq(a.as_ptr(), b.as_ptr()) && a.len() == b.len()
}

/// Whether `value` could be of the type `ty`, the value of a `type a`
/// parameter: by the type's head (`Int`, `List`, a type of the program),
/// as far as the value shows it.
pub fn of_type(ty: &Value, value: &Value) -> bool {
    match ty {
        Value::PrimitiveDescriptor(name) => match (*name, value) {
            ("Int", Value::Int(_))
            | ("Float", Value::Float(_))
            | ("String", Value::String(_))
            | ("Bool", Value::Bool(_)) => true,
            ("Int" | "Float" | "String" | "Bool", _) => false,
            _ => true,
        },
        Value::TypeDescriptor(info) => {
            let head = |name: &str| crate::defs::builtin_type_id(name);
            let id = match value {
                Value::Record(record) => Some(record.type_id()),
                Value::Variant(variant) => Some(variant.type_id()),
                Value::Int(_) => head("Int"),
                Value::Float(_) => head("Float"),
                Value::Bool(_) => head("Bool"),
                Value::String(_) => head("String"),
                Value::Bytes(_) => head("Bytes"),
                Value::List(_) => head("List"),
                Value::Set(_) => head("Set"),
                Value::Map(_) => head("Map"),
                Value::Tuple(_) => head("Tuple"),
                Value::Channel(_) => head("Channel"),
                _ => None,
            };
            id.is_none_or(|id| id == info.id)
        }
        _ => true,
    }
}

/// The type of the value `witness` shows, as a message names it: the
/// name of a record's or a variant's type, else the kind of the value.
pub fn witness_type(witness: &Value) -> String {
    match witness {
        Value::Record(record) if record.type_id() == crate::typeinfo::ty::ANON_RECORD => {
            let fields: Vec<String> = (record.named())
                .map(|(name, value)| format!("{name}: {}", witness_type(value)))
                .collect();
            format!("{{{}}}", fields.join(", "))
        }
        Value::Record(record) => record.ty().name.clone(),
        Value::Variant(variant) => variant.ty().name.clone(),
        Value::TypeDescriptor(info) => info.name.clone(),
        Value::PrimitiveDescriptor(name) => (*name).to_string(),
        Value::Unit => "()".to_string(),
        other => other.kind().to_string(),
    }
}

impl HostShape {
    /// Whether `value` is a value of the shape, its variables as the
    /// call's arguments show them (`witnesses`, by the variables'
    /// indexes): the first part it is not of, if any.
    pub fn check(&self, value: &Value, witnesses: &[Option<Witness<'_>>]) -> Result<(), HostFault> {
        fn all<'v>(
            mut shapes: impl Iterator<Item = (&'v HostShape, &'v Value)>,
            witnesses: &[Option<Witness<'_>>],
        ) -> Result<(), HostFault> {
            shapes.try_for_each(|(shape, v)| shape.check(v, witnesses))
        }
        let yes = |admits: bool| match admits {
            true => Ok(()),
            false => Err(HostFault::Shape),
        };
        match (self, value) {
            (HostShape::Any, _)
            | (HostShape::Int, Value::Int(_))
            | (HostShape::Float, Value::Float(_))
            | (HostShape::Bool, Value::Bool(_))
            | (HostShape::String, Value::String(_))
            | (HostShape::Bytes, Value::Bytes(_))
            | (HostShape::Unit, Value::Unit)
            | (HostShape::Channel(_), Value::Channel(_)) => Ok(()),
            (HostShape::List(item), Value::List(items)) => match items.range_ends() {
                None => all(items.runs().flatten().map(|v| (&**item, v)), witnesses),
                Some((lo, _)) => item.check(&Value::Int(lo), witnesses),
            },
            (HostShape::Set(item), Value::Set(items)) => {
                all(items.iter().map(|v| (&**item, v)), witnesses)
            }
            (HostShape::Map(k, v), Value::Map(entries)) => all(
                entries
                    .iter()
                    .flat_map(|(key, value)| [(&**k, key), (&**v, value)]),
                witnesses,
            ),
            (HostShape::Option(item), Value::Variant(variant)) => match variant.fields() {
                [v] if variant.is(bv::SOME) => item.check(v, witnesses),
                [] => yes(variant.is(bv::NONE)),
                _ => Err(HostFault::Shape),
            },
            (HostShape::Result(ok, err), Value::Variant(variant)) => match variant.fields() {
                [v] if variant.is(bv::OK) => ok.check(v, witnesses),
                [v] if variant.is(bv::ERR) => err.check(v, witnesses),
                _ => Err(HostFault::Shape),
            },
            (HostShape::Tuple(items), Value::Tuple(values)) => {
                yes(items.len() == values.len())?;
                all(items.iter().zip(values.iter()), witnesses)
            }
            (HostShape::Record { fields, row }, Value::Record(record)) => {
                let anonymous = record.type_id() == crate::typeinfo::ty::ANON_RECORD;
                for (name, shape) in fields {
                    match record.get(name) {
                        Some(v) => shape.check(v, witnesses)?,
                        None => return Err(HostFault::Shape),
                    }
                }
                let written = |name: &str| fields.iter().any(|(field, _)| field == name);
                let Some((at, _)) = row else {
                    // (A closed record is anonymous, of its fields alone.)
                    return yes(anonymous && record.named().all(|(name, _)| written(name)));
                };
                let Some(Some(witness)) = witnesses.get(*at) else {
                    return Ok(());
                };
                let Value::Record(given) = &witness.value else {
                    return Ok(());
                };
                let rest = |name: &str| !witness.source.written.iter().any(|w| w == name);
                let same_written = (fields.iter().map(|(name, _)| name.as_str())).eq(witness
                    .source
                    .written
                    .iter()
                    .map(String::as_str));
                let row_fits = match given.type_id() == crate::typeinfo::ty::ANON_RECORD {
                    // The row's fields are the given record's that its
                    // parameter does not write; nothing else is added.
                    true => {
                        anonymous
                            && (given.named().filter(|(name, _)| rest(name)))
                                .all(|(name, w)| record.get(name).is_some_and(|v| same_type(w, v)))
                            && (record.named().filter(|(name, _)| !written(name)))
                                .all(|(name, _)| given.get(name).is_some() && rest(name))
                    }
                    // A record of a declared type the row stands for is
                    // given back as a record of that type.
                    false if same_written => {
                        record.type_id() == given.type_id()
                            && (given.named().filter(|(name, _)| rest(name)))
                                .all(|(name, w)| record.get(name).is_some_and(|v| same_type(w, v)))
                    }
                    false => true,
                };
                match row_fits {
                    true => Ok(()),
                    false => Err(HostFault::Var(*at)),
                }
            }
            (HostShape::Var(at, _), value) => match witnesses.get(*at) {
                Some(Some(witness)) => {
                    let fits = match witness.source.of_type {
                        true => of_type(&witness.value, value),
                        false => same_type(&witness.value, value),
                    };
                    match fits {
                        true => Ok(()),
                        false => Err(HostFault::Var(*at)),
                    }
                }
                _ => Ok(()),
            },
            _ => Err(HostFault::Shape),
        }
    }
}

impl fmt::Display for HostShape {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let list = |f: &mut fmt::Formatter<'_>, name: &str, items: &[&HostShape]| {
            write!(f, "{name}(")?;
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{item}")?;
            }
            write!(f, ")")
        };
        match self {
            HostShape::Any => write!(f, "_"),
            HostShape::Never => write!(f, "Never"),
            HostShape::Int => write!(f, "Int"),
            HostShape::Float => write!(f, "Float"),
            HostShape::Bool => write!(f, "Bool"),
            HostShape::String => write!(f, "String"),
            HostShape::Bytes => write!(f, "Bytes"),
            HostShape::Unit => write!(f, "()"),
            HostShape::List(item) => list(f, "List", &[item]),
            HostShape::Set(item) => list(f, "Set", &[item]),
            HostShape::Map(k, v) => list(f, "Map", &[k, v]),
            HostShape::Option(item) => list(f, "Option", &[item]),
            HostShape::Result(ok, err) => list(f, "Result", &[ok, err]),
            HostShape::Tuple(items) => list(f, "", &items.iter().collect::<Vec<_>>()),
            HostShape::Channel(item) => list(f, "Channel", &[item]),
            HostShape::Record { fields, row } => {
                write!(f, "{{")?;
                for (i, (name, shape)) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{name}: {shape}")?;
                }
                if let Some((_, name)) = row {
                    if !fields.is_empty() {
                        write!(f, ", ")?;
                    }
                    write!(f, "...{name}")?;
                }
                write!(f, "}}")
            }
            HostShape::Var(_, name) => write!(f, "{name}"),
        }
    }
}

impl fmt::Debug for HostFn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HostFn({})", self.name)
    }
}

// ── Host function conversion traits ────────────────────────────────

/// Convert a `Value` into a Rust type.
pub trait FromValue: Sized {
    fn from_value(value: &Value) -> Result<Self, String>;
}

/// Convert a Rust type into a `Value`. The conversion fails when the
/// Rust value has no silt counterpart (a NaN or infinite `f64`: a silt
/// `Float` is always finite); a host function whose result fails to
/// convert raises a runtime error.
pub trait IntoValue {
    fn into_value(self) -> Result<Value, String>;
}

impl FromValue for Value {
    fn from_value(value: &Value) -> Result<Self, String> {
        Ok(value.clone())
    }
}

impl IntoValue for Value {
    fn into_value(self) -> Result<Value, String> {
        Ok(self)
    }
}

impl FromValue for i64 {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Int(n) => Ok(*n),
            other => Err(format!("expected Int, got {}", other.kind())),
        }
    }
}

impl IntoValue for i64 {
    fn into_value(self) -> Result<Value, String> {
        Ok(Value::Int(self))
    }
}

impl FromValue for f64 {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Float(n) => Ok(n.get()),
            Value::Int(n) => Ok(*n as f64),
            other => Err(format!("expected Float, got {}", other.kind())),
        }
    }
}

impl IntoValue for f64 {
    fn into_value(self) -> Result<Value, String> {
        // A silt `Float` is always finite and never `-0.0`: a NaN or
        // infinite result has no silt value, so it is an error rather
        // than a `Float` that every comparison, hash and container path
        // would mishandle.
        Float::new(self)
            .map(Value::Float)
            .ok_or_else(|| format!("non-finite float result: {self}"))
    }
}

impl FromValue for bool {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Bool(b) => Ok(*b),
            other => Err(format!("expected Bool, got {}", other.kind())),
        }
    }
}

impl IntoValue for bool {
    fn into_value(self) -> Result<Value, String> {
        Ok(Value::Bool(self))
    }
}

impl FromValue for String {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::String(s) => Ok(s.to_string()),
            other => Err(format!("expected String, got {}", other.kind())),
        }
    }
}

impl IntoValue for String {
    fn into_value(self) -> Result<Value, String> {
        Ok(Value::String(self.into()))
    }
}

impl IntoValue for &str {
    fn into_value(self) -> Result<Value, String> {
        Ok(Value::String(self.into()))
    }
}

impl FromValue for () {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Unit => Ok(()),
            other => Err(format!("expected Unit, got {}", other.kind())),
        }
    }
}

impl IntoValue for () {
    fn into_value(self) -> Result<Value, String> {
        Ok(Value::Unit)
    }
}

impl FromValue for Vec<Value> {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::List(xs) => xs.to_vec().map_err(|too_long| too_long.to_string()),
            other => Err(format!("expected List, got {}", other.kind())),
        }
    }
}

impl IntoValue for Vec<Value> {
    fn into_value(self) -> Result<Value, String> {
        Ok(Value::list(self))
    }
}

impl<T: IntoValue> IntoValue for Option<T> {
    fn into_value(self) -> Result<Value, String> {
        Ok(match self {
            Some(v) => Value::variant(bv::SOME, [v.into_value()?]),
            None => Value::variant(bv::NONE, []),
        })
    }
}

impl<T: IntoValue> IntoValue for Result<T, String> {
    fn into_value(self) -> Result<Value, String> {
        Ok(match self {
            Ok(v) => Value::variant(bv::OK, [v.into_value()?]),
            Err(e) => Value::variant(bv::ERR, [Value::String(e.into())]),
        })
    }
}
