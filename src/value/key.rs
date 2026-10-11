//! The key of a value: whether two values are equal, which of two
//! comes first, and a value's hash.
//!
//! All three read a value the same way: as its head (its kind, and
//! what tells it from another value of the kind before its parts do)
//! and its parts in order (the values it is made of). [`walk`] goes
//! through one value, or through two side by side, part by part;
//! [`Equal`], [`Order`] and [`Hashed`] say what is done at each step.
//!
//! The walk does not go down a value on the native stack: it calls
//! itself for the first few levels ([`NEAR`]), which is all most
//! values have, and below them keeps what it has yet to come back to
//! in a stack of its own ([`through_far`]). A value nested a million
//! levels deep is compared and hashed like any other.
//!
//! The kinds of value are ranked in one place ([`rank`]): the rank
//! orders two values of different kinds, and is the first byte of a
//! value's hash.
//!
//! A record is of its type: two records are equal if they are of one
//! type and their fields are equal, and records of one type are in the
//! order of their fields as the type declares them. An anonymous record
//! is not a declared one of the same fields.

use std::cmp::Ordering;
use std::collections::{btree_map, btree_set};
use std::convert::Infallible;
use std::hash::{Hash, Hasher};
use std::ops::ControlFlow::{self, Break, Continue};
use std::sync::Arc;

use super::list::Elements;
use super::{List, Value};
use crate::typeinfo::TypeInfo;

// ── A value's head and its parts ───────────────────────────────────
//
// The head of a value is its kind, and what tells it from another
// value of the kind before its parts do (all of the value, if it has
// no parts): a number, a text, which variant of which type, which
// channel. [`rank`], [`cmp_heads`] and [`hash_head`] read it.

/// The rank of the value's kind: the one table of the kinds. A value
/// of a kind of lower rank comes before one of a higher, and the rank
/// is the first byte of a value's hash.
fn rank(value: &Value) -> u8 {
    match value {
        Value::Unit => 0,
        Value::Bool(_) => 1,
        Value::Int(_) => 2,
        Value::Float(_) => 3,
        Value::String(_) => 4,
        Value::List(_) => 5,
        Value::Tuple(_) => 6,
        Value::Map(_) => 7,
        Value::Set(_) => 8,
        Value::Record(_) => 9,
        Value::Variant(_) => 10,
        Value::Channel(_) => 11,
        Value::Handle(_) => 12,
        Value::Bytes(_) => 13,
        Value::TcpListener(_) => 14,
        Value::TcpStream(_) => 15,
        Value::VmClosure(_) => 16,
        Value::BuiltinFn(_) => 17,
        Value::VariantConstructor(_) => 18,
        Value::TypeDescriptor(_) => 19,
        Value::PrimitiveDescriptor(_) => 20,
        Value::HostFn(_) => 21,
    }
}

/// Whether `value` is made of values: a value that is not is its head.
#[inline]
fn has_parts(value: &Value) -> bool {
    matches!(
        value,
        Value::List(_)
            | Value::Tuple(_)
            | Value::Map(_)
            | Value::Set(_)
            | Value::Record(_)
            | Value::Variant(_)
    )
}

/// The order of the heads of two values: of one kind by what the heads
/// hold (a list's, a tuple's, a map's and a set's hold nothing), of
/// two kinds by rank.
#[inline(always)]
fn cmp_heads(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => a.cmp(b),
        (Value::String(a), Value::String(b)) => a.cmp(b),
        (Value::Float(a), Value::Float(b)) => a.cmp(b),
        (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
        (Value::Unit, Value::Unit)
        | (Value::List(_), Value::List(_))
        | (Value::Tuple(_), Value::Tuple(_))
        | (Value::Map(_), Value::Map(_))
        | (Value::Set(_), Value::Set(_)) => Ordering::Equal,
        // Variants of one type order by declaration; variants of two
        // types (which a program cannot compare) by the types.
        (Value::Variant(a), Value::Variant(b)) => {
            (a.type_id(), a.ordinal()).cmp(&(b.type_id(), b.ordinal()))
        }
        (Value::Record(a), Value::Record(b)) => cmp_record_types(a.ty(), b.ty()),
        _ => cmp_other_heads(a, b),
    }
}

