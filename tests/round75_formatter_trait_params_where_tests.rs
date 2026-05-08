//! Round 75 regression lock: formatter dropped trait/impl parametric
//! pieces.
//!
//! `format_trait_with_comments` did not render `TraitDecl::params` or
//! `TraitDecl::param_where_clauses`, and
//! `format_trait_impl_with_comments` did not render `TraitImpl::trait_args`
//! or `TraitImpl::where_clauses`. So a parametric trait declaration like
//! `trait HashTable(k) { ... }` round-tripped to `trait HashTable { ... }`
//! and a parametric impl like `trait TryInto(Int) for String { ... }`
//! lost its `(Int)` arg, silently changing the program's meaning across
//! `silt fmt` (and breaking any subsequent typecheck).
//!
//! These tests pin all four dropped fields and assert formatter
//! idempotency (parse → format → parse → format = first format).
//! Each test exercises the actual code path: parses real silt source,
//! runs the public `format` API, asserts the literal pieces are present,
//! and then re-formats the result to lock idempotency.

use silt::formatter::format;
use silt::lexer::Lexer;
use silt::parser::Parser;

/// Format-then-reformat — asserts the formatter's output reparses cleanly
/// AND that a second formatting pass is a no-op (idempotency).
fn format_idempotent(src: &str) -> String {
    let first = format(src).unwrap_or_else(|e| panic!("first format failed: {e:?}\nsrc:\n{src}"));
    // Second pass: reparse and re-format. This is the round-trip lock —
    // a regression that drops fields would re-emerge as a divergent
    // second pass, but only if the formatter is asked to round-trip.
    let _tokens = Lexer::new(&first)
        .tokenize()
        .unwrap_or_else(|e| panic!("formatted output failed to lex: {e:?}\nfirst:\n{first}"));
    let second =
        format(&first).unwrap_or_else(|e| panic!("second format failed: {e:?}\nfirst:\n{first}"));
    assert_eq!(
        first, second,
        "formatter must be idempotent\n--- first ---\n{first}\n--- second ---\n{second}"
    );
    first
}

/// BROKEN-A: `TraitDecl::params` was dropped — `trait HashTable(k)`
/// round-tripped to `trait HashTable`.
#[test]
fn trait_decl_params_round_trip() {
    let src = "trait HashTable(k) {\n    fn get(self, key: k) -> Maybe(k)\n}\n";
    let formatted = format_idempotent(src);
    assert!(
        formatted.contains("trait HashTable(k)"),
        "trait params dropped — expected `trait HashTable(k)`, got:\n{formatted}"
    );
    // Parameter must appear on the trait header, not just inside the
    // method signature. Asserting both pins the regression precisely.
    assert!(
        formatted.contains("key: k"),
        "method body lost parameter k:\n{formatted}"
    );
}

/// BROKEN-B: `TraitDecl::param_where_clauses` was dropped —
/// `trait Foo(a) where a: Display { ... }` round-tripped to
/// `trait Foo { ... }` (params AND where both gone).
#[test]
fn trait_decl_params_and_where_round_trip() {
    let src = "trait Display { fn show(self) -> String }\n\
               trait Foo(a) where a: Display {\n    fn x(self)\n}\n";
    let formatted = format_idempotent(src);
    assert!(
        formatted.contains("trait Foo(a)"),
        "trait params dropped from Foo:\n{formatted}"
    );
    assert!(
        formatted.contains("where a: Display"),
        "trait param-where dropped from Foo:\n{formatted}"
    );
}

/// BROKEN-B (multi-trait `+`): the where-clause renderer must group
/// multi-trait bounds on the same type-var via `+`, matching FnDecl
/// behavior. Locks the grouping shape.
#[test]
fn trait_decl_where_multi_bound_grouping() {
    let src = "trait Display { fn show(self) -> String }\n\
               trait Hash { fn hash(self) -> Int }\n\
               trait Foo(a) where a: Display + Hash {\n    fn x(self)\n}\n";
    let formatted = format_idempotent(src);
    assert!(
        formatted.contains("trait Foo(a) where a: Display + Hash"),
        "multi-trait where-bounds not grouped via `+`:\n{formatted}"
    );
}

/// BROKEN-C: `TraitImpl::trait_args` was dropped —
/// `trait TryInto(Int) for String { ... }` round-tripped to
/// `trait TryInto for String { ... }`.
#[test]
fn trait_impl_trait_args_round_trip() {
    let src = "trait TryInto(b) { fn try_into(self) -> Maybe(b) }\n\
               trait TryInto(Int) for String {\n    fn try_into(self) -> Maybe(Int) = None\n}\n";
    let formatted = format_idempotent(src);
    assert!(
        formatted.contains("trait TryInto(Int) for String"),
        "trait_args dropped from impl header:\n{formatted}"
    );
}

/// BROKEN-D: `TraitImpl::where_clauses` was dropped —
/// `trait Greet for Box(a) where a: Greet { ... }` round-tripped to
/// `trait Greet for Box(a) { ... }`.
#[test]
fn trait_impl_where_round_trip() {
    let src = "trait Greet { fn greet(self) -> String }\n\
               type Box(a) = { value: a }\n\
               trait Greet for Box(a) where a: Greet {\n    fn greet(self) -> String = self.value.greet()\n}\n";
    let formatted = format_idempotent(src);
    assert!(
        formatted.contains("trait Greet for Box(a) where a: Greet"),
        "impl where-clause dropped:\n{formatted}"
    );
}

/// Combined: trait_args AND where_clauses on the same impl. Pins their
/// joint placement (`(args) for Target where ...`).
#[test]
fn trait_impl_trait_args_and_where_round_trip() {
    let src = "trait Conv(to) { fn conv(self) -> to }\n\
               trait Conv(Int) for List(a) where a: Conv(Int) {\n    fn conv(self) -> Int = 0\n}\n";
    let formatted = format_idempotent(src);
    assert!(
        formatted.contains("trait Conv(Int) for List(a) where a: Conv(Int)"),
        "combined trait_args + where dropped or reordered:\n{formatted}"
    );
}

/// Round-trip parse → format → parse — locks that the formatted output
/// is itself parseable (the round-trip-parse half of the round-trip
/// requirement). Pre-fix this would be true (the formatter just dropped
/// fields without producing invalid syntax) but a future regression that
/// produces malformed output would surface here.
#[test]
fn trait_decl_and_impl_format_output_reparses() {
    let src = "trait Hash { fn hash(self) -> Int }\n\
               trait HashTable(k) where k: Hash {\n    fn get(self, key: k) -> Maybe(k)\n}\n\
               trait HashTable(Int) for List(a) where a: Hash {\n    fn get(self, key: Int) -> Maybe(Int) = None\n}\n";
    let formatted = format(src).expect("first format");
    // Re-tokenize and re-parse — the round-trip-parse leg.
    let toks = Lexer::new(&formatted).tokenize().unwrap_or_else(|e| {
        panic!("formatted output failed to lex: {e:?}\nformatted:\n{formatted}")
    });
    Parser::new(toks).parse_program().unwrap_or_else(|e| {
        panic!("formatted output failed to parse: {e:?}\nformatted:\n{formatted}")
    });
    // And idempotency on top of round-trip-parse.
    let second = format(&formatted).expect("second format");
    assert_eq!(
        formatted, second,
        "format(format(src)) must equal format(src)\n--- 1 ---\n{formatted}\n--- 2 ---\n{second}"
    );
}
