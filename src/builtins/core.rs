//! Core builtin functions (`result.*`, `option.*`, `test.*`).

use super::typed::{Arg, builtins, unsound};
use crate::typeinfo::{BuiltinVariant, bv};
use crate::value::{Float, Value};
use crate::vm::{Step, VmError, call_then};

/// The one field of `value`, if it is the variant `tag` with one.
fn field(value: &Value, tag: BuiltinVariant) -> Option<&Value> {
    match value {
        Value::Variant(variant) if variant.is(tag) => match variant.fields() {
            [field] => Some(field),
            _ => None,
        },
        _ => None,
    }
}

/// A `Result` argument: the value, and what is in it.
#[derive(Clone, Copy)]
pub(crate) struct Res<'a> {
    pub(crate) whole: &'a Value,
    pub(crate) inner: Result<&'a Value, &'a Value>,
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
pub(crate) struct Opt<'a> {
    pub(crate) whole: &'a Value,
    pub(crate) inner: Option<&'a Value>,
}

impl<'a> Arg<'a> for Opt<'a> {
    fn take(whole: &'a Value) -> Option<Self> {
        let inner = match whole {
            Value::Variant(variant) if variant.is(bv::NONE) && variant.fields().is_empty() => None,
            _ => Some(field(whole, bv::SOME)?),
        };
        Some(Opt { whole, inner })
    }
}

/// `result.*`
pub(crate) mod result {
    use super::*;

    builtins! {
        fn map(r: Res, f: &Value) -> Step {
            match r.inner {
                Ok(value) => call_then("result.map", f.clone(), [value.clone()], |new| {
                    Ok(Value::variant(bv::OK, [new]))
                }),
                Err(_) => Step::Done(r.whole.clone()),
            }
        }

        fn map_err(r: Res, f: &Value) -> Step {
            match r.inner {
                Ok(_) => Step::Done(r.whole.clone()),
                Err(error) => call_then("result.map_err", f.clone(), [error.clone()], |new| {
                    Ok(Value::variant(bv::ERR, [new]))
                }),
            }
        }

        fn flat_map(r: Res, f: &Value) -> Step {
            match r.inner {
                Ok(value) => call_then("result.flat_map", f.clone(), [value.clone()], Ok),
                Err(_) => Step::Done(r.whole.clone()),
            }
        }

        fn flatten(r: Res) -> Result<Value, VmError> {
            match r.inner {
                Ok(inner) => match Res::take(inner) {
                    Some(inner) => Ok(inner.whole.clone()),
                    None => Err(unsound("result.flatten", "r")),
                },
                Err(_) => Ok(r.whole.clone()),
            }
        }

        fn unwrap_or(r: Res, default: &Value) -> Value {
            r.inner.unwrap_or(default).clone()
        }

        fn to_option(r: Res) -> Value {
            match r.inner {
                Ok(value) => Value::variant(bv::SOME, [value.clone()]),
                Err(_) => Value::variant(bv::NONE, []),
            }
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
                Some(value) => call_then("option.map", f.clone(), [value.clone()], |new| {
                    Ok(Value::variant(bv::SOME, [new]))
                }),
                None => Step::Done(opt.whole.clone()),
            }
        }

        fn flat_map(opt: Opt, f: &Value) -> Step {
            match opt.inner {
                Some(value) => call_then("option.flat_map", f.clone(), [value.clone()], Ok),
                None => Step::Done(opt.whole.clone()),
            }
        }

        fn flatten(opt: Opt) -> Result<Value, VmError> {
            match opt.inner {
                Some(inner) => match Opt::take(inner) {
                    Some(_) => Ok(inner.clone()),
                    None => Err(unsound("option.flatten", "opt")),
                },
                None => Ok(opt.whole.clone()),
            }
        }

        fn to_result(opt: Opt, error: &Value) -> Value {
            match opt.inner {
                Some(value) => Value::variant(bv::OK, [value.clone()]),
                None => Value::variant(bv::ERR, [error.clone()]),
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
/// the typed form has no way to say; so they are written as the
/// calls the macro would write. (The conventions step gives each one
/// form.)
pub(crate) mod test {
    use super::*;

    /// The failure of an assertion: what failed. (Values are shown as
    /// silt source writes them.)
    fn failed(what: String) -> VmError {
        VmError::new(format!("assertion failed: {what}"))
    }

    /// How many characters of each of two values that differ an
    /// assertion shows, at most, around where they first differ.
    const AROUND: usize = 40;

    /// `left` and `right`, as silt source writes them, between `between`
    /// (`!=`): whole when they are short; else each from a little before
    /// the first character at which they differ to a little after it,
    /// with `...` where a part is left out, and where that is. (Two texts
    /// of 3,000 characters were written whole, on one line.)
    fn two_values(left: &Value, right: &Value, between: &str) -> String {
        let (left, right) = (left.format_silt(), right.format_silt());
        let (l, r): (Vec<char>, Vec<char>) = (left.chars().collect(), right.chars().collect());
        if l.len() + r.len() <= 4 * AROUND {
            return format!("{left} {between} {right}");
        }
        let first = l.iter().zip(&r).take_while(|(a, b)| a == b).count();
        let same = l == r;
        let from = match same {
            true => 0,
            false => first.saturating_sub(AROUND / 2),
        };
        let part = |text: &[char]| {
            let to = text.len().min(from + AROUND);
            let mut shown = String::new();
            if from > 0 {
                shown.push_str("...");
            }
            shown.extend(text.get(from..to).unwrap_or_default());
            if to < text.len() {
                shown.push_str("...");
            }
            shown
        };
        let at = match first < l.len().min(r.len()) {
            true => format!(
                "they first differ at character {} of how they are written",
                first + 1
            ),
            false => format!("one is the other's first {first} characters"),
        };
        match same {
            true => format!("{} {between} {} (the same long value)", part(&l), part(&r)),
            false => format!("{} {between} {} ({at})", part(&l), part(&r)),
        }
    }

    builtins! {
        fn assert(condition: bool) -> Result<(), VmError> {
            match condition {
                true => Ok(()),
                false => Err(failed("the condition is false".to_string())),
            }
        }

        fn assert_eq(left: &Value, right: &Value) -> Result<(), VmError> {
            match left == right {
                true => Ok(()),
                // (Two values that are written alike and differ are URLs,
                // or hold URLs, that differ where they are hidden.)
                false if (left.holds_url() || right.holds_url())
                    && left.format_silt() == right.format_silt() =>
                {
                    Err(failed(format!(
                        "{} (the two differ in their hidden user or password)",
                        two_values(left, right, "!=")
                    )))
                }
                false => Err(failed(two_values(left, right, "!="))),
            }
        }

        // Absolute: `a` and `b` may differ by `tolerance` at most.
        fn assert_approx(a: Float, b: Float, tolerance: Float) -> Result<(), VmError> {
            if tolerance.get() < 0.0 {
                return Err(VmError::new(format!(
                    "test.assert_approx: negative tolerance {tolerance}"
                )));
            }
            // Two far ends of the range differ by more than a Float holds.
            let difference = Float::new((a.get() - b.get()).abs());
            match difference {
                Some(d) if d <= tolerance => Ok(()),
                Some(d) => Err(failed(format!(
                    "{a} and {b} differ by {d}, more than {tolerance}"
                ))),
                None => Err(failed(format!(
                    "{a} and {b} differ by more than {tolerance}"
                ))),
            }
        }

        fn assert_ne(left: &Value, right: &Value) -> Result<(), VmError> {
            match left != right {
                true => Ok(()),
                false => Err(failed(two_values(left, right, "=="))),
            }
        }
    }
}
