//! Tier 2 LSP features: inlay hints, document highlight, folding
//! range, selection range.
//!
//! Uses the shared LSP client in `support.rs`.

use serde_json::json;

use crate::support::LspClient;

// ── Tests ──────────────────────────────────────────────────────────

#[test]
fn inlay_hints_shows_inferred_types() {
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_t2_inlay.silt";
    let src = "fn main() {\n  let x = 42\n  let s = \"hi\"\n  x\n}\n";
    client.did_open_and_wait(file, src);

    let resp = client.request(
        "textDocument/inlayHint",
        json!({
            "textDocument": { "uri": file },
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 5, "character": 0 }
            }
        }),
    );
    let arr = resp
        .get("result")
        .and_then(|r| r.as_array())
        .expect("inlay hint result array");
    let labels: Vec<String> = arr
        .iter()
        .filter_map(|h| h.get("label").and_then(|l| l.as_str()).map(String::from))
        .collect();
    assert!(
        labels.iter().any(|l| l == ": Int"),
        "expected `: Int` hint; got {labels:?}"
    );
    assert!(
        labels.iter().any(|l| l == ": String"),
        "expected `: String` hint; got {labels:?}"
    );
    client.shutdown();
}

#[test]
fn document_highlight_returns_all_ident_occurrences() {
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_t2_hl.silt";
    // `count` appears three times in the body.
    let src = "fn main() {\n  let count = 1\n  let y = count + count\n  y\n}\n";
    client.did_open_and_wait(file, src);

    // Cursor on the first `count` use at line 2.
    let resp = client.request(
        "textDocument/documentHighlight",
        json!({
            "textDocument": { "uri": file },
            "position": { "line": 2, "character": 10 }
        }),
    );
    let arr = resp
        .get("result")
        .and_then(|r| r.as_array())
        .expect("highlight result");
    // The binder on line 1 plus the two uses on line 2 -> exactly 3
    // highlights. The previous `>= 2` gate would have silently accepted an
    // implementation that dropped the binder.
    assert_eq!(
        arr.len(),
        3,
        "expected exactly three highlights (binder + 2 uses), got {arr:?}"
    );
    let starts: Vec<(u64, u64)> = arr
        .iter()
        .filter_map(|h| {
            let line = h.pointer("/range/start/line").and_then(|v| v.as_u64())?;
            let ch = h
                .pointer("/range/start/character")
                .and_then(|v| v.as_u64())?;
            Some((line, ch))
        })
        .collect();
    assert_eq!(
        starts.len(),
        3,
        "every highlight must expose a start line/character; got {arr:?}"
    );
    // Binder on line 1 at character 6 (`  let count = 1`).
    assert!(
        starts.iter().any(|&(l, c)| l == 1 && c == 6),
        "expected binder highlight at line 1, character 6; got {starts:?}"
    );
    // First use on line 2 at character 10 (`  let y = count + count`).
    assert!(
        starts.iter().any(|&(l, c)| l == 2 && c == 10),
        "expected use highlight at line 2, character 10; got {starts:?}"
    );
    // Second use on line 2 at character 18.
    assert!(
        starts.iter().any(|&(l, c)| l == 2 && c == 18),
        "expected use highlight at line 2, character 18; got {starts:?}"
    );
    client.shutdown();
}

#[test]
fn folding_range_covers_fn_body() {
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_t2_fold.silt";
    let src = "fn main() {\n  let x = 1\n  let y = 2\n  x + y\n}\n";
    client.did_open_and_wait(file, src);

    let resp = client.request(
        "textDocument/foldingRange",
        json!({ "textDocument": { "uri": file } }),
    );
    let arr = resp
        .get("result")
        .and_then(|r| r.as_array())
        .expect("folding range result");
    // Round-76 D1: a single-fn body MUST emit exactly one fold. The
    // pre-fix code pushed the body fold twice (once from the explicit
    // `push_block_fold` in `collect_decl_folds`, once from
    // `walk_expr_folds`'s `Block` arm), and the original "at least
    // one" gate happily passed on duplicates.
    assert_eq!(
        arr.len(),
        1,
        "expected exactly one fold for a single fn body; got {} — {arr:?}",
        arr.len()
    );
    // Affirmative: the single fold spans the body lines.
    let f = &arr[0];
    assert_eq!(
        f.get("startLine").and_then(|l| l.as_u64()),
        Some(0),
        "fold should start at the fn header line"
    );
    let end = f.get("endLine").and_then(|l| l.as_u64()).unwrap_or(0);
    assert!(end > 0, "fold should end below the fn header; got {f:?}");
    client.shutdown();
}

#[test]
fn selection_range_returns_nested_chain() {
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_t2_sel.silt";
    let src = "fn main() {\n  1 + 2\n}\n";
    let cursor_line: u64 = 1;
    let cursor_char: u64 = 2;
    client.did_open_and_wait(file, src);

    let resp = client.request(
        "textDocument/selectionRange",
        json!({
            "textDocument": { "uri": file },
            "positions": [ { "line": cursor_line, "character": cursor_char } ]
        }),
    );
    let arr = resp
        .get("result")
        .and_then(|r| r.as_array())
        .expect("selection range result");
    assert_eq!(arr.len(), 1);
    let first = &arr[0];
    // The response should have a nested `parent` somewhere.
    let has_parent = first.get("parent").is_some();
    assert!(
        has_parent,
        "expected a selection range with a parent; got {first:?}"
    );

    // Strengthened gate (round 84): walk to the root of the parent
    // chain and assert its range encloses the cursor. A bug that
    // mis-anchors the chain root (e.g. emits a sibling span instead
    // of the enclosing one) would have the leaf cover the cursor but
    // the root would fall off — the previous weak-gate check on
    // `parent.is_some()` passes such a buggy response.
    let mut node = first;
    loop {
        match node.get("parent") {
            Some(parent) if parent.is_object() => node = parent,
            _ => break,
        }
    }
    let range = node.get("range").expect("root selection range has `range`");
    let start = range.get("start").expect("range has `start`");
    let end = range.get("end").expect("range has `end`");
    let s_line = start.get("line").and_then(|v| v.as_u64()).unwrap();
    let s_char = start.get("character").and_then(|v| v.as_u64()).unwrap();
    let e_line = end.get("line").and_then(|v| v.as_u64()).unwrap();
    let e_char = end.get("character").and_then(|v| v.as_u64()).unwrap();
    let start_ok = s_line < cursor_line || (s_line == cursor_line && s_char <= cursor_char);
    let end_ok = e_line > cursor_line || (e_line == cursor_line && e_char >= cursor_char);
    assert!(
        start_ok && end_ok,
        "selection-range chain root must enclose the cursor \
         (line={cursor_line}, char={cursor_char}); got root range \
         start=({s_line},{s_char}) end=({e_line},{e_char}); full root={node:?}"
    );
    client.shutdown();
}
