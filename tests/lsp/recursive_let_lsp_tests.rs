//! A local `let f = { … f(…) … }` is `f` inside its own closure: the
//! LSP's rename, references and definition treat the closure's calls of
//! itself as uses of the `let`'s name.

use serde_json::{Value, json};

use crate::support::LspClient;

const SRC: &str = "fn main() {\n  \
                   let count = { n ->\n    \
                   match n {\n      \
                   0 -> 0\n      \
                   _ -> 1 + count(n - 1)\n    \
                   }\n  \
                   }\n  \
                   println(count(3))\n\
                   }\n";

/// (0-based line, 0-based char) of the `occurrence`-th match of
/// `needle` in `text`.
fn pos_of(text: &str, needle: &str, occurrence: usize) -> (u64, u64) {
    let off = text
        .match_indices(needle)
        .nth(occurrence)
        .unwrap_or_else(|| panic!("{needle:?} ({occurrence}) is not in the text"))
        .0;
    let line = text[..off].matches('\n').count() as u64;
    let line_start = text[..off].rfind('\n').map_or(0, |i| i + 1);
    (line, (off - line_start) as u64)
}

fn starts(locations: &[Value]) -> Vec<(u64, u64)> {
    let mut starts: Vec<(u64, u64)> = locations
        .iter()
        .map(|l| {
            (
                l.pointer("/range/start/line")
                    .and_then(Value::as_u64)
                    .unwrap(),
                l.pointer("/range/start/character")
                    .and_then(Value::as_u64)
                    .unwrap(),
            )
        })
        .collect();
    starts.sort();
    starts
}

#[test]
fn rename_from_a_call_of_itself_renames_the_let_and_every_call() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_recursive_let_rename.silt";
    client.did_open_and_wait(uri, SRC);
    let (line, character) = pos_of(SRC, "count(n - 1)", 0);
    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
            "newName": "tally"
        }),
    );
    let edits = resp
        .pointer("/result/changes")
        .and_then(|c| c.get(uri))
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("rename must edit the file; got {resp}"))
        .clone();
    assert_eq!(
        starts(&edits),
        vec![
            pos_of(SRC, "count = ", 0),
            pos_of(SRC, "count(n - 1)", 0),
            pos_of(SRC, "count(3)", 0),
        ],
        "{edits:#?}"
    );
    client.shutdown();
}

#[test]
fn a_call_of_itself_goes_to_the_let() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_recursive_let_definition.silt";
    client.did_open_and_wait(uri, SRC);
    let (line, character) = pos_of(SRC, "count(n - 1)", 0);
    let resp = client.request(
        "textDocument/definition",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        }),
    );
    let result = resp.get("result").expect("a result");
    let locations = match result {
        Value::Array(locations) => locations.clone(),
        location => vec![location.clone()],
    };
    assert_eq!(
        starts(&locations),
        vec![pos_of(SRC, "count = ", 0)],
        "{resp}"
    );
    client.shutdown();
}

#[test]
fn a_closure_that_calls_itself_has_no_diagnostics() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_recursive_let_diagnostics.silt";
    let diagnostics = client.did_open_and_collect_diagnostics(uri, SRC);
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    client.shutdown();
}
