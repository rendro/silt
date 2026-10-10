//! The list value.
//!
//! A list is a run of the elements of a buffer that lists share: the
//! tail of a list, or any part of it, is a list of the same buffer, and
//! taking it copies no element. The list a range expression `a..b`
//! makes holds its two ends and no element at all. A clone is a count.
//!
//! How a list is stored is this file's own. The rest of silt has its
//! length, its elements by index and in order, and its parts
//! ([`List::len`], [`List::get`], [`List::iter`], [`List::slice`]), the
//! list of its elements and another's ([`List::concat`]), and nothing
//! that tells one way of storing a list from another. What
//! would otherwise visit every element of a list that holds none is
//! asked here, and answered from the two ends ([`List::contains`],
//! [`List::position`], [`List::sum_ints`], [`List::product_ints`],
//! [`List::sorted`], [`List::unique`]).

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use super::Value;

/// The most elements a list that holds none (`a..b`) may have when it
/// is made a list that holds them: reversed, written out, encoded.
/// Prevents exhausting memory with `(1..1_000_000_000) |> list.reverse`.
pub(crate) const MAX_RANGE_MATERIALIZE: usize = 10_000_000;

/// A list.
#[derive(Clone)]
pub struct List(Stored);

#[derive(Clone)]
enum Stored {
    /// `len` elements of `buf` from `start`.
    Items {
        buf: Arc<[Value]>,
        start: usize,
        len: usize,
    },
    /// The Ints from `lo` to `hi`, both included: `lo <= hi`, and at
    /// most [`MAX_LEN`] of them.
    Ints { lo: i64, hi: i64 },
}

/// The most elements a list has: what its length, a `usize`, counts.
/// (All the Ints there are, from the least to the greatest, are one
/// more than that where a `usize` has 64 bits.)
const MAX_LEN: u64 = usize::MAX as u64;

/// A list's elements as they are stored, for the walks of a value this
/// module's siblings do (its key, its text).
pub(super) enum Elements<'a> {
    Items(&'a [Value]),
    /// The Ints from the first to the second, both included; never
    /// empty.
    Ints(i64, i64),
}

/// What [`List::sum_ints`] and [`List::product_ints`] give.
pub enum IntTotal {
    Total(i64),
    /// The sum, or the product, is no `Int`.
    Overflow,
    /// An element is no `Int`.
    NotInts,
}

/// Why a list could not be made, or its elements written out: it has
/// too many. Its text is the runtime error's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TooLong {
    lo: i64,
    hi: i64,
    /// The most elements there may be.
    most: u64,
}

impl fmt::Display for TooLong {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let TooLong { lo, hi, most } = *self;
        let len = hi as i128 - lo as i128 + 1;
        if most == MAX_LEN {
            write!(
                f,
                "range {lo}..{hi} has {len} elements: a list has at most {most}"
            )
        } else {
            write!(
                f,
                "range {lo}..{hi} has {len} elements; materializing more than {most} is not allowed"
            )
        }
    }
}

impl List {
    /// The list of no elements.
    pub fn new() -> List {
        List::from(Vec::new())
    }

    /// The list of the Ints from `lo` to `hi`, both included: what the
    /// range expression `lo..hi` makes. It is empty if `hi` is less
    /// than `lo`.
    pub fn ints(lo: i64, hi: i64) -> Result<List, TooLong> {
        if hi < lo {
            return Ok(List::new());
        }
        // Its length is one more than the difference of the ends.
        let counted = usize::try_from(hi.abs_diff(lo))
            .ok()
            .and_then(|difference| difference.checked_add(1));
        match counted {
            Some(_) => Ok(List(Stored::Ints { lo, hi })),
            None => Err(TooLong {
                lo,
                hi,
                most: MAX_LEN,
            }),
        }
    }

