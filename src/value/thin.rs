//! A shared slice of values behind one pointer ([`ThinSlice`]): the
//! elements that lists and tuples share ([`super::deep::Buf`]).
//!
//! An `Arc<[Value]>` is two words, the pointer and the length; this is
//! one, the length being kept in the block with the count of holders.
//! That keeps a list in two words and a value in three
//! (`src/value/mod.rs`). It is used inside `src/value` only: no
//! program and no embedder sees it.
//!
//! # The block and its invariants
//!
//! A `ThinSlice` points at a [`Header`] followed, in the same
//! allocation, by `len` initialized values (at `items_offset()`):
//!
//! - The allocation was made by [`alloc`] with [`layout`]`(len)`, or is
//!   the one static empty block ([`EMPTY`]), which is never freed.
//! - `count` is the number of `ThinSlice`s that point at the block
//!   (the static block's count is not kept: it is never freed).
//! - The values are dropped, and the block freed, by the holder that
//!   takes the count from 1 to 0, and by no one else; after that no
//!   `ThinSlice` points at it.
//! - The values are never written after the block is made: a holder
//!   reads them through `&[Value]` only.
//!
//! The counting is that of `std::sync::Arc`: an increment `Relaxed`, a
//! decrement `Release`, and an `Acquire` fence before the values are
//! dropped.

use std::alloc::{Layout, alloc, dealloc, handle_alloc_error};
use std::marker::PhantomData;
use std::ops::Deref;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicUsize, Ordering, fence};

use super::Value;

/// What a block starts with.
#[repr(C)]
struct Header {
    count: AtomicUsize,
    len: usize,
}

/// The one block of no values, shared by every empty `ThinSlice`. Its
/// count is never touched (a clone or a drop of an empty slice looks
/// at the pointer first): it is never freed.
static EMPTY: Header = Header {
    count: AtomicUsize::new(1),
    len: 0,
};

/// A count beyond this aborts, as `Arc`'s does: a count that wraps
/// would free a block that is still held.
const MAX_COUNT: usize = isize::MAX as usize;

/// The most values an iterator's exact size hint is taken at its word
/// for: a block of that many is made before its values come. A hint of
/// more is not trusted (an iterator may say what it likes): its values
/// are gathered first, so what is allocated is what was given.
const HINT_MOST: usize = 1024;

/// `made` with the values of `items` after it, in a vector that grows as
/// they come: their size hint is not trusted (`collect` and `extend` make
/// room for as many as it says first).
fn gathered(mut made: Vec<Value>, items: impl Iterator<Item = Value>) -> Vec<Value> {
    for item in items {
        made.push(item);
    }
    made
}

/// The layout of a block of `len` values, and where its values start.
fn layout(len: usize) -> (Layout, usize) {
    let items = Layout::array::<Value>(len).expect("a block of values fits the address space");
    let (layout, offset) = Layout::new::<Header>()
        .extend(items)
        .expect("a block of values fits the address space");
    (layout.pad_to_align(), offset)
}

/// Where the values of any block start (the same for every length).
fn items_offset() -> usize {
    layout(0).1
}

/// A block of `len` values, its count 1 and its values not yet written.
fn alloc_block(len: usize) -> NonNull<Header> {
    let (layout, _) = layout(len);
    // SAFETY: the layout is not zero-sized (it holds a `Header`).
    let raw = unsafe { alloc(layout) }.cast::<Header>();
    let Some(block) = NonNull::new(raw) else {
        handle_alloc_error(layout)
    };
    // SAFETY: `block` is fresh, aligned for `Header`, and large enough.
    unsafe {
        block.as_ptr().write(Header {
            count: AtomicUsize::new(1),
            len,
        })
    };
    block
}

/// The first value of the block `block`.
///
/// # Safety
/// `block` points at a block (see the module's invariants).
unsafe fn first_item(block: NonNull<Header>) -> *mut Value {
    // SAFETY: the offset is within the block's allocation.
    unsafe {
        block
            .as_ptr()
            .cast::<u8>()
            .add(items_offset())
            .cast::<Value>()
    }
}

/// A shared slice of values: one pointer. See the module's invariants.
pub(crate) struct ThinSlice {
    block: NonNull<Header>,
    _owns: PhantomData<Value>,
}

// SAFETY: a `ThinSlice` is shared as an `Arc<[Value]>` is: the values
// are only read through `&[Value]`, and the count is atomic. So it may
// go to, and be shared between, threads as an `Arc<[Value]>` may, which
// needs values that are `Send + Sync` (checked here).
unsafe impl Send for ThinSlice {}
// SAFETY: as for `Send`.
unsafe impl Sync for ThinSlice {}
const _: () = {
    const fn shared_between_threads<T: Send + Sync>() {}
    shared_between_threads::<Value>()
};

