//! Closure parameters take the data-parameter grammar of a named
//! function (`{ p: Point -> ... }`, `{ (a, b) -> ... }`). The language
//! server must treat them like fn params: an annotation is a type
//! reference (definition, rename), an unannotated param gets an inlay
//! hint and an annotated one does not, and a plain param name is a
//! PARAMETER semantic token.
//!
//! Uses the shared LSP client in `support.rs`.

use serde_json::{Value, json};

use crate::support::LspClient;

/// Index of PARAMETER in `src/lsp/semantic_tokens.rs::TOKEN_LEGEND`.
const TT_PARAMETER: u64 = 5;

const SRC: &str = "type Point { x: Int, y: Int }\n\
                   \n\
                   fn main() {\n  \
                   let getx = { p: Point -> p.x }\n  \
                   let inc = { n -> n + 1 }\n  \
                   println(getx(Point { x: 1, y: 2 }) + inc(2))\n\
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
fn definition_on_closure_param_annotation_lands_on_type_decl() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_closure_params_def.silt";
    client.did_open_and_wait(uri, SRC);

    let (l, c) = pos_of(SRC, "Point ->");
    let resp = client.request(
        "textDocument/definition",
        json!({ "textDocument": { "uri": uri }, "position": at((l, c + 1)) }),
    );
    let result = resp.get("result").cloned().unwrap_or(Value::Null);
    let loc = if result.is_array() {
        result.get(0).cloned().unwrap_or(Value::Null)
    } else {
        result
    };
    assert_eq!(
        loc.pointer("/range/start/line").and_then(|v| v.as_u64()),
        Some(0),
        "definition of the annotation's `Point` must be the type decl on line 0; got {resp}"
    );
    client.shutdown();
}

#[test]
fn rename_type_edits_closure_param_annotation() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_closure_params_rename.silt";
    client.did_open_and_wait(uri, SRC);

    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": at(pos_of(SRC, "Point {")),
            "newName": "Pt"
        }),
    );
    let edits = resp
        .pointer(&format!("/result/changes/{uri}"))
        .or_else(|| resp.pointer("/result/changes").and_then(|c| c.get(uri)))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_else(|| panic!("rename must return edits for {uri}; got {resp}"));
    let annotation = pos_of(SRC, "Point ->");
    let hits_annotation = edits.iter().any(|e| {
        e.pointer("/range/start/line").and_then(|v| v.as_u64()) == Some(annotation.0)
            && e.pointer("/range/start/character").and_then(|v| v.as_u64()) == Some(annotation.1)
    });
    assert!(
        hits_annotation,
        "renaming `Point` must edit the closure annotation at {annotation:?}; got {edits:#?}"
    );
    client.shutdown();
}

#[test]
fn inlay_hint_only_on_unannotated_closure_param() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_closure_params_inlay.silt";
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
    let hint_at = |pos: (u64, u64)| -> Option<String> {
        hints.iter().find_map(|h| {
            let line = h.pointer("/position/line").and_then(|v| v.as_u64())?;
            let ch = h.pointer("/position/character").and_then(|v| v.as_u64())?;
            if (line, ch) == pos {
                h.get("label").and_then(|v| v.as_str()).map(str::to_string)
            } else {
                None
            }
        })
    };
    // Just past the `n` of `{ n -> ...`.
    let n = pos_of(SRC, "n -> n");
    assert_eq!(
        hint_at((n.0, n.1 + 1)).as_deref(),
        Some(": Int"),
        "unannotated closure param `n` must get a `: Int` hint; got {resp}"
    );
    // Just past the `p` of `{ p: Point -> ...`: annotated, so no hint.
    let p = pos_of(SRC, "p: Point");
    assert_eq!(
        hint_at((p.0, p.1 + 1)),
        None,
        "annotated closure param `p` must not get a hint; got {resp}"
    );
    client.shutdown();
}

#[test]
fn closure_param_names_are_parameter_tokens() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_closure_params_tokens.silt";
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
    let mut params = Vec::new();
    for chunk in data.as_chunks::<5>().0 {
        let dl = chunk[0].as_u64().unwrap();
        let ds = chunk[1].as_u64().unwrap();
        if dl == 0 {
            start += ds;
        } else {
            line += dl;
            start = ds;
        }
        if chunk[3].as_u64() == Some(TT_PARAMETER) {
            params.push((line, start));
        }
    }
    for needle in ["p: Point", "n -> n"] {
        let pos = pos_of(SRC, needle);
        assert!(
            params.contains(&pos),
            "closure param at {pos:?} ({needle:?}) must be a PARAMETER token; got {params:?}"
        );
    }
    client.shutdown();
}
