use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use crate::bytecode;

#[derive(Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    String(String),
    List(Rc<Vec<Value>>),
    Map(Rc<BTreeMap<Value, Value>>),
    Set(Rc<BTreeSet<Value>>),
    Tuple(Vec<Value>),
    Record(String, Rc<BTreeMap<String, Value>>),
    Variant(String, Vec<Value>),
    VmClosure(Rc<bytecode::VmClosure>),
    BuiltinFn(String),
    VariantConstructor(String, usize), // name, arity
    RecordDescriptor(String),          // record type name
    PrimitiveDescriptor(String),       // "Int", "Float", "String", "Bool" — for json.parse_map etc.
    Channel(Rc<Channel>),
    Handle(Rc<TaskHandle>),
    Unit,
}

/// A cooperative channel implemented as a bounded queue.
///
/// In a cooperative (non-preemptive) concurrency model, true rendezvous semantics
/// (where both sender and receiver must be simultaneously ready) are impractical
/// because tasks cannot be suspended mid-execution. Instead, `chan()` (capacity 0)
/// is treated as buffered-1: the sender can deposit one value without a receiver
/// being ready, and the receiver picks it up on its next turn. This provides
/// a reasonable approximation of unbuffered channel behaviour for cooperative tasks.
pub struct Channel {
    pub id: usize,
    pub buffer: RefCell<VecDeque<Value>>,
    pub capacity: usize,
    pub closed: Cell<bool>,
}

/// Result of attempting to send on a channel.
pub enum TrySendResult {
    /// Value was accepted into the buffer.
    Sent,
    /// Buffer is full; caller should retry later.
    Full,
    /// Channel has been closed; sending is not allowed.
    Closed,
}

/// Result of attempting to receive from a channel.
pub enum TryReceiveResult {
    /// A value was available.
    Value(Value),
    /// Buffer is empty but channel is still open; caller should retry later.
    Empty,
    /// Buffer is empty AND channel is closed; no more values will arrive.
    Closed,
}

impl Channel {
    pub fn new(id: usize, capacity: usize) -> Self {
        // Capacity 0 (rendezvous) is promoted to buffered-1. With preemptive
        // yielding, the sender's retry loop interleaves with other tasks,
        // giving a close approximation of rendezvous semantics: the sender
        // deposits one value and yields until a receiver consumes it.
        let effective_capacity = if capacity == 0 { 1 } else { capacity };
        Self {
            id,
            buffer: RefCell::new(VecDeque::new()),
            capacity: effective_capacity,
            closed: Cell::new(false),
        }
    }

    /// Try to send a value.
    pub fn try_send(&self, val: Value) -> TrySendResult {
        if self.closed.get() {
            return TrySendResult::Closed;
        }
        let mut buf = self.buffer.borrow_mut();
        if buf.len() < self.capacity {
            buf.push_back(val);
            TrySendResult::Sent
        } else {
            TrySendResult::Full
        }
    }

    /// Try to receive a value.
    pub fn try_receive(&self) -> TryReceiveResult {
        let mut buf = self.buffer.borrow_mut();
        if let Some(val) = buf.pop_front() {
            TryReceiveResult::Value(val)
        } else if self.closed.get() {
            TryReceiveResult::Closed
        } else {
            TryReceiveResult::Empty
        }
    }

    /// Close the channel. Future sends will fail; receives drain remaining
    /// buffered values and then return `Closed`.
    pub fn close(&self) {
        self.closed.set(true);
    }
}

/// Handle to a spawned task.
pub struct TaskHandle {
    pub id: usize,
    pub result: RefCell<Option<Result<Value, String>>>,
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(n) => write!(f, "{n}"),
            Value::Float(n) => write!(f, "{n}"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::String(s) => write!(f, "\"{s}\""),
            Value::List(xs) => f.debug_list().entries(xs.iter()).finish(),
            Value::Map(m) => f.debug_map().entries(m.iter()).finish(),
            Value::Set(s) => {
                write!(f, "#[")?;
                for (i, v) in s.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v:?}")?;
                }
                write!(f, "]")
            }
            Value::Tuple(vs) => {
                let mut t = f.debug_tuple("");
                for v in vs {
                    t.field(v);
                }
                t.finish()
            }
            Value::Record(name, fields) => {
                write!(f, "{name} {{")?;
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{k}: {v:?}")?;
                }
                write!(f, "}}")
            }
            Value::Variant(name, fields) => {
                if fields.is_empty() {
                    write!(f, "{name}")
                } else {
                    write!(f, "{name}(")?;
                    for (i, v) in fields.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{v:?}")?;
                    }
                    write!(f, ")")
                }
            }
            Value::VmClosure(c) => write!(f, "<fn:{}>", c.function.name),
            Value::BuiltinFn(name) => write!(f, "<builtin:{name}>"),
            Value::VariantConstructor(name, _) => write!(f, "<constructor:{name}>"),
            Value::RecordDescriptor(name) => write!(f, "<type:{name}>"),
            Value::PrimitiveDescriptor(name) => write!(f, "<type:{name}>"),
            Value::Channel(ch) => write!(f, "<channel:{}>", ch.id),
            Value::Handle(h) => write!(f, "<handle:{}>", h.id),
            Value::Unit => write!(f, "()"),
        }
    }
}

