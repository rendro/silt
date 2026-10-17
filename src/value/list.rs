//! The list value.
//!
//! A list is a run of the elements of storage that lists share: the
//! tail of a list, or any part of it that is a quarter of the storage
//! or more, is a list of the same storage, and taking it copies no
//! element ([`List::slice`]). A short list keeps its elements side by
//! side in a buffer of their own, and so does a long one made in one
//! go (by `list.map`, a literal, a decoder), which is read as fast. A
//! long list a change made keeps them in a tree (the RRB vector of the
//! crate `imbl`), and a list made from it by one change (an element
//! more at either end, one in place of another, one taken out, a short
//! list joined to it) shares all of the tree but a path from its root
//! with it: the change costs steps that grow with the logarithm of the
//! length, not with the length ([`FLAT_MOST`]). The first change of a
//! long list made in one go makes the tree of its elements, once, and
//! keeps it with them for the next.
//! The list a range expression `a..b` makes holds its two ends and no
//! element at all. A clone is a count.
//!
//! How a list is stored is this file's own. The rest of silt has its
//! length, its elements by index and in order, and its parts
//! ([`List::len`], [`List::get`], [`List::iter`], [`List::slice`]), the
//! list of its elements and another's ([`List::concat`]), and nothing
//! that tells one way of storing a list from another. What
//! would otherwise visit every element of a list that holds none is
//! asked here, and answered from the two ends ([`List::contains`],
//! [`List::position`], [`List::sum_ints`], [`List::product_ints`],
//! [`List::sorted`], [`List::unique`]). The walks of a value this
//! module's siblings do (its key, its text) read the elements a list
//! holds as runs of them side by side in memory ([`List::runs`]), and
//! a list that holds none by its two ends ([`List::range_ends`]).

use super::Value;
use super::deep::{Buf, Long, LongItems};
use imbl::Vector;
use imbl::shared_ptr::DefaultSharedPtr;
use std::collections::BTreeSet;
use std::fmt;
use std::sync::OnceLock;

/// The most elements a list that holds none (`a..b`) may have when it
/// is made a list that holds them: reversed, written out, encoded.
/// Prevents exhausting memory with `(1..1_000_000_000) |> list.reverse`.
pub(crate) const MAX_RANGE_MATERIALIZE: usize = 10_000_000;

/// The most elements a list a change makes keeps side by side in a
/// buffer; a longer one keeps them in a tree. A list made from one kept
/// in a tree goes back to a buffer only when it has half as many, so
/// that a list that grows and shrinks around the length does not cross
/// it each time.
///
/// A change to a list in a buffer copies the buffer: at most this many
/// elements, a cost that does not grow with the list.
///
/// Measured 2026-10-16 through the VM (release builds with 16, 32 and
/// 64, interleaved, median of five): lists of 40, 64, 96 and 200
/// elements built by `list.append`, `[x, ..xs]` and `list.set` and read
/// by `[x, ..rest]` and `list.get` took the same time with each, within
/// 5% and in no order. Memory decided: a tree has a fixed part of about
/// 8 KB however few elements it holds, so with 16 a list of 20 would
/// take 2.6 KB where its buffer takes 0.8 KB. Bytes per list a change
/// made, peak RSS over 20,000 lists (a list made in one go is kept side
/// by side, and takes what a buffer takes, at every length):
///
/// | elements | in a buffer | in a tree |
/// |---|---|---|
/// | 20 | 831 | — |
/// | 64 | 2,590 | — |
/// | 65 | 2,614 | 10,584 |
/// | 128 | 5,144 | 10,584 |
/// | 256 | 10,269 | 13,192 |
/// | 512 | 20,506 | 25,768 |
/// | 1,000 | 40,042 | 46,641 |
///
/// (The buffer column past 64 is what a buffer of that length takes:
/// a list made in one go, or one on main.) The trees of 65 to some 500
/// elements take two to four times a buffer, past 1,000 some 1.2
/// times; a size that kept them under 1.3 times everywhere would be
/// 2,048, with changes of every list up to that copying it whole.
pub(super) const FLAT_MOST: usize = 64;