/// [`cmp_heads`] for the kinds a program seldom compares, and for two
/// kinds.
fn cmp_other_heads(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Bytes(a), Value::Bytes(b)) => a.cmp(b),
        // A channel, a task handle, a listener, a stream: which one.
        (Value::Channel(a), Value::Channel(b)) => a.id().cmp(&b.id()),
        (Value::Handle(a), Value::Handle(b)) => a.id.cmp(&b.id),
        (Value::TcpListener(a), Value::TcpListener(b)) => a.id.cmp(&b.id),
        (Value::TcpStream(a), Value::TcpStream(b)) => a.id.cmp(&b.id),
        // A closure: where it is. (Two closures of one function with
        // different captures are two.)
        (Value::VmClosure(a), Value::VmClosure(b)) => {
            (Arc::as_ptr(a) as usize).cmp(&(Arc::as_ptr(b) as usize))
        }
        (Value::BuiltinFn(a), Value::BuiltinFn(b)) => a.cmp(b),
        (Value::HostFn(a), Value::HostFn(b)) => a.name.cmp(&b.name),
        (Value::VariantConstructor(a), Value::VariantConstructor(b)) => a.cmp(b),
        (Value::TypeDescriptor(a), Value::TypeDescriptor(b)) => a.id.cmp(&b.id),
        (Value::PrimitiveDescriptor(a), Value::PrimitiveDescriptor(b)) => a.cmp(b),
        _ => rank(a).cmp(&rank(b)),
    }
}

/// The order of the types of two records: by the types' ids, and for
/// two anonymous records, whose types share one id, by their field
/// names.
#[inline]
fn cmp_record_types(a: &Arc<TypeInfo>, b: &Arc<TypeInfo>) -> Ordering {
    if Arc::ptr_eq(a, b) {
        return Ordering::Equal;
    }
    a.id.cmp(&b.id).then_with(|| match a.is_anon() {
        true => field_names(a).cmp(field_names(b)),
        false => Ordering::Equal,
    })
}

fn field_names(ty: &TypeInfo) -> impl Iterator<Item = &str> {
    ty.fields().iter().map(|(name, _)| &**name)
}

/// The head's part of a hash: its rank, then what it holds.
fn hash_head<H: Hasher>(value: &Value, state: &mut H) {
    state.write_u8(rank(value));
    match value {
        Value::Unit | Value::List(_) | Value::Tuple(_) | Value::Map(_) | Value::Set(_) => {}
        // (A record's fields are hashed with their names: its type
        // adds nothing to them.)
        Value::Record(_) => {}
        // (No function has a hash: the VM refuses a value that holds
        // one before it asks.)
        Value::VmClosure(_) => {}
        Value::Bool(b) => b.hash(state),
        Value::Int(n) => n.hash(state),
        Value::Float(f) => f.hash(state),
        Value::String(s) => s.hash(state),
        Value::Variant(variant) => {
            variant.type_id().hash(state);
            variant.ordinal().hash(state);
        }
        Value::Channel(channel) => channel.id().hash(state),
        Value::Handle(handle) => handle.id.hash(state),
        Value::Bytes(bytes) => {
            bytes.len().hash(state);
            state.write(bytes);
        }
        Value::TcpListener(listener) => listener.id.hash(state),
        Value::TcpStream(stream) => stream.id.hash(state),
        Value::BuiltinFn(id) => id.hash(state),
        Value::HostFn(host) => host.name.hash(state),
        Value::VariantConstructor(tag) => tag.hash(state),
        Value::TypeDescriptor(ty) => ty.id.hash(state),
        Value::PrimitiveDescriptor(name) => name.hash(state),
    }
}

