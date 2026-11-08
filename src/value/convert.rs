use std::cell::OnceCell;
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
/// variable shows: the parameter `param` (its name `name`, none when
/// it is written as a pattern), then the
/// parts `path` of its value. A `type a` parameter is the type itself
/// (`of_type`). For a row variable, the place is a record, and `written`
/// are the fields its type writes: the others are the row's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSource {
    pub param: usize,
    pub name: Option<String>,
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
    /// What the arguments `args` of a call show of the function's
    /// variables, each looked up when a check first asks for it.
    pub fn witnesses<'h, 'a>(&'h self, args: &'a [Value]) -> Witnesses<'h, 'a> {
        Witnesses {
            host: self,
            args,
            found: (0..self.vars.len()).map(|_| OnceCell::new()).collect(),
        }
    }
}

/// What the arguments of a call show of each variable of a host
/// function: the first value found at one of its places, among the
/// first `SAME_TYPE_PARTS` parts of the argument at each place. A variable
/// is looked up only when a check meets it with a value to compare (a
/// `None` result asks for none), so a call costs no more than that
/// whatever the size of its arguments.
pub struct Witnesses<'h, 'a> {
    host: &'h HostFn,
    args: &'a [Value],
    found: Vec<OnceCell<Option<Witness<'h>>>>,
}

impl<'h> Witnesses<'h, '_> {
    /// What the arguments show of the variable of index `at`, if any.
    pub fn get(&self, at: usize) -> Option<&Witness<'h>> {
        let found = self.found.get(at)?;
        found.get_or_init(|| self.find(at)).as_ref()
    }

    fn find(&self, at: usize) -> Option<Witness<'h>> {
        let host = self.host;
        // (A call with another number of arguments shows no variable.)
        if self.args.len() != host.arity {
            return None;
        }
        // (Each place its own budget: a long argument that shows nothing
        // leaves a later one its search. The places are as many as the
        // signature writes, so a call still costs no more with the
        // arguments' size.)
        host.vars.get(at)?.sources.iter().find_map(|source| {
            let mut budget = SAME_TYPE_PARTS;
            let arg = self.args.get(source.param)?;
            let value = match source.of_type {
                true => matches!(
                    arg,
                    Value::TypeDescriptor(_) | Value::PrimitiveDescriptor(_)
                )
                .then(|| arg.clone())?,
                false => found_at(arg, &source.path, &mut budget)?,
            };
            Some(Witness { value, source })
        })
    }
}

