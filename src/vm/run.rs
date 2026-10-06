//! Main execution loop and opcode dispatch.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::bytecode::{Instr, Op, VmClosure, record_type_matches};
use crate::scheduler::SliceResult;
use crate::typeinfo::bv;
use crate::value::{MAX_RANGE_MATERIALIZE, Value, checked_range_len};

use super::calls::Entered;
use super::runtime::{Frame, Step};
use super::{Vm, VmError};
use crate::runtime::sync::Wait;

/// Gate the language-level `==` / `!=` operators against function-shaped
/// values, returning the canonical surface name (`"Fn"`) to name in the
/// error if the value cannot participate in equality, or `None` if it can.
///
/// silt does NOT statically enforce inferred trait bounds on polymorphic
/// templates: `pending_numeric_checks` (src/typechecker/inference.rs)
/// skips operands whose type is still a `Var`, on the documented promise
/// that "the VM catches it at runtime with a clean operator-domain
/// diagnostic." Ordering honors that promise (`compare()`'s catch-all in
/// src/vm/arithmetic.rs errors on function-shaped values) and so does
/// string-interpolation Display (the `Op::DisplayValue` gate). Equality was
/// the lone bypass: a polymorphic `fn eq(x: a, y: a) -> Bool { x == y }`
/// called with two functions used to silently return a `Bool` — `PartialEq
/// for Value` does `Arc::ptr_eq` on closures and name-equality on builtins
/// (src/value/key.rs) — instead of erroring like the concrete `f == g`, which
/// `is_valid_compare_operand` rejects at compile time (`Type::Fun` falls in
/// its `_ => false` arm).
///
/// The rejected set is the values that are, or transitively CONTAIN, a
/// function-shaped leaf — everything the typechecker's equality gate
/// rejects for a concrete operand. The bare shapes (round 96) are the
/// direct `Type::Fun` values; the recursion (via `Vm::value_contains_fn`,
/// src/vm/mod.rs) mirrors the round-97 container gate, whose
/// `operand_builtin_trait_violation` walker rejects the concrete forms
/// (`[{ x -> x }] == [{ x -> x }]`, tuples/records/variants wrapping
/// functions) at compile time. Without the recursion, laundering the same
/// values through a polymorphic wrapper (`fn eq(a: x, b: x) -> Bool
/// { a == b }`) silently produced an `Arc::ptr_eq`-based Bool. Channel /
/// Handle / TcpListener / TcpStream are deliberately NOT rejected: they
/// are equatable by identity at runtime and the typechecker accepts them
/// (`Type::Channel` and `Type::Generic(..)` in
/// `is_valid_compare_operand`), keeping the runtime and compile-time
/// layers in parity. Rust-level collection keying / dedup uses `PartialEq
/// for Value` directly, not this operator path (see
/// tests/typecheck/round74_hash_eq_ord_contract_tests.rs) — but the silt-visible
/// collection builtins that consume that ordering/equality (list.sort /
/// unique / contains / index_of, set.from_list / insert / contains /
/// remove and the set algebra ops) carry their own mirror of this gate:
/// `ensure_no_fn` in src/builtins/collections.rs, locked by
/// tests/lang/collection_builtin_fn_gate_tests.rs. Locked by
/// tests/lang/round96_eq_fn_runtime_tests.rs and
/// tests/typecheck/container_fn_compare_runtime_gate_tests.rs.
fn equality_operand_violation(val: &Value) -> Option<&'static str> {
    if Vm::value_contains_fn(val) {
        Some("Fn")
    } else {
        None
    }
}

/// What running one instruction did.
pub(super) enum DispatchResult {
    /// Go on with the next instruction of the frame on top (a call put
    /// the callee's frame there).
    Continue,
    /// The frame on top is finished, with this value: `Return`, or `?`
    /// on an `Err` or a `None`. The frame is still there.
    Return(Value),
    /// A builtin's frame is on top, not resumed yet.
    Native,
}

/// How a run of the instruction loop ended ([`Vm::run_frames`]).
pub(super) enum Slice {
    /// The frames above the floor are finished, with this value.
    Done(Value),
    /// The budget is used up; the frames can go on.
    OutOfBudget,
    /// A builtin's frame waits for this (see [`Step::Park`]).
    Parked(Wait),
}

impl Vm {
    /// The strings a list operand of the instruction being run names.
    fn names(&self, names: crate::bytecode::Operands<crate::bytecode::Const>) -> Vec<String> {
        let chunk = self.chunk();
        names
            .iter(chunk.code())
            .map(|k| chunk.string(k).to_owned())
            .collect()
    }

    // ── The loop ──────────────────────────────────────────────────

