//! Calling a value: a closure gets a frame, a builtin runs or gets a
//! frame of its own (see [`Native`]).

use crate::value::Value;

use super::dispatch::invoke_host_fn;
use super::runtime::{CallFrame, Frame, Native, Step};
use super::{Vm, VmError};

/// Maximum number of calls of silt functions in progress on a VM
/// before it reports a stack-overflow error (see `stack_overflow_error`
/// for the user-facing message). Every such call takes a frame, however
/// it is made: by a call, by a method call, or by a builtin that was
/// passed the function (`list.map`). The builtin's own frame is not
/// counted: it comes with a function's frame, the one of its caller.
const MAX_FRAMES: usize = 100_000;

/// Build the user-facing stack-overflow error reported when a call would
/// exceed `MAX_FRAMES`. A call in tail position, of a function or of a
/// method, runs in its caller's frame; any other takes one.
fn stack_overflow_error() -> VmError {
    VmError::new(format!(
        "stack overflow: recursion depth exceeded {MAX_FRAMES} frames"
    ))
    .with_help(
        "a call in tail position, of a function or a method, reuses its caller's frame; a call whose result is still used (`1 + f(n - 1)`) takes a frame of its own",
    )
}

/// What a call did.
pub(super) enum Entered {
    /// It is finished, with this value.
    Value(Value),
    /// A function's frame is on top, at its first instruction.
    Code,
    /// A builtin's frame is on top, not resumed yet.
    Native,
}

impl Vm {
    /// Push the frame of a call of `closure`, whose arguments are the
    /// stack's values from `base_slot` on, if the VM has room for one
    /// more.
    fn push_code_frame(
        &mut self,
        closure: std::sync::Arc<crate::bytecode::VmClosure>,
        base_slot: usize,
    ) -> Result<(), VmError> {
        if self.frames.len() - self.native_frames >= MAX_FRAMES {
            return Err(stack_overflow_error());
        }
        self.frames.push(Frame::Code(CallFrame {
            closure,
            ip: 0,
            base_slot,
        }));
        Ok(())
    }

    /// Push the frame of a builtin.
    pub(super) fn push_native_frame(&mut self, native: Box<dyn Native>) {
        self.frames.push(Frame::Native(native));
        self.native_frames += 1;
    }

    /// Call the value in the stack's slot `func_slot` with the `argc`
    /// values above it. Whatever the call does, those values are the
    /// callee's now: a closure's frame starts above `func_slot` and its
    /// return cuts the stack back to it.
    pub(super) fn call_value(
        &mut self,
        func_val: Value,
        argc: usize,
        func_slot: usize,
    ) -> Result<Entered, VmError> {
        match func_val {
            Value::VmClosure(closure) => {
                if argc != closure.function.arity() {
                    return Err(VmError::type_confusion(format!(
                        "function '{}' expects {} arguments, got {}",
                        closure.function.name(),
                        closure.function.arity(),
                        argc
                    )));
                }
                // The arguments are the frame's first values.
                self.push_code_frame(closure, func_slot + 1)?;
                Ok(Entered::Code)
            }
            other => {
                let args = self.stack.split_off(func_slot + 1);
                self.stack.truncate(func_slot);
                self.call_with(other, args)
            }
        }
    }

    /// The step of a builtin's frame that calls `callee` with `args`
    /// ([`Step::Call`]).
    pub(crate) fn call(&mut self, callee: Value, args: impl IntoIterator<Item = Value>) -> Step {
        self.call_step(callee, |stack| stack.extend(args))
    }

    /// [`Vm::call`] with the arguments `push_args` pushes on the stack.
    pub(crate) fn call_step(
        &mut self,
        callee: Value,
        push_args: impl FnOnce(&mut Vec<Value>),
    ) -> Step {
        // The slot a `Call` instruction has the function in.
        let func_slot = self.stack.len();
        self.stack.push(Value::Unit);
        push_args(&mut self.stack);
        Step::Call {
            callee,
            argc: self.stack.len() - func_slot - 1,
        }
    }

    /// Call `callee` with `args`, which are not on the stack.
    pub(super) fn call_with(
        &mut self,
        callee: Value,
        args: Vec<Value>,
    ) -> Result<Entered, VmError> {
        match callee {
            Value::VmClosure(_) => {
                // The slot a `Call` instruction has the function in.
                let func_slot = self.stack.len();
                let argc = args.len();
                self.stack.push(Value::Unit);
                self.stack.extend(args);
                self.call_value(callee, argc, func_slot)
            }
            Value::BuiltinFn(name) => self.enter_builtin(&name, &args),
            Value::HostFn(host) => invoke_host_fn(&host, &args).map(Entered::Value),
            Value::VariantConstructor(tag) => {
                let arity = tag.arity();
                if args.len() != arity {
                    return Err(VmError::type_confusion(format!(
                        "variant constructor '{tag}' expects {arity} arguments, got {}",
                        args.len()
                    )));
                }
                Ok(Entered::Value(Value::Variant(tag, args)))
            }
            other => Err(VmError::type_confusion(format!(
                "cannot call value of type {}",
                self.user_facing_type_name(&other)
            ))),
        }
    }

    /// Call the builtin `name` with `args`.
    pub(super) fn enter_builtin(&mut self, name: &str, args: &[Value]) -> Result<Entered, VmError> {
        match self.dispatch_builtin(name, args) {
            Ok(Step::Done(value)) => Ok(Entered::Value(value)),
            Ok(Step::Run(native)) => {
                self.push_native_frame(native);
                Ok(Entered::Native)
            }
            Ok(Step::Call { .. } | Step::Park(_) | Step::Yield) => Err(VmError::new(format!(
                "internal VM error: the builtin '{name}' calls or waits without a frame"
            ))),
            Err(e) => Err(e),
        }
    }
}
