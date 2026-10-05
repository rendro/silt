//! Main execution loop and opcode dispatch.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::bytecode::{Op, VmClosure, record_type_matches};
use crate::scheduler::SliceResult;
use crate::typeinfo::bv;
use crate::value::{MAX_RANGE_MATERIALIZE, Value, checked_range_len};

use super::calls::enter_native_level;
use super::{Vm, VmError};

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

/// Result of dispatching a single opcode.
pub(super) enum DispatchResult {
    /// Normal execution; continue to next opcode.
    Continue,
    /// Op::Return was executed. The return value is provided.
    /// The frame has NOT been popped — the caller must do that.
    Return(Value),
    /// Op::QuestionMark hit Err/None. The frame HAS been popped.
    /// The value and the finished frame's base_slot are provided.
    /// The caller must handle stack cleanup.
    EarlyReturn { value: Value, finished_base: usize },
}

impl Vm {
    // ── Main execution loop ───────────────────────────────────────

    pub(crate) fn execute(&mut self) -> Result<Value, VmError> {
        // Usually the outermost loop of its thread, but not always: a
        // target without threads runs a spawned task's `execute` inside
        // the `task.spawn` call of its parent.
        let _native_level = enter_native_level()?;
        loop {
            let op_byte = self.read_byte()?;
            let op = Op::from_byte(op_byte)
                .ok_or_else(|| VmError::new(format!("unknown opcode: {op_byte}")))?;
            match self.dispatch_one(op)? {
                DispatchResult::Continue => {}
                DispatchResult::Return(result) => {
                    let finished_base = self.current_frame()?.base_slot;
                    self.frames.pop();
                    // Prune any tail-call elided diagnostic entries that
                    // belong to the just-popped frame slot so stale data
                    // can't bleed into later unrelated calls at this depth.
                    let keep = self.frames.len();
                    self.prune_tco_elided(keep);
                    if self.frames.is_empty() {
                        return Ok(result);
                    }
                    let func_slot = finished_base.saturating_sub(1);
                    self.stack.truncate(func_slot);
                    self.push(result);
                }
                DispatchResult::EarlyReturn {
                    value,
                    finished_base,
                } => {
                    // EarlyReturn from `?` already popped its frame in
                    // Op::QuestionMark; prune tco_elided to match.
                    let keep = self.frames.len();
                    self.prune_tco_elided(keep);
                    if self.frames.is_empty() {
                        return Ok(value);
                    }
                    let func_slot = finished_base.saturating_sub(1);
                    self.stack.truncate(func_slot);
                    self.push(value);
                }
            }
        }
    }

    // ── Sliced execution (for M:N scheduler) ─────────────────────

    /// Run up to `max_steps` instructions and return a `SliceResult`.
    /// Used by the M:N scheduler's worker threads.
    pub fn execute_slice(&mut self, max_steps: usize) -> SliceResult {
        // Helper macro to convert Result to SliceResult::Failed on error.
        macro_rules! try_or_fail {
            ($expr:expr) => {
                match $expr {
                    Ok(v) => v,
                    Err(e) => return SliceResult::Failed(e),
                }
            };
        }
        for _ in 0..max_steps {
            if self.frames.is_empty() {
                let result = if self.stack.is_empty() {
                    Value::Unit
                } else {
                    self.stack.last().cloned().unwrap_or(Value::Unit)
                };
                return SliceResult::Completed(result);
            }
            let saved_ip = try_or_fail!(self.current_frame()).ip;
            let op_byte = try_or_fail!(self.read_byte());
            let op = match Op::from_byte(op_byte) {
                Some(op) => op,
                None => {
                    return SliceResult::Failed(VmError::new(format!("unknown opcode: {op_byte}")));
                }
            };
            match self.dispatch_one(op) {
                Ok(DispatchResult::Continue) => {}
                Ok(DispatchResult::Return(result)) => {
                    let finished_base = try_or_fail!(self.current_frame()).base_slot;
                    self.frames.pop();
                    let keep = self.frames.len();
                    self.prune_tco_elided(keep);
                    if self.frames.is_empty() {
                        return SliceResult::Completed(result);
                    }
                    let func_slot = finished_base.saturating_sub(1);
                    self.stack.truncate(func_slot);
                    self.push(result);
                }
                Ok(DispatchResult::EarlyReturn {
                    value,
                    finished_base,
                }) => {
                    let keep = self.frames.len();
                    self.prune_tco_elided(keep);
                    if self.frames.is_empty() {
                        return SliceResult::Completed(value);
                    }
                    let func_slot = finished_base.saturating_sub(1);
                    self.stack.truncate(func_slot);
                    self.push(value);
                }
                Err(e) if e.is_yield => {
                    try_or_fail!(self.current_frame_mut()).ip = saved_ip;
                    if self.block_reason.is_some() {
                        return SliceResult::Blocked;
                    }
                    return SliceResult::Yielded;
                }
                Err(e) => return SliceResult::Failed(e),
            }
            if self.block_reason.is_some() {
                return SliceResult::Blocked;
            }
        }
        // Time slice expired.
        SliceResult::Yielded
    }

