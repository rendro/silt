//! Collection builtin functions (`list.*`, `map.*`, `set.*`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::common::value_kind;
use crate::typeinfo::bv;
use crate::value::{MAX_RANGE_MATERIALIZE, Value, checked_range_len};
use crate::vm::{Flow, Native, Step, Vm, VmError, call_then, item_arg, iterate, next, stop};

/// Lazy iterator over `Value::List` or `Value::Range` without materializing.
enum ValueIter {
    List {
        items: Arc<Vec<Value>>,
        index: usize,
    },
    Range {
        current: i64,
        end: i64,
        done: bool,
    },
}

impl ValueIter {
    /// Build an iterator from a List or Range value.
    fn try_from(val: &Value, fn_name: &str) -> Result<Self, VmError> {
        match val {
            Value::List(xs) => Ok(ValueIter::List {
                items: Arc::clone(xs),
                index: 0,
            }),
            Value::Range(lo, hi) => Ok(ValueIter::Range {
                current: *lo,
                end: *hi,
                done: *lo > *hi,
            }),
            _ => Err(VmError::new(format!("{fn_name} requires a list or range"))),
        }
    }

    /// Collect all items into a Vec, returning an error if a range exceeds
    /// the materialization limit.
    fn collect_vec(self) -> Result<Vec<Value>, VmError> {
        if let ValueIter::Range { current, end, done } = &self
            && !done
        {
            checked_range_len(*current, *end).map_err(VmError::new)?;
        }
        Ok(self.collect())
    }
}

impl Iterator for ValueIter {
    type Item = Value;

    fn next(&mut self) -> Option<Value> {
        match self {
            ValueIter::List { items, index } => {
                let item = items.get(*index)?.clone();
                *index += 1;
                Some(item)
            }
            ValueIter::Range { current, end, done } => {
                if *done {
                    return None;
                }
                let val = Value::Int(*current);
                if *current == *end {
                    *done = true;
                } else {
                    *current += 1;
                }
                Some(val)
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = match self {
            ValueIter::List { items, index } => items.len().saturating_sub(*index),
            ValueIter::Range { current, end, done } => {
                if *done {
                    0
                } else {
                    // Use saturating arithmetic to avoid overflow on huge ranges.
                    (*end as i128 - *current as i128 + 1).min(usize::MAX as i128) as usize
                }
            }
        };
        (len, Some(len))
    }
}

impl ExactSizeIterator for ValueIter {}

/// Materialize a List or Range into a concrete `Vec<Value>` of items.
/// Used by the list builtins that call a function for each item.
/// Returns an error if the source is not a list or range, or if the range
/// would exceed the materialization limit.
fn materialize_iter(val: &Value, fn_name: &str) -> Result<Vec<Value>, VmError> {
    match val {
        Value::List(xs) => Ok((**xs).clone()),
        Value::Range(lo, hi) => {
            checked_range_len(*lo, *hi).map_err(VmError::new)?;
            if *lo > *hi {
                return Ok(Vec::new());
            }
            Ok((*lo..=*hi).map(Value::Int).collect())
        }
        _ => Err(VmError::new(format!("{fn_name} requires a list or range"))),
    }
}

/// Runtime backstop for the ordering/equality-consuming collection
/// builtins: error with the canonical operator-gate wording ("type 'Fn'
/// does not implement Compare/Equal") if any of `vals` transitively
/// contains a function-shaped value. Mirrors the `Op::Eq` gate in
/// src/vm/run.rs; deliberately NOT enforced as a static `where`
/// bound on the builtin signatures because that would reject currently
/// working programs (e.g. sorting tuples via `Value::cmp`). Locked by tests/lang/collection_builtin_fn_gate_tests.rs.
///
/// The contains-a-fn walk delegates to `Vm::value_contains_fn`
/// (src/vm/mod.rs) — the SINGLE runtime-side oracle for every
/// execution-site Compare/Equal/Hash gate (operator, dispatch, and
/// builtin surfaces). Do not re-inline a local copy of the walker
/// here: a new container `Value` variant added to one copy but not the
/// other would silently split gate behavior between the operator and
/// builtin surfaces. Single-definition is pinned by
/// tests/meta/value_contains_fn_dedup_lock_tests.rs.
fn ensure_no_fn(fn_name: &str, trait_name: &str, vals: &[&Value]) -> Result<(), VmError> {
    for v in vals {
        if Vm::value_contains_fn(v) {
            return Err(VmError::new(format!(
                "{fn_name}: type 'Fn' does not implement {trait_name}"
            )));
        }
    }
    Ok(())
}

// ── The functions that call a function ───────────────────────────
//
// Each is an iteration (`vm::iterate`): its state, the arguments of a
// call, what a call's result does to the state, and the value the
// state gives at the end.

fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Unit => false,
        _ => true,
    }
}

/// The arguments of a function that takes the state and the item
/// (`list.fold`).
fn acc_and_item(acc: &Value, item: &Value, stack: &mut Vec<Value>) {
    stack.push(acc.clone());
    stack.push(item.clone());
}

/// The arguments of a function that takes the key and the value of a
/// map's entry, which is the item `(key, value)`.
fn key_and_value<S>(_: &S, entry: &Value, stack: &mut Vec<Value>) {
    match entry {
        Value::Tuple(pair) => stack.extend(pair.iter().cloned()),
        other => stack.push(other.clone()),
    }
}

/// The entries of a map as items.
fn entries(m: &BTreeMap<Value, Value>) -> Vec<Value> {
    m.iter()
        .map(|(k, v)| Value::Tuple(vec![k.clone(), v.clone()]))
        .collect()
}

fn keep_result(out: &mut Vec<Value>, _item: Value, result: Value) -> Flow {
    out.push(result);
    next()
}

fn keep_item_if(out: &mut Vec<Value>, item: Value, result: Value) -> Flow {
    if truthy(&result) {
        out.push(item);
    }
    next()
}

fn ignore<S>(_: &mut S, _item: Value, _result: Value) -> Flow {
    next()
}

fn set_acc(acc: &mut Value, _item: Value, result: Value) -> Flow {
    *acc = result;
    next()
}

fn as_list(out: &mut Vec<Value>) -> Result<Value, VmError> {
    Ok(Value::List(Arc::new(std::mem::take(out))))
}

fn as_set(out: &mut Vec<Value>) -> Result<Value, VmError> {
    Ok(Value::Set(Arc::new(
        std::mem::take(out).into_iter().collect(),
    )))
}

fn as_map(out: &mut BTreeMap<Value, Value>) -> Result<Value, VmError> {
    Ok(Value::Map(Arc::new(std::mem::take(out))))
}