impl Value {
    /// Format a value in silt syntax, suitable for `io.inspect`.
    ///
    /// Unlike `Display` (which prints bare strings for user output) or `Debug`
    /// (which leaks Rust internals), this produces the silt-source representation:
    /// strings are quoted, collections use silt syntax, etc.
    pub fn format_silt(&self) -> String {
        match self {
            Value::Int(n) => format!("{n}"),
            Value::Float(n) => format!("{n}"),
            Value::Bool(b) => format!("{b}"),
            Value::String(s) => format!("\"{s}\""),
            Value::List(xs) => {
                let items: Vec<String> = xs.iter().map(|v| v.format_silt()).collect();
                format!("[{}]", items.join(", "))
            }
            Value::Map(m) => {
                let items: Vec<String> = m
                    .iter()
                    .map(|(k, v)| format!("{}: {}", k.format_silt(), v.format_silt()))
                    .collect();
                format!("#{{{}}}", items.join(", "))
            }
            Value::Set(s) => {
                let items: Vec<String> = s.iter().map(|v| v.format_silt()).collect();
                format!("#[{}]", items.join(", "))
            }
            Value::Tuple(vs) => {
                let items: Vec<String> = vs.iter().map(|v| v.format_silt()).collect();
                format!("({})", items.join(", "))
            }
            Value::Record(name, fields) => {
                let items: Vec<String> = fields
                    .iter()
                    .map(|(k, v)| format!("{k}: {}", v.format_silt()))
                    .collect();
                format!("{name} {{{}}}", items.join(", "))
            }
            Value::Variant(name, fields) => {
                if fields.is_empty() {
                    name.clone()
                } else {
                    let items: Vec<String> = fields.iter().map(|v| v.format_silt()).collect();
                    format!("{name}({})", items.join(", "))
                }
            }
            Value::VmClosure(_) => "<fn>".to_string(),
            Value::BuiltinFn(_) => "<fn>".to_string(),
            Value::VariantConstructor(name, _) => format!("<constructor:{name}>"),
            Value::RecordDescriptor(name) => format!("<type:{name}>"),
            Value::PrimitiveDescriptor(name) => format!("<type:{name}>"),
            Value::Channel(ch) => format!("<channel:{}>", ch.id),
            Value::Handle(h) => format!("<handle:{}>", h.id),
            Value::Unit => "()".to_string(),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(n) => write!(f, "{n}"),
            Value::Float(n) => write!(f, "{n}"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::String(s) => write!(f, "{s}"),
            Value::List(xs) => {
                write!(f, "[")?;
                for (i, v) in xs.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v}")?;
                }
                write!(f, "]")
            }
            Value::Map(m) => {
                write!(f, "#{{")?;
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    if let Value::String(s) = k {
                        write!(f, "\"{s}\": {v}")?;
                    } else {
                        write!(f, "{k}: {v}")?;
                    }
                }
                write!(f, "}}")
            }
            Value::Set(s) => {
                write!(f, "#[")?;
                for (i, v) in s.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v}")?;
                }
                write!(f, "]")
            }
            Value::Tuple(vs) => {
                write!(f, "(")?;
                for (i, v) in vs.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v}")?;
                }
                write!(f, ")")
            }
            Value::Record(name, fields) => {
                write!(f, "{name} {{")?;
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{k}: {v}")?;
                }
                write!(f, "}}")
            }
            Value::Variant(name, fields) => {
                if fields.is_empty() {
                    write!(f, "{name}")
                } else {
                    write!(f, "{name}(")?;
                    for (i, v) in fields.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{v}")?;
                    }
                    write!(f, ")")
                }
            }
            Value::VmClosure(c) => write!(f, "<fn:{}>", c.function.name),
            Value::BuiltinFn(name) => write!(f, "<builtin:{name}>"),
            Value::VariantConstructor(name, _) => write!(f, "<constructor:{name}>"),
            Value::RecordDescriptor(name) => write!(f, "<type:{name}>"),
            Value::PrimitiveDescriptor(name) => write!(f, "<type:{name}>"),
            Value::Channel(ch) => write!(f, "<channel:{}>", ch.id),
            Value::Handle(h) => write!(f, "<handle:{}>", h.id),
            Value::Unit => write!(f, "()"),
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Tuple(a), Value::Tuple(b)) => a == b,
            (Value::Variant(na, fa), Value::Variant(nb, fb)) => na == nb && fa == fb,
            (Value::Unit, Value::Unit) => true,
            (Value::List(a), Value::List(b)) => a == b,
            (Value::Map(a), Value::Map(b)) => a == b,
            (Value::Set(a), Value::Set(b)) => a == b,
            (Value::Record(na, fa), Value::Record(nb, fb)) => na == nb && fa == fb,
            (Value::RecordDescriptor(a), Value::RecordDescriptor(b)) => a == b,
            (Value::PrimitiveDescriptor(a), Value::PrimitiveDescriptor(b)) => a == b,
            (Value::Channel(a), Value::Channel(b)) => a.id == b.id,
            _ => false,
        }
    }
}

