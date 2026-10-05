//! Calling a value: closures, builtins held as values, and callbacks
//! invoked from builtins.

use crate::bytecode::Op;
use crate::value::Value;

use super::dispatch::invoke_host_fn;
use super::run::DispatchResult;
use super::runtime::{CallFrame, SuspendedBuiltin, SuspendedInvoke};
use super::{NativeDepthGuard, Vm, VmError, native_depth_limit};

/// Maximum number of call frames the VM will allocate before reporting a
/// stack-overflow error (see `stack_overflow_error` for the user-facing
/// message). Consulted by both `call_value` (normal Silt calls) and
/// `invoke_callable` (higher-order builtins re-entering user code).
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

/// Build the user-facing stack-overflow error reported when method calls
/// and builtin callbacks nest deeper than the current thread's stack can
/// hold. Same error as `stack_overflow_error`, for the second resource a
/// call can exhaust: these two kinds of call run a nested interpreter loop
/// on the host stack, so they are limited by the thread's stack size and
/// reach their limit long before `MAX_FRAMES`.
fn native_stack_overflow_error() -> VmError {
    let limit = native_depth_limit();
    VmError::new(format!(
        "stack overflow: recursion depth exceeded {limit} nested method or callback calls"
    ))
    .with_help(format!(
        "a method call, or a function passed to a builtin such as list.map, uses the host stack for every level of nesting; for deep recursion use a plain function call, which is limited to {MAX_FRAMES} frames, or a loop"
    ))
}

/// Count one more interpreter loop nested on this thread's host stack, or
/// fail with the stack-overflow error if the thread is at its limit. The
/// loop is uncounted when the returned guard is dropped, so bind the guard
/// to a local that lives as long as the loop does.
pub(super) fn enter_native_level() -> Result<NativeDepthGuard, VmError> {
    NativeDepthGuard::enter().ok_or_else(native_stack_overflow_error)
}

/// What became of a builtin that was called as a function value. See
/// `Vm::call_builtin_value`.
enum BuiltinValueCall {
    /// The builtin returned this value.
    Done(Value),
    /// The builtin yielded.
    Yielded {
        /// The yield, to be passed on to the scheduler.
        signal: VmError,
        /// The arguments to call the builtin with on resume.
        resume_args: Vec<Value>,
    },
}

impl Vm {
    // ── Call a value ──────────────────────────────────────────────

    pub(super) fn call_value(
        &mut self,
        func_val: Value,
        argc: usize,
        func_slot: usize,
    ) -> Result<(), VmError> {
        match func_val {
            Value::VmClosure(closure) => {
                if argc != closure.function.arity as usize {
                    return Err(VmError::new(format!(
                        "function '{}' expects {} arguments, got {}",
                        closure.function.name, closure.function.arity, argc
                    )));
                }
                if self.frames.len() >= MAX_FRAMES {
                    return Err(stack_overflow_error());
                }
                // Push a new call frame. The arguments are already on the stack
                // at positions [func_slot+1 .. func_slot+1+argc].
                // The base_slot for the new frame is func_slot+1 so the args
                // are at locals[0..argc].
                self.frames.push(CallFrame {
                    closure,
                    ip: 0,
                    base_slot: func_slot + 1,
                });
                Ok(())
            }
            Value::BuiltinFn(name) => {
                let start = func_slot + 1;
                let args: Vec<Value> = self.stack[start..start + argc].to_vec();
                // Pop everything including the function slot
                self.stack.truncate(func_slot);
                match self.call_builtin_value(&name, &args)? {
                    BuiltinValueCall::Done(result) => {
                        self.push(result);
                        Ok(())
                    }
                    BuiltinValueCall::Yielded {
                        signal,
                        resume_args,
                    } => {
                        // `Op::Call` is re-executed on resume: put back
                        // what it reads, the function value at func_slot
                        // and the arguments above it.
                        self.push(Value::BuiltinFn(name));
                        self.stack.extend(resume_args);
                        Err(signal)
                    }
                }
            }
            Value::HostFn(host) => {
                let start = func_slot + 1;
                let result = invoke_host_fn(&host, &self.stack[start..start + argc]);
                self.stack.truncate(func_slot);
                self.push(result?);
                Ok(())
            }
            Value::VariantConstructor(tag) => {
                let arity = tag.arity();
                if argc != arity {
                    return Err(VmError::new(format!(
                        "variant constructor '{tag}' expects {arity} arguments, got {argc}"
                    )));
                }
                let start = func_slot + 1;
                let fields: Vec<Value> = self.stack[start..start + argc].to_vec();
                self.stack.truncate(func_slot);
                self.push(Value::Variant(tag, fields));
                Ok(())
            }
            _ => Err(VmError::new(format!(
                "cannot call value of type {}",
                self.user_facing_type_name(&func_val)
            ))),
        }
    }