fn unit<S>(_: &mut S) -> Result<Value, VmError> {
    Ok(Value::Unit)
}

fn take_acc(acc: &mut Value) -> Result<Value, VmError> {
    Ok(std::mem::replace(acc, Value::Unit))
}

fn too_long(name: &str) -> VmError {
    VmError::new(format!(
        "{name}: accumulated result exceeds maximum list length of {MAX_RANGE_MATERIALIZE} elements"
    ))
}

fn flat_map_step(out: &mut Vec<Value>, _item: Value, result: Value) -> Flow {
    match result {
        Value::List(inner) => {
            // The cap applies to the final list size.
            if out.len().saturating_add(inner.len()) > MAX_RANGE_MATERIALIZE {
                return Err(too_long("list.flat_map"));
            }
            out.extend(inner.iter().cloned());
        }
        Value::Range(lo, hi) => {
            // Check that this range fits the cap before materializing
            // it, alone and added to the list so far: a callback that
            // returns `0..i64::MAX` must not exhaust memory.
            let range_len = checked_range_len(lo, hi)
                .map_err(|m| VmError::new(format!("list.flat_map: {m}")))?;
            if out.len().saturating_add(range_len) > MAX_RANGE_MATERIALIZE {
                return Err(too_long("list.flat_map"));
            }
            if lo <= hi {
                out.extend((lo..=hi).map(Value::Int));
            }
        }
        other => {
            if out.len() >= MAX_RANGE_MATERIALIZE {
                return Err(too_long("list.flat_map"));
            }
            out.push(other);
        }
    }
    next()
}

fn filter_map_step(out: &mut Vec<Value>, _item: Value, result: Value) -> Flow {
    match result {
        Value::Variant(ref tag, ref fields) if tag.is(bv::SOME) && fields.len() == 1 => {
            out.push(fields[0].clone());
        }
        Value::Variant(ref tag, _) if tag.is(bv::NONE) => {}
        other => out.push(other),
    }
    next()
}

fn fold_until_step(acc: &mut Value, _item: Value, result: Value) -> Flow {
    match result {
        Value::Variant(ref tag, ref fields) if tag.is(bv::CONTINUE) && fields.len() == 1 => {
            *acc = fields[0].clone();
            next()
        }
        Value::Variant(ref tag, ref fields) if tag.is(bv::STOP) && fields.len() == 1 => {
            stop(fields[0].clone())
        }
        other => {
            *acc = other;
            next()
        }
    }
}

/// The key a function gave for an item is compared, or is a map's key:
/// a key with a function in it would be ordered by the address of the
/// function, differently from run to run. Same policy as
/// [`ensure_no_fn`]; the signatures of these builtins have no bound, so
/// the typechecker does not reject such keys. Locked by
/// tests/lang/collection_fn_gate_sibling_surfaces_tests.rs.
fn key_without_fn(name: &str, trait_name: &str, key: &Value) -> Result<(), VmError> {
    ensure_no_fn(name, trait_name, &[key])
}

type Best = Option<(Value, Value)>;

/// `list.min_by` / `list.max_by`: keep the item whose key is `wanted`
/// against the best key so far.
fn best_step(
    name: &str,
    wanted: std::cmp::Ordering,
    best: &mut Best,
    item: Value,
    key: Value,
) -> Flow {
    key_without_fn(name, "Compare", &key)?;
    *best = Some(match best.take() {
        Some((best_key, best_item))
            if key
                .partial_cmp(&best_key)
                .unwrap_or(std::cmp::Ordering::Equal)
                != wanted =>
        {
            (best_key, best_item)
        }
        _ => (key, item),
    });
    next()
}

fn best_item(best: &mut Best) -> Result<Value, VmError> {
    Ok(match best.take() {
        Some((_, item)) => Value::variant(bv::SOME, vec![item]),
        None => Value::variant(bv::NONE, Vec::new()),
    })
}

/// `list.unfold(seed, f)`: `f` is called with the state until it gives
/// no next one.
struct Unfold {
    state: Value,
    out: Vec<Value>,
    callback: Value,
    called: bool,
}

impl Native for Unfold {
    fn name(&self) -> &str {
        "list.unfold"
    }

    fn resume(&mut self, vm: &mut Vm, input: Value) -> Result<Step, VmError> {
        if self.called {
            match input {
                Value::Variant(ref tag, ref fields) if tag.is(bv::SOME) && fields.len() == 1 => {
                    match &fields[0] {
                        Value::Tuple(pair) if pair.len() == 2 => {
                            self.out.push(pair[0].clone());
                            if self.out.len() > MAX_RANGE_MATERIALIZE {
                                return Err(too_long("list.unfold"));
                            }
                            self.state = pair[1].clone();
                        }
                        other => {
                            self.out.push(other.clone());
                            return as_list(&mut self.out).map(Step::Done);
                        }
                    }
                }
                Value::Variant(ref tag, _) if tag.is(bv::NONE) => {
                    return as_list(&mut self.out).map(Step::Done);
                }
                other => {
                    self.out.push(other);
                    return as_list(&mut self.out).map(Step::Done);
                }
            }
        }
        self.called = true;
        Ok(vm.call(self.callback.clone(), [self.state.clone()]))
    }
}

fn arity(name: &str, args: &[Value], count: usize) -> Result<(), VmError> {
    if args.len() == count {
        Ok(())
    } else {
        Err(VmError::new(format!("{name} takes {count} arguments")))
    }
}