    /// Run the frames above the first `floor`, which are finished when
    /// the function or builtin in frame `floor` has returned, for at
    /// most `budget` steps: instructions, and resumptions of a
    /// builtin's frame.
    ///
    /// This is the only loop that runs silt code. A call, whoever makes
    /// it (an instruction, a method call, a builtin that was passed a
    /// function), is a frame above the caller's, so the depth of silt
    /// calls costs frames and never host stack. The loop is entered
    /// with the frames as an earlier run left them, when that one ran
    /// out of budget or parked.
    ///
    /// On an error the frames stay as they are, for the caller to read
    /// the call stack from ([`Vm::enrich_error`]) and to drop
    /// ([`Vm::unwind`]).
    pub(super) fn run_frames(&mut self, floor: usize, mut budget: usize) -> Result<Slice, VmError> {
        // A builtin's frame on top waits for its input: the value an
        // earlier run had for it when the budget ran out, or unit.
        if self.frames.len() == floor {
            return Ok(Slice::Done(Value::Unit));
        }
        if let Some(Frame::Native(_)) = self.frames.last() {
            let input = self.pending_input.take().unwrap_or(Value::Unit);
            let mut left = budget;
            let end = self.deliver(floor, input, &mut left)?;
            budget = left;
            if let Some(end) = end {
                return Ok(end);
            }
        }
        loop {
            if budget == 0 {
                return Ok(Slice::OutOfBudget);
            }
            budget -= 1;
            let instr = self.fetch();
            let end = match self.dispatch_one(instr)? {
                DispatchResult::Continue => continue,
                DispatchResult::Return(result) => {
                    let Some(Frame::Code(finished)) = self.frames.pop() else {
                        unreachable!("an instruction runs in a function's frame")
                    };
                    // Prune any tail-call elided diagnostic entries that
                    // belong to the just-popped frame slot so stale data
                    // can't bleed into later unrelated calls at this depth.
                    if !self.tco_elided.is_empty() {
                        self.prune_tco_elided(self.frames.len());
                    }
                    // The function's own slot goes with its frame.
                    self.stack.truncate(finished.base_slot.saturating_sub(1));
                    // (A copy, so that the loop's own count stays in a
                    // register.)
                    let mut left = budget;
                    let end = self.deliver(floor, result, &mut left)?;
                    budget = left;
                    end
                }
                DispatchResult::Native => {
                    let mut left = budget;
                    let end = self.deliver(floor, Value::Unit, &mut left)?;
                    budget = left;
                    end
                }
            };
            if let Some(end) = end {
                return Ok(end);
            }
        }
    }

    /// Give `value` to the frame on top: a function's frame gets it on
    /// the stack, and the instruction loop goes on (`None`); a
    /// builtin's frame is resumed with it, until one calls a function
    /// (`None`) or the run ends.
    fn deliver(
        &mut self,
        floor: usize,
        mut value: Value,
        budget: &mut usize,
    ) -> Result<Option<Slice>, VmError> {
        loop {
            if self.frames.len() == floor {
                return Ok(Some(Slice::Done(value)));
            }
            if let Some(Frame::Code(_)) = self.frames.last() {
                self.push(value);
                return Ok(None);
            }
            if *budget == 0 {
                self.pending_input = Some(value);
                return Ok(Some(Slice::OutOfBudget));
            }
            *budget -= 1;
            // The frame is off the list while it runs: it is handed the
            // VM.
            let Some(Frame::Native(mut native)) = self.frames.pop() else {
                unreachable!("the frame on top is a builtin's")
            };
            let step = super::dispatch::resume_native(self, native.as_mut(), value);
            let entered = match step {
                Ok(Step::Done(result)) => {
                    self.native_frames -= 1;
                    Entered::Value(result)
                }
                Ok(Step::Run(next)) => {
                    self.frames.push(Frame::Native(next));
                    Entered::Native
                }
                Ok(Step::Park(wait)) => {
                    self.frames.push(Frame::Native(native));
                    return Ok(Some(Slice::Parked(wait)));
                }
                Ok(Step::Yield) => {
                    self.frames.push(Frame::Native(native));
                    return Ok(Some(Slice::OutOfBudget));
                }
                Ok(Step::Call { callee, argc }) => {
                    self.frames.push(Frame::Native(native));
                    let func_slot = self.stack.len() - argc - 1;
                    self.call_value(callee, argc, func_slot)?
                }
                Err(e) => {
                    self.frames.push(Frame::Native(native));
                    return Err(e);
                }
            };
            value = match entered {
                Entered::Value(result) => result,
                Entered::Native => Value::Unit,
                Entered::Code => return Ok(None),
            };
        }
    }

    /// Drop the frames above the first `floor` and the stack above
    /// `stack_floor`, after an error.
    pub(super) fn unwind(&mut self, floor: usize, stack_floor: usize) {
        while self.frames.len() > floor {
            if let Some(Frame::Native(mut native)) = self.frames.pop() {
                self.native_frames -= 1;
                native.abandon(self);
            }
        }
        self.stack.truncate(stack_floor);
        self.prune_tco_elided(floor);
        self.pending_input = None;
    }

    /// Call `callee` with `args` and run it to its end on the calling
    /// thread, which waits where the code parks ([`Vm::run_thread`]).
    /// This is how a thread that serves silt code outside the
    /// scheduler's workers (a stream stage, an HTTP handler) calls a
    /// function; a builtin never does, it asks the loop that runs it
    /// to ([`Step::Call`]).
    pub(crate) fn call_blocking(
        &mut self,
        callee: &Value,
        args: &[Value],
    ) -> Result<Value, VmError> {
        let floor = self.frames.len();
        let stack_floor = self.stack.len();
        let run = self.run_thread(floor, |vm| {
            match vm.call_with(callee.clone(), args.to_vec())? {
                Entered::Value(value) => Ok(Slice::Done(value)),
                Entered::Code | Entered::Native => vm.run_frames(floor, usize::MAX),
            }
        });
        self.finish_run(run, floor, stack_floor)
    }