    // ── Suspended-state stacks (B5, audit round 26) ─────────────
    //
    // `suspended_invoke` and `suspended_builtin` are the TOP of a LIFO
    // stack of suspended states (see field docs on `Vm`). Deeper states
    // live in `suspended_invoke_outer` / `suspended_builtin_outer`.
    // Nested yield scenarios (e.g. nested `task.deadline` + I/O, or
    // nested `list.map` with a yielding callback) can park several
    // suspended states at once; before the fix in round 26, the outer
    // yield would overwrite the inner state in the single Option slot
    // and the inner callback got re-run from scratch on resume.

    /// Push `s` onto the `suspended_invoke` stack. If the Option slot is
    /// already occupied (an inner yield happened first), spill the current
    /// occupant into `suspended_invoke_outer` before replacing the slot.
    pub(crate) fn push_suspended_invoke(&mut self, s: SuspendedInvoke) {
        if let Some(existing) = self.suspended_invoke.take() {
            self.suspended_invoke_outer.push(existing);
        }
        self.suspended_invoke = Some(s);
    }

    /// Pop the top of the `suspended_invoke` stack (i.e. take the Option
    /// slot) and auto-promote the next deeper state from
    /// `suspended_invoke_outer` into the slot so subsequent `.is_some()`
    /// checks on the field correctly reflect "a state is still parked
    /// below". Returns the state that was on top (or None if the stack
    /// is empty).
    pub(crate) fn take_suspended_invoke(&mut self) -> Option<SuspendedInvoke> {
        let head = self.suspended_invoke.take();
        if head.is_some()
            && let Some(next) = self.suspended_invoke_outer.pop()
        {
            self.suspended_invoke = Some(next);
        }
        head
    }

    /// Push `s` onto the `suspended_builtin` stack, with the same
    /// spill-on-overwrite discipline as `push_suspended_invoke`.
    pub(crate) fn push_suspended_builtin(&mut self, s: SuspendedBuiltin) {
        if let Some(existing) = self.suspended_builtin.take() {
            self.suspended_builtin_outer.push(existing);
        }
        self.suspended_builtin = Some(s);
    }

    /// Pop the top of the `suspended_builtin` stack, auto-promoting the
    /// next deeper state into the Option slot. Returns the state that
    /// was on top.
    pub(crate) fn take_suspended_builtin(&mut self) -> Option<SuspendedBuiltin> {
        let head = self.suspended_builtin.take();
        if head.is_some()
            && let Some(next) = self.suspended_builtin_outer.pop()
        {
            self.suspended_builtin = Some(next);
        }
        head
    }

    // ── Builtins called as function values ───────────────────────

    /// Run a builtin for a caller that holds it as a function VALUE:
    /// `Op::Call` and `Op::TailCall` on a builtin (`call_value`), and a
    /// builtin passed as a callback or stored in a record field
    /// (`invoke_callable`, `resume_suspended_invoke`). Every such caller
    /// goes through here.
    ///
    /// A builtin that yields re-pushes its arguments, because that is
    /// what `Op::CallBuiltin` pops again when it is re-executed on resume.
    /// A caller that holds the builtin as a value resumes in its own way,
    /// so this helper takes the re-pushed arguments back off the stack and
    /// hands them over with the yield. However the builtin ends, the stack
    /// has the height it had at entry; the caller then lays out what ITS
    /// resume reads.
    fn call_builtin_value(
        &mut self,
        name: &str,
        args: &[Value],
    ) -> Result<BuiltinValueCall, VmError> {
        let floor = self.stack.len();
        match self.dispatch_builtin(name, args) {
            Ok(value) => {
                self.stack.truncate(floor);
                Ok(BuiltinValueCall::Done(value))
            }
            Err(signal) if signal.is_yield => {
                let repushed = if self.stack.len() > floor {
                    self.stack.split_off(floor)
                } else {
                    Vec::new()
                };
                // A builtin re-pushes as many values as it was called
                // with. Should one ever not, resume with the original
                // arguments rather than with a call of the wrong arity.
                let resume_args = if repushed.len() == args.len() {
                    repushed
                } else {
                    args.to_vec()
                };
                Ok(BuiltinValueCall::Yielded {
                    signal,
                    resume_args,
                })
            }
            Err(e) => {
                self.stack.truncate(floor);
                Err(e)
            }
        }
    }

