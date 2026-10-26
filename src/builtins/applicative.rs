//! The built-in impls of Applicative, and the builtins that are passed
//! an Applicative's dictionary.
//!
//! An impl of the built-in traits for List, Option and Result is the
//! module functions where there are some (`map` is `list.map`,
//! `flat_map` is `option.flat_map`). `pure` and `map2` are no module's
//! functions: they are rows of the type (`List.map2`, see
//! `Registry::impl_row`), which the impl's dictionary holds.
//!
//! `list.traverse` and `list.sequence` are bounded by Applicative, so
//! they are given the dictionary of its impl for their result's
//! constructor before their arguments, and call its methods with
//! [`Vm::call_dict_method`].

use super::collections::{Kind, fits};
use super::core::{Opt, Res};
use super::typed::{List, builtins};
use crate::bytecode::dict::Dict;
use crate::typeinfo::bv;
use crate::value::{IntoIter, Value};
use crate::vm::{Native, Step, Vm, VmError, call_then};

/// The built-in impls of the traits of type constructors (HK13), each
/// with its methods in the order the trait declares them, and the row
/// that is each: a module function (`list.map`), or a row of the type
/// (`List.pure`, [`Registry::impl_row`](super::registry::Registry::impl_row)).
/// Channel is a Functor only (`stream.map`).
pub(crate) const IMPLS: &[(&str, &str, &[&str])] = &[
    ("Functor", "List", &["list.map"]),
    ("Functor", "Option", &["option.map"]),
    ("Functor", "Result", &["result.map"]),
    ("Functor", "Channel", &["stream.map"]),
    ("Applicative", "List", &["List.pure", "List.map2"]),
    ("Applicative", "Option", &["Option.pure", "Option.map2"]),
    ("Applicative", "Result", &["Result.pure", "Result.map2"]),
    ("Monad", "List", &["list.flat_map"]),
    ("Monad", "Option", &["option.flat_map"]),
    ("Monad", "Result", &["result.flat_map"]),
];

/// The row a method of a built-in impl is ([`IMPLS`]): `list.map`, or
/// `List.pure`.
pub(crate) fn method_row(qualified: &str) -> Option<&'static super::registry::Row> {
    let registry = super::registry::registry();
    registry.named(qualified).or_else(|| {
        let (ty, method) = qualified.split_once('.')?;
        registry.impl_row(ty, method)
    })
}

/// The place of `pure` among Applicative's methods, in the order the
/// trait declares them: a dictionary of an impl holds them so.
pub(crate) const PURE: usize = 0;

/// The place of `map2` among Applicative's methods.
pub(crate) const MAP2: usize = 1;

/// `List.*`: the methods of List's Applicative impl.
pub(crate) mod list {
    use super::*;

    builtins! {
        fn pure(x: &Value) -> Vec<Value> {
            vec![x.clone()]
        }

        // Every pair: for each element of `xs`, each of `ys`.
        fn map2(xs: List, ys: List, f: &Value) -> Result<Step, VmError> {
            xs.writable()?;
            ys.writable()?;
            fits("List.map2", Kind::List, xs.len().saturating_mul(ys.len()))?;
            Ok(Step::Run(Box::new(EveryPair {
                xs: xs.clone().into_iter(),
                x: None,
                ys: ys.clone(),
                rest: crate::value::List::from(Vec::new()).into_iter(),
                f: f.clone(),
                out: Vec::new(),
                calling: false,
            })))
        }
    }
}

/// `Option.*`: the methods of Option's Applicative impl.
pub(crate) mod option {
    use super::*;

    builtins! {
        fn pure(x: &Value) -> Value {
            Value::variant(bv::SOME, [x.clone()])
        }

        // The first `None`, the left one first.
        fn map2(xs: Opt, ys: Opt, f: &Value) -> Step {
            match (xs.inner, ys.inner) {
                (Some(x), Some(y)) => {
                    call_then("Option.map2", f.clone(), [x.clone(), y.clone()], |new| {
                        Ok(Value::variant(bv::SOME, [new]))
                    })
                }
                (None, _) => Step::Done(xs.whole.clone()),
                (Some(_), None) => Step::Done(ys.whole.clone()),
            }
        }
    }
}

/// `Result.*`: the methods of Result's Applicative impl.
pub(crate) mod result {
    use super::*;