    pub fn len(&self) -> usize {
        match &self.0 {
            Stored::Items { len, .. } => *len,
            Stored::Ints { lo, hi } => hi.abs_diff(*lo) as usize + 1,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The element at `index`, counted from 0.
    pub fn get(&self, index: usize) -> Option<Value> {
        match self.elements() {
            Elements::Items(items) => items.get(index).cloned(),
            Elements::Ints(lo, _) => (index < self.len()).then(|| Value::Int(nth(lo, index))),
        }
    }

    pub fn first(&self) -> Option<Value> {
        self.get(0)
    }

    pub fn last(&self) -> Option<Value> {
        self.get(self.len().checked_sub(1)?)
    }

    /// The elements in order.
    pub fn iter(&self) -> Iter<'_> {
        Iter {
            list: self,
            next: 0,
            end: self.len(),
        }
    }

    /// The elements from `from` up to, not including, `to`, as a list:
    /// no element is copied. A bound past the end is the end.
    pub fn slice(&self, from: usize, to: usize) -> List {
        let to = to.min(self.len());
        let from = from.min(to);
        if from == to {
            return List::new();
        }
        List(match &self.0 {
            Stored::Items { buf, start, .. } => Stored::Items {
                buf: buf.clone(),
                start: start + from,
                len: to - from,
            },
            Stored::Ints { lo, .. } => Stored::Ints {
                lo: nth(*lo, from),
                hi: nth(*lo, to - 1),
            },
        })
    }

    /// The elements, each a value of its own. A list that holds none
    /// may have too many for that.
    pub fn to_vec(&self) -> Result<Vec<Value>, TooLong> {
        self.writable()?;
        Ok(match self.elements() {
            Elements::Items(items) => items.to_vec(),
            Elements::Ints(lo, hi) => (lo..=hi).map(Value::Int).collect(),
        })
    }

    /// The elements of the list and then those of `other`, as one
    /// list that holds them: a list that holds none may have too many
    /// for that.
    pub fn concat(&self, other: &List) -> Result<List, TooLong> {
        self.writable()?;
        other.writable()?;
        if other.is_empty() {
            return Ok(self.clone());
        }
        if self.is_empty() {
            return Ok(other.clone());
        }
        Ok(match (self.elements(), other.elements()) {
            // (The elements of both are there: each slice is copied
            // in the loop a slice is copied in. One chain over the
            // two, read element by element, took 1.4 times as long.)
            (Elements::Items(first), Elements::Items(second)) => {
                let mut items = Vec::with_capacity(first.len() + second.len());
                items.extend_from_slice(first);
                items.extend_from_slice(second);
                List::from(items)
            }
            _ => self.iter().chain(other.iter()).collect(),
        })
    }

    /// Whether the elements may be written out one by one ([`List::to_vec`],
    /// the text of the list): a list that holds its elements always, one
    /// that holds none (`a..b`) up to 10,000,000 of them.
    pub fn writable(&self) -> Result<(), TooLong> {
        match self.0 {
            Stored::Ints { lo, hi } if hi.abs_diff(lo) >= MAX_RANGE_MATERIALIZE as u64 => {
                Err(TooLong {
                    lo,
                    hi,
                    most: MAX_RANGE_MATERIALIZE as u64,
                })
            }
            _ => Ok(()),
        }
    }

    /// Whether `value` is an element.
    pub fn contains(&self, value: &Value) -> bool {
        self.position(value).is_some()
    }

    /// The index of the first element equal to `value`.
    pub fn position(&self, value: &Value) -> Option<usize> {
        match self.elements() {
            Elements::Items(items) => items.iter().position(|item| item == value),
            Elements::Ints(lo, hi) => match value {
                Value::Int(n) if (lo..=hi).contains(n) => Some(n.abs_diff(lo) as usize),
                _ => None,
            },
        }
    }

    /// Whether an element is a function, or has one inside it
    /// ([`Value::contains_fn`]).
    pub fn contains_fn(&self) -> bool {
        match self.elements() {
            Elements::Items(items) => items.iter().any(Value::contains_fn),
            Elements::Ints(..) => false,
        }
    }

    /// The sum of the elements of a list of Ints.
    pub fn sum_ints(&self) -> IntTotal {
        match self.elements() {
            Elements::Items(items) => {
                let mut sum: i64 = 0;
                for item in items {
                    let Value::Int(n) = item else {
                        return IntTotal::NotInts;
                    };
                    let Some(next) = sum.checked_add(*n) else {
                        return IntTotal::Overflow;
                    };
                    sum = next;
                }
                IntTotal::Total(sum)
            }
            // Half of the count times the sum of the ends. (A product
            // that an `i128` does not hold is no `Int` halved either.)
            Elements::Ints(lo, hi) => {
                let count = hi as i128 - lo as i128 + 1;
                let twice = (lo as i128 + hi as i128).checked_mul(count);
                match twice.map(|twice| i64::try_from(twice / 2)) {
                    Some(Ok(sum)) => IntTotal::Total(sum),
                    _ => IntTotal::Overflow,
                }
            }
        }
    }

    /// The product of the elements of a list of Ints. It is 0 if one
    /// of them is 0, whatever the product of the others is.
    pub fn product_ints(&self) -> IntTotal {
        match self.elements() {
            Elements::Items(items) => {
                // `None` once the product so far is no `Int`: a 0
                // further on makes it one again.
                let mut product = Some(1_i64);
                for item in items {
                    match item {
                        Value::Int(0) => return IntTotal::Total(0),
                        Value::Int(n) => product = product.and_then(|p| p.checked_mul(*n)),
                        _ => return IntTotal::NotInts,
                    }
                }
                product.map_or(IntTotal::Overflow, IntTotal::Total)
            }
            Elements::Ints(lo, hi) if lo <= 0 && 0 <= hi => IntTotal::Total(0),
            // With no 0 among them all but one are 2 or more, or -2 or
            // less: the product is no `Int` before the 65th.
            Elements::Ints(lo, hi) => {
                let mut product: i64 = 1;
                for n in lo..=hi {
                    let Some(next) = product.checked_mul(n) else {
                        return IntTotal::Overflow;
                    };
                    product = next;
                }
                IntTotal::Total(product)
            }
        }
    }

    /// The elements in ascending order, as a list. (A list that holds
    /// no element has them in that order.)
    pub fn sorted(&self) -> List {
        match self.elements() {
            Elements::Items(items) => {
                let mut sorted = items.to_vec();
                sorted.sort();
                List::from(sorted)
            }
            Elements::Ints(..) => self.clone(),
        }
    }

    /// The elements that are equal to none before them, as a list. (A
    /// list that holds no element has none twice.)
    pub fn unique(&self) -> List {
        match self.elements() {
            Elements::Items(items) => {
                let mut seen = BTreeSet::new();
                items
                    .iter()
                    .filter(|item| seen.insert(*item))
                    .cloned()
                    .collect()
            }
            Elements::Ints(..) => self.clone(),
        }
    }

    /// The elements as they are stored.
    pub(super) fn elements(&self) -> Elements<'_> {
        match &self.0 {
            Stored::Items { buf, start, len } => Elements::Items(&buf[*start..*start + *len]),
            Stored::Ints { lo, hi } => Elements::Ints(*lo, *hi),
        }
    }

