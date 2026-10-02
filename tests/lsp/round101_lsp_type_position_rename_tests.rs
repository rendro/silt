//! Round-101 BROKEN: LSP rename / references / goto-definition were
//! blind to TYPE-POSITION references. Renaming a user type from its
//! declaration produced exactly ONE edit (the decl name) and silently
//! broke the program: every annotation (`p: Point`), return type
//! (`-> Point`), record-field type (`inner: Point`), construction head
//! (`Point { x: 3 }`), and pattern head (`Point { x, .. }`) kept the
//! old name, so `silt check` failed with `unknown type 'Point'` after
//! applying the WorkspaceEdit.
//!
//! Root cause: `workspace::collect_references_in_expr` matched only
//! `ExprKind::Ident`; `collect_references_in_decl` never walked
//! `TypeExpr` trees; `collect_references_in_pattern` ignored
//! Constructor/Record head names. `ast_walk::find_ident_in_decl` had
//! the same blind spot, so definition/prepareRename couldn't even
//! resolve type-position cursors.
//!
//! Lock: drive the live LSP server over stdio, rename the type at its
//! DECLARATION, apply the returned WorkspaceEdit to the source, and
//! assert the result still passes the full parse + typecheck (the bug
//! is in the LSP's edit output, so `check` on the applied text is the
//! right gate), plus per-site containment assertions. A decl-only
//! edit-count assertion alone would be a weak gate.
//!
//! Uses the shared LSP client in `support.rs`.

use serde_json::{Value, json};

use crate::support::LspClient;

/// Apply LSP single-line TextEdits to `text` and return the result.
fn apply_edits(text: &str, edits: &[Value]) -> String {
    let mut lines: Vec<String> = text.split('\n').map(|s| s.to_string()).collect();
    // Apply each edit; sort so later positions are applied first to keep
    // earlier offsets valid. All edits here are single-line.
    let mut sorted: Vec<&Value> = edits.iter().collect();
    sorted.sort_by_key(|e| {
        let line = e
            .pointer("/range/start/line")
            .and_then(|v| v.as_u64())
            .unwrap();
        let ch = e
            .pointer("/range/start/character")
            .and_then(|v| v.as_u64())
            .unwrap();
        std::cmp::Reverse((line, ch))
    });
    for e in sorted {
        let line = e
            .pointer("/range/start/line")
            .and_then(|v| v.as_u64())
            .unwrap() as usize;
        let sc = e
            .pointer("/range/start/character")
            .and_then(|v| v.as_u64())
            .unwrap() as usize;
        let ec = e
            .pointer("/range/end/character")
            .and_then(|v| v.as_u64())
            .unwrap() as usize;
        let new_text = e.get("newText").and_then(|v| v.as_str()).unwrap();
        let l = &lines[line];
        lines[line] = format!("{}{}{}", &l[..sc], new_text, &l[ec..]);
    }
    lines.join("\n")
}

/// (0-based line, 0-based char) of the `occurrence`-th (0-based) match
/// of `needle` in `text`. All-ASCII sources only.
fn pos_of(text: &str, needle: &str, occurrence: usize) -> (u64, u64) {
    let mut search_from = 0usize;
    let mut off = None;
    for _ in 0..=occurrence {
        let found = text[search_from..]
            .find(needle)
            .unwrap_or_else(|| panic!("needle {needle:?} (occurrence {occurrence}) not in text"));
        off = Some(search_from + found);
        search_from += found + 1;
    }
    let off = off.unwrap();
    let line = text[..off].bytes().filter(|&b| b == b'\n').count() as u64;
    let line_start = text[..off].rfind('\n').map(|i| i + 1).unwrap_or(0);
    (line, (off - line_start) as u64)
}

/// Full front-end gate: the source must check cleanly (lex, parse,
/// typecheck and compile, through the session). This is the load-bearing assertion —
/// the round-101 bug produced WorkspaceEdits whose application yielded
/// `unknown type` errors, i.e. this function returning an Err.
fn front_end_errors(source: &str) -> Result<(), String> {
    let errors: Vec<_> = silt::session::testing::check_str(source)
        .into_iter()
        .filter(|e| e.is_error())
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!("errors: {errors:?}"))
    }
}

/// One `Point` reference per type-position class:
///   line 0: declaration            `type Point { ... }`
///   line 1: record-field type      `inner: Point`
///   line 2: param annotation       `p: Point`
///   line 3: return type + construction head `-> Point { Point { ... } }`
///   line 5: stmt-let annotation    `let p: Point = ...`
///   line 7: pattern head           `match p { Point { x, .. } -> ... }`
const SOURCE: &str = "type Point { x: Int, y: Int }\n\
                      type Wrap { inner: Point }\n\
                      fn dist(p: Point) -> Int { p.x * p.x + p.y * p.y }\n\
                      fn mk(n: Int) -> Point { Point { x: n, y: n } }\n\
                      fn main() {\n  \
                      let p: Point = mk(3)\n  \
                      let d = dist(p)\n  \
                      match p { Point { x, .. } -> println(\"{x} {d}\") }\n\
                      }\n";

