//! The map value and the set value.
//!
//! A map is its entries, each a key and its value, in the order of the
//! keys; a set is its members, in their order. Neither is ever changed:
//! an update is a new map, or set, and the one it was made from stays
//! as it was. A clone is a count.
//!
//! How a map and a set are stored is this file's own. The rest of silt
//! has their sizes, their lookups, their entries and members in order
//! ([`Map::iter`], [`Set::iter`]) and the updates of the stdlib
//! ([`Map::insert`], [`Set::union`], ...), and nothing that tells one
//! way of storing them from another.

use std::collections::{BTreeMap, BTreeSet, btree_map, btree_set};
use std::sync::Arc;

use super::Value;
use super::deep::Kept;

/// How many entries, or members, a map or a set is built of at once
/// from at least: they are sorted first, which is faster for many of
/// them, and slower for a few (a literal) than inserting each in turn.
const IN_BULK: usize = 16;

/// A map.
#[derive(Clone, Default)]
pub struct Map(Arc<Kept<BTreeMap<Value, Value>>>);

/// A set.
#[derive(Clone, Default)]
pub struct Set(Arc<Kept<BTreeSet<Value>>>);

impl Map {
    /// The map of no entries.
    pub fn new() -> Map {
        Map::default()
    }

    fn of(entries: BTreeMap<Value, Value>) -> Map {
        Map(Arc::new(Kept::from(entries)))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The map of a literal: `items` are its keys and values, each key
    /// before its value; of a key given twice, the value given last.
    /// (Read in place: built through an iterator that moves each pair,
    /// a loop of map literals took 15% longer.)
    pub(crate) fn of_literal(items: &[Value]) -> Map {
        let mut entries = BTreeMap::new();
        let (pairs, _) = items.as_chunks::<2>();
        for [key, value] in pairs {
            entries.insert(key.clone(), value.clone());
        }
        Map::of(entries)
    }

    /// The value of `key`.
    pub fn get(&self, key: &Value) -> Option<&Value> {
        self.0.get(key)
    }

    pub fn contains_key(&self, key: &Value) -> bool {
        self.0.contains_key(key)
    }

    /// The entries, each a key and its value, in the order of the keys.
    pub fn iter(&self) -> MapIter<'_> {
        MapIter(self.0.iter())
    }

    /// The keys in order.
    pub fn keys(&self) -> impl Iterator<Item = &Value> {
        self.0.keys()
    }

    /// The values, in the order of their keys.
    pub fn values(&self) -> impl Iterator<Item = &Value> {
        self.0.values()
    }

    /// The map with `key` of the value `value`, whether it had the key
    /// or not.
    pub fn insert(&self, key: Value, value: Value) -> Map {
        let mut entries = BTreeMap::clone(&self.0);
        entries.insert(key, value);
        Map::of(entries)
    }

    /// The map without `key`.
    pub fn remove(&self, key: &Value) -> Map {
        let mut entries = BTreeMap::clone(&self.0);
        entries.remove(key);
        Map::of(entries)
    }

    /// The entries of the map and those of `other`: of a key both have,
    /// the value `other` has.
    pub fn merge(&self, other: &Map) -> Map {
        let mut entries = BTreeMap::clone(&self.0);
        entries.extend(
            other
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
        Map::of(entries)
    }
}

/// The map of the entries: of a key given twice, the value given last.
impl FromIterator<(Value, Value)> for Map {
    fn from_iter<I: IntoIterator<Item = (Value, Value)>>(entries: I) -> Map {
        let entries = entries.into_iter();
        if entries.size_hint().0 >= IN_BULK {
            return Map::of(entries.collect());
        }
        let mut map = BTreeMap::new();
        for (key, value) in entries {
            map.insert(key, value);
        }
        Map::of(map)
    }
}

/// The entries of a map in the order of their keys ([`Map::iter`]).
pub struct MapIter<'a>(btree_map::Iter<'a, Value, Value>);

