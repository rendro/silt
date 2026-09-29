//! Regression: LSP rename must reject new names that begin with a
//! non-ASCII Unicode letter, because the lexer's identifier-start set
//! is ASCII-only (`'a'..='z' | 'A'..='Z' | '_'` at `src/lexer.rs`).
//!
//! Before the fix, `is_valid_silt_ident` used `char::is_alphabetic` for
//! the first character, which returns `true` for Unicode letters like
//! `é` / `名` / `équipe`. rename then happily produced a `WorkspaceEdit`
//! rewriting every reference to a name that fails to lex on the next
//! `silt run` / `check`, silently corrupting the user's source.
//!
//! The fix switches the first-char check to `is_ascii_alphabetic`, so
//! the rename handler returns an `InvalidParams` error
//! (`is not a valid silt identifier`) instead of an edit.
//!
//! This locks the behaviour end-to-end through the LSP transport.
//! Uses the shared LSP client in `support.rs`.

use serde_json::json;

use crate::support::LspClient;

/// Drive `textDocument/rename` on a user-defined `fn foo` with a
/// Unicode-leading new name and assert the server rejects it with an
/// `InvalidParams` error mentioning "not a valid silt identifier",
/// rather than returning a `WorkspaceEdit`.
fn assert_unicode_rename_rejected(new_name: &str) {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_rn_unicode_ident.silt";
    // `foo` starts at line=0, char=3 (`fn foo`).
    let source = "fn foo() { 0 }\nfn main() { foo() }\n";
    client.did_open_and_wait(uri, source);

    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 3 },
            "newName": new_name
        }),
    );

    // The fix must reject the rename: an `error` response with the
    // identifier-validation message. A `WorkspaceEdit` (a `result` with
    // non-empty `changes`) would mean the server is about to corrupt the
    // user's source with a name the lexer cannot tokenize.
    let error_msg = resp
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let has_changes = resp
        .pointer("/result/changes")
        .and_then(|c| c.as_object())
        .map(|o| !o.is_empty())
        .unwrap_or(false);

    assert!(
        !has_changes,
        "rename to Unicode-leading name `{new_name}` must NOT return a \
         WorkspaceEdit — the resulting source fails to lex; got {resp}"
    );
    assert!(
        error_msg.contains("is not a valid silt identifier"),
        "rename to Unicode-leading name `{new_name}` must be rejected \
         with an InvalidParams `is not a valid silt identifier` error; \
         got {resp}"
    );
    client.shutdown();
}

#[test]
fn rename_to_latin_accented_name_is_rejected() {
    assert_unicode_rename_rejected("é");
}

#[test]
fn rename_to_cjk_name_is_rejected() {
    assert_unicode_rename_rejected("名");
}

#[test]
fn rename_to_accented_word_is_rejected() {
    assert_unicode_rename_rejected("équipe");
}

/// Positive control: a plain ASCII new name still succeeds, so the
/// fix only narrows the first-char set and does not block legitimate
/// renames.
#[test]
fn rename_to_ascii_name_still_succeeds() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_rn_ascii_ident.silt";
    let source = "fn foo() { 0 }\nfn main() { foo() }\n";
    client.did_open_and_wait(uri, source);

    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 3 },
            "newName": "bar"
        }),
    );
    let result = resp.get("result").expect("rename result present");
    assert!(
        !result.is_null(),
        "rename to plain ASCII name `bar` must succeed; got {resp}"
    );
    client.shutdown();
}
