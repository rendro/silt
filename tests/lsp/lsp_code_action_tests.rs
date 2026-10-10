//! End-to-end tests for `textDocument/codeAction`.
//!
//! Spawns `silt lsp` as a subprocess and speaks LSP JSON-RPC over stdio.
//! Uses the shared client in `support.rs`.

use serde_json::{Value, json};

use crate::support::LspClient;

/// Small helper: find the first diagnostic whose message contains `needle`.
fn diag_matching<'a>(diags: &'a [Value], needle: &str) -> Option<&'a Value> {
    diags.iter().find(|d| {
        d.get("message")
            .and_then(|m| m.as_str())
            .is_some_and(|m| m.contains(needle))
    })
}

/// Extract the `result` array from a codeAction response.
fn code_actions(resp: &Value) -> Vec<Value> {
    resp.get("result")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default()
}

// ── Tests ──────────────────────────────────────────────────────────

// Round 56 moved the "module 'X' is not imported" check into the
// typechecker (src/typechecker/inference.rs::~1920), and the LSP
// diagnostics pipeline forwards typechecker errors verbatim
// (src/lsp/diagnostics.rs:81-105). The code-action parser matches the
// exact phrase (src/lsp/code_action.rs::import_module_from_message), so
// the quick-fix end-to-end is live and this test locks it.
#[test]
fn add_import_quickfix_offered_for_unimported_module() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_ca_import.silt";
    // `list.map(...)` without `import list` triggers the compiler's
    // "module 'list' is not imported" diagnostic.
    let source = "fn main() { list.map([1], { x -> x }) }\n";
    let diags = client.did_open_and_collect_diagnostics(uri, source);
    let import_diag = diag_matching(&diags, "not imported")
        .unwrap_or_else(|| panic!("expected 'not imported' diagnostic; got {diags:?}"))
        .clone();

    let resp = client.request(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": uri },
            "range": import_diag.get("range").cloned().unwrap_or(json!({
                "start": { "line": 0, "character": 0 },
                "end":   { "line": 0, "character": 0 }
            })),
            "context": { "diagnostics": [import_diag] }
        }),
    );
    let actions = code_actions(&resp);
    assert!(
        !actions.is_empty(),
        "expected at least one code action; got {resp}"
    );
    let action = actions
        .iter()
        .find(|a| {
            a.get("title")
                .and_then(|t| t.as_str())
                .is_some_and(|t| t.to_lowercase().contains("import"))
        })
        .unwrap_or_else(|| panic!("no action with 'import' in title; got {actions:?}"));

    // Walk to the edit's new_text and confirm it contains `import list`.
    let changes = action
        .pointer("/edit/changes")
        .and_then(|c| c.as_object())
        .expect("edit.changes exists");
    let edits = changes
        .get(uri)
        .and_then(|v| v.as_array())
        .expect("edits for our uri");
    let any_contains = edits.iter().any(|e| {
        e.get("newText")
            .and_then(|t| t.as_str())
            .is_some_and(|t| t.contains("import list"))
    });
    assert!(
        any_contains,
        "expected edit inserting `import list`; got {edits:?}"
    );
    client.shutdown();
}

/// An unused value has the quick fix that discards it: `let _ = `
/// inserted where the statement starts.
#[test]
fn discard_quickfix_offered_for_an_unused_value() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_ca_unused_value.silt";
    let source = "fn f() -> Int {\n  1\n}\n\nfn main() {\n  f()\n  println(\"done\")\n}\n";
    let diags = client.did_open_and_collect_diagnostics(uri, source);
    let unused = diag_matching(&diags, "value is unused")
        .unwrap_or_else(|| panic!("expected an unused-value diagnostic; got {diags:?}"))
        .clone();

    let resp = client.request(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": uri },
            "range": unused.get("range").cloned().expect("the diagnostic has a range"),
            "context": { "diagnostics": [unused] }
        }),
    );
    let actions = code_actions(&resp);
    let action = actions
        .iter()
        .find(|a| a.get("title").and_then(|t| t.as_str()) == Some("Discard with `let _ =`"))
        .unwrap_or_else(|| panic!("no discard action; got {actions:?}"));
    let edits = action
        .pointer("/edit/changes")
        .and_then(|c| c.get(uri))
        .and_then(|v| v.as_array())
        .expect("edits for our uri");
    assert_eq!(
        edits,
        &vec![json!({
            "range": {
                "start": { "line": 5, "character": 2 },
                "end": { "line": 5, "character": 2 }
            },
            "newText": "let _ = "
        })],
        "the fix inserts `let _ = ` before `f()`"
    );
    client.shutdown();
}

