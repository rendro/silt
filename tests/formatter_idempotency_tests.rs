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

#[test]
fn test_fuzz_repro_round_phase4_single_element_tuple_expr_in_call_arg() {
    // Round phase-4 fuzz repro: a call argument that is a SINGLE-ELEMENT
    // TUPLE expression like `(0,)`. Pass 1 of the formatter previously
    // emitted the tuple WITHOUT its trailing comma (`(0)`), which the
    // parser folds to a parenthesized expression `0`. So the symptom on
    // the second pass was `f((0))` → `f(0)` — the parens disappear.
    //
    // CI minimized symptom: `list.unfold((0), d, s)` becoming
    // `list.unfold(0, d, s)` on the second pass.
    //
    // The fix mirrors the existing single-element tuple PATTERN rule in
    // `format_pattern`: when emitting an `ExprKind::Tuple` of length 1,
    // always include the trailing comma — `(x,)` — so the parser
    // re-recognises it as a tuple rather than a parenthesised expr.
    let source = "fn main() {\n  list.unfold((0,), d, s)\n}\n";
    assert_idempotent(source);
    let formatted = silt::formatter::format(source).unwrap();
    assert!(
        formatted.contains("(0,)"),
        "single-element tuple expression in call-arg position must keep trailing comma; got:\n{formatted}"
    );
}

#[test]
fn test_minimal_single_element_tuple_expr_in_call_arg_idempotent() {
    // Smaller hand-constructed shape of the same bug.
    let source = "fn main() {\n  f((0,))\n}\n";
    let first = silt::formatter::format(source).expect("first format failed");
    let second = silt::formatter::format(&first).expect("second format failed");
    assert_eq!(
        first, second,
        "single-elem tuple in call arg must be idempotent\n---first---\n{first}\n---second---\n{second}"
    );
    assert!(
        first.contains("(0,)"),
        "trailing comma must survive; got:\n{first}"
    );
}

#[test]
fn test_single_element_tuple_expr_variants_idempotent() {
    // Property-style coverage of paren-stripping in call-arg position
    // with various single-element tuple shapes. Each must round-trip
    // through two formatter passes with the trailing comma intact.
    for inner in ["0", "x", "\"s\"", "[1, 2]", "Some(1)"] {
        let source = format!("fn main() {{\n  f(({inner},))\n}}\n");
        let first = silt::formatter::format(&source)
            .unwrap_or_else(|e| panic!("first format failed for inner={inner}: {e:?}"));
        let second = silt::formatter::format(&first).unwrap_or_else(|e| {
            panic!("second format failed for inner={inner}: {e:?}\nfirst:\n{first}")
        });
        assert_eq!(
            first, second,
            "formatter must be idempotent for single-elem tuple expr ({inner},) in call arg\n\
             ---first---\n{first}\n---second---\n{second}"
        );
        assert!(
            first.contains(&format!("({inner},)")),
            "trailing comma lost for inner={inner}; output:\n{first}"
        );
    }
}

#[test]
fn test_parenthesised_binary_in_call_arg_keeps_precedence() {
    // Counter-test: a parenthesised binary expression like `(a + b)` is
    // an ExprKind::Binary, NOT an ExprKind::Tuple — the parser folds the
    // parens away entirely when there is no comma. In call-arg position
    // the parens ARE redundant (no precedence ambiguity), so the
    // formatter strips them on the first pass and pass 2 is a no-op.
    // This test pins that behaviour: the formatter normalises away
    // redundant parens around non-tuple parenthesised exprs in call args
    // without violating idempotency.
    let source = "fn main() {\n  f((a + b))\n}\n";
    assert_idempotent(source);
    let formatted = silt::formatter::format(source).unwrap();
    assert!(
        formatted.contains("f(a + b)"),
        "redundant parens around binary in call-arg position should be stripped; got:\n{formatted}"
    );
}

#[test]
fn test_multi_element_tuple_expr_in_call_arg_no_extra_comma() {
    // Ensure the single-elem fix didn't accidentally add a trailing
    // comma to multi-element tuples.
    let source = "fn main() {\n  f((1, 2))\n  f((1, 2, 3))\n}\n";
    assert_idempotent(source);
    let formatted = silt::formatter::format(source).unwrap();
    assert!(
        formatted.contains("f((1, 2))"),
        "two-element tuple must not gain trailing comma; got:\n{formatted}"
    );
    assert!(
        formatted.contains("f((1, 2, 3))"),
        "three-element tuple must not gain trailing comma; got:\n{formatted}"
    );
}

#[test]
fn test_minimal_dashes_inside_string_interpolation_idempotent() {
    // Round phase-4 fuzz repro (post-(0,)-fix): the line scanner that
    // builds the `trailing_map` (`extract_trailing_comment_from_line`)
    // toggled `in_string` only on `"`, ignoring string interpolations.
    // For source like `fn n(){"{"--"}"}` the scan walked past the inner
    // `"--"` (a NESTED string inside an interpolation expression), saw
    // the string close, then mis-classified the next `--` as a
    // top-level line comment. The phantom trailing comment text
    // (`--"}"}`) was attached to the body line; on each formatter pass
    // the same scan was rerun on the new output and a NEW phantom
    // comment was emitted, so the output grew unboundedly across passes.
    //
    // Fix: the scanner now mirrors the lexer's `interp_stack` — an
    // unescaped `{` inside a string opens an interpolation expression
    // (code mode), and only the matching `}` returns to string mode.
    let source = "fn n(){\"{\"--\"}\"}";
    assert_idempotent(source);
    let formatted = silt::formatter::format(source).unwrap();
    // The body must NOT contain a fabricated trailing comment.
    assert!(
        !formatted.contains("--\"}\"}"),
        "phantom trailing comment leaked into output:\n{formatted}"
    );
}

