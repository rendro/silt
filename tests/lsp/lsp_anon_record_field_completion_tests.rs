//! LSP regression tests for dot-completion on bindings whose typechecked
//! type is `Type::AnonRecord { .. }`.
//!
//! Before the fix, `record_fields_from_type` (src/lsp/fields.rs) only
//! matched `Type::Record` and `Type::Generic`, so a `let p: Point = { x:
//! 1, y: 2 }` (where the typechecker assigns `Type::AnonRecord` to `p`)
//! produced an empty completion list when the user requested `p.|`.
//! These tests exercise the LSP via the subprocess JSON-RPC scaffold —
//! the bug lives in the LSP path, not the typechecker, so unit tests on
//! `record_fields_from_type` alone wouldn't catch a regression here.
//!
//! The two scenarios:
//!   1. Named type annotation with a record-literal initializer
//!      (`let p: Point = { x: 1, y: 2 }`).
//!   2. Pure anonymous-record binding (`let p = { x: 1, y: 2 }`).

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

use crate::support::LspClient;

static URI_COUNTER: AtomicU64 = AtomicU64::new(1);
// `CompletionItemKind::FIELD` = 5 (LSP 3.17 spec).  We compare numerics
// in JSON rather than pulling in the `lsp_types` crate just for one enum.
const COMPLETION_ITEM_KIND_FIELD: u64 = 5;

fn unique_uri(tag: &str) -> String {
    let n = URI_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("file:///tmp/silt_lsp_anon_field_{tag}_{n}.silt")
}

/// Pull `(label, kind)` pairs out of either the bare-array or
/// `CompletionList` response shapes.
fn extract_completion_items(result: &Value) -> Vec<(String, Option<u64>)> {
    let arr = if let Some(arr) = result.as_array() {
        arr.clone()
    } else if let Some(arr) = result.pointer("/items").and_then(|v| v.as_array()) {
        arr.clone()
    } else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|it| {
            let label = it.get("label").and_then(|l| l.as_str())?.to_string();
            let kind = it.get("kind").and_then(|k| k.as_u64());
            Some((label, kind))
        })
        .collect()
}

fn assert_field(items: &[(String, Option<u64>)], expected_label: &str, resp: &Value) {
    let found = items.iter().find(|(l, _)| l == expected_label);
    let Some((_, kind)) = found else {
        panic!(
            "completion missing field `{expected_label}`; got {} items: {:?}; full resp: {}",
            items.len(),
            items.iter().map(|(l, _)| l).collect::<Vec<_>>(),
            resp
        );
    };
    assert_eq!(
        *kind,
        Some(COMPLETION_ITEM_KIND_FIELD),
        "completion item `{expected_label}` must have kind FIELD ({COMPLETION_ITEM_KIND_FIELD}); \
         got kind={kind:?}; full resp: {resp}"
    );
}

// ── Field completion via named record annotation ───────────────────

#[test]
fn field_completion_on_user_record_via_anon_type_returns_fields() {
    // Source layout:
    //   line 0: `type Point = { x: Int, y: Int }`
    //   line 1: `fn main() {`
    //   line 2: `  let p: Point = { x: 1, y: 2 }`
    //   line 3: `  println(p.x)`
    //   line 4: `}`
    //
    // The typechecker assigns `Type::AnonRecord { .. }` (not
    // `Type::Record(Point, ...)`) to `p` because the initialiser is a
    // bare record literal. Before the fix, `record_fields_from_type`
    // only handled `Type::Record` / `Type::Generic`, so dot-completion
    // on `p.|x` fell through and returned zero items.
    //
    // Cursor: line 3, character = 12 — the column just before the `x`
    // in `println(p.x)` (after the `.`):
    //
    //   `  println(p.x)`
    //    0         1
    //    0123456789012
    //              ^ char=12 is the position of `x`; trigger char `.`
    let mut client = LspClient::spawn();
    let uri = unique_uri("named_anon_record");
    let text = "type Point = { x: Int, y: Int }\nfn main() {\n  let p: Point = { x: 1, y: 2 }\n  println(p.x)\n}\n";
    client.did_open_and_wait(&uri, text);

    let resp = client.request(
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 3, "character": 12 },
            "context": { "triggerKind": 2, "triggerCharacter": "." }
        }),
    );
    let result = resp.get("result").expect("completion has result");
    let items = extract_completion_items(result);
    assert!(
        !items.is_empty(),
        "completion on `p.|x` must NOT return zero items for a user record \
         (regression: anon-record blind spot in record_fields_from_type); resp={resp}"
    );
    assert_field(&items, "x", &resp);
    assert_field(&items, "y", &resp);
    client.shutdown();
}

// ── Field completion via pure anonymous record ─────────────────────

#[test]
fn field_completion_on_anon_record_literal_returns_fields() {
    // Source layout (no named type — the binder is purely structural):
    //   line 0: `fn main() {`
    //   line 1: `  let p = { x: 1, y: 2 }`
    //   line 2: `  println(p.x)`
    //   line 3: `}`
    //
    // Cursor on the `x` in `p.x` (line 2, character=12):
    //   `  println(p.x)`
    //    0         1
    //    0123456789012
    //              ^ char=12
    let mut client = LspClient::spawn();
    let uri = unique_uri("pure_anon_record");
    let text = "fn main() {\n  let p = { x: 1, y: 2 }\n  println(p.x)\n}\n";
    client.did_open_and_wait(&uri, text);

    let resp = client.request(
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 2, "character": 12 },
            "context": { "triggerKind": 2, "triggerCharacter": "." }
        }),
    );
    let result = resp.get("result").expect("completion has result");
    let items = extract_completion_items(result);
    assert!(
        !items.is_empty(),
        "completion on `p.|x` must NOT return zero items for an anon-record \
         binding (regression: anon-record blind spot in \
         record_fields_from_type); resp={resp}"
    );
    assert_field(&items, "x", &resp);
    assert_field(&items, "y", &resp);
    client.shutdown();
}