/// `N` things that are there or not, taken together.
enum Among<T, const N: usize> {
    All([T; N]),
    None,
    /// Some are there: which are not.
    Some([bool; N]),
}

#[inline]
fn among<T: Copy, const N: usize>(items: [Option<T>; N]) -> Among<T, N> {
    // (Written as loops: over two items they are a few instructions,
    // where the adapters of an array's `map` were calls.)
    let mut missing = [false; N];
    let mut any = None;
    for (missing, item) in missing.iter_mut().zip(&items) {
        match item {
            Some(item) => any = Some(*item),
            None => *missing = true,
        }
    }
    let Some(any) = any else {
        return Among::None;
    };
    if missing.contains(&true) {
        return Among::Some(missing);
    }
    let mut all = [any; N];
    for (all, item) in all.iter_mut().zip(items) {
        if let Some(item) = item {
            *all = item;
        }
    }
    Among::All(all)
}

/// The next of each of `iters`, taken together.
#[inline]
fn next_of<I: Iterator, const N: usize>(iters: &mut [I; N]) -> Among<I::Item, N>
where
    I::Item: Copy,
{
    let mut next = [None; N];
    for (next, iter) in next.iter_mut().zip(iters) {
        *next = iter.next();
    }
    among(next)
}

/// The first of each of `slices`, taken together and taken off.
#[inline]
fn first_of<'a, const N: usize>(slices: &mut [&'a [Value]; N]) -> Among<&'a Value, N> {
    let mut next = [None; N];
    for (next, slice) in next.iter_mut().zip(slices) {
        if let Some((first, rest)) = slice.split_first() {
            *next = Some(first);
            *slice = rest;
        }
    }
    among(next)
}

