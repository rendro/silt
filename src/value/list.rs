//! The list value.
//!
//! A list is a run of the elements of a buffer that lists share: the
//! tail of a list, or any part of it, is a list of the same buffer, and
//! taking it copies no element. A clone is a count.
//!
//! How a list is stored is this file's own. The rest of silt has its
//! length, its elements by index and in order, and its parts
//! ([`List::len`], [`List::get`], [`List::iter`], [`List::slice`]), and
//! nothing that tells one way of storing a list from another.

use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

use super::Value;

/// A list.
#[derive(Clone)]
pub struct List {
    /// The buffer the elements are in.
    buf: Arc<[Value]>,
    /// Where in the buffer the list begins.
    start: usize,
    len: usize,
}

/// An element of a list, as reading the list gives it: read where it
/// is (it is a `&Value`), or taken as a value of its own
/// ([`Item::into_value`]).
pub struct Item<'a>(&'a Value);

impl Item<'_> {
    /// The element as a value of its own.
    pub fn into_value(self) -> Value {
        self.0.clone()
    }
}

impl fmt::Debug for Item<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl Deref for Item<'_> {
    type Target = Value;

    fn deref(&self) -> &Value {
        self.0
    }
}

impl List {
    /// The list of no elements.
    pub fn new() -> List {
        List::from(Vec::new())
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The element at `index`, counted from 0.
    pub fn get(&self, index: usize) -> Option<Item<'_>> {
        self.as_slice().get(index).map(Item)
    }

    pub fn first(&self) -> Option<Item<'_>> {
        self.get(0)
    }

    pub fn last(&self) -> Option<Item<'_>> {
        self.get(self.len.checked_sub(1)?)
    }

    /// The elements in order.
    pub fn iter(&self) -> Iter<'_> {
        Iter {
            list: self,
            next: 0,
            end: self.len,
        }
    }

    /// The elements from `from` up to, not including, `to`, as a list:
    /// no element is copied. A bound past the end is the end.
    pub fn slice(&self, from: usize, to: usize) -> List {
        let to = to.min(self.len);
        let from = from.min(to);
        List {
            buf: self.buf.clone(),
            start: self.start + from,
            len: to - from,
        }
    }

    /// The elements, each a value of its own.
    pub fn to_vec(&self) -> Vec<Value> {
        self.as_slice().to_vec()
    }

    /// The elements where they are: for the walks of a value this
    /// module's siblings do (its key, its text).
    pub(super) fn as_slice(&self) -> &[Value] {
        &self.buf[self.start..self.start + self.len]
    }
}

impl fmt::Debug for List {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.as_slice()).finish()
    }
}

impl Default for List {
    fn default() -> List {
        List::new()
    }
}

impl From<Vec<Value>> for List {
    fn from(items: Vec<Value>) -> List {
        let len = items.len();
        List {
            buf: Arc::from(items),
            start: 0,
            len,
        }
    }
}

impl FromIterator<Value> for List {
    fn from_iter<I: IntoIterator<Item = Value>>(items: I) -> List {
        List::from(items.into_iter().collect::<Vec<Value>>())
    }
}

/// The elements of a list in order ([`List::iter`]).
pub struct Iter<'a> {
    list: &'a List,
    next: usize,
    end: usize,
}

impl<'a> Iterator for Iter<'a> {
    type Item = Item<'a>;

    fn next(&mut self) -> Option<Item<'a>> {
        if self.next == self.end {
            return None;
        }
        let item = self.list.get(self.next);
        self.next += 1;
        item
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = self.end - self.next;
        (left, Some(left))
    }
}

impl DoubleEndedIterator for Iter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.next == self.end {
            return None;
        }
        self.end -= 1;
        self.list.get(self.end)
    }
}

impl ExactSizeIterator for Iter<'_> {}

impl<'a> IntoIterator for &'a List {
    type Item = Item<'a>;
    type IntoIter = Iter<'a>;

    fn into_iter(self) -> Iter<'a> {
        self.iter()
    }
}

/// The elements of a list in order, each a value of its own, for as
/// long as the iterator is kept: it holds the list.
pub struct IntoIter {
    list: List,
    next: usize,
}

impl Iterator for IntoIter {
    type Item = Value;

    fn next(&mut self) -> Option<Value> {
        let item = self.list.get(self.next)?.into_value();
        self.next += 1;
        Some(item)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = self.list.len() - self.next;
        (left, Some(left))
    }
}

impl IntoIterator for List {
    type Item = Value;
    type IntoIter = IntoIter;

    fn into_iter(self) -> IntoIter {
        IntoIter {
            list: self,
            next: 0,
        }
    }
}
