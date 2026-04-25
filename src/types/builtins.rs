//! Authoritative source-of-truth list of silt's built-in container and
//! primitive type names.
//!
//! Hand-rolled mirror lists (typechecker arity tables, LSP rename guard,
//! editor-grammar tests, etc.) historically drifted whenever a new
//! built-in type was added. This module is the single place to add or
//! remove a built-in type name; every consumer derives from
//! [`BUILTIN_TYPES`] (or, for editor-grammar text files, is parity-locked
//! against it via `tests/builtin_types_authoritative_parity_tests.rs`).
//!
//! ## Adding a new built-in type
//!
//! 1. Add an entry to [`BUILTIN_TYPES`] below.
//! 2. Add the surface name to both editor grammars:
//!    - `editors/vim/syntax/silt.vim` (siltType keyword line)
//!    - `editors/vscode/syntaxes/silt.tmLanguage.json` (`primitives` regex)
//! 3. Run `cargo test`. The parity-lock test asserts presence in both
//!    grammars. Every other consumer (typechecker arity check, LSP
//!    rename guard, editor-grammar primitive list test) auto-derives
//!    from this list.
//!
//! ## Arity semantics
//!
//! [`BuiltinType::arity`] is `Some(n)` when the type takes a fixed
//! number of arguments (e.g. `List(a)` → `Some(1)`, `Map(k, v)` →
//! `Some(2)`), `Some(0)` for primitives that take no parameters
//! (`Int`, `Bool`, etc.), and `None` for variadic / unspecified
//! shapes (`Tuple`, `Fn`, `Fun`, `Handle`). The typechecker only
//! consults `arity` when validating fixed-arity trait-impl targets;
//! the variadic shapes do not yet participate in that check.

/// Classification of a built-in type name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinKind {
    /// Scalar / nullary type with no parameters: `Int`, `Float`,
    /// `ExtFloat`, `Bool`, `String`, `Unit`, and the surface alias `()`.
    Primitive,
    /// Parameterized container, callable, or resource type: `List`,
    /// `Range`, `Map`, `Set`, `Channel`, `Tuple`, `Fn`, `Fun`, `Handle`.
    Container,
}

/// A single authoritative built-in type entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinType {
    /// Surface name as written in type-annotation position (`Int`,
    /// `List`, `()`, etc.).
    pub name: &'static str,
    /// Number of type arguments. `Some(n)` for fixed-arity entries
    /// (primitives use `Some(0)`); `None` for variadic shapes
    /// (`Tuple`, `Fn`, `Fun`, `Handle`).
    pub arity: Option<u32>,
    /// Whether this entry is a scalar primitive or a parameterized
    /// container.
    pub kind: BuiltinKind,
}

/// Single ordered authoritative list of every built-in type name.
///
/// Order: primitives first (matching the typechecker's `is_primitive`
/// match arm at `src/typechecker/mod.rs::check_trait_impl`), then
/// containers (matching the `is_builtin_container` match arm at the
/// same site). The `()` surface alias for `Unit` is kept as a separate
/// entry with the same arity/kind so a single iterator covers every
/// surface form a user might write.
pub static BUILTIN_TYPES: &[BuiltinType] = &[
    // ── Primitives ─────────────────────────────────────────────────
    BuiltinType { name: "Int",      arity: Some(0), kind: BuiltinKind::Primitive },
    BuiltinType { name: "Float",    arity: Some(0), kind: BuiltinKind::Primitive },
    BuiltinType { name: "ExtFloat", arity: Some(0), kind: BuiltinKind::Primitive },
    BuiltinType { name: "Bool",     arity: Some(0), kind: BuiltinKind::Primitive },
    BuiltinType { name: "String",   arity: Some(0), kind: BuiltinKind::Primitive },
    BuiltinType { name: "Unit",     arity: Some(0), kind: BuiltinKind::Primitive },
    BuiltinType { name: "()",       arity: Some(0), kind: BuiltinKind::Primitive },
    // ── Containers / callables / resources ─────────────────────────
    BuiltinType { name: "List",    arity: Some(1), kind: BuiltinKind::Container },
    BuiltinType { name: "Range",   arity: Some(1), kind: BuiltinKind::Container },
    BuiltinType { name: "Map",     arity: Some(2), kind: BuiltinKind::Container },
    BuiltinType { name: "Set",     arity: Some(1), kind: BuiltinKind::Container },
    BuiltinType { name: "Channel", arity: Some(1), kind: BuiltinKind::Container },
    BuiltinType { name: "Tuple",   arity: None,    kind: BuiltinKind::Container },
    BuiltinType { name: "Fn",      arity: None,    kind: BuiltinKind::Container },
    BuiltinType { name: "Fun",     arity: None,    kind: BuiltinKind::Container },
    BuiltinType { name: "Handle",  arity: None,    kind: BuiltinKind::Container },
];