#[test]
fn test_string_interp_with_dashes_in_nested_string_idempotent() {
    // Variant: explicit fn body with a string interpolation whose
    // expression is a string literal containing `--`. The formatter
    // must not see the outer `"`-`"` pair as bracketing the whole
    // string (the inner `"-- ..."` is a nested string inside an interp).
    let source = "fn main() = \"{\"-- not a comment\"}\"\n";
    assert_idempotent(source);
    let formatted = silt::formatter::format(source).unwrap();
    assert!(
        !formatted.contains(" -- not a comment"),
        "phantom trailing comment leaked:\n{formatted}"
    );
}

#[test]
fn test_string_interp_with_real_trailing_comment_idempotent() {
    // Counter-test: an actual `--` trailing comment AFTER a string
    // interpolation must still be detected as trailing.
    let source = "fn main() = \"{x}\" -- real trailing\n";
    assert_idempotent(source);
    let formatted = silt::formatter::format(source).unwrap();
    assert!(
        formatted.contains("-- real trailing"),
        "real trailing comment lost:\n{formatted}"
    );
}

#[test]
fn test_string_interp_dashes_variants_idempotent() {
    // Property-style: a handful of string-interp shapes that previously
    // tripped the line scanner's `--` detection. Each must be idempotent
    // and must not gain a phantom trailing comment between passes.
    for src in [
        // Dashes inside a nested string inside an interp.
        "fn main() = \"{\"--\"}\"\n",
        // Multiple interps, dashes inside one of them.
        "fn main() = \"{a}{b}{\"-- inert\"}{c}\"\n",
        // Block comment inside an interp expression.
        "fn main() = \"{a {- inline -} + 1}\"\n",
        // Negation inside an interp expression — `{-x}` must NOT be
        // misread as a block-comment open.
        "fn main() = \"{-x}\"\n",
        // Escaped brace before an interp on the same line.
        "fn main() = \"\\{not interp \\\"--\\\"}\"\n",
        // Interp expression that itself contains another interp string.
        "fn main() = \"{\"{\"-- z\"}\"}\"\n",
    ] {
        let first = silt::formatter::format(src)
            .unwrap_or_else(|e| panic!("first format failed for {src:?}: {e:?}"));
        let second = silt::formatter::format(&first)
            .unwrap_or_else(|e| panic!("second format failed for {src:?}: {e:?}\nfirst:\n{first}"));
        assert_eq!(
            first, second,
            "formatter must be idempotent for string-interp src {src:?}\n\
             ---first---\n{first}\n---second---\n{second}"
        );
    }
}

#[test]
fn test_multiline_string_dashes_no_phantom_comment() {
    // Regression for the round-multiline-string-comment fuzz find: the
    // formatter's per-line comment scanner did not track string state
    // across lines, so a `-- ...` at the start of a line that is actually
    // inside a multi-line regular `"..."` string was mis-classified as a
    // standalone or trailing comment. Pass 1 emitted phantom comments,
    // and the result was non-idempotent.
    //
    // Each input below contains a regular string literal with raw
    // newlines and `-- ...` on a continuation line. After formatting,
    // the string's newlines collapse into `\n` escapes, the phantom
    // comment must NOT appear, and a second pass must produce the same
    // output.
    for src in [
        // `--` at start of a mid-string line.
        "fn main() {\n  println(\"first line\n-- mid string\nlast line\")\n}\n",
        // `--` at start of the LAST string line (right before closing `\"`).
        "fn main() {\n  println(\"first line\n-- last\")\n}\n",
        // Multi-line string + a real trailing comment after the closing `\"`.
        "fn main() {\n  println(\"a\n-- inside\nb\") -- real trailing\n}\n",
        // Multi-line string at a let-binding position with no trailing
        // comment, just to exercise the `RegularEnds` no-comment path.
        "fn main() {\n  let s = \"a\n-- inside\nb\"\n  s\n}\n",
        // Multi-line string adjacent to a real standalone comment on the
        // following line (the standalone comment must still be attached).
        "fn main() {\n  let s = \"a\n-- inside\nb\"\n  -- real standalone\n  s\n}\n",
    ] {
        let first = silt::formatter::format(src)
            .unwrap_or_else(|e| panic!("first format failed for {src:?}: {e:?}"));
        // Sanity: the phantom comment text "-- inside" / "-- mid string" /
        // "-- last" must NOT appear OUTSIDE of an escaped `\n-- ...`
        // sequence in the formatted output. The simplest invariant: the
        // formatted output must not contain a literal newline followed by
        // `--` introduced by a phantom-comment extraction. Multi-line
        // string contents are collapsed to single-line via `\n` escapes,
        // so any real trailing `--` comment lives on its own physical
        // line — which is fine — but a phantom one would appear directly
        // after the call's closing `)`.
        for needle in ["-- mid string", "-- inside", "-- last"] {
            // The needle must only appear as part of `\n` escape inside
            // the string literal: i.e. preceded by a literal `\n` (the
            // 2-char escape) inside `"..."`. Anywhere it appears as an
            // actual line-comment (preceded by start-of-line whitespace)
            // would be a phantom — except the test inputs don't contain
            // any real `--` comments with these texts.
            let mut search_from = 0;
            while let Some(pos) = first[search_from..].find(needle) {
                let abs = search_from + pos;
                // Must be preceded (after stripping leading whitespace on
                // its line) by a `"` continuation, i.e. by `\n` in source.
                let line_start = first[..abs].rfind('\n').map_or(0, |i| i + 1);
                let prefix = &first[line_start..abs];
                if prefix.trim().is_empty() {
                    panic!(
                        "phantom comment `{needle}` appeared at start of a line in formatted output for {src:?}\n---formatted---\n{first}"
                    );
                }
                search_from = abs + needle.len();
            }
        }
        let second = silt::formatter::format(&first)
            .unwrap_or_else(|e| panic!("second format failed for {src:?}: {e:?}\nfirst:\n{first}"));
        assert_eq!(
            first, second,
            "formatter must be idempotent for multi-line-string src {src:?}\n\
             ---first---\n{first}\n---second---\n{second}"
        );
    }
}

