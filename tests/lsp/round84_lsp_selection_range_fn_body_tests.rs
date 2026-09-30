//! Round-84 LATENT lock: `textDocument/selectionRange` must compute a
//! tight upper bound on `expr_extent` for **non-Block** expressions.
//! Round 83 fixed the analogous issue for `Decl::Type` / `Decl::Trait`
//! via a dedicated `type_decl_extent` helper, but the `Decl::Let` arm
//! still flows through `expr_extent`, which pre-fix collapsed to
//! `source.len()` for any `ExprKind` other than `Block`: `let g = 99`
//! (value `Int`) claimed to extend to EOF. Selection-range chains for
//! any cursor in a later decl would drag the earlier `let` span in as a
//! parent — Shift+Alt+→ in editors cycled into the unrelated binding.
//!
//! Each test spawns a real `silt lsp` subprocess, opens a small source,
//! asks for the selection range at a cursor in a later decl, and asserts
//! that the chain is anchored inside the cursor's enclosing decl — not
//! at the file's first decl's keyword span.
//!
//! Pattern mirrors `tests/lsp/round83_lsp_selection_range_decl_bounds_tests.rs`;
//! uses the shared LSP client in `support.rs`.

use serde_json::{Value, json};

use crate::support::LspClient;

// ── Helpers ─────────────────────────────────────────────────────────

/// Convert an LSP `Position` (line, character) into a byte offset in
/// `source`, treating each line as ASCII (column == byte offset within
/// line). All test sources here are ASCII.
fn pos_to_byte_offset(source: &str, line: u64, character: u64) -> usize {
    let mut current_line: u64 = 0;
    let mut offset: usize = 0;
    for b in source.bytes() {
        if current_line == line {
            return offset + character as usize;
        }
        if b == b'\n' {
            current_line += 1;
        }
        offset += 1;
    }
    offset
}

/// Walk the entire selection-range chain rooted at `node`, calling `f`
/// on each `SelectionRange` value (root and every parent).
fn walk_chain(node: &Value, f: &mut impl FnMut(&Value)) {
    f(node);
    if let Some(parent) = node.get("parent") {
        if !parent.is_null() {
            walk_chain(parent, f);
        }
    }
}

/// Pull `(start_line, start_char, end_line, end_char)` out of a
/// SelectionRange's `range` field. Panics on a malformed response —
/// that itself is a test failure.
fn range_quad(node: &Value) -> (u64, u64, u64, u64) {
    let r = node.get("range").expect("selection range has range field");
    (
        r.pointer("/start/line").and_then(Value::as_u64).unwrap(),
        r.pointer("/start/character")
            .and_then(Value::as_u64)
            .unwrap(),
        r.pointer("/end/line").and_then(Value::as_u64).unwrap(),
        r.pointer("/end/character").and_then(Value::as_u64).unwrap(),
    )
}

/// Compute the byte offsets `(start, end)` of an LSP range against the
/// ASCII `source`.
fn range_byte_span(source: &str, node: &Value) -> (usize, usize) {
    let (sl, sc, el, ec) = range_quad(node);
    (
        pos_to_byte_offset(source, sl, sc),
        pos_to_byte_offset(source, el, ec),
    )
}

/// Walk the chain to its outermost element (topmost ancestor with no
/// parent). LSP roots chains at the INNERMOST range; `parent` points
/// successively outward.
fn outermost(node: &Value) -> &Value {
    let mut cur = node;
    while let Some(p) = cur.get("parent") {
        if p.is_null() {
            break;
        }
        cur = p;
    }
    cur
}

// ── Tests ───────────────────────────────────────────────────────────

#[test]
fn let_eq_expr_value_extent_does_not_extend_to_eof() {
    // Top-level `let g = 99` — the let's `value` is `Int(99)`, a non-Block
    // expression. Pre-fix the let-decl span was pushed for any cursor in
    // a later decl.
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_r84_let_eq_expr.silt";
    let src = "let g = 99\n\
               \n\
               fn main() {\n\
               \x20\x20let x = 1\n\
               \x20\x20println(x)\n\
               }\n";
    client.did_open_and_wait(file, src);

    // Cursor on the `x` in `println(x)` — line 4, column 10.
    let cursor_line: u64 = 4;
    let cursor_char: u64 = 10;
    assert_eq!(
        src.as_bytes()[pos_to_byte_offset(src, cursor_line, cursor_char)],
        b'x',
        "test source-shape sanity: cursor should point at `x`",
    );

    let resp = client.request(
        "textDocument/selectionRange",
        json!({
            "textDocument": { "uri": file },
            "positions": [ { "line": cursor_line, "character": cursor_char } ]
        }),
    );
    let arr = resp
        .get("result")
        .and_then(|r| r.as_array())
        .expect("selection range result");
    assert_eq!(arr.len(), 1);
    let first = &arr[0];

    // The outermost chain element MUST be anchored at or after the start
    // of `fn main`, NOT at the `let g` decl at byte 0.
    let root = outermost(first);
    let (root_start, _) = range_byte_span(src, root);
    let fn_main_start = src.find("fn main").expect("`fn main` exists in source");
    assert!(
        root_start >= fn_main_start,
        "outermost chain element must be anchored in the cursor's enclosing \
         decl (`fn main` at byte {fn_main_start}); got root starting at byte \
         {root_start}: {root:?}",
    );

    // Defensive: also check that no chain element starts at byte 0 (the
    // `let g` decl's start). This catches the case where some future
    // refactor still pushes the let-decl span but happens to NOT make it
    // outermost.
    walk_chain(first, &mut |node| {
        let (s, _e) = range_byte_span(src, node);
        assert!(
            s >= fn_main_start || s == pos_to_byte_offset(src, cursor_line, cursor_char),
            "no chain element should be anchored before `fn main` for a \
             cursor inside `fn main`; got start={s}, node={node:?}",
        );
    });

    client.shutdown();
}