    /// Dispatch a single opcode. All three execution loops call this.
    pub(super) fn dispatch_one(&mut self, op: Op) -> Result<DispatchResult, VmError> {
        match op {
            Op::Constant => {
                let index = self.read_u16()? as usize;
                let value = self.read_constant(index)?;
                self.push(value);
            }
            Op::Unit => self.push(Value::Unit),
            Op::True => self.push(Value::Bool(true)),
            Op::False => self.push(Value::Bool(false)),
            Op::Add => self.binary_arithmetic(Op::Add)?,
            Op::Sub => self.binary_arithmetic(Op::Sub)?,
            Op::Mul => self.binary_arithmetic(Op::Mul)?,
            Op::Div => self.binary_arithmetic(Op::Div)?,
            Op::Mod => self.binary_arithmetic(Op::Mod)?,
            Op::Eq => {
                let b = self.pop()?;
                let a = self.pop()?;
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
                    return Err(VmError::new(format!(
                        "type '{name}' does not implement Equal"
                    )));
                }
                self.push(Value::Bool(a == b));
            }
            Op::Neq => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.check_same_type(&a, &b)?;
                if let Some(name) =
                    equality_operand_violation(&a).or_else(|| equality_operand_violation(&b))
                {
                    return Err(VmError::new(format!(
                        "type '{name}' does not implement Equal"
                    )));
                }
                self.push(Value::Bool(a != b));
            }
            Op::Lt => self.compare(|ord| ord.is_lt())?,
            Op::Gt => self.compare(|ord| ord.is_gt())?,
            Op::Leq => self.compare(|ord| ord.is_le())?,
            Op::Geq => self.compare(|ord| ord.is_ge())?,
            Op::Negate => {
                let val = self.pop()?;
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
                        return Err(VmError::new(format!(
                            "cannot negate {}",
                            self.user_facing_type_name(&other)
                        )));
                    }
                }
            }
            Op::Not => {
                let val = self.pop()?;
                match val {
                    Value::Bool(b) => self.push(Value::Bool(!b)),
                    other => {
                        return Err(VmError::new(format!(
                            "cannot apply 'not' to {}",
                            self.user_facing_type_name(&other)
                        )));
                    }
                }
            }
            Op::DisplayValue => {
                let val = self.pop()?;
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
                        return Err(VmError::new(format!(
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
            Op::StringConcat => {
                let count = self.read_u8()? as usize;
                if count > self.stack.len() {
                    return Err(VmError::new(format!(
                        "internal VM error: string interpolation expects {} values but only {} are available",
                        count,
                        self.stack.len()
                    )));
                }
                let start = self.stack.len() - count;
                // Pre-calculate total capacity to avoid reallocations
                let mut total_len = 0;
                for i in start..self.stack.len() {
                    if let Value::String(ref s) = self.stack[i] {
                        total_len += s.len();
                    } else {
                        return Err(VmError::new(format!(
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
            Op::GetLocal => {
                let slot = self.read_u16()? as usize;
                let base = self.current_frame()?.base_slot;
                let value = self
                    .stack
                    .get(base + slot)
                    .ok_or_else(|| {
                        VmError::new(format!(
                            "stack index out of bounds (slot {slot}, base {base}, stack len {})",
                            self.stack.len()
                        ))
                    })?
                    .clone();
                self.push(value);
            }
            Op::SetLocal => {
                let slot = self.read_u16()? as usize;
                let base = self.current_frame()?.base_slot;
                let value = self.peek()?.clone();
                let target = base + slot;
                if target >= self.stack.len() {
                    return Err(VmError::new(format!(
                        "internal VM error: local binding slot out of range (slot {slot}, base {base}, stack len {})",
                        self.stack.len()
                    )));
                }
                self.stack[target] = value;
            }
            Op::GetGlobal => {
                let slot = self.read_u16()?;
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
            Op::SetGlobal => {
                let slot = self.read_u16()? as usize;
                let value = self.peek()?.clone();
                let Some(global) = self.globals.get_mut(slot) else {
                    return Err(VmError::new(format!(
                        "internal VM error: global slot {slot} out of range ({} slots)",
                        self.globals.len()
                    )));
                };
                *global = Some(value);
            }
            Op::GetUpvalue => {
                let index = self.read_u8()? as usize;
                let upvalues = &self.current_frame()?.closure.upvalues;
                let value = upvalues
                    .get(index)
                    .ok_or_else(|| {
                        VmError::new(format!(
                            "upvalue index {index} out of bounds (count {})",
                            upvalues.len()
                        ))
                    })?
                    .clone();
                self.push(value);
            }
            Op::Call => {
                let argc = self.read_u8()? as usize;
                if argc + 1 > self.stack.len() {
                    return Err(VmError::new(format!(
                        "call: argc {argc} exceeds stack size {}",
                        self.stack.len()
                    )));
                }
                let func_slot = self.stack.len() - 1 - argc;
                let func_val = self.stack[func_slot].clone();
                self.call_value(func_val, argc, func_slot)?;
            }
            Op::TailCall => {
                let argc = self.read_u8()? as usize;
                if argc + 1 > self.stack.len() {
                    return Err(VmError::new(format!(
                        "tail call: argc {argc} exceeds stack size {}",
                        self.stack.len()
                    )));
                }
                let func_slot = self.stack.len() - 1 - argc;
                let func_val = self.stack[func_slot].clone();
                if let Value::VmClosure(closure) = func_val {
                    if argc != closure.function.arity as usize {
                        return Err(VmError::new(format!(
                            "function '{}' expects {} arguments, got {}",
                            closure.function.name, closure.function.arity, argc
                        )));
                    }
                    let base = self.current_frame()?.base_slot;
                    if base + argc > self.stack.len() {
                        return Err(VmError::new(format!(
                            "tail call: destination slot out of bounds (base {base}, argc {argc}, stack len {})",
                            self.stack.len()
                        )));
                    }
                    for i in 0..argc {
                        self.stack[base + i] = self.stack[func_slot + 1 + i].clone();
                    }
                    self.stack.truncate(base + argc);
                    // Record the caller that's about to be overwritten so
                    // `enrich_error` can still surface the logical call
                    // stack for runtime errors. The caller's name comes
                    // from its closure's function name; the caller's span
                    // points at the tail-call site (the op just before
                    // `frame.ip`, which is pre-advanced by `read_u8`).
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
                        let frame = self.current_frame()?;
                        let caller_ip = frame.ip.saturating_sub(1);
                        (
                            frame.closure.function.name.clone(),
                            frame.closure.function.chunk.span_at(caller_ip),
                        )
                    };
                    let count_at_depth = self
                        .tco_elided
                        .iter()
                        .filter(|(d, _, _)| *d == depth)
                        .count();
                    if count_at_depth >= crate::vm::runtime::TCO_ELIDED_CAP
                        && let Some(pos) = self.tco_elided.iter().position(|(d, _, _)| *d == depth)
                    {
                        self.tco_elided.remove(pos);
                    }
                    self.tco_elided.push((depth, caller_name, caller_span));
                    let frame = self.current_frame_mut()?;
                    frame.closure = closure;
                    frame.ip = 0;
                } else {
                    self.call_value(func_val, argc, func_slot)?;
                }
            }
            Op::Return => {
                let result = self.pop()?;
                return Ok(DispatchResult::Return(result));
            }
            Op::CallBuiltin => {
                let name_index = self.read_u16()? as usize;
                let argc = self.read_u8()? as usize;
                let name = self.read_constant_string(name_index)?;
                if argc > self.stack.len() {
                    return Err(VmError::new(format!(
                        "call builtin '{name}': argc {argc} exceeds stack size {}",
                        self.stack.len()
                    )));
                }
                let start = self.stack.len() - argc;
                let args: Vec<Value> = self.stack[start..].to_vec();
                self.stack.truncate(start);
                match self.dispatch_builtin(&name, &args) {
                    Ok(result) => {
                        self.push(result);
                    }
                    Err(e) if e.is_yield => {
                        // Args already re-pushed by the builtin before yielding
                        return Err(e);
                    }
                    Err(e) => return Err(e),
                }
            }
            Op::MakeClosure => {
                let func_index = self.read_u16()? as usize;
                let upvalue_count = self.read_u8()? as usize;
                let constant = self.read_constant(func_index)?;
                let mut upvalues = Vec::with_capacity(upvalue_count);
                for _ in 0..upvalue_count {
                    let is_local = self.read_u8()? != 0;
                    let index = self.read_u8()? as usize;
                    let val = if is_local {
                        let base = self.current_frame()?.base_slot;
                        self.stack.get(base + index)
                            .ok_or_else(|| VmError::new(format!(
                                "closure capture: stack index out of bounds (index {index}, base {base}, stack len {})",
                                self.stack.len()
                            )))?
                            .clone()
                    } else {
                        let upvalues = &self.current_frame()?.closure.upvalues;
                        upvalues.get(index)
                            .ok_or_else(|| VmError::new(format!(
                                "closure capture: upvalue index {index} out of bounds (count {})",
                                upvalues.len()
                            )))?
                            .clone()
                    };
                    upvalues.push(val);
                }
                if let Value::VmClosure(existing) = constant {
                    let closure = Arc::new(VmClosure {
                        function: existing.function.clone(),
                        upvalues,
                    });
                    self.push(Value::VmClosure(closure));
                } else {
                    return Err(VmError::new(
                        "internal VM error: closure construction constant is not a closure"
                            .to_string(),
                    ));
                }
            }
            Op::MakeTuple => {
                let count = self.read_u8()? as usize;
                if count > self.stack.len() {
                    return Err(VmError::new(format!(
                        "internal VM error: tuple construction count {count} exceeds stack size {}",
                        self.stack.len()
                    )));
                }
                let start = self.stack.len() - count;
                let elements: Vec<Value> = self.stack[start..].to_vec();
                self.stack.truncate(start);
                self.push(Value::Tuple(elements));
            }
            Op::MakeList => {
                let count = self.read_u16()? as usize;
                if count > self.stack.len() {
                    return Err(VmError::new(format!(
                        "internal VM error: list construction count {count} exceeds stack size {}",
                        self.stack.len()
                    )));
                }
                let start = self.stack.len() - count;
                let elements: Vec<Value> = self.stack[start..].to_vec();
                self.stack.truncate(start);
                self.push(Value::List(Arc::new(elements)));
            }
            Op::MakeMap => {
                let pair_count = self.read_u16()? as usize;
                let total = pair_count * 2;
                if total > self.stack.len() {
                    return Err(VmError::new(format!(
                        "internal VM error: map construction needs {total} values but stack has {}",
                        self.stack.len()
                    )));
                }
                let start = self.stack.len() - total;
                let mut map = BTreeMap::new();
                for i in (start..self.stack.len()).step_by(2) {
                    map.insert(self.stack[i].clone(), self.stack[i + 1].clone());
                }
                self.stack.truncate(start);
                self.push(Value::Map(Arc::new(map)));
            }
            Op::MakeSet => {
                let count = self.read_u16()? as usize;
                if count > self.stack.len() {
                    return Err(VmError::new(format!(
                        "internal VM error: set construction count {count} exceeds stack size {}",
                        self.stack.len()
                    )));
                }
                let start = self.stack.len() - count;
                let mut set = BTreeSet::new();
                for i in start..self.stack.len() {
                    set.insert(self.stack[i].clone());
                }
                self.stack.truncate(start);
                self.push(Value::Set(Arc::new(set)));
            }
            Op::MakeRecord => {
                let type_name_index = self.read_u16()? as usize;
                let field_count = self.read_u8()? as usize;
                let mut field_names = Vec::with_capacity(field_count);
                for _ in 0..field_count {
                    let name_index = self.read_u16()? as usize;
                    field_names.push(self.read_constant_string(name_index)?);
                }
                let ty = self.read_constant_type(type_name_index)?;
                if field_count > self.stack.len() {
                    return Err(VmError::new(format!(
                        "MakeRecord: field count {field_count} exceeds stack size {}",
                        self.stack.len()
                    )));
                }
                let start = self.stack.len() - field_count;
                let mut fields = BTreeMap::new();
                for (i, name) in field_names.into_iter().enumerate() {
                    fields.insert(name, self.stack[start + i].clone());
                }
                self.stack.truncate(start);
                self.push(Value::Record(ty, Arc::new(fields)));
            }
            Op::RecordUpdate => {
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
                let field_count = self.read_u8()? as usize;
                let mut field_names = Vec::with_capacity(field_count);
                for _ in 0..field_count {
                    let ni = self.read_u16()? as usize;
                    field_names.push(self.read_constant_string(ni)?);
                }
                if field_count > self.stack.len() {
                    return Err(VmError::new(format!(
                        "RecordUpdate: field count {field_count} exceeds stack size {}",
                        self.stack.len()
                    )));
                }
                let start = self.stack.len() - field_count;
                let new_values: Vec<Value> = self.stack[start..].to_vec();
                self.stack.truncate(start);
                let base = self.pop()?;
                if let Value::Record(type_name, mut existing) = base {
                    let fields = Arc::make_mut(&mut existing);
                    for (name, val) in field_names.into_iter().zip(new_values) {
                        fields.insert(name, val);
                    }
                    self.push(Value::Record(type_name, existing));
                } else {
                    return Err(VmError::new(format!(
                        "record update `.{{...}}` requires a record, got {}",
                        self.user_facing_type_name(&base)
                    )));
                }
            }
            Op::MakeRange => {
                let end = self.pop()?;
                let start = self.pop()?;
                if let (Value::Int(a), Value::Int(b)) = (&start, &end) {
                    self.push(Value::Range(*a, *b));
                } else {
                    return Err(VmError::new(format!(
                        "range `a..b` requires two Int operands, got {} and {}",
                        self.user_facing_type_name(&start),
                        self.user_facing_type_name(&end)
                    )));
                }
            }
            Op::ListConcat => {
                let b = self.pop()?;
                let a = self.pop()?;
                let mut result = match a {
                    Value::List(xs) => xs.as_ref().clone(),
                    Value::Range(lo, hi) => {
                        checked_range_len(lo, hi).map_err(VmError::new)?;
                        (lo..=hi).map(Value::Int).collect()
                    }
                    _ => {
                        return Err(VmError::new(
                            "ListConcat: left operand is not a list or range".into(),
                        ));
                    }
                };
                // Pre-check combined size BEFORE materializing `b` to avoid
                // allocating ~800MB when two near-limit operands are concatenated.
                let b_len = match &b {
                    Value::List(xs) => xs.len(),
                    Value::Range(lo, hi) => checked_range_len(*lo, *hi).map_err(VmError::new)?,
                    _ => {
                        return Err(VmError::new(
                            "ListConcat: right operand is not a list or range".into(),
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
            Op::GetField => {
                let name_index = self.read_u16()? as usize;
                let name = self.read_constant_string(name_index)?;
                let target = self.pop()?;
                match target {
                    Value::Record(_, ref fields) => {
                        let val = fields
                            .get(&name)
                            .cloned()
                            .ok_or_else(|| VmError::new(format!("record has no field '{name}'")))?;
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
                        return Err(VmError::new(format!(
                            "cannot access field '{}' on {}",
                            name,
                            self.user_facing_type_name(&other)
                        )));
                    }
                }
            }
            Op::Jump => {
                let offset = self.read_u16()? as usize;
                self.current_frame_mut()?.ip += offset;
            }
            Op::JumpBack => {
                let offset = self.read_u16()? as usize;
                let frame = self.current_frame_mut()?;
                frame.ip = frame.ip.checked_sub(offset).ok_or_else(|| {
                    VmError::new("jump back offset exceeds current instruction pointer".to_string())
                })?;
            }
            Op::JumpIfFalse => {
                let offset = self.read_u16()? as usize;
                let val = self.pop()?;
                if self.is_falsy(&val) {
                    self.current_frame_mut()?.ip += offset;
                }
            }
            Op::JumpIfTrue => {
                let offset = self.read_u16()? as usize;
                let val = self.pop()?;
                if self.is_truthy(&val) {
                    self.current_frame_mut()?.ip += offset;
                }
            }
            Op::Pop => {
                self.pop()?;
            }
            Op::Dup => {
                let val = self.peek()?.clone();
                self.push(val);
            }
            Op::TestTag => {
                let ni = self.read_u16()? as usize;
                let expected = self.read_constant_tag(ni)?;
                let val = self.peek()?;
                let result = matches!(val, Value::Variant(tag, _) if *tag == expected);
                self.push(Value::Bool(result));
            }
            Op::TestEqual => {
                let ci = self.read_u16()? as usize;
                let constant = self.read_constant(ci)?;
                let val = self.peek()?;
                let result = *val == constant;
                self.push(Value::Bool(result));
            }
            Op::TestTupleLen => {
                let len = self.read_u8()? as usize;
                let val = self.peek()?;
                let result = matches!(val, Value::Tuple(elems) if elems.len() == len);
                self.push(Value::Bool(result));
            }
            Op::TestListMin => {
                let min_len = self.read_u8()? as usize;
                let val = self.peek()?;
                let result = val.collection_len().is_some_and(|len| len >= min_len);
                self.push(Value::Bool(result));
            }
            Op::TestListExact => {
                let len = self.read_u8()? as usize;
                let val = self.peek()?;
                let result = val.collection_len() == Some(len);
                self.push(Value::Bool(result));
            }
            Op::TestIntRange => {
                let lo_index = self.read_u16()? as usize;
                let hi_index = self.read_u16()? as usize;
                let lo = self.read_constant(lo_index)?;
                let hi = self.read_constant(hi_index)?;
                let val = self.peek()?;
                let result = match (val, &lo, &hi) {
                    (Value::Int(n), Value::Int(lo), Value::Int(hi)) => *n >= *lo && *n <= *hi,
                    _ => false,
                };
                self.push(Value::Bool(result));
            }
            Op::TestFloatRange => {
                let lo_index = self.read_u16()? as usize;
                let hi_index = self.read_u16()? as usize;
                let lo = self.read_constant(lo_index)?;
                let hi = self.read_constant(hi_index)?;
                let val = self.peek()?;
                let result = match (val, &lo, &hi) {
                    (Value::Float(n), Value::Float(lo), Value::Float(hi)) => *n >= *lo && *n <= *hi,
                    _ => false,
                };
                self.push(Value::Bool(result));
            }
            Op::TestBool => {
                let expected = self.read_u8()? != 0;
                let val = self.peek()?;
                let result = matches!(val, Value::Bool(b) if *b == expected);
                self.push(Value::Bool(result));
            }
            Op::DestructTuple => {
                let index = self.read_u8()? as usize;
                let val = self.peek()?.clone();
                if let Value::Tuple(elems) = val {
                    let elem = elems.get(index).ok_or_else(|| {
                        VmError::new(format!(
                            "tuple destructure: expected at least {} elements, got {}",
                            index + 1,
                            elems.len()
                        ))
                    })?;
                    self.push(elem.clone());
                } else {
                    return Err(VmError::new(format!(
                        "tuple destructure: expected tuple, got {}",
                        self.user_facing_type_name(&val)
                    )));
                }
            }
            Op::DestructVariant => {
                let index = self.read_u8()? as usize;
                let val = self.peek()?.clone();
                if let Value::Variant(_, fields) = val {
                    let field = fields.get(index).ok_or_else(|| {
                        VmError::new(format!(
                            "variant destructure: field index {} out of bounds (variant has {} fields)",
                            index,
                            fields.len()
                        ))
                    })?;
                    self.push(field.clone());
                } else {
                    return Err(VmError::new(format!(
                        "variant destructure: expected variant, got {}",
                        self.user_facing_type_name(&val)
                    )));
                }
            }
            Op::DestructList => {
                let index = self.read_u8()? as usize;
                let val = self.peek()?.clone();
                match val {
                    Value::List(ref xs) => {
                        let elem = xs.get(index).ok_or_else(|| {
                            VmError::new(format!(
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
                            .ok_or_else(|| VmError::new("range index overflow".to_string()))?;
                        if i > hi {
                            return Err(VmError::new("range index out of bounds".into()));
                        }
                        self.push(Value::Int(i));
                    }
                    _ => {
                        return Err(VmError::new(format!(
                            "list destructure: expected list, got {}",
                            self.user_facing_type_name(&val)
                        )));
                    }
                }
            }
            Op::DestructListRest => {
                let start = self.read_u8()? as usize;
                let val = self.peek()?.clone();
                match val {
                    Value::List(ref xs) => {
                        if start > xs.len() {
                            return Err(VmError::new(format!(
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
                            .ok_or_else(|| VmError::new("range index overflow".to_string()))?;
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
                        return Err(VmError::new(format!(
                            "list destructure: expected list, got {}",
                            self.user_facing_type_name(&val)
                        )));
                    }
                }
            }
            Op::DestructRecordField => {
                let ni = self.read_u16()? as usize;
                let name = self.read_constant_string(ni)?;
                let val = self.peek()?.clone();
                if let Value::Record(_, fields) = val {
                    let field = fields
                        .get(&name)
                        .cloned()
                        .ok_or_else(|| VmError::new(format!("record has no field '{name}'")))?;
                    self.push(field);
                } else {
                    return Err(VmError::new(format!(
                        "record destructure: expected record, got {}",
                        self.user_facing_type_name(&val)
                    )));
                }
            }
            Op::DestructRecordRest => {
                let count = self.read_u8()? as usize;
                let mut excluded: Vec<String> = Vec::with_capacity(count);
                for _ in 0..count {
                    let ni = self.read_u16()? as usize;
                    excluded.push(self.read_constant_string(ni)?);
                }
                let val = self.pop()?;
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
                    return Err(VmError::new(format!(
                        "record rest destructure: expected record, got {}",
                        self.user_facing_type_name(&val)
                    )));
                }
            }
            Op::TestRecordTag => {
                let ni = self.read_u16()? as usize;
                let expected = self.read_constant_type(ni)?;
                let val = self.peek()?;
                let result =
                    matches!(val, Value::Record(ty, _) if record_type_matches(ty, expected.id));
                self.push(Value::Bool(result));
            }
            Op::TestMapHasKey => {
                let ci = self.read_u16()? as usize;
                let key_name = self.read_constant_string(ci)?;
                let val = self.peek()?;
                let result = match val {
                    Value::Map(map) => map.contains_key(&Value::String(key_name)),
                    _ => false,
                };
                self.push(Value::Bool(result));
            }
            Op::DestructMapValue => {
                let ci = self.read_u16()? as usize;
                let key_name = self.read_constant_string(ci)?;
                let val = self.peek()?.clone();
                if let Value::Map(map) = val {
                    let value = map
                        .get(&Value::String(key_name.clone()))
                        .cloned()
                        .ok_or_else(|| VmError::new(format!("map has no key '{key_name}'")))?;
                    self.push(value);
                } else {
                    return Err(VmError::new(format!(
                        "map destructure: expected map, got {}",
                        self.user_facing_type_name(&val)
                    )));
                }
            }
            Op::Recur => {
                let arg_count = self.read_u8()? as usize;
                let first_slot = self.read_u16()? as usize;
                let base = self.current_frame()?.base_slot;
                if arg_count > self.stack.len() {
                    return Err(VmError::new(format!(
                        "recur: arg count {arg_count} exceeds stack size {}",
                        self.stack.len()
                    )));
                }
                let start = self.stack.len() - arg_count;
                let dest_end = base + first_slot + arg_count;
                if dest_end > self.stack.len() || start + arg_count > self.stack.len() {
                    return Err(VmError::new(format!(
                        "recur: destination slot out of bounds (base {base}, first_slot {first_slot}, arg_count {arg_count}, stack len {})",
                        self.stack.len()
                    )));
                }
                for i in 0..arg_count {
                    self.stack[base + first_slot + i] = self.stack[start + i].clone();
                }
                // Truncate all the way back to just after loop bindings.
                self.stack.truncate(base + first_slot + arg_count);
            }
            Op::QuestionMark => {
                let val = self.peek()?.clone();
                match val {
                    Value::Variant(ref tag, ref fields) => match tag {
                        _ if tag.is(bv::OK) || tag.is(bv::SOME) => {
                            self.pop()?;
                            self.push(if fields.len() == 1 {
                                fields[0].clone()
                            } else {
                                Value::Unit
                            });
                        }
                        _ if tag.is(bv::ERR) || tag.is(bv::NONE) => {
                            let value = self.pop()?;
                            let finished_base = self.current_frame()?.base_slot;
                            self.frames.pop();
                            return Ok(DispatchResult::EarlyReturn {
                                value,
                                finished_base,
                            });
                        }
                        _ => {
                            return Err(VmError::new(format!(
                                "`?` applies only to Result or Option; got variant `{tag}`"
                            )));
                        }
                    },
                    _ => {
                        return Err(VmError::new(format!(
                            "`?` applies only to Result or Option; got {}",
                            self.user_facing_type_name(&val)
                        )));
                    }
                }
            }
            Op::Panic => {
                let msg = self.pop()?;
                return Err(VmError::new(format!("panic: {}", self.display_value(&msg))));
            }
            Op::CallMethod => {
                let method_name_index = self.read_u16()? as usize;
                let argc = self.read_u8()? as usize;
                let trait_index = self.read_u16()?;
                let method_name = self.read_constant_string(method_name_index)?;
                // Defense-in-depth: the compiler always emits
                // `argc = (args.len() + 1) as u8` (the receiver counts
                // toward argc), so argc==0 means corrupt bytecode and
                // would otherwise OOB-index `self.stack[receiver_slot]`
                // below (receiver_slot would equal stack.len()).
                // Reject argc==0 alongside the upper-bound check, using
                // the canonical `internal VM error:` prefix.
                if argc == 0 || argc > self.stack.len() {
                    return Err(VmError::new(format!(
                        "internal VM error: call method '{method_name}' \
                         expects argc >= 1 (receiver required) and argc \
                         <= stack size; got argc {argc}, stack size {}",
                        self.stack.len()
                    )));
                }
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
                        &method_name,
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
                    .call_method(trait_index, receiver_type, &method_name)
                    .and_then(|slot| self.globals.get(slot as usize).cloned().flatten());
                if let Some(func) = method {
                    let args: Vec<Value> = if descriptor_receiver {
                        self.stack[receiver_slot + 1..].to_vec()
                    } else {
                        self.stack[receiver_slot..].to_vec()
                    };
                    self.stack.truncate(receiver_slot);
                    // Use the resumable variant so that if the method
                    // body yields (e.g. inside `task.spawn`), resuming
                    // restores the suspended invoke state rather than
                    // re-running the method body from ip=0 — which would
                    // duplicate side effects like println, mutation, and
                    // host function calls. The "original args" we re-push
                    // on yield must reproduce the stack layout that
                    // `Op::CallMethod` will consume when this same
                    // instruction re-executes after resume: descriptor
                    // (if any) at the bottom, then `args`.
                    let original_args: Vec<Value> = if descriptor_receiver {
                        let mut v = Vec::with_capacity(1 + args.len());
                        v.push(receiver.clone());
                        v.extend(args.iter().cloned());
                        v
                    } else {
                        args.clone()
                    };
                    let result = self.invoke_callable_resumable(&func, &args, &original_args)?;
                    self.push(result);
                } else {
                    let extra_args: Vec<Value> = self.stack[receiver_slot + 1..].to_vec();
                    // Try built-in trait methods (display, equal, compare)
                    if let Some(result) =
                        self.dispatch_trait_method(&receiver, &method_name, &extra_args)
                    {
                        self.stack.truncate(receiver_slot);
                        self.push(result?);
                    } else if let Value::Record(_, ref fields) = receiver {
                        if let Some(field_val) = fields.get(&method_name) {
                            let callable = field_val.clone();
                            self.stack.truncate(receiver_slot);
                            // Resumable invoke: see the impl-method arm
                            // above. On yield the original args we re-push
                            // are receiver + extra_args, so the same
                            // CallMethod instruction reads them again on
                            // resume and re-enters this arm — which then
                            // resumes via `suspended_invoke` instead of
                            // re-running the callable from scratch.
                            let mut original_args: Vec<Value> =
                                Vec::with_capacity(1 + extra_args.len());
                            original_args.push(receiver.clone());
                            original_args.extend(extra_args.iter().cloned());
                            let result = self.invoke_callable_resumable(
                                &callable,
                                &extra_args,
                                &original_args,
                            )?;
                            self.push(result);
                        } else {
                            return Err(VmError::new(format!(
                                "no method '{method_name}' for type '{}'",
                                crate::types::canonical::dispatch_type_name(&receiver)
                            )));
                        }
                    } else {
                        return Err(VmError::new(format!(
                            "no method '{method_name}' for type '{}'",
                            crate::types::canonical::dispatch_type_name(&receiver)
                        )));
                    }
                }
            }
            Op::Slide => {
                // Keep the top value, cut the frame back to `slot` values
                // and put the value on top of them. A frame shorter than
                // `slot` means the compiler counted a value that was never
                // pushed.
                let slot = self.read_u16()? as usize;
                let base = self.current_frame()?.base_slot;
                let value = self.pop()?;
                let target = base + slot;
                if target > self.stack.len() {
                    return Err(VmError::new(format!(
                        "internal VM error: scope result slot out of range (slot {slot}, base {base}, stack len {})",
                        self.stack.len()
                    )));
                }
                self.stack.truncate(target);
                self.push(value);
            }
        }
        Ok(DispatchResult::Continue)
    }
}
