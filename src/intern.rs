//! String interning for identifiers.
//!
//! All identifiers (variable names, function names, field names, type names)
//! are interned at lex time into compact `Symbol` values. Comparisons are
//! O(1) integer ops instead of O(n) string comparisons.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;

/// A compact, copyable handle representing an interned identifier string.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Symbol(u32);

impl fmt::Debug for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Symbol({}: {:?})", self.0, resolve(*self))
    }
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        INTERNER.with(|cell| {
            let interner = cell.borrow();
            write!(f, "{}", interner.strings[self.0 as usize])
        })
    }
}

struct Interner {
    strings: Vec<String>,
    lookup: HashMap<String, Symbol>,
}

impl Interner {
    fn new() -> Self {
        Interner {
            strings: Vec::new(),
            lookup: HashMap::new(),
        }
    }
}

thread_local! {
    static INTERNER: RefCell<Interner> = RefCell::new(Interner::new());
}

/// Intern a string, returning a `Symbol` that can be cheaply compared and hashed.
/// Calling `intern` with the same string always returns the same `Symbol`.
pub fn intern(s: &str) -> Symbol {
    INTERNER.with(|cell| {
        let mut interner = cell.borrow_mut();
        if let Some(&sym) = interner.lookup.get(s) {
            return sym;
        }
        let idx = interner.strings.len() as u32;
        let sym = Symbol(idx);
        interner.strings.push(s.to_owned());
        interner.lookup.insert(s.to_owned(), sym);
        sym
    })
}

/// Resolve a `Symbol` back to its string representation.
pub fn resolve(sym: Symbol) -> String {
    INTERNER.with(|cell| {
        let interner = cell.borrow();
        interner.strings[sym.0 as usize].clone()
    })
}

/// Clear the interner. Call between independent compilation units to prevent
/// unbounded growth (e.g. between REPL evaluations).
pub fn reset() {
    INTERNER.with(|cell| {
        let mut interner = cell.borrow_mut();
        interner.strings.clear();
        interner.lookup.clear();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_string_same_symbol() {
        reset();
        let a = intern("hello");
        let b = intern("hello");
        assert_eq!(a, b);
    }

    #[test]
    fn different_strings_different_symbols() {
        reset();
        let a = intern("foo");
        let b = intern("bar");
        assert_ne!(a, b);
    }

    #[test]
    fn resolve_roundtrip() {
        reset();
        let sym = intern("test_ident");
        assert_eq!(resolve(sym), "test_ident");
    }

    #[test]
    fn display_shows_string() {
        reset();
        let sym = intern("my_var");
        assert_eq!(format!("{sym}"), "my_var");
    }

    #[test]
    fn reset_clears_state() {
        reset();
        let a = intern("x");
        reset();
        let b = intern("y");
        // After reset, "y" gets index 0 (same as "x" had before)
        assert_eq!(a.0, 0);
        assert_eq!(b.0, 0);
    }
}
