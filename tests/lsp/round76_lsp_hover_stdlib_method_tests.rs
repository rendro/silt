//! Round-76 D3: hover on a stdlib module-method reference must NOT
//! render the top-of-hover signature with an unresolved TyVar
//! (`Fn(String) -> _`). The markdown body below the `---` is
//! authoritative; the redundant top signature must agree (or, where
//! that's hard, be suppressed when it would carry unresolved vars).
//!
//! Lock: real LSP `textDocument/hover` request, asserting the response
//! does NOT contain `-> _` for `string.length`. Test must FAIL before
//! the fix and PASS after.

use serde_json::json;

use crate::support::LspClient;

// ── Tests ──────────────────────────────────────────────────────────

/// Hover on `length` in `string.length(s)` when the user FORGOT
/// `import string`. The bug:
///
///     ```silt
///     Fn(String) -> _
///     ```
///     effects: !{}
///     ---
///     string.length(s: String) -> Int
///     ...
///
/// The top fenced block contradicted the markdown body: an unresolved
/// `Type::Var` in the return slot rendered as `_`. Cause: the
/// FieldAccess arm of inference (src/typechecker/inference.rs:2329)
/// stores a fresh `Type::Var` for unimported-builtin-module references
/// instead of the scheme's actual type, then the Call arm partially
/// unifies it (param side only) so post-resolve `expr.ty` becomes
/// `Fn(String, fresh_ret)` with `fresh_ret` never unified. The fix
/// suppresses the top signature block whenever the rendered type has
/// unresolved TyVars AND a markdown signature is available below.
#[test]
fn hover_on_string_length_does_not_render_unresolved_return_type() {
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_r76_hover_string_length.silt";
    // No `import string` — the FieldAccess arm of inference will record
    // a fresh TyVar for the qualified reference; the Call arm later
    // partially unifies it so post-resolve we get `Fn(String, Var(N))`.
    let src = "fn main() {\n  let s = \"hi\"\n  string.length(s)\n}\n";
    client.did_open_and_wait(file, src);

    // line 2 = `  string.length(s)` ; cursor at character 12 lands
    // inside `length`.
    let resp = client.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": file },
            "position": { "line": 2, "character": 12 }
        }),
    );

    let value = resp
        .pointer("/result/contents/value")
        .and_then(|v| v.as_str())
        .expect("hover result has markdown value");

    // The bug: hover renders `Fn(String) -> _` at the top. Lock that we
    // never produce that text. Also lock that no `-> _` appears in any
    // form (covers `Fn(_, _) -> _` shapes for sibling cases).
    assert!(
        !value.contains("-> _"),
        "hover for `string.length` must not render `-> _` (unresolved \
         TyVar in return position); got:\n{value}"
    );
    // Affirmative: we still surface the markdown signature with the
    // resolved return type `Int`.
    assert!(
        value.contains("string.length(s: String) -> Int"),
        "hover for `string.length` should surface the markdown signature; got:\n{value}"
    );

    client.shutdown();
}

/// Affirmative companion: when `import string` IS present and the call
/// fully unifies, the top signature block IS rendered (with the
/// resolved return type). Locks that the D3 fix is narrowly scoped to
/// the unresolved-var case and does not regress the imported path.
#[test]
fn hover_imported_string_length_renders_top_signature_with_int() {
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_r76_hover_imported_ok.silt";
    let src = "import string\nfn main() {\n  let s = \"hi\"\n  string.length(s)\n}\n";
    client.did_open_and_wait(file, src);

    // line 3 col 12 = inside `length`.
    let resp = client.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": file },
            "position": { "line": 3, "character": 12 }
        }),
    );
    let value = resp
        .pointer("/result/contents/value")
        .and_then(|v| v.as_str())
        .expect("hover result has markdown value");

    // Top signature renders with resolved Int return.
    assert!(
        value.contains("Fn(String) -> Int"),
        "imported `string.length` hover should render top signature with \
         resolved Int return; got:\n{value}"
    );
    // And the markdown signature is also present.
    assert!(
        value.contains("string.length(s: String) -> Int"),
        "markdown signature should be present; got:\n{value}"
    );

    client.shutdown();
}