/// The parts yet to be visited of `N` values of one kind, side by
/// side and in order: the values each is made of.
///
/// (Small, as the walk moves these about for every value with parts:
/// what is left of a map and of a set is kept apart.)
enum Parts<'a, const N: usize> {
    /// The items of tuples, the fields of variants, the elements that
    /// lists hold: those left.
    Values([&'a [Value]; N]),
    /// The fields of records, in the order the type of each declares
    /// them; the type of the first names them.
    Fields(&'a TypeInfo, [&'a [Value]; N]),
    /// The entries of maps in key order, the keys of an entry and then
    /// its values: the entries left, and the values of the keys last
    /// given.
    Entries(
        [Box<btree_map::Iter<'a, Value, Value>>; N],
        Option<[&'a Value; N]>,
    ),
    /// The members of sets, in order.
    Members([Box<btree_set::Iter<'a, Value>>; N]),
}

impl<'a, const N: usize> Parts<'a, N> {
    /// The entries of `values`, which are maps, or their members,
    /// which are sets; and the number of them in each. (They are of
    /// one kind, the first's.)
    fn of_many(values: [&'a Value; N]) -> (Option<[usize; N]>, Parts<'a, N>) {
        match values[0] {
            Value::Map(first) => {
                let maps = values.map(|value| match value {
                    Value::Map(entries) => entries,
                    _ => first,
                });
                let left = maps.map(|entries| Box::new(entries.iter()));
                (
                    Some(maps.map(|entries| entries.len())),
                    Parts::Entries(left, None),
                )
            }
            Value::Set(first) => {
                let sets = values.map(|value| match value {
                    Value::Set(members) => members,
                    _ => first,
                });
                let left = sets.map(|members| Box::new(members.iter()));
                (
                    Some(sets.map(|members| members.len())),
                    Parts::Members(left),
                )
            }
            _ => (None, Parts::Values([&[]; N])),
        }
    }

    /// The next part of each value.
    #[inline]
    fn next(&mut self) -> Among<&'a Value, N> {
        match self {
            Parts::Values(values) | Parts::Fields(_, values) => first_of(values),
            Parts::Entries(entries, values) => match values.take() {
                Some(values) => Among::All(values),
                None => match next_of(entries) {
                    Among::All(next) => {
                        *values = Some(next.map(|(_, value)| value));
                        Among::All(next.map(|(key, _)| key))
                    }
                    Among::None => Among::None,
                    Among::Some(ended) => Among::Some(ended),
                },
            },
            Parts::Members(members) => next_of(members),
        }
    }

    /// The name of the next part, if it is a record's field.
    #[inline]
    fn name(&self) -> Option<&'a str> {
        match self {
            Parts::Fields(ty, values) => next_name(ty, values[0]),
            _ => None,
        }
    }

    /// Whether no part of any of the values is left.
    #[inline]
    fn is_done(&self) -> bool {
        match self {
            Parts::Values(values) | Parts::Fields(_, values) => {
                values.iter().all(|values| values.is_empty())
            }
            Parts::Entries(entries, values) => {
                values.is_none() && entries.iter().all(|entries| entries.len() == 0)
            }
            Parts::Members(members) => members.iter().all(|members| members.len() == 0),
        }
    }
}

// ── The walk ───────────────────────────────────────────────────────

/// What is done at each step of a walk through `N` values side by
/// side. A step that stops the walk gives its outcome.
trait Visit<const N: usize> {
    type Stop;

    /// Whether the walk tells the name of each record field it comes
    /// to ([`Visit::name`]).
    const NAMES: bool = false;

    /// The heads of `N` values, side by side. The walk goes on to
    /// their parts only if they are of one kind.
    fn heads(&mut self, values: [&Value; N]) -> ControlFlow<Self::Stop>;

    /// The numbers of parts of `N` tuples, variants, maps or sets.
    fn lens(&mut self, lens: [usize; N]) -> ControlFlow<Self::Stop>;

    /// `N` lists: whether the walk goes on to their elements, which
    /// it may only if each list holds its elements. (A list that
    /// holds none is answered for here, from its two ends.)
    fn lists(&mut self, lists: [&List; N]) -> ControlFlow<Self::Stop, bool>;

    /// The name of the record field the walk comes to next.
    fn name(&mut self, _name: &str) {}

    /// Some of the values have no part left where the others have:
    /// `ended` says which. (The walk ends here.)
    fn uneven(&mut self, ended: [bool; N]) -> ControlFlow<Self::Stop>;
}

/// The parts of values that keep them in one run: the items of
/// tuples, the fields of variants and of records, the elements that
/// lists hold.
enum Runs<'a, const N: usize> {
    /// The parts of each value, and for records the type that names
    /// them.
    Of(Option<&'a TypeInfo>, [&'a [Value]; N]),
    /// Lists that are answered for without their elements.
    Done,
    /// Maps or sets: their parts are in no run.
    Not,
}

/// The parts of `values`, whose heads are visited and of one kind with
/// parts, if the values keep them in one run; with the step of the
/// visitor that comes before the parts.
#[inline(always)]
fn runs<'a, const N: usize, V: Visit<N>>(
    values: [&'a Value; N],
    visit: &mut V,
) -> ControlFlow<V::Stop, Runs<'a, N>> {
    // (The values are of one kind, the first's: the visitor went on
    // from their heads. One that is not has no parts here.)
    let mut runs: [&'a [Value]; N] = [&[]; N];
    match values[0] {
        Value::Tuple(_) | Value::Variant(_) => {
            let mut lens = [0; N];
            for ((run, len), value) in runs.iter_mut().zip(&mut lens).zip(values) {
                *run = match value {
                    Value::Tuple(items) => items,
                    Value::Variant(variant) => variant.fields(),
                    _ => &[],
                };
                *len = run.len();
            }
            visit.lens(lens)?;
            Continue(Runs::Of(None, runs))
        }
        Value::Record(first) => {
            for (run, value) in runs.iter_mut().zip(values) {
                if let Value::Record(record) = value {
                    *run = record.fields();
                }
            }
            Continue(Runs::Of(Some(first.ty()), runs))
        }
        Value::List(first) => {
            let mut lists = [first; N];
            for (list, value) in lists.iter_mut().zip(values) {
                if let Value::List(of_value) = value {
                    *list = of_value;
                }
            }
            if !visit.lists(lists)? {
                return Continue(Runs::Done);
            }
            for (run, list) in runs.iter_mut().zip(lists) {
                if let Elements::Items(held) = list.elements() {
                    *run = held;
                }
            }
            Continue(Runs::Of(None, runs))
        }
        _ => Continue(Runs::Not),
    }
}

/// The name of the next field of a record of the type `ty` of which
/// the fields `left` are left: they are the last of the type's.
#[inline]
fn next_name<'a>(ty: &'a TypeInfo, left: &[Value]) -> Option<&'a str> {
    let names = ty.fields();
    let (name, _) = names.get(names.len().wrapping_sub(left.len()))?;
    Some(name)
}