#[test]
fn test_triple_string_with_embedded_dashes_idempotent() {
    // Regression for the round-triple-string-match-arm fuzz find: the
    // per-line trailing-comment scanner (`extract_trailing_comment_from_line`)
    // toggled `in_string` on every `"`, treating every quote as a regular
    // string boundary. For a same-line triple-quoted string with embedded
    // `--` and an odd number of leading quotes (e.g. `""""--"""`), the
    // alternating toggle left the scanner OUTSIDE a string when it
    // reached the `--`, so it fabricated a phantom trailing comment
    // (e.g. `--"""}`) that grew on every formatter pass and broke
    // idempotency.
    //
    // The fix recognises `"""` as a triple-quote boundary before the
    // single-quote rule and tracks `in_triple` separately from
    // `in_string`. Inside a triple-quoted string everything (including
    // `"`, `--`, `{-`) is raw content with no escape processing.
    //
    // Minimised fuzz repro:
    let source = "fn i(){\"\"\"\"--\"\"\"}";
    assert_idempotent(source);
    let formatted = silt::formatter::format(source).unwrap();
    assert!(
        !formatted.contains("--\"\"\"}"),
        "phantom trailing comment leaked into output:\n{formatted}"
    );
}

#[test]
fn test_triple_string_dashes_variants_idempotent() {
    // Property-style coverage: every shape of same-line triple-quoted
    // string that previously tripped the per-line `--` scanner. Each
    // input must round-trip through two formatter passes unchanged and
    // must NOT gain a fabricated trailing comment.
    for src in [
        // The minimised fuzz repro (4-quote opener + dashes + 3-quote close).
        "fn i(){\"\"\"\"--\"\"\"}",
        // Same shape in let-binding.
        "let s = \"\"\"\"--\"\"\"\n",
        // Dashes inside a triple-quoted string at top level with content.
        "fn main() = \"\"\"foo--bar\"\"\"\n",
        // Two adjacent triple-quoted strings on the same line, one with
        // dashes inside.
        "fn main() = \"\"\"--\"\"\" + \"\"\"x\"\"\"\n",
        // Triple-quoted string in match-arm body with dashes inside.
        "fn main() {\n  match x {\n    Foo -> \"\"\"a--b\"\"\"\n    Bar -> 0\n  }\n}\n",
        // 5-quote opener (one literal `\"` inside, then `--`, then close).
        "fn i() = \"\"\"\"\"--\"\"\"\n",
        // Triple + real trailing comment after the close.
        "fn i() = \"\"\"--\"\"\" -- real trailing\n",
    ] {
        let first = silt::formatter::format(src)
            .unwrap_or_else(|e| panic!("first format failed for {src:?}: {e:?}"));
        let second = silt::formatter::format(&first)
            .unwrap_or_else(|e| panic!("second format failed for {src:?}: {e:?}\nfirst:\n{first}"));
        assert_eq!(
            first, second,
            "formatter must be idempotent for triple-string-with-dashes src {src:?}\n\
             ---first---\n{first}\n---second---\n{second}"
        );
    }
}

#[test]
fn test_multiline_triple_string_in_interp_idempotent() {
    // Second post-fix regression find from the same fuzz round: a
    // multi-line triple-quoted string INSIDE a string interpolation
    // expression, with a `--` after the outer interp's closing `}`.
    // `classify_lines` correctly tags the close line as `TripleEnds`,
    // but the post-close tail begins with `}` (the interp closer) and
    // the per-line trailing-comment scanner has no way to know it
    // should be in interp-expression state. Without the guard, every
    // `--` in that tail is misread as a real trailing comment that the
    // formatter then APPENDS to the opening line — and on the next
    // pass the same scan re-runs on the new output and finds the SAME
    // phantom again, growing the file unboundedly across passes.
    //
    // The fix: only extract a trailing comment from the `TripleEnds`
    // tail when the tail's prefix is whitespace-only (the idiomatic
    // shape `""" -- real trailing`). Any other prefix is treated as
    // post-close code and skipped.
    let source = "let s = \"{\"{\"\"\"a\nb\"\"\"}--c\"}\"\n";
    assert_idempotent(source);

    // Counter-test: a real trailing comment after the close on its own
    // physical line (idiomatic shape) MUST still be preserved across
    // passes — the whitespace-only-prefix rule still recognises it.
    let source = "let s = \"\"\"a\nb\"\"\" -- real trailing\n";
    assert_idempotent(source);
    let formatted = silt::formatter::format(source).unwrap();
    assert!(
        formatted.contains("-- real trailing"),
        "real trailing comment after multi-line triple-string close was lost; got:\n{formatted}"
    );
}

