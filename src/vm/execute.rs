//! Main execution loop and opcode dispatch.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::bytecode::{Op, VmClosure};
use crate::scheduler::SliceResult;
use crate::value::{MAX_RANGE_MATERIALIZE, Value, checked_range_len};

use super::runtime::{BuiltinAcc, CallFrame, SuspendedBuiltin, SuspendedInvoke};
use super::{Vm, VmError};

/// Language-level equality for the `==` / `!=` operators.
///
/// For almost all value kinds this delegates to `PartialEq for Value`, but
/// `ExtFloat` is handled specially: `PartialEq` on `ExtFloat` uses
/// `to_bits()` equality so that NaN is self-equal (needed for
/// `Ord`/`Eq` consistency on keys used in sets, maps, and
/// deduplication — see the comment at `src/value.rs`). The user-facing
/// `==` operator instead follows IEEE-754: `NaN == NaN` is `false`.
/// Mixed `Float` / `ExtFloat` comparisons also go through the IEEE-754
/// path, matching the `Float` / `Float` fallback already used by
/// `PartialEq`.
fn language_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::ExtFloat(x), Value::ExtFloat(y)) => x == y,
        (Value::Float(x), Value::ExtFloat(y)) | (Value::ExtFloat(y), Value::Float(x)) => x == y,
        _ => a == b,
    }
}

/// Kind of higher-order builtin iteration, used by `iterate_builtin` to
/// determine how to interpret the accumulator and what to do with each
/// callback result.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum BuiltinIterKind {
    // ── list.* / set.* unary-callback iterators ───────────
    ListMap,
    ListFilter,
    ListEach,
    ListFlatMap,
    ListFilterMap,
    ListFind,
    ListAny,
    ListAll,
    ListSortBy,
    ListGroupBy,
    // ── list.* / set.* fold-style iterators ────────────────
    ListFold,
    ListFoldUntil,
    // ── set.* unary-callback iterators ─────────────────────
    SetMap,
    SetFilter,
    SetEach,
    SetFold,
    // ── map.* key-value-callback iterators ─────────────────
    MapFilter,
    MapMap,
    MapEach,
}

impl BuiltinIterKind {
    fn name(self) -> &'static str {
        match self {
            BuiltinIterKind::ListMap => "list.map",
            BuiltinIterKind::ListFilter => "list.filter",
            BuiltinIterKind::ListEach => "list.each",
            BuiltinIterKind::ListFlatMap => "list.flat_map",
            BuiltinIterKind::ListFilterMap => "list.filter_map",
            BuiltinIterKind::ListFind => "list.find",
            BuiltinIterKind::ListAny => "list.any",
            BuiltinIterKind::ListAll => "list.all",
            BuiltinIterKind::ListSortBy => "list.sort_by",
            BuiltinIterKind::ListGroupBy => "list.group_by",
            BuiltinIterKind::ListFold => "list.fold",
            BuiltinIterKind::ListFoldUntil => "list.fold_until",
            BuiltinIterKind::SetMap => "set.map",
            BuiltinIterKind::SetFilter => "set.filter",
            BuiltinIterKind::SetEach => "set.each",
            BuiltinIterKind::SetFold => "set.fold",
            BuiltinIterKind::MapFilter => "map.filter",
            BuiltinIterKind::MapMap => "map.map",
            BuiltinIterKind::MapEach => "map.each",
        }
    }
}

/// Control flow signal returned by `apply_callback_result`.
enum ControlFlow {
    /// Continue iteration to the next item.
    Continue,
    /// Short-circuit and return the given value as the final result.
    Short(Value),
}

/// Initial accumulator value for a given builtin kind.
///
/// Note: for fold-style kinds (ListFold, ListFoldUntil, SetFold), the caller
/// is expected to seed the accumulator via a separate mechanism (the initial
/// fold value is tracked as part of `acc` from the start — see the callers
/// in collections.rs which pass the fold seed explicitly via `initial_acc`).
/// This function returns a placeholder for fold kinds that must be replaced
/// by the caller before invoking `iterate_builtin`.
fn initial_acc(kind: BuiltinIterKind) -> BuiltinAcc {
    match kind {
        BuiltinIterKind::ListMap
        | BuiltinIterKind::ListFilter
        | BuiltinIterKind::ListFlatMap
        | BuiltinIterKind::ListFilterMap
        | BuiltinIterKind::SetMap
        | BuiltinIterKind::SetFilter => BuiltinAcc::List(Vec::new()),
        BuiltinIterKind::ListEach | BuiltinIterKind::SetEach | BuiltinIterKind::MapEach => {
            BuiltinAcc::Unit
        }
        BuiltinIterKind::ListFind => BuiltinAcc::Unit,
        BuiltinIterKind::ListAny => BuiltinAcc::Fold(Value::Bool(false)),
        BuiltinIterKind::ListAll => BuiltinAcc::Fold(Value::Bool(true)),
        BuiltinIterKind::ListSortBy => BuiltinAcc::SortPairs(Vec::new()),
        BuiltinIterKind::ListGroupBy => BuiltinAcc::Groups(std::collections::BTreeMap::new()),
        BuiltinIterKind::ListFold | BuiltinIterKind::ListFoldUntil | BuiltinIterKind::SetFold => {
            // Placeholder — callers seed the accumulator by calling
            // `iterate_builtin_with_acc` below.
            BuiltinAcc::Fold(Value::Unit)
        }
        BuiltinIterKind::MapFilter | BuiltinIterKind::MapMap => {
            BuiltinAcc::MapEntries(std::collections::BTreeMap::new())
        }
    }
}

