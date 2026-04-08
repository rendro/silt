//! Collection builtin functions (`list.*`, `map.*`, `set.*`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::value::{Value, checked_range_len};
use crate::vm::{Vm, VmError};

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

/// Dispatch `list.<name>(args)`.
pub fn call_list(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "map" => {
            if args.len() != 2 {
                return Err(VmError::new("list.map takes 2 arguments (list, fn)".into()));
            }
            let mut iter = ValueIter::try_from(&args[0], "list.map")?;
            let func = &args[1];
            let mut result = Vec::with_capacity(iter.len());
            for item in &mut iter {
                result.push(vm.invoke_callable(func, std::slice::from_ref(&item))?);
            }
            Ok(Value::List(Arc::new(result)))
        }
        "filter" => {
            if args.len() != 2 {
                return Err(VmError::new("list.filter takes 2 arguments".into()));
            }
            let iter = ValueIter::try_from(&args[0], "list.filter")?;
            let func = &args[1];
            let mut result = Vec::new();
            for item in iter {
                let keep = vm.invoke_callable(func, std::slice::from_ref(&item))?;
                if vm.is_truthy(&keep) {
                    result.push(item);
                }
            }
            Ok(Value::List(Arc::new(result)))
        }
        "each" => {
            if args.len() != 2 {
                return Err(VmError::new("list.each takes 2 arguments".into()));
            }
            let iter = ValueIter::try_from(&args[0], "list.each")?;
            let func = &args[1];
            for item in iter {
                vm.invoke_callable(func, std::slice::from_ref(&item))?;
            }
            Ok(Value::Unit)
        }
        "fold" => {
            if args.len() != 3 {
                return Err(VmError::new("list.fold takes 3 arguments".into()));
            }
            let iter = ValueIter::try_from(&args[0], "list.fold")?;
            let func = &args[2];
            let mut acc = args[1].clone();
            for item in iter {
                acc = vm.invoke_callable(func, &[acc, item])?;
            }
            Ok(acc)
        }
        "find" => {
            if args.len() != 2 {
                return Err(VmError::new("list.find takes 2 arguments".into()));
            }
            let iter = ValueIter::try_from(&args[0], "list.find")?;
            let func = &args[1];
            for item in iter {
                let result = vm.invoke_callable(func, std::slice::from_ref(&item))?;
                if vm.is_truthy(&result) {
                    return Ok(Value::Variant("Some".into(), vec![item]));
                }
            }
            Ok(Value::Variant("None".into(), Vec::new()))
        }
        "any" => {
            if args.len() != 2 {
                return Err(VmError::new("list.any takes 2 arguments".into()));
            }
            let iter = ValueIter::try_from(&args[0], "list.any")?;
            let func = &args[1];
            for item in iter {
                let result = vm.invoke_callable(func, std::slice::from_ref(&item))?;
                if vm.is_truthy(&result) {
                    return Ok(Value::Bool(true));
                }
            }
            Ok(Value::Bool(false))
        }
        "all" => {
            if args.len() != 2 {
                return Err(VmError::new("list.all takes 2 arguments".into()));
            }
            let iter = ValueIter::try_from(&args[0], "list.all")?;
            let func = &args[1];
            for item in iter {
                let result = vm.invoke_callable(func, std::slice::from_ref(&item))?;
                if !vm.is_truthy(&result) {
                    return Ok(Value::Bool(false));
                }
            }
            Ok(Value::Bool(true))
        }
        "flat_map" => {
            if args.len() != 2 {
                return Err(VmError::new("list.flat_map takes 2 arguments".into()));
            }
            let iter = ValueIter::try_from(&args[0], "list.flat_map")?;
            let func = &args[1];
            let mut result = Vec::new();
            for item in iter {
                let val = vm.invoke_callable(func, std::slice::from_ref(&item))?;
                match val {
                    Value::List(inner) => result.extend(inner.iter().cloned()),
                    Value::Range(lo, hi) => {
                        checked_range_len(lo, hi).map_err(VmError::new)?;
                        for i in lo..=hi {
                            result.push(Value::Int(i));
                        }
                    }
                    other => result.push(other),
                }
            }
            Ok(Value::List(Arc::new(result)))
        }
        "filter_map" => {
            if args.len() != 2 {
                return Err(VmError::new("list.filter_map takes 2 arguments".into()));
            }
            let iter = ValueIter::try_from(&args[0], "list.filter_map")?;
            let func = &args[1];
            let mut result = Vec::new();
            for item in iter {
                let val = vm.invoke_callable(func, std::slice::from_ref(&item))?;
                match val {
                    Value::Variant(ref tag, ref fields) if tag == "Some" && fields.len() == 1 => {
                        result.push(fields[0].clone());
                    }
                    Value::Variant(ref tag, _) if tag == "None" => {}
                    _ => result.push(val),
                }
            }
            Ok(Value::List(Arc::new(result)))
        }
        // Non-closure list builtins
        "zip" => {
            if args.len() != 2 {
                return Err(VmError::new("list.zip takes 2 arguments".into()));
            }
            let mut a = ValueIter::try_from(&args[0], "list.zip")?;
            let mut b = ValueIter::try_from(&args[1], "list.zip")?;
            let cap = a.len().min(b.len());
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
            match args[0].collection_len() {
                Some(len) => Ok(Value::Int(len as i64)),
                None => Err(VmError::new("list.length requires a list or range".into())),
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
                    let i = lo + idx as i64;
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
                    let count = n_val;
                    let new_hi = (*lo + count - 1).min(*hi);
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
                    let new_lo = lo + n_val;
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
            if let Value::Range(lo, hi) = &args[0] {
                checked_range_len(*lo, *hi).map_err(VmError::new)?;
            }
            let iter = ValueIter::try_from(&args[0], "list.sort_by")?;
            let func = &args[1];
            let mut pairs: Vec<(Value, Value)> = Vec::with_capacity(iter.len());
            for item in iter {
                let key = vm.invoke_callable(func, std::slice::from_ref(&item))?;
                pairs.push((key, item));
            }
            pairs.sort_by(|(a, _), (b, _)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let sorted: Vec<Value> = pairs.into_iter().map(|(_, v)| v).collect();
            Ok(Value::List(Arc::new(sorted)))
        }
        "fold_until" => {
            if args.len() != 3 {
                return Err(VmError::new("list.fold_until takes 3 arguments".into()));
            }
            let iter = ValueIter::try_from(&args[0], "list.fold_until")?;
            let func = &args[2];
            let mut acc = args[1].clone();
            for item in iter {
                let result = vm.invoke_callable(func, &[acc.clone(), item])?;
                match result {
                    Value::Variant(ref tag, ref fields)
                        if tag == "Continue" && fields.len() == 1 =>
                    {
                        acc = fields[0].clone();
                    }
                    Value::Variant(ref tag, ref fields) if tag == "Stop" && fields.len() == 1 => {
                        return Ok(fields[0].clone());
                    }
                    _ => acc = result,
                }
            }
            Ok(acc)
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
            let iter = ValueIter::try_from(&args[0], "list.group_by")?;
            let func = &args[1];
            let mut groups: BTreeMap<Value, Vec<Value>> = BTreeMap::new();
            for item in iter {
                let key = vm.invoke_callable(func, std::slice::from_ref(&item))?;
                groups.entry(key).or_default().push(item);
            }
            let result: BTreeMap<Value, Value> = groups
                .into_iter()
                .map(|(k, v)| (k, Value::List(Arc::new(v))))
                .collect();
            Ok(Value::Map(Arc::new(result)))
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
            let func = &args[1];
            let mut result = BTreeMap::new();
            for (k, v) in m.iter() {
                let keep = vm.invoke_callable(func, &[k.clone(), v.clone()])?;
                if vm.is_truthy(&keep) {
                    result.insert(k.clone(), v.clone());
                }
            }
            Ok(Value::Map(Arc::new(result)))
        }
        "map" => {
            if args.len() != 2 {
                return Err(VmError::new("map.map takes 2 arguments".into()));
            }
            let Value::Map(m) = &args[0] else {
                return Err(VmError::new("map.map requires a map".into()));
            };
            let func = &args[1];
            let mut result = BTreeMap::new();
            for (k, v) in m.iter() {
                let mapped = vm.invoke_callable(func, &[k.clone(), v.clone()])?;
                match mapped {
                    Value::Tuple(pair) if pair.len() == 2 => {
                        result.insert(pair[0].clone(), pair[1].clone());
                    }
                    _ => {
                        return Err(VmError::new(
                            "map.map callback must return a (key, value) tuple".into(),
                        ));
                    }
                }
            }
            Ok(Value::Map(Arc::new(result)))
        }
        "each" => {
            if args.len() != 2 {
                return Err(VmError::new("map.each takes 2 arguments".into()));
            }
            let Value::Map(m) = &args[0] else {
                return Err(VmError::new("map.each requires a map".into()));
            };
            let func = &args[1];
            for (k, v) in m.iter() {
                vm.invoke_callable(func, &[k.clone(), v.clone()])?;
            }
            Ok(Value::Unit)
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
            let new_val = vm.invoke_callable(func, &[current])?;
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
        "map" => {
            if args.len() != 2 {
                return Err(VmError::new("set.map takes 2 arguments".into()));
            }
            let Value::Set(s) = &args[0] else {
                return Err(VmError::new("set.map requires a set".into()));
            };
            let func = &args[1];
            let mut result = BTreeSet::new();
            for item in s.iter() {
                let val = vm.invoke_callable(func, std::slice::from_ref(item))?;
                result.insert(val);
            }
            Ok(Value::Set(Arc::new(result)))
        }
        "filter" => {
            if args.len() != 2 {
                return Err(VmError::new("set.filter takes 2 arguments".into()));
            }
            let Value::Set(s) = &args[0] else {
                return Err(VmError::new("set.filter requires a set".into()));
            };
            let func = &args[1];
            let mut result = BTreeSet::new();
            for item in s.iter() {
                let keep = vm.invoke_callable(func, std::slice::from_ref(item))?;
                if vm.is_truthy(&keep) {
                    result.insert(item.clone());
                }
            }
            Ok(Value::Set(Arc::new(result)))
        }
        "each" => {
            if args.len() != 2 {
                return Err(VmError::new("set.each takes 2 arguments".into()));
            }
            let Value::Set(s) = &args[0] else {
                return Err(VmError::new("set.each requires a set".into()));
            };
            let func = &args[1];
            for item in s.iter() {
                vm.invoke_callable(func, std::slice::from_ref(item))?;
            }
            Ok(Value::Unit)
        }
        "fold" => {
            if args.len() != 3 {
                return Err(VmError::new("set.fold takes 3 arguments".into()));
            }
            let Value::Set(s) = &args[0] else {
                return Err(VmError::new("set.fold requires a set".into()));
            };
            let func = &args[2];
            let mut acc = args[1].clone();
            for item in s.iter() {
                acc = vm.invoke_callable(func, &[acc, item.clone()])?;
            }
            Ok(acc)
        }
        _ => Err(VmError::new(format!("unknown set function: {name}"))),
    }
}