    builtins! {
        fn pure(x: &Value) -> Value {
            Value::variant(bv::OK, [x.clone()])
        }

        // The first `Err`, the left one first.
        fn map2(xs: Res, ys: Res, f: &Value) -> Step {
            match (xs.inner, ys.inner) {
                (Ok(x), Ok(y)) => {
                    call_then("Result.map2", f.clone(), [x.clone(), y.clone()], |new| {
                        Ok(Value::variant(bv::OK, [new]))
                    })
                }
                (Err(_), _) => Step::Done(xs.whole.clone()),
                (Ok(_), Err(_)) => Step::Done(ys.whole.clone()),
            }
        }
    }
}

/// `List.map2`'s frame: a call of `f` for each pair, the elements of
/// `xs` in turn with each of `ys`.
struct EveryPair {
    /// The elements of `xs` after `x`.
    xs: IntoIter,
    /// The element of `xs` whose pairs are being made.
    x: Option<Value>,
    ys: crate::value::List,
    /// The elements of `ys` not paired with `x` yet.
    rest: IntoIter,
    f: Value,
    out: Vec<Value>,
    /// Whether a call is running, whose result is the next input.
    calling: bool,
}

impl Native for EveryPair {
    fn name(&self) -> &str {
        "List.map2"
    }

    fn resume(&mut self, vm: &mut Vm, input: Value) -> Result<Step, VmError> {
        if std::mem::take(&mut self.calling) {
            self.out.push(input);
        }
        loop {
            if let Some(x) = &self.x
                && let Some(y) = self.rest.next()
            {
                self.calling = true;
                return Ok(vm.call(self.f.clone(), [x.clone(), y]));
            }
            let Some(x) = self.xs.next() else {
                return Ok(Step::Done(Value::list(std::mem::take(&mut self.out))));
            };
            self.x = Some(x);
            self.rest = self.ys.clone().into_iter();
        }
    }
}

/// `list.traverse` (`f` of each element) and `list.sequence` (no `f`:
/// the elements are the values) over the Applicative whose dictionary
/// is `dict`: the frame that calls `f` on every element, left to right,
/// then makes the list in the Applicative, starting with `pure([])`
/// and adding each value with `map2(so_far, value, list.append)`, left
/// to right. So the first failure is the result, after every call of
/// `f`: `map2` is given both its values, whatever they are.
pub(crate) fn traverse(
    name: &'static str,
    dict: &Dict,
    xs: crate::value::List,
    f: Option<Value>,
) -> Step {
    let values = match f {
        Some(_) => Vec::new(),
        None => xs.clone().into_iter().collect(),
    };
    Step::Run(Box::new(Traverse {
        name,
        dict: dict.clone(),
        f,
        items: xs.into_iter(),
        values,
        stage: Stage::Apply { calling: false },
        append: super::registry::registry()
            .row("list", "append")
            .map_or(Value::Unit, super::registry::Row::value),
    }))
}

/// The frame of [`traverse`].
struct Traverse {
    name: &'static str,
    dict: Dict,
    f: Option<Value>,
    /// The elements no call of `f` was made for yet.
    items: IntoIter,
    /// The value of each element so far: what `f` made of it.
    values: Vec<Value>,
    stage: Stage,
    /// `list.append`, the function `map2` is given.
    append: Value,
}

/// Where a [`Traverse`] is.
enum Stage {
    /// Calling `f` on the elements; `calling`: a call is running, whose
    /// result is the next input.
    Apply { calling: bool },
    /// Adding the values: the input is the list in the Applicative so
    /// far, and these are the values still to add.
    Combine(std::vec::IntoIter<Value>),
}

impl Traverse {
    /// The calls of `f` are done: call `pure([])`, the list so far.
    fn start_combining(&mut self, vm: &mut Vm) -> Result<Step, VmError> {
        let values = std::mem::take(&mut self.values);
        self.stage = Stage::Combine(values.into_iter());
        vm.call_dict_method(&self.dict, PURE, [Value::list(Vec::new())])
    }
}

impl Native for Traverse {
    fn name(&self) -> &str {
        self.name
    }

    fn resume(&mut self, vm: &mut Vm, input: Value) -> Result<Step, VmError> {
        match &mut self.stage {
            Stage::Apply { calling } => {
                if std::mem::take(calling) {
                    self.values.push(input);
                }
                if let (Some(f), Some(item)) = (&self.f, self.items.next()) {
                    *calling = true;
                    return Ok(vm.call(f.clone(), [item]));
                }
                self.start_combining(vm)
            }
            Stage::Combine(rest) => match rest.next() {
                Some(value) => {
                    let args = [input, value, self.append.clone()];
                    vm.call_dict_method(&self.dict, MAP2, args)
                }
                None => Ok(Step::Done(input)),
            },
        }
    }
}