/// Dispatch `list.<name>(args)`.
pub(crate) fn call_list(_vm: &mut Vm, name: &str, args: &[Value]) -> Result<Step, VmError> {
    // `list.f(xs, f)` and `list.f(xs, init, f)`: the items and the
    // function.
    let items = |full: &str, count: usize| -> Result<(Vec<Value>, Value), VmError> {
        arity(full, args, count)?;
        Ok((materialize_iter(&args[0], full)?, args[count - 1].clone()))
    };
    Ok(match name {
        "map" => {
            let (xs, f) = items("list.map", 2)?;
            iterate(
                "list.map",
                xs,
                f,
                Vec::new(),
                item_arg,
                keep_result,
                as_list,
            )
        }
        "filter" => {
            let (xs, f) = items("list.filter", 2)?;
            iterate(
                "list.filter",
                xs,
                f,
                Vec::new(),
                item_arg,
                keep_item_if,
                as_list,
            )
        }
        "each" => {
            let (xs, f) = items("list.each", 2)?;
            iterate("list.each", xs, f, (), item_arg, ignore, unit)
        }
        "fold" => {
            let (xs, f) = items("list.fold", 3)?;
            iterate(
                "list.fold",
                xs,
                f,
                args[1].clone(),
                acc_and_item,
                set_acc,
                take_acc,
            )
        }
        "find" => {
            let (xs, f) = items("list.find", 2)?;
            iterate(
                "list.find",
                xs,
                f,
                (),
                item_arg,
                |_, item, found| match truthy(&found) {
                    true => stop(Value::variant(bv::SOME, vec![item])),
                    false => next(),
                },
                |_| Ok(Value::variant(bv::NONE, Vec::new())),
            )
        }
        "any" => {
            let (xs, f) = items("list.any", 2)?;
            iterate(
                "list.any",
                xs,
                f,
                (),
                item_arg,
                |_, _, holds| match truthy(&holds) {
                    true => stop(Value::Bool(true)),
                    false => next(),
                },
                |_| Ok(Value::Bool(false)),
            )
        }
        "all" => {
            let (xs, f) = items("list.all", 2)?;
            iterate(
                "list.all",
                xs,
                f,
                (),
                item_arg,
                |_, _, holds| match truthy(&holds) {
                    true => next(),
                    false => stop(Value::Bool(false)),
                },
                |_| Ok(Value::Bool(true)),
            )
        }
        "flat_map" => {
            let (xs, f) = items("list.flat_map", 2)?;
            iterate(
                "list.flat_map",
                xs,
                f,
                Vec::new(),
                item_arg,
                flat_map_step,
                as_list,
            )
        }
        "filter_map" => {
            let (xs, f) = items("list.filter_map", 2)?;
            iterate(
                "list.filter_map",
                xs,
                f,
                Vec::new(),
                item_arg,
                filter_map_step,
                as_list,
            )
        }
        "sort_by" => {
            let (xs, f) = items("list.sort_by", 2)?;
            iterate(
                "list.sort_by",
                xs,
                f,
                Vec::<(Value, Value)>::new(),
                item_arg,
                |keyed, item, key| {
                    keyed.push((key, item));
                    next()
                },
                |keyed| {
                    keyed.sort_by(|(a, _), (b, _)| {
                        a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
                    });
                    let sorted = std::mem::take(keyed).into_iter().map(|(_, item)| item);
                    Ok(Value::List(Arc::new(sorted.collect())))
                },
            )
        }
        "fold_until" => {
            let (xs, f) = items("list.fold_until", 3)?;
            iterate(
                "list.fold_until",
                xs,
                f,
                args[1].clone(),
                acc_and_item,
                fold_until_step,
                take_acc,
            )
        }
        "unfold" => {
            arity("list.unfold", args, 2)?;
            Step::Run(Box::new(Unfold {
                state: args[0].clone(),
                out: Vec::new(),
                callback: args[1].clone(),
                called: false,
            }))
        }
        "group_by" => {
            let (xs, f) = items("list.group_by", 2)?;
            iterate(
                "list.group_by",
                xs,
                f,
                BTreeMap::<Value, Vec<Value>>::new(),
                item_arg,
                |groups, item, key| {
                    key_without_fn("list.group_by", "Hash", &key)?;
                    groups.entry(key).or_default().push(item);
                    next()
                },
                |groups| {
                    let groups = std::mem::take(groups).into_iter();
                    Ok(Value::Map(Arc::new(
                        groups.map(|(k, v)| (k, Value::List(Arc::new(v)))).collect(),
                    )))
                },
            )
        }
        "min_by" => {
            let (xs, f) = items("list.min_by", 2)?;
            iterate(
                "list.min_by",
                xs,
                f,
                None,
                item_arg,
                |best, item, key| {
                    best_step("list.min_by", std::cmp::Ordering::Less, best, item, key)
                },
                best_item,
            )
        }
        "max_by" => {
            let (xs, f) = items("list.max_by", 2)?;
            iterate(
                "list.max_by",
                xs,
                f,
                None,
                item_arg,
                |best, item, key| {
                    best_step("list.max_by", std::cmp::Ordering::Greater, best, item, key)
                },
                best_item,
            )
        }
        "scan" => {
            // The running value, and the list of the values it had,
            // the initial one first.
            let (xs, f) = items("list.scan", 3)?;
            iterate(
                "list.scan",
                xs,
                f,
                (args[1].clone(), vec![args[1].clone()]),
                |(running, _), item, stack| acc_and_item(running, item, stack),
                |(running, prefix), _, result| {
                    *running = result.clone();
                    prefix.push(result);
                    if prefix.len() > MAX_RANGE_MATERIALIZE {
                        return Err(too_long("list.scan"));
                    }
                    next()
                },
                |(_, prefix)| as_list(prefix),
            )
        }
        _ => Step::Done(list_plain(name, args)?),
    })
}

/// Dispatch `map.<name>(args)`.
pub(crate) fn call_map(_vm: &mut Vm, name: &str, args: &[Value]) -> Result<Step, VmError> {
    // `map.f(m, f)`: the entries and the function.
    let items = |full: &str| -> Result<(Vec<Value>, Value), VmError> {
        arity(full, args, 2)?;
        match &args[0] {
            Value::Map(m) => Ok((entries(m), args[1].clone())),
            _ => Err(VmError::new(format!("{full} requires a map"))),
        }
    };
    Ok(match name {
        "filter" => {
            let (xs, f) = items("map.filter")?;
            iterate(
                "map.filter",
                xs,
                f,
                BTreeMap::new(),
                key_and_value,
                |kept, entry, keep| {
                    if truthy(&keep)
                        && let Value::Tuple(pair) = entry
                        && let Ok([k, v]) = <[Value; 2]>::try_from(pair)
                    {
                        kept.insert(k, v);
                    }
                    next()
                },
                as_map,
            )
        }
        "map" => {
            let (xs, f) = items("map.map")?;
            iterate(
                "map.map",
                xs,
                f,
                BTreeMap::new(),
                key_and_value,
                |out, _, result| {
                    let pair = match result {
                        Value::Tuple(pair) => <[Value; 2]>::try_from(pair).ok(),
                        _ => None,
                    };
                    let [k, v] = pair.ok_or_else(|| {
                        VmError::new("map.map callback must return a (key, value) tuple".into())
                    })?;
                    out.insert(k, v);
                    next()
                },
                as_map,
            )
        }
        "each" => {
            let (xs, f) = items("map.each")?;
            iterate("map.each", xs, f, (), key_and_value, ignore, unit)
        }
        "update" => {
            if args.len() != 4 {
                return Err(VmError::new(
                    "map.update takes 4 arguments (map, key, default, fn)".into(),
                ));
            }
            let Value::Map(m) = &args[0] else {
                return Err(VmError::new("map.update requires a map".into()));
            };
            let key = args[1].clone();
            // Runtime Fn gate on the KEY only — same rationale as the
            // `map.from_entries` gate (its signature has no bound, so a
            // Fn key typechecks and both the `m.get` probe and the
            // `insert` below would compare it by Arc pointer address).
            // The default and the callback result are map VALUES and
            // stay ungated.
            ensure_no_fn("map.update", "Hash", &[&key])?;
            let current = m.get(&key).unwrap_or(&args[2]).clone();
            let m = m.clone();
            call_then("map.update", args[3].clone(), current, move |new_val| {
                let mut new_map = (*m).clone();
                new_map.insert(key, new_val);
                Ok(Value::Map(Arc::new(new_map)))
            })
        }
        _ => Step::Done(map_plain(name, args)?),
    })
}

