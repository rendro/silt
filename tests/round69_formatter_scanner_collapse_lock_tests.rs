//! Round 69 regression locks for the formatter scanner-collapse refactor.
//!
//! Round 69 collapsed two ~140-line near-identical scanner functions in
//! `src/formatter.rs` (`compute_block_end_line` and
//! `compute_bracket_end_line`) into a single parameterized helper. The
//! brace-only entry point is now a one-line wrapper that delegates to
//! the bracket scanner with `('{', '}')`. The duplication had been
//! flagged as high drift risk: earlier rounds fixed string /
//! triple-string / block-comment / interp-depth tracking bugs in this
//! code, and any future fix would have had to land in BOTH twins to
//! avoid divergence.
//!
//! The scanner is internal, so the locks below test it indirectly via
//! the public formatter entry point. Two complementary proofs:
//!
//!  1. Corpus equivalence. Every shipped `examples/**/*.silt` file is
//!     already in canonical `silt fmt` form (this is independently
//!     pinned by `tests/examples_fmt_check_tests.rs`); we re-pin that
//!     here AFTER the refactor so a regression in the unified scanner
//!     surfaces as "an example no longer round-trips" instead of
//!     leaking through. With 36 example files exercising the scanner
//!     across many real silt programs (records, enums, lambdas,
//!     match arms, nested calls, multi-line strings, block comments),
//!     this is the strongest behavioural lock available.
//!
//!  2. Targeted state-machine edge cases. Synthetic inputs that
//!     deliberately exercise every branch of the consolidated scanner:
//!     - string interpolation (`"a{x}b"`)
//!     - triple-quoted multi-line strings (`"""..."""`) containing
//!       characters that would otherwise be parsed as code
//!     - nested block comments (`{- {- nested -} -}`)
//!     - same-line mixed open/close (`[1, [2, 3]]`)
//!     - brackets containing strings whose content includes `{`/`}`/`[`/`]`
//!     - both `{` and `[` outer wrappers around each weird shape
//!
//!     For each shape, format twice and assert the second pass
//!     produces the same bytes (idempotency).

use std::path::{Path, PathBuf};

use silt::formatter::format;
use silt::lexer::Lexer;
use silt::parser::Parser;

/// Format `source` and assert the result lexes + parses as valid silt.
/// Mirrors the helper in `tests/formatter_idempotency_tests.rs`.
fn assert_formatted_parses(source: &str) -> String {
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
    formatted
}

/// Assert `fmt(fmt(source)) == fmt(source)` (the formatter converges in
/// one pass on this input).
fn assert_idempotent(source: &str) {
    let first = assert_formatted_parses(source);
    let second =
        format(&first).unwrap_or_else(|e| panic!("second format failed: {e:?}\nfirst:\n{first}"));
    assert_eq!(
        first, second,
        "formatter must be idempotent across two passes\n---first---\n{first}\n---second---\n{second}"
    );
}

/// Recursively collect every `.silt` file under `dir`. Mirrors the
/// walker in `tests/examples_fmt_check_tests.rs` so this lock-in test
/// covers the exact same universe of files.
fn collect_silt_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            collect_silt_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("silt") {
            out.push(path);
        }
    }
}