/// The first value at the place `path` of `value`, among the parts of
/// it that `budget` lets the search look at (each one it looks at costs
/// one).
fn found_at(value: &Value, path: &[HostStep], budget: &mut usize) -> Option<Value> {
    if *budget == 0 {
        return None;
    }
    *budget -= 1;
    let Some((step, rest)) = path.split_first() else {
        return Some(value.clone());
    };
    // (The first found in `values`, as far as the budget goes.)
    fn first<V: std::borrow::Borrow<Value>>(
        values: impl Iterator<Item = V>,
        rest: &[HostStep],
        budget: &mut usize,
    ) -> Option<Value> {
        for value in values {
            if *budget == 0 {
                return None;
            }
            if let Some(found) = found_at(value.borrow(), rest, budget) {
                return Some(found);
            }
        }
        None
    }
    match (step, value) {
        (HostStep::Element, Value::List(items)) => first(items.iter(), rest, budget),
        (HostStep::Element, Value::Set(items)) => first(items.iter(), rest, budget),
        (HostStep::Key, Value::Map(entries)) => {
            first(entries.iter().map(|(key, _)| key), rest, budget)
        }
        (HostStep::Value, Value::Map(entries)) => {
            first(entries.iter().map(|(_, value)| value), rest, budget)
        }
        (HostStep::Some, Value::Variant(variant)) if variant.is(bv::SOME) => {
            found_at(variant.fields().first()?, rest, budget)
        }
        (HostStep::Ok, Value::Variant(variant)) if variant.is(bv::OK) => {
            found_at(variant.fields().first()?, rest, budget)
        }
        (HostStep::Err, Value::Variant(variant)) if variant.is(bv::ERR) => {
            found_at(variant.fields().first()?, rest, budget)
        }
        (HostStep::Part(n), Value::Tuple(parts)) => found_at(parts.get(*n)?, rest, budget),
        (HostStep::Field(name), Value::Record(record)) => found_at(record.get(name)?, rest, budget),
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
/// parts compared. Each element of a list, a set or a map is compared
/// with the witness's first. Past those, and for a part that only one of them
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
                // (Each element of the value, as far as the bound goes,
                // against the witness's first.)
                if let Some(first) = a.first() {
                    let room = room(budget, &work);
                    work.extend(b.iter().take(room).map(|b| (first.clone(), b)));
                }
            }
            (Value::Set(a), Value::Set(b)) => {
                if a.shares(b) {
                    continue;
                }
                if let Some(first) = a.iter().next() {
                    let room = room(budget, &work);
                    work.extend(b.iter().take(room).map(|b| (first.clone(), b.clone())));
                }
            }
            (Value::Map(a), Value::Map(b)) => {
                if a.shares(b) {
                    continue;
                }
                if let Some((key, value)) = a.iter().next() {
                    let room = room(budget, &work) / 2;
                    for (k, v) in b.iter().take(room.max(1)) {
                        work.push((key.clone(), k.clone()));
                        work.push((value.clone(), v.clone()));
                    }
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
                // (Anonymous records share one id: their field names
                // are their type, compared whole as an id is, outside
                // the bound; their values within it.)
                if !std::sync::Arc::ptr_eq(a.ty(), b.ty())
                    && a.type_id() == crate::typeinfo::ty::ANON_RECORD
                    && !(a.named().map(|(name, _)| name)).eq(b.named().map(|(name, _)| name))
                {
                    return false;
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

/// The type of the value `witness` shows, as a message names it: in
/// full as far as the value shows it (`List(Int)`, `Map(String, Int)`,
/// `Box(Int)`), `_` for a part it does not show (the elements of an
/// empty list, the value of a `None`, what a channel carries) and for
/// the parts past the first `SAME_TYPE_PARTS`. A function is an `Fn`,
/// and a type given as a value is of the type `type Int`.
pub fn witness_type(witness: &Value) -> String {
    shown_type(witness, &mut { SAME_TYPE_PARTS })
}

/// The type `ty`, the value of a `type a` parameter, denotes: `Int` of
/// the value `Int`, `List(Int)` of `List(Int)`.
pub fn given_type(ty: &Value) -> String {
    match ty {
        Value::PrimitiveDescriptor("Unit") => "()".to_string(),
        Value::PrimitiveDescriptor(name) => (*name).to_string(),
        Value::TypeDescriptor(info) if info.name == "Unit" => "()".to_string(),
        Value::TypeDescriptor(info) => info.name.clone(),
        other => witness_type(other),
    }
}

/// `witness_type` of `value`, of the parts `budget` still lets it name.
fn shown_type(value: &Value, budget: &mut usize) -> String {
    if *budget == 0 {
        return "_".to_string();
    }
    *budget -= 1;
    let of = |value: Option<&Value>, budget: &mut usize| match value {
        Some(value) => shown_type(value, budget),
        None => "_".to_string(),
    };
    match value {
        Value::Int(_) => "Int".to_string(),
        Value::Float(_) => "Float".to_string(),
        Value::Bool(_) => "Bool".to_string(),
        Value::String(_) => "String".to_string(),
        Value::Bytes(_) => "Bytes".to_string(),
        Value::Unit => "()".to_string(),
        Value::List(items) => format!("List({})", of(items.first().as_ref(), budget)),
        Value::Set(items) => format!("Set({})", of(items.iter().next(), budget)),
        Value::Map(entries) => match entries.iter().next() {
            Some((key, value)) => {
                let key = shown_type(key, budget);
                format!("Map({key}, {})", of(Some(value), budget))
            }
            None => "Map(_, _)".to_string(),
        },
        Value::Tuple(parts) => {
            let parts: Vec<String> = parts.iter().map(|part| of(Some(part), budget)).collect();
            format!("({})", parts.join(", "))
        }
        Value::Channel(_) => "Channel(_)".to_string(),
        Value::Handle(_) => "Handle(_)".to_string(),
        Value::Record(record) if record.type_id() == crate::typeinfo::ty::ANON_RECORD => {
            let fields: Vec<String> = (record.named())
                .map(|(name, value)| format!("{name}: {}", of(Some(value), budget)))
                .collect();
            format!("{{{}}}", fields.join(", "))
        }
        Value::Record(record) => {
            let declared = match &record.ty().shape {
                crate::typeinfo::Shape::Record(fields) => fields.iter().map(|(_, ty)| ty).collect(),
                _ => Vec::new(),
            };
            applied(&record.ty().name, &declared, record.fields(), budget)
        }
        Value::Variant(variant) => {
            let payload = variant.fields().first();
            if variant.is(bv::SOME) || variant.is(bv::NONE) {
                return format!("Option({})", of(payload, budget));
            }
            if variant.is(bv::OK) {
                return format!("Result({}, _)", of(payload, budget));
            }
            if variant.is(bv::ERR) {
                return format!("Result(_, {})", of(payload, budget));
            }
            let variants = match &variant.ty().shape {
                crate::typeinfo::Shape::Enum(variants) => variants.as_slice(),
                _ => &[],
            };
            // (The parameters every variant's fields have, and the ones
            // the variant of the value shows.)
            let every: Vec<&crate::typeinfo::FieldType> =
                variants.iter().flat_map(|v| v.fields.iter()).collect();
            let count = every
                .iter()
                .filter_map(|ty| last_param(ty))
                .max()
                .map_or(0, |n| n + 1);
            let own: Vec<&crate::typeinfo::FieldType> = (variants
                .get(usize::from(variant.ordinal())))
            .map(|v| v.fields.iter().collect())
            .unwrap_or_default();
            applied_of(&variant.ty().name, count, &own, variant.fields(), budget)
        }
        Value::TypeDescriptor(_) | Value::PrimitiveDescriptor(_) => {
            format!("type {}", given_type(value))
        }
        value if value.is_fn() => "Fn".to_string(),
        other => other.kind().to_string(),
    }
}

/// The type `name`, declared with the fields `declared`, of the value
/// whose fields are `values`: with the type arguments they show.
fn applied(
    name: &str,
    declared: &[&crate::typeinfo::FieldType],
    values: &[Value],
    budget: &mut usize,
) -> String {
    let count = declared
        .iter()
        .filter_map(|ty| last_param(ty))
        .max()
        .map_or(0, |n| n + 1);
    applied_of(name, count, declared, values, budget)
}

/// `applied` of a type of `count` type parameters.
fn applied_of(
    name: &str,
    count: u16,
    declared: &[&crate::typeinfo::FieldType],
    values: &[Value],
    budget: &mut usize,
) -> String {
    if count == 0 {
        return name.to_string();
    }
    let mut args: Vec<Option<String>> = vec![None; usize::from(count)];
    for (ty, value) in declared.iter().zip(values) {
        bind_params(ty, value, &mut args, budget);
    }
    let args: Vec<String> = (args.into_iter())
        .map(|arg| arg.unwrap_or_else(|| "_".to_string()))
        .collect();
    format!("{name}({})", args.join(", "))
}

/// The index of the last type parameter the field type `ty` has, if
/// any.
fn last_param(ty: &crate::typeinfo::FieldType) -> Option<u16> {
    use crate::typeinfo::FieldType as F;
    match ty {
        F::Param(n) => Some(*n),
        F::List(t) | F::Option(t) | F::Map(t) => last_param(t),
        F::KeyedMap(n, t) => Some((*n).max(last_param(t).unwrap_or(0))),
        F::Tuple(ts) | F::Applied(_, ts) => ts.iter().filter_map(last_param).max(),
        _ => None,
    }
}

/// Enter into `args` the type arguments the value `value` of the field
/// type `ty` shows: its type where `ty` is a parameter, and so on in
/// the elements of a list, an option's value, a map's first entry and
/// a tuple's parts.
fn bind_params(
    ty: &crate::typeinfo::FieldType,
    value: &Value,
    args: &mut [Option<String>],
    budget: &mut usize,
) {
    use crate::typeinfo::FieldType as F;
    match (ty, value) {
        (F::Param(n), value) => {
            if let Some(arg @ None) = args.get_mut(usize::from(*n)) {
                *arg = Some(shown_type(value, budget));
            }
        }
        (F::List(t), Value::List(items)) => {
            if let Some(first) = items.first() {
                bind_params(t, &first, args, budget);
            }
        }
        (F::Option(t), Value::Variant(variant)) if variant.is(bv::SOME) => {
            if let Some(v) = variant.fields().first() {
                bind_params(t, v, args, budget);
            }
        }
        (F::Map(t), Value::Map(entries)) => {
            if let Some((_, v)) = entries.iter().next() {
                bind_params(t, v, args, budget);
            }
        }
        (F::KeyedMap(n, t), Value::Map(entries)) => {
            if let Some((k, v)) = entries.iter().next() {
                bind_params(&F::Param(*n), k, args, budget);
                bind_params(t, v, args, budget);
            }
        }
        (F::Tuple(ts), Value::Tuple(parts)) => {
            for (t, part) in ts.iter().zip(parts.iter()) {
                bind_params(t, part, args, budget);
            }
        }
        _ => {}
    }
}

impl HostShape {
    /// Whether `value` is a value of the shape, its variables as the
    /// call's arguments show them (`witnesses`, by the variables'
    /// indexes): the first part it is not of, if any. The check looks
    /// at the first `SAME_TYPE_PARTS` parts of the value (fields,
    /// elements, payloads) at most: a call costs no more than that
    /// whatever the size of its result, and what lies beyond them
    /// passes.
    pub fn check(&self, value: &Value, witnesses: &Witnesses<'_, '_>) -> Result<(), HostFault> {
        self.check_in(value, witnesses, &mut { SAME_TYPE_PARTS })
    }

    /// `check`, of the parts `budget` still lets it look at.
    fn check_in(
        &self,
        value: &Value,
        witnesses: &Witnesses<'_, '_>,
        budget: &mut usize,
    ) -> Result<(), HostFault> {
        fn all<'v>(
            shapes: impl Iterator<Item = (&'v HostShape, &'v Value)>,
            witnesses: &Witnesses<'_, '_>,
            budget: &mut usize,
        ) -> Result<(), HostFault> {
            for (shape, v) in shapes {
                if *budget == 0 {
                    break;
                }
                shape.check_in(v, witnesses, budget)?;
            }
            Ok(())
        }
        if *budget == 0 {
            return Ok(());
        }
        *budget -= 1;
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
                None => all(
                    items.runs().flatten().map(|v| (&**item, v)),
                    witnesses,
                    budget,
                ),
                Some((lo, _)) => item.check_in(&Value::Int(lo), witnesses, budget),
            },
            (HostShape::Set(item), Value::Set(items)) => {
                all(items.iter().map(|v| (&**item, v)), witnesses, budget)
            }
            (HostShape::Map(k, v), Value::Map(entries)) => all(
                entries
                    .iter()
                    .flat_map(|(key, value)| [(&**k, key), (&**v, value)]),
                witnesses,
                budget,
            ),
            (HostShape::Option(item), Value::Variant(variant)) => match variant.fields() {
                [v] if variant.is(bv::SOME) => item.check_in(v, witnesses, budget),
                [] => yes(variant.is(bv::NONE)),
                _ => Err(HostFault::Shape),
            },
            (HostShape::Result(ok, err), Value::Variant(variant)) => match variant.fields() {
                [v] if variant.is(bv::OK) => ok.check_in(v, witnesses, budget),
                [v] if variant.is(bv::ERR) => err.check_in(v, witnesses, budget),
                _ => Err(HostFault::Shape),
            },
            (HostShape::Tuple(items), Value::Tuple(values)) => {
                yes(items.len() == values.len())?;
                all(items.iter().zip(values.iter()), witnesses, budget)
            }
            (HostShape::Record { fields, row }, Value::Record(record)) => {
                let anonymous = record.type_id() == crate::typeinfo::ty::ANON_RECORD;
                for (name, shape) in fields {
                    match record.get(name) {
                        Some(v) => shape.check_in(v, witnesses, budget)?,
                        None => return Err(HostFault::Shape),
                    }
                }
                let written = |name: &str| fields.iter().any(|(field, _)| field == name);
                let Some((at, _)) = row else {
                    // (A closed record is anonymous, of its fields alone.)
                    return yes(anonymous && record.named().all(|(name, _)| written(name)));
                };
                let Some(witness) = witnesses.get(*at) else {
                    return Ok(());
                };
                let Value::Record(given) = &witness.value else {
                    return Ok(());
                };
                let rest = |name: &str| !witness.source.written.iter().any(|w| w == name);
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
                    // given back as a record of that type, whatever fields
                    // the parameter and the result write (and in whatever
                    // order): the checker types the result as that type.
                    false => {
                        record.type_id() == given.type_id()
                            && (given.named().filter(|(name, _)| rest(name)))
                                .all(|(name, w)| record.get(name).is_some_and(|v| same_type(w, v)))
                    }
                };
                match row_fits {
                    true => Ok(()),
                    false => Err(HostFault::Var(*at)),
                }
            }
            (HostShape::Var(at, _), value) => match witnesses.get(*at) {
                Some(witness) => {
                    let fits = match witness.source.of_type {
                        true => of_type(&witness.value, value),
                        false => same_type(&witness.value, value),
                    };
                    match fits {
                        true => Ok(()),
                        false => Err(HostFault::Var(*at)),
                    }
                }
                None => Ok(()),
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
