//! The key of a value: whether two values are equal, which of two
//! comes first, and a value's hash.
//!
//! All three read a value the same way: as its head ([`Head`]: its
//! kind, and what tells it from another value of the kind before its
//! parts do) and its parts in order ([`Parts`]: the values it is made
//! of). [`walk`] goes through one value, or through two side by side,
//! part by part, and keeps what it has yet to visit in a stack of its
//! own: a value nested a million levels deep is compared and hashed
//! like any other, and the native stack does not bound it. [`Equal`],
//! [`Order`] and [`Hashed`] say what is done at each step.
//!
//! The kinds of value are ranked in one place ([`Head::rank`]): the
//! rank orders two values of different kinds, and is the first byte of
//! a value's hash.
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
use super::{Float, List, Value};
use crate::builtins::registry::BuiltinId;
use crate::defs::TypeId;
use crate::typeinfo::{FieldType, Tag, TypeInfo};

// ── A value's head and its parts ───────────────────────────────────

/// What a value is before its parts: its kind, and what tells it from
/// another value of the kind (all of the value, if it has no parts).
#[derive(Clone, Copy, PartialEq)]
enum Head<'a> {
    Unit,
    Bool(bool),
    Int(i64),
    Float(Float),
    Str(&'a str),
    List,
    Tuple,
    Map,
    Set,
    Record(RecordType<'a>),
    /// A variant: its type and the position of its declaration there.
    Variant(TypeId, u16),
    /// A channel, a task handle, a listener, a stream: which one.
    Channel(usize),
    Handle(usize),
    Bytes(&'a [u8]),
    TcpListener(usize),
    TcpStream(usize),
    /// A closure: where it is. (Two closures of one function with
    /// different captures are two.)
    Closure(usize),
    Builtin(BuiltinId),
    Constructor(&'a Tag),
    Type(TypeId),
    Primitive(&'static str),
    /// A host function: its name.
    HostFn(&'a str),
}

impl<'a> Head<'a> {
    fn of(value: &'a Value) -> Head<'a> {
        match value {
            Value::Unit => Head::Unit,
            Value::Bool(b) => Head::Bool(*b),
            Value::Int(n) => Head::Int(*n),
            Value::Float(f) => Head::Float(*f),
            Value::String(s) => Head::Str(s),
            Value::List(_) => Head::List,
            Value::Tuple(_) => Head::Tuple,
            Value::Map(_) => Head::Map,
            Value::Set(_) => Head::Set,
            Value::Record(record) => Head::Record(RecordType(record.ty())),
            Value::Variant(variant) => Head::Variant(variant.type_id(), variant.ordinal()),
            Value::Channel(channel) => Head::Channel(channel.id()),
            Value::Handle(handle) => Head::Handle(handle.id),
            Value::Bytes(bytes) => Head::Bytes(bytes),
            Value::TcpListener(listener) => Head::TcpListener(listener.id),
            Value::TcpStream(stream) => Head::TcpStream(stream.id),
            Value::VmClosure(closure) => Head::Closure(Arc::as_ptr(closure) as usize),
            Value::BuiltinFn(id) => Head::Builtin(*id),
            Value::VariantConstructor(tag) => Head::Constructor(tag),
            Value::TypeDescriptor(ty) => Head::Type(ty.id),
            Value::PrimitiveDescriptor(name) => Head::Primitive(name),
            Value::HostFn(host) => Head::HostFn(&host.name),
        }
    }

    /// The rank of the value's kind: the one table of the kinds. A
    /// value of a kind of lower rank comes before one of a higher, and
    /// the rank is the first byte of a value's hash.
    fn rank(&self) -> u8 {
        match self {
            Head::Unit => 0,
            Head::Bool(_) => 1,
            Head::Int(_) => 2,
            Head::Float(_) => 3,
            Head::Str(_) => 4,
            Head::List => 5,
            Head::Tuple => 6,
            Head::Map => 7,
            Head::Set => 8,
            Head::Record(_) => 9,
            Head::Variant(..) => 10,
            Head::Channel(_) => 11,
            Head::Handle(_) => 12,
            Head::Bytes(_) => 13,
            Head::TcpListener(_) => 14,
            Head::TcpStream(_) => 15,
            Head::Closure(_) => 16,
            Head::Builtin(_) => 17,
            Head::Constructor(_) => 18,
            Head::Type(_) => 19,
            Head::Primitive(_) => 20,
            Head::HostFn(_) => 21,
        }
    }

    /// The order of two heads: of one kind by what they hold (a kind
    /// that holds nothing has one head), of two kinds by rank.
    fn cmp(&self, other: &Head<'_>) -> Ordering {
        match (self, other) {
            (Head::Bool(a), Head::Bool(b)) => a.cmp(b),
            (Head::Int(a), Head::Int(b)) => a.cmp(b),
            (Head::Float(a), Head::Float(b)) => a.cmp(b),
            (Head::Str(a), Head::Str(b)) => a.cmp(b),
            (Head::Record(a), Head::Record(b)) => a.cmp(*b),
            // Variants of one type order by declaration; variants of
            // two types (which a program cannot compare) by the types.
            (Head::Variant(a, at), Head::Variant(b, bt)) => (a, at).cmp(&(b, bt)),
            (Head::Channel(a), Head::Channel(b))
            | (Head::Handle(a), Head::Handle(b))
            | (Head::TcpListener(a), Head::TcpListener(b))
            | (Head::TcpStream(a), Head::TcpStream(b))
            | (Head::Closure(a), Head::Closure(b)) => a.cmp(b),
            (Head::Bytes(a), Head::Bytes(b)) => a.cmp(b),
            (Head::Builtin(a), Head::Builtin(b)) => a.cmp(b),
            (Head::Constructor(a), Head::Constructor(b)) => a.cmp(b),
            (Head::Type(a), Head::Type(b)) => a.cmp(b),
            (Head::Primitive(a), Head::Primitive(b)) | (Head::HostFn(a), Head::HostFn(b)) => {
                a.cmp(b)
            }
            _ => self.rank().cmp(&other.rank()),
        }
    }

    /// The head's part of a hash: its rank, then what it holds.
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u8(self.rank());
        match *self {
            Head::Unit | Head::List | Head::Tuple | Head::Map | Head::Set => {}
            // (A record's fields are hashed with their names: its
            // type adds nothing to them.)
            Head::Record(_) => {}
            // (No function has a hash: the VM refuses a value that
            // holds one before it asks.)
            Head::Closure(_) => {}
            Head::Bool(b) => b.hash(state),
            Head::Int(n) => n.hash(state),
            Head::Float(f) => f.hash(state),
            Head::Str(s) | Head::Primitive(s) | Head::HostFn(s) => s.hash(state),
            Head::Variant(ty, ordinal) => {
                ty.hash(state);
                ordinal.hash(state);
            }
            Head::Channel(id)
            | Head::Handle(id)
            | Head::TcpListener(id)
            | Head::TcpStream(id) => id.hash(state),
            Head::Bytes(bytes) => {
                bytes.len().hash(state);
                state.write(bytes);
            }
            Head::Builtin(id) => id.hash(state),
            Head::Constructor(tag) => tag.hash(state),
            Head::Type(id) => id.hash(state),
        }
    }
}

/// A record's type as the record's key has it: the type's id, and for
/// an anonymous record, whose types share one id, its field names.
#[derive(Clone, Copy)]
struct RecordType<'a>(&'a TypeInfo);

impl<'a> RecordType<'a> {
    fn names(self) -> impl Iterator<Item = &'a str> {
        self.0.fields().iter().map(|(name, _)| &**name)
    }

    fn cmp(self, other: RecordType<'_>) -> Ordering {
        if std::ptr::eq(self.0, other.0) {
            return Ordering::Equal;
        }
        self.0.id.cmp(&other.0.id).then_with(|| match self.0.is_anon() {
            true => self.names().cmp(other.names()),
            false => Ordering::Equal,
        })
    }
}

impl PartialEq for RecordType<'_> {
    fn eq(&self, other: &RecordType<'_>) -> bool {
        self.cmp(*other).is_eq()
    }
}

/// The parts of a value yet to be visited, in order: the values it is
/// made of.
enum Parts<'a> {
    /// No parts: the value is its head.
    None,
    /// The items of a tuple, the fields of a variant, the elements a
    /// list holds.
    Values(std::slice::Iter<'a, Value>),
    /// The fields of a record, in the order its type declares them,
    /// each with its name.
    Fields(
        std::slice::Iter<'a, (String, FieldType)>,
        std::slice::Iter<'a, Value>,
    ),
    /// The entries of a map in key order, a key and then its value:
    /// the entries left, and the value of the key last given.
    Entries(btree_map::Iter<'a, Value, Value>, Option<&'a Value>),
    /// The members of a set, in order.
    Members(btree_set::Iter<'a, Value>),
}

/// A part of a value: the value, and its name if it is a record's
/// field.
type Part<'a> = (Option<&'a str>, &'a Value);

impl<'a> Parts<'a> {
    /// The parts of `value` that is no list, and their number if a
    /// value of its kind may have any number of them.
    fn of(value: &'a Value) -> (Option<usize>, Parts<'a>) {
        match value {
            Value::Tuple(items) => (Some(items.len()), Parts::Values(items.iter())),
            Value::Variant(variant) => {
                let fields = variant.fields();
                (Some(fields.len()), Parts::Values(fields.iter()))
            }
            Value::Record(record) => {
                let names = record.ty().fields().iter();
                (None, Parts::Fields(names, record.fields().iter()))
            }
            Value::Map(entries) => (Some(entries.len()), Parts::Entries(entries.iter(), None)),
            Value::Set(members) => (Some(members.len()), Parts::Members(members.iter())),
            _ => (None, Parts::None),
        }
    }

    fn next(&mut self) -> Option<Part<'a>> {
        match self {
            Parts::None => None,
            Parts::Values(values) => values.next().map(|value| (None, value)),
            Parts::Fields(names, values) => {
                let value = values.next()?;
                Some((names.next().map(|(name, _)| &**name), value))
            }
            Parts::Entries(entries, value) => match value.take() {
                Some(value) => Some((None, value)),
                None => {
                    let (key, of_key) = entries.next()?;
                    *value = Some(of_key);
                    Some((None, key))
                }
            },
            Parts::Members(members) => members.next().map(|member| (None, member)),
        }
    }

    /// Whether no part is left.
    fn is_done(&self) -> bool {
        match self {
            Parts::None => true,
            Parts::Values(values) | Parts::Fields(_, values) => values.len() == 0,
            Parts::Entries(entries, value) => value.is_none() && entries.len() == 0,
            Parts::Members(members) => members.len() == 0,
        }
    }
}

// ── The walk ───────────────────────────────────────────────────────

/// What is done at each step of a walk through `N` values side by
/// side. A step that stops the walk gives its outcome.
trait Visit<const N: usize> {
    type Stop;

    /// The heads of `N` values, side by side. The walk goes on to
    /// their parts only if they are of one kind.
    fn heads(&mut self, heads: [Head<'_>; N]) -> ControlFlow<Self::Stop>;

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

/// Visit the head of each of `values`, and give their parts if the
/// walk is to go through them.
fn enter<'a, const N: usize, V: Visit<N>>(
    values: [&'a Value; N],
    visit: &mut V,
) -> ControlFlow<V::Stop, [Parts<'a>; N]> {
    visit.heads(values.map(Head::of))?;
    // (The values are of one kind from here on: the visitor went on.
    // One that is not is read as the first, and no harm is done.)
    if let Value::List(first) = values[0] {
        let lists = values.map(|value| match value {
            Value::List(list) => list,
            _ => first,
        });
        let through = visit.lists(lists)?;
        return Continue(lists.map(|list| match list.elements() {
            Elements::Items(items) if through => Parts::Values(items.iter()),
            _ => Parts::None,
        }));
    }
    let parts = values.map(Parts::of);
    if let Among::All(lens) = among(parts.each_ref().map(|(len, _)| *len)) {
        visit.lens(lens)?;
    }
    Continue(parts.map(|(_, parts)| parts))
}

/// `N` things that are there or not, taken together.
enum Among<T, const N: usize> {
    All([T; N]),
    None,
    /// Some are there: which are not.
    Some([bool; N]),
}

fn among<T: Copy, const N: usize>(items: [Option<T>; N]) -> Among<T, N> {
    let missing = items.map(|item| item.is_none());
    match items.iter().flatten().next() {
        None => Among::None,
        Some(_) if missing.contains(&true) => Among::Some(missing),
        Some(any) => Among::All(items.map(|item| item.unwrap_or(*any))),
    }
}

/// The parts the walk has yet to come back to: a few of them in
/// place, so that a value nested a few levels deep is walked without
/// asking for memory.
struct Waiting<T> {
    near: [Option<T>; 4],
    len: usize,
    far: Vec<T>,
}

impl<T> Waiting<T> {
    fn new() -> Waiting<T> {
        Waiting {
            near: [const { None }; 4],
            len: 0,
            far: Vec::new(),
        }
    }

    fn push(&mut self, item: T) {
        match self.near.get_mut(self.len) {
            Some(slot) => *slot = Some(item),
            None => self.far.push(item),
        }
        self.len += 1;
    }

    fn pop(&mut self) -> Option<T> {
        self.len = self.len.checked_sub(1)?;
        match self.near.get_mut(self.len) {
            Some(slot) => slot.take(),
            None => self.far.pop(),
        }
    }
}

/// Go through `N` values side by side: their heads, then their parts
/// in order, each part a value gone through the same way before the
/// next. `Break` with the outcome of the step that stopped the walk.
fn walk<'a, const N: usize, V: Visit<N>>(
    values: [&'a Value; N],
    visit: &mut V,
) -> ControlFlow<V::Stop> {
    let mut parts = enter(values, visit)?;
    let mut waiting: Waiting<[Parts<'a>; N]> = Waiting::new();
    loop {
        match among(parts.each_mut().map(Parts::next)) {
            Among::All(next) => {
                if let Some(name) = next[0].0 {
                    visit.name(name);
                }
                let inner = enter(next.map(|(_, value)| value), visit)?;
                if inner.iter().all(Parts::is_done) {
                    continue;
                }
                // The parts of a part come first. (What has no part
                // left need not be come back to: a chain of a million
                // values, each the last part of the one before, waits
                // for nothing.)
                let outer = std::mem::replace(&mut parts, inner);
                if !outer.iter().all(Parts::is_done) {
                    waiting.push(outer);
                }
            }
            Among::None => match waiting.pop() {
                Some(outer) => parts = outer,
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

    fn heads(&mut self, [a, b]: [Head<'_>; 2]) -> ControlFlow<()> {
        match a == b {
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

    fn heads(&mut self, [a, b]: [Head<'_>; 2]) -> ControlFlow<Ordering> {
        match a.cmp(&b) {
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

    fn heads(&mut self, [head]: [Head<'_>; 1]) -> ControlFlow<Infallible> {
        head.hash(self.0);
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