/// Build the argument slice passed to the callback for a given item.
fn callback_args_for(kind: BuiltinIterKind, acc: &BuiltinAcc, item: &Value) -> Vec<Value> {
    match kind {
        BuiltinIterKind::ListFold | BuiltinIterKind::ListFoldUntil | BuiltinIterKind::SetFold => {
            // Fold callback takes (acc, item).
            let acc_val = match acc {
                BuiltinAcc::Fold(v) => v.clone(),
                _ => Value::Unit,
            };
            vec![acc_val, item.clone()]
        }
        BuiltinIterKind::MapFilter | BuiltinIterKind::MapMap | BuiltinIterKind::MapEach => {
            // map.* callback takes (key, value).  For these builtins, `item`
            // is stored as a Tuple(k, v).
            if let Value::Tuple(parts) = item
                && parts.len() == 2
            {
                vec![parts[0].clone(), parts[1].clone()]
            } else {
                vec![item.clone()]
            }
        }
        _ => vec![item.clone()],
    }
}

/// Apply a callback result to the accumulator for the given builtin kind.
/// Returns `Ok(ControlFlow::Short(val))` to short-circuit the iteration, or
/// `Err(VmError)` to abort iteration cleanly (e.g. on accumulator overflow).
fn apply_callback_result(
    kind: BuiltinIterKind,
    acc: &mut BuiltinAcc,
    item: Value,
    result: Value,
) -> Result<ControlFlow, VmError> {
    match kind {
        BuiltinIterKind::ListMap | BuiltinIterKind::SetMap => {
            if let BuiltinAcc::List(v) = acc {
                v.push(result);
            }
            Ok(ControlFlow::Continue)
        }
        BuiltinIterKind::ListFilter | BuiltinIterKind::SetFilter => {
            let keep = value_is_truthy(&result);
            if keep && let BuiltinAcc::List(v) = acc {
                v.push(item);
            }
            Ok(ControlFlow::Continue)
        }
        BuiltinIterKind::ListEach | BuiltinIterKind::SetEach | BuiltinIterKind::MapEach => {
            let _ = result;
            Ok(ControlFlow::Continue)
        }
        BuiltinIterKind::ListFlatMap => {
            if let BuiltinAcc::List(v) = acc {
                match result {
                    Value::List(inner) => {
                        // Guard against accumulated overflow even for list
                        // results: the cap applies to the final list size.
                        let projected =
                            (v.len() as u128).saturating_add(inner.len() as u128);
                        if projected > MAX_RANGE_MATERIALIZE as u128 {
                            return Err(VmError::new(format!(
                                "list.flat_map: accumulated result exceeds maximum list length of {} elements",
                                MAX_RANGE_MATERIALIZE
                            )));
                        }
                        v.extend(inner.iter().cloned());
                    }
                    Value::Range(lo, hi) => {
                        // Check that this single callback's range fits the
                        // cap before materializing it, and then check that
                        // adding it to the existing accumulator won't
                        // exceed the cap either. Without this, a callback
                        // returning `0..i64::MAX` would OOM the process.
                        let range_len = checked_range_len(lo, hi).map_err(|m| {
                            VmError::new(format!("list.flat_map: {m}"))
                        })?;
                        let projected =
                            (v.len() as u128).saturating_add(range_len as u128);
                        if projected > MAX_RANGE_MATERIALIZE as u128 {
                            return Err(VmError::new(format!(
                                "list.flat_map: accumulated result exceeds maximum list length of {} elements",
                                MAX_RANGE_MATERIALIZE
                            )));
                        }
                        if lo <= hi {
                            v.reserve(range_len);
                            for i in lo..=hi {
                                v.push(Value::Int(i));
                            }
                        }
                    }
                    other => {
                        if v.len() >= MAX_RANGE_MATERIALIZE {
                            return Err(VmError::new(format!(
                                "list.flat_map: accumulated result exceeds maximum list length of {} elements",
                                MAX_RANGE_MATERIALIZE
                            )));
                        }
                        v.push(other);
                    }
                }
            }
            Ok(ControlFlow::Continue)
        }
        BuiltinIterKind::ListFilterMap => {
            if let BuiltinAcc::List(v) = acc {
                match result {
                    Value::Variant(ref tag, ref fields) if tag == "Some" && fields.len() == 1 => {
                        v.push(fields[0].clone());
                    }
                    Value::Variant(ref tag, _) if tag == "None" => {}
                    other => v.push(other),
                }
            }
            Ok(ControlFlow::Continue)
        }
        BuiltinIterKind::ListFind => {
            if value_is_truthy(&result) {
                return Ok(ControlFlow::Short(Value::Variant(
                    "Some".into(),
                    vec![item],
                )));
            }
            Ok(ControlFlow::Continue)
        }
        BuiltinIterKind::ListAny => {
            if value_is_truthy(&result) {
                return Ok(ControlFlow::Short(Value::Bool(true)));
            }
            Ok(ControlFlow::Continue)
        }
        BuiltinIterKind::ListAll => {
            if !value_is_truthy(&result) {
                return Ok(ControlFlow::Short(Value::Bool(false)));
            }
            Ok(ControlFlow::Continue)
        }
        BuiltinIterKind::ListSortBy => {
            if let BuiltinAcc::SortPairs(v) = acc {
                v.push((result, item));
            }
            Ok(ControlFlow::Continue)
        }
        BuiltinIterKind::ListGroupBy => {
            if let BuiltinAcc::Groups(m) = acc {
                m.entry(result).or_default().push(item);
            }
            Ok(ControlFlow::Continue)
        }
        BuiltinIterKind::ListFold | BuiltinIterKind::SetFold => {
            if let BuiltinAcc::Fold(v) = acc {
                *v = result;
            }
            Ok(ControlFlow::Continue)
        }
        BuiltinIterKind::ListFoldUntil => match result {
            Value::Variant(ref tag, ref fields) if tag == "Continue" && fields.len() == 1 => {
                if let BuiltinAcc::Fold(v) = acc {
                    *v = fields[0].clone();
                }
                Ok(ControlFlow::Continue)
            }
            Value::Variant(ref tag, ref fields) if tag == "Stop" && fields.len() == 1 => {
                Ok(ControlFlow::Short(fields[0].clone()))
            }
            other => {
                if let BuiltinAcc::Fold(v) = acc {
                    *v = other;
                }
                Ok(ControlFlow::Continue)
            }
        },
        BuiltinIterKind::MapFilter => {
            // item is Tuple(k, v); result is truthy/falsy.
            if value_is_truthy(&result)
                && let (BuiltinAcc::MapEntries(m), Value::Tuple(parts)) = (acc, &item)
                && parts.len() == 2
            {
                m.insert(parts[0].clone(), parts[1].clone());
            }
            Ok(ControlFlow::Continue)
        }
        BuiltinIterKind::MapMap => {
            // Callback must return a (key, value) tuple.
            if let BuiltinAcc::MapEntries(m) = acc {
                if let Value::Tuple(pair) = result
                    && pair.len() == 2
                {
                    let mut it = pair.into_iter();
                    let k = it.next().unwrap();
                    let v = it.next().unwrap();
                    m.insert(k, v);
                } else {
                    // Type mismatch — propagate as a short-circuit error via
                    // a Variant that the caller will catch post-iteration.
                    // But we can't return an error from here, so we stash an
                    // error marker by inserting a sentinel and short-circuit.
                    return Ok(ControlFlow::Short(Value::Variant(
                        "__MapMapTypeError__".into(),
                        Vec::new(),
                    )));
                }
            }
            Ok(ControlFlow::Continue)
        }
    }
}