#[test]
fn test_triple_string_inside_interp_with_raw_newline_idempotent() {
    // Fuzz-found regression (post-c109e96): a regular `"..."` string
    // contains a `{...}` interpolation expression whose body opens a
    // triple-quoted string `"""..."""` with a raw newline inside. The
    // SECOND physical line of the input is INSIDE that triple-string-
    // inside-interp, but the line classifier's regular-string token
    // walker treats the whole `"...{...}..."` as one outer string range
    // and never recurses into the interp body. The continuation line
    // (which begins with `--`) gets classified as `Code`, so the per-
    // line scanner extracts a phantom `-- ...` trailing comment that
    // the formatter then re-emits on every subsequent pass.
    //
    // Minimized 25-byte repro from
    // `fuzz/corpus/fuzz_formatter/round-triple-in-interp-multiline.silt`.
    let source = "fn i(){\"){\"\"\".\n--)\"\"\"}\"}\n";
    assert_idempotent(source);

    // Variant: same shape, but at top-level (not wrapped in a fn) — to
    // make sure the fix isn't accidentally specific to function bodies.
    let source = "let s = \"a{\"\"\"x\ny\"\"\"}b\"\n";
    assert_idempotent(source);

    // Variant: triple-string inside interp with a `--` immediately
    // following its closing `"""` on the continuation line, then more
    // string content — the `--` is raw triple-string content here, not
    // a comment, but the per-line scanner without nesting awareness
    // can't tell.
    let source = "let s = \"p{\"\"\"a\n--b\"\"\"}q\"\n";
    assert_idempotent(source);
}

// ---------------------------------------------------------------------
// Round (post-c109e96) — comment between pipe (`|>`) stages migrates
// past the chain on re-format.
//
// Root cause: `format_pipe_chain_expr` only emitted *trailing* comments
// for each stage. Standalone `-- ...` lines whose source position fell
// strictly between two consecutive stages were not drained inside the
// chain. The enclosing block's `take_comments_between(last_stmt_line,
// block_close_line)` later picked them up and appended them after the
// chain. That made the very first formatting pass non-idempotent: pass
// 1 wrote the comment after the chain; on pass 2 the parser re-merged
// the chain across the comment line, so the chain's tail stages now
// preceded the comment — different output.
//
// Fix: drain standalone comments between consecutive stages inside
// `format_pipe_chain_expr` and emit them on their own indented lines
// before the next `|>` continuation.
// ---------------------------------------------------------------------

#[test]
fn test_pipe_chain_interior_comment_idempotent() {
    // Minimal hand-built repro: a single standalone comment between
    // two `|>` stages must stay inside the chain on every re-format.
    let source = "fn main() {\n  print_section(\"Pipeline 8\", result8)\n  --comment_in_pipe\n  |> sort_by_length\n  |> word_count\n}\n";
    assert_idempotent(source);
}

#[test]
fn test_pipe_chain_multiple_interior_comments_idempotent() {
    // Multiple standalone comments scattered between every adjacent
    // pair of stages — each must land between the same two stages
    // after both passes.
    let source = "fn main() {\n  x\n  -- before f\n  |> f\n  -- between f and g\n  -- another between\n  |> g\n  -- after g but inside chain\n  |> h\n}\n";
    assert_idempotent(source);
}

#[test]
fn test_pipe_chain_interior_comment_with_pipe_text_idempotent() {
    // Variant where the comment text itself contains `|>` substrings.
    // The pipe walker must not be fooled by comment content.
    let source = "fn main() {\n  x\n  -- decoration ----------|> filter_problems\n  |> sort_by_length\n  |> word_count\n}\n";
    assert_idempotent(source);
}

#[test]
fn test_fuzz_repro_round_post_c109e96_pipe_comment_idempotent() {
    // Reduced excerpt from `round-comment-attribution-pipeline.silt`:
    // two `print_section` calls separated by a `--` comment and
    // followed by `|>` continuations that the parser threads through
    // the comment, so the comment ends up syntactically between the
    // first call and the chain's tail stages.
    let source = concat!(
        "fn uppercase(lines) {\n",
        "  let result8 = lines\n",
        "  |> filter_problems\n",
        "  |> sort_by_length\n",
        "  |> word_count\n",
        "  print_section(\"Pipeline 8\", result8)\n",
        "  --print_section(\"Pipeline 13\", result13) ----bered\n",
        "  -- ----------------\n",
        "  |> sort_by_length\n",
        "  |> word_count\n",
        "}\n",
    );
    assert_idempotent(source);
    assert_formatted_parses(source);
}

