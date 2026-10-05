//! The iteration driver of the higher-order builtins.

use crate::typeinfo::bv;
use crate::value::{MAX_RANGE_MATERIALIZE, Value, checked_range_len};

use super::runtime::{BuiltinAcc, SuspendedBuiltin};
use super::{Vm, VmError};

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
    // ── list.* min/max/scan iterators (re-entrant under yield) ──
    ListMinBy,
    ListMaxBy,
    ListScan,
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
            BuiltinIterKind::ListMinBy => "list.min_by",
            BuiltinIterKind::ListMaxBy => "list.max_by",
            BuiltinIterKind::ListScan => "list.scan",
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
        BuiltinIterKind::ListMinBy | BuiltinIterKind::ListMaxBy => BuiltinAcc::Best(None),
        BuiltinIterKind::ListScan => {
            // Placeholder — callers seed the running acc + prefix via
            // `iterate_builtin_with_acc`.
            BuiltinAcc::Scan(Value::Unit, Vec::new())
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
        BuiltinIterKind::ListScan => {
            // Scan callback takes (acc, item).
            let acc_val = match acc {
                BuiltinAcc::Scan(v, _) => v.clone(),
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
                        let projected = (v.len() as u128).saturating_add(inner.len() as u128);
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
                        let range_len = checked_range_len(lo, hi)
                            .map_err(|m| VmError::new(format!("list.flat_map: {m}")))?;
                        let projected = (v.len() as u128).saturating_add(range_len as u128);
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
                    Value::Variant(ref tag, ref fields)
                        if tag.is(bv::SOME) && fields.len() == 1 =>
                    {
                        v.push(fields[0].clone());
                    }
                    Value::Variant(ref tag, _) if tag.is(bv::NONE) => {}
                    other => v.push(other),
                }
            }
            Ok(ControlFlow::Continue)
        }
        BuiltinIterKind::ListFind => {
            if value_is_truthy(&result) {
                return Ok(ControlFlow::Short(Value::variant(bv::SOME, vec![item])));
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
            // Runtime Fn gate, same policy as `ensure_no_fn` in
            // src/builtins/collections.rs: the callback-returned key
            // becomes a `BTreeMap` key, so a Fn-containing key would be
            // ordered by Arc pointer address — ASLR-nondeterministic
            // group order across runs. `list.group_by`'s signature is
            // unbounded (src/typechecker/builtins/list.rs), so the
            // typechecker never rejects Fn keys; the trait name matches
            // the static map-key contract (`k: Hash` on `map.get`/`set`).
            // Locked by tests/lang/collection_fn_gate_sibling_surfaces_tests.rs.
            if Vm::value_contains_fn(&result) {
                return Err(VmError::new(format!(
                    "{}: type 'Fn' does not implement Hash",
                    kind.name()
                )));
            }
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
            Value::Variant(ref tag, ref fields) if tag.is(bv::CONTINUE) && fields.len() == 1 => {
                if let BuiltinAcc::Fold(v) = acc {
                    *v = fields[0].clone();
                }
                Ok(ControlFlow::Continue)
            }
            Value::Variant(ref tag, ref fields) if tag.is(bv::STOP) && fields.len() == 1 => {
                Ok(ControlFlow::Short(fields[0].clone()))
            }
            other => {
                if let BuiltinAcc::Fold(v) = acc {
                    *v = other;
                }
                Ok(ControlFlow::Continue)
            }
        },
        BuiltinIterKind::ListMinBy => {
            // `result` is the key returned by the callback for `item`.
            // Runtime Fn gate mirroring the ListGroupBy gate above: Fn
            // keys would flow into `partial_cmp` and pick a winner by
            // Arc pointer address — ASLR-nondeterministic across runs.
            if Vm::value_contains_fn(&result) {
                return Err(VmError::new(format!(
                    "{}: type 'Fn' does not implement Compare",
                    kind.name()
                )));
            }
            if let BuiltinAcc::Best(slot) = acc {
                let new_pair = match slot.take() {
                    None => (result, item),
                    Some((bk, bv)) => {
                        if result.partial_cmp(&bk).unwrap_or(std::cmp::Ordering::Equal)
                            == std::cmp::Ordering::Less
                        {
                            (result, item)
                        } else {
                            (bk, bv)
                        }
                    }
                };
                *slot = Some(new_pair);
            }
            Ok(ControlFlow::Continue)
        }
        BuiltinIterKind::ListMaxBy => {
            // Runtime Fn gate — see the ListMinBy arm above.
            if Vm::value_contains_fn(&result) {
                return Err(VmError::new(format!(
                    "{}: type 'Fn' does not implement Compare",
                    kind.name()
                )));
            }
            if let BuiltinAcc::Best(slot) = acc {
                let new_pair = match slot.take() {
                    None => (result, item),
                    Some((bk, bv)) => {
                        if result.partial_cmp(&bk).unwrap_or(std::cmp::Ordering::Equal)
                            == std::cmp::Ordering::Greater
                        {
                            (result, item)
                        } else {
                            (bk, bv)
                        }
                    }
                };
                *slot = Some(new_pair);
            }
            Ok(ControlFlow::Continue)
        }
        BuiltinIterKind::ListScan => {
            let _ = item;
            if let BuiltinAcc::Scan(running, prefix) = acc {
                *running = result.clone();
                prefix.push(result);
                if prefix.len() > MAX_RANGE_MATERIALIZE {
                    return Err(VmError::new(format!(
                        "list.scan: accumulated result exceeds maximum list length of {} elements",
                        MAX_RANGE_MATERIALIZE
                    )));
                }
            }
            Ok(ControlFlow::Continue)
        }
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
                    return Ok(ControlFlow::Short(Value::variant(
                        bv::MAP_ERROR,
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
            Value::variant(bv::NONE, Vec::new())
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
        BuiltinIterKind::ListMinBy | BuiltinIterKind::ListMaxBy => {
            if let BuiltinAcc::Best(Some((_, v))) = acc {
                Value::variant(bv::SOME, vec![v])
            } else {
                Value::variant(bv::NONE, Vec::new())
            }
        }
        BuiltinIterKind::ListScan => {
            if let BuiltinAcc::Scan(_, prefix) = acc {
                Value::List(Arc::new(prefix))
            } else {
                Value::List(Arc::new(Vec::new()))
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

impl Vm {
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
            if let Some(susp) = self.take_suspended_builtin() {
                if susp.name == kind.name() {
                    (susp.items, susp.next_index, susp.acc, susp.callback)
                } else {
                    // The suspended state belongs to a different builtin.
                    // Put it back onto the top of the stack so the correct
                    // builtin can pick it up on its own re-dispatch. Use
                    // the stack-aware push in case `take_suspended_builtin`
                    // auto-promoted a deeper state we don't want to lose.
                    self.push_suspended_builtin(susp);
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
                    self.push_suspended_builtin(SuspendedBuiltin {
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
                    "internal VM error: builtin iteration resumed with stale index".into(),
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
                    self.push_suspended_builtin(SuspendedBuiltin {
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
}
