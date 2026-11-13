//! Round-80 LSP audit fixes — regression tests.
//!
//! Two fixes covered:
//!
//! - **G1** (`src/lsp/document_symbols.rs`): `textDocument/documentSymbol`
//!   used the keyword span for both `range` and `selectionRange` on every
//!   Decl arm (Fn, Type, Trait, Let, TraitImpl). Per LSP spec, `range`
//!   should encompass the entire declaration while `selectionRange` is
//!   just the identifier. We assert that for `fn add(...)`, `type T`,
//!   `trait T`, and `let x = ...` the `selectionRange` covers ONLY the
//!   identifier columns (not the keyword) and is strictly narrower than
//!   the surrounding `range`.
//!
//! - **L4** (a function's type in `src/lsp/definitions.rs`): an earlier
//!   implementation rebuilt it by walking the body for every param and
//!   returned `None` if any param was unused inside the body, dropping the
//!   entire fn signature; it is now the checker's type of the function. For `fn ignore(a: Int, b: Int) -> Int { 42 }` (unused
//!   params) hover returned `null` and signatureHelp lost the param
//!   types. After the fix, hover renders the full `Fn(Int, Int) -> Int`
//!   signature and signatureHelp emits `a: Int, b: Int` instead of just
//!   bare names.
//!
//! Both tests drive a real `silt lsp` subprocess end-to-end over LSP
//! JSON-RPC so the full pipeline (lex → parse → typecheck → respond) is
//! exercised. Uses the shared LSP client in `support.rs`.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

use crate::support::LspClient;

static URI_COUNTER: AtomicU64 = AtomicU64::new(1);

fn unique_uri(tag: &str) -> String {
    let n = URI_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("file:///tmp/silt_round80_{tag}_{n}.silt")
}

fn find_symbol<'a>(symbols: &'a [Value], name: &str) -> &'a Value {
    symbols
        .iter()
        .find(|s| s.get("name").and_then(|v| v.as_str()) == Some(name))
        .unwrap_or_else(|| panic!("missing symbol '{name}' in: {symbols:?}"))
}

/// Extract a (line, character) pair from an LSP Position object.
fn pos(v: &Value) -> (u64, u64) {
    (
        v.get("line").and_then(|x| x.as_u64()).unwrap_or(0),
        v.get("character").and_then(|x| x.as_u64()).unwrap_or(0),
    )
}

// ── G1 ──────────────────────────────────────────────────────────────
//
// `selectionRange` should cover ONLY the identifier (not the keyword),
// and `range` should encompass the entire declaration (not collapse to
// the same single-token range as `selectionRange`).

#[test]
fn round80_g1_document_symbols_selection_range_is_identifier() {
    let mut client = LspClient::spawn();

    // Source layout (line/col are 0-indexed, character columns are
    // UTF-16 code units which match ASCII bytes here):
    //
    // line 0: fn add(a: Int, b: Int) -> Int { a + b }
    //         01234567890123
    //                         identifier `add` lives at cols 3..6
    //         keyword `fn` at cols 0..2
    //
    // line 1: type Color { Red, Green, Blue }
    //         identifier `Color` at cols 5..10
    //
    // line 2: trait Show { fn show(self) -> String }
    //         identifier `Show` at cols 6..10
    //
    // line 3: let x = 42
    //         identifier `x` at cols 4..5
    let source = "fn add(a: Int, b: Int) -> Int { a + b }\n\
                  type Color { Red, Green, Blue }\n\
                  trait Show { fn show(self) -> String }\n\
                  let x = 42\n";
    let uri = unique_uri("g1");
    client.did_open_and_wait(&uri, source);

    let result = client.request_result(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": uri } }),
    );
    let symbols = result
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("documentSymbol must return array; got: {result}"));

    // ── add: selectionRange = (line 0, cols 3..6); range strictly larger
    let add = find_symbol(&symbols, "add");
    let sel = add
        .get("selectionRange")
        .expect("add must have selectionRange");
    let sel_start = pos(sel.get("start").unwrap());
    let sel_end = pos(sel.get("end").unwrap());
    assert_eq!(
        sel_start,
        (0, 3),
        "fn `add` selectionRange.start must be (line 0, col 3) — the start of the identifier, \
         not the keyword. got: {sel_start:?} (full sym: {add})"
    );
    assert_eq!(
        sel_end,
        (0, 6),
        "fn `add` selectionRange.end must be (line 0, col 6) — covering only the 3-char \
         identifier `add`. got: {sel_end:?} (full sym: {add})"
    );
    let rng = add.get("range").expect("add must have range");
    let rng_start = pos(rng.get("start").unwrap());
    let rng_end = pos(rng.get("end").unwrap());
    assert_eq!(
        rng_start,
        (0, 0),
        "fn `add` range.start must be at the keyword (line 0, col 0); got: {rng_start:?}"
    );
    assert!(
        rng_end > sel_end,
        "fn `add` range.end ({rng_end:?}) must be strictly larger than \
         selectionRange.end ({sel_end:?}) — `range` covers the whole decl, \
         `selectionRange` covers only the identifier."
    );

    // ── Color: selectionRange = (line 1, cols 5..10)
    let color = find_symbol(&symbols, "Color");
    let sel = color.get("selectionRange").unwrap();
    let sel_start = pos(sel.get("start").unwrap());
    let sel_end = pos(sel.get("end").unwrap());
    assert_eq!(
        sel_start,
        (1, 5),
        "type `Color` selectionRange.start must be the identifier start; got: {sel_start:?}"
    );
    assert_eq!(
        sel_end,
        (1, 10),
        "type `Color` selectionRange.end must end at the identifier end; got: {sel_end:?}"
    );

    // ── Show: selectionRange = (line 2, cols 6..10)
    let show = find_symbol(&symbols, "Show");
    let sel = show.get("selectionRange").unwrap();
    let sel_start = pos(sel.get("start").unwrap());
    let sel_end = pos(sel.get("end").unwrap());
    assert_eq!(
        sel_start,
        (2, 6),
        "trait `Show` selectionRange.start must be the identifier start; got: {sel_start:?}"
    );
    assert_eq!(
        sel_end,
        (2, 10),
        "trait `Show` selectionRange.end must end at the identifier end; got: {sel_end:?}"
    );

    // ── x: selectionRange = (line 3, cols 4..5)
    let x = find_symbol(&symbols, "x");
    let sel = x.get("selectionRange").unwrap();
    let sel_start = pos(sel.get("start").unwrap());
    let sel_end = pos(sel.get("end").unwrap());
    assert_eq!(
        sel_start,
        (3, 4),
        "let `x` selectionRange.start must be the identifier start; got: {sel_start:?}"
    );
    assert_eq!(
        sel_end,
        (3, 5),
        "let `x` selectionRange.end must end at the 1-char identifier end; got: {sel_end:?}"
    );

    client.shutdown();
}

