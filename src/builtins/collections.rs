//! Collection builtin functions (`list.*`, `map.*`, `set.*`).

use std::collections::{BTreeMap, BTreeSet};

use super::typed::{List, Map, Set, builtins, unsound};
use crate::typeinfo::bv;
use crate::value::{IntTotal, MAX_RANGE_MATERIALIZE, Value};
use crate::vm::{Flow, Native, Step, Vm, VmError, call_then, item_arg, iterate, next, stop};

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
/// The walk is `Value::contains_fn` (src/value/mod.rs), the one answer
/// of the run time for every such gate (operator, method and builtin).
fn ensure_no_fn<'a>(
    fn_name: &str,
    trait_name: &str,
    vals: impl IntoIterator<Item = &'a Value>,
) -> Result<(), VmError> {
    match vals.into_iter().any(Value::contains_fn) {
        true => Err(fn_gate(fn_name, trait_name)),
        false => Ok(()),
    }
}

/// [`ensure_no_fn`] for the elements of a list.
fn ensure_no_fn_in(fn_name: &str, trait_name: &str, xs: List) -> Result<(), VmError> {
    match xs.contains_fn() {
        true => Err(fn_gate(fn_name, trait_name)),
        false => Ok(()),
    }
}

fn fn_gate(fn_name: &str, trait_name: &str) -> VmError {
    VmError::new(format!(
        "{fn_name}: type 'Fn' does not implement {trait_name}"
    ))
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

/// The list `xs` for a builtin that makes a list with an element for
/// each of its elements, or for some of them: of a list that holds no
/// element (`a..b`) there may be too many for that. (A builtin that
/// only reads the elements, like `list.fold` or `list.find`, reads them
/// one by one however many there are.)
fn made(xs: List) -> Result<crate::value::List, VmError> {
    xs.writable()?;
    Ok(xs.clone())
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

/// The list of the elements of `xs` and then those of `ys`:
/// `list.concat(xs, ys)`, and `[..xs, ..ys]`.
///
/// Both sizes are checked before an element of either is made: two
/// operands near the limit would make a list of 800 MB.
pub(crate) fn concat(xs: List, ys: List) -> Result<Value, VmError> {
    xs.writable()?;
    ys.writable()?;
    if xs.len() + ys.len() > MAX_RANGE_MATERIALIZE {
        return Err(VmError::new(format!(
            "concatenated list exceeds maximum size of {} elements",
            MAX_RANGE_MATERIALIZE
        )));
    }
    Ok(Value::List(xs.concat(ys)?))
}

fn as_list(out: &mut Vec<Value>) -> Result<Value, VmError> {
    Ok(Value::list(std::mem::take(out)))
}

fn as_set(out: &mut Vec<Value>) -> Result<Value, VmError> {
    Ok(Value::set(std::mem::take(out)))
}

fn as_map(out: &mut Vec<(Value, Value)>) -> Result<Value, VmError> {
    Ok(Value::map(std::mem::take(out)))
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
            // Whether the list fits is checked before its elements are
            // made, alone and added to the list so far: a callback that
            // returns `0..i64::MAX` must not exhaust memory.
            inner
                .writable()
                .map_err(|m| VmError::new(format!("list.flat_map: {m}")))?;
            if out.len().saturating_add(inner.len()) > MAX_RANGE_MATERIALIZE {
                return Err(too_long("list.flat_map"));
            }
            out.extend(inner);
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
        Value::Variant(variant) if variant.is(bv::SOME) && variant.fields().len() == 1 => {
            out.push(variant.fields()[0].clone());
        }
        Value::Variant(variant) if variant.is(bv::NONE) => {}
        other => out.push(other),
    }
    next()
}

fn fold_until_step(acc: &mut Value, _item: Value, result: Value) -> Flow {
    match result {
        Value::Variant(variant) if variant.is(bv::CONTINUE) && variant.fields().len() == 1 => {
            *acc = variant.fields()[0].clone();
            next()
        }
        Value::Variant(variant) if variant.is(bv::STOP) && variant.fields().len() == 1 => {
            stop(variant.fields()[0].clone())
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
                Value::Variant(variant) if variant.is(bv::SOME) && variant.fields().len() == 1 => {
                    match &variant.fields()[0] {
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
                Value::Variant(variant) if variant.is(bv::NONE) => {
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

/// `list.sort_with`: a merge sort, bottom up, that calls the
/// comparison for each pair it compares. Runs of `width` elements of
/// `from` are merged pairwise into `into`, then the width doubles. An
/// element of the right run goes first only when the comparison says
/// it is less, so equal elements keep their order.
struct SortWith {
    compare: Value,
    from: Vec<Value>,
    into: Vec<Value>,
    width: usize,
    /// The merge under way: the next element of the left run, its end,
    /// the next element of the right run, its end.
    left: usize,
    mid: usize,
    right: usize,
    end: usize,
    /// A comparison of `from[left]` with `from[right]` is running.
    comparing: bool,
}

impl SortWith {
    fn new(compare: Value, items: Vec<Value>) -> SortWith {
        let mut sort = SortWith {
            compare,
            from: items,
            into: Vec::new(),
            width: 1,
            left: 0,
            mid: 0,
            right: 0,
            end: 0,
            comparing: false,
        };
        sort.start_merge(0);
        sort
    }

    /// Set up the merge of the two runs that start at `start`.
    fn start_merge(&mut self, start: usize) {
        let n = self.from.len();
        self.left = start;
        self.mid = (start + self.width).min(n);
        self.right = self.mid;
        self.end = (start + 2 * self.width).min(n);
    }
}

impl Native for SortWith {
    fn name(&self) -> &str {
        "list.sort_with"
    }

    fn resume(&mut self, vm: &mut Vm, input: Value) -> Result<Step, VmError> {
        if self.comparing {
            self.comparing = false;
            let right_first = match input {
                Value::Int(order) => order > 0,
                other => {
                    return Err(VmError::new(format!(
                        "list.sort_with: the comparison returned {}, not an Int",
                        other.format_silt()
                    )));
                }
            };
            let taken = match right_first {
                true => &mut self.right,
                false => &mut self.left,
            };
            self.into.push(self.from[*taken].clone());
            *taken += 1;
        }
        loop {
            let n = self.from.len();
            if self.width >= n {
                return Ok(Step::Done(Value::list(std::mem::take(&mut self.from))));
            }
            if self.left < self.mid && self.right < self.end {
                self.comparing = true;
                let (a, b) = (self.from[self.left].clone(), self.from[self.right].clone());
                return Ok(vm.call(self.compare.clone(), [a, b]));
            }
            // One run is used up: the rest of the other follows as it is.
            self.into.extend_from_slice(&self.from[self.left..self.mid]);
            self.into
                .extend_from_slice(&self.from[self.right..self.end]);
            if self.end < n {
                self.start_merge(self.end);
            } else {
                // A pass is done: its result is the next pass's input.
                self.from = std::mem::take(&mut self.into);
                self.width *= 2;
                self.start_merge(0);
            }
        }
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
                made(xs)?,
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
                made(xs)?,
                f.clone(),
                Vec::new(),
                item_arg,
                keep_item_if,
                as_list,
            ))
        }

        fn each(xs: List, f: &Value) -> Step {
            iterate("list.each", xs.clone(), f.clone(), (), item_arg, ignore, unit)
        }

        fn fold(xs: List, init: &Value, f: &Value) -> Step {
            iterate(
                "list.fold",
                xs.clone(),
                f.clone(),
                init.clone(),
                acc_and_item,
                set_acc,
                take_acc,
            )
        }

        fn find(xs: List, f: &Value) -> Step {
            iterate(
                "list.find",
                xs.clone(),
                f.clone(),
                (),
                item_arg,
                |_, item, found| match truthy(&found) {
                    true => stop(some(item)),
                    false => next(),
                },
                |_| Ok(none()),
            )
        }

        fn any(xs: List, f: &Value) -> Step {
            iterate(
                "list.any",
                xs.clone(),
                f.clone(),
                (),
                item_arg,
                |_, _, holds| match truthy(&holds) {
                    true => stop(Value::Bool(true)),
                    false => next(),
                },
                |_| Ok(Value::Bool(false)),
            )
        }

        fn all(xs: List, f: &Value) -> Step {
            iterate(
                "list.all",
                xs.clone(),
                f.clone(),
                (),
                item_arg,
                |_, _, holds| match truthy(&holds) {
                    true => next(),
                    false => stop(Value::Bool(false)),
                },
                |_| Ok(Value::Bool(true)),
            )
        }

        fn flat_map(xs: List, f: &Value) -> Result<Step, VmError> {
            Ok(iterate(
                "list.flat_map",
                made(xs)?,
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
                made(xs)?,
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
                made(xs)?,
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
                    Ok(Value::list(sorted.collect()))
                },
            ))
        }

        fn sort_with(xs: List, compare: &Value) -> Result<Step, VmError> {
            let items = made(xs)?.into_iter().collect();
            Ok(Step::Run(Box::new(SortWith::new(compare.clone(), items))))
        }

        // The first element of each key, in the list's order.
        fn unique_by(xs: List, key: &Value) -> Result<Step, VmError> {
            Ok(iterate(
                "list.unique_by",
                made(xs)?,
                key.clone(),
                (BTreeSet::<Value>::new(), Vec::new()),
                item_arg,
                |(seen, kept), item, key| {
                    key_without_fn("list.unique_by", "Hash", &key)?;
                    if seen.insert(key) {
                        kept.push(item);
                    }
                    next()
                },
                |(_, kept)| as_list(kept),
            ))
        }

        fn fold_until(xs: List, init: &Value, f: &Value) -> Step {
            iterate(
                "list.fold_until",
                xs.clone(),
                f.clone(),
                init.clone(),
                acc_and_item,
                fold_until_step,
                take_acc,
            )
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
                made(xs)?,
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
                    Ok(Value::map(groups.map(|(k, v)| (k, Value::list(v)))))
                },
            ))
        }

        fn min_by(xs: List, key: &Value) -> Step {
            iterate(
                "list.min_by",
                xs.clone(),
                key.clone(),
                None,
                item_arg,
                |best, item, key| {
                    best_step("list.min_by", std::cmp::Ordering::Less, best, item, key)
                },
                best_item,
            )
        }

        fn max_by(xs: List, key: &Value) -> Step {
            iterate(
                "list.max_by",
                xs.clone(),
                key.clone(),
                None,
                item_arg,
                |best, item, key| {
                    best_step("list.max_by", std::cmp::Ordering::Greater, best, item, key)
                },
                best_item,
            )
        }

        // The running value, and the list of the values it had, the
        // initial one first.
        fn scan(xs: List, init: &Value, f: &Value) -> Result<Step, VmError> {
            Ok(iterate(
                "list.scan",
                made(xs)?,
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
            if expected > MAX_RANGE_MATERIALIZE {
                return Err(VmError::new(format!(
                    "list.zip: result length {expected} exceeds maximum materialized length {MAX_RANGE_MATERIALIZE}"
                )));
            }
            Ok(xs
                .iter()
                .zip(ys.iter())
                .map(|(x, y)| Value::tuple(vec![x, y]))
                .collect())
        }

        fn flatten(xs: List) -> Result<Vec<Value>, VmError> {
            let mut result = Vec::new();
            for item in xs.iter() {
                match item {
                    Value::List(inner) => {
                        inner.writable()?;
                        result.extend(inner);
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
            xs.first()
        }

        fn tail(xs: List) -> Value {
            Value::List(xs.slice(1, xs.len()))
        }

        fn last(xs: List) -> Option<Value> {
            xs.last()
        }

        fn reverse(xs: List) -> Result<Value, VmError> {
            xs.writable()?;
            Ok(Value::List(xs.iter().rev().collect()))
        }

        fn sort(xs: List) -> Result<Value, VmError> {
            // Fn elements of two functions would sort by the functions'
            // addresses (ASLR-nondeterministic) — reject like the
            // operator gates do.
            ensure_no_fn_in("list.sort", "Compare", xs)?;
            Ok(Value::List(xs.sorted()))
        }

        fn unique(xs: List) -> Result<Value, VmError> {
            // Fn elements would dedup by identity (function and captures
            // / builtin name) instead of erroring like `f == g` does.
            ensure_no_fn_in("list.unique", "Equal", xs)?;
            Ok(Value::List(xs.unique()))
        }

        fn contains(xs: List, elem: &Value) -> Result<bool, VmError> {
            // Fn membership would silently answer by function identity
            // (`list.contains([f], g)` -> false) instead of erroring.
            ensure_no_fn_in("list.contains", "Equal", xs)?;
            ensure_no_fn("list.contains", "Equal", [elem])?;
            Ok(xs.contains(elem))
        }

        // The list of the Ints between two far ends can have more
        // elements than an `Int` counts.
        fn length(xs: List) -> Result<i64, VmError> {
            i64::try_from(xs.len()).map_err(|_| {
                VmError::new(
                    "list.length overflow: the list has more elements than an Int counts".into(),
                )
            })
        }

        fn is_empty(xs: List) -> bool {
            xs.is_empty()
        }

        fn append(xs: List, elem: &Value) -> Result<Value, VmError> {
            Ok(Value::List(xs.push_back(elem.clone())?))
        }

        fn prepend(xs: List, elem: &Value) -> Result<Value, VmError> {
            Ok(Value::List(xs.push_front(elem.clone())?))
        }

        fn concat(xs: List, ys: List) -> Result<Value, VmError> {
            super::concat(xs, ys)
        }

        // An index is a place in the list or it is none: a negative
        // one, or one past the end, is `None` in all three.
        fn get(xs: List, i: i64) -> Option<Value> {
            xs.get(usize::try_from(i).ok()?)
        }

        fn set(xs: List, index: i64, value: &Value) -> Result<Option<Value>, VmError> {
            let Ok(index) = usize::try_from(index) else {
                return Ok(None);
            };
            Ok(xs.set(index, value.clone())?.map(Value::List))
        }

        fn take(xs: List, n: i64) -> Result<Value, VmError> {
            let count = natural("list.take", "count", n)?;
            Ok(Value::List(xs.slice(0, count)))
        }

        fn drop(xs: List, n: i64) -> Result<Value, VmError> {
            let count = natural("list.drop", "count", n)?;
            Ok(Value::List(xs.slice(count, xs.len())))
        }

        fn enumerate(xs: List) -> Result<Vec<Value>, VmError> {
            xs.writable()?;
            let items = xs.iter().enumerate();
            Ok(items
                .map(|(i, item)| Value::tuple(vec![Value::Int(i as i64), item]))
                .collect())
        }

        fn index_of(xs: List, target: &Value) -> Result<Option<Value>, VmError> {
            // Fn search would silently answer by function identity
            // (`list.index_of([f, g], g)` -> Some(1)) instead of erroring.
            ensure_no_fn_in("list.index_of", "Equal", xs)?;
            ensure_no_fn("list.index_of", "Equal", [target])?;
            let Some(at) = xs.position(target) else {
                return Ok(None);
            };
            let at = i64::try_from(at).map_err(|_| {
                VmError::new("list.index_of overflow: index too large to represent as Int".into())
            })?;
            Ok(Some(Value::Int(at)))
        }

        fn remove_at(xs: List, index: i64) -> Result<Option<Value>, VmError> {
            let Ok(index) = usize::try_from(index) else {
                return Ok(None);
            };
            Ok(xs.remove_at(index)?.map(Value::List))
        }

        fn sum(xs: List) -> Result<i64, VmError> {
            match xs.sum_ints() {
                IntTotal::Total(total) => Ok(total),
                IntTotal::Overflow => Err(VmError::new("list.sum overflow".into())),
                IntTotal::NotInts => Err(unsound("list.sum", "xs")),
            }
        }

        fn sum_float(xs: List) -> Result<Value, VmError> {
            let mut total: f64 = 0.0;
            for item in xs.iter() {
                let Value::Float(n) = item else {
                    return Err(unsound("list.sum_float", "xs"));
                };
                total += n.get();
            }
            // A finite sum only ever overflows to ±inf, and the sum
            // cannot come back from there, so one check at the end sees it.
            crate::builtins::numeric::checked_float(total, || "list.sum_float overflow".into())
        }

        fn product(xs: List) -> Result<i64, VmError> {
            match xs.product_ints() {
                IntTotal::Total(total) => Ok(total),
                IntTotal::Overflow => Err(VmError::new("list.product overflow".into())),
                IntTotal::NotInts => Err(unsound("list.product", "xs")),
            }
        }

        fn product_float(xs: List) -> Result<Value, VmError> {
            let mut total: f64 = 1.0;
            for item in xs.iter() {
                let Value::Float(n) = item else {
                    return Err(unsound("list.product_float", "xs"));
                };
                total *= n.get();
            }
            // As in `sum_float`: once the product leaves the finite range
            // it stays out, so one check at the end sees it.
            crate::builtins::numeric::checked_float(total, || "list.product_float overflow".into())
        }

        fn intersperse(xs: List, sep: &Value) -> Result<Value, VmError> {
            xs.writable()?;
            // Result length = 2*N - 1
            let out_len = xs.len().saturating_mul(2).saturating_sub(1);
            if out_len > MAX_RANGE_MATERIALIZE {
                return Err(VmError::new(format!(
                    "list.intersperse: result length {out_len} exceeds maximum materialized length {MAX_RANGE_MATERIALIZE}"
                )));
            }
            let mut items = xs.iter();
            let first = items.next();
            let rest = items.flat_map(|item| [sep.clone(), item]);
            Ok(Value::List(first.into_iter().chain(rest).collect()))
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
                m.clone(),
                f.clone(),
                Vec::new(),
                key_and_value,
                |kept, entry, keep| {
                    if truthy(&keep)
                        && let Value::Tuple(pair) = entry
                        && let [k, v] = &pair[..]
                    {
                        kept.push((k.clone(), v.clone()));
                    }
                    next()
                },
                as_map,
            )
        }

        fn map(m: Map, f: &Value) -> Step {
            iterate(
                "map.map",
                m.clone(),
                f.clone(),
                Vec::new(),
                key_and_value,
                |out, _, result| {
                    let pair = match &result {
                        Value::Tuple(pair) => <&[Value; 2]>::try_from(&pair[..]).ok(),
                        _ => None,
                    };
                    let [k, v] = pair.ok_or_else(|| unsound("map.map", "f"))?.clone();
                    out.push((k, v));
                    next()
                },
                as_map,
            )
        }

        fn each(m: Map, f: &Value) -> Step {
            iterate("map.each", m.clone(), f.clone(), (), key_and_value, ignore, unit)
        }

        fn update(m: Map, key: &Value, default: &Value, f: &Value) -> Result<Step, VmError> {
            // Runtime Fn gate on the KEY only — same rationale as the
            // `map.from_entries` gate (its signature has no bound, so a
            // Fn key typechecks and both the `m.get` probe and the
            // `insert` below would order it by its function's address).
            // The default and the callback result are map VALUES and
            // stay ungated.
            ensure_no_fn("map.update", "Hash", [key])?;
            let current = m.get(key).unwrap_or(default).clone();
            let (m, key) = (m.clone(), key.clone());
            Ok(call_then("map.update", f.clone(), current, move |new_val| {
                Ok(Value::Map(m.insert(key, new_val)))
            }))
        }

        fn get(m: Map, k: &Value) -> Option<Value> {
            m.get(k).cloned()
        }

        fn set(m: Map, k: &Value, v: &Value) -> Value {
            Value::Map(m.insert(k.clone(), v.clone()))
        }

        fn delete(m: Map, key: &Value) -> Value {
            Value::Map(m.remove(key))
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

        fn is_empty(m: Map) -> bool {
            m.is_empty()
        }

        fn merge(m1: Map, m2: Map) -> Value {
            Value::Map(m1.merge(m2))
        }

        fn entries(m: Map) -> Value {
            let entries = m.iter();
            Value::List(entries.map(|(k, v)| Value::tuple(vec![k.clone(), v.clone()])).collect())
        }

        fn from_entries(entries: List) -> Result<Value, VmError> {
            let mut result = Vec::with_capacity(entries.len());
            for entry in entries.iter() {
                let pair = match &entry {
                    Value::Tuple(pair) => <&[Value; 2]>::try_from(&pair[..]).ok(),
                    _ => None,
                };
                let [key, value] = pair
                    .ok_or_else(|| unsound("map.from_entries", "entries"))?
                    .clone();
                // Runtime Fn gate on the KEY only (values are never
                // compared): `map.from_entries` has no `where` bound
                // that rules a function out as a key, and such keys
                // would be ordered by their functions' addresses —
                // ASLR-nondeterministic entry order. The trait name
                // matches the static map-key contract.
                ensure_no_fn("map.from_entries", "Hash", [&key])?;
                result.push((key, value));
            }
            Ok(Value::map(result))
        }
    }
}

/// `set.*`
pub(crate) mod set {
    use super::*;

    /// The operations on two sets are gated on both: that catches sets
    /// with functions in them that an ungated producer built (a
    /// `set.map` callback that returns closures). A set keeps the
    /// answer once it is known ([`Set::holds_fn`]), so a fold of
    /// `set.union` does not walk the set it grows at every step.
    fn both_without_fn(name: &str, a: Set, b: Set) -> Result<(), VmError> {
        match a.holds_fn() || b.holds_fn() {
            true => Err(fn_gate(name, "Compare")),
            false => Ok(()),
        }
    }

    builtins! {
        fn map(s: Set, f: &Value) -> Step {
            iterate("set.map", s.clone(), f.clone(), Vec::new(), item_arg, keep_result, as_set)
        }

        fn filter(s: Set, f: &Value) -> Step {
            iterate(
                "set.filter",
                s.clone(),
                f.clone(),
                Vec::new(),
                item_arg,
                keep_item_if,
                as_set,
            )
        }

        fn each(s: Set, f: &Value) -> Step {
            iterate("set.each", s.clone(), f.clone(), (), item_arg, ignore, unit)
        }

        fn fold(s: Set, init: &Value, f: &Value) -> Step {
            iterate(
                "set.fold",
                s.clone(),
                f.clone(),
                init.clone(),
                acc_and_item,
                set_acc,
                take_acc,
            )
        }

        fn new() -> Value {
            Value::Set(crate::value::Set::new())
        }

        fn from_list(xs: List) -> Result<Value, VmError> {
            xs.writable()?;
            // A set of Fn values is ordered by the functions' addresses —
            // ASLR-nondeterministic iteration order. Reject at construction.
            ensure_no_fn_in("set.from_list", "Compare", xs)?;
            Ok(Value::Set(crate::value::Set::without_fn(xs)))
        }

        fn to_list(s: Set) -> Value {
            Value::List(s.iter().cloned().collect())
        }

        // Only the probe is gated (the lookup stays O(log n)): the
        // typechecker unifies the probe's type with the elements', so a
        // set with functions in it can only be probed with a value
        // that has one. Likewise for `insert` and `remove`.
        fn contains(s: Set, elem: &Value) -> Result<bool, VmError> {
            ensure_no_fn("set.contains", "Compare", [elem])?;
            Ok(s.contains(elem))
        }

        fn insert(s: Set, elem: &Value) -> Result<Value, VmError> {
            ensure_no_fn("set.insert", "Compare", [elem])?;
            Ok(Value::Set(s.insert(elem.clone())))
        }

        fn remove(s: Set, elem: &Value) -> Result<Value, VmError> {
            ensure_no_fn("set.remove", "Compare", [elem])?;
            Ok(Value::Set(s.remove(elem)))
        }

        fn length(s: Set) -> i64 {
            s.len() as i64
        }

        fn is_empty(s: Set) -> bool {
            s.is_empty()
        }

        fn union(a: Set, b: Set) -> Result<Value, VmError> {
            both_without_fn("set.union", a, b)?;
            Ok(Value::Set(a.union(b)))
        }

        fn intersection(a: Set, b: Set) -> Result<Value, VmError> {
            both_without_fn("set.intersection", a, b)?;
            Ok(Value::Set(a.intersection(b)))
        }

        fn difference(a: Set, b: Set) -> Result<Value, VmError> {
            both_without_fn("set.difference", a, b)?;
            Ok(Value::Set(a.difference(b)))
        }

        fn is_subset(a: Set, b: Set) -> Result<bool, VmError> {
            both_without_fn("set.is_subset", a, b)?;
            Ok(a.is_subset(b))
        }

        fn symmetric_difference(a: Set, b: Set) -> Result<Value, VmError> {
            both_without_fn("set.symmetric_difference", a, b)?;
            Ok(Value::Set(a.symmetric_difference(b)))
        }
    }
}
