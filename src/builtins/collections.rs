//! Collection builtin functions (`list.*`, `map.*`, `set.*`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::typed::{List, Map, Set, builtins, unsound};
use crate::typeinfo::bv;
use crate::value::{MAX_RANGE_MATERIALIZE, Value, checked_range_len};
use crate::vm::{Flow, Native, Step, Vm, VmError, call_then, item_arg, iterate, next, stop};

impl List<'_> {
    /// How many elements it has: a range can have more than an `Int`
    /// counts.
    fn len(self) -> u128 {
        match self {
            List::Items(items) => items.len() as u128,
            List::Range(lo, hi) if hi < lo => 0,
            List::Range(lo, hi) => (hi as i128 - lo as i128 + 1) as u128,
        }
    }

    /// The list as the value it was.
    fn value(self) -> Value {
        match self {
            List::Items(items) => Value::List(items.clone()),
            List::Range(lo, hi) => Value::Range(lo, hi),
        }
    }
}

fn some(value: Value) -> Value {
    Value::variant(bv::SOME, vec![value])
}

fn none() -> Value {
    Value::variant(bv::NONE, Vec::new())
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
fn ensure_no_fn<'a>(
    fn_name: &str,
    trait_name: &str,
    vals: impl IntoIterator<Item = &'a Value>,
) -> Result<(), VmError> {
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
    ensure_no_fn(name, trait_name, [key])
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
        Some((_, item)) => some(item),
        None => none(),
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

/// A non-negative `Int` argument as a position or a count, for
/// `name`'s `what` ("index", "count").
fn natural(name: &str, what: &str, n: i64) -> Result<usize, VmError> {
    usize::try_from(n).map_err(|_| VmError::new(format!("{name}: negative {what} {n}")))
}

/// `list.*`
pub(crate) mod list {
    use super::*;

    builtins! {
        fn map(xs: List, f: &Value) -> Result<Step, VmError> {
            Ok(iterate(
                "list.map",
                xs.to_vec()?,
                f.clone(),
                Vec::new(),
                item_arg,
                keep_result,
                as_list,
            ))
        }

        fn filter(xs: List, f: &Value) -> Result<Step, VmError> {
            Ok(iterate(
                "list.filter",
                xs.to_vec()?,
                f.clone(),
                Vec::new(),
                item_arg,
                keep_item_if,
                as_list,
            ))
        }

        fn each(xs: List, f: &Value) -> Result<Step, VmError> {
            Ok(iterate("list.each", xs.to_vec()?, f.clone(), (), item_arg, ignore, unit))
        }

        fn fold(xs: List, init: &Value, f: &Value) -> Result<Step, VmError> {
            Ok(iterate(
                "list.fold",
                xs.to_vec()?,
                f.clone(),
                init.clone(),
                acc_and_item,
                set_acc,
                take_acc,
            ))
        }

        fn find(xs: List, f: &Value) -> Result<Step, VmError> {
            Ok(iterate(
                "list.find",
                xs.to_vec()?,
                f.clone(),
                (),
                item_arg,
                |_, item, found| match truthy(&found) {
                    true => stop(some(item)),
                    false => next(),
                },
                |_| Ok(none()),
            ))
        }

        fn any(xs: List, f: &Value) -> Result<Step, VmError> {
            Ok(iterate(
                "list.any",
                xs.to_vec()?,
                f.clone(),
                (),
                item_arg,
                |_, _, holds| match truthy(&holds) {
                    true => stop(Value::Bool(true)),
                    false => next(),
                },
                |_| Ok(Value::Bool(false)),
            ))
        }

        fn all(xs: List, f: &Value) -> Result<Step, VmError> {
            Ok(iterate(
                "list.all",
                xs.to_vec()?,
                f.clone(),
                (),
                item_arg,
                |_, _, holds| match truthy(&holds) {
                    true => next(),
                    false => stop(Value::Bool(false)),
                },
                |_| Ok(Value::Bool(true)),
            ))
        }

        fn flat_map(xs: List, f: &Value) -> Result<Step, VmError> {
            Ok(iterate(
                "list.flat_map",
                xs.to_vec()?,
                f.clone(),
                Vec::new(),
                item_arg,
                flat_map_step,
                as_list,
            ))
        }

        fn filter_map(xs: List, f: &Value) -> Result<Step, VmError> {
            Ok(iterate(
                "list.filter_map",
                xs.to_vec()?,
                f.clone(),
                Vec::new(),
                item_arg,
                filter_map_step,
                as_list,
            ))
        }

        fn sort_by(xs: List, key: &Value) -> Result<Step, VmError> {
            Ok(iterate(
                "list.sort_by",
                xs.to_vec()?,
                key.clone(),
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
            ))
        }

        fn fold_until(xs: List, init: &Value, f: &Value) -> Result<Step, VmError> {
            Ok(iterate(
                "list.fold_until",
                xs.to_vec()?,
                f.clone(),
                init.clone(),
                acc_and_item,
                fold_until_step,
                take_acc,
            ))
        }

        fn unfold(seed: &Value, f: &Value) -> Step {
            Step::Run(Box::new(Unfold {
                state: seed.clone(),
                out: Vec::new(),
                callback: f.clone(),
                called: false,
            }))
        }

        fn group_by(xs: List, f: &Value) -> Result<Step, VmError> {
            Ok(iterate(
                "list.group_by",
                xs.to_vec()?,
                f.clone(),
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
            ))
        }

        fn min_by(xs: List, key: &Value) -> Result<Step, VmError> {
            Ok(iterate(
                "list.min_by",
                xs.to_vec()?,
                key.clone(),
                None,
                item_arg,
                |best, item, key| {
                    best_step("list.min_by", std::cmp::Ordering::Less, best, item, key)
                },
                best_item,
            ))
        }

        fn max_by(xs: List, key: &Value) -> Result<Step, VmError> {
            Ok(iterate(
                "list.max_by",
                xs.to_vec()?,
                key.clone(),
                None,
                item_arg,
                |best, item, key| {
                    best_step("list.max_by", std::cmp::Ordering::Greater, best, item, key)
                },
                best_item,
            ))
        }

        // The running value, and the list of the values it had, the
        // initial one first.
        fn scan(xs: List, init: &Value, f: &Value) -> Result<Step, VmError> {
            Ok(iterate(
                "list.scan",
                xs.to_vec()?,
                f.clone(),
                (init.clone(), vec![init.clone()]),
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
            ))
        }

        fn zip(xs: List, ys: List) -> Result<Vec<Value>, VmError> {
            // The result is as long as the shorter of the two, which
            // for two ranges can be more than a list may hold: checked
            // before anything is made.
            let expected = xs.len().min(ys.len());
            if expected > MAX_RANGE_MATERIALIZE as u128 {
                return Err(VmError::new(format!(
                    "list.zip: result length {expected} exceeds maximum materialized length {MAX_RANGE_MATERIALIZE}"
                )));
            }
            Ok(xs
                .iter()
                .zip(ys.iter())
                .map(|(x, y)| Value::Tuple(vec![x, y]))
                .collect())
        }

        fn flatten(xs: List) -> Result<Vec<Value>, VmError> {
            let mut result = Vec::new();
            for item in xs.iter() {
                match item {
                    Value::List(inner) => result.extend(inner.iter().cloned()),
                    Value::Range(lo, hi) => {
                        checked_range_len(lo, hi).map_err(VmError::new)?;
                        result.extend((lo..=hi).map(Value::Int));
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
            Ok(result)
        }

        fn head(xs: List) -> Option<Value> {
            xs.iter().next()
        }

        fn tail(xs: List) -> Value {
            match xs {
                List::Items(items) => {
                    Value::List(Arc::new(items.get(1..).unwrap_or_default().to_vec()))
                }
                List::Range(lo, hi) if lo >= hi => Value::List(Arc::new(Vec::new())),
                List::Range(lo, hi) => Value::Range(lo + 1, hi),
            }
        }

        fn last(xs: List) -> Option<Value> {
            match xs {
                List::Items(items) => items.last().cloned(),
                List::Range(lo, hi) => (lo <= hi).then_some(Value::Int(hi)),
            }
        }

        fn reverse(xs: List) -> Result<Vec<Value>, VmError> {
            let mut items = xs.to_vec()?;
            items.reverse();
            Ok(items)
        }

        fn sort(xs: List) -> Result<Value, VmError> {
            // A range is sorted as it is.
            let List::Items(items) = xs else {
                return Ok(xs.value());
            };
            // Fn elements would sort by Arc pointer address (ASLR-
            // nondeterministic) — reject like the operator gates do.
            ensure_no_fn("list.sort", "Compare", items.iter())?;
            let mut sorted = (**items).clone();
            sorted.sort();
            Ok(Value::List(Arc::new(sorted)))
        }

        fn unique(xs: List) -> Result<Value, VmError> {
            // A range has no element twice.
            let List::Items(items) = xs else {
                return Ok(xs.value());
            };
            // Fn elements would dedup by identity (Arc pointer / builtin
            // name) instead of erroring like `f == g` does.
            ensure_no_fn("list.unique", "Equal", items.iter())?;
            let mut seen = BTreeSet::new();
            let unique = items.iter().filter(|x| seen.insert(*x)).cloned().collect();
            Ok(Value::List(Arc::new(unique)))
        }

        fn contains(xs: List, elem: &Value) -> Result<bool, VmError> {
            match xs {
                List::Items(items) => {
                    // Fn membership would silently answer via Arc identity
                    // (`list.contains([f], g)` -> false) instead of erroring.
                    ensure_no_fn("list.contains", "Equal", items.iter().chain([elem]))?;
                    Ok(items.contains(elem))
                }
                List::Range(lo, hi) => {
                    ensure_no_fn("list.contains", "Equal", [elem])?;
                    Ok(matches!(elem, Value::Int(n) if (lo..=hi).contains(n)))
                }
            }
        }

        // A range can have more elements than an `Int` counts
        // (`i64::MIN..i64::MAX`).
        fn length(xs: List) -> Result<i64, VmError> {
            i64::try_from(xs.len()).map_err(|_| {
                VmError::new(format!(
                    "list.length overflow: {} too large to represent as Int",
                    match xs {
                        List::Items(_) => "list",
                        List::Range(..) => "range",
                    }
                ))
            })
        }

        fn append(xs: List, elem: &Value) -> Result<Vec<Value>, VmError> {
            let mut items = xs.to_vec()?;
            items.push(elem.clone());
            Ok(items)
        }

        fn prepend(xs: List, elem: &Value) -> Result<Vec<Value>, VmError> {
            let mut items = xs.to_vec()?;
            items.insert(0, elem.clone());
            Ok(items)
        }

        fn concat(xs: List, ys: List) -> Result<Vec<Value>, VmError> {
            let mut result = xs.to_vec()?;
            result.extend(ys.to_vec()?);
            if result.len() > MAX_RANGE_MATERIALIZE {
                return Err(VmError::new(format!(
                    "concatenated list exceeds maximum size of {} elements",
                    MAX_RANGE_MATERIALIZE
                )));
            }
            Ok(result)
        }

        fn get(xs: List, i: i64) -> Result<Option<Value>, VmError> {
            let index = natural("list.get", "index", i)?;
            Ok(match xs {
                List::Items(items) => items.get(index).cloned(),
                List::Range(lo, hi) => lo
                    .checked_add(i)
                    .filter(|at| *at <= hi)
                    .map(Value::Int),
            })
        }

        fn set(xs: List, index: i64, value: &Value) -> Result<Vec<Value>, VmError> {
            let mut items = xs.to_vec()?;
            let index = natural("list.set", "index", index)?;
            let Some(slot) = items.get_mut(index) else {
                return Err(VmError::new("list.set index out of bounds".into()));
            };
            *slot = value.clone();
            Ok(items)
        }

        fn take(xs: List, n: i64) -> Result<Value, VmError> {
            let count = natural("list.take", "count", n)?;
            Ok(match xs {
                List::Items(items) => {
                    Value::List(Arc::new(items[..count.min(items.len())].to_vec()))
                }
                // (Taking no element is the empty list wherever the
                // range begins: `lo + 0 - 1` is no Int for the least
                // one.)
                List::Range(..) if n == 0 => Value::List(Arc::new(Vec::new())),
                List::Range(lo, hi) => {
                    let new_hi = lo
                        .checked_add(n)
                        .and_then(|end| end.checked_sub(1))
                        .map_or(hi, |end| end.min(hi));
                    match new_hi < lo {
                        true => Value::List(Arc::new(Vec::new())),
                        false => Value::Range(lo, new_hi),
                    }
                }
            })
        }

        fn drop(xs: List, n: i64) -> Result<Value, VmError> {
            let count = natural("list.drop", "count", n)?;
            Ok(match xs {
                List::Items(items) => {
                    Value::List(Arc::new(items[count.min(items.len())..].to_vec()))
                }
                List::Range(lo, hi) => match lo.checked_add(n).filter(|new_lo| *new_lo <= hi) {
                    Some(new_lo) => Value::Range(new_lo, hi),
                    None => Value::List(Arc::new(Vec::new())),
                },
            })
        }

        fn enumerate(xs: List) -> Result<Vec<Value>, VmError> {
            let items = xs.to_vec()?.into_iter().enumerate();
            Ok(items
                .map(|(i, item)| Value::Tuple(vec![Value::Int(i as i64), item]))
                .collect())
        }

        fn index_of(xs: List, target: &Value) -> Result<Option<Value>, VmError> {
            // Fn search would silently answer via Arc identity
            // (`list.index_of([f, g], g)` -> Some(1)) instead of erroring.
            if let List::Items(items) = xs {
                ensure_no_fn("list.index_of", "Equal", items.iter())?;
            }
            ensure_no_fn("list.index_of", "Equal", [target])?;
            let Some(at) = xs.iter().position(|item| item == *target) else {
                return Ok(None);
            };
            let at = i64::try_from(at).map_err(|_| {
                VmError::new("list.index_of overflow: index too large to represent as Int".into())
            })?;
            Ok(Some(Value::Int(at)))
        }

        fn remove_at(xs: List, index: i64) -> Result<Vec<Value>, VmError> {
            let index = natural("list.remove_at", "index", index)?;
            let mut items = xs.to_vec()?;
            if index >= items.len() {
                return Err(VmError::new("list.remove_at index out of bounds".into()));
            }
            items.remove(index);
            Ok(items)
        }

        fn sum(xs: List) -> Result<i64, VmError> {
            let mut total: i64 = 0;
            for item in xs.iter() {
                let Value::Int(n) = item else {
                    return Err(unsound("list.sum", "xs"));
                };
                total = total
                    .checked_add(n)
                    .ok_or_else(|| VmError::new("list.sum overflow".into()))?;
            }
            Ok(total)
        }

        fn sum_float(xs: List) -> Result<Value, VmError> {
            let mut total: f64 = 0.0;
            for item in xs.iter() {
                let Value::Float(n) = item else {
                    return Err(unsound("list.sum_float", "xs"));
                };
                total += n;
            }
            // A finite sum only ever overflows to ±inf, and the sum
            // cannot come back from there, so one check at the end sees it.
            crate::builtins::numeric::checked_float(total, || "list.sum_float overflow".into())
        }

        fn product(xs: List) -> Result<i64, VmError> {
            let mut total: i64 = 1;
            for item in xs.iter() {
                let Value::Int(n) = item else {
                    return Err(unsound("list.product", "xs"));
                };
                total = total
                    .checked_mul(n)
                    .ok_or_else(|| VmError::new("list.product overflow".into()))?;
            }
            Ok(total)
        }

        fn product_float(xs: List) -> Result<Value, VmError> {
            let mut total: f64 = 1.0;
            for item in xs.iter() {
                let Value::Float(n) = item else {
                    return Err(unsound("list.product_float", "xs"));
                };
                total *= n;
            }
            // As in `sum_float`: once the product leaves the finite range
            // it stays out, so one check at the end sees it.
            crate::builtins::numeric::checked_float(total, || "list.product_float overflow".into())
        }

        fn intersperse(xs: List, sep: &Value) -> Result<Vec<Value>, VmError> {
            let items = xs.to_vec()?;
            if items.len() <= 1 {
                return Ok(items);
            }
            // Result length = 2*N - 1
            let out_len = items.len() * 2 - 1;
            if out_len > MAX_RANGE_MATERIALIZE {
                return Err(VmError::new(format!(
                    "list.intersperse: result length {out_len} exceeds maximum materialized length {MAX_RANGE_MATERIALIZE}"
                )));
            }
            let mut result = Vec::with_capacity(out_len);
            for (at, item) in items.into_iter().enumerate() {
                if at > 0 {
                    result.push(sep.clone());
                }
                result.push(item);
            }
            Ok(result)
        }
    }
}

/// `map.*`
pub(crate) mod map {
    use super::*;

    builtins! {
        fn filter(m: Map, f: &Value) -> Step {
            iterate(
                "map.filter",
                super::entries(m),
                f.clone(),
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

        fn map(m: Map, f: &Value) -> Step {
            iterate(
                "map.map",
                super::entries(m),
                f.clone(),
                BTreeMap::new(),
                key_and_value,
                |out, _, result| {
                    let pair = match result {
                        Value::Tuple(pair) => <[Value; 2]>::try_from(pair).ok(),
                        _ => None,
                    };
                    let [k, v] = pair.ok_or_else(|| unsound("map.map", "f"))?;
                    out.insert(k, v);
                    next()
                },
                as_map,
            )
        }

        fn each(m: Map, f: &Value) -> Step {
            iterate("map.each", super::entries(m), f.clone(), (), key_and_value, ignore, unit)
        }

        fn update(m: Map, key: &Value, default: &Value, f: &Value) -> Result<Step, VmError> {
            // Runtime Fn gate on the KEY only — same rationale as the
            // `map.from_entries` gate (its signature has no bound, so a
            // Fn key typechecks and both the `m.get` probe and the
            // `insert` below would compare it by Arc pointer address).
            // The default and the callback result are map VALUES and
            // stay ungated.
            ensure_no_fn("map.update", "Hash", [key])?;
            let current = m.get(key).unwrap_or(default).clone();
            let (m, key) = (m.clone(), key.clone());
            Ok(call_then("map.update", f.clone(), current, move |new_val| {
                let mut new_map = (*m).clone();
                new_map.insert(key, new_val);
                Ok(Value::Map(Arc::new(new_map)))
            }))
        }

        fn get(m: Map, k: &Value) -> Option<Value> {
            m.get(k).cloned()
        }

        fn set(m: Map, k: &Value, v: &Value) -> BTreeMap<Value, Value> {
            let mut new_map = (**m).clone();
            new_map.insert(k.clone(), v.clone());
            new_map
        }

        fn delete(m: Map, key: &Value) -> BTreeMap<Value, Value> {
            let mut new_map = (**m).clone();
            new_map.remove(key);
            new_map
        }

        fn contains(m: Map, key: &Value) -> bool {
            m.contains_key(key)
        }

        fn keys(m: Map) -> Vec<Value> {
            m.keys().cloned().collect()
        }

        fn values(m: Map) -> Vec<Value> {
            m.values().cloned().collect()
        }

        fn length(m: Map) -> i64 {
            m.len() as i64
        }

        fn merge(m1: Map, m2: Map) -> BTreeMap<Value, Value> {
            let mut result = (**m1).clone();
            result.extend(m2.iter().map(|(k, v)| (k.clone(), v.clone())));
            result
        }

        fn entries(m: Map) -> Vec<Value> {
            super::entries(m)
        }

        fn from_entries(entries: List) -> Result<BTreeMap<Value, Value>, VmError> {
            let mut result = BTreeMap::new();
            for entry in entries.iter() {
                let pair = match entry {
                    Value::Tuple(pair) => <[Value; 2]>::try_from(pair).ok(),
                    _ => None,
                };
                let [key, value] = pair.ok_or_else(|| unsound("map.from_entries", "entries"))?;
                // Runtime Fn gate on the KEY only (values are never
                // compared): `map.from_entries` has no `where` bound
                // that rules a function out as a key, and such keys
                // would be ordered by Arc pointer address —
                // ASLR-nondeterministic entry order. The trait name
                // matches the static map-key contract.
                ensure_no_fn("map.from_entries", "Hash", [&key])?;
                result.insert(key, value);
            }
            Ok(result)
        }
    }
}

/// `set.*`
pub(crate) mod set {
    use super::*;

    /// The elements of a set as items.
    fn elements(s: Set) -> Vec<Value> {
        s.iter().cloned().collect()
    }

    /// The operations on two sets walk both anyway, so both are
    /// gated: that catches sets with functions in them that an ungated
    /// producer built (a `set.map` callback that returns closures).
    fn both_without_fn(name: &str, a: Set, b: Set) -> Result<(), VmError> {
        ensure_no_fn(name, "Compare", a.iter().chain(b.iter()))
    }

    builtins! {
        fn map(s: Set, f: &Value) -> Step {
            iterate("set.map", elements(s), f.clone(), Vec::new(), item_arg, keep_result, as_set)
        }

        fn filter(s: Set, f: &Value) -> Step {
            iterate(
                "set.filter",
                elements(s),
                f.clone(),
                Vec::new(),
                item_arg,
                keep_item_if,
                as_set,
            )
        }

        fn each(s: Set, f: &Value) -> Step {
            iterate("set.each", elements(s), f.clone(), (), item_arg, ignore, unit)
        }

        fn fold(s: Set, init: &Value, f: &Value) -> Step {
            iterate(
                "set.fold",
                elements(s),
                f.clone(),
                init.clone(),
                acc_and_item,
                set_acc,
                take_acc,
            )
        }

        fn new() -> BTreeSet<Value> {
            BTreeSet::new()
        }

        fn from_list(xs: List) -> Result<BTreeSet<Value>, VmError> {
            let items = xs.to_vec()?;
            // A set of Fn values is BTree-ordered by Arc pointer address —
            // ASLR-nondeterministic iteration order. Reject at construction.
            ensure_no_fn("set.from_list", "Compare", items.iter())?;
            Ok(items.into_iter().collect())
        }

        fn to_list(s: Set) -> Vec<Value> {
            elements(s)
        }

        // Only the probe is gated (the lookup stays O(log n)): the
        // typechecker unifies the probe's type with the elements', so a
        // set with functions in it can only be probed with a value
        // that has one. Likewise for `insert` and `remove`.
        fn contains(s: Set, elem: &Value) -> Result<bool, VmError> {
            ensure_no_fn("set.contains", "Compare", [elem])?;
            Ok(s.contains(elem))
        }

        fn insert(s: Set, elem: &Value) -> Result<BTreeSet<Value>, VmError> {
            ensure_no_fn("set.insert", "Compare", [elem])?;
            let mut new_set = (**s).clone();
            new_set.insert(elem.clone());
            Ok(new_set)
        }

        fn remove(s: Set, elem: &Value) -> Result<BTreeSet<Value>, VmError> {
            ensure_no_fn("set.remove", "Compare", [elem])?;
            let mut new_set = (**s).clone();
            new_set.remove(elem);
            Ok(new_set)
        }

        fn length(s: Set) -> i64 {
            s.len() as i64
        }

        fn union(a: Set, b: Set) -> Result<BTreeSet<Value>, VmError> {
            both_without_fn("set.union", a, b)?;
            Ok(a.union(b).cloned().collect())
        }

        fn intersection(a: Set, b: Set) -> Result<BTreeSet<Value>, VmError> {
            both_without_fn("set.intersection", a, b)?;
            Ok(a.intersection(b).cloned().collect())
        }

        fn difference(a: Set, b: Set) -> Result<BTreeSet<Value>, VmError> {
            both_without_fn("set.difference", a, b)?;
            Ok(a.difference(b).cloned().collect())
        }

        fn is_subset(a: Set, b: Set) -> Result<bool, VmError> {
            both_without_fn("set.is_subset", a, b)?;
            Ok(a.is_subset(b))
        }

        fn symmetric_difference(a: Set, b: Set) -> Result<BTreeSet<Value>, VmError> {
            both_without_fn("set.symmetric_difference", a, b)?;
            Ok(a.symmetric_difference(b).cloned().collect())
        }
    }
}