impl<'a> Iterator for MapIter<'a> {
    type Item = (&'a Value, &'a Value);

    #[inline]
    fn next(&mut self) -> Option<(&'a Value, &'a Value)> {
        self.0.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl ExactSizeIterator for MapIter<'_> {}

/// The entries of a map in the order of their keys, for as long as the
/// iterator is kept.
// (Copied out of the map when the iterator is made: a map stored as a
// persistent tree will be walked in place, step M.)
pub struct MapIntoIter(std::vec::IntoIter<(Value, Value)>);

impl Iterator for MapIntoIter {
    type Item = (Value, Value);

    fn next(&mut self) -> Option<(Value, Value)> {
        self.0.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl IntoIterator for Map {
    type Item = (Value, Value);
    type IntoIter = MapIntoIter;

    fn into_iter(self) -> MapIntoIter {
        let entries = self.iter().map(|(key, value)| (key.clone(), value.clone()));
        MapIntoIter(entries.collect::<Vec<_>>().into_iter())
    }
}

impl Set {
    /// The set of no members.
    pub fn new() -> Set {
        Set::default()
    }

    fn of(members: BTreeSet<Value>) -> Set {
        Set(Arc::new(Kept::from(members)))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn contains(&self, value: &Value) -> bool {
        self.0.contains(value)
    }

    /// The members in order.
    pub fn iter(&self) -> SetIter<'_> {
        SetIter(self.0.iter())
    }

    /// The set with `value` a member, whether it was or not.
    pub fn insert(&self, value: Value) -> Set {
        let mut members = BTreeSet::clone(&self.0);
        members.insert(value);
        Set::of(members)
    }

    /// The set without `value`.
    pub fn remove(&self, value: &Value) -> Set {
        let mut members = BTreeSet::clone(&self.0);
        members.remove(value);
        Set::of(members)
    }

    /// The members of either set.
    pub fn union(&self, other: &Set) -> Set {
        self.0.union(&other.0).cloned().collect()
    }

    /// The members of both sets.
    pub fn intersection(&self, other: &Set) -> Set {
        self.0.intersection(&other.0).cloned().collect()
    }

    /// The members of the set that `other` does not have.
    pub fn difference(&self, other: &Set) -> Set {
        self.0.difference(&other.0).cloned().collect()
    }

    /// The members of one of the sets and not of the other.
    pub fn symmetric_difference(&self, other: &Set) -> Set {
        self.0.symmetric_difference(&other.0).cloned().collect()
    }

    /// Whether `other` has every member of the set.
    pub fn is_subset(&self, other: &Set) -> bool {
        self.0.is_subset(&other.0)
    }
}

impl FromIterator<Value> for Set {
    fn from_iter<I: IntoIterator<Item = Value>>(members: I) -> Set {
        let members = members.into_iter();
        if members.size_hint().0 >= IN_BULK {
            return Set::of(members.collect());
        }
        let mut set = BTreeSet::new();
        for member in members {
            set.insert(member);
        }
        Set::of(set)
    }
}

/// The members of a set in order ([`Set::iter`]).
pub struct SetIter<'a>(btree_set::Iter<'a, Value>);

impl<'a> Iterator for SetIter<'a> {
    type Item = &'a Value;

    #[inline]
    fn next(&mut self) -> Option<&'a Value> {
        self.0.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl ExactSizeIterator for SetIter<'_> {}

/// The members of a set in order, for as long as the iterator is kept.
// (Copied out of the set when the iterator is made: a set stored as a
// persistent tree will be walked in place, step M.)
pub struct SetIntoIter(std::vec::IntoIter<Value>);

impl Iterator for SetIntoIter {
    type Item = Value;

    fn next(&mut self) -> Option<Value> {
        self.0.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl IntoIterator for Set {
    type Item = Value;
    type IntoIter = SetIntoIter;

    fn into_iter(self) -> SetIntoIter {
        SetIntoIter(self.iter().cloned().collect::<Vec<_>>().into_iter())
    }
}
