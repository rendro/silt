//! Regression tests for formatter idempotency (fmt(fmt(x)) == fmt(x)).
//!
//! The fuzzer (round 29) found an input where formatting twice produced a
//! different output than formatting once — a blank line appeared between
//! adjacent imports on the second pass that wasn't there on the first.
//!
//! Root cause: when the import sorter moved a mid-list import (one whose
//! preceding comments were stored in `buckets[i]`, i > 0, and emitted as
//! an attached `comment_block` without a blank-line separator) to the
//! first slot after alphabetical sort, the output looked like
//! `comment\nimport_a\n...`. On a second pass, those same comments landed
//! in `buckets[0]` (the "pre-first-decl" bucket) which inserts a blank
//! line before the imports. The fix promotes the first sorted import's
//! `comment_block` into the header-block emission so both passes produce
//! the same output.

use silt::formatter::format;
use silt::lexer::Lexer;
use silt::parser::Parser;

fn assert_formatted_parses(source: &str) {
    let formatted =
        format(source).unwrap_or_else(|e| panic!("format failed: {e:?}\nsource:\n{source}"));
    let tokens = Lexer::new(&formatted).tokenize().unwrap_or_else(|e| {
        panic!("formatted output failed to lex: {e:?}\nformatted:\n{formatted}")
    });
    Parser::new(tokens).parse_program().unwrap_or_else(|e| {
        panic!(
            "formatted output failed to parse: {e:?}\nsource:\n{source}\nformatted:\n{formatted}"
        )
    });
}

fn assert_idempotent(source: &str) {
    let first = format(source).unwrap_or_else(|e| {
        panic!("first format failed: {e:?}");
    });
    let second = format(&first).unwrap_or_else(|e| {
        panic!("second format failed: {e:?}\nfirst:\n{first}");
    });
    assert_eq!(
        first, second,
        "formatter must be idempotent\n---first---\n{first}\n---second---\n{second}"
    );
}

#[test]
fn test_fuzz_repro_round29_import_block_idempotent() {
    // The exact 549-byte input captured by the fuzzer. After the first
    // formatting pass, imports sort and a comment that was originally
    // mid-list ends up attached to the new first import. Before the fix,
    // a second pass inserted a blank line between that comment and the
    // first import, violating idempotency.
    let source = "import silt\n\
import option\n\
import string\n\
\n\
-- BFS maze solver on a grid.\n\
-- Parses a maze from strings (# = wall, . = open, S = start, E = end),\n\
-- finds the shortest path, and prints the solutio.#\", \"#.##.###.#\", \"#....#.#.#\", \
\"####.#.#.#\", \"#....#...#\", \"#.####.#.#\", \"#.#....#.#\", \"#...####E#\", \
\"###import list\n\
import map\n\
import option\n\
\n\
-- Search algorithms using loop\n\
--\n\
-- Demonstrates why loop is pieferred over fold_until for search:\n\
-- loop can return a different type (e.g., Option) than the iteration state,\n\
-- while fold_until req#######\"ui";
    assert_idempotent(source);
}

#[test]
fn test_minimal_reduced_import_block_idempotent() {
    // Minimal reduction of the fuzz input that still triggers the bug:
    // a few imports, a blank line, a top-level comment whose body happens
    // to contain `###import list` (stays a comment — the lexer does not
    // treat it as code), then more imports. Sorting moves `import map`
    // to the front; its attached comment_block then needs the same
    // emission shape as a `buckets[0]` header block.
    let source = "\
import silt\n\
import option\n\
\n\
-- foo ###import list\n\
import map\n\
import option\n";
    assert_idempotent(source);
}

#[test]
fn test_file_header_with_mid_list_comment_idempotent() {
    // File-level header comment plus a mid-list comment attached to an
    // import that sorting will move to the front. Without the fix, pass 1
    // emits `header\n\nmid_comment\nimport_a\n...` and pass 2 merges the
    // two comment lines into one header block with a blank line after.
    let source = "\
-- file header\n\
import z\n\
-- explains a\n\
import a\n";
    assert_idempotent(source);
}

#[test]
fn test_leading_comment_then_imports_idempotent() {
    // Standard pre-sorted form: a header comment followed by imports in
    // alphabetical order. This is the shape pass 2 converges to; must
    // round-trip unchanged.
    let source = "\
-- doc for imports\n\
import a\n\
import b\n";
    assert_idempotent(source);
}

#[test]
fn test_comment_between_imports_then_trailing_code_idempotent() {
    // A mid-list comment plus a following non-import declaration.
    let source = "\
import b\n\
-- why we use a\n\
import a\n\
\n\
fn main() = 1\n";
    assert_idempotent(source);
}

#[test]
fn test_imports_with_trailing_only_file_comment_idempotent() {
    // Imports followed by an end-of-file comment after a blank line.
    let source = "\
import b\n\
import a\n\
\n\
-- trailing file comment\n";
    assert_idempotent(source);
}