impl ThinSlice {
    /// The empty slice (the static block: nothing is allocated).
    pub(crate) fn empty() -> ThinSlice {
        ThinSlice {
            block: NonNull::from(&EMPTY),
            _owns: PhantomData,
        }
    }

    /// Whether this is the static empty block, which is not counted.
    #[inline]
    fn is_static(&self) -> bool {
        ptr::eq(self.block.as_ptr(), &EMPTY)
    }

    fn header(&self) -> &Header {
        // SAFETY: `block` points at a block, whose header lives as long
        // as a `ThinSlice` points at it.
        unsafe { self.block.as_ref() }
    }

    /// A slice of exactly `len` values, written from `items`; a slice of
    /// those `items` gives if it gives fewer or more. A block of `len`
    /// values is made first: `len` is at most [`HINT_MOST`], or the
    /// number of values a vector holds.
    fn from_exact(len: usize, mut items: impl Iterator<Item = Value>) -> ThinSlice {
        if len == 0 {
            return match items.next() {
                None => ThinSlice::empty(),
                Some(first) => ThinSlice::from(gathered(vec![first], items)),
            };
        }
        /// The block being written: if a value's making panics, the
        /// values written so far are dropped and the block freed.
        struct Writing {
            block: NonNull<Header>,
            written: usize,
        }
        impl Drop for Writing {
            fn drop(&mut self) {
                // SAFETY: the first `written` values are initialized,
                // and nobody else knows the block.
                unsafe {
                    let first = first_item(self.block);
                    ptr::drop_in_place(ptr::slice_from_raw_parts_mut(first, self.written));
                    let len = self.block.as_ref().len;
                    dealloc(self.block.as_ptr().cast(), layout(len).0);
                }
            }
        }
        let mut writing = Writing {
            block: alloc_block(len),
            written: 0,
        };
        // SAFETY: the block has room for `len` values.
        let first = unsafe { first_item(writing.block) };
        while writing.written < len {
            let Some(item) = items.next() else {
                // Fewer than promised: the ones written, through a vector.
                // (The guard is told first that it holds none of them:
                // were the reading to unwind, they would be leaked, never
                // dropped twice.)
                let written = std::mem::replace(&mut writing.written, 0);
                let made: Vec<Value> = (0..written)
                    // SAFETY: each of these was written, and is read once.
                    .map(|i| unsafe { first.add(i).read() })
                    .collect();
                return ThinSlice::from(made);
            };
            // SAFETY: within the block, not yet written.
            unsafe { first.add(writing.written).write(item) };
            writing.written += 1;
        }
        if let Some(more) = items.next() {
            // More than promised: all of them, through a vector (the
            // guard told first, as above).
            writing.written = 0;
            let mut made: Vec<Value> = (0..len)
                // SAFETY: as above.
                .map(|i| unsafe { first.add(i).read() })
                .collect();
            made.push(more);
            return ThinSlice::from(gathered(made, items));
        }
        let block = writing.block;
        std::mem::forget(writing);
        ThinSlice {
            block,
            _owns: PhantomData,
        }
    }
}

impl From<Vec<Value>> for ThinSlice {
    fn from(items: Vec<Value>) -> ThinSlice {
        let len = items.len();
        ThinSlice::from_exact(len, items.into_iter())
    }
}

impl FromIterator<Value> for ThinSlice {
    fn from_iter<I: IntoIterator<Item = Value>>(items: I) -> ThinSlice {
        let items = items.into_iter();
        match items.size_hint() {
            (least, Some(most)) if least == most && most <= HINT_MOST => {
                ThinSlice::from_exact(least, items)
            }
            (least, _) => {
                let made = Vec::with_capacity(least.min(HINT_MOST));
                ThinSlice::from(gathered(made, items))
            }
        }
    }
}

impl Default for ThinSlice {
    fn default() -> ThinSlice {
        ThinSlice::empty()
    }
}

impl Clone for ThinSlice {
    #[inline]
    fn clone(&self) -> ThinSlice {
        if !self.is_static() && self.header().count.fetch_add(1, Ordering::Relaxed) > MAX_COUNT {
            std::process::abort();
        }
        ThinSlice {
            block: self.block,
            _owns: PhantomData,
        }
    }
}

