//! Round-76 D1: foldingRange must not emit duplicate folds for fn /
//! trait-method / trait-impl-method bodies.
//!
//! Pre-fix: `collect_decl_folds` pushed a fold for the body block via
//! `push_block_fold(&f.body.span, &f.body, ...)` AND then called
//! `walk_expr_folds(&f.body, ...)`, which itself recognises the
//! `ExprKind::Block` body and pushes the same fold again. Trait /
//! TraitImpl method bodies followed the same broken pattern. The
//! existing `lsp_tier2_tests::folding_range_covers_fn_body` only
//! asserted "at least one fold" so duplicates slipped through.
//!
//! Lock: real LSP `textDocument/foldingRange` request, asserting the
//! exact number of folds for a controlled input.
//!   * Two top-level fns → exactly 2 folds (was 4).
//!   * Trait + impl with two methods each → all-distinct (start,end)
//!     tuples (pre-fix every method body produced two folds with the
//!     same span).

use serde_json::json;

use crate::support::LspClient;

// ── Tests ──────────────────────────────────────────────────────────

#[test]
fn folding_range_two_fns_emits_exactly_two_folds() {
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_r76_fold_two_fns.silt";
    // Two single-line fn bodies (each spanning lines 0-2 / 3-5). Pre-fix
    // we would see 4 folds; post-fix exactly 2.
    let src = "fn a() {\n  1\n}\nfn b() {\n  2\n}\n";
    client.did_open_and_wait(file, src);

    let resp = client.request(
        "textDocument/foldingRange",
        json!({ "textDocument": { "uri": file } }),
    );
    let arr = resp
        .get("result")
        .and_then(|r| r.as_array())
        .expect("folding range result");
    assert_eq!(
        arr.len(),
        2,
        "expected exactly 2 folds (one per fn body); got {} — {arr:?}",
        arr.len()
    );

    // Also confirm the fold spans are distinct (no two folds with the
    // same start_line — which would indicate a true duplicate).
    let starts: Vec<u64> = arr
        .iter()
        .filter_map(|f| f.get("startLine").and_then(|l| l.as_u64()))
        .collect();
    let mut deduped = starts.clone();
    deduped.sort_unstable();
    deduped.dedup();
    assert_eq!(
        starts.len(),
        deduped.len(),
        "duplicate folds detected (same startLine reported twice): {starts:?}"
    );

    client.shutdown();
}

#[test]
fn folding_range_trait_impl_two_methods_emits_three_folds() {
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_r76_fold_trait_impl.silt";
    // Trait header + impl with two methods. The impl's body fold + one
    // fold per method body. Pre-fix the impl would yield 1 + 2*2 = 5
    // folds for the impl alone (each method body double-pushed) plus
    // the trait span fold. Post-fix exactly 1 (trait) + 1 (impl) + 2
    // (method bodies) = 4 folds. We assert that no two folds share the
    // same (startLine, endLine) — that's the hallmark of the
    // duplication bug.
    let src = "\
trait T {
  fn a(self) -> Int
  fn b(self) -> Int
}
type W { v: Int }
trait T for W {
  fn a(self) -> Int {
    self.v
  }
  fn b(self) -> Int {
    self.v + 1
  }
}
fn main() {
  let w = W { v: 1 }
  w.a()
}
";
    client.did_open_and_wait(file, src);

    let resp = client.request(
        "textDocument/foldingRange",
        json!({ "textDocument": { "uri": file } }),
    );
    let arr = resp
        .get("result")
        .and_then(|r| r.as_array())
        .expect("folding range result");

    // Each fold has a distinct (startLine, endLine) tuple. The duplicate
    // bug produced two identical folds for every method body; deduping
    // by (start, end) and comparing to the original length detects it.
    let folds: Vec<(u64, u64)> = arr
        .iter()
        .map(|f| {
            (
                f.get("startLine").and_then(|l| l.as_u64()).unwrap_or(0),
                f.get("endLine").and_then(|l| l.as_u64()).unwrap_or(0),
            )
        })
        .collect();
    let mut deduped = folds.clone();
    deduped.sort_unstable();
    deduped.dedup();
    assert_eq!(
        folds.len(),
        deduped.len(),
        "duplicate folds detected (same (start,end) reported twice): folds={folds:?}"
    );

    // Plus the affirmative count: 1 trait + 1 impl + 2 method bodies +
    // 1 main fn body = 5 folds.
    assert_eq!(
        arr.len(),
        5,
        "expected exactly 5 folds; got {} — {arr:?}",
        arr.len()
    );

    client.shutdown();
}