#[test]
fn test_fn_with_triple_string_braces_preserves_interior_comments_idempotent() {
    // Pre-existing bug surfaced post-c1d1af6 fuzz audit: the brace-
    // counting line scanner in `resolve_decl_end_lines` did not
    // recognise triple-quoted strings, so a fn body containing
    // `"""...{..."""` confused depth tracking. The fn's `decl_end_line`
    // collapsed to its start line, classifying all interior comments
    // as "after-last-decl" — pass 1 emitted them as ORPHANS after the
    // closing `}`, semantically relocating them out of the fn body.
    //
    // The fix replaces the per-line ad-hoc state machine with a
    // character-level frame-stack scanner mirroring `classify_lines`,
    // so triple-string content (and unbalanced inner `{`/`}`) is
    // correctly skipped during brace-balance computation.
    //
    // Minimal hand-built repro (`\d{2}` style — balanced inner braces
    // happen to NOT trip the old scanner; truly unbalanced `{` does):
    let source =
        "fn main() {\n  -- comment inside fn body\n  let r = \"\"\"abc{def\"\"\"\n  r\n}\n";
    assert_idempotent(source);
    let formatted = silt::formatter::format(source).unwrap();
    // The comment must remain INSIDE the fn body (above `let r`),
    // not orphaned outside the closing `}`.
    let close_idx = formatted
        .find("\n}")
        .expect("formatted output has closing brace");
    let comment_idx = formatted
        .find("-- comment inside fn body")
        .expect("comment must survive formatting");
    assert!(
        comment_idx < close_idx,
        "comment must remain INSIDE the fn body, not orphaned after `}}`:\n{formatted}"
    );
}

#[test]
fn test_fn_with_triple_string_braces_in_let_after_comment_idempotent() {
    // Variant: a triple-string with an inner `{` lives in a let, and
    // the comment lives BETWEEN the let and the trailing expression.
    let source = "fn main() {\n  let r = \"\"\"abc{def\"\"\"\n  -- comment after let\n  r\n}\n";
    assert_idempotent(source);
    let formatted = silt::formatter::format(source).unwrap();
    let close_idx = formatted
        .find("\n}")
        .expect("formatted output has closing brace");
    let comment_idx = formatted
        .find("-- comment after let")
        .expect("comment must survive formatting");
    assert!(
        comment_idx < close_idx,
        "comment must remain INSIDE the fn body:\n{formatted}"
    );
}

#[test]
fn test_match_arm_with_triple_string_braces_preserves_interior_comments_idempotent() {
    // Variant: triple-string with `{` lives inside a match arm body.
    // The match arm itself doesn't have its own decl_end_line, but the
    // enclosing fn does — so the same brace miscount applied.
    let source = concat!(
        "fn main(x) {\n",
        "  -- top comment\n",
        "  match x {\n",
        "    1 -> \"\"\"a{b\"\"\"\n",
        "    _ -> \"\"\"c{d\"\"\"\n",
        "  }\n",
        "}\n",
    );
    assert_idempotent(source);
    let formatted = silt::formatter::format(source).unwrap();
    let close_idx = formatted
        .rfind("\n}")
        .expect("formatted output has closing brace");
    let comment_idx = formatted
        .find("-- top comment")
        .expect("comment must survive formatting");
    assert!(
        comment_idx < close_idx,
        "comment must remain INSIDE the fn body:\n{formatted}"
    );
}

#[test]
fn test_two_fns_with_triple_string_braces_preserves_interior_comments_idempotent() {
    // Variant: two top-level fns, each with a triple-string carrying
    // an unbalanced inner `{`. Both decls' `end_line` resolutions must
    // skip the triple-string content; otherwise the second fn's
    // pre-comment leaks across the boundary.
    let source = concat!(
        "fn first() {\n",
        "  -- first body comment\n",
        "  let p = \"\"\"x{y\"\"\"\n",
        "  p\n",
        "}\n",
        "\n",
        "fn second() {\n",
        "  -- second body comment\n",
        "  let q = \"\"\"a{b\"\"\"\n",
        "  q\n",
        "}\n",
    );
    assert_idempotent(source);
    let formatted = silt::formatter::format(source).unwrap();
    // Both interior comments must remain BEFORE their respective fn's
    // closing `}`. We assert by ordering: first-comment < first-close
    // (= the FIRST `}`), second-comment > first-close, second-comment
    // < second-close (= the LAST `}`).
    let comment1 = formatted
        .find("-- first body comment")
        .expect("first comment must survive");
    let comment2 = formatted
        .find("-- second body comment")
        .expect("second comment must survive");
    let first_close = formatted.find("\n}\n").expect("first closing brace");
    let last_close = formatted.rfind("\n}").expect("last closing brace");
    assert!(
        comment1 < first_close,
        "first comment must stay inside first fn body:\n{formatted}"
    );
    assert!(
        comment2 > first_close && comment2 < last_close,
        "second comment must stay inside second fn body:\n{formatted}"
    );
}