    /// Call a builtin that `invoke_callable` was given as the callable.
    /// If it yields, a `SuspendedInvoke::Builtin` is left behind, so that
    /// the caller finds `suspended_invoke` set exactly as it does after a
    /// closure yielded, and `resume_suspended_invoke` calls the builtin
    /// again.
    fn invoke_builtin_value(&mut self, name: &str, args: &[Value]) -> Result<Value, VmError> {
        match self.call_builtin_value(name, args)? {
            BuiltinValueCall::Done(value) => Ok(value),
            BuiltinValueCall::Yielded {
                signal,
                resume_args,
            } => {
                self.push_suspended_invoke(SuspendedInvoke::Builtin {
                    name: name.to_string(),
                    args: resume_args,
                });
                Err(signal)
            }
        }
    }

    /// Call a callable Value and return its result. Used for higher-order builtins.
    pub(crate) fn invoke_callable(
        &mut self,
        func: &Value,
        args: &[Value],
    ) -> Result<Value, VmError> {
        // The closure arm runs a nested interpreter loop, and the builtin
        // arm may run one through the builtin's own callback.
        let _native_level = enter_native_level()?;
        match func {
            Value::VmClosure(closure) => {
                if args.len() != closure.function.arity as usize {
                    return Err(VmError::new(format!(
                        "function '{}' expects {} arguments, got {}",
                        closure.function.name,
                        closure.function.arity,
                        args.len()
                    )));
                }
                if self.frames.len() >= MAX_FRAMES {
                    return Err(stack_overflow_error());
                }
                // Save state
                let saved_frame_count = self.frames.len();
                let func_slot = self.stack.len();
                // Push a dummy for the function slot
                self.push(Value::Unit);
                for arg in args {
                    self.push(arg.clone());
                }
                self.frames.push(CallFrame {
                    closure: closure.clone(),
                    ip: 0,
                    base_slot: func_slot + 1,
                });
                // Run the execution loop until we return to the previous frame count
                loop {
                    let saved_ip = self.current_frame()?.ip;
                    let op_byte = self.read_byte()?;
                    let op = Op::from_byte(op_byte).ok_or_else(|| {
                        self.frames.truncate(saved_frame_count);
                        self.stack.truncate(func_slot);
                        VmError::new(format!("unknown opcode: {op_byte}"))
                    })?;
                    match self.dispatch_one(op) {
                        Ok(DispatchResult::Continue) => {}
                        Ok(DispatchResult::Return(result)) => {
                            let finished_base = self.current_frame()?.base_slot;
                            self.frames.pop();
                            // Prune tail-call elided diagnostic entries for
                            // the just-popped frame so stale data from prior
                            // callback iterations can't bleed into later
                            // error reports. Mirrors execute()/execute_slice().
                            self.prune_tco_elided(self.frames.len());
                            if self.frames.len() < saved_frame_count {
                                return Err(VmError::new(
                                    "internal VM error: frame stack underflow during call".into(),
                                ));
                            }
                            if self.frames.len() == saved_frame_count {
                                // We've returned from our closure
                                self.stack.truncate(func_slot);
                                return Ok(result);
                            }
                            // Inner return from nested call
                            let inner_func_slot = finished_base.saturating_sub(1);
                            self.stack.truncate(inner_func_slot);
                            self.push(result);
                        }
                        Ok(DispatchResult::EarlyReturn {
                            value,
                            finished_base,
                        }) => {
                            // EarlyReturn from `?` already popped its frame;
                            // prune tco_elided to match, same as execute()/execute_slice().
                            self.prune_tco_elided(self.frames.len());
                            // QuestionMark popped a frame. Check if we've returned to our level.
                            if self.frames.len() <= saved_frame_count {
                                self.stack.truncate(func_slot);
                                return Ok(value);
                            }
                            // Inner early return
                            let inner_func_slot = finished_base.saturating_sub(1);
                            self.stack.truncate(inner_func_slot);
                            self.push(value);
                        }
                        Err(e) if e.is_yield => {
                            // A builtin inside the callback yielded (e.g. IO).
                            // Rewind the current frame's IP so the yielding
                            // opcode will be re-executed on resume.
                            if let Ok(f) = self.current_frame_mut() {
                                f.ip = saved_ip;
                            }
                            // Save the extra frames and stack so the caller
                            // (e.g. channel.each) can resume instead of
                            // re-running the callback from scratch.  Use the
                            // stack-aware push so an inner suspended state
                            // that yielded first (nested yield: B5) is NOT
                            // overwritten.
                            let extra_frames = self.frames.split_off(saved_frame_count);
                            let extra_stack = self.stack.split_off(func_slot);
                            self.push_suspended_invoke(SuspendedInvoke::Closure {
                                frames: extra_frames,
                                stack: extra_stack,
                                func_slot,
                            });
                            return Err(e);
                        }
                        Err(e) => {
                            // Capture the callback's span + call_stack BEFORE
                            // truncating frames, otherwise the outer
                            // enrich_error at Vm::run will see only the builtin
                            // dispatch site and relocalize the error away from
                            // the callback body where the real bug lives.
                            // (Audit L2 callback-frame erasure — rounds 1-15
                            // deferred, round 16 fix.)
                            let enriched = self.enrich_error(e);
                            self.frames.truncate(saved_frame_count);
                            // Prune tail-call elided diagnostic entries for
                            // the frames we just truncated, mirroring the
                            // Return/EarlyReturn arms. Without this, stale
                            // tco_elided entries from the callback can bleed
                            // into later unrelated call_stack renders.
                            // (Audit round 26 L7 — mirror fix for round-22.)
                            self.prune_tco_elided(self.frames.len());
                            self.stack.truncate(func_slot);
                            return Err(enriched);
                        }
                    }
                }
            }
            Value::BuiltinFn(name) => self.invoke_builtin_value(name, args),
            Value::HostFn(host) => invoke_host_fn(host, args),
            Value::VariantConstructor(tag) => {
                let arity = tag.arity();
                if args.len() != arity {
                    return Err(VmError::new(format!(
                        "variant constructor '{tag}' expects {arity} arguments, got {}",
                        args.len()
                    )));
                }
                Ok(Value::Variant(tag.clone(), args.to_vec()))
            }
            _ => Err(VmError::new(format!(
                "cannot call value of type {}",
                self.user_facing_type_name(func)
            ))),
        }
    }