/// Look up a built-in type entry by surface name. Returns `None` for
/// names not in [`BUILTIN_TYPES`] (e.g. user-declared records, type
/// variables, unrecognised text).
pub fn lookup(name: &str) -> Option<&'static BuiltinType> {
    BUILTIN_TYPES.iter().find(|b| b.name == name)
}

/// Iterate every primitive entry in [`BUILTIN_TYPES`] (preserves the
/// authoritative order).
pub fn iter_primitives() -> impl Iterator<Item = &'static BuiltinType> {
    BUILTIN_TYPES.iter().filter(|b| b.kind == BuiltinKind::Primitive)
}

/// Iterate every container entry in [`BUILTIN_TYPES`] (preserves the
/// authoritative order).
pub fn iter_containers() -> impl Iterator<Item = &'static BuiltinType> {
    BUILTIN_TYPES.iter().filter(|b| b.kind == BuiltinKind::Container)
}

/// Iterate every authoritative entry, primitives then containers, in
/// the order declared by [`BUILTIN_TYPES`].
pub fn iter_all() -> impl Iterator<Item = &'static BuiltinType> {
    BUILTIN_TYPES.iter()
}

/// Convenience: is `name` a built-in primitive surface name?
pub fn is_primitive(name: &str) -> bool {
    matches!(lookup(name), Some(b) if b.kind == BuiltinKind::Primitive)
}

/// Convenience: is `name` a built-in container surface name?
pub fn is_container(name: &str) -> bool {
    matches!(lookup(name), Some(b) if b.kind == BuiltinKind::Container)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_finds_known_primitives() {
        assert_eq!(lookup("Int").map(|b| b.kind), Some(BuiltinKind::Primitive));
        assert_eq!(lookup("Bool").map(|b| b.kind), Some(BuiltinKind::Primitive));
        assert_eq!(lookup("Unit").map(|b| b.kind), Some(BuiltinKind::Primitive));
        assert_eq!(lookup("()").map(|b| b.kind), Some(BuiltinKind::Primitive));
    }

    #[test]
    fn lookup_finds_known_containers() {
        assert_eq!(lookup("List").map(|b| b.arity), Some(Some(1)));
        assert_eq!(lookup("Map").map(|b| b.arity), Some(Some(2)));
        assert_eq!(lookup("Fn").map(|b| b.arity), Some(None));
    }

    #[test]
    fn lookup_unknown_returns_none() {
        assert!(lookup("NotABuiltin").is_none());
        assert!(lookup("").is_none());
        assert!(lookup("int").is_none()); // case-sensitive
    }

    #[test]
    fn iterators_partition_authoritative_set() {
        let prim_count = iter_primitives().count();
        let cont_count = iter_containers().count();
        let all_count = iter_all().count();
        assert_eq!(prim_count + cont_count, all_count);
        assert!(prim_count > 0);
        assert!(cont_count > 0);
    }

    #[test]
    fn no_duplicate_names() {
        let mut names: Vec<&str> = BUILTIN_TYPES.iter().map(|b| b.name).collect();
        names.sort();
        let unique = names.len();
        names.dedup();
        assert_eq!(unique, names.len(), "duplicate name in BUILTIN_TYPES");
    }
}
