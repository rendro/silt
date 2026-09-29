//! Regression tests for Finding F8:
//!
//! Hover and goto-definition on identifiers introduced by a destructuring
//! `let` pattern used to return `null` because
//! `src/lsp/local_bindings.rs` (for block-scope `Stmt::Let`) and
//! `src/lsp/definitions.rs` (for top-level `Decl::Let`) only extracted
//! `PatternKind::Ident`. Tuple, record, and constructor patterns were
//! skipped entirely, so `let (a, b) = (1, 2)` left `a` and `b` invisible
//! to the LSP.
//!
//! Each test drives the real parse + typecheck + LSP hover/goto pipeline
//! via the stdio transport the way a client (VS Code, etc.) would, so it
//! locks down the end-to-end behaviour on both the local-bindings path
//! and the top-level `definitions` path.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;

use crate::support::{LspClient, next_id};

static URI_COUNTER: AtomicU64 = AtomicU64::new(1);

fn unique_uri() -> String {
    let n = URI_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("file:///tmp/silt_lsp_destructured_{n}.silt")
}

// ── Helpers ────────────────────────────────────────────────────────

fn hover_value_at(client: &mut LspClient, uri: &str, line: u32, character: u32) -> Option<String> {
    let id = next_id();
    client.send_request(
        id,
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        }),
    );
    let resp = client.recv_response_for(id);
    assert!(
        resp.get("error").is_none(),
        "hover request returned an error: {resp}"
    );
    let result = resp.get("result")?;
    if result.is_null() {
        return None;
    }
    Some(
        result
            .pointer("/contents/value")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("hover result missing contents.value: {result}"))
            .to_string(),
    )
}

fn goto_def_at(
    client: &mut LspClient,
    uri: &str,
    line: u32,
    character: u32,
) -> Option<(String, u64, u64)> {
    let id = next_id();
    client.send_request(
        id,
        "textDocument/definition",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        }),
    );
    let resp = client.recv_response_for(id);
    assert!(
        resp.get("error").is_none(),
        "definition request returned an error: {resp}"
    );
    let result = resp.get("result")?;
    if result.is_null() {
        return None;
    }
    let def_uri = result.get("uri").and_then(|v| v.as_str())?.to_string();
    let line = result
        .pointer("/range/start/line")
        .and_then(|v| v.as_u64())?;
    let character = result
        .pointer("/range/start/character")
        .and_then(|v| v.as_u64())?;
    Some((def_uri, line, character))
}

// ── Tests ──────────────────────────────────────────────────────────

// ── 1. Tuple destructure: hover + goto on usage of `a` ─────────────

#[test]
fn test_hover_on_tuple_destructure_usage() {
    // GAP (F8): `let (a, b) = (1, 2)` used to leave `a` and `b` invisible
    // to the LSP — hover on their usage returned null because the
    // binding was never registered as a local.  After the fix, hover
    // on the usage of `a` in `println(a + b)` must resolve to `Int`.
    //
    //   line 0: fn main() {
    //   line 1:   let (a, b) = (1, 2)
    //   line 2:   println(a + b)
    //   line 3: }
    let source = "fn main() {\n  let (a, b) = (1, 2)\n  println(a + b)\n}\n";

    let mut client = LspClient::spawn();
    let uri = unique_uri();
    client.did_open_and_wait(&uri, source);

    // Line 2, column 10 is the `a` of `println(a + b)`:
    //   "  println(a + b)"
    //    0         1
    //    0123456789012
    let value = hover_value_at(&mut client, &uri, 2, 10)
        .expect("hover on usage of `a` must return a non-null result");
    assert!(
        value.contains("Int"),
        "hover on usage of `a` must resolve to `Int`, got: {value}"
    );

    client.shutdown();
}

#[test]
fn test_goto_def_on_tuple_destructure_usage() {
    // GAP (F8): goto-definition on a tuple-destructured binding used to
    // return `null`.  After the fix, goto-def on the usage of `a` must
    // point at the `a` in the pattern `(a, b)` on line 1 — column 7.
    //
    //   line 1: "  let (a, b) = (1, 2)"
    //            0         1
    //            0123456789012
    //                  ^ col 7 = `a`
    let source = "fn main() {\n  let (a, b) = (1, 2)\n  println(a + b)\n}\n";

    let mut client = LspClient::spawn();
    let uri = unique_uri();
    client.did_open_and_wait(&uri, source);

    // Goto-def on the `a` of `println(a + b)` on line 2, col 10.
    let (def_uri, line, character) = goto_def_at(&mut client, &uri, 2, 10)
        .expect("goto-def on usage of `a` must return a non-null result");
    assert_eq!(
        def_uri, uri,
        "definition must point back into this document"
    );
    assert_eq!(
        line, 1,
        "definition of `a` should be on line 1 (the `let` pattern), got {line}"
    );
    assert_eq!(
        character, 7,
        "definition of `a` should be at column 7 (the `a` in `(a, b)`), got {character}"
    );

    client.shutdown();
}

