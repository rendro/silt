//! Round-83 GAP lock: `textDocument/selectionRange` must bound the
//! cursor on BOTH sides when collecting `Decl::Type` / `Decl::Trait`
//! ranges. Pre-fix code only checked that the cursor was at or after the
//! decl's start,
//! letting an unrelated 4-byte `type` keyword span (or 5-byte `trait`)
//! land in the selection chain whenever the cursor was anywhere AFTER
//! the decl in source order. The chain was sorted by source-rest-length
//! so the spurious keyword span landed OUTERMOST — yet it didn't
//! enclose its supposed inner ranges (which sat inside a later
//! function). Editor Shift+Alt+→ semantics broken.
//!
//! Each test spins up a real `silt lsp` subprocess and issues a
//! `textDocument/selectionRange` request through the shared client in
//! `support.rs`. The existing `selection_range_returns_
//! nested_chain` test only asserts "first response has any parent" — a
//! WEAK gate that wouldn't catch this bug. The bug-repro test below
//! walks the full chain and asserts that the lone 4-byte `type` keyword
//! (offset 0-4) is never a member of the chain when the cursor is in an
//! unrelated later decl, AND that the chain's outermost element starts
//! at or after the start of the cursor's enclosing decl (i.e. the chain
//! root is anchored inside the cursor's own decl, not at the file's
//! first decl).

use serde_json::{Value, json};

use crate::support::LspClient;

// ── Helpers ─────────────────────────────────────────────────────────

/// Convert an LSP `Position` (line, character) into a byte offset in
/// `source`, treating each line as ASCII. The tests below use only
/// ASCII source so column == byte offset within line.
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
/// on each `SelectionRange` value (including the root and every parent).
fn walk_chain(node: &Value, f: &mut impl FnMut(&Value)) {
    f(node);
    if let Some(parent) = node.get("parent") {
        if !parent.is_null() {
            walk_chain(parent, f);
        }
    }
}

/// Pull `(start_line, start_char, end_line, end_char)` out of a
/// SelectionRange's `range` field. Panics if any field is missing —
/// signals a malformed response from the LSP, which is itself a test
/// failure.
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
/// ASCII `source`. Used to detect "is this the lone 4-byte `type`
/// keyword span at offset 0?"-style assertions.
fn range_byte_span(source: &str, node: &Value) -> (usize, usize) {
    let (sl, sc, el, ec) = range_quad(node);
    (
        pos_to_byte_offset(source, sl, sc),
        pos_to_byte_offset(source, el, ec),
    )
}