// ── Bug 1: multi-line block comment with code on closing `-}` line ──
//
// Source shape:
//   fn s() {
//     {-
//     -} p
//   }
//
// `extract_comments` walks the multi-line `{- ... -}` and on the closer
// line (`-} p`) saw real code following the closer (`p`). With the old
// `if found_end || depth == 0` guard, the entire block (which textually
// included `p`) was pushed as a standalone comment AND `p` was emitted
// as code by the AST walker — so pass 1 produced both a `{-\n  -} p`
// comment line AND a separate `  p` statement line. Pass 2 saw the
// emitted standalone comment as new source, attributed `p` again, and
// the dup grew with each pass. Fix: only push the comment when the
// closer is whitespace-terminated; otherwise let
// `splice_inline_block_comments` re-insert the block comment text in
// the gap before the next AST token.
#[test]
fn test_multiline_block_comment_closer_with_code_idempotent() {
    let source = "fn s() {\n  {-\n  -} p\n}\n";
    assert_idempotent(source);
    let formatted = format(source).unwrap();
    // The identifier `p` must appear exactly once in the body — not duplicated.
    let p_count = formatted
        .lines()
        .filter(|l| l.trim_start().trim_end_matches(',') == "p" || l.contains("-} p"))
        .count();
    assert_eq!(
        p_count, 1,
        "identifier p must appear exactly once, formatted:\n{formatted}"
    );
}

#[test]
fn test_multiline_block_comment_closer_with_let_idempotent() {
    let source = "fn s() {\n  {-\n  -} let a = 1\n  let b = 2\n}\n";
    assert_idempotent(source);
}

#[test]
fn test_multiline_block_comment_closer_inline_idempotent() {
    let source = "fn s() {\n  {- multi\n  line -} p\n}\n";
    assert_idempotent(source);
}

#[test]
fn test_multiline_block_comment_compact_close_with_code_idempotent() {
    // The verbatim shape from the prior agent's report.
    let source = "fn s(){{-\n-}p}";
    assert_idempotent(source);
}

// ── Bug 2: comment after open `(` falsely attributed as call-trailing ──
//
// Source shape:
//   fn main() {
//     f({-c1-}
//       a, -- c2
//     )
//   }
//
// `extract_trailing_comment_from_line` saw `f({-c1-}` and decided
// `{-c1-}` was a trailing block comment of the call STATEMENT (because
// only whitespace followed the `-}` on the same line). The call was
// also rendered multi-line because of `-- c2`, so the splice routine
// independently re-inserted `{-c1-}` between the opening `(` and the
// first arg as a leading-inline annotation. Result: pass 1 emitted the
// comment in BOTH positions (`f(\n  {-c1-} a, ...) {-c1-}`), and pass
// 2 reinterpreted the post-`)` copy at yet another offset, breaking
// idempotency.
//
// Fix: track bracket-pair depth in
// `extract_trailing_comment_from_line` and refuse to emit a trailing
// comment whose `--` or `{-` opens inside an unclosed bracket pair —
// such comments belong to a multi-line interior position, handled by
// `splice_inline_block_comments` (or the multi-line emitter for the
// surrounding construct).
#[test]
fn test_block_comment_after_open_paren_idempotent() {
    let source = "fn main() {\n  f({-c1-}\n    a, -- c2\n  )\n}\n";
    assert_idempotent(source);
    let formatted = format(source).unwrap();
    // `{-c1-}` must appear exactly once in the formatted output.
    let occurrences = formatted.matches("{-c1-}").count();
    assert_eq!(
        occurrences, 1,
        "block comment must not be duplicated, formatted:\n{formatted}"
    );
}

#[test]
fn test_line_comment_after_open_paren_idempotent() {
    // The `-- c1` after `f(` must NOT be attached to the call as a
    // trailing comment; it belongs inside the multi-line layout.
    let source = "fn main() {\n  f( -- c1\n    a, -- c2\n  )\n}\n";
    assert_idempotent(source);
}

// ── Bug 2 (fuzzer crash variant): comments inside fn-param region get
// dropped when `compute_param_lines` cannot resolve per-param lines ──
//
// The fuzzer found a malformed input with multi-line params separated
// by whitespace (no commas), several `--` line comments inside the
// param region, and degenerate non-printable bytes. The lexer parsed
// the whitespace-separated idents as 7 params; the param-region
// comment lines triggered `should_layout_multiline = true`. But
// `compute_param_lines` could only resolve the FIRST param's source
// line (the only `,` it saw was inside a `--` comment, which it
// correctly skipped); the rest fell back to a synthesized `prev_line +
// 1` sequence that monotonically advanced past every comment line
// without ever calling `take_comments_between` with a range that
// contained one. Result: pass 1 emitted multi-line params with the
// comments silently dropped; pass 2 saw no comments → single-line.
//
// Fix: drain ALL unconsumed standalone comments between the fn's start
// line and the params' close line at the end of the multi-line param
// emission, instead of only between `prev_line` (the last param's
// resolved line) and the close. The per-param loop still consumes
// well-attributed comments at their natural position; this catch-all
// just ensures stragglers are emitted (at the end of the param list)
// so subsequent passes still see them.
#[test]
fn test_fn_params_unresolved_lines_preserve_comments_idempotent() {
    // Minimal hand-crafted variant: 3 idents on separate source lines
    // with no commas, plus a comment line between them. The parser
    // lenient-parses the idents as 3 params; the comment line forces
    // multi-line layout; without the fix the comment is dropped on
    // pass 1.
    let source = "fn man(a\nb\n-- C\nc\n) {}\n";
    assert_idempotent(source);
    let formatted = format(source).unwrap();
    assert!(
        formatted.contains("-- C"),
        "comment in unresolved-param region must survive:\n{formatted}"
    );
}