/// Finalize the accumulator into a return Value for the given builtin kind.
fn finalize_acc(kind: BuiltinIterKind, acc: BuiltinAcc) -> Value {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;
    match kind {
        BuiltinIterKind::ListMap
        | BuiltinIterKind::ListFilter
        | BuiltinIterKind::ListFlatMap
        | BuiltinIterKind::ListFilterMap => {
            if let BuiltinAcc::List(v) = acc {
                Value::List(Arc::new(v))
            } else {
                Value::List(Arc::new(Vec::new()))
            }
        }
        BuiltinIterKind::SetMap | BuiltinIterKind::SetFilter => {
            if let BuiltinAcc::List(v) = acc {
                let set: BTreeSet<Value> = v.into_iter().collect();
                Value::Set(Arc::new(set))
            } else {
                Value::Set(Arc::new(BTreeSet::new()))
            }
        }
        BuiltinIterKind::ListEach | BuiltinIterKind::SetEach | BuiltinIterKind::MapEach => {
            Value::Unit
        }
        BuiltinIterKind::ListFind => {
            // If we reach finalize (didn't short-circuit), no item matched.
            Value::Variant("None".into(), Vec::new())
        }
        BuiltinIterKind::ListAny => Value::Bool(false),
        BuiltinIterKind::ListAll => Value::Bool(true),
        BuiltinIterKind::ListSortBy => {
            if let BuiltinAcc::SortPairs(mut pairs) = acc {
                pairs.sort_by(|(a, _), (b, _)| {
                    a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
                });
                let sorted: Vec<Value> = pairs.into_iter().map(|(_, v)| v).collect();
                Value::List(Arc::new(sorted))
            } else {
                Value::List(Arc::new(Vec::new()))
            }
        }
        BuiltinIterKind::ListGroupBy => {
            if let BuiltinAcc::Groups(groups) = acc {
                let result: BTreeMap<Value, Value> = groups
                    .into_iter()
                    .map(|(k, v)| (k, Value::List(Arc::new(v))))
                    .collect();
                Value::Map(Arc::new(result))
            } else {
                Value::Map(Arc::new(BTreeMap::new()))
            }
        }
        BuiltinIterKind::ListFold | BuiltinIterKind::ListFoldUntil | BuiltinIterKind::SetFold => {
            if let BuiltinAcc::Fold(v) = acc {
                v
            } else {
                Value::Unit
            }
        }
        BuiltinIterKind::MapFilter | BuiltinIterKind::MapMap => {
            if let BuiltinAcc::MapEntries(m) = acc {
                Value::Map(Arc::new(m))
            } else {
                Value::Map(Arc::new(BTreeMap::new()))
            }
        }
    }
}

