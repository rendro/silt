//! End-to-end LSP tests for `textDocument/typeDefinition` and
//! `textDocument/implementation`.
//!
//! Uses the shared LSP client in `support.rs`, which spawns
//! `silt lsp` as a subprocess and speaks LSP JSON-RPC over stdio.

use serde_json::{Value, json};

use crate::support::LspClient;

// ── Tests ──────────────────────────────────────────────────────────

#[test]
fn type_definition_jumps_to_user_type() {
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_typedef_point.silt";
    // Line 0: `type Point { x: Int, y: Int }`
    // Line 1: (blank)
    // Line 2: `fn main() { let p = Point { x: 1, y: 2 } p }`
    let src = "type Point { x: Int, y: Int }\n\nfn main() { let p = Point { x: 1, y: 2 } p }\n";
    client.did_open_and_wait(file, src);

    // Click on the trailing `p` at the end of main's body. The `p` we
    // land on sits right before the closing `}`. Its inferred type is
    // the record `Point`, so typeDefinition should jump to the type
    // decl on line 0.
    let line2 = "fn main() { let p = Point { x: 1, y: 2 } p }";
    let p_col = line2.rfind(" p ").unwrap() + 1; // index of the trailing `p`
    let resp = client.request(
        "textDocument/typeDefinition",
        json!({
            "textDocument": { "uri": file },
            "position": { "line": 2, "character": p_col }
        }),
    );
    let result = resp
        .get("result")
        .unwrap_or_else(|| panic!("expected typeDefinition result; got {resp}"));
    assert!(
        !result.is_null(),
        "typeDefinition should not be null; got {resp}"
    );
    let location = match result {
        Value::Object(_) => result.clone(),
        Value::Array(arr) if !arr.is_empty() => arr[0].clone(),
        _ => panic!("unexpected result shape: {result}"),
    };
    assert_eq!(
        location.get("uri").and_then(|v| v.as_str()),
        Some(file),
        "type definition should live in the same file; got {location}"
    );
    let start_line = location
        .pointer("/range/start/line")
        .and_then(|v| v.as_u64())
        .expect("range.start.line");
    assert_eq!(
        start_line, 0,
        "Point's declaration is on line 0; got line {start_line}"
    );
    client.shutdown();
}

#[test]
fn implementation_lists_trait_impls() {
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_impl_foo.silt";
    // A trait `Foo` with two impls. We click on the `Foo` reference
    // inside the first `trait Foo for A` header (which is itself a
    // trait_name ident reference recognised by find_ident_at_offset
    // via the surrounding program). The handler walks every open doc's
    // decls so the two impls are returned.
    let src = "\
trait Foo { fn m(self) -> Int }
type A { n: Int }
type B { n: Int }
trait Foo for A { fn m(self) -> Int { 1 } }
trait Foo for B { fn m(self) -> Int { 2 } }
fn caller() { Foo }
";
    client.did_open_and_wait(file, src);

    // The last line, `fn caller() { Foo }`, places `Foo` as an Ident
    // expression inside a function body — exactly where
    // `find_ident_at_offset` can recognise it. Column of `Foo` on
    // line 5 is 14 (0-based). Any column inside the 3-letter name works.
    let resp = client.request(
        "textDocument/implementation",
        json!({
            "textDocument": { "uri": file },
            "position": { "line": 5, "character": 15 }
        }),
    );
    let result = resp
        .get("result")
        .unwrap_or_else(|| panic!("expected implementation result; got {resp}"));
    let arr = match result {
        Value::Array(arr) => arr.clone(),
        Value::Object(_) => vec![result.clone()],
        _ => panic!("unexpected result shape: {result}"),
    };
    assert_eq!(
        arr.len(),
        2,
        "expected 2 trait impls for Foo; got {} — {arr:?}",
        arr.len()
    );
    for loc in &arr {
        assert_eq!(
            loc.get("uri").and_then(|v| v.as_str()),
            Some(file),
            "impl location should be in the opened file; got {loc}"
        );
    }
    client.shutdown();
}