// ── Round 31: comment-attachment-to-statement reorder ────────────────
//
// Fuzz repro at fuzz/corpus/fuzz_formatter/round-call-arg-wrap-followup.silt
// (166 bytes). Two cooperating bugs:
//
// 1. `format_expr_inner`'s `Unary(Neg, _)` arm emitted `-{wrapped}` with
//    no space, so an AST node like `Unary(Neg, Unary(Neg, x))` formatted
//    to `--x`. The lexer treats `--` as a line-comment opener, so the
//    output truncated the rest of the expression on re-parse and the
//    next-statement-start collapsed onto the operator's RHS, changing
//    statement boundaries between passes.
//
// 2. `format_stmts_with_comments` partitioned standalone comments using
//    each statement's *start* line. When a binary operator's RHS lived
//    several lines below the operator (because the lexer skipped
//    intervening blank/comment lines), a standalone comment between the
//    operator line and the RHS line was attributed to the *next*
//    statement here. After the parser-frame shifted on a subsequent
//    pass, the same comment landed on a different statement, producing
//    a reorder. The fix tracks the previous statement's *end* line
//    (max line touched by any sub-expression) and drains interior
//    comments after the statement they live inside.

#[test]
fn test_inner_block_comment_between_statements_idempotent() {
    // Hand-minimized repro: an expression whose RHS is forced onto a
    // later line by a `--` comment between operator and operand. The
    // pre-fix formatter emitted `a % - -b` as `a % --b` (lex-changing!)
    // which then bound `c` as the missing RHS on pass 2.
    let source = "fn main() {\n  a %\n  -\n  -b\n  -- comment\n  c\n}\n";
    assert_idempotent(source);
}

#[test]
fn test_unary_neg_of_neg_does_not_lex_as_comment_idempotent() {
    // Direct test of fix #1: `-(-x)` and `- -x` must format to text
    // that re-lexes as two unary minuses, not as a `--` comment.
    let sources = [
        "fn main() {\n  let x = -(-5)\n}\n",
        "fn main() {\n  let x = - -5\n}\n",
        "fn main() {\n  let x = -(-(-5))\n}\n",
        "fn main() {\n  let x = -(0 - 5)\n}\n",
    ];
    for s in &sources {
        assert_idempotent(s);
        let formatted = format(s).unwrap();
        // The double-negation must NOT collapse to `--` (a comment opener).
        assert!(
            !formatted.contains("--"),
            "double-negation must not lex as a `--` comment:\n{formatted}"
        );
    }
}

#[test]
fn test_comment_inside_expression_span_idempotent() {
    // A `+` whose RHS is on a line below the operator, with a standalone
    // comment between. Pre-fix the comment was attached to the *next*
    // statement; the structure stayed stable here only because no `--`
    // ambiguity was present, but the regression test pins down the
    // correct attribution: comments inside an expression's source span
    // are emitted right after that expression's collapsed line.
    let source = "fn main() {\n  a +\n  -- middle\n  b\n  c\n}\n";
    assert_idempotent(source);
    let formatted = format(source).unwrap();
    // Comment must survive. Its exact placement (between `a + b` and
    // `c`) is incidental — the load-bearing assertion is idempotency
    // plus survival.
    assert!(
        formatted.contains("-- middle"),
        "interior comment must survive:\n{formatted}"
    );
}

#[test]
fn test_comment_at_start_of_block_idempotent() {
    let source = "fn main() {\n  -- header\n  a\n  b\n}\n";
    assert_idempotent(source);
}

#[test]
fn test_comment_at_end_of_block_idempotent() {
    let source = "fn main() {\n  a\n  b\n  -- footer\n}\n";
    assert_idempotent(source);
}

#[test]
fn test_multiple_comments_between_statements_idempotent() {
    let source = "fn main() {\n  a\n  -- c1\n  -- c2\n  b\n  -- c3\n  c\n}\n";
    assert_idempotent(source);
}

#[test]
fn test_fuzz_repro_round_call_arg_wrap_followup_idempotent() {
    // Verbatim 166-byte input from
    // fuzz/corpus/fuzz_formatter/round-call-arg-wrap-followup.silt.
    // The original input contains NUL bytes inside `--:` line comments
    // — the lexer tolerates them as comment content. Use a byte literal
    // to preserve them exactly.
    let source_bytes: &[u8] = b"fn anic() {\n--:\x00\x00 wodc listanic() {\n--:\x00\x00 wodc list\n-henath%\n\n-- Trait simorpt litsim() {\n-\n-pcmi- omrpimpowheh-tn-\na\n Traiz s\n-- BFn?-- Hy dent,\n-- coim() {\n--:\x00n\n}\n";
    let source = std::str::from_utf8(source_bytes).expect("corpus is utf-8 with embedded NULs");
    assert_idempotent(source);
}

