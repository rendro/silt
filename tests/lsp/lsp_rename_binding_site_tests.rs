//! Round-60 B8 + G4 regression: LSP rename / prepareRename must work
//! when the cursor sits on a *binding* site (the LHS of a `let`, a
//! `fn` parameter pattern, or a `fn` declaration name) — not only on a
//! use-site.
//!
//! Before the fix, `find_ident_at_offset` walked only `ExprKind::Ident`
//! nodes, so:
//!   * `prepareRename` on a binder returned `null`
//!   * `rename` on a binder returned `null`
//!
//! Even though the references collector in `workspace.rs` already
//! covered binding sites once the symbol was known, the initial cursor
//! lookup never produced a symbol from a binder. This test locks the
//! end-to-end LSP behaviour via the stdio transport.
//!
//! Uses the shared LSP client in `support.rs`.

use serde_json::json;

use crate::support::LspClient;

// ── Tests ──────────────────────────────────────────────────────────

#[test]
fn rename_from_let_binding_site() {
    // Source layout — cursor on `xvar` binder of `let xvar = 42`:
    //   line 0: `fn main() {`
    //   line 1: `  let xvar = 42`
    //                ^^^^ starts at char=6
    //   line 2: `  println(xvar)`
    //   line 3: `}`
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_rn_let_binder.silt";
    client.did_open_and_wait(uri, "fn main() {\n  let xvar = 42\n  println(xvar)\n}\n");

    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 1, "character": 6 },
            "newName": "renamed_xvar"
        }),
    );
    let result = resp.get("result").expect("rename has result");
    assert!(
        !result.is_null(),
        "rename on let binder must NOT return null (round-60 B8); got {resp}"
    );
    let changes = result
        .get("changes")
        .and_then(|c| c.as_object())
        .expect("rename result has changes");
    let edits = changes
        .get(uri)
        .and_then(|v| v.as_array())
        .expect("file edits");
    assert!(
        edits.len() >= 2,
        "expected at least 2 edits (binder + use); got {}: {edits:?}",
        edits.len()
    );
    client.shutdown();
}

#[test]
fn prepare_rename_from_let_binding_site() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_pr_let_binder.silt";
    client.did_open_and_wait(uri, "fn main() {\n  let xvar = 42\n  println(xvar)\n}\n");

    let resp = client.request(
        "textDocument/prepareRename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 1, "character": 6 }
        }),
    );
    let result = resp.get("result").expect("prepareRename has result");
    assert!(
        !result.is_null(),
        "prepareRename on let binder must NOT return null (round-60 B8); got {resp}"
    );
    client.shutdown();
}

#[test]
fn rename_from_fn_param_binding_site() {
    // `fn add(x, y) { x + y }` — cursor on `x` parameter binder.
    // Line 0: `fn add(x, y) { x + y }`
    //          0123456789012
    // `x` at char=7
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_rn_fn_param.silt";
    client.did_open_and_wait(uri, "fn add(x, y) { x + y }\n");

    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 7 },
            "newName": "renamed_x"
        }),
    );
    let result = resp.get("result").expect("rename has result");
    assert!(
        !result.is_null(),
        "rename on fn-param binder must NOT return null (round-60 B8); got {resp}"
    );
    let changes = result
        .get("changes")
        .and_then(|c| c.as_object())
        .expect("rename result has changes");
    let edits = changes
        .get(uri)
        .and_then(|v| v.as_array())
        .expect("file edits");
    assert!(
        edits.len() >= 2,
        "expected at least 2 edits (binder + use); got {}: {edits:?}",
        edits.len()
    );
    client.shutdown();
}

#[test]
fn rename_from_fn_decl_name() {
    // `fn helper() { 0 }\nfn main() { helper() }` — cursor on `helper`
    // declaration name. Line 0: `fn helper() { 0 }`
    //                            0123456789
    // `helper` at char=3
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_rn_fn_decl_name.silt";
    client.did_open_and_wait(uri, "fn helper() { 0 }\nfn main() { helper() }\n");

    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 3 },
            "newName": "renamed_helper"
        }),
    );
    let result = resp.get("result").expect("rename has result");
    assert!(
        !result.is_null(),
        "rename on fn-decl name binder must NOT return null (round-60 B8); got {resp}"
    );
    let changes = result
        .get("changes")
        .and_then(|c| c.as_object())
        .expect("rename result has changes");
    let edits = changes
        .get(uri)
        .and_then(|v| v.as_array())
        .expect("file edits");
    assert!(
        edits.len() >= 2,
        "expected at least 2 edits (decl + call); got {}: {edits:?}",
        edits.len()
    );
    client.shutdown();
}

#[test]
fn rename_from_use_site_still_works() {
    // Positive guard — the existing use-site path must not regress.
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_rn_use_site_guard.silt";
    client.did_open_and_wait(uri, "fn helper() { 0 }\nfn main() { helper() }\n");

    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            // Cursor on `helper()` call: line 1, the `h` is at char=12.
            "position": { "line": 1, "character": 12 },
            "newName": "fresh"
        }),
    );
    let result = resp.get("result").expect("rename has result");
    assert!(
        !result.is_null(),
        "rename from use-site must continue to work; got {resp}"
    );
    client.shutdown();
}
