//! The map value and the set value.
//!
//! A map is its entries, each a key and its value, in the order of the
//! keys; a set is its members, in their order. Neither is ever changed:
//! an update is a new map, or set, and the one it was made from stays
//! as it was. A clone is a count.
//!
//! A small map keeps its entries side by side in key order, and finds
//! a key by halving; a large one keeps them in a tree (the B-tree of
//! the crate `imbl`), and a map made from it by one change shares all
//! of the tree but a path from its root with it: the change costs
//! steps that grow with the logarithm of the size, not with the size
//! ([`FLAT_MOST`]). A set is kept in the same two ways.
//!
//! How a map and a set are stored is this file's own. The rest of silt
//! has their sizes, their lookups, their entries and members in order
//! ([`Map::iter`], [`Set::iter`]) and the updates of the stdlib
//! ([`Map::insert`], [`Set::union`], ...), and nothing that tells one
//! way of storing them from another.

use std::cmp::Ordering;
use std::iter::Peekable;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering as Atomic};

use imbl::shared_ptr::DefaultSharedPtr;
use imbl::{OrdMap, OrdSet};

use super::Value;
use super::deep::Kept;

/// The most entries a map, or members a set, keeps side by side; a
/// larger one keeps them in a tree. One made from a tree goes back to
/// side by side only when it has half as many, so that a map that
/// grows and shrinks around the size does not cross it each time.
///
/// A change to a map kept side by side copies its entries: at most
/// this many, a cost that does not grow with the map.
///
/// Measured 2026-10-17 through the VM (one release build with 8, 16
/// and 32, interleaved with main, median of three): map and set
/// literals of 5 and 20 entries, `map.get`, `map.set` and `set.insert`
/// on maps and sets of 5 to 20, JSON objects of 5 and 20 keys, and
/// maps of 1,000 built by `map.set` took the same time with each,
/// within the run's spread of 9%. Memory decided (peak RSS per map of
/// 100,000 built by `map.set`): with 8 or 16 a map of 20 entries is a
/// tree and takes 3,745 or 3,827 B, more than the 3,792 B a std
/// BTreeMap took; with 32 it takes 1,696 B, and one of 32 2,656 B.
pub(super) const FLAT_MOST: usize = 32;

/// A map.
#[derive(Clone, Default)]
pub struct Map(Arc<Kept<Entries>>);

/// A set.
#[derive(Clone, Default)]
pub struct Set(Arc<SetOf>);

/// What a set holds: its members, and whether one of them is, or has
/// in it, a function, once that is known.
#[derive(Default)]
struct SetOf {
    members: Kept<Members>,
    fns: AtomicU8,
}

/// Whether a set holds a function ([`Set::holds_fn`]): not known yet,
/// no, yes. (What [`SetOf::fns`] holds.)
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fns {
    Unknown = 0,
    No = 1,
    Yes = 2,
}

impl Fns {
    /// Of a set that holds the members of two sets of these answers.
    fn either(self, other: Fns) -> Fns {
        match (self, other) {
            (Fns::Yes, _) | (_, Fns::Yes) => Fns::Yes,
            (Fns::No, Fns::No) => Fns::No,
            _ => Fns::Unknown,
        }
    }

    /// Of a set that holds some of the members of a set of this answer.
    fn some_of(self) -> Fns {
        match self {
            Fns::No => Fns::No,
            _ => Fns::Unknown,
        }
    }
}

/// The entries of a map as they are kept.
pub(crate) enum Entries {
    /// Side by side, in key order: at most [`FLAT_MOST`].
    Flat(Box<[(Value, Value)]>),
    /// In a tree: more than half of [`FLAT_MOST`].
    Tree(OrdMap<Value, Value>),
}

/// The members of a set as they are kept.
pub(crate) enum Members {
    /// Side by side, in order: at most [`FLAT_MOST`].
    Flat(Box<[Value]>),
    /// In a tree: more than half of [`FLAT_MOST`].
    Tree(OrdSet<Value>),
}