/// Walk the chain to its outermost element (the topmost ancestor that
/// has no parent). The LSP returns the chain rooted at the INNERMOST
/// range, with `parent` pointing successively outward; we walk parents
/// until none remain.
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
fn type_decl_keyword_span_not_pushed_when_cursor_lives_in_later_fn() {
    // Bug repro: cursor on `+` inside `fn main` body. Pre-fix code
    // pushed the `type` keyword span (offset 0-4) into the chain as an
    // OUTERMOST range, even though `+` sits 16+ bytes past the end of
    // the type decl. Fix: bound the cursor on both sides.
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_r83_type_bug.silt";
    //                  0123456789012  (offset on first line)
    let src = "type Foo {}\nfn main() {\n  1 + 2\n}\n";
    client.did_open_and_wait(file, src);

    // `+` is at line 2 (0-indexed), col 4 (also 0-indexed): line is
    // `  1 + 2`, the `+` sits at column 4.
    let cursor_line: u64 = 2;
    let cursor_char: u64 = 4;
    assert_eq!(
        src.as_bytes()[pos_to_byte_offset(src, cursor_line, cursor_char)],
        b'+',
        "test source-shape sanity: cursor should point at `+`"
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
    assert_eq!(arr.len(), 1, "exactly one position requested → one result");
    let first = &arr[0];

    // STRONG ASSERTION 1: no element of the chain is the lone `type`
    // keyword span. The keyword span is exactly the bytes 0..4 of the
    // source (`type`). On the pre-fix code path this span was pushed
    // outermost — a 4-byte unrelated keyword sitting in a chain whose
    // inner ranges live in `fn main` 12+ bytes later.
    walk_chain(first, &mut |node| {
        let (start_off, end_off) = range_byte_span(src, node);
        assert!(
            !(start_off == 0 && end_off == 4),
            "the lone `type` keyword span (offset 0..4) must never appear \
             in the chain when the cursor is in an unrelated later decl; \
             got {node:?}",
        );
    });

    // STRONG ASSERTION 2: the outermost element of the chain starts at
    // or AFTER the start of the cursor's enclosing decl (`fn main` at
    // byte 12). Pre-fix the outermost was the `type` keyword at byte 0
    // — well before `fn main`. This pins the regression directly: even
    // if a future refactor changes what the chain elements look like,
    // the chain root must never be anchored at the FILE's first decl
    // when the cursor lives elsewhere.
    let root = outermost(first);
    let (root_start, _) = range_byte_span(src, root);
    let fn_main_start = src.find("fn main").expect("`fn main` exists in source");
    assert!(
        root_start >= fn_main_start,
        "outermost chain element must be anchored in the cursor's enclosing \
         decl (`fn main` at byte {fn_main_start}); got root starting at byte \
         {root_start}: {root:?}",
    );

    client.shutdown();
}

#[test]
fn type_decl_span_included_when_cursor_inside_type_body() {
    // Positive case: cursor inside a `type Foo { x: Int }` body. The
    // type-decl span SHOULD appear in the chain (and the chain should
    // enclose the cursor at every level — sanity for the new bound).
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_r83_type_positive.silt";
    let src = "type Foo { x: Int }\n";
    client.did_open_and_wait(file, src);

    // Cursor on the `x` field name: line 0, character 11.
    let cursor_line: u64 = 0;
    let cursor_char: u64 = 11;
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

    // Mirror image of the bug-repro test: SOME chain element must be
    // anchored at the type-decl's start (byte 0) when the cursor is
    // inside the decl body. Round 84 widened these chain elements from
    // a bare keyword-only span (0..4) to cover the whole decl extent
    // (0..len) so the chain root encloses the cursor; the round-83
    // anchor invariant (some element starts at byte 0) is preserved.
    let mut saw_decl_anchor = false;
    let cursor_off = pos_to_byte_offset(src, cursor_line, cursor_char);
    walk_chain(first, &mut |node| {
        let (s, e) = range_byte_span(src, node);
        if s == 0 && e >= cursor_off {
            saw_decl_anchor = true;
        }
    });
    assert!(
        saw_decl_anchor,
        "with cursor inside the type body, some chain element should be \
         anchored at the type-decl's start (byte 0) and enclose the cursor \
         (byte {cursor_off}); chain={first:?}",
    );

    client.shutdown();
}

#[test]
fn trait_decl_keyword_span_not_pushed_when_cursor_lives_in_later_fn() {
    // Parallel to the Decl::Type case: `trait Bar { fn foo() }` at the
    // top of the file, cursor inside an unrelated later `fn main`. The
    // 5-byte `trait` keyword span (offset 0..5) must not appear in the
    // chain.
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_r83_trait_bug.silt";
    // `trait Bar { fn foo() }` — a parameter-less trait with one
    // method header. Followed by an unrelated `fn main` body whose `+`
    // expression is the cursor target.
    let src = "trait Bar { fn foo() }\nfn main() {\n  1 + 2\n}\n";
    client.did_open_and_wait(file, src);

    // Cursor on `+` inside `fn main`: line 2 (0-indexed), col 4.
    let cursor_line: u64 = 2;
    let cursor_char: u64 = 4;
    assert_eq!(
        src.as_bytes()[pos_to_byte_offset(src, cursor_line, cursor_char)],
        b'+',
        "test source-shape sanity: cursor should point at `+`"
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

    // The lone `trait` keyword span sits at offset 0..5 in source. It
    // must NOT appear in the chain for a cursor on line 2.
    walk_chain(first, &mut |node| {
        let (s, e) = range_byte_span(src, node);
        assert!(
            !(s == 0 && e == 5),
            "the lone `trait` keyword span (offset 0..5) must never \
             appear in the chain for a cursor in an unrelated decl; \
             got {node:?}",
        );
    });

    // The outermost chain element must be anchored at or after the
    // `fn main` decl (byte offset of `fn main` substring).
    let root = outermost(first);
    let (root_start, _) = range_byte_span(src, root);
    let fn_main_start = src.find("fn main").expect("`fn main` exists in source");
    assert!(
        root_start >= fn_main_start,
        "outermost chain element must be anchored in the cursor's enclosing \
         decl (`fn main` at byte {fn_main_start}); got root starting at byte \
         {root_start}: {root:?}",
    );

    client.shutdown();
}