#[test]
fn test_single_element_tuple_pattern_preserves_trailing_comma() {
    // Round-30 fuzz repro: `(_0, )` is a single-element tuple pattern.
    // Pass 1 of the formatter previously stripped the trailing comma,
    // emitting `(_0)`. The parser folds `(x)` away as just `x`, so
    // pass 2 lost the parens entirely — `_0`. Idempotency violation.
    //
    // The fix: single-element tuple patterns must always emit `(x,)`.
    let source = "\
fn fizzbuzz(n) {
  match (n % 3, n % 5) {
    (0, 0) -> \"FizzBuzz\"
    (_0, ) -> \"Fizz\"
    (_, 0) -> \"Buzz\"
    _ -> \"{n}\"
  }
}
";
    assert_idempotent(source);
    let formatted = silt::formatter::format(source).unwrap();
    assert!(
        formatted.contains("(_0,)"),
        "single-element tuple pattern must keep trailing comma; got:\n{formatted}"
    );
}

#[test]
fn test_closure_with_tuple_param_pattern_roundtrips_cleanly() {
    // Round-33 fuzz repro shape (`fuzz_roundtrip` saw `expected
    // parameter name, found (` after one format pass on a non-trailing
    // closure with a tuple-destructuring parameter).
    //
    // The parser accepts richer parameter patterns inside closure form
    // `{ (a, b) -> ... }` (see `parse_closure_params`) but the `fn(...)`
    // form only accepts plain identifiers (see `parse_simple_param_pattern`,
    // which fails with `expected parameter name, found <tok>`).
    //
    // Before the fix, the formatter emitted any non-trailing Lambda as
    // `fn(<pattern>) { ... }` regardless of the parameter shape, so a
    // tuple-pattern closure used as a value (or as a Call's callee, not
    // its trailing arg) round-tripped to invalid syntax.
    let source = "let f = ({ (a, b) -> a + b })(1, 2)\n";
    assert_formatted_parses(source);
    assert_idempotent(source);
    let formatted = silt::formatter::format(source).unwrap();
    assert!(
        formatted.contains("{ (a, b) -> a + b }"),
        "tuple-pattern closure must keep closure form; got:\n{formatted}"
    );
    assert!(
        !formatted.contains("fn((a, b))"),
        "must not emit `fn((a, b))` (parser rejects); got:\n{formatted}"
    );
}

#[test]
fn test_closure_with_constructor_param_pattern_roundtrips_cleanly() {
    // Same root cause, different non-Ident pattern: a constructor
    // pattern as a closure parameter.
    let source = "let f = ({ Some(x) -> x })(Some(1))\n";
    assert_formatted_parses(source);
    assert_idempotent(source);
    let formatted = silt::formatter::format(source).unwrap();
    assert!(
        !formatted.contains("fn(Some("),
        "constructor-pattern closure must not be emitted as `fn(...)`; got:\n{formatted}"
    );
}

#[test]
fn test_closure_lambda_param_variants_roundtrip_cleanly() {
    // Property-style: every closure-only parameter pattern shape that
    // the parser accepts (per `parse_closure_params` → `parse_pattern`)
    // must format to something that re-parses. Single-element tuple
    // pattern is the most likely re-trigger of round-30's other fix.
    for pat in [
        "(a, b)",
        "(x, y, z)",
        "(_, b)",
        "(a, _)",
        "Some(x)",
        "(only,)",
    ] {
        let source = format!("let f = ({{ {pat} -> 1 }})(0)\n");
        assert_formatted_parses(&source);
        let first = silt::formatter::format(&source)
            .unwrap_or_else(|e| panic!("first format failed for pat={pat}: {e:?}"));
        let second = silt::formatter::format(&first).unwrap_or_else(|e| {
            panic!("second format failed for pat={pat}: {e:?}\nfirst:\n{first}")
        });
        assert_eq!(
            first, second,
            "formatter must be idempotent for closure pattern {pat}\n\
             ---first---\n{first}\n---second---\n{second}"
        );
    }
}

#[test]
fn test_single_element_tuple_pattern_variants_idempotent() {
    // Property-style: every common shape of single-element tuple pattern
    // must round-trip with its trailing comma intact through two passes.
    for inner in ["x", "_y", "_0", "longerName", "_", "0", "\"s\""] {
        let source =
            format!("fn f(v) {{\n  match v {{\n    ({inner},) -> 1\n    _ -> 0\n  }}\n}}\n");
        let first = silt::formatter::format(&source)
            .unwrap_or_else(|e| panic!("first format failed for inner={inner}: {e:?}"));
        let second = silt::formatter::format(&first).unwrap_or_else(|e| {
            panic!("second format failed for inner={inner}: {e:?}\nfirst:\n{first}")
        });
        assert_eq!(
            first, second,
            "formatter must be idempotent for single-element tuple pattern ({inner},)\n\
             ---first---\n{first}\n---second---\n{second}"
        );
        assert!(
            first.contains(&format!("({inner},)")),
            "trailing comma lost for inner={inner}; output:\n{first}"
        );
    }
}
