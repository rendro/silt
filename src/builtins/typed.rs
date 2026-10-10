//! Typed bodies of builtins.
//!
//! The body of a builtin is a Rust function of the arguments it takes,
//! as Rust types: `fn trim(s: &str) -> String`. The [`builtins!`] macro
//! writes, around each such function, the one function a call of the
//! row runs ([`TypedCall`]): it takes the arguments out of the values,
//! each as the type its parameter says ([`Arg`]), calls the body, and
//! makes the result a value ([`Ret`]).
//!
//! That is the one check of a call's arguments: how many they are, and
//! of which kinds. Arguments that do not fit are not an error a body
//! writes; the call gives `None`, and the row raises the one error
//! there is for it ([`Row::call`](super::registry::Row)): the checker
//! said that this could not happen. What a body still raises are the
//! errors a correct program can meet (an overflow, an index out of
//! bounds).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::runtime::handle::{TaskHandle, TcpListenerHandle, TcpStreamHandle};
use crate::runtime::sync::Channel;
use crate::typeinfo::{FieldType, TypeInfo, bv};
use crate::value::Value;
use crate::vm::{Step, Vm, VmError};

/// What a typed call gives: what the body did, or `None` if the
/// arguments are not the body's (their number, or the kind of one).
pub(crate) type Called = Option<Result<Step, VmError>>;

/// A call of a builtin with its arguments as values: written around a
/// typed body by [`builtins!`].
pub(crate) type TypedCall = fn(&mut Vm, &[Value]) -> Called;

/// An argument of a builtin, as its body takes it: borrowed from the
/// value, or copied out of it where it is a number.
pub(crate) trait Arg<'a>: Sized {
    /// The argument, if the value is of its kind.
    fn take(value: &'a Value) -> Option<Self>;
}

/// Any value: a parameter of a type the body does not look into (`a`).
impl<'a> Arg<'a> for &'a Value {
    fn take(value: &'a Value) -> Option<Self> {
        Some(value)
    }
}

impl<'a> Arg<'a> for i64 {
    fn take(value: &'a Value) -> Option<Self> {
        match value {
            Value::Int(n) => Some(*n),
            _ => None,
        }
    }
}

impl<'a> Arg<'a> for f64 {
    fn take(value: &'a Value) -> Option<Self> {
        match value {
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }
}

impl<'a> Arg<'a> for bool {
    fn take(value: &'a Value) -> Option<Self> {
        match value {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

impl<'a> Arg<'a> for &'a str {
    fn take(value: &'a Value) -> Option<Self> {
        match value {
            Value::String(s) => Some(s),
            _ => None,
        }
    }
}

/// A `Bytes` argument.
pub(crate) type Bytes<'a> = &'a Arc<Vec<u8>>;

/// A `Bytes`.
impl<'a> Arg<'a> for &'a Arc<Vec<u8>> {
    fn take(value: &'a Value) -> Option<Self> {
        match value {
            Value::Bytes(bytes) => Some(bytes),
            _ => None,
        }
    }
}

/// A `Channel` argument.
pub(crate) type Chan<'a> = &'a Arc<Channel>;

impl<'a> Arg<'a> for &'a Arc<Channel> {
    fn take(value: &'a Value) -> Option<Self> {
        match value {
            Value::Channel(channel) => Some(channel),
            _ => None,
        }
    }
}

/// A `Handle` argument: a task's.
pub(crate) type Handle<'a> = &'a Arc<TaskHandle>;

impl<'a> Arg<'a> for &'a Arc<TaskHandle> {
    fn take(value: &'a Value) -> Option<Self> {
        match value {
            Value::Handle(handle) => Some(handle),
            _ => None,
        }
    }
}

/// A `TcpStream` argument.
#[cfg(feature = "tcp")]
pub(crate) type TcpStream<'a> = &'a Arc<TcpStreamHandle>;

impl<'a> Arg<'a> for &'a Arc<TcpStreamHandle> {
    fn take(value: &'a Value) -> Option<Self> {
        match value {
            Value::TcpStream(stream) => Some(stream),
            _ => None,
        }
    }
}

/// A `TcpListener` argument.
#[cfg(feature = "tcp")]
pub(crate) type TcpListener<'a> = &'a Arc<TcpListenerHandle>;

impl<'a> Arg<'a> for &'a Arc<TcpListenerHandle> {
    fn take(value: &'a Value) -> Option<Self> {
        match value {
            Value::TcpListener(listener) => Some(listener),
            _ => None,
        }
    }
}

/// A `Map` argument.
pub(crate) type Map<'a> = &'a Arc<BTreeMap<Value, Value>>;

/// A `Set` argument.
pub(crate) type Set<'a> = &'a Arc<BTreeSet<Value>>;