// ── 2. Nested tuple destructure: hover on each leaf ────────────────

#[test]
fn test_hover_on_nested_tuple_destructure_usage() {
    // GAP (F8): a nested destructure `let ((a, b), c) = ((1, 2), 3)`
    // must register `a`, `b`, and `c` as bindings with their resolved
    // element types.  Hover on each usage must return `Int`.
    //
    //   line 0: fn main() {
    //   line 1:   let ((a, b), c) = ((1, 2), 3)
    //   line 2:   println(a + b + c)
    //   line 3: }
    let source = "fn main() {\n  let ((a, b), c) = ((1, 2), 3)\n  println(a + b + c)\n}\n";

    let mut client = LspClient::spawn();
    let uri = unique_uri();
    client.did_open_and_wait(&uri, source);

    // Line 2: "  println(a + b + c)"
    //          0         1
    //          0123456789012345678
    // col 10 = a, col 14 = b, col 18 = c
    for (col, name) in [(10u32, "a"), (14u32, "b"), (18u32, "c")] {
        let value = hover_value_at(&mut client, &uri, 2, col)
            .unwrap_or_else(|| panic!("hover on usage of `{name}` must return a non-null result"));
        assert!(
            value.contains("Int"),
            "hover on usage of `{name}` must resolve to `Int`, got: {value}"
        );
    }

    client.shutdown();
}

// ── 3. Record destructure: hover on usage ──────────────────────────

#[test]
fn test_hover_on_record_destructure_usage() {
    // GAP (F8): `let P { x, y } = P { x: 1, y: 2 }` must register `x`
    // and `y` as bindings whose types are propagated from the record's
    // declared field types. Hover on their usage must resolve to `Int`.
    //
    //   line 0: type P { x: Int, y: Int }
    //   line 1: fn main() {
    //   line 2:   let P { x, y } = P { x: 1, y: 2 }
    //   line 3:   println(x + y)
    //   line 4: }
    let source = "type P { x: Int, y: Int }\n\
                  fn main() {\n  \
                  let P { x, y } = P { x: 1, y: 2 }\n  \
                  println(x + y)\n\
                  }\n";

    let mut client = LspClient::spawn();
    let uri = unique_uri();
    client.did_open_and_wait(&uri, source);

    // Line 3: "  println(x + y)"
    //          0         1
    //          01234567890123
    // col 10 = x, col 14 = y
    for (col, name) in [(10u32, "x"), (14u32, "y")] {
        let value = hover_value_at(&mut client, &uri, 3, col)
            .unwrap_or_else(|| panic!("hover on usage of `{name}` must return a non-null result"));
        assert!(
            value.contains("Int"),
            "hover on usage of `{name}` must resolve to `Int`, got: {value}"
        );
    }

    client.shutdown();
}

// ── 4. Top-level destructure: goto-def on usage ────────────────────

#[test]
fn test_goto_def_on_top_level_tuple_destructure_usage() {
    // GAP (F8): `let (a, b) = (1, 2)` at module level — the
    // `definitions.rs` path — used to skip destructuring patterns.
    // After the fix, goto-def on a usage of `a` inside `main` must
    // point back at the `a` on line 0 (column 5 in `let (a, b) = ...`).
    //
    //   line 0: let (a, b) = (1, 2)
    //           0         1
    //           0123456789
    //               ^ col 5 = `a`
    //   line 1: fn main() { println(a + b) }
    let source = "let (a, b) = (1, 2)\nfn main() { println(a + b) }\n";

    let mut client = LspClient::spawn();
    let uri = unique_uri();
    client.did_open_and_wait(&uri, source);

    // Line 1: "fn main() { println(a + b) }"
    //          0         1         2
    //          0123456789012345678901234567
    // The `a` of `println(a + b)` is at column 20.
    let (def_uri, line, character) = goto_def_at(&mut client, &uri, 1, 20)
        .expect("goto-def on top-level destructured `a` must return a non-null result");
    assert_eq!(
        def_uri, uri,
        "definition must point back into this document"
    );
    assert_eq!(
        line, 0,
        "definition of top-level `a` should be on line 0, got {line}"
    );
    assert_eq!(
        character, 5,
        "definition of top-level `a` should be at column 5 (the `a` in `(a, b)`), got {character}"
    );

    client.shutdown();
}