/// Dispatch `set.<name>(args)`.
pub(crate) fn call_set(_vm: &mut Vm, name: &str, args: &[Value]) -> Result<Step, VmError> {
    // `set.f(s, f)` and `set.f(s, init, f)`: the elements and the
    // function.
    let items = |full: &str, count: usize| -> Result<(Vec<Value>, Value), VmError> {
        arity(full, args, count)?;
        match &args[0] {
            Value::Set(s) => Ok((s.iter().cloned().collect(), args[count - 1].clone())),
            _ => Err(VmError::new(format!("{full} requires a set"))),
        }
    };
    Ok(match name {
        "map" => {
            let (xs, f) = items("set.map", 2)?;
            iterate("set.map", xs, f, Vec::new(), item_arg, keep_result, as_set)
        }
        "filter" => {
            let (xs, f) = items("set.filter", 2)?;
            iterate(
                "set.filter",
                xs,
                f,
                Vec::new(),
                item_arg,
                keep_item_if,
                as_set,
            )
        }
        "each" => {
            let (xs, f) = items("set.each", 2)?;
            iterate("set.each", xs, f, (), item_arg, ignore, unit)
        }
        "fold" => {
            let (xs, f) = items("set.fold", 3)?;
            iterate(
                "set.fold",
                xs,
                f,
                args[1].clone(),
                acc_and_item,
                set_acc,
                take_acc,
            )
        }
        _ => Step::Done(set_plain(name, args)?),
    })
}

