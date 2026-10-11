//! Dropping a deep value.
//!
//! The last holder of a value drops what the value holds: the fields
//! of a variant or a record, the elements of a list, a tuple or a set,
//! the entries of a map, the values a closure has captured, the values
//! that wait in a channel. What is held and is the last holder of a
//! value drops that one's in turn: a list of a program's own, a
//! million links long, was dropped a million calls deep, and
//! overflowed the native stack. So the holders that are being dropped
//! on a thread, each inside the one before, are counted, and past a
//! few of them what a holder holds is put aside and dropped by the
//! outermost when it is done with its own.
//!
//! Every holder a program can nest values through without bound drops
//! what it holds through [`drop_held`]: an object ([`super::obj`]), the
//! entries of a map and the members of a set ([`Entries`],
//! [`Members`]), a closure (`bytecode::VmClosure`) and a channel
//! (`runtime::sync::Channel`). The elements that lists and tuples share
//! are counted where each of their holders is dropped ([`Buf`]).

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use super::Value;

/// How many holders drop what they hold each inside the one before;
/// what one further in holds is put aside.
const NEAR_DROPS: usize = 64;

thread_local! {
    /// How many holders are being dropped on this thread, each inside
    /// the one before.
    static DROPPING: Cell<usize> = const { Cell::new(0) };
    /// Whether anything is put aside.
    static ASIDE: Cell<bool> = const { Cell::new(false) };
    /// What is put aside.
    static PUT_ASIDE: RefCell<Vec<Held>> = const { RefCell::new(Vec::new()) };
}

/// What a holder holds, put aside.
pub(crate) enum Held {
    Values(#[allow(dead_code)] Box<[Value]>),
    Shared(#[allow(dead_code)] Arc<[Value]>),
    Entries(#[allow(dead_code)] BTreeMap<Value, Value>),
    Members(#[allow(dead_code)] BTreeSet<Value>),
}

impl From<Box<[Value]>> for Held {
    fn from(values: Box<[Value]>) -> Held {
        Held::Values(values)
    }
}

impl From<Vec<Value>> for Held {
    fn from(values: Vec<Value>) -> Held {
        Held::Values(values.into_boxed_slice())
    }
}

impl From<Arc<[Value]>> for Held {
    fn from(values: Arc<[Value]>) -> Held {
        Held::Shared(values)
    }
}

impl From<BTreeMap<Value, Value>> for Held {
    fn from(entries: BTreeMap<Value, Value>) -> Held {
        Held::Entries(entries)
    }
}

impl From<BTreeSet<Value>> for Held {
    fn from(members: BTreeSet<Value>) -> Held {
        Held::Members(members)
    }
}

/// A holder starts to drop what it holds. True if it is too far in to
/// do so where it is: it puts aside what it holds ([`put_aside`]).
/// Either way [`leave`] follows.
fn enter() -> bool {
    let depth = DROPPING.get();
    DROPPING.set(depth + 1);
    depth >= NEAR_DROPS
}

/// Leave `held` to the outermost holder that is being dropped.
fn put_aside(held: Held) {
    // (A thread that is ending has nothing to put it into, and drops
    // it where it is.)
    if PUT_ASIDE
        .try_with(move |aside| aside.borrow_mut().push(held))
        .is_ok()
    {
        ASIDE.set(true);
    }
}

/// A holder has dropped what it holds, or put it aside. The outermost
/// drops what was put aside, and what is put aside meanwhile.
fn leave() {
    let depth = DROPPING.get();
    while depth == 1 && ASIDE.get() {
        let next = PUT_ASIDE.try_with(|aside| aside.borrow_mut().pop());
        match next {
            Ok(Some(held)) => drop(held),
            _ => ASIDE.set(false),
        }
    }
    DROPPING.set(depth - 1);
}

/// Drop `held`, which a holder that is being dropped has taken out of
/// itself.
pub(crate) fn drop_held<T>(held: T)
where
    Held: From<T>,
{
    match enter() {
        true => put_aside(held.into()),
        false => drop(held),
    }
    leave();
}

/// The elements that lists, or tuples, share.
///
/// The last of those that share them drops them, and which is the last
/// is known only where the count of them is taken down: so each one
/// that is dropped is counted as a holder while its share is given up.
/// [`Drop::drop`] counts it, the share is given up after that (a field
/// is dropped after the `drop` of what it is a field of), and the
/// second field, which is dropped last, counts it out.
#[derive(Clone)]
pub(super) struct Buf {
    items: Arc<[Value]>,
    _leave: Leave,
}

/// Counts a [`Buf`] out when it is dropped.
#[derive(Clone)]
struct Leave;

impl Drop for Buf {
    fn drop(&mut self) {
        if enter() {
            put_aside(Held::Shared(std::mem::take(&mut self.items)));
        }
    }
}

impl Drop for Leave {
    fn drop(&mut self) {
        leave();
    }
}

impl Deref for Buf {
    type Target = [Value];

    fn deref(&self) -> &[Value] {
        &self.items
    }
}

impl From<Vec<Value>> for Buf {
    fn from(items: Vec<Value>) -> Buf {
        Buf {
            items: Arc::from(items),
            _leave: Leave,
        }
    }
}

impl FromIterator<Value> for Buf {
    fn from_iter<I: IntoIterator<Item = Value>>(items: I) -> Buf {
        Buf {
            items: items.into_iter().collect(),
            _leave: Leave,
        }
    }
}

/// The entries of a map: a `BTreeMap` whose keys and values are
/// dropped on no native stack however deep they nest.
#[derive(Clone, Default)]
pub struct Entries(BTreeMap<Value, Value>);

/// The members of a set: a `BTreeSet` whose members are dropped on no
/// native stack however deep they nest.
#[derive(Clone, Default)]
pub struct Members(BTreeSet<Value>);

impl Drop for Entries {
    fn drop(&mut self) {
        if !self.0.is_empty() {
            drop_held(std::mem::take(&mut self.0));
        }
    }
}

impl Drop for Members {
    fn drop(&mut self) {
        if !self.0.is_empty() {
            drop_held(std::mem::take(&mut self.0));
        }
    }
}

impl Entries {
    /// The map itself.
    pub fn into_inner(mut self) -> BTreeMap<Value, Value> {
        std::mem::take(&mut self.0)
    }
}

impl Members {
    /// The set itself.
    pub fn into_inner(mut self) -> BTreeSet<Value> {
        std::mem::take(&mut self.0)
    }
}

impl Deref for Entries {
    type Target = BTreeMap<Value, Value>;

    fn deref(&self) -> &BTreeMap<Value, Value> {
        &self.0
    }
}

impl DerefMut for Entries {
    fn deref_mut(&mut self) -> &mut BTreeMap<Value, Value> {
        &mut self.0
    }
}

impl Deref for Members {
    type Target = BTreeSet<Value>;

    fn deref(&self) -> &BTreeSet<Value> {
        &self.0
    }
}

impl DerefMut for Members {
    fn deref_mut(&mut self) -> &mut BTreeSet<Value> {
        &mut self.0
    }
}

impl From<BTreeMap<Value, Value>> for Entries {
    fn from(entries: BTreeMap<Value, Value>) -> Entries {
        Entries(entries)
    }
}

impl From<BTreeSet<Value>> for Members {
    fn from(members: BTreeSet<Value>) -> Members {
        Members(members)
    }
}