#[test]
fn rename_type_from_decl_updates_all_type_position_references() {
    // Sanity: the fixture itself must be a clean program, otherwise the
    // post-rename gate below would be vacuous.
    front_end_errors(SOURCE).expect("fixture must lex/parse/typecheck cleanly");

    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_r101_rn_type_pos.silt";
    client.did_open_and_wait(uri, SOURCE);

    // Rename at the DECLARATION name (`Point` in `type Point`).
    let (line, ch) = pos_of(SOURCE, "Point", 0);
    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": ch },
            "newName": "Pt"
        }),
    );
    let result = resp.get("result").expect("rename has result");
    assert!(
        !result.is_null(),
        "rename on a type decl must not return null; got {resp}"
    );
    let edits = result
        .pointer(&format!("/changes/{uri}"))
        .or_else(|| result.get("changes").and_then(|c| c.get(uri)))
        .and_then(|v| v.as_array())
        .expect("file edits");

    // Exactly 7 references: decl + field-type + param annotation +
    // return type + construction head + stmt-let annotation + pattern
    // head. Pre-fix the response contained exactly ONE edit (the decl).
    assert_eq!(
        edits.len(),
        7,
        "expected 7 edits (decl, `inner: Point`, `p: Point`, `-> Point`, \
         `Point {{ x: n`, `let p: Point`, pattern `Point {{ x, ..`); got {edits:#?}"
    );

    let applied = apply_edits(SOURCE, edits);
    for expected in [
        "type Pt { x: Int, y: Int }",
        "type Wrap { inner: Pt }",
        "fn dist(p: Pt) -> Int",
        "fn mk(n: Int) -> Pt { Pt { x: n, y: n } }",
        "let p: Pt = mk(3)",
        "match p { Pt { x, .. } ->",
    ] {
        assert!(
            applied.contains(expected),
            "renamed source must contain {expected:?}; got:\n{applied}"
        );
    }
    assert!(
        !applied.contains("Point"),
        "no `Point` reference may survive the rename; got:\n{applied}"
    );

    // Load-bearing gate: the program produced by APPLYING the LSP's
    // WorkspaceEdit must still pass the full front end. Pre-fix this
    // failed with `unknown type 'Point'` at every non-decl site.
    front_end_errors(&applied).unwrap_or_else(|e| {
        panic!("applying the rename edit must yield a compiling program; {e}\n---\n{applied}")
    });
    client.shutdown();
}

#[test]
fn type_position_cursors_resolve_for_definition_references_prepare_rename() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_r101_type_pos_nav.silt";
    client.did_open_and_wait(uri, SOURCE);

    // (a) goto-definition on the `Point` in the `p: Point` annotation
    // must land on the declaration (line 0). Pre-fix: `null`.
    let (ann_line, ann_ch) = {
        let (l, c) = pos_of(SOURCE, "p: Point", 0);
        (l, c + 3)
    };
    let resp = client.request(
        "textDocument/definition",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": ann_line, "character": ann_ch }
        }),
    );
    let result = resp.get("result").expect("definition has result");
    assert!(
        !result.is_null(),
        "goto-definition on a type annotation must not return null; got {resp}"
    );
    let def_line = result
        .pointer("/range/start/line")
        .or_else(|| result.pointer("/0/range/start/line"))
        .and_then(|v| v.as_u64());
    assert_eq!(
        def_line,
        Some(0),
        "definition of `Point` must be the decl on line 0; got {resp}"
    );

    // (b) references from the annotation cursor must cover the decl and
    // the other type-position sites. Pre-fix: `null`.
    let resp = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": ann_line, "character": ann_ch },
            "context": { "includeDeclaration": true }
        }),
    );
    let refs = resp
        .get("result")
        .and_then(|r| r.as_array())
        .unwrap_or_else(|| {
            panic!("references on a type annotation must return an array; got {resp}")
        });
    assert!(
        refs.len() >= 7,
        "expected >= 7 references (decl + 6 type-position sites); got {}: {refs:#?}",
        refs.len()
    );

    // (c) prepareRename on the construction head `Point {{ x: n ... }}`
    // must offer a range. Pre-fix: `null`.
    let (con_line, con_ch) = pos_of(SOURCE, "Point { x: n", 0);
    let resp = client.request(
        "textDocument/prepareRename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": con_line, "character": con_ch + 1 }
        }),
    );
    let result = resp.get("result").expect("prepareRename has result");
    assert!(
        !result.is_null(),
        "prepareRename on a record-construction head must not return null; got {resp}"
    );
    client.shutdown();
}