/// Go through `N` values side by side: their heads, then their parts
/// in order, each part a value gone through the same way before the
/// next. `Break` with the outcome of the step that stopped the walk.
#[inline]
fn walk<const N: usize, V: Visit<N>>(values: [&Value; N], visit: &mut V) -> ControlFlow<V::Stop> {
    visit.heads(values)?;
    match has_parts(values[0]) {
        true => through(values, visit, 0),
        false => Continue(()),
    }
}

/// How many levels of a value the walk goes down by calling itself,
/// with what it comes back to left in its frames: values are seldom
/// nested deeper. Below that it keeps what it comes back to in a stack
/// of its own ([`through_far`]), however deep the value goes.
const NEAR: usize = 16;

/// [`walk`] for values whose heads are visited and of one kind with
/// parts, `depth` levels down from where the walk began: go through
/// their parts.
fn through<const N: usize, V: Visit<N>>(
    values: [&Value; N],
    visit: &mut V,
    depth: usize,
) -> ControlFlow<V::Stop> {
    let (names, mut runs) = match runs(values, visit)? {
        Runs::Of(names, runs) => (names, runs),
        Runs::Done => return Continue(()),
        Runs::Not => return through_many(values, visit, depth),
    };
    loop {
        if V::NAMES
            && let Some(ty) = names
            && let Some(name) = next_name(ty, runs[0])
        {
            visit.name(name);
        }
        match first_of(&mut runs) {
            Among::All(next) => {
                visit.heads(next)?;
                if has_parts(next[0]) {
                    below(next, visit, depth)?;
                }
            }
            Among::None => return Continue(()),
            Among::Some(ended) => return visit.uneven(ended),
        }
    }
}

/// [`through`] for maps and for sets, whose parts are in no run.
fn through_many<const N: usize, V: Visit<N>>(
    values: [&Value; N],
    visit: &mut V,
    depth: usize,
) -> ControlFlow<V::Stop> {
    let (lens, mut parts) = Parts::of_many(values);
    if let Some(lens) = lens {
        visit.lens(lens)?;
    }
    loop {
        match parts.next() {
            Among::All(next) => {
                visit.heads(next)?;
                if has_parts(next[0]) {
                    below(next, visit, depth)?;
                }
            }
            Among::None => return Continue(()),
            Among::Some(ended) => return visit.uneven(ended),
        }
    }
}

/// Go through the parts of `values`, which are parts of values `depth`
/// levels down, with their heads visited and of one kind with parts:
/// by a call if that is near where the walk began, with a stack of
/// the walk's own if it is not.
#[inline]
fn below<const N: usize, V: Visit<N>>(
    values: [&Value; N],
    visit: &mut V,
    depth: usize,
) -> ControlFlow<V::Stop> {
    match depth < NEAR {
        true => through(values, visit, depth + 1),
        false => through_far(values, visit),
    }
}