/// The error of a body that finds, in what it was given for the
/// parameter `param`, a value that the parameter's type does not have:
/// an element of a `List(Int)` that is no `Int`, a result of a function
/// argument that is not of the type the function returns. Like
/// arguments that do not fit a row, no checked program reaches it, and
/// it has one wording.
pub(crate) fn unsound(name: &str, param: &str) -> VmError {
    VmError::type_confusion(format!(
        "{name} was given, for {param}, a value that its signature does not allow"
    ))
}

/// A `Map`.
impl<'a> Arg<'a> for &'a Arc<BTreeMap<Value, Value>> {
    fn take(value: &'a Value) -> Option<Self> {
        match value {
            Value::Map(map) => Some(map),
            _ => None,
        }
    }
}

/// A `Set`.
impl<'a> Arg<'a> for &'a Arc<BTreeSet<Value>> {
    fn take(value: &'a Value) -> Option<Self> {
        match value {
            Value::Set(set) => Some(set),
            _ => None,
        }
    }
}

/// A `List`: a list's elements, or the range of Ints that stands for
/// them. (A range is a list to the checker. The value of its own goes
/// in step V3, and this with it: a `List` is then the elements.)
#[derive(Clone, Copy)]
pub(crate) enum List<'a> {
    Items(&'a crate::value::List),
    Range(i64, i64),
}

/// The elements of a list, one after the other, a range's without
/// making a list of them.
pub(crate) enum Items<'a> {
    List(crate::value::Iter<'a>),
    Range(std::ops::RangeInclusive<i64>),
}

impl Iterator for Items<'_> {
    type Item = Value;

    fn next(&mut self) -> Option<Value> {
        match self {
            Items::List(items) => items.next().map(crate::value::Item::into_value),
            Items::Range(range) => range.next().map(Value::Int),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Items::List(items) => items.size_hint(),
            Items::Range(range) => range.size_hint(),
        }
    }
}

/// The elements of a list that outlive the call they were an argument
/// of ([`List::elements`]).
pub(crate) enum Elements {
    List(crate::value::IntoIter),
    Range(std::ops::RangeInclusive<i64>),
}

impl Iterator for Elements {
    type Item = Value;

    fn next(&mut self) -> Option<Value> {
        match self {
            Elements::List(items) => items.next(),
            Elements::Range(range) => range.next().map(Value::Int),
        }
    }
}

impl<'a> Arg<'a> for List<'a> {
    fn take(value: &'a Value) -> Option<Self> {
        match value {
            Value::List(items) => Some(List::Items(items)),
            Value::Range(lo, hi) => Some(List::Range(*lo, *hi)),
            _ => None,
        }
    }
}

impl<'a> List<'a> {
    pub(crate) fn iter(self) -> Items<'a> {
        match self {
            List::Items(items) => Items::List(items.iter()),
            List::Range(lo, hi) => Items::Range(lo..=hi),
        }
    }

    /// The elements, to be taken one after the other beyond the call
    /// (by a stream's stage): no list is made of a range, however long
    /// it is.
    pub(crate) fn elements(self) -> Elements {
        match self {
            List::Items(items) => Elements::List(items.clone().into_iter()),
            List::Range(lo, hi) => Elements::Range(lo..=hi),
        }
    }

    /// The elements, each a value of its own. A range of more elements
    /// than a list may have is an error.
    pub(crate) fn to_vec(self) -> Result<Vec<Value>, VmError> {
        match self {
            List::Items(items) => Ok(items.to_vec()),
            List::Range(lo, hi) => {
                crate::value::checked_range_len(lo, hi).map_err(VmError::new)?;
                Ok((lo..=hi).map(Value::Int).collect())
            }
        }
    }
}

/// A `type a` argument: one of the primitive types, as the type of a
/// field of it, or a type of a name.
pub(crate) enum Type<'a> {
    Primitive(FieldType),
    Named(&'a Arc<TypeInfo>),
}

impl<'a> Arg<'a> for Type<'a> {
    fn take(value: &'a Value) -> Option<Self> {
        match value {
            Value::PrimitiveDescriptor(name) => Some(Type::Primitive(match name.as_str() {
                "Int" => FieldType::Int,
                "Float" => FieldType::Float,
                "String" => FieldType::String,
                "Bool" => FieldType::Bool,
                _ => return None,
            })),
            Value::TypeDescriptor(ty) => Some(Type::Named(ty)),
            _ => None,
        }
    }
}

/// The result of a typed body, made the result of the call.
pub(crate) trait Ret {
    fn ret(self) -> Result<Step, VmError>;
}

impl Ret for Value {
    fn ret(self) -> Result<Step, VmError> {
        Ok(Step::Done(self))
    }
}

