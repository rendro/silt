//! Round-81 DX-G1 regression: LSP rename / prepareRename must allow
//! renaming a USER LOCAL whose lexeme happens to match a builtin name.
//!
//! Background: silt allows shadowing builtins via let-bindings, fn
//! declarations, type/trait declarations, etc. Before this fix the
//! rename gate (`is_user_renameable`) was a name-only check that
//! refused to rename anything spelled like a builtin (`Int`, `Ok`,
//! `println`, …) regardless of whether the cursor sat on a user
//! binding or a real builtin reference. A user who wrote
//! `let Int = 42` could not rename their own `Int` — the LSP refused.
//!
//! Fix: scope-aware gate. If the cursor's symbol resolves to a user
//! binding visible at this cursor (top-level def in this doc, local
//! binding identifier the cursor sits on, OR a local with this name in
//! scope), rename is allowed regardless of lexeme. Only when there is
//! NO user binding for this name in scope at the cursor do we fall
//! back to the historical `is_user_renameable` name filter.
//!
//! Uses the shared LSP client in `support.rs`.

use serde_json::json;

use crate::support::LspClient;

// ── Tests ──────────────────────────────────────────────────────────

/// (a) DX-G1 core: a user `let println = 42` should be renameable. The
/// rename request fires on the use-site (`println` in
/// `fn main() { println }`); the response must contain a non-empty
/// WorkspaceEdit covering both the binding identifier on line 0
/// (col 4..11) and the use-site on line 1 (col 12..19) — NOT a
/// "is a builtin" rejection.
///
/// NOTE on the spec example: the round-81 task description used
/// `let Int = 42` as the canonical case. silt's parser treats any
/// capitalised identifier (`Int`, `Ok`, `Some`, …) as a *constructor*
/// pattern, never a binder — so `let Int = 42` is rejected at parse
/// time before the LSP gate even runs. The DX-G1 fix is about the
/// scope-aware gate, which is equally exercised by any user binding
/// that shadows a builtin name. We use `println` (a lowercase free
/// function builtin) as the renameable shadow here. The same
/// renameability applies to `fn println` shadowing the builtin too —
/// covered by the secondary test below.
///
/// Source layout (0-based char offsets in comments, matching LSP):
///   line 0: `let println = 42`        — `println` binder at col 4..11
///   line 1: `fn main() { println }`   — `println` use-site at col 12..19
#[test]
fn rename_user_local_shadowing_builtin_int_succeeds() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_rn_user_shadow_println.silt";
    let source = "let println = 42\nfn main() { println }\n";
    client.did_open_and_wait(uri, source);

    // Cursor on the use-site `println` at line 1, char 12.
    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 1, "character": 12 },
            "newName": "n"
        }),
    );

    // No error — the user-shadowed `println` is renameable.
    assert!(
        resp.get("error").is_none(),
        "rename of user-shadowed `println` must NOT be rejected; got {resp}"
    );
    let result = resp.get("result").expect("rename has result");
    assert!(
        !result.is_null(),
        "rename of user-shadowed `println` must produce a WorkspaceEdit, not null; got {resp}"
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
        "expected at least 2 edits (binder + use-site); got {}: {edits:?}",
        edits.len()
    );

    // Locate the binder edit (line 0) and the use-site edit (line 1).
    let mut saw_binder = false;
    let mut saw_use_site = false;
    for edit in edits {
        let line = edit.pointer("/range/start/line").and_then(|v| v.as_u64());
        let start_ch = edit
            .pointer("/range/start/character")
            .and_then(|v| v.as_u64());
        let end_ch = edit
            .pointer("/range/end/character")
            .and_then(|v| v.as_u64());
        let new_text = edit.get("newText").and_then(|v| v.as_str());
        assert_eq!(new_text, Some("n"), "edit newText must be `n`; got {edit}");
        if line == Some(0) && start_ch == Some(4) && end_ch == Some(11) {
            saw_binder = true;
        }
        if line == Some(1) && start_ch == Some(12) && end_ch == Some(19) {
            saw_use_site = true;
        }
    }
    assert!(
        saw_binder,
        "expected an edit at line 0, col 4..11 (the `let println` binder); got edits {edits:?}"
    );
    assert!(
        saw_use_site,
        "expected an edit at line 1, col 12..19 (the `println` use-site); got edits {edits:?}"
    );

    client.shutdown();
}

