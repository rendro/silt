//! Round-60 G4 regression: LSP hover on a `fn` declaration name (the
//! binder, not a call site) must return the function's signature, not
//! `null`.
//!
//! Before the fix, `find_ident_at_offset` walked only `ExprKind::Ident`
//! nodes and never matched the `fn foo` declaration name, so hover at
//! the binder fell through to a no-type response.
//!
//! Uses the shared LSP client in `support.rs`.

use serde_json::json;

use crate::support::LspClient;

// ── Tests ──────────────────────────────────────────────────────────

#[test]
fn hover_on_fn_decl_name() {
    // Source: `fn helper() { 0 }`
    //          0123456789
    // Cursor on `helper` at char=3.
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_hover_fn_decl_name.silt";
    client.did_open_and_wait(uri, "fn helper() { 0 }\nfn main() { helper() }\n");

    let resp = client.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 3 }
        }),
    );
    let result = resp.get("result").expect("hover has result");
    assert!(
        !result.is_null(),
        "hover on fn decl name must NOT be null (round-60 G4); got {resp}"
    );
    // Hover content should mention a type — for a 0-arg Int-returning fn
    // the signature pretty-printer renders as something containing
    // either `Int` (return type) or `()` (params). We just assert the
    // markup is non-empty and contains a `silt` code fence.
    let contents = result
        .get("contents")
        .expect("hover.result.contents is present");
    let value_str = contents
        .get("value")
        .and_then(|v| v.as_str())
        .expect("hover.result.contents.value is a string");
    assert!(
        value_str.contains("silt"),
        "expected hover markdown to contain a `silt` code fence; got {value_str:?}"
    );
    assert!(
        !value_str.trim().is_empty(),
        "hover markdown must not be empty"
    );
    client.shutdown();
}

#[test]
fn hover_on_fn_decl_name_renders_signature_substring() {
    // `fn add(x: Int, y: Int): Int { x + y }` — fully-annotated so the
    // typechecker resolves all type variables, and hover on `add`
    // produces a non-empty type with no `Var(_)` placeholders. Without
    // the round-60 fix, `find_ident_at_offset` would not match the
    // `add` binder and hover would return null.
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_hover_fn_decl_sig.silt";
    client.did_open_and_wait(
        uri,
        "fn add(x: Int, y: Int) -> Int { x + y }\nfn main() { add(1, 2) }\n",
    );

    let resp = client.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 3 }
        }),
    );
    let result = resp.get("result").expect("hover has result");
    assert!(
        !result.is_null(),
        "hover on fn decl name must NOT be null; got {resp}"
    );
    let contents_value = result
        .get("contents")
        .and_then(|c| c.get("value"))
        .and_then(|v| v.as_str())
        .expect("hover contents.value is a string");
    // Pre-fix the assertion accepted any hover containing `->` OR `Int`,
    // which would pass even if hover returned just `Int`. Tighten: require
    // the return-type arrow AND a parameter shape — either the pretty
    // `(Int, Int)` form or the raw `fn add` declaration prefix.
    assert!(
        contents_value.contains("->"),
        "hover did not render return-type arrow; got {contents_value:?}"
    );
    assert!(
        contents_value.contains("(Int, Int)") || contents_value.contains("fn add"),
        "hover did not render signature parameters or fn decl; got {contents_value:?}"
    );
    client.shutdown();
}
