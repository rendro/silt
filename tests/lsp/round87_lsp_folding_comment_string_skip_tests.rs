//! Round-87 LATENT: `folding::compute_span_end_line` walked raw source
//! counting `{`/`}` to find the matching close-brace of a `type`,
//! `trait`, or `trait_impl` decl. It correctly skipped simple `"..."`
//! strings but did NOT skip:
//!
//!   * `--` line comments,
//!   * `{- ... -}` block comments (nestable),
//!   * `"""..."""` triple-quoted strings.
//!
//! A `}` inside any of those would decrement the depth counter too early
//! and collapse the fold range to a couple of lines — for the
//! `--`/block-comment cases the fold would end on the very line of the
//! stray `}`. A fold now runs over the lines of its node's span, which
//! ends where the parser found the closing `}`.
//!
//! These tests drive the real LSP `textDocument/foldingRange` handler
//! via subprocess (through the shared LSP client in `support.rs`).

use serde_json::json;

use crate::support::LspClient;

/// Fetch all folds for `src`, return them as a Vec of (startLine,
/// endLine) tuples.
fn folds_for(uri: &str, src: &str) -> Vec<(u64, u64)> {
    let mut client = LspClient::spawn();
    client.did_open_and_wait(uri, src);
    let resp = client.request(
        "textDocument/foldingRange",
        json!({ "textDocument": { "uri": uri } }),
    );
    let arr = resp
        .get("result")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    let out = arr
        .iter()
        .map(|f| {
            (
                f.get("startLine").and_then(|l| l.as_u64()).unwrap_or(0),
                f.get("endLine").and_then(|l| l.as_u64()).unwrap_or(0),
            )
        })
        .collect();
    client.shutdown();
    out
}

// ── Tests ──────────────────────────────────────────────────────────

#[test]
fn folding_type_body_with_line_comment_containing_close_brace() {
    // type body has a `-- comment }` line in the middle. The `}` inside
    // the line comment must NOT short-circuit brace matching — the fold
    // must end at the real closing `}` on line 3.
    //
    // Lines (0-indexed):
    //   0: type Foo {
    //   1:   -- the close } here
    //   2:   bar: Int,
    //   3: }
    let src = "type Foo {\n  -- the close } here\n  bar: Int,\n}\n";
    let folds = folds_for("file:///tmp/silt_r87_fold_line_comment.silt", src);
    // Exactly one fold (the type decl) and it spans 0..3.
    assert_eq!(
        folds.len(),
        1,
        "expected exactly 1 fold for the type decl; got {folds:?}"
    );
    assert_eq!(
        folds[0],
        (0, 3),
        "fold should span the full type body (line 0 to line 3), not \
         stop at the `}}` inside the `--` line comment; got {folds:?}"
    );
}

#[test]
fn folding_type_body_with_block_comment_containing_close_brace() {
    // type body has a `{- comment } -}` line in the middle. The `}`
    // inside the block comment (and the `{-` open) must NOT corrupt
    // the brace count. Fold must end at the real `}` on line 3.
    //
    // Lines (0-indexed):
    //   0: type Foo {
    //   1:   {- the close } here -}
    //   2:   bar: Int,
    //   3: }
    let src = "type Foo {\n  {- the close } here -}\n  bar: Int,\n}\n";
    let folds = folds_for("file:///tmp/silt_r87_fold_block_comment.silt", src);
    assert_eq!(
        folds.len(),
        1,
        "expected exactly 1 fold for the type decl; got {folds:?}"
    );
    assert_eq!(
        folds[0],
        (0, 3),
        "fold should span the full type body (line 0 to line 3), not \
         stop at the `}}` inside the `{{- ... -}}` block comment; \
         got {folds:?}"
    );
}

#[test]
fn folding_trait_impl_with_triple_string_containing_close_brace() {
    // Triple-quoted strings can carry both `"` and `}` in their body,
    // and the closing `"""` is the only delimiter. The pre-fix scanner
    // only knew how to skip simple `"..."` strings, so a triple string
    // with an embedded `"` would split the body into mis-paired skip
    // windows and leak a `}` into the brace counter.
    //
    // We use a `trait Greet for Foo { ... }` impl because `type` bodies
    // are records/enums and don't accept string-literal field defaults.
    //
    // Lines (0-indexed):
    //   0: type Foo { val: Int }
    //   1: trait Greet for Foo {
    //   2:   fn greet(self) -> String {
    //   3:     """a"b}c"""
    //   4:   }
    //   5: }
    //
    // The triple string `"""a"b}c"""` contains a `"` (between `a` and
    // `b`) and a `}` (after `b`). Pre-fix: the simple-string skip
    // mode pairs the `"` chars unevenly and the inner `}` decrements
    // depth — collapsing the trait-impl fold's end_line.
    let src = "\
type Foo { val: Int }
trait Greet for Foo {
  fn greet(self) -> String {
    \"\"\"a\"b}c\"\"\"
  }
}
";
    let folds = folds_for("file:///tmp/silt_r87_fold_triple_string.silt", src);

    // The fold we care about is the trait-impl span (starts at line 1).
    // It must end on line 5 (the real `}` of the trait impl), not
    // earlier due to the embedded `}` inside the triple-quoted string.
    let impl_fold = folds.iter().find(|(s, _)| *s == 1).copied();
    let Some((start, end)) = impl_fold else {
        panic!(
            "no fold found starting at line 1 (the `trait Greet for Foo` header); got folds={folds:?}"
        );
    };
    assert_eq!(start, 1, "impl fold should start at line 1");
    assert_eq!(
        end, 5,
        "impl fold should end at line 5 (the trait-impl's real \
         closing `}}`), not at a `}}` inside the `\"\"\"...\"\"\"` \
         triple-quoted string; got folds={folds:?}"
    );
}
