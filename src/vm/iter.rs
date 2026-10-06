//! The frames of the builtins that call a function: one call
//! ([`call_then`]) or a call for each item of a collection
//! ([`iterate`]).

use std::ops::ControlFlow;

use crate::value::Value;

use super::runtime::{Native, Step};
use super::{Vm, VmError};

/// What an iteration does after a call: go on to the next item, or
/// stop with a value, which is the builtin's.
pub(crate) type Flow = Result<ControlFlow<Value>, VmError>;

/// Go on to the next item.
pub(crate) fn next() -> Flow {
    Ok(ControlFlow::Continue(()))
}

/// Stop the iteration with `value`.
pub(crate) fn stop(value: Value) -> Flow {
    Ok(ControlFlow::Break(value))
}

/// A builtin that calls `callback` for each of `items` in turn, with a
/// state `S` of its own (`list.map`: the results so far).
struct Iterate<S> {
    name: &'static str,
    items: std::vec::IntoIter<Value>,
    /// The item the call that is running was made for.
    current: Option<Value>,
    callback: Value,
    state: S,
    args: fn(&S, &Value) -> Vec<Value>,
    step: fn(&mut S, Value, Value) -> Flow,
    finish: fn(&mut S) -> Result<Value, VmError>,
}

impl<S: Send> Native for Iterate<S> {
    fn name(&self) -> &str {
        self.name
    }

    fn resume(&mut self, _vm: &mut Vm, input: Value) -> Result<Step, VmError> {
        if let Some(item) = self.current.take()
            && let ControlFlow::Break(value) = (self.step)(&mut self.state, item, input)?
        {
            return Ok(Step::Done(value));
        }
        match self.items.next() {
            Some(item) => {
                let args = (self.args)(&self.state, &item);
                self.current = Some(item);
                Ok(Step::Call {
                    callee: self.callback.clone(),
                    args,
                })
            }
            None => (self.finish)(&mut self.state).map(Step::Done),
        }
    }
}

/// The builtin `name` as an iteration over `items`: for each item,
/// `callback` is called with `args(state, item)`, and `step(state,
/// item, result)` takes the value it returned; after the last item, or
/// with no items, the builtin's value is `finish(state)`.
pub(crate) fn iterate<S: Send + 'static>(
    name: &'static str,
    items: Vec<Value>,
    callback: Value,
    state: S,
    args: fn(&S, &Value) -> Vec<Value>,
    step: fn(&mut S, Value, Value) -> Flow,
    finish: fn(&mut S) -> Result<Value, VmError>,
) -> Step {
    Step::Run(Box::new(Iterate {
        name,
        items: items.into_iter(),
        current: None,
        callback,
        state,
        args,
        step,
        finish,
    }))
}

/// The arguments of a function that takes the item.
pub(crate) fn item_arg<S>(_: &S, item: &Value) -> Vec<Value> {
    vec![item.clone()]
}

/// A builtin that makes one call and makes its value from the result.
struct CallThen<F> {
    name: &'static str,
    call: Option<(Value, Vec<Value>)>,
    then: Option<F>,
}

impl<F> Native for CallThen<F>
where
    F: FnOnce(Value) -> Result<Value, VmError> + Send,
{
    fn name(&self) -> &str {
        self.name
    }

    fn resume(&mut self, _vm: &mut Vm, input: Value) -> Result<Step, VmError> {
        if let Some((callee, args)) = self.call.take() {
            return Ok(Step::Call { callee, args });
        }
        let then = self.then.take().expect("a frame is resumed once per call");
        then(input).map(Step::Done)
    }
}

/// The builtin `name` as one call of `callee` with `args`; its value
/// is `then(result)`.
pub(crate) fn call_then<F>(name: &'static str, callee: Value, args: Vec<Value>, then: F) -> Step
where
    F: FnOnce(Value) -> Result<Value, VmError> + Send + 'static,
{
    Step::Run(Box::new(CallThen {
        name,
        call: Some((callee, args)),
        then: Some(then),
    }))
}
