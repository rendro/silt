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
