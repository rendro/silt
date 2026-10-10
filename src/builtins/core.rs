//! Core builtin functions (`result.*`, `option.*`, `test.*`).

use super::typed::{Arg, Called, builtins, unsound};
use crate::typeinfo::{BuiltinVariant, bv};
use crate::value::Value;
use crate::vm::{Step, Vm, VmError, call_then};

/// The one field of `value`, if it is the variant `tag` with one.
fn field(value: &Value, tag: BuiltinVariant) -> Option<&Value> {
    match value {
        Value::Variant(name, fields) if name.is(tag) => match fields.as_slice() {
            [field] => Some(field),
            _ => None,
        },
        _ => None,
    }
}

/// A `Result` argument: the value, and what is in it.
#[derive(Clone, Copy)]
struct Res<'a> {
    whole: &'a Value,
    inner: Result<&'a Value, &'a Value>,
}

impl<'a> Arg<'a> for Res<'a> {
    fn take(whole: &'a Value) -> Option<Self> {
        let inner = match field(whole, bv::OK) {
            Some(value) => Ok(value),
            None => Err(field(whole, bv::ERR)?),
        };
        Some(Res { whole, inner })
    }
}

/// An `Option` argument: the value, and what is in it.
#[derive(Clone, Copy)]
struct Opt<'a> {
    whole: &'a Value,
    inner: Option<&'a Value>,
}

impl<'a> Arg<'a> for Opt<'a> {
    fn take(whole: &'a Value) -> Option<Self> {
        let inner = match whole {
            Value::Variant(name, fields) if name.is(bv::NONE) && fields.is_empty() => None,
            _ => Some(field(whole, bv::SOME)?),
        };
        Some(Opt { whole, inner })
    }
}

/// `result.*`
pub(crate) mod result {
    use super::*;

    builtins! {
        fn map_ok(r: Res, f: &Value) -> Step {
            match r.inner {
                Ok(value) => call_then("result.map_ok", f.clone(), value.clone(), |new| {
                    Ok(Value::variant(bv::OK, vec![new]))
                }),
                Err(_) => Step::Done(r.whole.clone()),
            }
        }

        fn map_err(r: Res, f: &Value) -> Step {
            match r.inner {
                Ok(_) => Step::Done(r.whole.clone()),
                Err(error) => call_then("result.map_err", f.clone(), error.clone(), |new| {
                    Ok(Value::variant(bv::ERR, vec![new]))
                }),
            }
        }

        fn flat_map(r: Res, f: &Value) -> Step {
            match r.inner {
                Ok(value) => call_then("result.flat_map", f.clone(), value.clone(), Ok),
                Err(_) => Step::Done(r.whole.clone()),
            }
        }

        fn flatten(r: Res) -> Result<Value, VmError> {
            match r.inner {
                Ok(inner) => match Res::take(inner) {
                    Some(inner) => Ok(inner.whole.clone()),
                    None => Err(unsound("result.flatten")),
                },
                Err(_) => Ok(r.whole.clone()),
            }
        }

        fn unwrap_or(r: Res, default: &Value) -> Value {
            r.inner.unwrap_or(default).clone()
        }

        fn is_ok(r: Res) -> bool {
            r.inner.is_ok()
        }

        fn is_err(r: Res) -> bool {
            r.inner.is_err()
        }
    }
}

/// `option.*`
pub(crate) mod option {
    use super::*;

    builtins! {
        fn map(opt: Opt, f: &Value) -> Step {
            match opt.inner {
                Some(value) => call_then("option.map", f.clone(), value.clone(), |new| {
                    Ok(Value::variant(bv::SOME, vec![new]))
                }),
                None => Step::Done(opt.whole.clone()),
            }
        }

        fn flat_map(opt: Opt, f: &Value) -> Step {
            match opt.inner {
                Some(value) => call_then("option.flat_map", f.clone(), value.clone(), Ok),
                None => Step::Done(opt.whole.clone()),
            }
        }

        fn to_result(opt: Opt, error: &Value) -> Value {
            match opt.inner {
                Some(value) => Value::variant(bv::OK, vec![value.clone()]),
                None => Value::variant(bv::ERR, vec![error.clone()]),
            }
        }

        fn unwrap_or(opt: Opt, default: &Value) -> Value {
            opt.inner.unwrap_or(default).clone()
        }

        fn is_some(opt: Opt) -> bool {
            opt.inner.is_some()
        }

        fn is_none(opt: Opt) -> bool {
            opt.inner.is_none()
        }
    }
}

/// `test.*`: each takes a last argument or not, the message, which
/// the typed form has no way to say; so the three are written as the
/// calls the macro would write. (The conventions step gives each one
/// form.)
pub(crate) mod test {
    use super::*;

    /// The arguments without the message, and the message if there is
    /// one: `None` if they are not `N` or `N + 1`, or the message is no
    /// `String`.
    fn with_message<const N: usize>(args: &[Value]) -> Option<(&[Value; N], Option<&str>)> {
        let (args, message) = args.split_first_chunk::<N>()?;
        match message {
            [] => Some((args, None)),
            [message] => Some((args, Some(<&str>::take(message)?))),
            _ => None,
        }
    }

    /// The failure of an assertion: what failed, after the message if
    /// there is one. (Values are shown as silt source writes them.)
    fn failed(message: Option<&str>, what: String) -> Called {
        Some(Err(VmError::new(match message {
            Some(message) => format!("assertion failed: {message}: {what}"),
            None => format!("assertion failed: {what}"),
        })))
    }

    const PASSED: Called = Some(Ok(Step::Done(Value::Unit)));

    pub(crate) fn assert(_vm: &mut Vm, args: &[Value]) -> Called {
        let ([condition], message) = with_message(args)?;
        match bool::take(condition)? {
            true => PASSED,
            // (The message stands for the condition: it is `false`.)
            false => failed(None, message.map_or("false", |message| message).to_string()),
        }
    }

    pub(crate) fn assert_eq(_vm: &mut Vm, args: &[Value]) -> Called {
        let ([left, right], message) = with_message(args)?;
        match left == right {
            true => PASSED,
            false => failed(
                message,
                format!("{} != {}", left.format_silt(), right.format_silt()),
            ),
        }
    }

    pub(crate) fn assert_ne(_vm: &mut Vm, args: &[Value]) -> Called {
        let ([left, right], message) = with_message(args)?;
        match left != right {
            true => PASSED,
            false => failed(
                message,
                format!("{} == {}", left.format_silt(), right.format_silt()),
            ),
        }
    }
}
