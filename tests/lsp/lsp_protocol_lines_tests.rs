//! The language server counts lines as the protocol does: a line ends at
//! `\n`, `\r\n` or a lone `\r`. The lexer and the CLI take a lone `\r`
//! for white space; an editor that follows the protocol does not, and
//! every position the server hands out or reads must agree with it.

use serde_json::{Value, json};

use crate::support::{LspClient, next_id};

/// The byte offset of the protocol position `line`:`character` (UTF-16
/// units) in `text`, lines counted as the protocol counts them; written
/// here apart from the server's own.
fn offset_of(text: &str, line: u64, character: u64) -> usize {
    let bytes = text.as_bytes();
    let mut start = 0;
    for _ in 0..line {
        let rest = &bytes[start..];
        match rest.iter().position(|b| *b == b'\n' || *b == b'\r') {
            Some(at) if rest[at] == b'\r' && rest.get(at + 1) == Some(&b'\n') => start += at + 2,
            Some(at) => start += at + 1,
            None => return text.len(),
        }
    }
    let line_end = (bytes[start..].iter())
        .position(|b| *b == b'\n' || *b == b'\r')
        .map_or(text.len(), |at| start + at);
    let mut units = 0;
    for (i, c) in text[start..line_end].char_indices() {
        if units >= character {
            return start + i;
        }
        units += c.len_utf16() as u64;
    }
    line_end
}

/// `text` with the edits of a formatting reply applied, as an editor
/// that counts lines as the protocol does applies them.
fn applied(text: &str, edits: &[Value]) -> String {
    let mut out = text.to_string();
    let mut edits = edits.to_vec();
    let at = |e: &Value, end: &str| {
        let n = |k: &str| {
            e.pointer(&format!("/range/{end}/{k}"))
                .and_then(Value::as_u64)
                .unwrap()
        };
        (n("line"), n("character"))
    };
    edits.sort_by_key(|e| std::cmp::Reverse(at(e, "start")));
    for e in &edits {
        let (sl, sc) = at(e, "start");
        let (el, ec) = at(e, "end");
        let (from, to) = (offset_of(&out, sl, sc), offset_of(&out, el, ec));
        out.replace_range(from..to, e["newText"].as_str().unwrap());
    }
    out
}

fn format(client: &mut LspClient, uri: &str) -> Vec<Value> {
    let id = next_id();
    client.send_request(
        id,
        "textDocument/formatting",
        json!({
            "textDocument": { "uri": uri },
            "options": { "tabSize": 2, "insertSpaces": true }
        }),
    );
    let reply = client.recv_response_for(id);
    reply["result"].as_array().cloned().unwrap_or_default()
}

/// The formatting edit replaces the whole document in an editor that
/// counts a lone `\r` as a line end: nothing of the old text is left
/// behind the new one. FAIL on the base: the edit's range ended at the
/// line the line feeds count, and the editor kept the rest (`}\n` of
/// the first text).
#[test]
fn the_formatting_edit_replaces_the_whole_document_with_lone_crs_crlf_and_mixed() {
    let mut client = LspClient::spawn();
    let texts = [
        // A lone CR inside a line comment.
        ("lone_cr_in_comment", "fn main() {\nprintln(1) -- a\rb\n}\n"),
        // CRLF line ends, and a carriage return of the string's own in
        // front of one.
        ("crlf", "fn main() {\r\n  println(\"a\r\r\nb\")\r\n}\r\n"),
        // Lone CR, CRLF and LF line ends in one text.
        ("mixed", "fn main() {\r  println(1)\r\n}\n"),
        // Carriage returns after the last line feed.
        ("cr_at_the_end", "fn main() {\nprintln(1)\n}\r\r"),
    ];
    for (name, text) in texts {
        let uri = format!(
            "file:///tmp/silt_protocol_lines_{name}_{}.silt",
            std::process::id()
        );
        client.did_open_and_wait(&uri, text);
        let edits = format(&mut client, &uri);
        assert_eq!(
            edits.len(),
            1,
            "{name}: one edit for the whole document: {edits:?}"
        );
        let new_text = edits[0]["newText"].as_str().unwrap();
        assert_eq!(applied(text, &edits), new_text, "{name}: {text:?}");
    }
}

/// A diagnostic, hover and folding go by the protocol's lines in a text
/// with a lone `\r` (in a comment). FAIL on the base: the diagnostic of
/// `yy` was on line 4, one line above `yy` in the editor.
#[test]
fn diagnostics_hover_and_folding_count_a_lone_cr_as_a_line_end() {
    let mut client = LspClient::spawn();
    let uri = format!(
        "file:///tmp/silt_protocol_lines_cr_{}.silt",
        std::process::id()
    );
    // (The lexer takes a lone CR for white space, so it stands in a
    // comment here: statements need line feeds between them.)
    let text = "fn main() {\n  -- a\rb\n  let x = 1\n  println(x)\n  println(yy)\n}\n";
    let diagnostics = client.did_open_and_collect_diagnostics(&uri, text);
    let yy = (diagnostics.iter())
        .find(|d| d["message"].as_str().is_some_and(|m| m.contains("'yy'")))
        .unwrap_or_else(|| panic!("a diagnostic for yy: {diagnostics:?}"));
    assert_eq!(
        yy["range"],
        json!({ "start": { "line": 5, "character": 10 }, "end": { "line": 5, "character": 12 } }),
        "{yy}"
    );

    let hover = client.hover(&uri, 4, 10);
    let shown = hover
        .pointer("/result/contents/value")
        .and_then(Value::as_str);
    assert!(
        shown.is_some_and(|s| s.contains("Int")),
        "hover on x: {hover}"
    );

    let folds = client.request_result(
        "textDocument/foldingRange",
        json!({ "textDocument": { "uri": uri } }),
    );
    let main = (folds.as_array().into_iter().flatten())
        .find(|f| f["startLine"] == 0)
        .unwrap_or_else(|| panic!("a fold of main: {folds}"));
    assert_eq!(main["endLine"], 6, "{main}");
}
