//! Round-60 L5 regression: behavioural LSP test that rename on a
//! gated builtin constructor (e.g. `IoNotFound`) is rejected.
//!
//! `rename.rs` consults `module::all_builtin_constructor_names`; this
//! test locks the rejection end-to-end through the LSP transport.
//!
//! Uses the shared LSP client in `support.rs`.

use serde_json::json;

use crate::support::LspClient;

// ── Tests ──────────────────────────────────────────────────────────

/// Behaviourally lock the rename rejection on a gated constructor.
/// The current correct behaviour returns either an LSP error response
/// (preferred — `is_user_renameable` rejects builtin constructors) or
/// an empty/null `result` (no edits to apply). Either is acceptable as
/// long as no `WorkspaceEdit` with non-empty `changes` comes back.
#[test]
fn rename_on_gated_constructor_is_rejected() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_rn_gated_ctor.silt";
    // `IoNotFound` is a gated constructor under `module::io`. We use
    // it as a name in a context the parser will accept (a reference
    // mention) so the typechecker doesn't reject it before rename runs.
    // Even with a parse/typecheck error the rename pipeline is driven
    // by AST tokens, so `IoNotFound` mentioned in source is enough for
    // `find_ident_at_offset` to surface the symbol to the rename guard.
    let source = "import io\n\nfn main() {\n  let x = io.IoNotFound\n  x\n}\n";
    client.did_open_and_wait(uri, source);

    // `IoNotFound` starts at line=3, char=13.
    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 3, "character": 13 },
            "newName": "RenamedCtor"
        }),
    );

    // Tightened (round 86 L4): the server MUST explicitly respond.
    // Either an `error` is set (preferred — `is_user_renameable`
    // rejects the builtin constructor and the rename handler returns
    // `Err(Response::new_err(...))`), or the server responded with a
    // present `result` that is null / has empty changes. A bare `{}`
    // (no `result` and no `error` — e.g. stub server stopped
    // responding) must NOT pass this assertion.
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
        "rename on gated constructor `IoNotFound` must be explicitly \
         rejected (error response) or explicitly empty (present null \
         result / present empty changes); a missing-both shape (no \
         `error`, no `result`) means the server did not respond — got \
         {resp}"
    );
    client.shutdown();
}

/// Negative companion: rename on a *user-defined* identifier in a
/// program that ALSO mentions `IoNotFound` succeeds. This locks the
/// finer-grained behaviour: the rejection must apply only to the
/// builtin, not blanket-block the document.
#[test]
fn rename_on_user_ident_in_doc_mentioning_gated_ctor_succeeds() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_rn_user_in_gated_doc.silt";
    let source = "import io\n\nfn renamable_fn() { 0 }\nfn main() { let _ = io.IoNotFound\n  renamable_fn() }\n";
    client.did_open_and_wait(uri, source);

    // Cursor on `renamable_fn` call site at line 4, char=2.
    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 4, "character": 2 },
            "newName": "fresh_user_fn"
        }),
    );
    let result = resp.get("result").expect("rename result present");
    assert!(
        !result.is_null(),
        "rename on user-defined fn must succeed even when doc mentions a gated constructor; got {resp}"
    );
    // Round 79 follow-up: round-60 L5 sibling (above) calls
    // `client.shutdown()` at the end. This test was missing the
    // call, so on Windows runners nextest flagged the LSP child
    // process as a leak (PID 2500) and the job exited 1 even
    // though the assertion itself passed. Shut down explicitly to
    // match the sibling and clear the runner's leak detector.
    client.shutdown();
}

/// Lock for round 86 L4: the tightened rename-rejection assertion
/// shape must NOT accept a degenerate `{}` response (server stopped
/// responding — no `error`, no `result`). Without this guard, the
/// previous `result_is_null = ...unwrap_or(true)` / `changes_empty =
/// ...unwrap_or(true)` made every "no response" case pass silently.
///
/// This unit test exercises the same boolean expression used in
/// `rename_on_gated_constructor_is_rejected` and
/// `round81_lsp_rename_user_shadow_tests::rename_use_site_of_actual_builtin_int_is_rejected`,
/// against a `{}` value. If anyone relaxes that expression back to
/// the old shape, this test fires.
#[test]
fn tightened_rename_rejection_predicate_rejects_empty_object() {
    use serde_json::json;

    let resp = json!({}); // no `error`, no `result`

    let has_error = resp.get("error").is_some();
    let result = resp.get("result");
    let result_present = result.is_some();
    let result_is_null = result.map(|v| v.is_null()).unwrap_or(false);
    let changes_empty = result
        .and_then(|r| r.get("changes"))
        .and_then(|c| c.as_object())
        .map(|o| o.is_empty())
        .unwrap_or(false);

    let tightened_passes = has_error || (result_present && (result_is_null || changes_empty));
    assert!(
        !tightened_passes,
        "tightened rejection predicate must REJECT a bare {{}} response \
         (no `error`, no `result`); otherwise a non-responding server \
         silently 'passes' the rename-rejection tests"
    );

    // Sanity: the predicate should still accept the three legitimate
    // rejection shapes so the integration tests it backs do not
    // regress.
    let with_error = json!({"error": {"code": -32602, "message": "x"}});
    let has_error = with_error.get("error").is_some();
    let result = with_error.get("result");
    let result_present = result.is_some();
    let result_is_null = result.map(|v| v.is_null()).unwrap_or(false);
    let changes_empty = result
        .and_then(|r| r.get("changes"))
        .and_then(|c| c.as_object())
        .map(|o| o.is_empty())
        .unwrap_or(false);
    assert!(
        has_error || (result_present && (result_is_null || changes_empty)),
        "an explicit error response must still pass the tightened predicate"
    );

    let with_null_result = json!({"result": null});
    let has_error = with_null_result.get("error").is_some();
    let result = with_null_result.get("result");
    let result_present = result.is_some();
    let result_is_null = result.map(|v| v.is_null()).unwrap_or(false);
    let changes_empty = result
        .and_then(|r| r.get("changes"))
        .and_then(|c| c.as_object())
        .map(|o| o.is_empty())
        .unwrap_or(false);
    assert!(
        has_error || (result_present && (result_is_null || changes_empty)),
        "an explicit null result must still pass the tightened predicate"
    );

    let with_empty_changes = json!({"result": {"changes": {}}});
    let has_error = with_empty_changes.get("error").is_some();
    let result = with_empty_changes.get("result");
    let result_present = result.is_some();
    let result_is_null = result.map(|v| v.is_null()).unwrap_or(false);
    let changes_empty = result
        .and_then(|r| r.get("changes"))
        .and_then(|c| c.as_object())
        .map(|o| o.is_empty())
        .unwrap_or(false);
    assert!(
        has_error || (result_present && (result_is_null || changes_empty)),
        "an explicit empty-changes result must still pass the tightened predicate"
    );
}
