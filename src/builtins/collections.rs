//! Collection builtin functions (`list.*`, `map.*`, `set.*`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::value::{MAX_RANGE_MATERIALIZE, Value, checked_range_len};
use crate::vm::{BuiltinAcc, BuiltinIterKind, Vm, VmError};

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
/// Used by higher-order list/set/map builtins to feed `iterate_builtin`.
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

/// Dispatch `list.<name>(args)`.
pub fn call_list(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "map" => {
            if args.len() != 2 {
                return Err(VmError::new("list.map takes 2 arguments (list, fn)".into()));
            }
            // On a fresh call, materialize items.  On resume, the helper
            // discards this and uses the saved items from `suspended_builtin`.
            let items = materialize_iter(&args[0], "list.map")?;
            vm.iterate_builtin(BuiltinIterKind::ListMap, items, args[1].clone(), args)
        }
        "filter" => {
            if args.len() != 2 {
                return Err(VmError::new("list.filter takes 2 arguments".into()));
            }
            let items = materialize_iter(&args[0], "list.filter")?;
            vm.iterate_builtin(BuiltinIterKind::ListFilter, items, args[1].clone(), args)
        }
        "each" => {
            if args.len() != 2 {
                return Err(VmError::new("list.each takes 2 arguments".into()));
            }
            let items = materialize_iter(&args[0], "list.each")?;
            vm.iterate_builtin(BuiltinIterKind::ListEach, items, args[1].clone(), args)
        }
        "fold" => {
            if args.len() != 3 {
                return Err(VmError::new("list.fold takes 3 arguments".into()));
            }
            let items = materialize_iter(&args[0], "list.fold")?;
            vm.iterate_builtin_with_acc(
                BuiltinIterKind::ListFold,
                items,
                args[2].clone(),
                BuiltinAcc::Fold(args[1].clone()),
                args,
            )
        }
        "find" => {
            if args.len() != 2 {
                return Err(VmError::new("list.find takes 2 arguments".into()));
            }
            let items = materialize_iter(&args[0], "list.find")?;
            vm.iterate_builtin(BuiltinIterKind::ListFind, items, args[1].clone(), args)
        }
        "any" => {
            if args.len() != 2 {
                return Err(VmError::new("list.any takes 2 arguments".into()));
            }
            let items = materialize_iter(&args[0], "list.any")?;
            vm.iterate_builtin(BuiltinIterKind::ListAny, items, args[1].clone(), args)
        }
        "all" => {
            if args.len() != 2 {
                return Err(VmError::new("list.all takes 2 arguments".into()));
            }
            let items = materialize_iter(&args[0], "list.all")?;
            vm.iterate_builtin(BuiltinIterKind::ListAll, items, args[1].clone(), args)
        }
        "flat_map" => {
            if args.len() != 2 {
                return Err(VmError::new("list.flat_map takes 2 arguments".into()));
            }
            let items = materialize_iter(&args[0], "list.flat_map")?;
            vm.iterate_builtin(BuiltinIterKind::ListFlatMap, items, args[1].clone(), args)
        }
        "filter_map" => {
            if args.len() != 2 {
                return Err(VmError::new("list.filter_map takes 2 arguments".into()));
            }
            let items = materialize_iter(&args[0], "list.filter_map")?;
            vm.iterate_builtin(BuiltinIterKind::ListFilterMap, items, args[1].clone(), args)
        }
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
                    Some(val) => Ok(Value::Variant("Some".into(), vec![val.clone()])),
                    None => Ok(Value::Variant("None".into(), Vec::new())),
                },
                Value::Range(lo, hi) => {
                    if lo <= hi {
                        Ok(Value::Variant("Some".into(), vec![Value::Int(*lo)]))
                    } else {
                        Ok(Value::Variant("None".into(), Vec::new()))
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
                    Some(val) => Ok(Value::Variant("Some".into(), vec![val.clone()])),
                    None => Ok(Value::Variant("None".into(), Vec::new())),
                },
                Value::Range(lo, hi) => {
                    if lo <= hi {
                        Ok(Value::Variant("Some".into(), vec![Value::Int(*hi)]))
                    } else {
                        Ok(Value::Variant("None".into(), Vec::new()))
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
                return Err(VmError::new("list.get index must be int".into()));
            };
            let n_val = *n;
            if n_val < 0 {
                return Err(VmError::new(format!("list.get: negative index {n_val}")));
            }
            let idx = n_val as usize;
            match &args[0] {
                Value::List(xs) => match xs.get(idx) {
                    Some(val) => Ok(Value::Variant("Some".into(), vec![val.clone()])),
                    None => Ok(Value::Variant("None".into(), Vec::new())),
                },
                Value::Range(lo, hi) => {
                    let i = match lo.checked_add(idx as i64) {
                        Some(i) => i,
                        None => return Ok(Value::Variant("None".into(), Vec::new())),
                    };
                    if i <= *hi {
                        Ok(Value::Variant("Some".into(), vec![Value::Int(i)]))
                    } else {
                        Ok(Value::Variant("None".into(), Vec::new()))
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
                return Err(VmError::new("list.set index must be int".into()));
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
                return Err(VmError::new("list.take requires int".into()));
            };
            let n_val = *n;
            if n_val < 0 {
                return Err(VmError::new(format!("list.take: negative index {n_val}")));
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
                return Err(VmError::new("list.drop requires int".into()));
            };
            let n_val = *n;
            if n_val < 0 {
                return Err(VmError::new(format!("list.drop: negative index {n_val}")));
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
        "sort_by" => {
            if args.len() != 2 {
                return Err(VmError::new("list.sort_by takes 2 arguments".into()));
            }
            let items = materialize_iter(&args[0], "list.sort_by")?;
            vm.iterate_builtin(BuiltinIterKind::ListSortBy, items, args[1].clone(), args)
        }
        "fold_until" => {
            if args.len() != 3 {
                return Err(VmError::new("list.fold_until takes 3 arguments".into()));
            }
            let items = materialize_iter(&args[0], "list.fold_until")?;
            vm.iterate_builtin_with_acc(
                BuiltinIterKind::ListFoldUntil,
                items,
                args[2].clone(),
                BuiltinAcc::Fold(args[1].clone()),
                args,
            )
        }
        "unfold" => {
            if args.len() != 2 {
                return Err(VmError::new("list.unfold takes 2 arguments".into()));
            }
            let func = &args[1];
            let mut state = args[0].clone();
            let mut result = Vec::new();
            loop {
                let val = vm.invoke_callable(func, &[state.clone()])?;
                match val {
                    Value::Variant(ref tag, ref fields) if tag == "Some" && fields.len() == 1 => {
                        if let Value::Tuple(pair) = &fields[0]
                            && pair.len() == 2
                        {
                            result.push(pair[0].clone());
                            if result.len() > MAX_RANGE_MATERIALIZE {
                                return Err(VmError::new(format!(
                                    "list.unfold: accumulated result exceeds maximum list length of {} elements",
                                    MAX_RANGE_MATERIALIZE
                                )));
                            }
                            state = pair[1].clone();
                            continue;
                        }
                        result.push(fields[0].clone());
                        break;
                    }
                    Value::Variant(ref tag, _) if tag == "None" => {
                        break;
                    }
                    _ => {
                        result.push(val);
                        break;
                    }
                }
            }
            Ok(Value::List(Arc::new(result)))
        }
        "group_by" => {
            if args.len() != 2 {
                return Err(VmError::new("list.group_by takes 2 arguments".into()));
            }
            let items = materialize_iter(&args[0], "list.group_by")?;
            vm.iterate_builtin(BuiltinIterKind::ListGroupBy, items, args[1].clone(), args)
        }
        "index_of" => {
            if args.len() != 2 {
                return Err(VmError::new("list.index_of takes 2 arguments".into()));
            }
            let iter = ValueIter::try_from(&args[0], "list.index_of")?;
            let target = &args[1];
            for (i, v) in iter.enumerate() {
                if &v == target {
                    let idx = i64::try_from(i).map_err(|_| {
                        VmError::new(
                            "list.index_of overflow: index too large to represent as Int".into(),
                        )
                    })?;
                    return Ok(Value::Variant("Some".into(), vec![Value::Int(idx)]));
                }
            }
            Ok(Value::Variant("None".into(), Vec::new()))
        }
        "remove_at" => {
            if args.len() != 2 {
                return Err(VmError::new("list.remove_at takes 2 arguments".into()));
            }
            let Value::Int(n) = &args[1] else {
                return Err(VmError::new("list.remove_at index must be int".into()));
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
        "min_by" => {
            if args.len() != 2 {
                return Err(VmError::new("list.min_by takes 2 arguments".into()));
            }
            let items = materialize_iter(&args[0], "list.min_by")?;
            let func = &args[1];
            let mut best: Option<(Value, Value)> = None;
            for item in items {
                let key = vm.invoke_callable(func, &[item.clone()])?;
                best = Some(match best {
                    None => (key, item),
                    Some((bk, bv)) => {
                        if key.partial_cmp(&bk).unwrap_or(std::cmp::Ordering::Equal)
                            == std::cmp::Ordering::Less
                        {
                            (key, item)
                        } else {
                            (bk, bv)
                        }
                    }
                });
            }
            match best {
                Some((_, v)) => Ok(Value::Variant("Some".into(), vec![v])),
                None => Ok(Value::Variant("None".into(), Vec::new())),
            }
        }
        "max_by" => {
            if args.len() != 2 {
                return Err(VmError::new("list.max_by takes 2 arguments".into()));
            }
            let items = materialize_iter(&args[0], "list.max_by")?;
            let func = &args[1];
            let mut best: Option<(Value, Value)> = None;
            for item in items {
                let key = vm.invoke_callable(func, &[item.clone()])?;
                best = Some(match best {
                    None => (key, item),
                    Some((bk, bv)) => {
                        if key.partial_cmp(&bk).unwrap_or(std::cmp::Ordering::Equal)
                            == std::cmp::Ordering::Greater
                        {
                            (key, item)
                        } else {
                            (bk, bv)
                        }
                    }
                });
            }
            match best {
                Some((_, v)) => Ok(Value::Variant("Some".into(), vec![v])),
                None => Ok(Value::Variant("None".into(), Vec::new())),
            }
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
            Ok(Value::Float(total))
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
            Ok(Value::Float(total))
        }
        "scan" => {
            if args.len() != 3 {
                return Err(VmError::new("list.scan takes 3 arguments".into()));
            }
            let items = materialize_iter(&args[0], "list.scan")?;
            let init = args[1].clone();
            let func = &args[2];
            let mut acc = init.clone();
            let mut result = Vec::with_capacity(items.len() + 1);
            result.push(acc.clone());
            for item in items {
                acc = vm.invoke_callable(func, &[acc.clone(), item])?;
                result.push(acc.clone());
                if result.len() > MAX_RANGE_MATERIALIZE {
                    return Err(VmError::new(format!(
                        "list.scan: accumulated result exceeds maximum list length of {} elements",
                        MAX_RANGE_MATERIALIZE
                    )));
                }
            }
            Ok(Value::List(Arc::new(result)))
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

/// Dispatch `map.<name>(args)`.
pub fn call_map(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "get" => {
            if args.len() != 2 {
                return Err(VmError::new("map.get takes 2 arguments".into()));
            }
            let Value::Map(m) = &args[0] else {
                return Err(VmError::new("map.get requires a map".into()));
            };
            match m.get(&args[1]) {
                Some(val) => Ok(Value::Variant("Some".into(), vec![val.clone()])),
                None => Ok(Value::Variant("None".into(), Vec::new())),
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
                    result.insert(pair[0].clone(), pair[1].clone());
                    continue;
                }
                return Err(VmError::new(
                    "map.from_entries requires (key, value) tuples".into(),
                ));
            }
            Ok(Value::Map(Arc::new(result)))
        }
        "filter" => {
            if args.len() != 2 {
                return Err(VmError::new("map.filter takes 2 arguments".into()));
            }
            let Value::Map(m) = &args[0] else {
                return Err(VmError::new("map.filter requires a map".into()));
            };
            // Materialize entries as Tuple(k, v) for iterate_builtin.
            let items: Vec<Value> = m
                .iter()
                .map(|(k, v)| Value::Tuple(vec![k.clone(), v.clone()]))
                .collect();
            vm.iterate_builtin(BuiltinIterKind::MapFilter, items, args[1].clone(), args)
        }
        "map" => {
            if args.len() != 2 {
                return Err(VmError::new("map.map takes 2 arguments".into()));
            }
            let Value::Map(m) = &args[0] else {
                return Err(VmError::new("map.map requires a map".into()));
            };
            let items: Vec<Value> = m
                .iter()
                .map(|(k, v)| Value::Tuple(vec![k.clone(), v.clone()]))
                .collect();
            // iterate_builtin will short-circuit with a marker Variant on a
            // type error; translate that to a proper VmError here.
            let result =
                vm.iterate_builtin(BuiltinIterKind::MapMap, items, args[1].clone(), args)?;
            if let Value::Variant(ref tag, _) = result
                && tag == "__MapMapTypeError__"
            {
                return Err(VmError::new(
                    "map.map callback must return a (key, value) tuple".into(),
                ));
            }
            Ok(result)
        }
        "each" => {
            if args.len() != 2 {
                return Err(VmError::new("map.each takes 2 arguments".into()));
            }
            let Value::Map(m) = &args[0] else {
                return Err(VmError::new("map.each requires a map".into()));
            };
            let items: Vec<Value> = m
                .iter()
                .map(|(k, v)| Value::Tuple(vec![k.clone(), v.clone()]))
                .collect();
            vm.iterate_builtin(BuiltinIterKind::MapEach, items, args[1].clone(), args)
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
            let key = &args[1];
            let default = &args[2];
            let func = &args[3];
            let current = m.get(key).unwrap_or(default).clone();
            // map.update is a single-callback builtin.  Use the resumable
            // helper so yields inside `func` are handled correctly.
            let new_val = vm.invoke_callable_resumable(func, &[current], args)?;
            let mut new_map = (**m).clone();
            new_map.insert(key.clone(), new_val);
            Ok(Value::Map(Arc::new(new_map)))
        }
        _ => Err(VmError::new(format!("unknown map function: {name}"))),
    }
}

/// Dispatch `set.<name>(args)`.
pub fn call_set(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "new" => Ok(Value::Set(Arc::new(BTreeSet::new()))),
        "from_list" => {
            if args.len() != 1 {
                return Err(VmError::new("set.from_list takes 1 argument".into()));
            }
            let Value::List(xs) = &args[0] else {
                return Err(VmError::new("set.from_list requires a list".into()));
            };
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
            Ok(Value::Bool(s.contains(&args[1])))
        }
        "insert" => {
            if args.len() != 2 {
                return Err(VmError::new("set.insert takes 2 arguments".into()));
            }
            let Value::Set(s) = &args[0] else {
                return Err(VmError::new("set.insert requires a set".into()));
            };
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
            Ok(Value::Set(Arc::new(a.union(b).cloned().collect())))
        }
        "intersection" => {
            if args.len() != 2 {
                return Err(VmError::new("set.intersection takes 2 arguments".into()));
            }
            let (Value::Set(a), Value::Set(b)) = (&args[0], &args[1]) else {
                return Err(VmError::new("set.intersection requires sets".into()));
            };
            Ok(Value::Set(Arc::new(a.intersection(b).cloned().collect())))
        }
        "difference" => {
            if args.len() != 2 {
                return Err(VmError::new("set.difference takes 2 arguments".into()));
            }
            let (Value::Set(a), Value::Set(b)) = (&args[0], &args[1]) else {
                return Err(VmError::new("set.difference requires sets".into()));
            };
            Ok(Value::Set(Arc::new(a.difference(b).cloned().collect())))
        }
        "is_subset" => {
            if args.len() != 2 {
                return Err(VmError::new("set.is_subset takes 2 arguments".into()));
            }
            let (Value::Set(a), Value::Set(b)) = (&args[0], &args[1]) else {
                return Err(VmError::new("set.is_subset requires sets".into()));
            };
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
            Ok(Value::Set(Arc::new(
                a.symmetric_difference(b).cloned().collect(),
            )))
        }
        "map" => {
            if args.len() != 2 {
                return Err(VmError::new("set.map takes 2 arguments".into()));
            }
            let Value::Set(s) = &args[0] else {
                return Err(VmError::new("set.map requires a set".into()));
            };
            let items: Vec<Value> = s.iter().cloned().collect();
            vm.iterate_builtin(BuiltinIterKind::SetMap, items, args[1].clone(), args)
        }
        "filter" => {
            if args.len() != 2 {
                return Err(VmError::new("set.filter takes 2 arguments".into()));
            }
            let Value::Set(s) = &args[0] else {
                return Err(VmError::new("set.filter requires a set".into()));
            };
            let items: Vec<Value> = s.iter().cloned().collect();
            vm.iterate_builtin(BuiltinIterKind::SetFilter, items, args[1].clone(), args)
        }
        "each" => {
            if args.len() != 2 {
                return Err(VmError::new("set.each takes 2 arguments".into()));
            }
            let Value::Set(s) = &args[0] else {
                return Err(VmError::new("set.each requires a set".into()));
            };
            let items: Vec<Value> = s.iter().cloned().collect();
            vm.iterate_builtin(BuiltinIterKind::SetEach, items, args[1].clone(), args)
        }
        "fold" => {
            if args.len() != 3 {
                return Err(VmError::new("set.fold takes 3 arguments".into()));
            }
            let Value::Set(s) = &args[0] else {
                return Err(VmError::new("set.fold requires a set".into()));
            };
            let items: Vec<Value> = s.iter().cloned().collect();
            vm.iterate_builtin_with_acc(
                BuiltinIterKind::SetFold,
                items,
                args[2].clone(),
                BuiltinAcc::Fold(args[1].clone()),
                args,
            )
        }
        _ => Err(VmError::new(format!("unknown set function: {name}"))),
    }
}