/// The `list` functions that call no function.
fn list_plain(name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        // Non-closure list builtins
        "zip" => {
            if args.len() != 2 {
                return Err(VmError::new("list.zip takes 2 arguments".into()));
            }
            // Cap check: the result length is bounded by the minimum of the
            // two input lengths. Compute expected lengths using `u128` for
            // ranges (to avoid `usize` overflow on e.g. `0..i64::MAX`), then
            // enforce `MAX_RANGE_MATERIALIZE`. Without this guard,
            // `Vec::with_capacity(usize::MAX)` panics opaquely as
            // "builtin module 'list' panicked".
            fn zip_input_len(v: &Value) -> Result<u128, VmError> {
                match v {
                    Value::List(xs) => Ok(xs.len() as u128),
                    Value::Range(lo, hi) => {
                        if hi < lo {
                            Ok(0)
                        } else {
                            Ok((*hi as i128 - *lo as i128 + 1) as u128)
                        }
                    }
                    _ => Err(VmError::new("list.zip requires a list or range".into())),
                }
            }
            let len_a = zip_input_len(&args[0])?;
            let len_b = zip_input_len(&args[1])?;
            let expected = len_a.min(len_b);
            if expected > MAX_RANGE_MATERIALIZE as u128 {
                return Err(VmError::new(format!(
                    "list.zip: result length {expected} exceeds maximum materialized length {MAX_RANGE_MATERIALIZE}"
                )));
            }
            let cap = expected as usize;
            let mut a = ValueIter::try_from(&args[0], "list.zip")?;
            let mut b = ValueIter::try_from(&args[1], "list.zip")?;
            let mut pairs = Vec::with_capacity(cap);
            while let (Some(x), Some(y)) = (a.next(), b.next()) {
                pairs.push(Value::Tuple(vec![x, y]));
            }
            Ok(Value::List(Arc::new(pairs)))
        }
        "flatten" => {
            if args.len() != 1 {
                return Err(VmError::new("list.flatten takes 1 argument".into()));
            }
            let iter = ValueIter::try_from(&args[0], "list.flatten")?;
            let mut result = Vec::new();
            for item in iter {
                match item {
                    Value::List(inner) => result.extend(inner.iter().cloned()),
                    Value::Range(lo, hi) => {
                        checked_range_len(lo, hi).map_err(VmError::new)?;
                        for i in lo..=hi {
                            result.push(Value::Int(i));
                        }
                    }
                    other => result.push(other),
                }
                if result.len() > MAX_RANGE_MATERIALIZE {
                    return Err(VmError::new(format!(
                        "list.flatten: accumulated result exceeds maximum list length of {} elements",
                        MAX_RANGE_MATERIALIZE
                    )));
                }
            }
            Ok(Value::List(Arc::new(result)))
        }
        "head" => {
            if args.len() != 1 {
                return Err(VmError::new("list.head takes 1 argument".into()));
            }
            match &args[0] {
                Value::List(xs) => match xs.first() {
                    Some(val) => Ok(Value::variant(bv::SOME, vec![val.clone()])),
                    None => Ok(Value::variant(bv::NONE, Vec::new())),
                },
                Value::Range(lo, hi) => {
                    if lo <= hi {
                        Ok(Value::variant(bv::SOME, vec![Value::Int(*lo)]))
                    } else {
                        Ok(Value::variant(bv::NONE, Vec::new()))
                    }
                }
                _ => Err(VmError::new("list.head requires a list or range".into())),
            }
        }
        "tail" => {
            if args.len() != 1 {
                return Err(VmError::new("list.tail takes 1 argument".into()));
            }
            match &args[0] {
                Value::List(xs) => {
                    if xs.is_empty() {
                        Ok(Value::List(Arc::new(Vec::new())))
                    } else {
                        Ok(Value::List(Arc::new(xs[1..].to_vec())))
                    }
                }
                Value::Range(lo, hi) => {
                    if lo >= hi {
                        Ok(Value::List(Arc::new(Vec::new())))
                    } else {
                        Ok(Value::Range(lo + 1, *hi))
                    }
                }
                _ => Err(VmError::new("list.tail requires a list or range".into())),
            }
        }
        "last" => {
            if args.len() != 1 {
                return Err(VmError::new("list.last takes 1 argument".into()));
            }
            match &args[0] {
                Value::List(xs) => match xs.last() {
                    Some(val) => Ok(Value::variant(bv::SOME, vec![val.clone()])),
                    None => Ok(Value::variant(bv::NONE, Vec::new())),
                },
                Value::Range(lo, hi) => {
                    if lo <= hi {
                        Ok(Value::variant(bv::SOME, vec![Value::Int(*hi)]))
                    } else {
                        Ok(Value::variant(bv::NONE, Vec::new()))
                    }
                }
                _ => Err(VmError::new("list.last requires a list or range".into())),
            }
        }
        "reverse" => {
            if args.len() != 1 {
                return Err(VmError::new("list.reverse takes 1 argument".into()));
            }
            // Range fast path: iterate backwards without materializing then reversing.
            if let Value::Range(lo, hi) = &args[0] {
                checked_range_len(*lo, *hi).map_err(VmError::new)?;
                let items: Vec<Value> = (*lo..=*hi).rev().map(Value::Int).collect();
                return Ok(Value::List(Arc::new(items)));
            }
            let mut v: Vec<Value> = ValueIter::try_from(&args[0], "list.reverse")?.collect_vec()?;
            v.reverse();
            Ok(Value::List(Arc::new(v)))
        }
        "sort" => {
            if args.len() != 1 {
                return Err(VmError::new("list.sort takes 1 argument".into()));
            }
            // Range is already sorted — return as-is.
            if matches!(&args[0], Value::Range(..)) {
                return Ok(args[0].clone());
            }
            // Fn elements would sort by Arc pointer address (ASLR-
            // nondeterministic) — reject like the operator gates do.
            ensure_no_fn("list.sort", "Compare", &[&args[0]])?;
            let mut v: Vec<Value> = ValueIter::try_from(&args[0], "list.sort")?.collect_vec()?;
            v.sort();
            Ok(Value::List(Arc::new(v)))
        }
        "unique" => {
            if args.len() != 1 {
                return Err(VmError::new("list.unique takes 1 argument".into()));
            }
            // Range has no duplicates — return as-is.
            if matches!(&args[0], Value::Range(..)) {
                return Ok(args[0].clone());
            }
            // Fn elements would dedup by identity (Arc pointer / builtin
            // name) instead of erroring like `f == g` does.
            ensure_no_fn("list.unique", "Equal", &[&args[0]])?;
            let iter = ValueIter::try_from(&args[0], "list.unique")?;
            let mut seen = BTreeSet::new();
            let mut result = Vec::new();
            for x in iter {
                if seen.insert(x.clone()) {
                    result.push(x);
                }
            }
            Ok(Value::List(Arc::new(result)))
        }
        "contains" => {
            if args.len() != 2 {
                return Err(VmError::new("list.contains takes 2 arguments".into()));
            }
            // Fn membership would silently answer via Arc identity
            // (`list.contains([f], g)` -> false) instead of erroring.
            ensure_no_fn("list.contains", "Equal", &[&args[0], &args[1]])?;
            match &args[0] {
                Value::List(xs) => Ok(Value::Bool(xs.contains(&args[1]))),
                Value::Range(lo, hi) => {
                    if let Value::Int(n) = &args[1] {
                        Ok(Value::Bool(*n >= *lo && *n <= *hi))
                    } else {
                        Ok(Value::Bool(false))
                    }
                }
                _ => Err(VmError::new(
                    "list.contains requires a list or range".into(),
                )),
            }
        }
        "length" => {
            if args.len() != 1 {
                return Err(VmError::new("list.length takes 1 argument".into()));
            }
            // Ranges can describe spans larger than `i64::MAX`
            // (e.g. `i64::MIN..i64::MAX` has `u64::MAX + 1` elements).
            // Previously this went through `collection_len -> usize as
            // i64`, which wrapped to `i64::MIN` on 64-bit platforms.
            // Surface a clean overflow error instead.
            match &args[0] {
                Value::List(xs) => {
                    let len = xs.len();
                    i64::try_from(len).map(Value::Int).map_err(|_| {
                        VmError::new(
                            "list.length overflow: list too large to represent as Int".into(),
                        )
                    })
                }
                Value::Range(lo, hi) => {
                    if hi < lo {
                        Ok(Value::Int(0))
                    } else {
                        // Compute in i128 so we can detect spans that
                        // exceed i64::MAX without losing precision.
                        let span = (*hi as i128) - (*lo as i128) + 1;
                        i64::try_from(span).map(Value::Int).map_err(|_| {
                            VmError::new(
                                "list.length overflow: range too large to represent as Int".into(),
                            )
                        })
                    }
                }
                _ => Err(VmError::new("list.length requires a list or range".into())),
            }
        }
        "append" => {
            if args.len() != 2 {
                return Err(VmError::new("list.append takes 2 arguments".into()));
            }
            let mut v = ValueIter::try_from(&args[0], "list.append")?.collect_vec()?;
            v.push(args[1].clone());
            Ok(Value::List(Arc::new(v)))
        }
        "prepend" => {
            if args.len() != 2 {
                return Err(VmError::new("list.prepend takes 2 arguments".into()));
            }
            let mut v = ValueIter::try_from(&args[0], "list.prepend")?.collect_vec()?;
            v.insert(0, args[1].clone());
            Ok(Value::List(Arc::new(v)))
        }
        "concat" => {
            if args.len() != 2 {
                return Err(VmError::new("list.concat takes 2 arguments".into()));
            }
            let a = ValueIter::try_from(&args[0], "list.concat")?;
            let b = ValueIter::try_from(&args[1], "list.concat")?;
            if let Value::Range(lo, hi) = &args[0] {
                checked_range_len(*lo, *hi).map_err(VmError::new)?;
            }
            if let Value::Range(lo, hi) = &args[1] {
                checked_range_len(*lo, *hi).map_err(VmError::new)?;
            }
            let mut result = Vec::with_capacity(a.len() + b.len());
            result.extend(a);
            result.extend(b);
            if result.len() > MAX_RANGE_MATERIALIZE {
                return Err(VmError::new(format!(
                    "concatenated list exceeds maximum size of {} elements",
                    MAX_RANGE_MATERIALIZE
                )));
            }
            Ok(Value::List(Arc::new(result)))
        }
        "get" => {
            if args.len() != 2 {
                return Err(VmError::new("list.get takes 2 arguments".into()));
            }
            let Value::Int(n) = &args[1] else {
                return Err(VmError::new(format!(
                    "list.get requires Int, got {}",
                    value_kind(&args[1])
                )));
            };
            let n_val = *n;
            if n_val < 0 {
                return Err(VmError::new(format!("list.get: negative index {n_val}")));
            }
            let idx = n_val as usize;
            match &args[0] {
                Value::List(xs) => match xs.get(idx) {
                    Some(val) => Ok(Value::variant(bv::SOME, vec![val.clone()])),
                    None => Ok(Value::variant(bv::NONE, Vec::new())),
                },
                Value::Range(lo, hi) => {
                    let i = match lo.checked_add(idx as i64) {
                        Some(i) => i,
                        None => return Ok(Value::variant(bv::NONE, Vec::new())),
                    };
                    if i <= *hi {
                        Ok(Value::variant(bv::SOME, vec![Value::Int(i)]))
                    } else {
                        Ok(Value::variant(bv::NONE, Vec::new()))
                    }
                }
                _ => Err(VmError::new("list.get requires a list or range".into())),
            }
        }
        "set" => {
            if args.len() != 3 {
                return Err(VmError::new("list.set takes 3 arguments".into()));
            }
            let mut v = ValueIter::try_from(&args[0], "list.set")?.collect_vec()?;
            let Value::Int(n) = &args[1] else {
                return Err(VmError::new(format!(
                    "list.set requires Int, got {}",
                    value_kind(&args[1])
                )));
            };
            let n_val = *n;
            if n_val < 0 {
                return Err(VmError::new(format!("list.set: negative index {n_val}")));
            }
            let idx = n_val as usize;
            if idx >= v.len() {
                return Err(VmError::new("list.set index out of bounds".into()));
            }
            v[idx] = args[2].clone();
            Ok(Value::List(Arc::new(v)))
        }
        "take" => {
            if args.len() != 2 {
                return Err(VmError::new("list.take takes 2 arguments".into()));
            }
            let Value::Int(n) = &args[1] else {
                return Err(VmError::new(format!(
                    "list.take requires Int, got {}",
                    value_kind(&args[1])
                )));
            };
            let n_val = *n;
            if n_val < 0 {
                return Err(VmError::new(format!("list.take: negative count {n_val}")));
            }
            match &args[0] {
                Value::List(xs) => {
                    let n = (n_val as usize).min(xs.len());
                    Ok(Value::List(Arc::new(xs[..n].to_vec())))
                }
                Value::Range(lo, hi) => {
                    // Short-circuit zero count: without this, `lo.checked_add(0)
                    // .and_then(|v| v.checked_sub(1))` returns `None` when
                    // `lo == i64::MIN` because `i64::MIN - 1` underflows, and
                    // the old fallback returned the full range instead of an
                    // empty list. Taking zero elements must always yield an
                    // empty result regardless of `lo`.
                    let count = n_val;
                    if count == 0 {
                        return Ok(Value::List(Arc::new(Vec::new())));
                    }
                    let new_hi = match lo.checked_add(count).and_then(|v| v.checked_sub(1)) {
                        Some(v) => v.min(*hi),
                        None => *hi,
                    };
                    if new_hi < *lo {
                        Ok(Value::List(Arc::new(Vec::new())))
                    } else {
                        Ok(Value::Range(*lo, new_hi))
                    }
                }
                _ => Err(VmError::new("list.take requires a list or range".into())),
            }
        }
        "drop" => {
            if args.len() != 2 {
                return Err(VmError::new("list.drop takes 2 arguments".into()));
            }
            let Value::Int(n) = &args[1] else {
                return Err(VmError::new(format!(
                    "list.drop requires Int, got {}",
                    value_kind(&args[1])
                )));
            };
            let n_val = *n;
            if n_val < 0 {
                return Err(VmError::new(format!("list.drop: negative count {n_val}")));
            }
            match &args[0] {
                Value::List(xs) => {
                    let n = (n_val as usize).min(xs.len());
                    Ok(Value::List(Arc::new(xs[n..].to_vec())))
                }
                Value::Range(lo, hi) => {
                    let new_lo = match lo.checked_add(n_val) {
                        Some(v) => v,
                        None => return Ok(Value::List(Arc::new(Vec::new()))),
                    };
                    if new_lo > *hi {
                        Ok(Value::List(Arc::new(Vec::new())))
                    } else {
                        Ok(Value::Range(new_lo, *hi))
                    }
                }
                _ => Err(VmError::new("list.drop requires a list or range".into())),
            }
        }
        "enumerate" => {
            if args.len() != 1 {
                return Err(VmError::new("list.enumerate takes 1 argument".into()));
            }
            if let Value::Range(lo, hi) = &args[0] {
                checked_range_len(*lo, *hi).map_err(VmError::new)?;
            }
            let iter = ValueIter::try_from(&args[0], "list.enumerate")?;
            let mut result = Vec::with_capacity(iter.len());
            for (i, v) in iter.enumerate() {
                result.push(Value::Tuple(vec![Value::Int(i as i64), v]));
            }
            Ok(Value::List(Arc::new(result)))
        }
        "index_of" => {
            if args.len() != 2 {
                return Err(VmError::new("list.index_of takes 2 arguments".into()));
            }
            // Fn search would silently answer via Arc identity
            // (`list.index_of([f, g], g)` -> Some(1)) instead of erroring.
            ensure_no_fn("list.index_of", "Equal", &[&args[0], &args[1]])?;
            let iter = ValueIter::try_from(&args[0], "list.index_of")?;
            let target = &args[1];
            for (i, v) in iter.enumerate() {
                if &v == target {
                    let idx = i64::try_from(i).map_err(|_| {
                        VmError::new(
                            "list.index_of overflow: index too large to represent as Int".into(),
                        )
                    })?;
                    return Ok(Value::variant(bv::SOME, vec![Value::Int(idx)]));
                }
            }
            Ok(Value::variant(bv::NONE, Vec::new()))
        }
        "remove_at" => {
            if args.len() != 2 {
                return Err(VmError::new("list.remove_at takes 2 arguments".into()));
            }
            let Value::Int(n) = &args[1] else {
                return Err(VmError::new(format!(
                    "list.remove_at requires Int, got {}",
                    value_kind(&args[1])
                )));
            };
            let n_val = *n;
            if n_val < 0 {
                return Err(VmError::new(format!(
                    "list.remove_at: negative index {n_val}"
                )));
            }
            let mut v = ValueIter::try_from(&args[0], "list.remove_at")?.collect_vec()?;
            let idx = n_val as usize;
            if idx >= v.len() {
                return Err(VmError::new("list.remove_at index out of bounds".into()));
            }
            v.remove(idx);
            Ok(Value::List(Arc::new(v)))
        }
        "sum" => {
            if args.len() != 1 {
                return Err(VmError::new("list.sum takes 1 argument".into()));
            }
            let iter = ValueIter::try_from(&args[0], "list.sum")?;
            let mut total: i64 = 0;
            for v in iter {
                match v {
                    Value::Int(n) => {
                        total = total
                            .checked_add(n)
                            .ok_or_else(|| VmError::new("list.sum overflow".into()))?;
                    }
                    _ => return Err(VmError::new("list.sum requires a list of Int".into())),
                }
            }
            Ok(Value::Int(total))
        }
        "sum_float" => {
            if args.len() != 1 {
                return Err(VmError::new("list.sum_float takes 1 argument".into()));
            }
            let iter = ValueIter::try_from(&args[0], "list.sum_float")?;
            let mut total: f64 = 0.0;
            for v in iter {
                match v {
                    Value::Float(n) => total += n,
                    _ => {
                        return Err(VmError::new(
                            "list.sum_float requires a list of Float".into(),
                        ));
                    }
                }
            }
            // A finite sum only ever overflows to ±inf, and the sum
            // cannot come back from there, so one check at the end sees it.
            crate::builtins::numeric::checked_float(total, || "list.sum_float overflow".into())
        }
        "product" => {
            if args.len() != 1 {
                return Err(VmError::new("list.product takes 1 argument".into()));
            }
            let iter = ValueIter::try_from(&args[0], "list.product")?;
            let mut total: i64 = 1;
            for v in iter {
                match v {
                    Value::Int(n) => {
                        total = total
                            .checked_mul(n)
                            .ok_or_else(|| VmError::new("list.product overflow".into()))?;
                    }
                    _ => {
                        return Err(VmError::new("list.product requires a list of Int".into()));
                    }
                }
            }
            Ok(Value::Int(total))
        }
        "product_float" => {
            if args.len() != 1 {
                return Err(VmError::new("list.product_float takes 1 argument".into()));
            }
            let iter = ValueIter::try_from(&args[0], "list.product_float")?;
            let mut total: f64 = 1.0;
            for v in iter {
                match v {
                    Value::Float(n) => total *= n,
                    _ => {
                        return Err(VmError::new(
                            "list.product_float requires a list of Float".into(),
                        ));
                    }
                }
            }
            // As in `sum_float`: once the product leaves the finite range
            // it stays out, so one check at the end sees it.
            crate::builtins::numeric::checked_float(total, || "list.product_float overflow".into())
        }
        "intersperse" => {
            if args.len() != 2 {
                return Err(VmError::new("list.intersperse takes 2 arguments".into()));
            }
            let items = ValueIter::try_from(&args[0], "list.intersperse")?.collect_vec()?;
            let sep = &args[1];
            if items.len() <= 1 {
                return Ok(Value::List(Arc::new(items)));
            }
            // Result length = 2*N - 1
            let out_len = items.len() * 2 - 1;
            if out_len > MAX_RANGE_MATERIALIZE {
                return Err(VmError::new(format!(
                    "list.intersperse: result length {out_len} exceeds maximum materialized length {MAX_RANGE_MATERIALIZE}"
                )));
            }
            let mut result = Vec::with_capacity(out_len);
            let mut iter = items.into_iter();
            if let Some(first) = iter.next() {
                result.push(first);
            }
            for v in iter {
                result.push(sep.clone());
                result.push(v);
            }
            Ok(Value::List(Arc::new(result)))
        }
        _ => Err(VmError::new(format!("unknown list function: {name}"))),
    }
}