/// The parts of `values`, whose heads are visited and of one kind with
/// parts, if the walk is to go through them; with the step of the
/// visitor that comes before the parts.
fn parts<'a, const N: usize, V: Visit<N>>(
    values: [&'a Value; N],
    visit: &mut V,
) -> ControlFlow<V::Stop, Option<Parts<'a, N>>> {
    Continue(match runs(values, visit)? {
        Runs::Of(Some(ty), runs) => Some(Parts::Fields(ty, runs)),
        Runs::Of(None, runs) => Some(Parts::Values(runs)),
        Runs::Done => None,
        Runs::Not => {
            let (lens, parts) = Parts::of_many(values);
            if let Some(lens) = lens {
                visit.lens(lens)?;
            }
            Some(parts)
        }
    })
}

/// [`through`] with a stack of the walk's own for what it comes back
/// to: for values nested deeper than a walk goes by calling itself.
fn through_far<'a, const N: usize, V: Visit<N>>(
    values: [&'a Value; N],
    visit: &mut V,
) -> ControlFlow<V::Stop> {
    let Some(mut current) = parts(values, visit)? else {
        return Continue(());
    };
    let mut waiting: Vec<Parts<'a, N>> = Vec::new();
    loop {
        if V::NAMES
            && let Some(name) = current.name()
        {
            visit.name(name);
        }
        match current.next() {
            Among::All(next) => {
                visit.heads(next)?;
                if !has_parts(next[0]) {
                    continue;
                }
                let Some(inner) = parts(next, visit)? else {
                    continue;
                };
                // The parts of a part come first. (What has no part
                // left need not be come back to: a chain of a million
                // values, each the last part of the one before, waits
                // for nothing.)
                let outer = std::mem::replace(&mut current, inner);
                if !outer.is_done() {
                    waiting.push(outer);
                }
            }
            Among::None => match waiting.pop() {
                Some(outer) => current = outer,
                None => return Continue(()),
            },
            Among::Some(ended) => return visit.uneven(ended),
        }
    }
}

// ── The three visitors ─────────────────────────────────────────────

/// The order of a list that holds the elements `items` and the list
/// of the Ints from `lo` to `hi`: element by element, and a list that
/// ends first is the lesser.
fn cmp_items_ints(items: &[Value], lo: i64, hi: i64) -> Ordering {
    for (item, n) in items.iter().zip(lo..=hi) {
        let ordering = item.cmp(&Value::Int(n));
        if ordering.is_ne() {
            return ordering;
        }
    }
    (items.len() as u64).cmp(&(hi.abs_diff(lo) + 1))
}

/// The order of two lists of which one at least holds no element;
/// `None` if both hold theirs.
fn cmp_without_elements(a: &List, b: &List) -> Option<Ordering> {
    match (a.elements(), b.elements()) {
        (Elements::Items(_), Elements::Items(_)) => None,
        // Of two runs of Ints the one that begins lower is the
        // lesser, and of two that begin alike the shorter.
        (Elements::Ints(a_lo, a_hi), Elements::Ints(b_lo, b_hi)) => {
            Some(a_lo.cmp(&b_lo).then(a_hi.cmp(&b_hi)))
        }
        (Elements::Items(items), Elements::Ints(lo, hi)) => Some(cmp_items_ints(items, lo, hi)),
        (Elements::Ints(lo, hi), Elements::Items(items)) => {
            Some(cmp_items_ints(items, lo, hi).reverse())
        }
    }
}

/// Whether two values are equal: the walk stops at the first step that
/// tells them apart.
struct Equal;

impl Visit<2> for Equal {
    type Stop = ();

    #[inline]
    fn heads(&mut self, [a, b]: [&Value; 2]) -> ControlFlow<()> {
        match cmp_heads(a, b).is_eq() {
            true => Continue(()),
            false => Break(()),
        }
    }

