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
    // Apply the edits to the line they are on, the last first.
    let mut line: Vec<char> = source.lines().nth(2).unwrap().chars().collect();
    let mut placed: Vec<(u64, u64, &str)> = edits
        .iter()
        .map(|e| {
            let start = e.pointer("/range/start/character").and_then(|c| c.as_u64());
            let end = e.pointer("/range/end/character").and_then(|c| c.as_u64());
            let text = e.get("newText").and_then(|t| t.as_str());
            (start.unwrap(), end.unwrap(), text.unwrap())
        })
        .collect();
    placed.sort_by_key(|e| std::cmp::Reverse(e.0));
    for (start, end, text) in placed {
        line.splice(start as usize..end as usize, text.chars());
    }
    let line: String = line.into_iter().collect();
    assert_eq!(line, "  let r: Result(Int, String) = Ok(compute(1, 2) + 3)");
    client.shutdown();
}

/// On the head of an arm's constructor pattern the innermost node with a
/// type is the scrutinee: hover shows `Shape` and typeDefinition jumps to
/// `type Shape`, not to the type of the whole match (`Float`).
#[test]
fn a_pattern_head_has_the_scrutinee_type() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_stage5_pattern_head.silt";
    let source = "type Shape { Circle(Float), Square(Float) }\n\
                  fn area(s: Shape) -> Float {\n  match s {\n    Circle(r) -> r * r\n    Square(w) -> w * w\n  }\n}\n\
                  fn main() { println(area(Circle(1.0))) }\n";
    client.did_open_and_wait(uri, source);

    let result = hover(&mut client, uri, 3, 6);
    let value = result
        .pointer("/contents/value")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("hover on `Circle` has a result: {result}"));
    assert!(value.contains("Shape"), "hover on `Circle`: {value}");

    let resp = client.request(
        "textDocument/typeDefinition",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 3, "character": 6 }
        }),
    );
    let line = resp
        .pointer("/result/range/start/line")
        .unwrap_or_else(|| panic!("typeDefinition on `Circle` has a location: {resp}"));
    assert_eq!(line, 0, "jumps to `type Shape`: {resp}");
    client.shutdown();
}