/// The `map` functions that call no function.
fn map_plain(name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "get" => {
            if args.len() != 2 {
                return Err(VmError::new("map.get takes 2 arguments".into()));
            }
            let Value::Map(m) = &args[0] else {
                return Err(VmError::new("map.get requires a map".into()));
            };
            match m.get(&args[1]) {
                Some(val) => Ok(Value::variant(bv::SOME, vec![val.clone()])),
                None => Ok(Value::variant(bv::NONE, Vec::new())),
            }
        }
        "set" => {
            if args.len() != 3 {
                return Err(VmError::new("map.set takes 3 arguments".into()));
            }
            let Value::Map(m) = &args[0] else {
                return Err(VmError::new("map.set requires a map".into()));
            };
            let mut new_map = (**m).clone();
            new_map.insert(args[1].clone(), args[2].clone());
            Ok(Value::Map(Arc::new(new_map)))
        }
        "delete" => {
            if args.len() != 2 {
                return Err(VmError::new("map.delete takes 2 arguments".into()));
            }
            let Value::Map(m) = &args[0] else {
                return Err(VmError::new("map.delete requires a map".into()));
            };
            let mut new_map = (**m).clone();
            new_map.remove(&args[1]);
            Ok(Value::Map(Arc::new(new_map)))
        }
        "contains" => {
            if args.len() != 2 {
                return Err(VmError::new("map.contains takes 2 arguments".into()));
            }
            let Value::Map(m) = &args[0] else {
                return Err(VmError::new("map.contains requires a map".into()));
            };
            Ok(Value::Bool(m.contains_key(&args[1])))
        }
        "keys" => {
            if args.len() != 1 {
                return Err(VmError::new("map.keys takes 1 argument".into()));
            }
            let Value::Map(m) = &args[0] else {
                return Err(VmError::new("map.keys requires a map".into()));
            };
            Ok(Value::List(Arc::new(m.keys().cloned().collect())))
        }
        "values" => {
            if args.len() != 1 {
                return Err(VmError::new("map.values takes 1 argument".into()));
            }
            let Value::Map(m) = &args[0] else {
                return Err(VmError::new("map.values requires a map".into()));
            };
            Ok(Value::List(Arc::new(m.values().cloned().collect())))
        }
        "length" => {
            if args.len() != 1 {
                return Err(VmError::new("map.length takes 1 argument".into()));
            }
            let Value::Map(m) = &args[0] else {
                return Err(VmError::new("map.length requires a map".into()));
            };
            Ok(Value::Int(m.len() as i64))
        }
        "merge" => {
            if args.len() != 2 {
                return Err(VmError::new("map.merge takes 2 arguments".into()));
            }
            let (Value::Map(m1), Value::Map(m2)) = (&args[0], &args[1]) else {
                return Err(VmError::new("map.merge requires maps".into()));
            };
            let mut result = (**m1).clone();
            for (k, v) in m2.iter() {
                result.insert(k.clone(), v.clone());
            }
            Ok(Value::Map(Arc::new(result)))
        }
        "entries" => {
            if args.len() != 1 {
                return Err(VmError::new("map.entries takes 1 argument".into()));
            }
            let Value::Map(m) = &args[0] else {
                return Err(VmError::new("map.entries requires a map".into()));
            };
            let entries: Vec<Value> = m
                .iter()
                .map(|(k, v)| Value::Tuple(vec![k.clone(), v.clone()]))
                .collect();
            Ok(Value::List(Arc::new(entries)))
        }
        "from_entries" => {
            if args.len() != 1 {
                return Err(VmError::new("map.from_entries takes 1 argument".into()));
            }
            let Value::List(xs) = &args[0] else {
                return Err(VmError::new("map.from_entries requires a list".into()));
            };
            let mut result = BTreeMap::new();
            for item in xs.iter() {
                if let Value::Tuple(pair) = item
                    && pair.len() == 2
                {
                    // Runtime Fn gate on the KEY only (values are never
                    // compared): `map.from_entries` has no `where`
                    // bound (its row in the builtin registry), unlike
                    // `map.get`/`set` which carry `k: Hash`, so Fn keys
                    // typecheck and would be BTreeMap-ordered by Arc
                    // pointer address — ASLR-nondeterministic entry
                    // order. The trait name matches the static map-key
                    // contract.
                    ensure_no_fn("map.from_entries", "Hash", &[&pair[0]])?;
                    result.insert(pair[0].clone(), pair[1].clone());
                    continue;
                }
                return Err(VmError::new(
                    "map.from_entries requires (key, value) tuples".into(),
                ));
            }
            Ok(Value::Map(Arc::new(result)))
        }
        _ => Err(VmError::new(format!("unknown map function: {name}"))),
    }
}

