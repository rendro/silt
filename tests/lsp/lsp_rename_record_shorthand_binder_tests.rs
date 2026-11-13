//! Round-100 BROKEN: `textDocument/rename` on a record SHORTHAND binder
//! (`let Point { x, y } = p`, cursor on `x`) corrupted the source.
//!
//! `collect_references_in_pattern` pushed `pattern.span` for a shorthand
//! binder, but that span is the record HEAD (the constructor name for a
//! nominal record, the opening `{` for an anon record) — not the field
//! token. The resulting edit rewrote the constructor / brace and left the
//! binder untouched: applying it produced `let XXX { x, y } = p`, which no
//! longer compiles.
//!
//! The edit targets the field token and writes the field out, `x:
//! renamed_x`: the field keeps its name, the binder gets the new one.
//! This test drives the live LSP server over stdio, applies the returned
//! edits, and asserts that the constructor survives, that the binder and
//! its use are renamed, and that the result checks.
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

#[test]
fn rename_record_shorthand_binder_targets_field_not_constructor() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_r100_rn_shorthand.silt";
    // Line 0: `type Point { x: Int, y: Int }`
    // Line 1: `fn main() {`
    // Line 2: `  let Point { x, y } = Point { x: 1, y: 2 }`
    //                          ^ field `x` binder at char 14
    // Line 3: `  println("{x} {y}")`
    let text = "type Point { x: Int, y: Int }\n\
                fn main() {\n  \
                let Point { x, y } = Point { x: 1, y: 2 }\n  \
                println(\"{x} {y}\")\n}\n";
    client.did_open_and_wait(uri, text);

    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 2, "character": 14 },
            "newName": "renamed_x"
        }),
    );
    let result = resp.get("result").expect("rename has result");
    assert!(
        !result.is_null(),
        "rename on a shorthand binder must not return null; got {resp}"
    );
    let edits = result
        .pointer(&format!("/changes/{uri}"))
        .or_else(|| result.get("changes").and_then(|c| c.get(uri)))
        .and_then(|v| v.as_array())
        .expect("file edits");

    // The binder edit must target the FIELD token (line 2), NOT the
    // constructor `Point` (which starts at char 6). char 14 is the `x`.
    let binder_edit = edits
        .iter()
        .find(|e| e.pointer("/range/start/line").and_then(|v| v.as_u64()) == Some(2))
        .expect("an edit on the binder line");
    let start_char = binder_edit
        .pointer("/range/start/character")
        .and_then(|v| v.as_u64())
        .unwrap();
    assert!(
        start_char >= 12,
        "binder edit must target the field token (>=12), not the `Point` \
         constructor head (char 6); got char {start_char}"
    );

    // Applying every edit must keep `Point` intact and rename the binder
    // plus its use — the result must still be valid (compiling) code.
    let applied = apply_edits(text, edits);
    // The field keeps its name; the binder behind it gets the new one.
    assert!(
        applied.contains("let Point { x: renamed_x, y } ="),
        "constructor `Point` must survive and the binder be renamed; got:\n{applied}"
    );
    assert!(
        applied.contains("{renamed_x}"),
        "the binder's use inside the interpolation must be renamed; got:\n{applied}"
    );
    assert!(
        !applied.contains("renamed_x { x, y }"),
        "the constructor name must NOT be clobbered; got:\n{applied}"
    );
    client.shutdown();

    // And the result is a program that checks.
    let dir = std::env::temp_dir().join(format!("silt_r100_shorthand_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("case.silt");
    std::fs::write(&file, &applied).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("check")
        .arg(&file)
        .output()
        .expect("run silt check");
    assert!(
        out.status.success(),
        "the renamed program does not check:\n{}\n{applied}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}