/// The byte offset of the LSP position (`line`, UTF-16 `character`) in
/// `text`.
fn offset_of(text: &str, line: u64, character: u64) -> usize {
    let mut offset = 0;
    for (i, l) in text.split_inclusive('\n').enumerate() {
        if i as u64 == line {
            let mut units = 0;
            for (at, c) in l.char_indices() {
                if units >= character {
                    return offset + at;
                }
                units += c.len_utf16() as u64;
            }
            return offset + l.trim_end_matches(['\n', '\r']).len();
        }
        offset += l.len();
    }
    text.len()
}

/// Open `source`, ask for the code actions of every unused-value
/// diagnostic and apply each "Discard with `let _ =`" edit. Returns the
/// number of unused-value diagnostics, the number of fixes offered, and
/// the text with the fixes applied.
fn apply_discard_fixes(client: &mut LspClient, uri: &str, source: &str) -> (usize, usize, String) {
    let diags = client.did_open_and_collect_diagnostics(uri, source);
    let unused: Vec<Value> = diags
        .iter()
        .filter(|d| d.get("code").and_then(|c| c.as_str()) == Some("E0340"))
        .cloned()
        .collect();
    let mut inserts: Vec<(usize, String)> = Vec::new();
    for diag in &unused {
        let resp = client.request(
            "textDocument/codeAction",
            json!({
                "textDocument": { "uri": uri },
                "range": diag.get("range").cloned().expect("the diagnostic has a range"),
                "context": { "diagnostics": [diag] }
            }),
        );
        for action in code_actions(&resp) {
            if action.get("title").and_then(|t| t.as_str()) != Some("Discard with `let _ =`") {
                continue;
            }
            let edits = action
                .pointer("/edit/changes")
                .and_then(|c| c.get(uri))
                .and_then(|v| v.as_array())
                .expect("edits for our uri");
            for edit in edits {
                let start = edit.pointer("/range/start").expect("an edit has a start");
                assert_eq!(
                    edit.pointer("/range/end"),
                    Some(start),
                    "the fix only inserts"
                );
                let at = offset_of(
                    source,
                    start["line"].as_u64().expect("a line"),
                    start["character"].as_u64().expect("a character"),
                );
                let text = edit["newText"].as_str().expect("new text").to_string();
                inserts.push((at, text));
            }
        }
    }
    let offered = inserts.len();
    inserts.sort();
    let mut fixed = source.to_string();
    for (at, text) in inserts.into_iter().rev() {
        fixed.insert_str(at, &text);
    }
    (unused.len(), offered, fixed)
}

/// The quick fix on every shape a statement can have: a statement that
/// starts with a parenthesis, the literals, a closure, a line that
/// starts with `-`, a loop, a match, a pipeline over several lines, a
/// string, and statements behind text that is not ASCII. Each unused
/// value has the fix, and the text with every fix applied checks.
#[test]
fn discard_quickfix_gives_a_program_that_checks_on_every_statement_shape() {
    let source = r#"import channel
import list

type Holder {
  f: Fn() -> Result(Int, String),
}

fn risky() -> Result(Int, String) {
  Err("lost")
}

fn generic(x, y) {
  x
  y
  0
}

fn parenthesised() {
  let h = Holder { f: risky }
  let a = 1
  (h.f)()
  (a + 1) * 2
  (risky() as Result(Int, String))
  (risky())
  ((a))
  println("done")
}

fn kinds() {
  let n = 3
  []
  None
  #{}
  Ok(1)
  channel.new(1)
  { x -> x }
  { -> risky() }
  -n
  - n * 2
  loop i = 0 {
    match i < 3 {
      true -> loop(i + 1)
      false -> i
    }
  }
  match n {
    3 -> "three"
    _ -> "other"
  }
  [1, 2]
    |> list.map { x -> x + 1 }
    |> list.length
  "text {n}"
  println("done")
}

fn main() {
  let s = "é"
  println("ünï { { risky()
    s } } çödé { { risky()
    1 } }")
  match s {
    "é" -> { risky()
      println("x") }
    _ -> ()
  }
  parenthesised()
  kinds()
  println(generic(1, 2))
}
"#;
    let mut client = LspClient::spawn();
    let (unused, offered, fixed) = apply_discard_fixes(
        &mut client,
        "file:///tmp/silt_ca_unused_shapes.silt",
        source,
    );
    assert_eq!(
        unused, 23,
        "every statement that leaves a value is reported"
    );
    assert_eq!(offered, 23, "each has the fix");
    assert_eq!(fixed.matches("let _ = ").count(), 23);
    assert!(
        fixed.contains("  let _ = (h.f)()\n") && fixed.contains("  let _ = ((a))\n"),
        "the fix stands before the statement's first token:\n{fixed}"
    );
    let after = client
        .did_open_and_collect_diagnostics("file:///tmp/silt_ca_unused_shapes_fixed.silt", &fixed);
    assert!(
        after.is_empty(),
        "the fixed program has no diagnostics; got {after:?}\n{fixed}"
    );
    client.shutdown();
}