impl Eq for Value {}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Value {
    fn cmp(&self, other: &Self) -> Ordering {
        let disc = |v: &Value| -> u8 {
            match v {
                Value::Unit => 0,
                Value::Bool(_) => 1,
                Value::Int(_) => 2,
                Value::Float(_) => 3,
                Value::String(_) => 4,
                Value::List(_) => 5,
                Value::Tuple(_) => 6,
                Value::Map(_) => 7,
                Value::Set(_) => 8,
                Value::Record(..) => 9,
                Value::Variant(..) => 10,
                Value::Channel(_) => 11,
                Value::Handle(_) => 12,
                Value::VmClosure(_) => 13,
                Value::BuiltinFn(_) => 14,
                Value::VariantConstructor(..) => 15,
                Value::RecordDescriptor(_) => 16,
                Value::PrimitiveDescriptor(_) => 17,
            }
        };
        let d1 = disc(self);
        let d2 = disc(other);
        if d1 != d2 {
            return d1.cmp(&d2);
        }
        match (self, other) {
            (Value::Unit, Value::Unit) => Ordering::Equal,
            (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
            (Value::Int(a), Value::Int(b)) => a.cmp(b),
            (Value::Float(a), Value::Float(b)) => {
                a.partial_cmp(b).unwrap_or_else(|| {
                    // NaN handling: treat as equal for ordering purposes
                    a.to_bits().cmp(&b.to_bits())
                })
            }
            (Value::String(a), Value::String(b)) => a.cmp(b),
            (Value::List(a), Value::List(b)) => a.as_slice().cmp(b.as_slice()),
            (Value::Tuple(a), Value::Tuple(b)) => a.cmp(b),
            (Value::Map(a), Value::Map(b)) => {
                a.iter().cmp(b.iter())
            }
            (Value::Set(a), Value::Set(b)) => {
                a.iter().cmp(b.iter())
            }
            (Value::Record(na, fa), Value::Record(nb, fb)) => {
                na.cmp(nb).then_with(|| fa.iter().cmp(fb.iter()))
            }
            (Value::Variant(na, fa), Value::Variant(nb, fb)) => {
                na.cmp(nb).then_with(|| fa.cmp(fb))
            }
            (Value::RecordDescriptor(a), Value::RecordDescriptor(b)) => a.cmp(b),
            (Value::PrimitiveDescriptor(a), Value::PrimitiveDescriptor(b)) => a.cmp(b),
            (Value::Channel(a), Value::Channel(b)) => a.id.cmp(&b.id),
            _ => Ordering::Equal,
        }
    }
}

// ── FFI conversion traits ──────────────────────────────────────────

/// Convert a `Value` into a Rust type.
pub trait FromValue: Sized {
    fn from_value(value: &Value) -> Result<Self, String>;
}

/// Convert a Rust type into a `Value`.
pub trait IntoValue {
    fn into_value(self) -> Value;
}

impl FromValue for Value {
    fn from_value(value: &Value) -> Result<Self, String> { Ok(value.clone()) }
}

impl IntoValue for Value {
    fn into_value(self) -> Value { self }
}

impl FromValue for i64 {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Int(n) => Ok(*n),
            other => Err(format!("expected Int, got {}", value_type_name(other))),
        }
    }
}

