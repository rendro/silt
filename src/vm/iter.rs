//! The frames of the builtins that call a function: one call
//! ([`call_then`]) or a call for each item of a collection
//! ([`iterate`]).

use std::ops::ControlFlow;

use crate::value::{IntoIter, List, Value};

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

/// What puts the arguments of the call for an item on the stack.
pub(crate) type Args<S> = fn(&S, &Value, &mut Vec<Value>);

/// A builtin that calls `callback` for each of `items` in turn, with a
/// state `S` of its own (`list.map`: the results so far).
struct Iterate<S> {
    name: &'static str,
    /// The items no call was made for yet. They are taken one by one
    /// from the list as it is: no item is copied before its call, so a
    /// builtin that stops at the first item has read one.
    items: IntoIter,
    /// The item of the call that is running.
    current: Option<Value>,
    callback: Value,
    state: S,
    args: Args<S>,
    step: fn(&mut S, Value, Value) -> Flow,
    finish: fn(&mut S) -> Result<Value, VmError>,
}

impl<S: Send> Native for Iterate<S> {
    fn name(&self) -> &str {
        self.name
    }

    fn resume(&mut self, vm: &mut Vm, input: Value) -> Result<Step, VmError> {
        if let Some(item) = self.current.take()
            && let ControlFlow::Break(value) = (self.step)(&mut self.state, item, input)?
        {
            return Ok(Step::Done(value));
        }
        let Some(item) = self.items.next() else {
            return (self.finish)(&mut self.state).map(Step::Done);
        };
        let call = vm.call_step(self.callback.clone(), |stack| {
            (self.args)(&self.state, &item, stack)
        });
        self.current = Some(item);
        Ok(call)
    }
}

/// The builtin `name` as an iteration over `items`: for each item,
/// `callback` is called with the arguments `args(state, item, stack)`
/// pushes, and `step(state, item, result)` takes the value it returned; after the last item, or
/// with no items, the builtin's value is `finish(state)`.
pub(crate) fn iterate<S: Send + 'static>(
    name: &'static str,
    items: impl Into<List>,
    callback: Value,
    state: S,
    args: Args<S>,
    step: fn(&mut S, Value, Value) -> Flow,
    finish: fn(&mut S) -> Result<Value, VmError>,
) -> Step {
    Step::Run(Box::new(Iterate {
        name,
        items: items.into().into_iter(),
        current: None,
        callback,
        state,
        args,
        step,
        finish,
    }))
}

/// The arguments of a function that takes the item.
pub(crate) fn item_arg<S>(_: &S, item: &Value, stack: &mut Vec<Value>) {
    stack.push(item.clone());
}

/// A builtin that makes one call and makes its value from the result.
struct CallThen<F> {
    name: &'static str,
    call: Option<(Value, Value)>,
    then: Option<F>,
}

impl<F> Native for CallThen<F>
where
    F: FnOnce(Value) -> Result<Value, VmError> + Send,
{
    fn name(&self) -> &str {
        self.name
    }

    fn resume(&mut self, vm: &mut Vm, input: Value) -> Result<Step, VmError> {
        if let Some((callee, arg)) = self.call.take() {
            return Ok(vm.call(callee, [arg]));
        }
        let then = self.then.take().expect("a frame is resumed once per call");
        then(input).map(Step::Done)
    }
}

/// The builtin `name` as one call of `callee` with `arg`; its value
/// is `then(result)`.
pub(crate) fn call_then<F>(name: &'static str, callee: Value, arg: Value, then: F) -> Step
where
    F: FnOnce(Value) -> Result<Value, VmError> + Send + 'static,
{
    Step::Run(Box::new(CallThen {
        name,
        call: Some((callee, arg)),
        then: Some(then),
    }))
}