    /// Run the frames above the first `floor` to their end on the
    /// calling thread, starting with `start`. The thread counts as a
    /// task of the scheduler while it does; where the code parks, the
    /// thread waits, and the builtin that parked runs again when its
    /// wait has ended.
    pub(super) fn run_thread(
        &mut self,
        floor: usize,
        start: impl FnOnce(&mut Vm) -> Result<Slice, VmError>,
    ) -> Result<Slice, VmError> {
        let scheduler = self.runtime.scheduler.clone();
        let _running = scheduler.enter();
        let mut run = start(self)?;
        loop {
            match run {
                Slice::Done(_) => return Ok(run),
                // The frame gave way to other tasks, which a thread
                // of its own has no need to.
                Slice::OutOfBudget => {}
                Slice::Parked(wait) => {
                    self.woken = Some(scheduler.block_thread(wait, self.is_program())?);
                }
            }
            run = self.run_frames(floor, usize::MAX)?;
        }
    }

    /// The value of a run to the end, or its error with the call stack
    /// of the frames it leaves, which are dropped.
    pub(super) fn finish_run(
        &mut self,
        run: Result<Slice, VmError>,
        floor: usize,
        stack_floor: usize,
    ) -> Result<Value, VmError> {
        let error = match run {
            Ok(Slice::Done(value)) => return Ok(value),
            Ok(Slice::OutOfBudget | Slice::Parked(_)) => {
                VmError::new("internal VM error: a run to the end stopped before it".into())
            }
            Err(e) => e,
        };
        // The call stack is read off the frames this run leaves, which
        // must not stay: the next run on this VM (a REPL's next entry,
        // a stage's next item) would show them as its own.
        let enriched = self.enrich_error(error);
        self.unwind(floor, stack_floor);
        Err(enriched)
    }

    // ── Sliced execution (for M:N scheduler) ─────────────────────

    /// Run a task's frames for up to `max_steps` steps and return a
    /// `SliceResult`. Used by the M:N scheduler's worker threads.
    pub(crate) fn execute_slice(&mut self, max_steps: usize) -> SliceResult {
        match self.run_frames(0, max_steps) {
            Ok(Slice::Done(value)) => SliceResult::Completed(value),
            Ok(Slice::OutOfBudget) => SliceResult::Yielded,
            Ok(Slice::Parked(wait)) => SliceResult::Blocked(wait),
            Err(e) => SliceResult::Failed(e),
        }
    }

    /// What the loop does after a call an instruction made: a value is
    /// the instruction's result.
    fn entered(&mut self, entered: Entered) -> DispatchResult {
        match entered {
            Entered::Value(value) => {
                self.push(value);
                DispatchResult::Continue
            }
            Entered::Code => DispatchResult::Continue,
            Entered::Native => DispatchResult::Native,
        }
    }