/// Truthiness helper that mirrors `Vm::is_truthy` but is a free function so
/// it can be used from the stateless helpers above.
fn value_is_truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Unit => false,
        _ => true,
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
        loop {
            let op_byte = self.read_byte()?;
            let op = Op::from_byte(op_byte)
                .ok_or_else(|| VmError::new(format!("unknown opcode: {op_byte}")))?;
            match self.dispatch_one(op)? {
                DispatchResult::Continue => {}
                DispatchResult::Return(result) => {
                    let finished_base = self.current_frame()?.base_slot;
                    self.frames.pop();
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

    // ── Call a value ──────────────────────────────────────────────

    pub(super) fn call_value(
        &mut self,
        func_val: Value,
        argc: usize,
        func_slot: usize,
    ) -> Result<(), VmError> {
        const MAX_FRAMES: usize = 100_000;
        match func_val {
            Value::VmClosure(closure) => {
                if argc != closure.function.arity as usize {
                    return Err(VmError::new(format!(
                        "function '{}' expects {} arguments, got {}",
                        closure.function.name, closure.function.arity, argc
                    )));
                }
                if self.frames.len() >= MAX_FRAMES {
                    return Err(VmError::new(format!(
                        "stack overflow: recursion depth exceeded {} frames (tip: put the recursive call in tail position to avoid this limit)",
                        MAX_FRAMES
                    )));
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
                let result = self.dispatch_builtin(&name, &args)?;
                self.push(result);
                Ok(())
            }
            Value::VariantConstructor(name, arity) => {
                if argc != arity {
                    return Err(VmError::new(format!(
                        "variant constructor '{name}' expects {arity} arguments, got {argc}"
                    )));
                }
                let start = func_slot + 1;
                let fields: Vec<Value> = self.stack[start..start + argc].to_vec();
                self.stack.truncate(func_slot);
                self.push(Value::Variant(name, fields));
                Ok(())
            }
            _ => Err(VmError::new(format!(
                "cannot call value of type {}",
                self.type_name(&func_val)
            ))),
        }
    }

    /// Call a callable Value and return its result. Used for higher-order builtins.
    pub(crate) fn invoke_callable(
        &mut self,
        func: &Value,
        args: &[Value],
    ) -> Result<Value, VmError> {
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
                const MAX_FRAMES: usize = 100_000;
                if self.frames.len() >= MAX_FRAMES {
                    return Err(VmError::new(format!(
                        "stack overflow: recursion depth exceeded {} frames (tip: put the recursive call in tail position to avoid this limit)",
                        MAX_FRAMES
                    )));
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
                            if self.frames.len() < saved_frame_count {
                                return Err(VmError::new(
                                    "frame underflow in invoke_callable".into(),
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
                            // re-running the callback from scratch.
                            let extra_frames = self.frames.split_off(saved_frame_count);
                            let extra_stack = self.stack.split_off(func_slot);
                            self.suspended_invoke = Some(SuspendedInvoke {
                                frames: extra_frames,
                                stack: extra_stack,
                                func_slot,
                            });
                            return Err(e);
                        }
                        Err(e) => {
                            self.frames.truncate(saved_frame_count);
                            self.stack.truncate(func_slot);
                            return Err(e);
                        }
                    }
                }
            }
            Value::BuiltinFn(name) => self.dispatch_builtin(name, args),
            Value::VariantConstructor(name, arity) => {
                if args.len() != *arity {
                    return Err(VmError::new(format!(
                        "variant constructor '{name}' expects {arity} arguments, got {}",
                        args.len()
                    )));
                }
                Ok(Value::Variant(name.clone(), args.to_vec()))
            }
            _ => Err(VmError::new(
                "cannot call value in invoke_callable".to_string(),
            )),
        }
    }

    /// Resume a previously suspended `invoke_callable`.
    ///
    /// When a builtin inside a callback yielded (e.g. IO inside
    /// `channel.each`), the callback's frames and stack were saved in
    /// `self.suspended_invoke`.  This method restores them and continues
    /// the execution loop until the callback returns a result.
    pub(crate) fn resume_suspended_invoke(&mut self) -> Result<Value, VmError> {
        let suspended = self.suspended_invoke.take().ok_or_else(|| {
            VmError::new("internal: resume_suspended_invoke called with no suspended state".into())
        })?;
        let saved_frame_count = self.frames.len();
        let func_slot = suspended.func_slot;
        // Restore the saved frames and stack.
        self.frames.extend(suspended.frames);
        self.stack.extend(suspended.stack);
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
                    if self.frames.len() < saved_frame_count {
                        return Err(VmError::new(
                            "frame underflow in resume_suspended_invoke".into(),
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
                    self.suspended_invoke = Some(SuspendedInvoke {
                        frames: extra_frames,
                        stack: extra_stack,
                        func_slot,
                    });
                    return Err(e);
                }
                Err(e) => {
                    self.frames.truncate(saved_frame_count);
                    self.stack.truncate(func_slot);
                    return Err(e);
                }
            }
        }
    }

    // ── Higher-order builtin iteration helper ─────────────────────

    /// Run a callback over each item, accumulating results, with correct
    /// yield/resume handling.
    ///
    /// This is the shared driver for all higher-order builtins (`list.map`,
    /// `list.filter`, `list.fold`, `set.map`, `map.filter`, etc.).  When the
    /// callback yields (e.g. because it contains an IO call), this function:
    ///   1. Saves its partial iteration state into `self.suspended_builtin`
    ///      (the current index, accumulator, callback, and items).
    ///   2. Re-pushes the builtin's original args so the outer `CallBuiltin`
    ///      opcode will re-dispatch the same builtin on resume.
    ///   3. Returns `Err(yield)` so the yield propagates to the scheduler.
    ///
    /// On resume, the outer `CallBuiltin` re-pops the args and re-enters this
    /// helper.  The helper detects that `suspended_builtin` matches the same
    /// kind, restores state, and — if `suspended_invoke` is also set (because
    /// the callback was mid-execution when it yielded) — resumes the callback
    /// via `resume_suspended_invoke` to get its final result before advancing.
    ///
    /// The `items` vector should be a fresh materialization of the iteration
    /// source on first call.  On resume (detected by `suspended_builtin`),
    /// items are restored from the saved state and the passed-in `items` is
    /// discarded.
    ///
    /// `original_args` is the list of `Value`s that will be re-pushed onto the
    /// VM stack on yield so that `CallBuiltin` can re-read them on resume.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn iterate_builtin(
        &mut self,
        kind: BuiltinIterKind,
        items: Vec<Value>,
        callback: Value,
        original_args: &[Value],
    ) -> Result<Value, VmError> {
        self.iterate_builtin_with_acc(kind, items, callback, initial_acc(kind), original_args)
    }

    /// Like `iterate_builtin`, but lets callers supply an explicit initial
    /// accumulator.  Used by fold-style builtins (`list.fold`,
    /// `list.fold_until`, `set.fold`) to seed the accumulator.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn iterate_builtin_with_acc(
        &mut self,
        kind: BuiltinIterKind,
        items: Vec<Value>,
        callback: Value,
        seeded_acc: BuiltinAcc,
        original_args: &[Value],
    ) -> Result<Value, VmError> {
        // ── Restore state from a prior yield, if any ──────────
        let (items, mut index, mut acc, callback) = {
            let fresh_items = items;
            let fresh_callback = callback;
            let fresh_acc = seeded_acc;
            if let Some(susp) = self.suspended_builtin.take() {
                if susp.name == kind.name() {
                    (susp.items, susp.next_index, susp.acc, susp.callback)
                } else {
                    // The suspended state belongs to a different builtin.
                    // Restore it and start fresh — this shouldn't happen in
                    // practice because yields propagate immediately, but be
                    // safe and put it back so the correct builtin can pick
                    // it up.
                    self.suspended_builtin = Some(susp);
                    (fresh_items, 0, fresh_acc, fresh_callback)
                }
            } else {
                (fresh_items, 0, fresh_acc, fresh_callback)
            }
        };

        // `items` and `callback` are owned locals that may be moved into
        // `SuspendedBuiltin` on a yield.  They don't need to be declared
        // `mut` because the moves happen in terminating branches.

        // ── If the callback was mid-execution on yield, finish it now ──
        if self.suspended_invoke.is_some() {
            let callback_result = match self.resume_suspended_invoke() {
                Ok(v) => v,
                Err(e) if e.is_yield => {
                    // Still yielding — stash our state and re-push args.
                    self.suspended_builtin = Some(SuspendedBuiltin {
                        name: kind.name().to_string(),
                        items,
                        next_index: index,
                        callback,
                        acc,
                    });
                    for a in original_args {
                        self.push(a.clone());
                    }
                    return Err(e);
                }
                Err(e) => return Err(e),
            };
            // The callback that yielded was processing items[index].  Apply
            // its result to the accumulator and advance the index.
            if index < items.len() {
                let item = items[index].clone();
                match apply_callback_result(kind, &mut acc, item, callback_result)? {
                    ControlFlow::Continue => index += 1,
                    ControlFlow::Short(val) => {
                        return Ok(val);
                    }
                }
            } else {
                // Shouldn't happen — defensive.
                return Err(VmError::new(
                    "internal: iterate_builtin resumed with stale index".into(),
                ));
            }
        }

        // ── Main iteration loop ──────────────────────────────
        loop {
            if index >= items.len() {
                break;
            }
            let item = items[index].clone();
            let cb_args = callback_args_for(kind, &acc, &item);
            let invoke_result = self.invoke_callable(&callback, &cb_args);
            match invoke_result {
                Ok(v) => match apply_callback_result(kind, &mut acc, item, v)? {
                    ControlFlow::Continue => index += 1,
                    ControlFlow::Short(val) => return Ok(val),
                },
                Err(e) if e.is_yield => {
                    // Callback yielded.  Save state and re-push args.
                    self.suspended_builtin = Some(SuspendedBuiltin {
                        name: kind.name().to_string(),
                        items,
                        next_index: index,
                        callback,
                        acc,
                    });
                    for a in original_args {
                        self.push(a.clone());
                    }
                    return Err(e);
                }
                Err(e) => return Err(e),
            }
        }

        // ── Finalize accumulator into return value ───────────
        // Explicit drops to anchor the lifetime of `items`/`callback` past
        // the loop even when all callback invocations succeeded.
        drop(items);
        drop(callback);
        Ok(finalize_acc(kind, acc))
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
                // The language-level `==` operator follows IEEE-754 for
                // `ExtFloat` (so NaN != NaN). `PartialEq for Value` on
                // `ExtFloat` uses `to_bits()` equality so that NaN is
                // self-equal — that is required for `Ord`/`Eq`
                // consistency on keys used in sets, maps, and
                // deduplication. See `src/value.rs`.
                self.push(Value::Bool(language_eq(&a, &b)));
            }
            Op::Neq => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.check_same_type(&a, &b)?;
                self.push(Value::Bool(!language_eq(&a, &b)));
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
                            return Err(VmError::new(format!("integer overflow: -{n}")));
                        }
                    },
                    Value::Float(n) => {
                        let result = if -n == 0.0 { 0.0 } else { -n };
                        self.push(Value::Float(result));
                    }
                    Value::ExtFloat(n) => {
                        self.push(Value::ExtFloat(-n));
                    }
                    other => {
                        return Err(VmError::new(format!(
                            "cannot negate {}",
                            self.type_name(&other)
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
                            self.type_name(&other)
                        )));
                    }
                }
            }
            Op::And => {
                let b = self.pop()?;
                let a = self.pop()?;
                match (&a, &b) {
                    (Value::Bool(a_val), Value::Bool(b_val)) => {
                        self.push(Value::Bool(*a_val && *b_val))
                    }
                    _ => return Err(VmError::new("logical 'and' requires two booleans".into())),
                }
            }
            Op::Or => {
                let b = self.pop()?;
                let a = self.pop()?;
                match (&a, &b) {
                    (Value::Bool(a_val), Value::Bool(b_val)) => {
                        self.push(Value::Bool(*a_val || *b_val))
                    }
                    _ => return Err(VmError::new("logical 'or' requires two booleans".into())),
                }
            }
            Op::DisplayValue => {
                let val = self.pop()?;
                if matches!(&val, Value::String(_)) {
                    self.push(val);
                } else {
                    let s = self.display_value(&val);
                    self.push(Value::String(s));
                }
            }
            Op::StringConcat => {
                let count = self.read_u8()? as usize;
                if count > self.stack.len() {
                    return Err(VmError::new(format!(
                        "StringConcat: need {} values but stack has {}",
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
                        return Err(VmError::new(
                            "StringConcat: non-string value on stack".into(),
                        ));
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
                        "internal: SetLocal slot out of range (slot {slot}, base {base}, stack len {})",
                        self.stack.len()
                    )));
                }
                self.stack[target] = value;
            }
            Op::GetGlobal => {
                let name_index = self.read_u16()? as usize;
                let name = self.read_constant_string(name_index)?;
                let value = self
                    .globals
                    .get(&name)
                    .cloned()
                    .ok_or_else(|| VmError::new(format!("undefined global: {name}")))?;
                self.push(value);
            }
            Op::SetGlobal => {
                let name_index = self.read_u16()? as usize;
                let name = self.read_constant_string(name_index)?;
                let value = self.peek()?.clone();
                self.globals.insert(name, value);
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
                    for i in 0..argc {
                        self.stack[base + i] = self.stack[func_slot + 1 + i].clone();
                    }
                    self.stack.truncate(base + argc);
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
                        "internal: MakeClosure constant is not a VmClosure".to_string(),
                    ));
                }
            }
            Op::MakeTuple => {
                let count = self.read_u8()? as usize;
                if count > self.stack.len() {
                    return Err(VmError::new(format!(
                        "MakeTuple: count {count} exceeds stack size {}",
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
                        "MakeList: count {count} exceeds stack size {}",
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
                        "MakeMap: need {total} values but stack has {}",
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
                        "MakeSet: count {count} exceeds stack size {}",
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
                let type_name = self.read_constant_string(type_name_index)?;
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
                self.push(Value::Record(type_name, Arc::new(fields)));
            }
            Op::MakeVariant => {
                let name_index = self.read_u16()? as usize;
                let field_count = self.read_u8()? as usize;
                let name = self.read_constant_string(name_index)?;
                if field_count > self.stack.len() {
                    return Err(VmError::new(format!(
                        "MakeVariant: field count {field_count} exceeds stack size {}",
                        self.stack.len()
                    )));
                }
                let start = self.stack.len() - field_count;
                let fields: Vec<Value> = self.stack[start..].to_vec();
                self.stack.truncate(start);
                self.push(Value::Variant(name, fields));
            }
            Op::RecordUpdate => {
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
                    return Err(VmError::new("record update on non-record".into()));
                }
            }
            Op::MakeRange => {
                let end = self.pop()?;
                let start = self.pop()?;
                if let (Value::Int(a), Value::Int(b)) = (&start, &end) {
                    self.push(Value::Range(*a, *b));
                } else {
                    return Err(VmError::new("range requires two integers".into()));
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
                match b {
                    Value::List(xs) => result.extend(xs.iter().cloned()),
                    Value::Range(lo, hi) => {
                        checked_range_len(lo, hi).map_err(VmError::new)?;
                        result.extend((lo..=hi).map(Value::Int));
                    }
                    _ => {
                        return Err(VmError::new(
                            "ListConcat: right operand is not a list or range".into(),
                        ));
                    }
                }
                if result.len() > MAX_RANGE_MATERIALIZE {
                    return Err(VmError::new(format!(
                        "concatenated list exceeds maximum size of {} elements",
                        MAX_RANGE_MATERIALIZE
                    )));
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
                            self.type_name(&other)
                        )));
                    }
                }
            }
            Op::GetIndex => {
                let index = self.read_u8()? as usize;
                let target = self.pop()?;
                if let Value::Tuple(ref elems) = target {
                    let val = elems
                        .get(index)
                        .cloned()
                        .ok_or_else(|| VmError::new("tuple index out of bounds".to_string()))?;
                    self.push(val);
                } else {
                    return Err(VmError::new(format!(
                        "cannot index into {}",
                        self.type_name(&target)
                    )));
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
            Op::PopN => {
                let count = self.read_u8()? as usize;
                let new_len = self.stack.len().saturating_sub(count);
                self.stack.truncate(new_len);
            }
            Op::Dup => {
                let val = self.peek()?.clone();
                self.push(val);
            }
            Op::TestTag => {
                let ni = self.read_u16()? as usize;
                let name = self.read_constant_string(ni)?;
                let val = self.peek()?;
                let result = matches!(val, Value::Variant(tag, _) if *tag == name);
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
                            "DestructTuple index {} out of bounds (len {})",
                            index,
                            elems.len()
                        ))
                    })?;
                    self.push(elem.clone());
                } else {
                    return Err(VmError::new("DestructTuple on non-tuple".into()));
                }
            }
            Op::DestructVariant => {
                let index = self.read_u8()? as usize;
                let val = self.peek()?.clone();
                if let Value::Variant(_, fields) = val {
                    let field = fields.get(index).ok_or_else(|| {
                        VmError::new(format!(
                            "DestructVariant index {} out of bounds (len {})",
                            index,
                            fields.len()
                        ))
                    })?;
                    self.push(field.clone());
                } else {
                    return Err(VmError::new("DestructVariant on non-variant".into()));
                }
            }
            Op::DestructList => {
                let index = self.read_u8()? as usize;
                let val = self.peek()?.clone();
                match val {
                    Value::List(xs) => {
                        let elem = xs.get(index).ok_or_else(|| {
                            VmError::new(format!(
                                "DestructList index {} out of bounds (len {})",
                                index,
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
                    _ => return Err(VmError::new("DestructList on non-list".into())),
                }
            }
            Op::DestructListRest => {
                let start = self.read_u8()? as usize;
                let val = self.peek()?.clone();
                match val {
                    Value::List(xs) => {
                        if start > xs.len() {
                            return Err(VmError::new(format!(
                                "DestructListRest start {} out of bounds (len {})",
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
                    _ => return Err(VmError::new("DestructListRest on non-list".into())),
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
                    return Err(VmError::new("DestructRecordField on non-record".into()));
                }
            }
            Op::TestRecordTag => {
                let ni = self.read_u16()? as usize;
                let name = self.read_constant_string(ni)?;
                let val = self.peek()?;
                let result = matches!(val, Value::Record(tag, _) if *tag == name);
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
                    return Err(VmError::new("DestructMapValue on non-map".into()));
                }
            }
            Op::LoopSetup => {
                // L4 fix: this opcode is reserved but never emitted by
                // the compiler. If we ever reach it, that is a compiler
                // bug — crash loudly rather than silently no-op.
                unreachable!(
                    "Op::LoopSetup is not emitted by the compiler; this is a compiler bug"
                );
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
                    Value::Variant(ref tag, ref fields) => match tag.as_str() {
                        "Ok" | "Some" => {
                            self.pop()?;
                            self.push(if fields.len() == 1 {
                                fields[0].clone()
                            } else {
                                Value::Unit
                            });
                        }
                        "Err" | "None" => {
                            let value = self.pop()?;
                            let finished_base = self.current_frame()?.base_slot;
                            self.frames.pop();
                            return Ok(DispatchResult::EarlyReturn {
                                value,
                                finished_base,
                            });
                        }
                        _ => return Err(VmError::new(format!("? on non-Result/Option: {tag}"))),
                    },
                    _ => {
                        return Err(VmError::new(format!(
                            "? on non-variant: {}",
                            self.type_name(&val)
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
                let method_name = self.read_constant_string(method_name_index)?;
                if argc > self.stack.len() {
                    return Err(VmError::new(format!(
                        "call method '{method_name}': argc {argc} exceeds stack size {}",
                        self.stack.len()
                    )));
                }
                let receiver_slot = self.stack.len() - argc;
                let receiver = self.stack[receiver_slot].clone();
                let type_name = self.value_type_name_for_dispatch(&receiver);
                let qualified = format!("{type_name}.{method_name}");
                if let Some(func) = self.globals.get(&qualified).cloned() {
                    let args: Vec<Value> = self.stack[receiver_slot..].to_vec();
                    self.stack.truncate(receiver_slot);
                    let result = self.invoke_callable(&func, &args)?;
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
                            let result = self.invoke_callable(&callable, &extra_args)?;
                            self.push(result);
                        } else {
                            return Err(VmError::new(format!(
                                "no method '{method_name}' for type '{type_name}'"
                            )));
                        }
                    } else {
                        return Err(VmError::new(format!(
                            "no method '{method_name}' for type '{type_name}'"
                        )));
                    }
                }
            }
            Op::NarrowFloat => {
                let offset = self.read_u16()? as usize;
                let val = self.peek()?.clone();
                match val {
                    Value::ExtFloat(f) if f.is_finite() => {
                        self.pop()?;
                        self.push(Value::Float(if f == 0.0 { 0.0 } else { f }));
                        self.current_frame_mut()?.ip += offset;
                    }
                    Value::ExtFloat(_) => {
                        self.pop()?;
                    }
                    Value::Float(_) => {
                        self.current_frame_mut()?.ip += offset;
                    }
                    _ => {
                        return Err(VmError::new(
                            "NarrowFloat: expected float value".to_string(),
                        ));
                    }
                }
            }
        }
        Ok(DispatchResult::Continue)
    }
}