/// (a-bis) Same scenario, but cursor on the BINDING site of
/// `let println`. Both directions of cursor placement (binding-site vs
/// use-site) must resolve to the same user-binding renameability.
#[test]
fn rename_user_local_shadowing_builtin_int_from_binder_succeeds() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_rn_user_shadow_println_binder.silt";
    let source = "let println = 42\nfn main() { println }\n";
    client.did_open_and_wait(uri, source);

    // Cursor on the binder `println` at line 0, char 4.
    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 4 },
            "newName": "n"
        }),
    );
    assert!(
        resp.get("error").is_none(),
        "rename of user-shadowed `println` (cursor on binder) must NOT be rejected; got {resp}"
    );
    let result = resp.get("result").expect("rename has result");
    assert!(
        !result.is_null(),
        "rename from binder must produce a WorkspaceEdit, not null; got {resp}"
    );
    client.shutdown();
}

/// (a-ter) Sibling shadow form: `fn println(x) { x }` shadows the
/// builtin via a Decl::Fn declaration. Pre-fix this was rejected as
/// "is a builtin" because the gate was lexeme-only; post-fix the
/// scope-aware gate observes the user fn-decl in `doc.definitions`
/// and allows rename.
#[test]
fn rename_user_fn_decl_shadowing_builtin_println_succeeds() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_rn_user_shadow_fn_println.silt";
    let source = "fn println(x) { x }\nfn main() { println(42) }\n";
    client.did_open_and_wait(uri, source);

    // Cursor on the fn-decl name `println` at line 0, char 3.
    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 3 },
            "newName": "n"
        }),
    );
    assert!(
        resp.get("error").is_none(),
        "rename of user `fn println` must NOT be rejected; got {resp}"
    );
    let result = resp.get("result").expect("rename has result");
    assert!(
        !result.is_null(),
        "rename of user `fn println` must produce a WorkspaceEdit; got {resp}"
    );
    client.shutdown();
}

/// (b) Negative companion: a use-site of an ACTUAL builtin (no
/// user-binding shadowing in scope) must still be rejected.
///
/// Source layout:
///   line 0: `let x: Int = 42`   — `Int` is a TYPE annotation reference
///                                  to the builtin `Int`, not a binder.
///   line 1: `fn main() { println(x) }`
///
/// Two acceptable shapes (matching the gated-constructor rejection
/// test): an explicit error response, or `result: null` / empty
/// `changes` (no edits to apply). A non-empty WorkspaceEdit would mean
/// the LSP started rewriting the builtin `Int` type — that's the bug
/// we are guarding against.
///
/// Note: since round-101, TypeExpr nodes ARE walked, so the cursor on
/// the `Int` annotation resolves to the `Int` symbol. No user binding
/// for `Int` is in scope here, so the gate falls through to
/// `is_user_renameable("Int") == false` and the server answers with an
/// explicit rejection (previously `find_ident_at_offset` returned
/// `None` for type-annotation cursors and the server answered `null`;
/// both shapes are accepted below).
#[test]
fn rename_use_site_of_actual_builtin_int_is_rejected() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_rn_real_builtin_int.silt";
    let source = "let x: Int = 42\nfn main() { println(x) }\n";
    client.did_open_and_wait(uri, source);

    // Cursor on the type annotation `Int` at line 0, char 7.
    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 7 },
            "newName": "n"
        }),
    );

    // Tightened (round 86 L4): the server MUST explicitly respond —
    // either with an `error` (preferred) or with a present `result`
    // that is null / has empty changes. A bare `{}` (no `result`, no
    // `error` — e.g. server stopped responding) must NOT pass.
    let has_error = resp.get("error").is_some();
    let result = resp.get("result");
    let result_present = result.is_some();
    let result_is_null = result.map(|v| v.is_null()).unwrap_or(false);
    let changes_empty = result
        .and_then(|r| r.get("changes"))
        .and_then(|c| c.as_object())
        .map(|o| o.is_empty())
        .unwrap_or(false);
    assert!(
        has_error || (result_present && (result_is_null || changes_empty)),
        "rename on real builtin `Int` must be explicitly rejected \
         (error response) or explicitly empty (present null result / \
         present empty changes); a missing-both shape (no `error`, no \
         `result`) means the server did not respond — got {resp}"
    );
    client.shutdown();
}