impl Drop for ThinSlice {
    #[inline]
    fn drop(&mut self) {
        if self.is_static() || self.header().count.fetch_sub(1, Ordering::Release) != 1 {
            return;
        }
        self.drop_last();
    }
}

impl ThinSlice {
    /// The last holder drops the values and frees the block. (Out of
    /// line, as `Arc`'s is: inlined, it made the drop of every value,
    /// an Int's too, save and restore six registers.)
    #[cold]
    #[inline(never)]
    fn drop_last(&mut self) {
        fence(Ordering::Acquire);
        // SAFETY: this was the last holder: nobody else reads the
        // values, which are initialized, and the block was allocated
        // with `layout(len)`.
        unsafe {
            let len = self.header().len;
            ptr::drop_in_place(ptr::slice_from_raw_parts_mut(first_item(self.block), len));
            dealloc(self.block.as_ptr().cast(), layout(len).0);
        }
    }
}

impl Deref for ThinSlice {
    type Target = [Value];

    #[inline]
    fn deref(&self) -> &[Value] {
        let len = self.header().len;
        // SAFETY: the block holds `len` initialized values, which live
        // as long as `self` and are not written.
        unsafe { std::slice::from_raw_parts(first_item(self.block), len) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn a_slice_is_one_pointer() {
        assert_eq!(
            std::mem::size_of::<ThinSlice>(),
            std::mem::size_of::<usize>()
        );
        assert_eq!(
            std::mem::size_of::<Option<ThinSlice>>(),
            std::mem::size_of::<usize>()
        );
    }

    #[test]
    fn the_values_are_kept_and_shared() {
        let made = ThinSlice::from(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        let shared = made.clone();
        assert_eq!(made.len(), 3);
        assert!(matches!(shared[2], Value::Int(3)));
        assert!(ptr::eq(made.as_ptr(), shared.as_ptr()));
        drop(made);
        assert!(matches!(shared[0], Value::Int(1)));
    }

    #[test]
    fn the_last_holder_drops_the_values_once() {
        let marker = Arc::new(Vec::<u8>::new());
        let item = || Value::Bytes(marker.clone());
        let made: ThinSlice = (0..4).map(|_| item()).collect();
        assert_eq!(Arc::strong_count(&marker), 5);
        let copies: Vec<ThinSlice> = (0..3).map(|_| made.clone()).collect();
        drop(made);
        assert_eq!(Arc::strong_count(&marker), 5);
        drop(copies);
        assert_eq!(Arc::strong_count(&marker), 1);
    }

    #[test]
    fn an_empty_slice_allocates_nothing() {
        let a = ThinSlice::empty();
        let b: ThinSlice = std::iter::empty().collect();
        let c = ThinSlice::from(Vec::new());
        assert!(a.is_empty() && b.is_empty() && c.is_empty());
        assert!(ptr::eq(a.block.as_ptr(), &EMPTY) && ptr::eq(c.block.as_ptr(), &EMPTY));
        let many: Vec<ThinSlice> = (0..100).map(|_| a.clone()).collect();
        drop(many);
        drop((a, b, c));
        assert_eq!(EMPTY.count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_wrong_size_hint_still_gives_every_value() {
        /// Says it gives `said` values and gives `gives`.
        struct Lying {
            said: usize,
            gives: std::ops::Range<i64>,
        }
        impl Iterator for Lying {
            type Item = Value;
            fn next(&mut self) -> Option<Value> {
                self.gives.next().map(Value::Int)
            }
            fn size_hint(&self) -> (usize, Option<usize>) {
                (self.said, Some(self.said))
            }
        }
        for (said, gives) in [(5, 0..3), (2, 0..6), (0, 0..2), (3, 0..0)] {
            let n = gives.end;
            let made: ThinSlice = Lying { said, gives }.collect();
            let ints: Vec<i64> = made
                .iter()
                .map(|v| match v {
                    Value::Int(i) => *i,
                    _ => -1,
                })
                .collect();
            assert_eq!(ints, (0..n).collect::<Vec<_>>(), "said {said}");
        }
    }

    #[test]
    fn a_panic_while_writing_drops_what_was_written() {
        let marker = Arc::new(Vec::<u8>::new());
        let m = marker.clone();
        let made = std::panic::catch_unwind(move || {
            let items = (0..4).map(move |i| match i {
                3 => panic!("the fourth value"),
                _ => Value::Bytes(m.clone()),
            });
            let exact = ExactLen(items, 4);
            exact.collect::<ThinSlice>()
        });
        assert!(made.is_err());
        assert_eq!(Arc::strong_count(&marker), 1);
    }

    /// An iterator with an exact size hint of `.1`.
    struct ExactLen<I>(I, usize);
    impl<I: Iterator<Item = Value>> Iterator for ExactLen<I> {
        type Item = Value;
        fn next(&mut self) -> Option<Value> {
            self.0.next()
        }
        fn size_hint(&self) -> (usize, Option<usize>) {
            (self.1, Some(self.1))
        }
    }

    #[test]
    fn slices_go_between_threads() {
        let made: ThinSlice = (0..1000).map(Value::Int).collect();
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let mine = made.clone();
                std::thread::spawn(move || {
                    let copies: Vec<ThinSlice> = (0..1000).map(|_| mine.clone()).collect();
                    copies.len() + mine.len()
                })
            })
            .collect();
        for t in threads {
            assert_eq!(t.join().unwrap(), 2000);
        }
        assert_eq!(made.len(), 1000);
    }

    #[test]
    fn a_panic_after_more_than_promised_drops_every_value_once() {
        let marker = Arc::new(Vec::<u8>::new());
        let m = marker.clone();
        let made = std::panic::catch_unwind(move || {
            let items = (0..6).map(move |i| match i {
                5 => panic!("the sixth value"),
                _ => Value::Bytes(m.clone()),
            });
            ExactLen(items, 2).collect::<ThinSlice>()
        });
        assert!(made.is_err());
        assert_eq!(Arc::strong_count(&marker), 1);
    }

    #[test]
    fn fewer_than_promised_keeps_each_value_once() {
        let marker = Arc::new(Vec::<u8>::new());
        let m = marker.clone();
        let items = (0..2).map(move |_| Value::Bytes(m.clone()));
        let made = ExactLen(items, 7).collect::<ThinSlice>();
        assert_eq!(made.len(), 2);
        assert_eq!(Arc::strong_count(&marker), 3);
        drop(made);
        assert_eq!(Arc::strong_count(&marker), 1);
    }

    #[test]
    fn a_panic_on_the_first_value_frees_the_block() {
        let made = std::panic::catch_unwind(|| {
            ExactLen((0..3).map(|_| -> Value { panic!("the first value") }), 3)
                .collect::<ThinSlice>()
        });
        assert!(made.is_err());
    }

    #[test]
    fn a_huge_size_hint_allocates_only_what_is_given() {
        let none = ExactLen(std::iter::empty(), usize::MAX / 8).collect::<ThinSlice>();
        assert!(none.is_empty());
        let three = ExactLen((0..3).map(Value::Int), usize::MAX).collect::<ThinSlice>();
        assert_eq!(three.len(), 3);
        // One past the hint that is trusted, and told truly.
        let many =
            ExactLen((0..=HINT_MOST as i64).map(Value::Int), HINT_MOST + 1).collect::<ThinSlice>();
        assert_eq!(many.len(), HINT_MOST + 1);
        assert!(matches!(many[HINT_MOST], Value::Int(n) if n == HINT_MOST as i64));
    }

    #[test]
    fn the_last_holder_may_be_on_another_thread() {
        for _ in 0..20 {
            let marker = Arc::new(Vec::<u8>::new());
            let made: ThinSlice = (0..8).map(|_| Value::Bytes(marker.clone())).collect();
            let threads: Vec<_> = (0..3)
                .map(|k| {
                    let mine = made.clone();
                    std::thread::spawn(move || {
                        let copy = mine.clone();
                        let len = copy.len() + k;
                        drop(mine);
                        drop(copy);
                        len
                    })
                })
                .collect();
            drop(made);
            for t in threads {
                t.join().unwrap();
            }
            assert_eq!(Arc::strong_count(&marker), 1);
        }
    }

    #[test]
    fn empty_slices_go_between_threads() {
        let threads: Vec<_> = (0..3)
            .map(|_| {
                std::thread::spawn(|| {
                    let empty = ThinSlice::empty();
                    let copies: Vec<ThinSlice> = (0..50).map(|_| empty.clone()).collect();
                    copies.iter().map(|s| s.len()).sum::<usize>()
                })
            })
            .collect();
        for t in threads {
            assert_eq!(t.join().unwrap(), 0);
        }
        assert_eq!(EMPTY.count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_deep_nest_of_lists_is_dropped() {
        // Deeper than the holders dropped each inside the one before: the
        // buffers further in are put aside (`src/value/deep.rs`).
        let mut nest = Value::list(vec![Value::Int(1)]);
        for _ in 0..300 {
            nest = Value::list(vec![nest.clone(), Value::Int(2)]);
        }
        let copy = nest.clone();
        drop(nest);
        drop(copy);
    }
}