impl Default for Entries {
    fn default() -> Entries {
        Entries::Flat(Box::default())
    }
}

impl Default for Members {
    fn default() -> Members {
        Members::Flat(Box::default())
    }
}

/// Where `key` is among `entries`, or where it would go.
#[inline]
fn find(entries: &[(Value, Value)], key: &Value) -> Result<usize, usize> {
    entries.binary_search_by(|(other, _)| other.cmp(key))
}

/// Where `value` is among `members`, or where it would go.
#[inline]
fn find_member(members: &[Value], value: &Value) -> Result<usize, usize> {
    members.binary_search_by(|other| other.cmp(value))
}

/// `items` with `item` at `at`, side by side.
fn with_at<T: Clone>(items: &[T], at: usize, item: T) -> Vec<T> {
    let mut made = Vec::with_capacity(items.len() + 1);
    made.extend_from_slice(&items[..at]);
    made.push(item);
    made.extend_from_slice(&items[at..]);
    copied(made.len());
    made
}

/// `items` without the one at `at`, side by side.
fn without_at<T: Clone>(items: &[T], at: usize) -> Vec<T> {
    let mut made = Vec::with_capacity(items.len() - 1);
    made.extend_from_slice(&items[..at]);
    made.extend_from_slice(&items[at + 1..]);
    copied(made.len());
    made
}

impl Map {
    /// The map of no entries.
    pub fn new() -> Map {
        Map::default()
    }

    fn of(entries: Entries) -> Map {
        Map(Arc::new(Kept::from(entries)))
    }

    /// The map of `entries`, which are in key order, each key once.
    fn of_sorted(entries: Vec<(Value, Value)>) -> Map {
        copied(entries.len());
        match entries.len() <= FLAT_MOST {
            true => Map::of(Entries::Flat(entries.into_boxed_slice())),
            false => Map::of(Entries::Tree(OrdMap::from_iter(entries))),
        }
    }

    /// The map of the entries of `tree`, kept side by side if it has
    /// shrunk to half of [`FLAT_MOST`].
    fn of_tree(tree: OrdMap<Value, Value>) -> Map {
        if tree.len() > FLAT_MOST / 2 {
            return Map::of(Entries::Tree(tree));
        }
        copied(tree.len());
        let entries = tree.iter().map(|(key, value)| (key.clone(), value.clone()));
        Map::of(Entries::Flat(entries.collect()))
    }

    fn entries(&self) -> &Entries {
        &self.0
    }