/// (b-bis) Stronger guard: cursor on the `println` builtin call
/// (a use-site of a builtin global with no shadowing) must be rejected
/// with the explicit "is a builtin" error message — this exercises the
/// real branch where the cursor DOES resolve to a builtin symbol.
#[test]
fn rename_use_site_of_actual_builtin_println_is_rejected_with_message() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_rn_real_builtin_println.silt";
    let source = "fn main() { println(42) }\n";
    client.did_open_and_wait(uri, source);

    // `println` starts at line 0, char 12.
    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 12 },
            "newName": "n"
        }),
    );
    let err = resp.get("error").expect("expected an error response");
    let msg = err
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or_default();
    assert!(
        msg.contains("is a builtin and cannot be renamed"),
        "expected `is a builtin and cannot be renamed` rejection; got {resp}"
    );
    client.shutdown();
}

/// (c) prepareRename complements the rename behaviour:
///   * cursor on `let println` binder ⇒ Some(Range)
///   * cursor on a real builtin reference (here, `println` use-site
///     in a doc with no user shadow) ⇒ None
///
/// Note: same parser caveat as test (a) — capital-letter pattern
/// binders are constructor patterns in silt, so we exercise the
/// scope-aware gate via a lowercase shadow (`println`).
#[test]
fn prepare_rename_user_shadow_returns_range() {
    let mut client = LspClient::spawn();
    let uri_a = "file:///tmp/silt_pr_user_shadow_println.silt";
    let uri_b = "file:///tmp/silt_pr_real_builtin_println.silt";

    client.did_open_and_wait(uri_a, "let println = 42\nfn main() { println }\n");
    client.did_open_and_wait(uri_b, "fn main() { println(42) }\n");

    // (c.1) prepareRename on the user binder `let println = 42`.
    let resp_a = client.request(
        "textDocument/prepareRename",
        json!({
            "textDocument": { "uri": uri_a },
            "position": { "line": 0, "character": 4 }
        }),
    );
    let result_a = resp_a.get("result").expect("prepareRename has result");
    assert!(
        !result_a.is_null(),
        "prepareRename on user-shadowed `let println` binder must NOT be null; got {resp_a}"
    );

    // (c.2) prepareRename on a real-builtin `println` use-site —
    // no user binding in scope, gate falls back to name-based filter
    // and rejects.
    let resp_b = client.request(
        "textDocument/prepareRename",
        json!({
            "textDocument": { "uri": uri_b },
            "position": { "line": 0, "character": 12 }
        }),
    );
    // Tightened (round 86 L4): server must explicitly respond with a
    // `result` field that is null. A bare `{}` (no `result` at all)
    // means the server did not respond and must NOT pass.
    let result_b = resp_b.get("result").expect(
        "prepareRename on real-builtin must include an explicit \
         `result` field (null is the rejection signal — absence \
         means the server did not respond)",
    );
    assert!(
        result_b.is_null(),
        "prepareRename on real-builtin `println` use-site must be null; got {resp_b}"
    );

    client.shutdown();
}