/// The `set` functions that call no function.
fn set_plain(name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "new" => Ok(Value::Set(Arc::new(BTreeSet::new()))),
        "from_list" => {
            if args.len() != 1 {
                return Err(VmError::new("set.from_list takes 1 argument".into()));
            }
            let Value::List(xs) = &args[0] else {
                return Err(VmError::new("set.from_list requires a list".into()));
            };
            // A set of Fn values is BTree-ordered by Arc pointer address —
            // ASLR-nondeterministic iteration order. Reject at construction.
            ensure_no_fn("set.from_list", "Compare", &[&args[0]])?;
            Ok(Value::Set(Arc::new(xs.iter().cloned().collect())))
        }
        "to_list" => {
            if args.len() != 1 {
                return Err(VmError::new("set.to_list takes 1 argument".into()));
            }
            let Value::Set(s) = &args[0] else {
                return Err(VmError::new("set.to_list requires a set".into()));
            };
            Ok(Value::List(Arc::new(s.iter().cloned().collect())))
        }
        "contains" => {
            if args.len() != 2 {
                return Err(VmError::new("set.contains takes 2 arguments".into()));
            }
            let Value::Set(s) = &args[0] else {
                return Err(VmError::new("set.contains requires a set".into()));
            };
            // Gate the probe only (keeps the lookup O(log n)): the
            // typechecker unifies the probe type with the element type
            // (`set.contains: (Set(a), a) -> Bool`), so a Fn-bearing set
            // can only be probed with a Fn-bearing value.
            ensure_no_fn("set.contains", "Compare", &[&args[1]])?;
            Ok(Value::Bool(s.contains(&args[1])))
        }
        "insert" => {
            if args.len() != 2 {
                return Err(VmError::new("set.insert takes 2 arguments".into()));
            }
            let Value::Set(s) = &args[0] else {
                return Err(VmError::new("set.insert requires a set".into()));
            };
            // See set.contains for why gating the inserted value alone
            // is sufficient.
            ensure_no_fn("set.insert", "Compare", &[&args[1]])?;
            let mut new_set = (**s).clone();
            new_set.insert(args[1].clone());
            Ok(Value::Set(Arc::new(new_set)))
        }
        "remove" => {
            if args.len() != 2 {
                return Err(VmError::new("set.remove takes 2 arguments".into()));
            }
            let Value::Set(s) = &args[0] else {
                return Err(VmError::new("set.remove requires a set".into()));
            };
            // See set.contains for why gating the removed value alone
            // is sufficient.
            ensure_no_fn("set.remove", "Compare", &[&args[1]])?;
            let mut new_set = (**s).clone();
            new_set.remove(&args[1]);
            Ok(Value::Set(Arc::new(new_set)))
        }
        "length" => {
            if args.len() != 1 {
                return Err(VmError::new("set.length takes 1 argument".into()));
            }
            let Value::Set(s) = &args[0] else {
                return Err(VmError::new("set.length requires a set".into()));
            };
            Ok(Value::Int(s.len() as i64))
        }
        "union" => {
            if args.len() != 2 {
                return Err(VmError::new("set.union takes 2 arguments".into()));
            }
            let (Value::Set(a), Value::Set(b)) = (&args[0], &args[1]) else {
                return Err(VmError::new("set.union requires sets".into()));
            };
            // Algebra ops walk both sets anyway, so gate both operands:
            // catches Fn-bearing sets built by ungated producers
            // (e.g. a `set.map` callback returning closures).
            ensure_no_fn("set.union", "Compare", &[&args[0], &args[1]])?;
            Ok(Value::Set(Arc::new(a.union(b).cloned().collect())))
        }
        "intersection" => {
            if args.len() != 2 {
                return Err(VmError::new("set.intersection takes 2 arguments".into()));
            }
            let (Value::Set(a), Value::Set(b)) = (&args[0], &args[1]) else {
                return Err(VmError::new("set.intersection requires sets".into()));
            };
            ensure_no_fn("set.intersection", "Compare", &[&args[0], &args[1]])?;
            Ok(Value::Set(Arc::new(a.intersection(b).cloned().collect())))
        }
        "difference" => {
            if args.len() != 2 {
                return Err(VmError::new("set.difference takes 2 arguments".into()));
            }
            let (Value::Set(a), Value::Set(b)) = (&args[0], &args[1]) else {
                return Err(VmError::new("set.difference requires sets".into()));
            };
            ensure_no_fn("set.difference", "Compare", &[&args[0], &args[1]])?;
            Ok(Value::Set(Arc::new(a.difference(b).cloned().collect())))
        }
        "is_subset" => {
            if args.len() != 2 {
                return Err(VmError::new("set.is_subset takes 2 arguments".into()));
            }
            let (Value::Set(a), Value::Set(b)) = (&args[0], &args[1]) else {
                return Err(VmError::new("set.is_subset requires sets".into()));
            };
            ensure_no_fn("set.is_subset", "Compare", &[&args[0], &args[1]])?;
            Ok(Value::Bool(a.is_subset(b)))
        }
        "symmetric_difference" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "set.symmetric_difference takes 2 arguments".into(),
                ));
            }
            let (Value::Set(a), Value::Set(b)) = (&args[0], &args[1]) else {
                return Err(VmError::new(
                    "set.symmetric_difference requires sets".into(),
                ));
            };
            ensure_no_fn("set.symmetric_difference", "Compare", &[&args[0], &args[1]])?;
            Ok(Value::Set(Arc::new(
                a.symmetric_difference(b).cloned().collect(),
            )))
        }
        _ => Err(VmError::new(format!("unknown set function: {name}"))),
    }
}