/// The most elements of the shorter of two lists that [`List::concat`]
/// puts into the tree of the other one by one; past it the two are made
/// trees and joined.
///
/// Measured 2026-10-17 through the VM (release builds that always put
/// in, and always join; a fresh short list of `k` joined to a tree of
/// `n` a thousand or two thousand times, median of three; microseconds
/// per join, put in / joined):
///
/// | k | n = 1,000 | n = 100,000 | n = 1,000,000 |
/// |---|---|---|---|
/// | 16 | 0 / 16 | 0 / 29 | — |
/// | 64 | 0 / 19 | 2 / 35 | 1 / 16 |
/// | 256 | 11 / 24 | 11 / 46 | — |
/// | 512 | 15 / 42 | 10 / 57 | 22 / 53 |
/// | 1,024 | 70 / 58 | 33 / 86 | 30 / 55 |
/// | 2,048 | — | 104 / 117 | 77 / 87 |
///
/// (A short list before the tree; after it, joining cost one and a
/// half to three times as much, putting in about the same.) Joining
/// a tree costs some 15 to 110 microseconds however short the list
/// joined; putting an element in, some 30 nanoseconds.
const PUSHED_MOST: usize = 1024;

/// A list.
#[derive(Clone)]
pub struct List(Stored);

#[derive(Clone)]
enum Stored {
    /// `len` elements of `buf` from `start`.
    Items { buf: Buf, start: usize, len: usize },
    /// A list kept otherwise.
    Other(Other),
}

/// A list kept otherwise than in a buffer. (Apart from [`Stored::Items`]
/// so that a list stays 32 bytes, and a value 40 with a tag of its own:
/// the pointer of a buffer is never null, and the null one says a list
/// is one of these; and the pointer of a long list's elements, the one
/// of a list that holds none.)
#[derive(Clone)]
enum Other {
    /// `len` elements of `long` from `start`: a list of more than
    /// [`FLAT_MOST`], kept side by side if it was made in one go, and in
    /// a tree if a change made it.
    Long {
        long: Long,
        start: usize,
        len: usize,
    },
    /// The Ints from `lo` to `hi`, both included: `lo <= hi`, and at
    /// most [`MAX_LEN`] of them.
    Ints { lo: i64, hi: i64 },
}

// A list is 32 bytes where pointers are 64 bits, and has no value of
// its bytes to spare for another's tag: a value, which holds one, is
// 40 bytes with its own tag (src/value/mod.rs).
#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<List>() <= 32);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<Option<List>>() > std::mem::size_of::<List>());

/// The most elements a list has: what its length, a `usize`, counts.
/// (All the Ints there are, from the least to the greatest, are one
/// more than that where a `usize` has 64 bits.)
const MAX_LEN: u64 = usize::MAX as u64;

