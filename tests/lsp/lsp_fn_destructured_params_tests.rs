//! Named functions and trait methods take the parameter grammar of a
//! closure: a name or a destructuring pattern, each optionally typed
//! (`fn g(P { x, y })`, `fn h((a, b): (Int, Int))`). The language server
//! must handle the binders of such a parameter: hover on a body use
//! shows its type, rename edits the binder and every body use, the
//! binders are VARIABLE semantic tokens (as for a destructuring closure
//! parameter), and the annotated parameter gets no inlay hint.
//!
//! Uses the shared LSP client in `support.rs`.

use serde_json::{Value, json};

use crate::support::LspClient;

/// Index of VARIABLE in `src/lsp/semantic_tokens.rs::TOKEN_LEGEND`.
const TT_VARIABLE: u64 = 6;

const SRC: &str = "type P { x: Int, y: Int }\n\
                   \n\
                   fn g(P { x, y }) {\n  \
                   x + y\n\
                   }\n\
                   \n\
                   fn h((a, b): (Int, String)) {\n  \
                   a\n\
                   }\n\
                   \n\
                   fn main() {\n  \
                   println(g(P { x: 1, y: 2 }) + h((5, \"s\")))\n\
                   }\n";

/// (0-based line, 0-based char) of the first match of `needle`.
fn pos_of(text: &str, needle: &str) -> (u64, u64) {
    let off = text
        .find(needle)
        .unwrap_or_else(|| panic!("needle {needle:?} not in text"));
    let line = text[..off].bytes().filter(|&b| b == b'\n').count() as u64;
    let line_start = text[..off].rfind('\n').map(|i| i + 1).unwrap_or(0);
    (line, (off - line_start) as u64)
}

fn at(pos: (u64, u64)) -> Value {
    json!({ "line": pos.0, "character": pos.1 })
}

#[test]
fn hover_on_destructured_fn_param_use_shows_its_type() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_fn_destructured_params_hover.silt";
    client.did_open_and_wait(uri, SRC);

    let (l, c) = pos_of(SRC, "x + y");
    let resp = client.hover(uri, l as u32, c as u32);
    let text = resp
        .pointer("/result/contents/value")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("hover on `x` must return contents; got {resp}"));
    assert!(
        text.contains("Int"),
        "hover on `x` must show Int; got {text:?}"
    );

    let (l, c) = pos_of(SRC, "a\n}");
    let resp = client.hover(uri, l as u32, c as u32);
    let text = resp
        .pointer("/result/contents/value")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("hover on `a` must return contents; got {resp}"));
    assert!(
        text.contains("Int"),
        "hover on `a` must show Int; got {text:?}"
    );
    client.shutdown();
}

#[test]
fn rename_destructured_fn_param_edits_binder_and_uses() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_fn_destructured_params_rename.silt";
    client.did_open_and_wait(uri, SRC);

    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": at(pos_of(SRC, "a\n}")),
            "newName": "first"
        }),
    );
    let edits = resp
        .pointer(&format!("/result/changes/{uri}"))
        .or_else(|| resp.pointer("/result/changes").and_then(|c| c.get(uri)))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_else(|| panic!("rename must return edits for {uri}; got {resp}"));
    let mut starts: Vec<(u64, u64)> = edits
        .iter()
        .map(|e| {
            (
                e.pointer("/range/start/line")
                    .and_then(|v| v.as_u64())
                    .unwrap(),
                e.pointer("/range/start/character")
                    .and_then(|v| v.as_u64())
                    .unwrap(),
            )
        })
        .collect();
    starts.sort();
    assert_eq!(
        starts,
        vec![pos_of(SRC, "a, b)"), pos_of(SRC, "a\n}")],
        "renaming `a` must edit the binder in the param list and its body use; got {edits:#?}"
    );
    client.shutdown();
}

#[test]
fn destructured_fn_param_binders_are_variable_tokens() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_fn_destructured_params_tokens.silt";
    client.did_open_and_wait(uri, SRC);

    let resp = client.request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": uri } }),
    );
    let data = resp
        .pointer("/result/data")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("expected /result/data array; got: {resp}"));
    let mut line = 0u64;
    let mut start = 0u64;
    let mut vars = Vec::new();
    for chunk in data.chunks_exact(5) {
        let dl = chunk[0].as_u64().unwrap();
        let ds = chunk[1].as_u64().unwrap();
        if dl == 0 {
            start += ds;
        } else {
            line += dl;
            start = ds;
        }
        if chunk[3].as_u64() == Some(TT_VARIABLE) {
            vars.push((line, start));
        }
    }
    for needle in ["x, y }) {", "y }) {", "a, b)", "b)"] {
        let pos = pos_of(SRC, needle);
        assert!(
            vars.contains(&pos),
            "binder at {pos:?} ({needle:?}) must be a VARIABLE token; got {vars:?}"
        );
    }
    client.shutdown();
}

#[test]
fn no_inlay_hint_on_annotated_destructured_fn_param() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_fn_destructured_params_inlay.silt";
    client.did_open_and_wait(uri, SRC);

    let resp = client.request(
        "textDocument/inlayHint",
        json!({
            "textDocument": { "uri": uri },
            "range": { "start": at((0, 0)), "end": at((20, 0)) }
        }),
    );
    let hints = resp
        .get("result")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    let h_line = pos_of(SRC, "fn h(").0;
    let on_h = hints
        .iter()
        .filter(|h| h.pointer("/position/line").and_then(|v| v.as_u64()) == Some(h_line))
        .count();
    assert_eq!(
        on_h, 0,
        "the annotated parameter of `h` must get no inlay hint; got {resp}"
    );
    client.shutdown();
}