// ── L4 ──────────────────────────────────────────────────────────────
//
// A function's type must NOT drop the whole fn signature when params are
// unreferenced in the body. For `fn ignore(a: Int, b: Int) -> Int { 42 }`,
// hover and signatureHelp must surface the full signature.

#[test]
fn round80_l4_fn_type_preserved_for_unreferenced_params() {
    let mut client = LspClient::spawn();

    // line 0: fn ignore(a: Int, b: Int) -> Int { 42 }
    //         identifier `ignore` lives at cols 3..9
    let source = "fn ignore(a: Int, b: Int) -> Int { 42 }\n\
                  fn main() { ignore(1, 2) }\n";
    let uri = unique_uri("l4");
    client.did_open_and_wait(&uri, source);

    // ── Hover on `ignore` at line 0, col 3 (start of identifier).
    let hover = client.request_result(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 3 },
        }),
    );
    assert!(
        !hover.is_null(),
        "hover on `ignore` must NOT return null — round-80 L4 regression \
         (the signature was dropped for unused params). got: {hover}"
    );
    let hover_text = hover
        .pointer("/contents/value")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("hover must have /contents/value; got: {hover}"));
    // The hover text must contain the rendered Fn type with both Int
    // params present. We pin the canonical render `Fn(Int, Int) -> Int`
    // which is what `Type::Fun` prints (src/types/mod.rs:111).
    assert!(
        hover_text.contains("Fn(Int, Int) -> Int"),
        "hover on `ignore` must render the full `Fn(Int, Int) -> Int` signature; \
         got: {hover_text:?}"
    );

    // ── signatureHelp at the call site `ignore(`. We position the
    // cursor immediately after the `(`. Line 1 is `fn main() { ignore(1, 2) }`.
    // The `(` after `ignore` is at column 18; cursor at column 19 sits
    // just inside the call.
    let sig = client.request_result(
        "textDocument/signatureHelp",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 1, "character": 19 },
        }),
    );
    assert!(
        !sig.is_null(),
        "signatureHelp at `ignore(` call site must return a result; got: {sig}"
    );
    let label = sig
        .pointer("/signatures/0/label")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("signatureHelp must have /signatures/0/label; got: {sig}"));
    // The signature label must include both param types. The exact
    // form produced by `build_signature_from_def` is
    // `fn ignore(a: Int, b: Int) -> Int`. We check the "Int, Int"
    // substring of the param list — without the L4 fix the label
    // collapses to `fn ignore(a, b)` (no types) because `def.ty` is
    // None, so this assertion pins the regression.
    assert!(
        label.contains("a: Int") && label.contains("b: Int"),
        "signatureHelp label must include `a: Int` and `b: Int`; got: {label:?}"
    );

    client.shutdown();
}