    /// The first and the last element of a list of Ints, each one more
    /// than the one before: such a list is equal to the range of its
    /// ends, however it is stored.
    pub(super) fn ascending_ints(&self) -> Option<(i64, i64)> {
        match self.elements() {
            Elements::Ints(lo, hi) => Some((lo, hi)),
            Elements::Items([Value::Int(lo), rest @ ..]) => {
                let mut last = *lo;
                for item in rest {
                    match item {
                        Value::Int(n) if last.checked_add(1) == Some(*n) => last = *n,
                        _ => return None,
                    }
                }
                Some((*lo, last))
            }
            Elements::Items(_) => None,
        }
    }
}

/// The Int `index` places after `lo` in a list of Ints that has it:
/// the sum is an Int, though `index` alone may be more than one holds.
fn nth(lo: i64, index: usize) -> i64 {
    lo.wrapping_add(index as i64)
}

impl Default for List {
    fn default() -> List {
        List::new()
    }
}

impl From<Vec<Value>> for List {
    fn from(items: Vec<Value>) -> List {
        let len = items.len();
        List(Stored::Items {
            buf: Arc::from(items),
            start: 0,
            len,
        })
    }
}

impl FromIterator<Value> for List {
    fn from_iter<I: IntoIterator<Item = Value>>(items: I) -> List {
        let buf: Arc<[Value]> = items.into_iter().collect();
        let len = buf.len();
        List(Stored::Items { buf, start: 0, len })
    }
}

/// The elements of a list in order ([`List::iter`]).
pub struct Iter<'a> {
    list: &'a List,
    next: usize,
    end: usize,
}

impl Iterator for Iter<'_> {
    type Item = Value;

    fn next(&mut self) -> Option<Value> {
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
    fn next_back(&mut self) -> Option<Value> {
        if self.next == self.end {
            return None;
        }
        self.end -= 1;
        self.list.get(self.end)
    }
}

impl ExactSizeIterator for Iter<'_> {}

impl<'a> IntoIterator for &'a List {
    type Item = Value;
    type IntoIter = Iter<'a>;

    fn into_iter(self) -> Iter<'a> {
        self.iter()
    }
}

/// The elements of a list in order, for as long as the iterator is
/// kept: it holds the list.
pub struct IntoIter {
    list: List,
    next: usize,
}

impl Iterator for IntoIter {
    type Item = Value;

    fn next(&mut self) -> Option<Value> {
        let item = self.list.get(self.next)?;
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