    /// Resume a previously suspended `invoke_callable`.
    ///
    /// When a builtin inside a callback yielded (e.g. IO inside
    /// `channel.each`), the callback's frames and stack were saved in
    /// `self.suspended_invoke`.  This method restores them and continues
    /// the execution loop until the callback returns a result.
    pub(crate) fn resume_suspended_invoke(&mut self) -> Result<Value, VmError> {
        // Resuming nests an interpreter loop exactly as the first call did.
        let _native_level = enter_native_level()?;
        // Pop via the stack-aware helper so any deeper suspended state
        // (e.g. inner yield from a nested `task.deadline`) is auto-promoted
        // into the top slot for subsequent `.is_some()` checks. (B5.)
        let suspended = self.take_suspended_invoke().ok_or_else(|| {
            VmError::new("internal VM error: missing suspended state during resume".into())
        })?;
        let (frames, stack, func_slot) = match suspended {
            SuspendedInvoke::Closure {
                frames,
                stack,
                func_slot,
            } => (frames, stack, func_slot),
            // A builtin is resumed by calling it again, with the
            // arguments it asked for when it yielded.
            SuspendedInvoke::Builtin { name, args } => {
                return self.invoke_builtin_value(&name, &args);
            }
        };
        let saved_frame_count = self.frames.len();
        // Restore the saved frames and stack.
        self.frames.extend(frames);
        self.stack.extend(stack);
        // Continue the execution loop (same as invoke_callable's inner loop).
        loop {
            let saved_ip = self.current_frame()?.ip;
            let op_byte = self.read_byte()?;
            let op = Op::from_byte(op_byte).ok_or_else(|| {
                self.frames.truncate(saved_frame_count);
                self.stack.truncate(func_slot);
                VmError::new(format!("unknown opcode: {op_byte}"))
            })?;
            match self.dispatch_one(op) {
                Ok(DispatchResult::Continue) => {}
                Ok(DispatchResult::Return(result)) => {
                    let finished_base = self.current_frame()?.base_slot;
                    self.frames.pop();
                    // Prune tail-call elided diagnostic entries for the
                    // just-popped frame, mirroring invoke_callable's Return arm.
                    self.prune_tco_elided(self.frames.len());
                    if self.frames.len() < saved_frame_count {
                        return Err(VmError::new(
                            "internal VM error: frame stack underflow during resume".into(),
                        ));
                    }
                    if self.frames.len() == saved_frame_count {
                        self.stack.truncate(func_slot);
                        return Ok(result);
                    }
                    let inner_func_slot = finished_base.saturating_sub(1);
                    self.stack.truncate(inner_func_slot);
                    self.push(result);
                }
                Ok(DispatchResult::EarlyReturn {
                    value,
                    finished_base,
                }) => {
                    // Prune tail-call elided diagnostic entries after
                    // EarlyReturn's implicit frame pop, mirroring
                    // invoke_callable's EarlyReturn arm.
                    self.prune_tco_elided(self.frames.len());
                    if self.frames.len() <= saved_frame_count {
                        self.stack.truncate(func_slot);
                        return Ok(value);
                    }
                    let inner_func_slot = finished_base.saturating_sub(1);
                    self.stack.truncate(inner_func_slot);
                    self.push(value);
                }
                Err(e) if e.is_yield => {
                    if let Ok(f) = self.current_frame_mut() {
                        f.ip = saved_ip;
                    }
                    let extra_frames = self.frames.split_off(saved_frame_count);
                    let extra_stack = self.stack.split_off(func_slot);
                    // Push onto the stack; if an even-deeper suspended
                    // state exists (nested-yield case), it's preserved in
                    // `suspended_invoke_outer`. (B5 fix.)
                    self.push_suspended_invoke(SuspendedInvoke::Closure {
                        frames: extra_frames,
                        stack: extra_stack,
                        func_slot,
                    });
                    return Err(e);
                }
                Err(e) => {
                    // Same fix as invoke_callable: enrich the error with the
                    // callback's span + call_stack before truncating frames,
                    // so the outer enrich_error at Vm::run doesn't relocalize
                    // to the builtin dispatch site. (Audit L2 callback-frame
                    // erasure — rounds 1-15 deferred, round 16 fix.)
                    //
                    // Lock: tests/lang/callback_frame_capture_tests.rs
                    // `test_resume_suspended_invoke_preserves_callback_frame`
                    // mutates this line and asserts the callback-body span
                    // disappears (snaps back to the `channel.each` call site).
                    let enriched = self.enrich_error(e);
                    self.frames.truncate(saved_frame_count);
                    // Mirror Return/EarlyReturn's prune so stale tco_elided
                    // entries don't bleed into later call_stack renders.
                    // (Audit round 26 L7 — mirror fix for round-22.)
                    self.prune_tco_elided(self.frames.len());
                    self.stack.truncate(func_slot);
                    return Err(enriched);
                }
            }
        }
    }

    /// Check `suspended_invoke` on entry to a single-callback builtin
    /// (e.g. `result.map_ok`).  If set, resume it and return the callback's
    /// result.  Otherwise, invoke the callback fresh.  On yield, re-push
    /// the passed `original_args` so the outer `CallBuiltin` can re-dispatch.
    pub(crate) fn invoke_callable_resumable(
        &mut self,
        callback: &Value,
        cb_args: &[Value],
        original_args: &[Value],
    ) -> Result<Value, VmError> {
        let result = if self.suspended_invoke.is_some() {
            self.resume_suspended_invoke()
        } else {
            self.invoke_callable(callback, cb_args)
        };
        match result {
            Ok(v) => Ok(v),
            Err(e) if e.is_yield => {
                for a in original_args {
                    self.push(a.clone());
                }
                Err(e)
            }
            Err(e) => Err(e),
        }
    }
}
