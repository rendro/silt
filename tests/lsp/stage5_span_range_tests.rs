//! Spans are byte ranges (stage 5, step 1): the server converts them to
//! LSP positions through the document's line table, and every node knows
//! where it ends.
//!
//! - Hover at a position that is on no character (past the last line, or
//!   past the end of its line) returns nothing. It used to return the
//!   type of the last expression that started before the position.
//! - The "Wrap expression in `Ok(...)`" quick fix wraps the whole
//!   expression the type error is about. With point spans the diagnostic
//!   covered only the first token, and the fix produced
//!   `Ok(compute)(1, 2) + 3`.

use serde_json::{Value, json};

use crate::support::LspClient;

fn hover(client: &mut LspClient, uri: &str, line: u32, character: u32) -> Value {
    let resp = client.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        }),
    );
    resp.get("result").cloned().unwrap_or(Value::Null)
}

#[test]
fn hover_on_no_character_returns_nothing() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_stage5_hover_range.silt";
    client.did_open_and_wait(uri, "fn main() {\n  let s = \"é\"\n  s\n}\n");

    // Control: on `s` in line 2 there is a type.
    assert!(
        !hover(&mut client, uri, 2, 2).is_null(),
        "hover on an identifier has a result"
    );
    // Past the last line.
    let past_last = hover(&mut client, uri, 999, 0);
    assert!(past_last.is_null(), "hover past the last line: {past_last}");
    // Past the end of a line.
    let past_end = hover(&mut client, uri, 1, 999);
    assert!(
        past_end.is_null(),
        "hover past the end of a line: {past_end}"
    );
    // Just after the last character of a line: no character there either.
    let at_end = hover(&mut client, uri, 2, 3);
    assert!(at_end.is_null(), "hover after the end of a line: {at_end}");
    client.shutdown();
}

#[test]
fn wrap_in_ok_wraps_the_whole_expression() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_stage5_wrap_ok.silt";
    let source = "fn compute(a: Int, b: Int) -> Int { a + b }\n\
                  fn main() {\n  let r: Result(Int, String) = compute(1, 2) + 3\n  r\n}\n";
    let diags = client.did_open_and_collect_diagnostics(uri, source);
    let diag = diags
        .iter()
        .find(|d| {
            d.get("message")
                .and_then(|m| m.as_str())
                .is_some_and(|m| m.contains("expected Result"))
        })
        .unwrap_or_else(|| panic!("expected a Result mismatch; got {diags:?}"))
        .clone();

    let resp = client.request(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": uri },
            "range": diag.get("range").cloned().unwrap(),
            "context": { "diagnostics": [diag] }
        }),
    );
    let actions = resp
        .get("result")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    let wrap = actions
        .iter()
        .find(|a| {
            a.get("title")
                .and_then(|t| t.as_str())
                .is_some_and(|t| t.contains("Ok("))
        })
        .unwrap_or_else(|| panic!("no Ok-wrap action; got {resp}"));
    let edits = wrap
        .pointer(&format!("/edit/changes/{}", uri.replace('/', "~1")))
        .and_then(|e| e.as_array())
        .unwrap_or_else(|| panic!("no edits for the document; got {wrap}"));
    let new_text = edits[0].get("newText").and_then(|t| t.as_str()).unwrap();
    assert_eq!(new_text, "Ok(compute(1, 2) + 3)");
    client.shutdown();
}
