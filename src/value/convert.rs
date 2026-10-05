use std::fmt;
use std::sync::Arc;

use super::{Value, checked_range_len};
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
}

/// The shape of the values of a type a host function's signature
/// names, as far as a value shows it: a type variable, or a type whose
/// values are not told apart here, admits anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostShape {
    Any,
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
}

impl HostShape {
    /// Whether `value` is a value of the shape.
    pub fn admits(&self, value: &Value) -> bool {
        match (self, value) {
            (HostShape::Any, _)
            | (HostShape::Int, Value::Int(_))
            | (HostShape::Float, Value::Float(_))
            | (HostShape::Bool, Value::Bool(_))
            | (HostShape::String, Value::String(_))
            | (HostShape::Bytes, Value::Bytes(_))
            | (HostShape::Unit, Value::Unit) => true,
            (HostShape::List(item), Value::List(items)) => items.iter().all(|v| item.admits(v)),
            (HostShape::List(item), Value::Range(..)) => item.admits(&Value::Int(0)),
            (HostShape::Set(item), Value::Set(items)) => items.iter().all(|v| item.admits(v)),
            (HostShape::Map(k, v), Value::Map(entries)) => entries
                .iter()
                .all(|(key, value)| k.admits(key) && v.admits(value)),
            (HostShape::Option(item), Value::Variant(tag, payload)) => match payload.as_slice() {
                [v] if tag.is(bv::SOME) => item.admits(v),
                [] => tag.is(bv::NONE),
                _ => false,
            },
            (HostShape::Result(ok, err), Value::Variant(tag, payload)) => {
                match payload.as_slice() {
                    [v] if tag.is(bv::OK) => ok.admits(v),
                    [v] if tag.is(bv::ERR) => err.admits(v),
                    _ => false,
                }
            }
            (HostShape::Tuple(items), Value::Tuple(values)) => {
                items.len() == values.len()
                    && items.iter().zip(values).all(|(item, v)| item.admits(v))
            }
            _ => false,
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
            other => Err(format!("expected Int, got {}", value_type_name(other))),
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
            Value::Float(n) => Ok(*n),
            Value::Int(n) => Ok(*n as f64),
            other => Err(format!("expected Float, got {}", value_type_name(other))),
        }
    }
}

impl IntoValue for f64 {
    fn into_value(self) -> Result<Value, String> {
        // A silt `Float` is always finite and never `-0.0`: a NaN or
        // infinite result has no silt value, so it is an error rather
        // than a `Float` that every comparison, hash and container path
        // would mishandle.
        if !self.is_finite() {
            return Err(format!("non-finite float result: {self}"));
        }
        Ok(Value::Float(if self == 0.0 { 0.0 } else { self }))
    }
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
    fn into_value(self) -> Result<Value, String> {
        Ok(Value::Bool(self))
    }
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
    fn into_value(self) -> Result<Value, String> {
        Ok(Value::String(self))
    }
}

impl IntoValue for &str {
    fn into_value(self) -> Result<Value, String> {
        Ok(Value::String(self.to_string()))
    }
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
    fn into_value(self) -> Result<Value, String> {
        Ok(Value::Unit)
    }
}

impl FromValue for Vec<Value> {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::List(xs) => Ok(xs.as_ref().clone()),
            Value::Range(lo, hi) => {
                checked_range_len(*lo, *hi)?;
                Ok((*lo..=*hi).map(Value::Int).collect())
            }
            other => Err(format!("expected List, got {}", value_type_name(other))),
        }
    }
}

impl IntoValue for Vec<Value> {
    fn into_value(self) -> Result<Value, String> {
        Ok(Value::List(Arc::new(self)))
    }
}

impl<T: IntoValue> IntoValue for Option<T> {
    fn into_value(self) -> Result<Value, String> {
        Ok(match self {
            Some(v) => Value::variant(bv::SOME, vec![v.into_value()?]),
            None => Value::variant(bv::NONE, vec![]),
        })
    }
}

impl<T: IntoValue> IntoValue for Result<T, String> {
    fn into_value(self) -> Result<Value, String> {
        Ok(match self {
            Ok(v) => Value::variant(bv::OK, vec![v.into_value()?]),
            Err(e) => Value::variant(bv::ERR, vec![Value::String(e)]),
        })
    }
}

/// Surface kind name used by the `FromValue` impls' diagnostic shape
/// (`"expected <Kind>, got <kind>"`). Round 76 ERR-1 GAP collapse: this
/// previously hand-rolled its own match arms that drifted from the two
/// canonical kind oracles (`builtins::common::value_kind` and
/// `vm::Vm::type_name`) on four variants — `BuiltinFn` ("Fn" vs
/// "BuiltinFn"), `VariantConstructor` ("Constructor" vs
/// "VariantConstructor"), `TypeDescriptor` ("Type" vs "TypeDescriptor"),
/// `PrimitiveDescriptor` ("Type" vs "PrimitiveDescriptor"). Round 75
/// ERR-1 GAP unified `value_kind` with `Vm::type_name` for all 22
/// variants; this helper now delegates to that canonical source so a
/// single edit to one match arm propagates to every conversion error message.
/// Per the project's "one way to do things" convention.
fn value_type_name(v: &Value) -> &'static str {
    crate::builtins::value_kind(v)
}