    pub fn len(&self) -> usize {
        match self.entries() {
            Entries::Flat(entries) => entries.len(),
            Entries::Tree(tree) => tree.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The map of a literal: `items` are its keys and values, each key
    /// before its value; of a key given twice, the value given last.
    /// (Read in place: built through an iterator that moves each pair,
    /// a loop of map literals took 15% longer.)
    pub(crate) fn of_literal(items: &[Value]) -> Map {
        let (pairs, _) = items.as_chunks::<2>();
        let entries = pairs
            .iter()
            .map(|[key, value]| (key.clone(), value.clone()));
        Map::of_unsorted(entries.collect())
    }

    /// The map of `entries`, in any order: of a key given twice, the
    /// value given last.
    fn of_unsorted(mut entries: Vec<(Value, Value)>) -> Map {
        // (A stable sort keeps the entries of one key in the order they
        // were given, and the last of them stays.)
        entries.sort_by(|(a, _), (b, _)| a.cmp(b));
        entries.dedup_by(|later, earlier| match later.0.cmp(&earlier.0) {
            Ordering::Equal => {
                std::mem::swap(later, earlier);
                true
            }
            _ => false,
        });
        Map::of_sorted(entries)
    }

    /// The value of `key`.
    pub fn get(&self, key: &Value) -> Option<&Value> {
        match self.entries() {
            Entries::Flat(entries) => find(entries, key).ok().map(|at| &entries[at].1),
            Entries::Tree(tree) => tree.get(key),
        }
    }

    pub fn contains_key(&self, key: &Value) -> bool {
        self.get(key).is_some()
    }

    /// The entries, each a key and its value, in the order of the keys.
    pub fn iter(&self) -> MapIter<'_> {
        MapIter(match self.entries() {
            Entries::Flat(entries) => MapIterOf::Flat(entries.iter()),
            Entries::Tree(tree) => MapIterOf::Tree(tree.iter()),
        })
    }

    /// The keys in order.
    pub fn keys(&self) -> impl Iterator<Item = &Value> {
        self.iter().map(|(key, _)| key)
    }

    /// The values, in the order of their keys.
    pub fn values(&self) -> impl Iterator<Item = &Value> {
        self.iter().map(|(_, value)| value)
    }

    /// The map with `key` of the value `value`, whether it had the key
    /// or not.
    pub fn insert(&self, key: Value, value: Value) -> Map {
        match self.entries() {
            Entries::Flat(entries) => match find(entries, &key) {
                Ok(at) => {
                    let mut made = entries.to_vec();
                    copied(made.len());
                    made[at].1 = value;
                    Map::of(Entries::Flat(made.into_boxed_slice()))
                }
                Err(at) if entries.len() < FLAT_MOST => {
                    let made = with_at(entries, at, (key, value));
                    Map::of(Entries::Flat(made.into_boxed_slice()))
                }
                Err(_) => {
                    copied(entries.len() + 1);
                    let mut tree = OrdMap::from_iter(entries.iter().cloned());
                    tree.insert(key, value);
                    Map::of(Entries::Tree(tree))
                }
            },
            Entries::Tree(tree) => {
                let mut tree = tree.clone();
                copied(1);
                tree.insert(key, value);
                Map::of(Entries::Tree(tree))
            }
        }
    }

    /// The map without `key`.
    pub fn remove(&self, key: &Value) -> Map {
        match self.entries() {
            Entries::Flat(entries) => match find(entries, key) {
                Ok(at) => Map::of(Entries::Flat(without_at(entries, at).into_boxed_slice())),
                Err(_) => self.clone(),
            },
            Entries::Tree(tree) => match tree.contains_key(key) {
                true => {
                    let mut tree = tree.clone();
                    copied(1);
                    tree.remove(key);
                    Map::of_tree(tree)
                }
                false => self.clone(),
            },
        }
    }

    /// The entries of the map and those of `other`: of a key both have,
    /// the value `other` has.
    ///
    /// The entries of the smaller map go into the larger one: the cost
    /// grows with the smaller one's size times the logarithm of the
    /// larger one's.
    pub fn merge(&self, other: &Map) -> Map {
        if other.is_empty() {
            return self.clone();
        }
        if self.is_empty() {
            return other.clone();
        }
        let (larger, smaller, smaller_wins) = match self.len() >= other.len() {
            true => (self, other, true),
            false => (other, self, false),
        };
        let Entries::Tree(tree) = larger.entries() else {
            // Both are small: side by side, each key once.
            let mut made = Vec::with_capacity(self.len() + other.len());
            side_by_side(
                self.iter(),
                other.iter(),
                |(key, _)| key,
                |ours, theirs| {
                    let (key, value) = match (ours, theirs) {
                        (Some((key, _)), Some((_, value))) => (key, value),
                        (Some(entry), None) | (None, Some(entry)) => entry,
                        (None, None) => return,
                    };
                    made.push((key.clone(), value.clone()));
                },
            );
            return Map::of_sorted(made);
        };
        let mut tree = tree.clone();
        for (key, value) in smaller.iter() {
            if smaller_wins || !tree.contains_key(key) {
                copied(1);
                tree.insert(key.clone(), value.clone());
            }
        }
        Map::of(Entries::Tree(tree))
    }
}

/// Walk two sequences in the order of `key` side by side, each with no
/// key twice: `each` gets an item of the first with the item of the
/// second of its key, if any, and an item of either that the other has
/// no item of the key of, in the order of the keys.
fn side_by_side<A, I, J>(
    first: I,
    second: J,
    key: impl Fn(&A) -> &Value,
    mut each: impl FnMut(Option<A>, Option<A>),
) where
    I: Iterator<Item = A>,
    J: Iterator<Item = A>,
{
    let (mut first, mut second): (Peekable<I>, Peekable<J>) = (first.peekable(), second.peekable());
    loop {
        let order = match (first.peek(), second.peek()) {
            (Some(a), Some(b)) => key(a).cmp(key(b)),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => return,
        };
        match order {
            Ordering::Less => each(first.next(), None),
            Ordering::Greater => each(None, second.next()),
            Ordering::Equal => each(first.next(), second.next()),
        }
    }
}

/// The map of the entries: of a key given twice, the value given last.
impl FromIterator<(Value, Value)> for Map {
    fn from_iter<I: IntoIterator<Item = (Value, Value)>>(entries: I) -> Map {
        Map::of_unsorted(entries.into_iter().collect())
    }
}

/// The entries of a map in the order of their keys ([`Map::iter`]).
pub struct MapIter<'a>(MapIterOf<'a>);

