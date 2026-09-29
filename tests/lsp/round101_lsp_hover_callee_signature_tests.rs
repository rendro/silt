//! Round-101 GAP regression: LSP hover on a call's CALLEE identifier
//! must show the function's signature, not the call's RESULT type.
//!
//! Before the fix, the typechecker's named-callee shortcut (the
//! `ExprKind::Call` / `ExprKind::Pipe` arms in
//! `src/typechecker/inference.rs`) looked the callee scheme up directly
//! and never stashed `callee.ty`, so the LSP expression walk fell back
//! to the enclosing Call node's result type: hover on `add` in
//! `let r = add(1, 2)` rendered a signature block of `Int` (while the
//! effects/doc blocks in the same hover described the FUNCTION), and
//! hover on `println` rendered a bare `()`. Qualified callees
//! (`list.sum`) already stashed the instantiated fn type — this fix
//! makes bare callees consistent with them.
//!
//! Uses the shared LSP client in `support.rs`.

use serde_json::json;

use crate::support::LspClient;

fn hover_value(client: &mut LspClient, uri: &str, line: u32, character: u32) -> String {
    let resp = client.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        }),
    );
    let result = resp.get("result").expect("hover has result");
    assert!(!result.is_null(), "hover must not be null; got {resp}");
    result
        .pointer("/contents/value")
        .and_then(|v| v.as_str())
        .expect("hover.contents.value is a string")
        .to_string()
}

// Shared source under test:
//   line 0: fn add(a: Int, b: Int) -> Int { a + b }
//   line 1: fn main() {
//   line 2:   let r = add(1, 2)
//   line 3:   println(r)
//   line 4: }
const SOURCE: &str =
    "fn add(a: Int, b: Int) -> Int { a + b }\nfn main() {\n  let r = add(1, 2)\n  println(r)\n}\n";

// ── Tests ──────────────────────────────────────────────────────────

#[test]
fn hover_on_user_fn_callee_shows_signature() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_hover_callee_user_fn.silt";
    client.did_open_and_wait(uri, SOURCE);

    // Cursor on `add` in `let r = add(1, 2)` (line 2, `add` spans
    // characters 10..13).
    let value = hover_value(&mut client, uri, 2, 11);
    assert!(
        value.contains("Fn(Int, Int) -> Int"),
        "hover on the callee `add` must show the fn signature \
         `Fn(Int, Int) -> Int`, not the call's result type; got {value:?}"
    );
    assert!(
        !value.contains("```silt\nInt\n```"),
        "hover on the callee `add` must not render the call RESULT type \
         as the signature block; got {value:?}"
    );
    client.shutdown();
}

#[test]
fn hover_on_println_callee_is_not_bare_unit() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_hover_callee_println.silt";
    client.did_open_and_wait(uri, SOURCE);

    // Cursor on `println` in `println(r)` (line 3, `println` spans
    // characters 2..9).
    let value = hover_value(&mut client, uri, 3, 4);
    assert!(
        !value.contains("```silt\n()\n```"),
        "hover on the callee `println` must not render a bare `()` \
         signature block (the call's result type); got {value:?}"
    );
    client.shutdown();
}

#[test]
fn hover_on_let_binder_still_shows_call_result() {
    // Control: the whole-call result position (the `let` binder) keeps
    // showing the call's result type, not the callee's signature.
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_hover_callee_control_binder.silt";
    client.did_open_and_wait(uri, SOURCE);

    // Cursor on `r` in `let r = add(1, 2)` (line 2, character 6).
    let value = hover_value(&mut client, uri, 2, 6);
    assert!(
        value.contains("Int"),
        "hover on the let binder must show the call result `Int`; got {value:?}"
    );
    assert!(
        !value.contains("Fn("),
        "hover on the let binder must not show the callee signature; got {value:?}"
    );
    client.shutdown();
}

#[test]
fn hover_on_piped_callee_shows_signature() {
    // The Pipe arm has the same named-callee shortcut as the Call arm;
    // lock it too.
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_hover_callee_piped.silt";
    let source = "fn add(a: Int, b: Int) -> Int { a + b }\nfn main() {\n  let s = 1 |> add(2)\n  println(s)\n}\n";
    client.did_open_and_wait(uri, source);

    // Cursor on `add` in `1 |> add(2)` (line 2, `add` spans
    // characters 15..18).
    let value = hover_value(&mut client, uri, 2, 16);
    assert!(
        value.contains("Fn(Int, Int) -> Int"),
        "hover on a piped callee must show the fn signature; got {value:?}"
    );
    client.shutdown();
}
