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
/// exceed `MAX_FRAMES`. The tip is scoped honestly: tail-call elimination
/// only exists for *plain* function calls (`Op::TailCall`) — method
/// dispatch (`Op::CallMethod`) and builtin callbacks (`Op::CallBuiltin`)
/// have no tail form and always consume a frame, so "put the call in tail
/// position" is not actionable advice for recursive trait methods.
fn stack_overflow_error() -> VmError {
    VmError::new(format!(
        "stack overflow: recursion depth exceeded {MAX_FRAMES} frames"
    ))
    .with_help(
        "tail-call elimination applies to plain function calls in tail position; method and builtin calls always consume a frame",
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
    /// A builtin's frame is on top and the task's slice ends (see
    /// [`Step::Park`]).
    Parked,
}

/// A builtin that asked to be called again when the task next runs: it
/// found nothing to take (an empty channel, an I/O call that has not
/// finished) and said what the task waits for.
struct Retry {
    name: String,
    args: Vec<Value>,
}

impl Native for Retry {
    fn name(&self) -> &str {
        &self.name
    }

    fn resume(&mut self, vm: &mut Vm, _input: Value) -> Result<Step, VmError> {
        match vm.dispatch_builtin(&self.name, &self.args) {
            Err(e) if e.is_yield => {
                if let Some(args) = vm.retry_args.take() {
                    self.args = args;
                }
                Ok(Step::Park)
            }
            other => other,
        }
    }
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
    fn push_native_frame(&mut self, native: Box<dyn Native>) {
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
            Ok(Step::Call { .. } | Step::Park) => Err(VmError::new(format!(
                "internal VM error: the builtin '{name}' calls or parks without a frame"
            ))),
            Err(e) if e.is_yield => {
                let args = self.retry_args.take().unwrap_or_else(|| args.to_vec());
                self.push_native_frame(Box::new(Retry {
                    name: name.to_string(),
                    args,
                }));
                Ok(Entered::Parked)
            }
            Err(e) => Err(e),
        }
    }
}