impl IntoValue for i64 {
    fn into_value(self) -> Value { Value::Int(self) }
}

impl FromValue for f64 {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Float(n) => Ok(*n),
            Value::Int(n) => Ok(*n as f64),
            other => Err(format!("expected Float, got {}", value_type_name(other))),
        }
    }
}

impl IntoValue for f64 {
    fn into_value(self) -> Value { Value::Float(self) }
}

impl FromValue for bool {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Bool(b) => Ok(*b),
            other => Err(format!("expected Bool, got {}", value_type_name(other))),
        }
    }
}

impl IntoValue for bool {
    fn into_value(self) -> Value { Value::Bool(self) }
}

impl FromValue for String {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::String(s) => Ok(s.clone()),
            other => Err(format!("expected String, got {}", value_type_name(other))),
        }
    }
}

impl IntoValue for String {
    fn into_value(self) -> Value { Value::String(self) }
}

impl IntoValue for &str {
    fn into_value(self) -> Value { Value::String(self.to_string()) }
}

impl FromValue for () {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Unit => Ok(()),
            other => Err(format!("expected Unit, got {}", value_type_name(other))),
        }
    }
}

impl IntoValue for () {
    fn into_value(self) -> Value { Value::Unit }
}

impl FromValue for Vec<Value> {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::List(xs) => Ok(xs.as_ref().clone()),
            other => Err(format!("expected List, got {}", value_type_name(other))),
        }
    }
}

impl IntoValue for Vec<Value> {
    fn into_value(self) -> Value { Value::List(Rc::new(self)) }
}

impl<T: IntoValue> IntoValue for Option<T> {
    fn into_value(self) -> Value {
        match self {
            Some(v) => Value::Variant("Some".into(), vec![v.into_value()]),
            None => Value::Variant("None".into(), vec![]),
        }
    }
}

impl<T: IntoValue> IntoValue for Result<T, String> {
    fn into_value(self) -> Value {
        match self {
            Ok(v) => Value::Variant("Ok".into(), vec![v.into_value()]),
            Err(e) => Value::Variant("Err".into(), vec![Value::String(e)]),
        }
    }
}

fn value_type_name(v: &Value) -> &'static str {
    match v {
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
        Value::VmClosure(_) | Value::BuiltinFn(_) => "Function",
        Value::VariantConstructor(..) => "Constructor",
        Value::RecordDescriptor(_) | Value::PrimitiveDescriptor(_) => "Type",
        Value::Channel(_) => "Channel",
        Value::Handle(_) => "Handle",
        Value::Unit => "Unit",
    }
}

impl Hash for Value {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Value::Unit => {}
            Value::Bool(b) => b.hash(state),
            Value::Int(n) => n.hash(state),
            Value::Float(f) => f.to_bits().hash(state),
            Value::String(s) => s.hash(state),
            Value::List(xs) => {
                xs.len().hash(state);
                for x in xs.iter() {
                    x.hash(state);
                }
            }
            Value::Tuple(vs) => {
                vs.len().hash(state);
                for v in vs {
                    v.hash(state);
                }
            }
            Value::Map(m) => {
                m.len().hash(state);
                for (k, v) in m.iter() {
                    k.hash(state);
                    v.hash(state);
                }
            }
            Value::Set(s) => {
                s.len().hash(state);
                for v in s.iter() {
                    v.hash(state);
                }
            }
            Value::Record(name, fields) => {
                name.hash(state);
                for (k, v) in fields.iter() {
                    k.hash(state);
                    v.hash(state);
                }
            }
            Value::Variant(name, fields) => {
                name.hash(state);
                fields.len().hash(state);
                for f in fields {
                    f.hash(state);
                }
            }
            Value::Channel(ch) => ch.id.hash(state),
            Value::Handle(h) => h.id.hash(state),
            Value::VmClosure(_) => {} // not meaningfully hashable
            Value::BuiltinFn(name) => name.hash(state),
            Value::VariantConstructor(name, arity) => {
                name.hash(state);
                arity.hash(state);
            }
            Value::RecordDescriptor(name) => name.hash(state),
            Value::PrimitiveDescriptor(name) => name.hash(state),
        }
    }
}
