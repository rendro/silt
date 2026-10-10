use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::builtins::registry::BuiltinId;
use crate::bytecode;
use crate::runtime::handle::{TaskHandle, TcpListenerHandle, TcpStreamHandle};
use crate::runtime::sync::Channel;
use crate::typeinfo::{Tag, TypeInfo};

mod convert;
mod fmt;
mod key;

#[cfg(test)]
mod tests;

pub use convert::{FromValue, HostFn, HostImpl, HostShape, IntoValue};
pub use fmt::{Shown, Written};

/// Maximum number of elements that may be materialized from a range into a
/// list, JSON array, or similar eager collection.  Prevents accidental OOM
/// when a user writes something like `(1..1_000_000_000) |> list.reverse`.
pub(crate) const MAX_RANGE_MATERIALIZE: usize = 10_000_000;

/// Return the number of elements in the inclusive range `lo..=hi`, or an error
/// string if the count exceeds [`MAX_RANGE_MATERIALIZE`].
pub(crate) fn checked_range_len(lo: i64, hi: i64) -> Result<usize, String> {
    if lo > hi {
        return Ok(0);
    }
    let len = (hi as i128 - lo as i128 + 1) as u128;
    if len > MAX_RANGE_MATERIALIZE as u128 {
        Err(format!(
            "range {}..{} has {} elements; materializing more than {} is not allowed",
            lo, hi, len, MAX_RANGE_MATERIALIZE,
        ))
    } else {
        Ok(len as usize)
    }
}

#[derive(Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    String(String),
    List(Arc<Vec<Value>>),
    Range(i64, i64), // inclusive on both ends: start..end
    Map(Arc<BTreeMap<Value, Value>>),
    Set(Arc<BTreeSet<Value>>),
    Tuple(Vec<Value>),
    /// A record: its type and its fields by name.
    Record(Arc<TypeInfo>, Arc<BTreeMap<String, Value>>),
    /// A variant: which variant of which enum, and its fields.
    Variant(Tag, Vec<Value>),
    VmClosure(Arc<bytecode::VmClosure>),
    /// A builtin function: its row of the builtin registry.
    BuiltinFn(BuiltinId),
    /// A function of a host module an embedder declared to the session
    /// (see `session::HostModule`), installed by the program that
    /// imports the module.
    HostFn(Arc<HostFn>),
    /// The constructor of a variant with fields, as a value.
    VariantConstructor(Tag),
    /// Runtime token for a record or enum type, or a builtin container
    /// type, passed as a `type a` argument. Keeps `type T`-style values
    /// distinct from primitives (see `PrimitiveDescriptor`).
    TypeDescriptor(Arc<TypeInfo>),
    PrimitiveDescriptor(String), // "Int", "Float", "String", "Bool" — for json.parse_map etc.
    Channel(Arc<Channel>),
    Handle(Arc<TaskHandle>),
    /// Immutable byte sequence. Structural equality and hashing — two
    /// `Bytes` values are equal iff they hold the same bytes. Forward-
    /// compatible with a future `Type::Bytes` promotion: literal syntax
    /// (`b"..."`), pattern matching, and method dispatch can be layered on
    /// top of this variant without changing semantics.
    Bytes(Arc<Vec<u8>>),
    /// TCP listener handle. Identity-based equality (id field). Created by
    /// `tcp.listen`; consumed by `tcp.accept`.
    TcpListener(Arc<TcpListenerHandle>),
    /// TCP stream handle. Identity-based equality. Wraps a trait object
    /// so plain TCP and (future) TLS streams share the same handle type
    /// transparently — the TLS layer in v0.9 PR 3 will substitute a
    /// rustls-wrapped stream behind the same `Arc<Mutex<Box<dyn ReadWrite>>>`.
    TcpStream(Arc<TcpStreamHandle>),
    Unit,
}

impl Value {
    /// The variant `tag` (a [`Tag`], or a builtin variant of
    /// [`crate::typeinfo::bv`]) with the fields `fields`.
    pub fn variant(tag: impl Into<Tag>, fields: Vec<Value>) -> Value {
        Value::Variant(tag.into(), fields)
    }

    /// A record of the builtin record type `ty` (`ty::DATE`).
    pub fn builtin_record(ty: crate::defs::TypeId, fields: BTreeMap<String, Value>) -> Value {
        Value::Record(crate::typeinfo::builtin_type(ty).clone(), Arc::new(fields))
    }
}

impl Value {
    /// The value's kind, as an error names it: the name of the `Value`
    /// variant. A range is a "Range", not a "List", though the two are
    /// one type to a program: the error shows what the value is.
    ///
    /// Not for method dispatch: that is
    /// `crate::types::canonical::dispatch_type_for_value`.
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Int(_) => "Int",
            Value::Float(_) => "Float",
            Value::Bool(_) => "Bool",
            Value::String(_) => "String",
            Value::List(_) => "List",
            Value::Range(..) => "Range",
            Value::Map(_) => "Map",
            Value::Set(_) => "Set",
            Value::Tuple(_) => "Tuple",
            Value::Record(..) => "Record",
            Value::Variant(..) => "Variant",
            // Surface name matches `Type::Fun`'s Display (`Fn(...) -> R`)
            // and the canonical dispatch name returned by
            // `dispatch_type_name`. Round 71 follow-up unified
            // `Function` / `Fun` / `Fn` on `"Fn"`.
            Value::VmClosure(_) => "Fn",
            Value::BuiltinFn(_) => "BuiltinFn",
            Value::HostFn(_) => "HostFn",
            Value::VariantConstructor(..) => "VariantConstructor",
            Value::TypeDescriptor(_) => "TypeDescriptor",
            Value::PrimitiveDescriptor(_) => "PrimitiveDescriptor",
            Value::Channel(_) => "Channel",
            Value::Handle(_) => "Handle",
            Value::Bytes(_) => "Bytes",
            Value::TcpListener(_) => "TcpListener",
            Value::TcpStream(_) => "TcpStream",
            Value::Unit => "Unit",
        }
    }
}

impl Value {
    /// Get the length of a list or range, if applicable.
    pub fn collection_len(&self) -> Option<usize> {
        match self {
            Value::List(xs) => Some(xs.len()),
            Value::Range(lo, hi) => {
                if hi >= lo {
                    (*hi as i128 - *lo as i128 + 1).try_into().ok()
                } else {
                    Some(0)
                }
            }
            _ => None,
        }
    }
}