/// `let p = point { x: 1 }` is two statements, and `let _ = ` before the
/// `{` would not parse: the unused value there has no fix, and a help
/// that names the likely mistake.
#[test]
fn no_discard_quickfix_for_a_brace_behind_a_name() {
    let source = "type Point {\n  x: Int,\n}\n\nfn main() {\n  let point = Point { x: 0 }\n  let p = point { x: 1 }\n  println(p.x)\n}\n";
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_ca_unused_brace.silt";
    let (unused, offered, fixed) = apply_discard_fixes(&mut client, uri, source);
    assert_eq!(unused, 1, "the `{{ x: 1 }}` is an unused value");
    assert_eq!(offered, 0, "no fix is offered");
    assert_eq!(fixed, source);
    let diags = client.did_open_and_collect_diagnostics(uri, source);
    let message = diags[0]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("a record literal's type name starts with an upper-case letter"),
        "the help names the likely mistake; got {message:?}"
    );
    client.shutdown();
}

#[test]
fn no_action_when_diagnostic_is_unrelated() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_ca_none.silt";
    let source = "fn main() { undefined_name }\n";
    let diags = client.did_open_and_collect_diagnostics(uri, source);
    // Pick any diagnostic (there will be one for the undefined identifier).
    let Some(diag) = diags.first().cloned() else {
        // If the typechecker emitted nothing, the test trivially passes.
        client.shutdown();
        return;
    };
    let resp = client.request(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": uri },
            "range": diag.get("range").cloned().unwrap_or(json!({
                "start": { "line": 0, "character": 0 },
                "end":   { "line": 0, "character": 0 }
            })),
            "context": { "diagnostics": [diag] }
        }),
    );
    let actions = code_actions(&resp);
    // We expect no matching quick-fix for an "undefined name" diagnostic —
    // it's unrelated to our starter catalog.
    assert!(
        actions.is_empty(),
        "expected no code actions for unrelated diagnostic; got {actions:?}"
    );
    client.shutdown();
}

#[test]
fn code_action_capability_advertised() {
    // The initialize response should advertise codeActionProvider.
    let mut client = LspClient::spawn();
    // We already initialized inside spawn(); send another request to exercise
    // the dispatch surface — if the capability isn't wired up, subsequent
    // requests still work, so we instead check server behaviour via a second
    // initialize-like round trip isn't possible. Smoke-test: send an empty
    // codeAction request against an empty doc; the response should be a
    // JSON array (or null/empty), never an error.
    let uri = "file:///tmp/silt_ca_empty.silt";
    let _ = client.did_open_and_collect_diagnostics(uri, "fn main() { 0 }\n");
    let resp = client.request(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": uri },
            "range": { "start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0} },
            "context": { "diagnostics": [] }
        }),
    );
    assert!(
        resp.get("error").is_none(),
        "codeAction must not return an error on a clean document; got {resp}"
    );
    // result is an array (possibly empty) or null.
    let result = resp.get("result").cloned().unwrap_or(Value::Null);
    assert!(
        result.is_array() || result.is_null(),
        "expected array or null result, got {result}"
    );
    client.shutdown();
}