    /// Run the instruction `instr`, which [`Vm::fetch`] has stepped
    /// past.
    pub(super) fn dispatch_one(&mut self, instr: Instr) -> Result<DispatchResult, VmError> {
        match instr {
            Instr::Constant { k } => {
                let value = self.chunk().constant(k).clone();
                self.push(value);
            }
            Instr::Unit => self.push(Value::Unit),
            Instr::True => self.push(Value::Bool(true)),
            Instr::False => self.push(Value::Bool(false)),
            Instr::Add => self.binary_arithmetic(Op::Add)?,
            Instr::Sub => self.binary_arithmetic(Op::Sub)?,
            Instr::Mul => self.binary_arithmetic(Op::Mul)?,
            Instr::Div => self.binary_arithmetic(Op::Div)?,
            Instr::Mod => self.binary_arithmetic(Op::Mod)?,
            Instr::Eq => {
                let b = self.pop();
                let a = self.pop();
                self.check_same_type(&a, &b)?;
                // Reject function-shaped operands at the execution site: the
                // typechecker skips this bound on still-polymorphic operands
                // and relies on the VM to catch it (see
                // `equality_operand_violation`). Both operands are checked:
                // the gate recurses into containers, and a shared
                // discriminant no longer implies a shared violation (an
                // empty list and a list of closures both have the List
                // discriminant).
                if let Some(name) =
                    equality_operand_violation(&a).or_else(|| equality_operand_violation(&b))
                {
                    return Err(VmError::type_confusion(format!(
                        "type '{name}' does not implement Equal"
                    )));
                }
                self.push(Value::Bool(a == b));
            }
            Instr::Neq => {
                let b = self.pop();
                let a = self.pop();
                self.check_same_type(&a, &b)?;
                if let Some(name) =
                    equality_operand_violation(&a).or_else(|| equality_operand_violation(&b))
                {
                    return Err(VmError::type_confusion(format!(
                        "type '{name}' does not implement Equal"
                    )));
                }
                self.push(Value::Bool(a != b));
            }
            Instr::Lt => self.compare(|ord| ord.is_lt())?,
            Instr::Gt => self.compare(|ord| ord.is_gt())?,
            Instr::Leq => self.compare(|ord| ord.is_le())?,
            Instr::Geq => self.compare(|ord| ord.is_ge())?,
            Instr::Negate => {
                let val = self.pop();
                match val {
                    Value::Int(n) => match n.checked_neg() {
                        Some(v) => self.push(Value::Int(v)),
                        None => {
                            return Err(VmError::new(format!("integer overflow: negate {n}")));
                        }
                    },
                    Value::Float(n) => {
                        let result = if -n == 0.0 { 0.0 } else { -n };
                        self.push(Value::Float(result));
                    }
                    other => {
                        return Err(VmError::type_confusion(format!(
                            "cannot negate {}",
                            self.user_facing_type_name(&other)
                        )));
                    }
                }
            }
            Instr::Not => {
                let val = self.pop();
                match val {
                    Value::Bool(b) => self.push(Value::Bool(!b)),
                    other => {
                        return Err(VmError::type_confusion(format!(
                            "cannot apply 'not' to {}",
                            self.user_facing_type_name(&other)
                        )));
                    }
                }
            }
            Instr::DisplayValue => {
                let val = self.pop();
                match &val {
                    Value::String(_) => self.push(val),
                    // Mirror the typechecker's string-interpolation Display
                    // gate (inference.rs ~2654): the typechecker's
                    // `type_name_for_impl` reduces a *concrete* operand to a
                    // canonical name and rejects interpolation when that name
                    // is absent from the Display `trait_impl_set` — which
                    // excludes every first-class value that has no Display
                    // impl: function-shaped values (`Fn`), `Channel`,
                    // `Handle` (task handles), `TcpListener` / `TcpStream`
                    // (opaque resources left explicitly unprintable, see
                    // mod.rs ~7878), and the descriptor values
                    // (`TypeDescriptor` / `PrimitiveDescriptor`). For a
                    // polymorphic type variable the operand type is still
                    // `Var` at the interpolation site (type_name_for_impl ->
                    // None), so the compile-time gate is skipped and the value
                    // reaches here. Erroring at the execution site closes the
                    // silent-wrong-behavior hole and matches how a polymorphic
                    // `x > y` on incomparable values (e.g. two Channels) errors
                    // at runtime rather than silently producing a result.
                    //
                    // The rejected set is sourced from the single predicate
                    // `value_implements_display` (below) so the runtime gate
                    // and the surface-name reporting cannot drift from the
                    // `type_name` oracle. Parity is locked by
                    // tests/typecheck/round95_interp_display_runtime_tests.rs.
                    _ if !Self::value_implements_display(&val) => {
                        // Report the canonical surface name so the runtime
                        // message matches the typechecker's compile-time one
                        // (which names `Type::Fun` -> "Fn", not the per-shape
                        // `BuiltinFn` / `VariantConstructor` tags). Function-
                        // shaped values, Channel, Handle and the Tcp resources
                        // collapse to their canonical name via
                        // `dispatch_type_name`; the descriptor values
                        // (whose canonical name is the *carried* type name)
                        // fall back to their `type_name` so the diagnostic
                        // names the descriptor kind, not the reflected type.
                        let name = match &val {
                            Value::TypeDescriptor(_) | Value::PrimitiveDescriptor(_) => {
                                self.type_name(&val).to_string()
                            }
                            _ => crate::types::canonical::dispatch_type_name(&val),
                        };
                        return Err(VmError::type_confusion(format!(
                            "type '{name}' does not implement Display \
                             (required for string interpolation)"
                        )));
                    }
                    _ => {
                        let s = self.display_value(&val);
                        self.push(Value::String(s));
                    }
                }
            }
            Instr::StringConcat { count } => {
                let start = self.stack.len() - count;
                // Pre-calculate total capacity to avoid reallocations
                let mut total_len = 0;
                for i in start..self.stack.len() {
                    if let Value::String(ref s) = self.stack[i] {
                        total_len += s.len();
                    } else {
                        return Err(VmError::type_confusion(format!(
                            "string interpolation requires string values, got {} \
                             — call `.to_string()` or `.display()` on the value first",
                            self.user_facing_type_name(&self.stack[i])
                        )));
                    }
                }
                let mut result = String::with_capacity(total_len);
                for i in start..self.stack.len() {
                    if let Value::String(ref s) = self.stack[i] {
                        result.push_str(s);
                    }
                }
                self.stack.truncate(start);
                self.push(Value::String(result));
            }
            Instr::GetLocal { slot } => {
                let base = self.frame().base_slot;
                let value = self.stack[base + slot].clone();
                self.push(value);
            }
            Instr::SetLocal { slot } => {
                let base = self.frame().base_slot;
                let value = self.peek().clone();
                self.stack[base + slot] = value;
            }
            Instr::GetGlobal { slot } => {
                let value = match self.globals.get(slot as usize) {
                    Some(Some(value)) => value.clone(),
                    // A top-level `let` initializer that calls code which
                    // reads a `let` initialized after it.
                    _ => {
                        return Err(VmError::new(format!(
                            "'{}' is used before its top-level definition has run",
                            self.global_slots.name(slot)
                        )));
                    }
                };
                self.push(value);
            }
            Instr::SetGlobal { slot } => {
                // A slot is installed the first time it is set: a VM that
                // was handed no program (a test's) has none yet.
                let slot = usize::from(slot);
                let value = self.peek().clone();
                if slot >= self.globals.len() {
                    self.globals.resize(slot + 1, None);
                }
                self.globals[slot] = Some(value);
            }
            Instr::GetUpvalue { index } => {
                let value = self.frame().closure.upvalues[index].clone();
                self.push(value);
            }
            Instr::Call { argc } => {
                let func_slot = self.stack.len() - 1 - argc;
                let func_val = self.stack[func_slot].clone();
                let entered = self.call_value(func_val, argc, func_slot)?;
                return Ok(self.entered(entered));
            }
            Instr::TailCall { argc } => {
                let func_slot = self.stack.len() - 1 - argc;
                let func_val = self.stack[func_slot].clone();
                if let Value::VmClosure(closure) = func_val {
                    if argc != closure.function.arity() {
                        return Err(VmError::type_confusion(format!(
                            "function '{}' expects {} arguments, got {}",
                            closure.function.name(),
                            closure.function.arity(),
                            argc
                        )));
                    }
                    let base = self.frame().base_slot;
                    for i in 0..argc {
                        self.stack[base + i] = self.stack[func_slot + 1 + i].clone();
                    }
                    self.stack.truncate(base + argc);
                    // Record the caller that's about to be overwritten so
                    // `enrich_error` can still surface the logical call
                    // stack for runtime errors. The caller's name comes
                    // from its closure's function name; the caller's span
                    // points at the tail-call site (the op just before
                    // `frame.ip`, which `fetch` has stepped past it).
                    // Bounded by TCO_ELIDED_CAP entries per depth — on
                    // overflow we drop the oldest caller at this depth.
                    // The existing `render_call_stack` head/tail truncation
                    // then surfaces the remaining chain with a "... (N more
                    // frames)" marker for long chains.
                    //
                    // Lock: tests/lang/callback_frame_capture_tests.rs
                    // `test_tail_call_chain_preserves_caller_frames_in_call_stack`
                    // and `test_tail_call_chain_ring_buffer_caps_diagnostic_chain`.
                    let depth = self.frames.len().saturating_sub(1);
                    let (caller_name, caller_span) = {
                        let frame = self.frame();
                        let caller_ip = frame.ip.saturating_sub(1);
                        (
                            frame.closure.function.name().to_string(),
                            frame.closure.function.chunk().span_at(caller_ip),
                        )
                    };
                    // The entries of this depth are the log's last ones.
                    let at_depth = self
                        .tco_elided
                        .iter()
                        .rev()
                        .take_while(|(d, _, _)| *d == depth)
                        .count();
                    if at_depth >= crate::vm::runtime::TCO_ELIDED_CAP {
                        self.tco_elided.remove(self.tco_elided.len() - at_depth);
                    }
                    self.tco_elided.push((depth, caller_name, caller_span));
                    let frame = self.frame_mut();
                    frame.closure = closure;
                    frame.ip = 0;
                } else {
                    let entered = self.call_value(func_val, argc, func_slot)?;
                    return Ok(self.entered(entered));
                }
            }
            Instr::Return => {
                let result = self.pop();
                return Ok(DispatchResult::Return(result));
            }
            Instr::CallBuiltin { name, argc } => {
                // The name stays where it is, in the function's constants,
                // which the function's closure keeps while the builtin
                // has the VM.
                let closure = self.frame().closure.clone();
                let name = closure.function.chunk().string(name);
                let args = self.stack.split_off(self.stack.len() - argc);
                let entered = self.enter_builtin(name, &args)?;
                return Ok(self.entered(entered));
            }
            Instr::MakeClosure { f, captures } => {
                let frame = self.frame();
                let chunk = frame.closure.function.chunk();
                let upvalues = captures
                    .iter(chunk.code())
                    .map(|capture| {
                        let index = capture.index;
                        match capture.is_local {
                            true => self.stack[frame.base_slot + index].clone(),
                            false => frame.closure.upvalues[index].clone(),
                        }
                    })
                    .collect();
                let function = chunk.closure(f).function.clone();
                self.push(Value::VmClosure(Arc::new(VmClosure { function, upvalues })));
            }
            Instr::MakeTuple { count } => {
                let start = self.stack.len() - count;
                let elements: Vec<Value> = self.stack[start..].to_vec();
                self.stack.truncate(start);
                self.push(Value::Tuple(elements));
            }
            Instr::MakeList { count } => {
                let start = self.stack.len() - count;
                let elements: Vec<Value> = self.stack[start..].to_vec();
                self.stack.truncate(start);
                self.push(Value::List(Arc::new(elements)));
            }
            Instr::MakeMap { pairs } => {
                let total = pairs * 2;
                let start = self.stack.len() - total;
                let mut map = BTreeMap::new();
                for i in (start..self.stack.len()).step_by(2) {
                    map.insert(self.stack[i].clone(), self.stack[i + 1].clone());
                }
                self.stack.truncate(start);
                self.push(Value::Map(Arc::new(map)));
            }
            Instr::MakeSet { count } => {
                let start = self.stack.len() - count;
                let mut set = BTreeSet::new();
                for i in start..self.stack.len() {
                    set.insert(self.stack[i].clone());
                }
                self.stack.truncate(start);
                self.push(Value::Set(Arc::new(set)));
            }
            Instr::MakeRecord { ty, fields } => {
                let field_names = self.names(fields);
                let ty = self.chunk().type_info(ty).clone();
                let start = self.stack.len() - field_names.len();
                let mut fields = BTreeMap::new();
                for (i, name) in field_names.into_iter().enumerate() {
                    fields.insert(name, self.stack[start + i].clone());
                }
                self.stack.truncate(start);
                self.push(Value::Record(ty, Arc::new(fields)));
            }
            Instr::RecordUpdate { fields } => {
                // Functional record update: preserves the base's
                // `type_name`. Used for both nominal `.{...}` updates
                // and `{...base, ...}` spreads. Round 83 had introduced
                // a sibling `RecordUpdateAnon` that rebranded the
                // result's `type_name` to `"<anon>"` for spread
                // expressions whose typed result was `Type::AnonRecord`
                // — round 85's follow-up removed it because round 84's
                // `Value::PartialEq` + round 85's `Value::Ord` /
                // `Value::Hash` `<anon>` wildcards close the soundness
                // gap from the other side, leaving the rebrand strictly
                // redundant.
                let field_names = self.names(fields);
                let start = self.stack.len() - field_names.len();
                let new_values: Vec<Value> = self.stack[start..].to_vec();
                self.stack.truncate(start);
                let base = self.pop();
                if let Value::Record(type_name, mut existing) = base {
                    let fields = Arc::make_mut(&mut existing);
                    for (name, val) in field_names.into_iter().zip(new_values) {
                        fields.insert(name, val);
                    }
                    self.push(Value::Record(type_name, existing));
                } else {
                    return Err(VmError::type_confusion(format!(
                        "record update `.{{...}}` requires a record, got {}",
                        self.user_facing_type_name(&base)
                    )));
                }
            }
            Instr::MakeRange => {
                let end = self.pop();
                let start = self.pop();
                if let (Value::Int(a), Value::Int(b)) = (&start, &end) {
                    self.push(Value::Range(*a, *b));
                } else {
                    return Err(VmError::type_confusion(format!(
                        "range `a..b` requires two Int operands, got {} and {}",
                        self.user_facing_type_name(&start),
                        self.user_facing_type_name(&end)
                    )));
                }
            }
            Instr::ListConcat => {
                let b = self.pop();
                let a = self.pop();
                let mut result = match a {
                    Value::List(xs) => xs.as_ref().clone(),
                    Value::Range(lo, hi) => {
                        checked_range_len(lo, hi).map_err(VmError::new)?;
                        (lo..=hi).map(Value::Int).collect()
                    }
                    _ => {
                        return Err(VmError::type_confusion(
                            "ListConcat: left operand is not a list or range",
                        ));
                    }
                };
                // Pre-check combined size BEFORE materializing `b` to avoid
                // allocating ~800MB when two near-limit operands are concatenated.
                let b_len = match &b {
                    Value::List(xs) => xs.len(),
                    Value::Range(lo, hi) => checked_range_len(*lo, *hi).map_err(VmError::new)?,
                    _ => {
                        return Err(VmError::type_confusion(
                            "ListConcat: right operand is not a list or range",
                        ));
                    }
                };
                if result.len() + b_len > MAX_RANGE_MATERIALIZE {
                    return Err(VmError::new(format!(
                        "concatenated list exceeds maximum size of {} elements",
                        MAX_RANGE_MATERIALIZE
                    )));
                }
                match b {
                    Value::List(xs) => result.extend(xs.iter().cloned()),
                    Value::Range(lo, hi) => {
                        result.extend((lo..=hi).map(Value::Int));
                    }
                    _ => unreachable!(),
                }
                self.push(Value::List(Arc::new(result)));
            }
            Instr::GetField { name } => {
                let name = self.chunk().string(name).to_owned();
                let target = self.pop();
                match target {
                    Value::Record(_, ref fields) => {
                        let val = fields.get(&name).cloned().ok_or_else(|| {
                            VmError::type_confusion(format!("record has no field '{name}'"))
                        })?;
                        self.push(val);
                    }
                    Value::Map(ref map) => {
                        let val = map
                            .get(&Value::String(name.clone()))
                            .cloned()
                            .ok_or_else(|| VmError::new(format!("map has no key '{name}'")))?;
                        self.push(val);
                    }
                    other => {
                        return Err(VmError::type_confusion(format!(
                            "cannot access field '{}' on {}",
                            name,
                            self.user_facing_type_name(&other)
                        )));
                    }
                }
            }
            Instr::Jump { to } => {
                self.frame_mut().ip = to;
            }
            Instr::JumpIfFalse { to } => {
                let val = self.pop();
                if self.is_falsy(&val) {
                    self.frame_mut().ip = to;
                }
            }
            Instr::JumpIfTrue { to } => {
                let val = self.pop();
                if self.is_truthy(&val) {
                    self.frame_mut().ip = to;
                }
            }
            Instr::Pop => {
                self.pop();
            }
            Instr::Dup => {
                let val = self.peek().clone();
                self.push(val);
            }
            Instr::TestTag { tag } => {
                let expected = self.chunk().tag(tag);
                let result = matches!(self.peek(), Value::Variant(tag, _) if tag == expected);
                self.push(Value::Bool(result));
            }
            Instr::TestEqual { k } => {
                let result = self.peek() == self.chunk().constant(k);
                self.push(Value::Bool(result));
            }
            Instr::TestTupleLen { len } => {
                let val = self.peek();
                let result = matches!(val, Value::Tuple(elems) if elems.len() == len);
                self.push(Value::Bool(result));
            }
            Instr::TestListMin { len: min_len } => {
                let val = self.peek();
                let result = val.collection_len().is_some_and(|len| len >= min_len);
                self.push(Value::Bool(result));
            }
            Instr::TestListExact { len } => {
                let val = self.peek();
                let result = val.collection_len() == Some(len);
                self.push(Value::Bool(result));
            }
            Instr::TestIntRange { lo, hi } => {
                let chunk = self.chunk();
                let result = match (self.peek(), chunk.constant(lo), chunk.constant(hi)) {
                    (Value::Int(n), Value::Int(lo), Value::Int(hi)) => *n >= *lo && *n <= *hi,
                    _ => false,
                };
                self.push(Value::Bool(result));
            }
            Instr::TestFloatRange { lo, hi } => {
                let chunk = self.chunk();
                let result = match (self.peek(), chunk.constant(lo), chunk.constant(hi)) {
                    (Value::Float(n), Value::Float(lo), Value::Float(hi)) => *n >= *lo && *n <= *hi,
                    _ => false,
                };
                self.push(Value::Bool(result));
            }
            Instr::DestructTuple { index } => {
                let val = self.peek().clone();
                if let Value::Tuple(elems) = val {
                    let elem = elems.get(index).ok_or_else(|| {
                        VmError::type_confusion(format!(
                            "tuple destructure: expected at least {} elements, got {}",
                            index + 1,
                            elems.len()
                        ))
                    })?;
                    self.push(elem.clone());
                } else {
                    return Err(VmError::type_confusion(format!(
                        "tuple destructure: expected tuple, got {}",
                        self.user_facing_type_name(&val)
                    )));
                }
            }
            Instr::DestructVariant { index } => {
                let val = self.peek().clone();
                if let Value::Variant(_, fields) = val {
                    let field = fields.get(index).ok_or_else(|| {
                        VmError::type_confusion(format!(
                            "variant destructure: field index {} out of bounds (variant has {} fields)",
                            index,
                            fields.len()
                        ))
                    })?;
                    self.push(field.clone());
                } else {
                    return Err(VmError::type_confusion(format!(
                        "variant destructure: expected variant, got {}",
                        self.user_facing_type_name(&val)
                    )));
                }
            }
            Instr::DestructList { index } => {
                let val = self.peek().clone();
                match val {
                    Value::List(ref xs) => {
                        let elem = xs.get(index).ok_or_else(|| {
                            VmError::type_confusion(format!(
                                "list destructure: expected at least {} elements, got {}",
                                index + 1,
                                xs.len()
                            ))
                        })?;
                        self.push(elem.clone());
                    }
                    Value::Range(lo, hi) => {
                        let i = lo
                            .checked_add(index as i64)
                            .ok_or_else(|| VmError::type_confusion("range index overflow"))?;
                        if i > hi {
                            return Err(VmError::type_confusion("range index out of bounds"));
                        }
                        self.push(Value::Int(i));
                    }
                    _ => {
                        return Err(VmError::type_confusion(format!(
                            "list destructure: expected list, got {}",
                            self.user_facing_type_name(&val)
                        )));
                    }
                }
            }
            Instr::DestructListRest { start } => {
                let val = self.peek().clone();
                match val {
                    Value::List(ref xs) => {
                        if start > xs.len() {
                            return Err(VmError::type_confusion(format!(
                                "list destructure: rest pattern start {} exceeds list length {}",
                                start,
                                xs.len()
                            )));
                        }
                        self.push(Value::List(Arc::new(xs[start..].to_vec())));
                    }
                    Value::Range(lo, hi) => {
                        let new_lo = lo
                            .checked_add(start as i64)
                            .ok_or_else(|| VmError::type_confusion("range index overflow"))?;
                        let exceeds = match hi.checked_add(1) {
                            Some(hi_plus_1) => new_lo > hi_plus_1,
                            None => false, // hi == i64::MAX; new_lo can never exceed hi+1
                        };
                        if exceeds {
                            self.push(Value::List(Arc::new(Vec::new())));
                        } else {
                            self.push(Value::Range(new_lo, hi));
                        }
                    }
                    _ => {
                        return Err(VmError::type_confusion(format!(
                            "list destructure: expected list, got {}",
                            self.user_facing_type_name(&val)
                        )));
                    }
                }
            }
            Instr::DestructRecordField { name } => {
                let name = self.chunk().string(name).to_owned();
                let val = self.peek().clone();
                if let Value::Record(_, fields) = val {
                    let field = fields.get(&name).cloned().ok_or_else(|| {
                        VmError::type_confusion(format!("record has no field '{name}'"))
                    })?;
                    self.push(field);
                } else {
                    return Err(VmError::type_confusion(format!(
                        "record destructure: expected record, got {}",
                        self.user_facing_type_name(&val)
                    )));
                }
            }
            Instr::DestructRecordRest { excluded } => {
                let excluded = self.names(excluded);
                let val = self.pop();
                if let Value::Record(_, fields) = val {
                    let mut rest_fields: std::collections::BTreeMap<String, Value> =
                        std::collections::BTreeMap::new();
                    for (k, v) in fields.iter() {
                        if !excluded.iter().any(|e| e == k) {
                            rest_fields.insert(k.clone(), v.clone());
                        }
                    }
                    self.push(Value::builtin_record(
                        crate::typeinfo::ty::ANON_RECORD,
                        rest_fields,
                    ));
                } else {
                    return Err(VmError::type_confusion(format!(
                        "record rest destructure: expected record, got {}",
                        self.user_facing_type_name(&val)
                    )));
                }
            }
            Instr::TestRecordTag { ty } => {
                let expected = self.chunk().type_info(ty).id;
                let result = matches!(self.peek(), Value::Record(ty, _) if record_type_matches(ty, expected));
                self.push(Value::Bool(result));
            }
            Instr::TestMapHasKey { key } => {
                let key_name = self.chunk().string(key).to_owned();
                let val = self.peek();
                let result = match val {
                    Value::Map(map) => map.contains_key(&Value::String(key_name)),
                    _ => false,
                };
                self.push(Value::Bool(result));
            }
            Instr::DestructMapValue { key } => {
                let key_name = self.chunk().string(key).to_owned();
                let val = self.peek().clone();
                if let Value::Map(map) = val {
                    let value = map
                        .get(&Value::String(key_name.clone()))
                        .cloned()
                        .ok_or_else(|| {
                            VmError::type_confusion(format!("map has no key '{key_name}'"))
                        })?;
                    self.push(value);
                } else {
                    return Err(VmError::type_confusion(format!(
                        "map destructure: expected map, got {}",
                        self.user_facing_type_name(&val)
                    )));
                }
            }
            Instr::Recur {
                argc: arg_count,
                first: first_slot,
            } => {
                let base = self.frame().base_slot;
                let start = self.stack.len() - arg_count;
                for i in 0..arg_count {
                    self.stack[base + first_slot + i] = self.stack[start + i].clone();
                }
                // Truncate all the way back to just after loop bindings.
                self.stack.truncate(base + first_slot + arg_count);
            }
            Instr::QuestionMark => {
                let val = self.peek().clone();
                match val {
                    Value::Variant(ref tag, ref fields) => match tag {
                        _ if tag.is(bv::OK) || tag.is(bv::SOME) => {
                            self.pop();
                            self.push(if fields.len() == 1 {
                                fields[0].clone()
                            } else {
                                Value::Unit
                            });
                        }
                        _ if tag.is(bv::ERR) || tag.is(bv::NONE) => {
                            let value = self.pop();
                            return Ok(DispatchResult::Return(value));
                        }
                        _ => {
                            return Err(VmError::type_confusion(format!(
                                "`?` applies only to Result or Option; got variant `{tag}`"
                            )));
                        }
                    },
                    _ => {
                        return Err(VmError::type_confusion(format!(
                            "`?` applies only to Result or Option; got {}",
                            self.user_facing_type_name(&val)
                        )));
                    }
                }
            }
            Instr::Panic => {
                let msg = self.pop();
                return Err(VmError::new(format!("panic: {}", self.display_value(&msg))));
            }
            Instr::CallMethod {
                method,
                argc,
                of: trait_index,
            } => {
                // As for a builtin's name: the method's stays in the
                // constants.
                let closure = self.frame().closure.clone();
                let method_name = closure.function.chunk().string(method);
                let receiver_slot = self.stack.len() - argc;
                let receiver = self.stack[receiver_slot].clone();
                let receiver_type = crate::types::canonical::dispatch_type_for_value(&receiver);
                // Descriptor-as-receiver (e.g. `Int.default()`,
                // `body.decode(Todo)` where the descriptor is piped in) is
                // a dispatch key, not a value argument. The method's
                // compiled body never has a slot for it — skip it when
                // assembling the argument vector.
                let descriptor_receiver = matches!(
                    &receiver,
                    Value::TypeDescriptor(_) | Value::PrimitiveDescriptor(_)
                );
                if trait_index == crate::bytecode::NO_TRAIT
                    && self.global_slots.ambiguous(
                        receiver_type,
                        method_name,
                        !matches!(receiver, Value::Record(..) | Value::Variant(..)),
                    )
                {
                    return Err(VmError::new(format!(
                        "ambiguous method '{method_name}' for type '{}': two traits provide it, \
                         and this call names neither; add a `where` bound for the receiver",
                        crate::types::canonical::dispatch_type_name(&receiver)
                    )));
                }
                let method = self
                    .global_slots
                    .call_method(trait_index, receiver_type, method_name)
                    .and_then(|slot| self.globals.get(slot as usize).cloned().flatten());
                if let Some(func) = method {
                    // The method's frame starts above a slot of its own,
                    // as a called function's does: the descriptor's, or
                    // one made below the receiver.
                    if !descriptor_receiver {
                        self.stack.insert(receiver_slot, Value::Unit);
                    }
                    let argc = self.stack.len() - receiver_slot - 1;
                    let entered = self.call_value(func, argc, receiver_slot)?;
                    return Ok(self.entered(entered));
                }
                // Try built-in trait methods (display, equal, compare)
                if let Some(result) = self.dispatch_trait_method(
                    &receiver,
                    method_name,
                    &self.stack[receiver_slot + 1..],
                ) {
                    self.stack.truncate(receiver_slot);
                    self.push(result?);
                } else if let Value::Record(_, ref fields) = receiver
                    && let Some(callable) = fields.get(method_name).cloned()
                {
                    // A record's field that holds a function: the
                    // receiver's slot is the function's.
                    let argc = self.stack.len() - receiver_slot - 1;
                    let entered = self.call_value(callable, argc, receiver_slot)?;
                    return Ok(self.entered(entered));
                } else {
                    return Err(VmError::type_confusion(format!(
                        "no method '{method_name}' for type '{}'",
                        crate::types::canonical::dispatch_type_name(&receiver)
                    )));
                }
            }
            Instr::Slide { slot } => {
                // Keep the top value, cut the frame back to `slot` values
                // and put the value on top of them.
                let base = self.frame().base_slot;
                let value = self.pop();
                let target = base + slot;
                self.stack.truncate(target);
                self.push(value);
            }
        }
        Ok(DispatchResult::Continue)
    }
}
