//! A doc comment reaches the editor's hover and completion with its
//! control characters shown as the line of source under a diagnostic
//! shows them (ESC as U+241B), not raw.

use serde_json::Value;

use crate::support::LspClient;

/// FAIL on the base: the hover held the raw ESC of the doc comment.
#[test]
fn hover_and_completion_show_a_doc_comment_s_escape_character_as_a_picture() {
    let mut client = LspClient::spawn();
    let uri = format!(
        "file:///tmp/silt_markdown_controls_{}.silt",
        std::process::id()
    );
    let text = "-- docs \u{1b}c here\nfn helper() -> Int {\n  1\n}\n\nfn main() {\n  println(helper())\n}\n";
    client.did_open_and_wait(&uri, text);

    let hover = client.hover(&uri, 6, 11);
    let shown = hover
        .pointer("/result/contents/value")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(shown.contains("docs \u{241b}c here"), "hover: {hover}");
    assert!(!shown.contains('\u{1b}'), "hover: {hover}");

    let completion = client.completion(&uri, 6, 12);
    let items = completion
        .pointer("/result/items")
        .or_else(|| completion.get("result"));
    let items = items.and_then(Value::as_array).cloned().unwrap_or_default();
    let helper = (items.iter())
        .find(|item| item["label"] == "helper")
        .unwrap_or_else(|| panic!("a completion for helper: {completion}"));
    let doc = helper
        .pointer("/documentation/value")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        doc.contains('\u{241b}') && !doc.contains('\u{1b}'),
        "{helper}"
    );
}