/// A body that goes on as a frame, or waits ([`Step`]).
impl Ret for Step {
    fn ret(self) -> Result<Step, VmError> {
        Ok(self)
    }
}

impl Ret for () {
    fn ret(self) -> Result<Step, VmError> {
        Ok(Step::Done(Value::Unit))
    }
}

impl Ret for i64 {
    fn ret(self) -> Result<Step, VmError> {
        Ok(Step::Done(Value::Int(self)))
    }
}

impl Ret for bool {
    fn ret(self) -> Result<Step, VmError> {
        Ok(Step::Done(Value::Bool(self)))
    }
}

/// A `Float`: finite, as every float a body computes from floats with
/// an operation that cannot leave them is; one that can goes through
/// `numeric::checked_float`. (`-0.0` becomes `0.0`: see
/// `numeric::float_value`.)
impl Ret for f64 {
    fn ret(self) -> Result<Step, VmError> {
        Ok(Step::Done(super::numeric::float_value(self)))
    }
}

impl Ret for String {
    fn ret(self) -> Result<Step, VmError> {
        Ok(Step::Done(Value::String(self)))
    }
}

/// A `List`.
impl Ret for Vec<Value> {
    fn ret(self) -> Result<Step, VmError> {
        Ok(Step::Done(Value::list(self)))
    }
}

/// A `Bytes`.
impl Ret for Vec<u8> {
    fn ret(self) -> Result<Step, VmError> {
        Ok(Step::Done(Value::Bytes(Arc::new(self))))
    }
}

/// A `Map`.
impl Ret for BTreeMap<Value, Value> {
    fn ret(self) -> Result<Step, VmError> {
        Ok(Step::Done(Value::Map(Arc::new(self))))
    }
}

/// A `Set`.
impl Ret for BTreeSet<Value> {
    fn ret(self) -> Result<Step, VmError> {
        Ok(Step::Done(Value::Set(Arc::new(self))))
    }
}

/// An `Option`: `Some(value)` or `None`.
impl Ret for Option<Value> {
    fn ret(self) -> Result<Step, VmError> {
        Ok(Step::Done(match self {
            Some(value) => Value::variant(bv::SOME, vec![value]),
            None => Value::variant(bv::NONE, vec![]),
        }))
    }
}

/// A body that can fail with a runtime error.
impl<T: Ret> Ret for Result<T, VmError> {
    fn ret(self) -> Result<Step, VmError> {
        self?.ret()
    }
}

/// Typed bodies, each `fn name(args) -> result { ... }`, made the
/// functions that rows run ([`TypedCall`]) under the same names.
///
/// The parameters are of types that are [`Arg`]s, at most six; the
/// result is a [`Ret`] (`()` if none is written). A body that needs
/// the VM names it first, without a type: `fn random(vm) -> f64`.
macro_rules! builtins {
    ($($(#[$meta:meta])* fn $name:ident ( $($params:tt)* ) $(-> $ret:ty)? $body:block)*) => {
        $(
            $crate::builtins::typed::builtins!(
                @one [$(#[$meta])*] $name ( $($params)* ) [$($ret)?] $body
            );
        )*
    };
    (@one [$($meta:tt)*] $name:ident ( $vm:ident $(, $arg:ident : $ty:ty)* $(,)? ) [$($ret:ty)?] $body:block) => {
        $($meta)*
        pub(crate) fn $name(
            vm: &mut $crate::vm::Vm,
            args: &[$crate::value::Value],
        ) -> $crate::builtins::typed::Called {
            #[allow(clippy::too_many_arguments)]
            fn typed($vm: &mut $crate::vm::Vm, $($arg: $ty),*) $(-> $ret)? $body
            let [$($arg),*] = args else {
                return None;
            };
            $(let $arg = <$ty as $crate::builtins::typed::Arg>::take($arg)?;)*
            Some($crate::builtins::typed::Ret::ret(typed(vm, $($arg),*)))
        }
    };
    (@one [$($meta:tt)*] $name:ident ( $($arg:ident : $ty:ty),* $(,)? ) [$($ret:ty)?] $body:block) => {
        $($meta)*
        pub(crate) fn $name(
            _vm: &mut $crate::vm::Vm,
            args: &[$crate::value::Value],
        ) -> $crate::builtins::typed::Called {
            fn typed($($arg: $ty),*) $(-> $ret)? $body
            let [$($arg),*] = args else {
                return None;
            };
            $(let $arg = <$ty as $crate::builtins::typed::Arg>::take($arg)?;)*
            Some($crate::builtins::typed::Ret::ret(typed($($arg),*)))
        }
    };
}
pub(crate) use builtins;