/// Where a list's elements are.
enum Kept<'a> {
    /// Side by side, and where in their storage they begin.
    Side(&'a [Value], usize),
    /// The `len` elements of a tree from `start`.
    Tree(&'a Vector<Value>, usize, usize),
    /// The Ints from this one on: the list holds none.
    Ints(i64),
}

/// A list's elements as they are stored.
enum Elements<'a> {
    /// The elements the list holds.
    Held(Runs<'a>),
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
            Some(_) => Ok(List(Stored::Other(Other::Ints { lo, hi }))),
            None => Err(TooLong {
                lo,
                hi,
                most: MAX_LEN,
            }),
        }
    }

    pub fn len(&self) -> usize {
        match &self.0 {
            Stored::Items { len, .. } | Stored::Other(Other::Long { len, .. }) => *len,
            Stored::Other(Other::Ints { lo, hi }) => hi.abs_diff(*lo) as usize + 1,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The element at `index`, counted from 0.
    #[inline]
    pub fn get(&self, index: usize) -> Option<Value> {
        match &self.0 {
            Stored::Items { buf, start, len } => buf[*start..*start + *len].get(index).cloned(),
            Stored::Other(Other::Long { long, start, len }) => {
                (index < *len).then(|| match &**long {
                    LongItems::Side { items, .. } => items[start + index].clone(),
                    LongItems::Tree(tree) => tree[start + index].clone(),
                })
            }
            Stored::Other(Other::Ints { lo, .. }) => {
                (index < self.len()).then(|| Value::Int(nth(*lo, index)))
            }
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
        self.iter_part(0, self.len())
    }

    /// The elements from `from` up to, not including, `to`, which are
    /// in the list, in order.
    fn iter_part(&self, from: usize, to: usize) -> Iter<'_> {
        Iter(match self.kept() {
            Kept::Side(items, _) => IterOf::Items(items[from..to].iter()),
            Kept::Tree(tree, start, _) => {
                IterOf::Tree(tree.focus().narrow(start + from..start + to).into_iter())
            }
            Kept::Ints(lo) => IterOf::Ints(IntsLeft {
                next: nth(lo, from),
                left: to - from,
            }),
        })
    }

    /// Where the elements are.
    #[inline]
    fn kept(&self) -> Kept<'_> {
        match &self.0 {
            Stored::Items { buf, start, len } => Kept::Side(&buf[*start..*start + *len], *start),
            Stored::Other(Other::Long { long, start, len }) => match &**long {
                LongItems::Side { items, .. } => Kept::Side(&items[*start..*start + *len], *start),
                LongItems::Tree(tree) => Kept::Tree(tree, *start, *len),
            },
            Stored::Other(Other::Ints { lo, .. }) => Kept::Ints(*lo),
        }
    }

    /// The elements the list holds, in order, as runs of them that are
    /// side by side in memory: no run is empty, and a list that holds
    /// no element ([`List::range_ends`]) has none. Where one run ends
    /// and the next begins is nothing a list shows: two equal lists
    /// may have their runs end in different places.
    pub(super) fn runs(&self) -> Runs<'_> {
        match self.kept() {
            Kept::Side(items, _start) => Runs {
                left: items,
                leaves: None,
                #[cfg(test)]
                at: _start,
            },
            Kept::Tree(tree, start, len) => Runs {
                left: &[],
                leaves: Some(Leaves {
                    leaves: tree.leaves(),
                    passed: 0,
                    skip: start,
                    left: len,
                }),
                #[cfg(test)]
                at: 0,
            },
            Kept::Ints(..) => Runs {
                left: &[],
                leaves: None,
                #[cfg(test)]
                at: 0,
            },
        }
    }

    /// The first and the last element of a list that holds no element
    /// (`a..b`): the Ints from the one to the other. `None` for a list
    /// that holds its elements ([`List::runs`]).
    pub(super) fn range_ends(&self) -> Option<(i64, i64)> {
        match self.0 {
            Stored::Other(Other::Ints { lo, hi }) => Some((lo, hi)),
            Stored::Items { .. } | Stored::Other(Other::Long { .. }) => None,
        }
    }

    /// The elements from `from` up to, not including, `to`, as a list.
    /// A bound past the end is the end.
    ///
    /// The part is a list of the same storage, and no element is
    /// copied, unless it is less than a quarter of the storage: then it
    /// gets storage of its own, so that a list never keeps more than
    /// four times its elements from being freed (the one element kept
    /// of a list of a million would keep the million). A part of a
    /// tree gets a tree of its own made of the parts of the tree it
    /// shares with it, in steps that grow with the logarithm of the
    /// length; a part of a list kept side by side (in a buffer, or a
    /// long one made in one go), or a part of a tree short enough to be
    /// kept in a buffer, is copied. A walk down a list by its rest
    /// copies a third of a list kept side by side in all (a quarter of
    /// it once it is that short, a quarter of that, and so on), and of
    /// a list in a tree the elements of a buffer's length.
    pub fn slice(&self, from: usize, to: usize) -> List {
        let to = to.min(self.len());
        let from = from.min(to);
        if from == to {
            return List::new();
        }
        let len = to - from;
        List(match &self.0 {
            Stored::Items { buf, start, .. } if len < buf.len() / 4 => {
                return List::flat(buf[start + from..start + to].to_vec());
            }
            Stored::Items { buf, start, .. } => Stored::Items {
                buf: buf.clone(),
                start: start + from,
                len,
            },
            Stored::Other(Other::Long { long, start, .. }) if len < long.len() / 4 => {
                return match (self.flat_for(len), &**long) {
                    (true, _) => List::flat(self.iter_part(from, to).collect()),
                    (false, LongItems::Side { items, .. }) => {
                        List::from(items[start + from..start + to].to_vec())
                    }
                    (false, LongItems::Tree(tree)) => {
                        List::of_tree(part_of(tree, start + from, len))
                    }
                };
            }
            Stored::Other(Other::Long { long, start, .. }) => Stored::Other(Other::Long {
                long: long.clone(),
                start: start + from,
                len,
            }),
            Stored::Other(Other::Ints { lo, .. }) => Stored::Other(Other::Ints {
                lo: nth(*lo, from),
                hi: nth(*lo, to - 1),
            }),
        })
    }

    /// The elements, each a value of its own. A list that holds none
    /// may have too many for that.
    pub fn to_vec(&self) -> Result<Vec<Value>, TooLong> {
        self.writable()?;
        let mut items = Vec::with_capacity(self.len());
        self.write_into(&mut items);
        Ok(items)
    }

    /// The elements of the list and then those of `other`, as one
    /// list that holds them: a list that holds none may have too many
    /// for that.
    ///
    /// Of two lists of which one is short (1,024 elements at most) the
    /// elements of the short one are put into the tree of the other
    /// one by one: `[x, ..xs]` costs what an element more at the front
    /// does. Two longer ones are joined tree to tree.
    pub fn concat(&self, other: &List) -> Result<List, TooLong> {
        self.writable()?;
        other.writable()?;
        if other.is_empty() {
            return Ok(self.clone());
        }
        if self.is_empty() {
            return Ok(other.clone());
        }
        let len = self.len() + other.len();
        // (A range joined to a list that keeps no tree is written out
        // with it; a short range joined to a long list is put into its
        // tree as any short list is, and a long one made a tree and
        // joined: writing out the long list would copy it at each
        // `list.concat(acc, i..j)`.)
        let written_out = |range: &List, rest: &List| range.written_out() && !rest.in_long();
        if (self.flat_for(len) && other.flat_for(len))
            || written_out(self, other)
            || written_out(other, self)
        {
            let mut items = Vec::with_capacity(len);
            self.write_into(&mut items);
            other.write_into(&mut items);
            return Ok(List::from(items));
        }
        let tree = if self.len() <= PUSHED_MOST && self.len() <= other.len() {
            let mut tree = other.to_tree();
            copied(self.len());
            for item in self.iter().rev() {
                tree.push_front(item);
            }
            tree
        } else if other.len() <= PUSHED_MOST {
            let mut tree = self.to_tree();
            copied(other.len());
            tree.extend(other.iter());
            tree
        } else {
            let mut tree = self.to_tree();
            tree.append(other.to_tree());
            tree
        };
        Ok(List::of_tree(tree))
    }

    /// The list of `value` and then the elements.
    pub fn push_front(&self, value: Value) -> Result<List, TooLong> {
        self.writable()?;
        let len = self.len() + 1;
        if self.flat_for(len) || self.written_out() {
            let mut items = Vec::with_capacity(len);
            items.push(value);
            self.write_into(&mut items);
            return Ok(List::from(items));
        }
        let mut tree = self.to_tree();
        copied(1);
        tree.push_front(value);
        Ok(List::of_tree(tree))
    }

    /// The list of the elements and then `value`.
    pub fn push_back(&self, value: Value) -> Result<List, TooLong> {
        self.writable()?;
        let len = self.len() + 1;
        if self.flat_for(len) || self.written_out() {
            let mut items = Vec::with_capacity(len);
            self.write_into(&mut items);
            items.push(value);
            return Ok(List::from(items));
        }
        let mut tree = self.to_tree();
        copied(1);
        tree.push_back(value);
        Ok(List::of_tree(tree))
    }

    /// The list with `value` in place of the element at `index`;
    /// `None` if the list has no element there.
    pub fn set(&self, index: usize, value: Value) -> Result<Option<List>, TooLong> {
        if index >= self.len() {
            return Ok(None);
        }
        self.writable()?;
        if self.flat_for(self.len()) || self.written_out() {
            let mut items = self.to_vec()?;
            items[index] = value;
            return Ok(Some(List::from(items)));
        }
        let mut tree = self.to_tree();
        copied(1);
        tree.set(index, value);
        Ok(Some(List::of_tree(tree)))
    }

    /// The list without the element at `index`; `None` if the list has
    /// no element there.
    pub fn remove_at(&self, index: usize) -> Result<Option<List>, TooLong> {
        if index >= self.len() {
            return Ok(None);
        }
        self.writable()?;
        if self.flat_for(self.len() - 1) || self.written_out() {
            let mut items = self.to_vec()?;
            items.remove(index);
            return Ok(Some(List::from(items)));
        }
        let mut tree = self.to_tree();
        tree.remove(index);
        Ok(Some(List::of_tree(tree)))
    }

    /// Whether the elements may be written out one by one ([`List::to_vec`],
    /// the text of the list): a list that holds its elements always, one
    /// that holds none (`a..b`) up to 10,000,000 of them.
    pub fn writable(&self) -> Result<(), TooLong> {
        match self.0 {
            Stored::Other(Other::Ints { lo, hi })
                if hi.abs_diff(lo) >= MAX_RANGE_MATERIALIZE as u64 =>
            {
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
            Elements::Held(runs) => runs.flatten().position(|item| item == value),
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
            Elements::Held(mut runs) => runs.any(|run| run.iter().any(Value::contains_fn)),
            Elements::Ints(..) => false,
        }
    }

    /// The sum of the elements of a list of Ints.
    pub fn sum_ints(&self) -> IntTotal {
        match self.elements() {
            Elements::Held(runs) => {
                let mut sum: i64 = 0;
                for item in runs.flatten() {
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
            Elements::Held(runs) => {
                // `None` once the product so far is no `Int`: a 0
                // further on makes it one again.
                let mut product = Some(1_i64);
                for item in runs.flatten() {
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
            Elements::Held(runs) => {
                let mut sorted: Vec<Value> = runs.flatten().cloned().collect();
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
            Elements::Held(runs) => {
                let mut seen = BTreeSet::new();
                runs.flatten()
                    .filter(|item| seen.insert(*item))
                    .cloned()
                    .collect()
            }
            Elements::Ints(..) => self.clone(),
        }
    }

    /// The elements as they are stored.
    fn elements(&self) -> Elements<'_> {
        match self.0 {
            Stored::Other(Other::Ints { lo, hi }) => Elements::Ints(lo, hi),
            Stored::Items { .. } | Stored::Other(Other::Long { .. }) => Elements::Held(self.runs()),
        }
    }

    /// Push the elements onto `items`, in order.
    fn write_into(&self, items: &mut Vec<Value>) {
        match self.0 {
            Stored::Other(Other::Ints { lo, hi }) => items.extend((lo..=hi).map(Value::Int)),
            Stored::Items { .. } | Stored::Other(Other::Long { .. }) => {
                for run in self.runs() {
                    items.extend_from_slice(run);
                }
            }
        }
    }

    /// The list of `items`, kept side by side in a buffer.
    fn flat(items: Vec<Value>) -> List {
        copied(items.len());
        let len = items.len();
        List(Stored::Items {
            buf: Buf::from(items),
            start: 0,
            len,
        })
    }

    /// The list of the elements of `tree`.
    fn of_tree(tree: Vector<Value>) -> List {
        let len = tree.len();
        List(Stored::Other(Other::Long {
            long: Long::from(LongItems::Tree(tree)),
            start: 0,
            len,
        }))
    }

    /// Whether a list a change makes of this one writes its elements out
    /// side by side, with the change, however many they are: a list that
    /// holds none (`a..b`) has no tree to share, and one built element
    /// by element costs twelve times what writing them out does. (The
    /// list made keeps them as a list made in one go does, and its own
    /// first change makes its tree.)
    fn written_out(&self) -> bool {
        matches!(self.0, Stored::Other(Other::Ints { .. }))
    }

    /// Whether the list is a part of a long list's elements, side by
    /// side or in a tree.
    fn in_long(&self) -> bool {
        matches!(self.0, Stored::Other(Other::Long { .. }))
    }

    /// Whether a list of `len` elements made from this one keeps them
    /// side by side in a buffer: up to [`FLAT_MOST`] of them, and if
    /// this one keeps its own in a tree, up to half as many.
    fn flat_for(&self, len: usize) -> bool {
        match &self.0 {
            Stored::Other(Other::Long { long, .. }) if matches!(**long, LongItems::Tree(_)) => {
                len <= FLAT_MOST / 2
            }
            Stored::Items { .. } | Stored::Other(Other::Long { .. } | Other::Ints { .. }) => {
                len <= FLAT_MOST
            }
        }
    }

    /// The elements, in a tree of their own that a change may be made
    /// to: the tree the list keeps them in, or the part of it the list
    /// is, which shares all of it but its edges with it.
    fn to_tree(&self) -> Vector<Value> {
        match &self.0 {
            Stored::Other(Other::Long { long, start, len }) => match &**long {
                LongItems::Tree(tree) => part_of(tree, *start, *len),
                // (The tree of a list made in one go is made once, and
                // the next change starts from it too.)
                LongItems::Side { items, tree } => {
                    let tree = tree.get_or_init(|| {
                        copied(items.len());
                        items.iter().cloned().collect()
                    });
                    part_of(tree, *start, *len)
                }
            },
            Stored::Items { .. } | Stored::Other(Other::Ints { .. }) => {
                copied(self.len());
                self.iter().collect()
            }
        }
    }

    /// The list of `items`, kept side by side in a buffer however many
    /// they are: the twin the tests compare a list kept otherwise with.
    #[cfg(test)]
    pub(super) fn held_flat(items: Vec<Value>) -> List {
        List::flat(items)
    }

    /// How the list keeps its elements: in a buffer (`"buffer"`), side
    /// by side as a long list made in one go (`"side"`), in a tree
    /// (`"tree"`), or none (`"ints"`). What the tests of how a list is
    /// kept ask.
    #[cfg(test)]
    pub(super) fn kept_as(&self) -> &'static str {
        match &self.0 {
            Stored::Items { .. } => "buffer",
            Stored::Other(Other::Long { long, .. }) => match **long {
                LongItems::Side { .. } => "side",
                LongItems::Tree(_) => "tree",
            },
            Stored::Other(Other::Ints { .. }) => "ints",
        }
    }

    /// The number of elements of the storage the list is a part of, and
    /// where the storage is: what the tests of sharing ask.
    #[cfg(test)]
    pub(super) fn buffer(&self) -> Option<(usize, *const ())> {
        match &self.0 {
            Stored::Items { buf, .. } => Some((buf.len(), buf.as_ptr().cast())),
            Stored::Other(Other::Long { long, .. }) => {
                Some((long.len(), (&**long as *const LongItems).cast()))
            }
            Stored::Other(Other::Ints { .. }) => None,
        }
    }

    /// The first and the last element of a list of Ints, each one more
    /// than the one before: such a list is equal to the range of its
    /// ends, however it is stored.
    pub(super) fn ascending_ints(&self) -> Option<(i64, i64)> {
        match self.elements() {
            Elements::Ints(lo, hi) => Some((lo, hi)),
            Elements::Held(runs) => {
                let mut items = runs.flatten();
                let Some(Value::Int(lo)) = items.next() else {
                    return None;
                };
                let mut last = *lo;
                for item in items {
                    match item {
                        Value::Int(n) if last.checked_add(1) == Some(*n) => last = *n,
                        _ => return None,
                    }
                }
                Some((*lo, last))
            }
        }
    }
}

/// The `len` elements of `tree` from `start`, as a tree of their own.
fn part_of(tree: &Vector<Value>, start: usize, len: usize) -> Vector<Value> {
    let mut part = match start {
        0 => tree.clone(),
        _ => tree.skip(start),
    };
    part.truncate(len);
    part
}

/// Count `n` elements copied, or moved, into storage made for a list.
#[inline]
fn copied(n: usize) {
    #[cfg(any(test, feature = "test-hooks"))]
    copies::COPIED.set(copies::COPIED.get() + n as u64);
    #[cfg(not(any(test, feature = "test-hooks")))]
    let _ = n;
}

/// How many elements have been copied, or moved, into storage made for
/// lists: what the tests of how the cost of a change to a list grows
/// with its length count. An element more put into a tree, or one put
/// in place of another, is one; a buffer, or a tree, made of elements
/// is as many as it holds. (A tree copies part of the path from its
/// root to the element it changes, at most its 64 elements a level,
/// which is not counted here.)
#[cfg(any(test, feature = "test-hooks"))]
pub mod copies {
    use std::cell::Cell;

    thread_local! {
        pub(super) static COPIED: Cell<u64> = const { Cell::new(0) };
    }

    /// The elements copied, or moved, into storage for lists on this
    /// thread so far.
    pub fn count() -> u64 {
        COPIED.get()
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
        if items.len() <= FLAT_MOST {
            return List::flat(items);
        }
        copied(items.len());
        let len = items.len();
        // (A vector of their exact size is kept; one that grew past them
        // is moved to storage of their exact size, as a buffer is made:
        // shrunk in place, it made the memory the next list of the
        // length takes come from the system each time, ten times the
        // page faults of `list.map` of 10,000.)
        let items = match items.capacity() == len {
            true => items,
            false => {
                let mut exact = Vec::with_capacity(len);
                exact.extend(items);
                exact
            }
        };
        let long = LongItems::Side {
            items: items.into_boxed_slice(),
            tree: OnceLock::new(),
        };
        List(Stored::Other(Other::Long {
            long: Long::from(long),
            start: 0,
            len,
        }))
    }
}

impl FromIterator<Value> for List {
    fn from_iter<I: IntoIterator<Item = Value>>(items: I) -> List {
        let items = items.into_iter();
        // (Elements of a known number that a buffer holds go straight
        // into it, as a list literal's do: through a vector they were
        // allocated and copied twice, some 9% of the cycles of a loop
        // that makes a one-element list each round.)
        match items.size_hint() {
            (least, Some(most)) if least == most && most <= FLAT_MOST => {
                let buf: Buf = items.collect();
                let len = buf.len();
                copied(len);
                List(Stored::Items { buf, start: 0, len })
            }
            _ => List::from(items.collect::<Vec<Value>>()),
        }
    }
}

/// The elements of a list in order ([`List::iter`]).
pub struct Iter<'a>(IterOf<'a>);

enum IterOf<'a> {
    Items(std::slice::Iter<'a, Value>),
    Tree(imbl::vector::Iter<'a, Value, DefaultSharedPtr>),
    Ints(IntsLeft),
}

/// The Ints left of a list that holds none: `left` of them from `next`.
struct IntsLeft {
    next: i64,
    left: usize,
}

impl IntsLeft {
    fn next(&mut self) -> Option<Value> {
        self.left = self.left.checked_sub(1)?;
        let n = self.next;
        // (Past the last there is no next: `left` is 0 then.)
        self.next = n.wrapping_add(1);
        Some(Value::Int(n))
    }

    fn next_back(&mut self) -> Option<Value> {
        self.left = self.left.checked_sub(1)?;
        Some(Value::Int(nth(self.next, self.left)))
    }
}

impl Iterator for Iter<'_> {
    type Item = Value;

    #[inline]
    fn next(&mut self) -> Option<Value> {
        match &mut self.0 {
            IterOf::Items(items) => items.next().cloned(),
            IterOf::Tree(items) => items.next().cloned(),
            IterOf::Ints(ints) => ints.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = match &self.0 {
            IterOf::Items(items) => items.len(),
            IterOf::Tree(items) => items.len(),
            IterOf::Ints(ints) => ints.left,
        };
        (left, Some(left))
    }
}

impl DoubleEndedIterator for Iter<'_> {
    fn next_back(&mut self) -> Option<Value> {
        match &mut self.0 {
            IterOf::Items(items) => items.next_back().cloned(),
            IterOf::Tree(items) => items.next_back().cloned(),
            IterOf::Ints(ints) => ints.next_back(),
        }
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
/// kept: it holds the list's elements.
pub struct IntoIter(IntoIterOf);

enum IntoIterOf {
    /// The elements of `buf` from `next` up to, not including, `end`.
    Items {
        buf: Buf,
        next: usize,
        end: usize,
    },
    /// The elements of a long list kept side by side from `next` up
    /// to, not including, `end`: read as those of a buffer are.
    Side {
        long: Long,
        next: usize,
        end: usize,
    },
    /// The elements of a tree from `next` up to, not including, `end`,
    /// after those of the leaf at hand that are left (`leaf`, the last
    /// first): the elements of a leaf of a tree are taken out of it
    /// together.
    Tree {
        long: Long,
        next: usize,
        end: usize,
        leaf: Vec<Value>,
    },
    Ints(IntsLeft),
}

impl Iterator for IntoIter {
    type Item = Value;

    #[inline]
    fn next(&mut self) -> Option<Value> {
        match &mut self.0 {
            IntoIterOf::Items { buf, next, end } => {
                if next == end {
                    return None;
                }
                let item = buf[*next].clone();
                *next += 1;
                Some(item)
            }
            IntoIterOf::Side { long, next, end } => {
                if next == end {
                    return None;
                }
                // (A list kept side by side stays so: the other arm is
                // never taken.)
                let item = match &**long {
                    LongItems::Side { items, .. } => items[*next].clone(),
                    LongItems::Tree(tree) => tree[*next].clone(),
                };
                *next += 1;
                Some(item)
            }
            IntoIterOf::Tree {
                long,
                next,
                end,
                leaf,
            } => match leaf.pop() {
                Some(item) => Some(item),
                None => next_of_long(long, next, *end, leaf),
            },
            IntoIterOf::Ints(ints) => ints.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = match &self.0 {
            IntoIterOf::Items { next, end, .. } => end - next,
            IntoIterOf::Side { next, end, .. } => end - next,
            IntoIterOf::Tree {
                next, end, leaf, ..
            } => leaf.len() + (end - next),
            IntoIterOf::Ints(ints) => ints.left,
        };
        (left, Some(left))
    }
}

/// The element of `long` at `next`, if it is before `end`, with those
/// of its leaf after it put into `leaf`, the last first, if `long` is a
/// tree; `next` is moved past them.
fn next_of_long(long: &Long, next: &mut usize, end: usize, leaf: &mut Vec<Value>) -> Option<Value> {
    if *next == end {
        return None;
    }
    let tree = match &**long {
        LongItems::Side { items, .. } => {
            let item = items[*next].clone();
            *next += 1;
            return Some(item);
        }
        LongItems::Tree(tree) => tree,
    };
    let mut focus = tree.focus();
    let (at, items) = focus.chunk_at(*next);
    let upto = at.end.min(end);
    leaf.extend(
        items[*next - at.start..upto - at.start]
            .iter()
            .rev()
            .cloned(),
    );
    *next = upto;
    leaf.pop()
}

impl IntoIterator for List {
    type Item = Value;
    type IntoIter = IntoIter;

    fn into_iter(self) -> IntoIter {
        let len = self.len();
        IntoIter(match self.0 {
            Stored::Items { buf, start, len } => IntoIterOf::Items {
                buf,
                next: start,
                end: start + len,
            },
            Stored::Other(Other::Long { long, start, len }) => match &*long {
                LongItems::Side { .. } => IntoIterOf::Side {
                    long,
                    next: start,
                    end: start + len,
                },
                LongItems::Tree(_) => IntoIterOf::Tree {
                    long,
                    next: start,
                    end: start + len,
                    leaf: Vec::new(),
                },
            },
            Stored::Other(Other::Ints { lo, .. }) => IntoIterOf::Ints(IntsLeft {
                next: lo,
                left: len,
            }),
        })
    }
}

/// The runs of the elements a list holds ([`List::runs`]).
pub(super) struct Runs<'a> {
    /// What is left of the run at hand.
    left: &'a [Value],
    /// The leaves of the tree the list is kept in, after the run at
    /// hand.
    leaves: Option<Leaves<'a>>,
    /// Where in its storage what is left of the run at hand begins.
    #[cfg(test)]
    at: usize,
}

/// The leaves of a tree, each a run of its elements, from where a list
/// kept in it begins to where it ends.
struct Leaves<'a> {
    leaves: imbl::vector::Chunks<'a, Value, DefaultSharedPtr>,
    /// The elements of the leaves gone by.
    passed: usize,
    /// How many elements of the next leaves come before the list.
    skip: usize,
    /// How many elements of the list the next leaves have.
    left: usize,
}

impl<'a> Leaves<'a> {
    /// The next run of the list, and where in the tree it begins.
    fn next(&mut self) -> Option<(usize, &'a [Value])> {
        while self.left > 0 {
            let leaf = self.leaves.next()?;
            let at = self.passed;
            self.passed += leaf.len();
            if self.skip >= leaf.len() {
                self.skip -= leaf.len();
                continue;
            }
            let from = std::mem::take(&mut self.skip);
            let run = &leaf[from..leaf.len().min(from + self.left)];
            self.left -= run.len();
            return Some((at + from, run));
        }
        None
    }
}

impl Runs<'_> {
    /// Whether no run is left.
    pub(super) fn is_done(&self) -> bool {
        self.left.is_empty() && self.leaves.as_ref().is_none_or(|leaves| leaves.left == 0)
    }
}

impl<'a> Iterator for Runs<'a> {
    type Item = &'a [Value];

    #[inline]
    fn next(&mut self) -> Option<&'a [Value]> {
        if self.left.is_empty() {
            let (_at, run) = self.leaves.as_mut()?.next()?;
            self.left = run;
            #[cfg(test)]
            {
                self.at = _at;
            }
        }
        let take = self.left.len();
        #[cfg(test)]
        let take = {
            let most = cut::MOST.get();
            let taken = take.min(most - self.at % most);
            self.at += taken;
            taken
        };
        let (run, left) = self.left.split_at(take);
        self.left = left;
        Some(run)
    }
}

/// Runs cut short, for the tests of what reads a list by its runs: a
/// list held whole in one buffer has one run, where a list stored in
/// pieces (a tree's leaves) would have several, and two equal lists
/// would have them cut in different places.
#[cfg(test)]
pub(super) mod cut {
    use std::cell::Cell;

    thread_local! {
        /// A run ends where its index in the buffer is a multiple of
        /// this: a list in a buffer of its own and a list that begins
        /// further in a buffer have their runs cut in different places.
        pub(super) static MOST: Cell<usize> = const { Cell::new(usize::MAX) };
    }

    /// `f`, with the runs of every list cut at the multiples of `most`.
    pub(in crate::value) fn runs_at_most<R>(most: usize, f: impl FnOnce() -> R) -> R {
        assert!(most > 0);
        let before = MOST.replace(most);
        let result = f();
        MOST.set(before);
        result
    }
}
