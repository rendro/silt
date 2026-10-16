//! Dropping a deep value.
//!
//! The last holder of a value drops what the value holds: the fields
//! of a variant or a record, the elements of a list, a tuple or a set,
//! the entries of a map, the values a closure has captured, the values
//! that wait in a channel, the value a task ended with. What is held
//! and is the last holder of a value drops that one's in turn: a list
//! of a program's own, a million links long, was dropped a million
//! calls deep, and overflowed the native stack. So the holders that
//! are being dropped on a thread, each inside the one before, are
//! counted, and past a few of them what a holder holds is put aside
//! and dropped by the outermost when it is done with its own.
//!
//! Every holder a program can nest values through without bound is
//! counted. An object ([`super::obj`]), a closure
//! (`bytecode::VmClosure`), a channel (`runtime::sync::Channel`) and
//! the handle of a task that has ended (`runtime::handle::TaskHandle`)
//! take what they hold out of themselves and drop it through
//! [`drop_held`]. The elements that lists and tuples share ([`Buf`]),
//! the tree of a long list ([`Tree`]), the entries of a map and the
//! members of a set ([`Entries`], [`Members`]) stay where they are:
//! their holder is counted in where its `drop` starts, and counted out
//! by its last field.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Deref;
use std::sync::Arc;

use imbl::Vector;

use super::Value;

/// How many holders drop what they hold each inside the one before;
/// what one further in holds is put aside.
const NEAR_DROPS: usize = 64;

/// Set in [`DROPPING`] while anything is put aside.
const ASIDE: usize = 1 << (usize::BITS - 1);

thread_local! {
    /// How many holders are being dropped on this thread, each inside
    /// the one before; and [`ASIDE`].
    static DROPPING: Cell<usize> = const { Cell::new(0) };
    /// What is put aside.
    static PUT_ASIDE: RefCell<Vec<Held>> = const { RefCell::new(Vec::new()) };
}

/// What a holder holds, put aside: it is kept, and dropped.
#[allow(dead_code)]
pub(crate) enum Held {
    Value(Value),
    Values(Box<[Value]>),
    Shared(Arc<[Value]>),
    Tree(Arc<Vector<Value>>),
    Entries(BTreeMap<Value, Value>),
    Members(BTreeSet<Value>),
}

impl From<Value> for Held {
    fn from(value: Value) -> Held {
        Held::Value(value)
    }
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
#[inline]
fn enter() -> bool {
    let dropping = DROPPING.get();
    DROPPING.set(dropping + 1);
    dropping & !ASIDE >= NEAR_DROPS
}

/// Leave `held` to the outermost holder that is being dropped.
#[cold]
#[inline(never)]
fn put_aside(held: Held) {
    // (A thread that is ending has nothing to put it into, and drops
    // it where it is.)
    if PUT_ASIDE
        .try_with(move |aside| aside.borrow_mut().push(held))
        .is_ok()
    {
        DROPPING.set(DROPPING.get() | ASIDE);
    }
}

/// A holder has dropped what it holds, or put it aside. The outermost
/// drops what was put aside.
#[inline]
fn leave() {
    let dropping = DROPPING.get();
    if dropping == ASIDE | 1 {
        drop_aside();
    } else {
        DROPPING.set(dropping - 1);
    }
}

/// The outermost holder drops what was put aside, and what is put
/// aside meanwhile, and is done.
#[cold]
#[inline(never)]
fn drop_aside() {
    while let Ok(Some(held)) = PUT_ASIDE.try_with(|aside| aside.borrow_mut().pop()) {
        drop(held);
    }
    DROPPING.set(0);
}

/// Drop `held`, which a holder that is being dropped has taken out of
/// itself.
#[inline]
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

/// Counts out the holder it is the last field of, when that is dropped.
#[derive(Clone, Default)]
struct Leave;

impl Drop for Buf {
    #[inline]
    fn drop(&mut self) {
        if enter() {
            put_aside(Held::Shared(std::mem::take(&mut self.items)));
        }
    }
}

impl Drop for Leave {
    #[inline]
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

/// The elements of a long list, in a tree that lists share ([`Buf`]
/// holds those of a short one): the tree, and the parts of it that
/// later versions of the list share with it, are dropped on no native
/// stack however deep the elements nest.
///
/// (As for a `Buf`: the tree is dropped after `drop`, and the second
/// field counts its holder out.)
#[derive(Clone)]
pub(super) struct Tree {
    tree: Arc<Vector<Value>>,
    _leave: Leave,
}

impl Drop for Tree {
    #[inline]
    fn drop(&mut self) {
        if enter() {
            put_aside(Held::Tree(std::mem::take(&mut self.tree)));
        }
    }
}

impl Deref for Tree {
    type Target = Vector<Value>;

    fn deref(&self) -> &Vector<Value> {
        &self.tree
    }
}

impl From<Vector<Value>> for Tree {
    fn from(tree: Vector<Value>) -> Tree {
        Tree {
            tree: Arc::new(tree),
            _leave: Leave,
        }
    }
}

/// The entries of a map: a `BTreeMap` whose keys and values are
/// dropped on no native stack however deep they nest.
#[derive(Default)]
pub(super) struct Entries {
    entries: BTreeMap<Value, Value>,
    _leave: Leave,
}

/// The members of a set: a `BTreeSet` whose members are dropped on no
/// native stack however deep they nest.
#[derive(Default)]
pub(super) struct Members {
    members: BTreeSet<Value>,
    _leave: Leave,
}

// (As for a `Buf`: the map, or the set, is dropped after `drop`, and
// the second field counts its holder out.)
impl Drop for Entries {
    #[inline]
    fn drop(&mut self) {
        if enter() {
            put_aside(Held::Entries(std::mem::take(&mut self.entries)));
        }
    }
}

impl Drop for Members {
    #[inline]
    fn drop(&mut self) {
        if enter() {
            put_aside(Held::Members(std::mem::take(&mut self.members)));
        }
    }
}

impl Deref for Entries {
    type Target = BTreeMap<Value, Value>;

    fn deref(&self) -> &BTreeMap<Value, Value> {
        &self.entries
    }
}

impl Deref for Members {
    type Target = BTreeSet<Value>;

    fn deref(&self) -> &BTreeSet<Value> {
        &self.members
    }
}

impl From<BTreeMap<Value, Value>> for Entries {
    fn from(entries: BTreeMap<Value, Value>) -> Entries {
        Entries {
            entries,
            _leave: Leave,
        }
    }
}

impl From<BTreeSet<Value>> for Members {
    fn from(members: BTreeSet<Value>) -> Members {
        Members {
            members,
            _leave: Leave,
        }
    }
}