enum MapIterOf<'a> {
    Flat(std::slice::Iter<'a, (Value, Value)>),
    Tree(imbl::ordmap::Iter<'a, Value, Value, DefaultSharedPtr>),
}

impl<'a> Iterator for MapIter<'a> {
    type Item = (&'a Value, &'a Value);

    #[inline]
    fn next(&mut self) -> Option<(&'a Value, &'a Value)> {
        match &mut self.0 {
            MapIterOf::Flat(entries) => entries.next().map(|(key, value)| (key, value)),
            MapIterOf::Tree(entries) => entries.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match &self.0 {
            MapIterOf::Flat(entries) => entries.size_hint(),
            MapIterOf::Tree(entries) => entries.size_hint(),
        }
    }
}

impl ExactSizeIterator for MapIter<'_> {}

/// The entries of a map in the order of their keys, for as long as the
/// iterator is kept: of a map kept side by side, each in turn; of one
/// kept in a tree, the parts of the tree in turn, each copied when the
/// iterator comes to it. Nothing is copied when the iterator is made.
pub struct MapIntoIter(MapIntoIterOf);

enum MapIntoIterOf {
    Flat { map: Map, at: usize },
    Tree(imbl::ordmap::ConsumingIter<Value, Value, DefaultSharedPtr>),
}

impl Iterator for MapIntoIter {
    type Item = (Value, Value);

    fn next(&mut self) -> Option<(Value, Value)> {
        match &mut self.0 {
            MapIntoIterOf::Flat { map, at } => {
                let Entries::Flat(entries) = map.entries() else {
                    return None;
                };
                let entry = entries.get(*at)?.clone();
                *at += 1;
                Some(entry)
            }
            MapIntoIterOf::Tree(entries) => entries.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match &self.0 {
            MapIntoIterOf::Flat { map, at } => {
                let left = map.len() - at;
                (left, Some(left))
            }
            MapIntoIterOf::Tree(entries) => entries.size_hint(),
        }
    }
}

impl IntoIterator for Map {
    type Item = (Value, Value);
    type IntoIter = MapIntoIter;

    fn into_iter(self) -> MapIntoIter {
        MapIntoIter(match self.entries() {
            Entries::Flat(_) => MapIntoIterOf::Flat { map: self, at: 0 },
            Entries::Tree(tree) => MapIntoIterOf::Tree(tree.clone().into_iter()),
        })
    }
}

impl Set {
    /// The set of no members.
    pub fn new() -> Set {
        Set::default()
    }

    fn of(members: Members) -> Set {
        Set(Arc::new(SetOf {
            members: Kept::from(members),
            fns: AtomicU8::new(Fns::Unknown as u8),
        }))
    }

    /// The set, whose answer to whether it holds a function is `fns`
    /// if that is known.
    fn knowing(self, fns: Fns) -> Set {
        if fns != Fns::Unknown {
            self.0.fns.store(fns as u8, Atomic::Relaxed);
        }
        self
    }

    /// Whether the set holds a function, if that is known.
    fn fns(&self) -> Fns {
        match self.0.fns.load(Atomic::Relaxed) {
            1 => Fns::No,
            2 => Fns::Yes,
            _ => Fns::Unknown,
        }
    }

    /// Whether a member of the set is, or has in it, a function
    /// ([`Value::contains_fn`]): the gate of the operations on two sets.
    ///
    /// The answer is kept with the set once it is known, and the sets
    /// made from it by an update carry it where it follows from theirs
    /// (an update of a set that holds none, by a member that is no
    /// function, holds none): a fold of `set.union` walks each set once,
    /// not the one it grows at each step.
    pub fn holds_fn(&self) -> bool {
        match self.fns() {
            Fns::Yes => true,
            Fns::No => false,
            Fns::Unknown => {
                walked(self.len());
                let holds = self.iter().any(Value::contains_fn);
                let _ = self.clone().knowing(match holds {
                    true => Fns::Yes,
                    false => Fns::No,
                });
                holds
            }
        }
    }

    /// The set of `members`, none of which is, or has in it, a
    /// function: a gate has walked them ([`Set::holds_fn`]).
    pub fn without_fn(members: impl IntoIterator<Item = Value>) -> Set {
        members.into_iter().collect::<Set>().knowing(Fns::No)
    }

    /// [`Set::holds_fn`] if it is known, without a walk.
    pub(crate) fn holds_fn_known(&self) -> Option<bool> {
        match self.fns() {
            Fns::Yes => Some(true),
            Fns::No => Some(false),
            Fns::Unknown => None,
        }
    }

    /// The set of `members`, which are in order, each once.
    fn of_sorted(members: Vec<Value>) -> Set {
        copied(members.len());
        match members.len() <= FLAT_MOST {
            true => Set::of(Members::Flat(members.into_boxed_slice())),
            false => Set::of(Members::Tree(OrdSet::from_iter(members))),
        }
    }

    /// The set of the members of `tree`, kept side by side if it has
    /// shrunk to half of [`FLAT_MOST`].
    fn of_tree(tree: OrdSet<Value>) -> Set {
        if tree.len() > FLAT_MOST / 2 {
            return Set::of(Members::Tree(tree));
        }
        copied(tree.len());
        Set::of(Members::Flat(tree.iter().cloned().collect()))
    }

    fn members(&self) -> &Members {
        &self.0.members
    }

    pub fn len(&self) -> usize {
        match self.members() {
            Members::Flat(members) => members.len(),
            Members::Tree(tree) => tree.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn contains(&self, value: &Value) -> bool {
        match self.members() {
            Members::Flat(members) => find_member(members, value).is_ok(),
            Members::Tree(tree) => tree.contains(value),
        }
    }

    /// The members in order.
    pub fn iter(&self) -> SetIter<'_> {
        SetIter(match self.members() {
            Members::Flat(members) => SetIterOf::Flat(members.iter()),
            Members::Tree(tree) => SetIterOf::Tree(tree.iter()),
        })
    }

    /// The set with `value` a member, whether it was or not.
    pub fn insert(&self, value: Value) -> Set {
        let fns = match self.fns() {
            Fns::Yes => Fns::Yes,
            Fns::No if is_no_fn(&value) => Fns::No,
            _ => Fns::Unknown,
        };
        self.inserted(value).knowing(fns)
    }

    fn inserted(&self, value: Value) -> Set {
        match self.members() {
            Members::Flat(members) => match find_member(members, &value) {
                Ok(_) => self.clone(),
                Err(at) if members.len() < FLAT_MOST => Set::of(Members::Flat(
                    with_at(members, at, value).into_boxed_slice(),
                )),
                Err(_) => {
                    copied(members.len() + 1);
                    let mut tree = OrdSet::from_iter(members.iter().cloned());
                    tree.insert(value);
                    Set::of(Members::Tree(tree))
                }
            },
            Members::Tree(tree) => match tree.contains(&value) {
                true => self.clone(),
                false => {
                    let mut tree = tree.clone();
                    copied(1);
                    tree.insert(value);
                    Set::of(Members::Tree(tree))
                }
            },
        }
    }

    /// The set without `value`.
    pub fn remove(&self, value: &Value) -> Set {
        self.removed(value).knowing(self.fns().some_of())
    }

    fn removed(&self, value: &Value) -> Set {
        match self.members() {
            Members::Flat(members) => match find_member(members, value) {
                Ok(at) => Set::of(Members::Flat(without_at(members, at).into_boxed_slice())),
                Err(_) => self.clone(),
            },
            Members::Tree(tree) => match tree.contains(value) {
                true => {
                    let mut tree = tree.clone();
                    copied(1);
                    tree.remove(value);
                    Set::of_tree(tree)
                }
                false => self.clone(),
            },
        }
    }

    /// The set and `other`, the larger first.
    fn by_size<'a>(&'a self, other: &'a Set) -> (&'a Set, &'a Set) {
        match self.len() >= other.len() {
            true => (self, other),
            false => (other, self),
        }
    }

    /// The members of either set.
    ///
    /// The members of the smaller set go into the larger one: the cost
    /// grows with the smaller one's size times the logarithm of the
    /// larger one's.
    pub fn union(&self, other: &Set) -> Set {
        self.united(other).knowing(self.fns().either(other.fns()))
    }

    fn united(&self, other: &Set) -> Set {
        let (larger, smaller) = self.by_size(other);
        if smaller.is_empty() {
            return larger.clone();
        }
        let Members::Tree(tree) = larger.members() else {
            // Both are small: side by side, each member once.
            let mut made = Vec::with_capacity(self.len() + other.len());
            side_by_side(
                self.iter(),
                other.iter(),
                |member| member,
                |ours, theirs| {
                    if let Some(member) = ours.or(theirs) {
                        made.push(member.clone());
                    }
                },
            );
            return Set::of_sorted(made);
        };
        let mut made: Option<OrdSet<Value>> = None;
        for member in smaller.iter() {
            if !tree.contains(member) {
                copied(1);
                made.get_or_insert_with(|| tree.clone())
                    .insert(member.clone());
            }
        }
        match made {
            Some(tree) => Set::of(Members::Tree(tree)),
            None => larger.clone(),
        }
    }

    /// The members of both sets.
    ///
    /// The smaller set is walked and each of its members looked up in
    /// the larger one.
    pub fn intersection(&self, other: &Set) -> Set {
        let fns = match (self.fns(), other.fns()) {
            (Fns::No, _) | (_, Fns::No) => Fns::No,
            _ => Fns::Unknown,
        };
        let (larger, smaller) = self.by_size(other);
        let both = smaller.iter().filter(|member| larger.contains(member));
        Set::of_sorted(both.cloned().collect()).knowing(fns)
    }

    /// The members of the set that `other` does not have.
    ///
    /// The smaller set is walked: the members of the set that `other`
    /// lacks are kept, or those of `other` are taken out of the set.
    pub fn difference(&self, other: &Set) -> Set {
        self.differed(other).knowing(self.fns().some_of())
    }

    fn differed(&self, other: &Set) -> Set {
        let tree = match self.members() {
            Members::Tree(tree) if self.len() > other.len() => tree,
            _ => {
                let kept = self.iter().filter(|member| !other.contains(member));
                let kept: Vec<Value> = kept.cloned().collect();
                return match kept.len() == self.len() {
                    true => self.clone(),
                    false => Set::of_sorted(kept),
                };
            }
        };
        let mut made: Option<OrdSet<Value>> = None;
        for member in other.iter() {
            if tree.contains(member) {
                copied(1);
                made.get_or_insert_with(|| tree.clone()).remove(member);
            }
        }
        match made {
            Some(tree) => Set::of_tree(tree),
            None => self.clone(),
        }
    }

    /// The members of one of the sets and not of the other.
    ///
    /// Each member of the smaller set goes into the larger one, or out
    /// of it if it has it.
    pub fn symmetric_difference(&self, other: &Set) -> Set {
        let fns = self.fns().either(other.fns()).some_of();
        self.symmetric_differed(other).knowing(fns)
    }

    fn symmetric_differed(&self, other: &Set) -> Set {
        let (larger, smaller) = self.by_size(other);
        if smaller.is_empty() {
            return larger.clone();
        }
        let Members::Tree(tree) = larger.members() else {
            // Both are small: side by side.
            let mut made = Vec::with_capacity(self.len() + other.len());
            side_by_side(
                self.iter(),
                other.iter(),
                |member| member,
                |ours, theirs| {
                    if let (Some(member), None) | (None, Some(member)) = (ours, theirs) {
                        made.push(member.clone());
                    }
                },
            );
            return Set::of_sorted(made);
        };
        let mut tree = tree.clone();
        for member in smaller.iter() {
            copied(1);
            if tree.remove(member).is_none() {
                tree.insert(member.clone());
            }
        }
        Set::of_tree(tree)
    }

    /// Whether `other` has every member of the set.
    pub fn is_subset(&self, other: &Set) -> bool {
        self.len() <= other.len() && self.iter().all(|member| other.contains(member))
    }
}

impl FromIterator<Value> for Set {
    fn from_iter<I: IntoIterator<Item = Value>>(members: I) -> Set {
        let mut members: Vec<Value> = members.into_iter().collect();
        members.sort();
        members.dedup_by(|later, earlier| Value::cmp(later, earlier) == Ordering::Equal);
        Set::of_sorted(members)
    }
}

/// The members of a set in order ([`Set::iter`]).
pub struct SetIter<'a>(SetIterOf<'a>);

enum SetIterOf<'a> {
    Flat(std::slice::Iter<'a, Value>),
    Tree(imbl::ordset::Iter<'a, Value, DefaultSharedPtr>),
}

impl<'a> Iterator for SetIter<'a> {
    type Item = &'a Value;

    #[inline]
    fn next(&mut self) -> Option<&'a Value> {
        match &mut self.0 {
            SetIterOf::Flat(members) => members.next(),
            SetIterOf::Tree(members) => members.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match &self.0 {
            SetIterOf::Flat(members) => members.size_hint(),
            SetIterOf::Tree(members) => members.size_hint(),
        }
    }
}

impl ExactSizeIterator for SetIter<'_> {}

/// The members of a set in order, for as long as the iterator is kept:
/// as the entries of a map are ([`MapIntoIter`]).
pub struct SetIntoIter(SetIntoIterOf);

enum SetIntoIterOf {
    Flat { set: Set, at: usize },
    Tree(imbl::ordset::ConsumingIter<Value, DefaultSharedPtr>),
}

impl Iterator for SetIntoIter {
    type Item = Value;

    fn next(&mut self) -> Option<Value> {
        match &mut self.0 {
            SetIntoIterOf::Flat { set, at } => {
                let Members::Flat(members) = set.members() else {
                    return None;
                };
                let member = members.get(*at)?.clone();
                *at += 1;
                Some(member)
            }
            SetIntoIterOf::Tree(members) => members.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match &self.0 {
            SetIntoIterOf::Flat { set, at } => {
                let left = set.len() - at;
                (left, Some(left))
            }
            SetIntoIterOf::Tree(members) => members.size_hint(),
        }
    }
}

impl IntoIterator for Set {
    type Item = Value;
    type IntoIter = SetIntoIter;

    fn into_iter(self) -> SetIntoIter {
        SetIntoIter(match self.members() {
            Members::Flat(_) => SetIntoIterOf::Flat { set: self, at: 0 },
            Members::Tree(tree) => SetIntoIterOf::Tree(tree.clone().into_iter()),
        })
    }
}

/// Whether `value` is known to be no function and to have none in it,
/// without a walk: a value that is made of no values and is no
/// function, or a set that is known to hold none.
fn is_no_fn(value: &Value) -> bool {
    match value {
        Value::Set(set) => set.holds_fn_known() == Some(false),
        Value::List(_) | Value::Tuple(_) | Value::Map(_) | Value::Record(_) | Value::Variant(_) => {
            false
        }
        other => !other.is_fn(),
    }
}

/// Count `n` members walked to learn whether a set holds a function.
#[inline]
fn walked(n: usize) {
    #[cfg(test)]
    walks::WALKED.set(walks::WALKED.get() + n as u64);
    #[cfg(not(test))]
    let _ = n;
}

/// How many members have been walked to learn whether a set holds a
/// function ([`Set::holds_fn`]): what the test of the gate of the
/// operations on two sets counts.
#[cfg(test)]
pub mod walks {
    use std::cell::Cell;

    thread_local! {
        pub(super) static WALKED: Cell<u64> = const { Cell::new(0) };
    }

    /// The members walked on this thread so far.
    pub fn count() -> u64 {
        WALKED.get()
    }
}

/// Count `n` entries, or members, copied or moved into storage made
/// for a map or a set.
#[inline]
fn copied(n: usize) {
    #[cfg(any(test, feature = "test-hooks"))]
    copies::COPIED.set(copies::COPIED.get() + n as u64);
    #[cfg(not(any(test, feature = "test-hooks")))]
    let _ = n;
}

/// How many entries and members have been copied, or moved, into
/// storage made for maps and sets: what the tests of how the cost of a
/// change grows with the size count. An entry more put into a tree, or
/// one taken out of it, is one; entries side by side, or a tree, made
/// of entries are as many as they hold. (A tree copies part of the
/// path from its root to the entry it changes, at most 16 entries a
/// level, which is not counted here.)
#[cfg(any(test, feature = "test-hooks"))]
pub mod copies {
    use std::cell::Cell;

    thread_local! {
        pub(super) static COPIED: Cell<u64> = const { Cell::new(0) };
    }

    /// The entries and members copied, or moved, into storage for maps
    /// and sets on this thread so far.
    pub fn count() -> u64 {
        COPIED.get()
    }
}

#[cfg(test)]
impl Map {
    /// The map of `entries`, in key order, each key once, kept side by
    /// side however many there are (`flat`) or in a tree.
    pub(super) fn kept_as(entries: Vec<(Value, Value)>, flat: bool) -> Map {
        match flat {
            true => Map::of(Entries::Flat(entries.into_boxed_slice())),
            false => Map::of(Entries::Tree(OrdMap::from_iter(entries))),
        }
    }

    /// Whether the map keeps its entries in a tree.
    pub(super) fn is_tree(&self) -> bool {
        matches!(self.entries(), Entries::Tree(_))
    }
}

#[cfg(test)]
impl Set {
    /// The set of `members`, in order, each once, kept side by side
    /// however many there are (`flat`) or in a tree.
    pub(super) fn kept_as(members: Vec<Value>, flat: bool) -> Set {
        match flat {
            true => Set::of(Members::Flat(members.into_boxed_slice())),
            false => Set::of(Members::Tree(OrdSet::from_iter(members))),
        }
    }

    /// Whether the set keeps its members in a tree.
    pub(super) fn is_tree(&self) -> bool {
        matches!(self.members(), Members::Tree(_))
    }
}
