//! Calling a value: a closure gets a frame, a builtin runs or gets a
//! frame of its own (see [`Native`]).

use std::panic::AssertUnwindSafe;

use crate::builtins::registry::{Row, registry};
use crate::defs::BuiltinId;
use crate::value::Value;

use super::dispatch::{catch_builtin_panic, invoke_host_fn};
use super::run::push_with;
use super::runtime::{CallFrame, Frame, Native, Room, Step, chunks_for, frame_bytes};
use super::{Vm, VmError};

/// The error of a call whose frame the frame budget has no room for
/// (`runtime::FrameBudget`): the budget, of `limit` MiB, is the
/// program's, so the VM that asks when it is used up gets it, and the
/// error says how much that VM's own frames take (`bytes`). A call in
/// tail position, of a function or of a method, runs in its caller's
/// frame; any other takes one.
fn stack_overflow_error(limit: usize, bytes: usize) -> VmError {
    let holds = match bytes >> 20 {
        0 => format!("{} KiB", bytes >> 10),
        mib => format!("{mib} MiB"),
    };
    VmError::new(format!(
        "stack overflow: call frames take more than {limit} MiB; this task holds {holds}"
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
    /// stack's values from `base_slot` on, if the frame budget has room
    /// for it. Every call of a silt function takes a frame, however it
    /// is made: by a call, by a method call, or by a builtin that was
    /// passed the function (`list.map`), whose own frame is counted too.
    fn push_code_frame(
        &mut self,
        closure: std::sync::Arc<crate::bytecode::VmClosure>,
        base_slot: usize,
    ) -> Result<(), VmError> {
        let bytes = frame_bytes(self.frames.len() + 1, self.stack.len());
        if !self.room.fits(bytes) {
            self.make_room(bytes)?;
        }
        // (Built where it goes, as the values the loop pushes.)
        push_with(&mut self.frames, || {
            Frame::Code(CallFrame {
                closure,
                ip: 0,
                base_slot,
            })
        });
        Ok(())
    }

    /// Take from the frame budget what frames of `bytes` need beyond
    /// what this VM holds, or give back what they do not: a call's
    /// frames are outside the VM's room (`Room`).
    #[cold]
    #[inline(never)]
    fn make_room(&mut self, bytes: usize) -> Result<(), VmError> {
        let need = chunks_for(bytes);
        let budget = &self.runtime.frame_budget;
        if need > self.held {
            if !budget.take(need - self.held) {
                return Err(stack_overflow_error(budget.limit(), bytes));
            }
        } else {
            budget.give(self.held - need);
        }
        self.held = need;
        self.room = Room::holding(need);
        Ok(())
    }

    /// Give back to the frame budget what this VM's frames do not need
    /// now, all of it beyond `keep` chunks: where its frames have
    /// shrunk: a slice of a task ends, a run ends or fails (a REPL
    /// entry is over), the VM ends.
    pub(super) fn give_back_frames(&mut self) {
        let need = chunks_for(frame_bytes(self.frames.len(), self.stack.len()));
        if need < self.held {
            self.runtime.frame_budget.give(self.held - need);
            self.held = need;
            self.room = Room::holding(need);
        }
    }

    /// Push the frame of a builtin.
    pub(crate) fn push_native_frame(&mut self, native: Box<dyn Native>) {
        self.frames.push(Frame::Native(native));
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
            Value::VmClosure(closure) => self.call_closure(closure, argc, func_slot),
            other => self.call_other(other, func_slot),
        }
    }

    /// [`Vm::call_value`] of a closure.
    #[inline(always)]
    pub(super) fn call_closure(
        &mut self,
        closure: std::sync::Arc<crate::bytecode::VmClosure>,
        argc: usize,
        func_slot: usize,
    ) -> Result<Entered, VmError> {
        let argc = super::dicts::with_bound(&mut self.stack, &closure, func_slot + 1, argc);
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

    /// [`Vm::call_value`] of a value that is no closure: its arguments,
    /// and its slot, leave the stack.
    fn call_other(&mut self, callee: Value, func_slot: usize) -> Result<Entered, VmError> {
        match callee {
            Value::BuiltinFn(id) => self.with_args(func_slot + 1, |vm, args| {
                vm.stack.truncate(func_slot);
                vm.enter_builtin(id, args)
            }),
            Value::HostFn(host) => self.with_args(func_slot + 1, |vm, args| {
                vm.stack.truncate(func_slot);
                invoke_host_fn(&host, args).map(Entered::Value)
            }),
            Value::VariantConstructor(tag) => {
                // (The fields are taken off the stack into the variant:
                // one of a single field makes no list of them.)
                let (arity, given) = (tag.arity(), self.stack.len() - func_slot - 1);
                if given != arity {
                    self.stack.truncate(func_slot);
                    return Err(VmError::type_confusion(format!(
                        "variant constructor '{tag}' expects {arity} arguments, got {given}"
                    )));
                }
                let variant = Value::variant(tag, self.stack.drain(func_slot + 1..));
                self.stack.truncate(func_slot);
                Ok(Entered::Value(variant))
            }
            other => {
                self.stack.truncate(func_slot);
                Err(VmError::type_confusion(format!(
                    "cannot call value of type {}",
                    self.user_facing_type_name(&other)
                )))
            }
        }
    }

    /// Run `call` with the values of the stack from `at` on, which
    /// leave the stack, as the arguments of a builtin.
    ///
    /// A builtin's body has the VM and its arguments at once, and may
    /// push on the stack: the arguments are off the stack before it
    /// runs. They are moved to the VM's one list for them
    /// (`builtin_args`), which is out of the VM for the time of the
    /// call and back, emptied, after it: a call allocates no list of
    /// its own. The list is given up where the VM waits
    /// (`Vm::deliver_native`). (A call from inside a call would find the VM's list
    /// empty and fill a new one. No builtin makes one: one that calls
    /// a function returns first, and the call is a frame.)
    pub(super) fn with_args<T>(
        &mut self,
        at: usize,
        call: impl FnOnce(&mut Vm, &[Value]) -> T,
    ) -> T {
        let mut args = std::mem::take(&mut self.builtin_args);
        for value in &mut self.stack[at..] {
            args.push(std::mem::replace(value, Value::Unit));
        }
        self.stack.truncate(at);
        let result = call(self, &args);
        args.clear();
        self.builtin_args = args;
        result
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

    /// Call the builtin `id`, a row of the builtin registry, with
    /// `args`: the row's body. A panic inside it becomes a clean
    /// `VmError` instead of tearing down the current scheduler worker
    /// thread, as for a host function (`invoke_host_fn`). A host clock
    /// that has panicked, in this call or on one of the runtime's
    /// threads, fails the call: the readings the builtin got since are
    /// not real, and the waits it was woken from have not ended.
    pub(super) fn enter_builtin(
        &mut self,
        id: BuiltinId,
        args: &[Value],
    ) -> Result<Entered, VmError> {
        match registry().builtin(id) {
            Some(row) => self.enter_row(row, args),
            None => Err(VmError::internal(format_args!(
                "a call of the builtin {}, and there is none",
                id.0
            ))),
        }
    }

    /// Call the builtin that is `row`.
    pub(super) fn enter_row(&mut self, row: &Row, args: &[Value]) -> Result<Entered, VmError> {
        // (What a panic is reported under: the module, or the name of a
        // function of the prelude.)
        let owner = match row.module {
            "" => row.name,
            module => module,
        };
        let step = catch_builtin_panic(
            owner,
            AssertUnwindSafe(|| {
                #[cfg(any(test, feature = "test-hooks"))]
                self.runtime.scheduler.panic_in_a_builtin_for_test();
                row.call(self, args)
            }),
        )?;
        if let Some(failure) = self.runtime.io.clock_failure() {
            return Err(VmError::ending(failure));
        }
        match step {
            Step::Done(value) => Ok(Entered::Value(value)),
            Step::Run(native) => {
                self.push_native_frame(native);
                Ok(Entered::Native)
            }
            Step::Call { .. } | Step::Park(_) | Step::Yield => {
                Err(VmError::internal(format_args!(
                    "the builtin '{}' calls or waits without a frame",
                    row.qualified()
                )))
            }
        }
    }
}