#[test]
fn test_trailing_comment_after_triple_string_with_imbalanced_quotes_idempotent() {
    // Hand-minimized repro of the bug found in
    // fuzz/corpus/fuzz_formatter/round-comment-attach-followup-NEW-BUG.silt.
    //
    // Root cause: `compute_block_end_line` (and its sibling
    // `compute_bracket_end_line`) walked source LINES toggling an
    // `in_string` flag at every `"` character. Triple-quoted strings
    // with an internal odd-quote-count line (e.g. `"""*""%"  """`,
    // which the lexer parses as a triple `"""..."""` whose content
    // happens to include three single `"`s) flip the per-line scanner's
    // `in_string` to `true` and never flip it back. The next `}` —
    // which closes the enclosing block — is then treated as raw string
    // content and the brace counter never reaches zero. The fallback
    // `span.line` is returned as the block close, so the final
    // `take_comments_between(prev_end_line, block_close_line)` drain
    // is empty and any standalone comment that lived between the last
    // statement and the block's `}` is silently dropped.
    //
    // The fix teaches both scanners about triple-quoted strings via a
    // persistent `ScanMode` (Code / InRegular / InTriple) so a `"""`
    // opens a triple-string mode that ignores everything (including
    // raw `}`) until the matching closing `"""`.
    let source = "fn f() {\n  \"\"\"*\"\"%\"       \"\"\"\n  -- trailing\n}\n";
    assert_idempotent(source);
    let formatted = format(source).unwrap();
    assert!(
        formatted.contains("-- trailing"),
        "trailing comment must survive when the preceding stmt is a \
         triple-quoted string with odd internal quote counts:\n{formatted}"
    );
}

#[test]
fn test_multiple_trailing_comments_after_triple_string_idempotent() {
    // Companion to the test above: multiple trailing standalone
    // comments after a triple-quoted-string statement. All of them must
    // survive both passes — the original fuzz repro lost the LAST two
    // of five trailing comments because the broken scanner returned a
    // close-line smaller than the last statement's line, so the tail
    // drain was empty.
    let source = concat!(
        "fn f() {\n",
        "  x\n",
        "  \"\"\"*\"\"%\"       \"\"\"\n",
        "  -- a\n",
        "  -- b\n",
        "  -- c\n",
        "}\n",
    );
    assert_idempotent(source);
    let formatted = format(source).unwrap();
    for c in ["-- a", "-- b", "-- c"] {
        assert!(
            formatted.contains(c),
            "trailing comment `{c}` must survive after a triple-quoted \
             string with odd internal quote counts:\n{formatted}"
        );
    }
}

#[test]
fn test_fuzz_repro_round_comment_attach_followup_new_bug_idempotent() {
    // Verbatim 1553-byte input from
    // fuzz/corpus/fuzz_formatter/round-comment-attach-followup-NEW-BUG.silt.
    // Pre-fix, the first formatting pass produced a 1436-byte output
    // and the second pass produced a 1362-byte output (dropping the
    // last two trailing comments inside the outer fn body) — a clear
    // idempotency break. The root cause is documented on
    // `test_trailing_comment_after_triple_string_with_imbalanced_quotes_idempotent`.
    let source =
        include_str!("../fuzz/corpus/fuzz_formatter/round-comment-attach-followup-NEW-BUG.silt");
    assert_idempotent(source);
}

#[test]
fn test_triple_string_content_ending_in_quote_idempotent() {
    // Hand-minimized repro for the post-2c979d4 fuzz finding
    // `round-triple-fold-tail.silt`. Source has a triple-quoted string
    // whose content ends in `"` (the lexer's indent stripping yields
    // `""` as the AST string after consuming a multi-line triple). The
    // formatter previously emitted the single-line form `"""""""""`
    // (3 + content + 3 = 8 quotes), which the lexer re-tokenises as
    // `triple-empty + regular-empty` — losing the trailing content
    // quote(s) and breaking idempotency. The fix forces multi-line
    // emission whenever the content ends with `"` so the close `"""`
    // sits on its own line and can't fuse with the content.
    let source = "fn main() {\n  \"\"\"\"\"\n  \"\"\"\n}\n";
    assert_idempotent(source);
    let formatted = format(source).unwrap();
    // The formatted output must contain a multi-line triple form, not
    // the dangerous run-together `"""""""""`.
    assert!(
        !formatted.contains("\"\"\"\"\"\"\"\"\""),
        "must not emit 9 consecutive `\"` (single-line form fuses content with close):\n{formatted}"
    );
}

#[test]
fn test_triple_string_content_single_quote_idempotent() {
    // Smaller variant: content of just `"` (one quote). The single-line
    // form would be `"""""""` (3 + 1 + 3 = 7 quotes), which re-lexes as
    // empty triple (6 quotes) + an unterminated regular string. Force
    // multi-line.
    //
    // We reach this state via a multi-line source that the lexer's
    // strip_triple_string_indentation reduces to a single-quote AST
    // value: `"""\n"\n"""` -> raw `\n"\n`, indent=0, lines=["", "\"", ""],
    // first/last blank dropped -> `"`.
    let source = "fn main() {\n  \"\"\"\n\"\n\"\"\"\n}\n";
    assert_idempotent(source);
    let formatted = format(source).unwrap();
    assert!(
        !formatted.contains("\"\"\"\"\"\"\""),
        "must not emit 7 consecutive `\"` (single-line form would lex as triple+leftover):\n{formatted}"
    );
}

#[test]
fn test_fuzz_repro_round_triple_fold_tail_idempotent() {
    // Verbatim 221-byte input from
    // fuzz/corpus/fuzz_formatter/round-triple-fold-tail.silt.
    // Pre-fix, pass 1 emitted a triple-string content-ending-in-quote
    // as a single-line `"""""""""` run that pass 2 re-lexed into two
    // separate string tokens (`""""""` triple-empty + `""` regular),
    // changing both content and statement count. See
    // `test_triple_string_content_ending_in_quote_idempotent` for the
    // root cause.
    let source = include_str!("../fuzz/corpus/fuzz_formatter/round-triple-fold-tail.silt");
    assert_idempotent(source);
}