    fn lens(&mut self, [a, b]: [usize; 2]) -> ControlFlow<()> {
        match a == b {
            true => Continue(()),
            false => Break(()),
        }
    }

    /// Two lists are equal if they have the same elements, however
    /// each is stored.
    fn lists(&mut self, [a, b]: [&List; 2]) -> ControlFlow<(), bool> {
        match cmp_without_elements(a, b) {
            None if a.len() == b.len() => Continue(true),
            Some(Ordering::Equal) => Continue(false),
            _ => Break(()),
        }
    }

    fn uneven(&mut self, _: [bool; 2]) -> ControlFlow<()> {
        Break(())
    }
}

/// The order of two values: that of the first step at which they
/// differ. Of two values that are alike as far as the shorter goes
/// (two lists, two tuples), the shorter comes first.
struct Order;

impl Visit<2> for Order {
    type Stop = Ordering;

    #[inline]
    fn heads(&mut self, [a, b]: [&Value; 2]) -> ControlFlow<Ordering> {
        match cmp_heads(a, b) {
            Ordering::Equal => Continue(()),
            unequal => Break(unequal),
        }
    }

    fn lens(&mut self, _: [usize; 2]) -> ControlFlow<Ordering> {
        Continue(())
    }

    fn lists(&mut self, [a, b]: [&List; 2]) -> ControlFlow<Ordering, bool> {
        match cmp_without_elements(a, b) {
            None => Continue(true),
            Some(Ordering::Equal) => Continue(false),
            Some(unequal) => Break(unequal),
        }
    }

    fn uneven(&mut self, [a_ended, _]: [bool; 2]) -> ControlFlow<Ordering> {
        Break(match a_ended {
            true => Ordering::Less,
            false => Ordering::Greater,
        })
    }
}

/// The hash of a value: each head in the order of the walk, with the
/// number of parts of what may have any number, and the name of each
/// field of a record.
struct Hashed<'h, H>(&'h mut H);

impl<H: Hasher> Visit<1> for Hashed<'_, H> {
    type Stop = Infallible;

    const NAMES: bool = true;

    fn heads(&mut self, [value]: [&Value; 1]) -> ControlFlow<Infallible> {
        hash_head(value, self.0);
        Continue(())
    }

    fn lens(&mut self, [len]: [usize; 1]) -> ControlFlow<Infallible> {
        len.hash(self.0);
        Continue(())
    }

    /// Equal lists hash alike however each is stored, and a list that
    /// holds no element (`0..4000000000`) is not walked for its hash:
    /// a list of Ints that each are one more than the one before is
    /// hashed by its length and its first element, whether it holds
    /// them or not. (A list that holds them is read once for that, up
    /// to the first element that does not fit, and its hash is as
    /// much work again.)
    fn lists(&mut self, [list]: [&List; 1]) -> ControlFlow<Infallible, bool> {
        list.len().hash(self.0);
        match list.ascending_ints() {
            Some((lo, _)) => {
                self.0.write_u8(1);
                lo.hash(self.0);
                Continue(false)
            }
            None => {
                self.0.write_u8(0);
                Continue(true)
            }
        }
    }

    fn name(&mut self, name: &str) {
        name.hash(self.0);
    }

    /// (One value has a part left or has none.)
    fn uneven(&mut self, _: [bool; 1]) -> ControlFlow<Infallible> {
        Continue(())
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Value) -> bool {
        walk([self, other], &mut Equal).is_continue()
    }
}

impl Eq for Value {}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Value) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Value {
    fn cmp(&self, other: &Value) -> Ordering {
        match walk([self, other], &mut Order) {
            Break(unequal) => unequal,
            Continue(()) => Ordering::Equal,
        }
    }
}

impl Hash for Value {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let _ = walk([self], &mut Hashed(state));
    }
}
