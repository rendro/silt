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
mod list;

#[cfg(test)]
mod tests;

pub use convert::{FromValue, HostFn, HostImpl, HostShape, IntoValue};
pub use fmt::{Shown, Written};
pub(crate) use list::MAX_RANGE_MATERIALIZE;
pub use list::{IntTotal, IntoIter, Iter, List, TooLong};

#[derive(Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    String(String),
    List(List),
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
    /// The list of `items`.
    pub fn list(items: Vec<Value>) -> Value {
        Value::List(List::from(items))
    }

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
    /// variant.
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
    /// Whether the value is a function, or has one inside it: the one
    /// answer of the run time to whether a value can be compared,
    /// ordered or hashed. The checker rejects those of a value that
    /// holds a function (`Equal`, `Compare` and `Hash` are decided by
    /// structure); where its answer does not reach (a key a callback
    /// gave to `list.sort_by`), this stands behind it: two functions
    /// would be ordered by their addresses, differently from run to
    /// run. A channel, a handle and a connection are equal by
    /// identity, and are not functions.
    ///
    /// Asked by the VM's `compare` (src/vm/arithmetic.rs), the gate of
    /// `==` and `!=` (src/vm/run.rs), the `equal`, `compare` and
    /// `hash` methods (src/vm/dispatch.rs) and the collection builtins
    /// (`ensure_no_fn`, src/builtins/collections.rs). Locked by
    /// tests/typecheck/container_fn_compare_runtime_gate_tests.rs.
    pub fn contains_fn(&self) -> bool {
        // A worklist, not recursion: values nest as deep as a program
        // builds them.
        let mut pending = vec![self];
        while let Some(value) = pending.pop() {
            match value {
                Value::VmClosure(_)
                | Value::BuiltinFn(_)
                | Value::HostFn(_)
                | Value::VariantConstructor(..) => {
                    return true;
                }
                Value::List(items) => {
                    // (A list that holds no element holds no function.)
                    if let list::Elements::Items(items) = items.elements() {
                        pending.extend(items);
                    }
                }
                Value::Tuple(items) | Value::Variant(_, items) => pending.extend(items.iter()),
                Value::Set(items) => pending.extend(items.iter()),
                Value::Map(entries) => {
                    for (k, v) in entries.iter() {
                        pending.push(k);
                        pending.push(v);
                    }
                }
                Value::Record(_, fields) => pending.extend(fields.values()),
                _ => {}
            }
        }
        false
    }
}
