//! String interning for identifiers.
//!
//! All identifiers (variable names, function names, field names, type names)
//! are interned at lex time into compact `Symbol` values. Comparisons are
//! O(1) integer ops instead of O(n) string comparisons.
//!
//! The interner is one for the whole process: a `Symbol` names the same
//! string on every thread, so a session, a syntax tree or an analysis
//! made on one thread is read right on another. Its strings are kept for
//! as long as the process runs (a program's names are few, and an
//! editor's grow only with the names written).

use std::collections::HashMap;
use std::fmt;
use std::sync::{Mutex, OnceLock};

/// A compact, copyable handle representing an interned identifier string.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Symbol(u32);

impl fmt::Debug for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Symbol({}: {:?})", self.0, text(*self))
    }
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(text(*self))
    }
}

/// How many strings the first block holds; each next block holds twice
/// as many as the one before.
const FIRST_BLOCK: usize = 256;

/// Blocks enough for every `u32` index.
const BLOCKS: usize = 25;

/// The strings, by symbol: in blocks that are made as they are needed
/// and never move, so a string is read without a lock.
struct Strings {
    blocks: [OnceLock<Box<[OnceLock<&'static str>]>>; BLOCKS],
}

/// The block and the place in it of the symbol of index `index`.
fn place(index: usize) -> (usize, usize) {
    // Block k holds FIRST_BLOCK * 2^k strings and starts at
    // FIRST_BLOCK * (2^k - 1).
    let k = (index / FIRST_BLOCK + 1).ilog2() as usize;
    (k, index - FIRST_BLOCK * ((1 << k) - 1))
}

fn strings() -> &'static Strings {
    static STRINGS: OnceLock<Strings> = OnceLock::new();
    STRINGS.get_or_init(|| Strings {
        blocks: std::array::from_fn(|_| OnceLock::new()),
    })
}

/// The symbols, by string: what interning looks up, under a lock.
fn symbols() -> &'static Mutex<HashMap<&'static str, Symbol>> {
    static SYMBOLS: OnceLock<Mutex<HashMap<&'static str, Symbol>>> = OnceLock::new();
    SYMBOLS.get_or_init(Default::default)
}

/// Intern a string, returning a `Symbol` that can be cheaply compared and hashed.
/// Calling `intern` with the same string always returns the same `Symbol`,
/// on every thread.
pub fn intern(s: &str) -> Symbol {
    let mut symbols = symbols().lock().unwrap_or_else(|e| e.into_inner());
    match symbols.get(s) {
        Some(sym) => *sym,
        None => {
            let index = symbols.len();
            let sym = Symbol(u32::try_from(index).expect("fewer than 2^32 names"));
            let text: &'static str = Box::leak(s.to_owned().into_boxed_str());
            let (block, at) = place(index);
            let block = strings().blocks[block]
                .get_or_init(|| (0..FIRST_BLOCK << block).map(|_| OnceLock::new()).collect());
            // (Set before the symbol is given out: whoever holds it finds
            // its string.)
            let _ = block[at].set(text);
            symbols.insert(text, sym);
            sym
        }
    }
}

/// The string of the symbol `sym`.
fn text(sym: Symbol) -> &'static str {
    let (block, at) = place(sym.0 as usize);
    (strings().blocks[block].get())
        .and_then(|block| block[at].get())
        .expect("a symbol is made by `intern`, which sets its string")
}

/// Resolve a `Symbol` back to its string representation.
pub fn resolve(sym: Symbol) -> String {
    text(sym).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_string_same_symbol() {
        let a = intern("hello");
        let b = intern("hello");
        assert_eq!(a, b);
    }

    #[test]
    fn different_strings_different_symbols() {
        let a = intern("foo");
        let b = intern("bar");
        assert_ne!(a, b);
    }

    #[test]
    fn resolve_roundtrip() {
        let sym = intern("test_ident");
        assert_eq!(resolve(sym), "test_ident");
    }

    #[test]
    fn display_shows_string() {
        let sym = intern("my_var");
        assert_eq!(format!("{sym}"), "my_var");
    }

    /// The blocks hold every index once, in order.
    #[test]
    fn the_places_of_the_indexes_follow_each_other() {
        let mut expected = (0, 0);
        for index in 0..FIRST_BLOCK * 20 {
            assert_eq!(place(index), expected, "index {index}");
            expected.1 += 1;
            if expected.1 == FIRST_BLOCK << expected.0 {
                expected = (expected.0 + 1, 0);
            }
        }
        assert!(place(u32::MAX as usize).0 < BLOCKS);
    }

    /// A symbol names one string on every thread: one made here is read
    /// there, and one made there is the one made here for that string
    /// (AR-1: the interner was per thread, and a symbol moved to another
    /// thread read another name, or none).
    #[test]
    fn a_symbol_is_one_string_on_every_thread() {
        let here = intern("made_on_the_first_thread");
        let (there, read) = std::thread::spawn(move || {
            // (Another thread's own names first: a per-thread table
            // would give the next one this index.)
            for n in 0..100 {
                intern(&format!("of_the_second_thread_{n}"));
            }
            (intern("made_on_the_second_thread"), resolve(here))
        })
        .join()
        .unwrap();
        assert_eq!(read, "made_on_the_first_thread");
        assert_eq!(resolve(there), "made_on_the_second_thread");
        assert_eq!(intern("made_on_the_second_thread"), there);
    }

    /// Threads that intern the same names at once each get one symbol
    /// per name.
    #[test]
    fn threads_interning_at_once_agree() {
        let names: Vec<String> = (0..2_000).map(|n| format!("shared_name_{n}")).collect();
        let runs: Vec<Vec<Symbol>> = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..4)
                .map(|_| scope.spawn(|| names.iter().map(|name| intern(name)).collect()))
                .collect();
            workers.into_iter().map(|w| w.join().unwrap()).collect()
        });
        for run in &runs[1..] {
            assert_eq!(run, &runs[0]);
        }
        for (name, sym) in names.iter().zip(&runs[0]) {
            assert_eq!(&resolve(*sym), name);
        }
    }
}