/// Lock 1 (corpus equivalence): the unified scanner must produce
/// byte-identical output to the pre-refactor twin functions on every
/// shipped example. The canonical-form invariant (`fmt(x) == x`) is
/// also pinned independently by `tests/examples_fmt_check_tests.rs`;
/// this test re-pins it from the perspective of the scanner refactor.
/// A failure here means the scanner refactor regressed close-line
/// resolution on at least one example, which would manifest as a
/// formatting diff on a previously-canonical file.
#[test]
fn every_example_formats_canonically_after_scanner_collapse() {
    let examples_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    assert!(
        examples_dir.is_dir(),
        "expected examples directory at {}",
        examples_dir.display()
    );

    let mut files = Vec::new();
    collect_silt_files(&examples_dir, &mut files);
    assert!(
        files.len() >= 30,
        "expected at least 30 .silt files under examples/, found {}",
        files.len()
    );

    let mut failures: Vec<String> = Vec::new();
    for path in &files {
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                failures.push(format!("{}: read error: {}", path.display(), e));
                continue;
            }
        };
        // Reset the interner so one file's interned symbols don't
        // leak into another. The companion walker does the same.
        silt::intern::reset();
        let formatted = match format(&source) {
            Ok(s) => s,
            Err(e) => {
                failures.push(format!("{}: format error: {:?}", path.display(), e));
                continue;
            }
        };
        if formatted != source {
            failures.push(format!(
                "{}: formatter is not a no-op (scanner regression?)",
                path.display()
            ));
            continue;
        }
        // Second pass also a no-op: idempotency.
        silt::intern::reset();
        let second = match format(&formatted) {
            Ok(s) => s,
            Err(e) => {
                failures.push(format!("{}: second format error: {:?}", path.display(), e));
                continue;
            }
        };
        if second != formatted {
            failures.push(format!(
                "{}: formatter is not idempotent across two passes",
                path.display()
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "scanner refactor broke {} example file(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// =====================================================================
// Lock 2: targeted state-machine edge cases.
//
// Each test below constructs a small silt program that deliberately
// exercises one branch of the consolidated scanner state machine
// (string / triple-string / block-comment / interp-depth /
// multi-bracket). The assertion is: the formatter produces a parseable
// output and is idempotent on the second pass. If a future scanner
// fix lands incorrectly (e.g. a triple-string boundary is mis-tracked,
// or a block-comment `{` is treated as a real brace), one of these
// inputs will surface the regression as either a parse failure on the
// formatter output or a non-idempotent second pass.
//
// silt syntax notes used below:
// - `fn name() { body }` is the block-body form (used here whenever the
//   body needs `let`s); `fn name() = expr` is for single expressions.
// - String interpolation uses `"...{expr}..."` (no `$`).
// - Triple-quoted strings `"""..."""` are raw — escapes are literal,
//   and `{` inside triple-strings is not interpolation.
// =====================================================================

/// Regular-string interpolation: the scanner must track `{` inside a
/// `"..."` string as opening an interpolation expression (push interp
/// depth, switch to Code mode), and the matching `}` as closing it
/// (pop, switch back to InRegular). If this branch regressed, the
/// outer block's matching `}` would be miscounted.
#[test]
fn scanner_handles_string_interpolation() {
    let source = "fn main() {\n  let x = 5\n  let s = \"a{x}b\"\n  println(s)\n}\n";
    assert_idempotent(source);
}

/// Interpolation expression that itself contains brackets. The
/// per-interp-frame `{}` depth counter must track `{...}` blocks
/// nested in the interpolation expression so the matching close-`}`
/// of the interp can be found correctly.
#[test]
fn scanner_handles_interp_with_inner_brackets() {
    // Use a list literal inside the interpolation expression. List
    // literals contain `[...]` which the scanner must skip without
    // mis-treating them as the outer bracket pair.
    let source = "fn main() {\n  let xs = [1, 2, 3]\n  println(\"len={list.length(xs)}\")\n}\n";
    assert_idempotent(source);
}

/// Triple-quoted string spanning multiple lines, with content that
/// includes literal `{` `}` `[` `]` `(` `)` and `--` — none of which
/// must affect the bracket scanner. If triple-string awareness were
/// dropped from the unified scanner, the closing `}` of the enclosing
/// fn body would be miscounted.
#[test]
fn scanner_handles_triple_string_with_brackets_inside() {
    let source = concat!(
        "fn main() {\n",
        "  let s = \"\"\"\n",
        "raw { content } -- not a comment\n",
        "more [ inside ] ( parens )\n",
        "\"\"\"\n",
        "  println(s)\n",
        "}\n",
    );
    assert_idempotent(source);
}

/// Nested block comments `{- {- inner -} -}` containing literal `{`,
/// `}`, `[`, `]`, `(`, `)` bytes. The scanner's `block_depth` counter
/// must keep incrementing on `{-` and decrementing on `-}` while
/// IGNORING all other bracket bytes. If this branch regressed, the
/// outer fn body's matching `}` would never be reached.
#[test]
fn scanner_handles_nested_block_comments_with_brackets() {
    let source = concat!(
        "fn main() {\n",
        "  {- {- inner [1, 2] {x: 3} -} outer (a, b) -}\n",
        "  let x = 1\n",
        "  println(x)\n",
        "}\n",
    );
    assert_idempotent(source);
}

/// Mixed open/close brackets on the same line: `[1, [2, 3]]`. The
/// scanner must track depth correctly when the same kind of bracket
/// nests on a single line. This exercises the same-line early-return
/// path of the canonical scanner.
#[test]
fn scanner_handles_same_line_nested_brackets() {
    let source = "fn main() {\n  let xs = [1, [2, 3], [4, [5, 6]]]\n  println(xs)\n}\n";
    assert_idempotent(source);
}

/// `{` outer (block) wrapping the same weird shapes. The scanner is
/// invoked here to find the closing `}` of the fn body, and must
/// thread through every weird interior shape correctly.
#[test]
fn scanner_brace_outer_wrapping_weird_shapes() {
    let source = concat!(
        "fn main() {\n",
        "  let x = 5\n",
        "  let s = \"a{x}b\"\n",
        "  let t = \"\"\"\n",
        "raw { x } [ y ]\n",
        "\"\"\"\n",
        "  let xs = [1, [2, 3]]\n",
        "  let r = {- {- nested -} -} 42\n",
        "  println(s)\n",
        "  println(t)\n",
        "  println(xs)\n",
        "  println(r)\n",
        "}\n",
    );
    assert_idempotent(source);
}

/// `[` outer (list literal) wrapping strings whose content includes
/// brackets and other strings. The scanner is invoked here to find
/// the matching `]` of the list, and must skip the string content
/// without mis-treating its bracket bytes as real depth changes.
#[test]
fn scanner_bracket_outer_wrapping_weird_shapes() {
    let source = concat!(
        "fn main() {\n",
        "  let xs = [\n",
        "    \"plain\",\n",
        "    \"with brackets [ y ] inside\",\n",
        "    \"\"\"\n",
        "raw [ y ] { z }\n",
        "\"\"\",\n",
        "  ]\n",
        "  println(xs)\n",
        "}\n",
    );
    assert_idempotent(source);
}

/// Multiple consecutive triple-strings on different lines: each must
/// reset to Code mode after its closing `"""` so the scanner doesn't
/// erroneously stay in InTriple across a code section.
#[test]
fn scanner_handles_consecutive_triple_strings() {
    let source = concat!(
        "fn main() {\n",
        "  let a = \"\"\"\n",
        "first\n",
        "\"\"\"\n",
        "  let b = \"\"\"\n",
        "second\n",
        "\"\"\"\n",
        "  println(a)\n",
        "  println(b)\n",
        "}\n",
    );
    assert_idempotent(source);
}

/// Line comment `--` immediately followed by characters that look
/// like a block-comment closer or a string opener. Anything after `--`
/// on the same line must be treated as inert content by the scanner.
#[test]
fn scanner_handles_line_comments_with_tricky_payload() {
    let source = concat!(
        "fn main() {\n",
        "  -- payload with } and -} and [ and ( inside\n",
        "  let x = 1\n",
        "  println(x)\n",
        "}\n",
    );
    assert_idempotent(source);
}

/// Escaped quote inside a regular string: `\"`. The scanner's
/// InRegular branch advances by 2 on `\\`, so the next `"` is real
/// content, not the string's terminator. If the escape branch
/// regressed, the string would be considered closed early and the
/// following code would be treated as inside-string raw bytes.
#[test]
fn scanner_handles_escaped_quote_in_regular_string() {
    let source = "fn main() {\n  let s = \"a\\\"b\"\n  println(s)\n}\n";
    assert_idempotent(source);
}

/// Combined stress test: every weird shape in a single program. If
/// any single branch of the unified scanner is wrong, this test
/// fails.
#[test]
fn scanner_combined_stress_test_idempotent() {
    let source = concat!(
        "fn main() {\n",
        "  -- regular line comment with stray } and -} payload\n",
        "  {- block {- nested with [1, 2] {x: 3} -} comment -}\n",
        "  let x = 1\n",
        "  let interp = \"a{x}b\"\n",
        "  let triple = \"\"\"\n",
        "with [ brackets ] and { braces }\n",
        "\"\"\"\n",
        "  let lst = [1, [2, [3, [4, 5]]]]\n",
        "  println(interp)\n",
        "  println(triple)\n",
        "  println(lst)\n",
        "}\n",
    );
    assert_idempotent(source);
}
