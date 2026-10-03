//! The LSP's quick fixes come from the diagnostics themselves: the
//! producer attaches them (`Diagnostic::fixes`), the LSP diagnostic
//! carries them in `data`, and `textDocument/codeAction` hands them back.
//! These run end to end through `silt lsp`: the real diagnostic must make
//! the server offer the fix.

use serde_json::{Value, json};

use crate::support::LspClient;

/// The code actions the server offers for the first diagnostic of
/// `source` whose message contains `needle`.
fn actions_for(uri: &str, source: &str, needle: &str) -> Vec<Value> {
    let mut client = LspClient::spawn();
    let diags = client.did_open_and_collect_diagnostics(uri, source);
    let diag = diags
        .iter()
        .find(|d| d["message"].as_str().is_some_and(|m| m.contains(needle)))
        .unwrap_or_else(|| panic!("no diagnostic with {needle:?}; got {diags:?}"))
        .clone();
    let resp = client.request(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": uri },
            "range": diag["range"].clone(),
            "context": { "diagnostics": [diag] }
        }),
    );
    client.shutdown();
    resp["result"].as_array().cloned().unwrap_or_default()
}

/// A function type written `(A -> B)` gets the fix that rewrites it to
/// `Fn(A) -> B`, as one edit over the parenthesised type.
#[test]
fn arrow_fn_type_quickfix_rewrites_the_type() {
    let uri = "file:///tmp/silt_r86_arrow_fn.silt";
    let actions = actions_for(uri, "fn f(g: (Int -> Int)) { 1 }\n", "tuple type");
    let fix = actions
        .iter()
        .find(|a| a["title"] == "Change `(a -> b)` to `Fn(a) -> b`")
        .unwrap_or_else(|| panic!("no arrow fix; got {actions:?}"));
    let edits = &fix["edit"]["changes"][uri];
    assert_eq!(edits[0]["newText"], "Fn(Int) -> Int");
    assert_eq!(edits[0]["range"]["start"]["character"], 8);
    assert_eq!(edits[0]["range"]["end"]["character"], 20);
}

/// A bare `Int` where `Result(Int, Int)` is declared gets the fix that
/// wraps it in `Ok(...)`.
#[test]
fn wrap_in_ok_quickfix_offered_for_live_typechecker_error() {
    let uri = "file:///tmp/silt_r86_wrap_in_ok.silt";
    let actions = actions_for(
        uri,
        "fn produce() -> Result(Int, Int) { 21 }\n",
        "expected Result",
    );
    assert!(
        actions
            .iter()
            .any(|a| a["title"] == "Wrap expression in `Ok(...)`"),
        "no Ok-wrap fix; got {actions:?}"
    );
}

/// A use of a builtin module that is not imported gets the fix that adds
/// the import at the top of the file.
#[test]
fn add_import_quickfix_inserts_the_import() {
    let uri = "file:///tmp/silt_r86_add_import.silt";
    let actions = actions_for(uri, "fn main() { list.length([1]) }\n", "is not imported");
    let fix = actions
        .iter()
        .find(|a| a["title"] == "Add import for `list`")
        .unwrap_or_else(|| panic!("no import fix; got {actions:?}"));
    let edits = &fix["edit"]["changes"][uri];
    assert_eq!(edits[0]["newText"], "import list\n");
    assert_eq!(edits[0]["range"]["start"]["line"], 0);
}

/// Every published diagnostic carries its code, and the labels of a
/// diagnostic travel as related information.
#[test]
fn diagnostics_carry_their_code_and_labels() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_stage5_codes.silt";
    let source = "fn main() {\n  let x: Int = \"a\"\n  x\n}\nfn twice() { 1 }\nfn twice() { 2 }\n";
    let diags = client.did_open_and_collect_diagnostics(uri, source);
    client.shutdown();
    let mismatch = diags
        .iter()
        .find(|d| {
            d["message"]
                .as_str()
                .is_some_and(|m| m.starts_with("type mismatch"))
        })
        .unwrap_or_else(|| panic!("no type mismatch; got {diags:?}"));
    assert_eq!(mismatch["code"], "E0301");
    assert_eq!(mismatch["source"], "silt");
    let twice = diags
        .iter()
        .find(|d| {
            d["message"]
                .as_str()
                .is_some_and(|m| m.contains("bound twice"))
        })
        .unwrap_or_else(|| panic!("no duplicate; got {diags:?}"));
    assert_eq!(twice["code"], "E0110");
    let related = &twice["relatedInformation"][0];
    assert_eq!(related["location"]["range"]["start"]["line"], 4);
    assert_eq!(related["message"], "first bound here, by the function");
}

/// `source` with the LSP `edits` applied (ASCII text: a character is a
/// byte), the last first.
fn apply_edits(source: &str, edits: &Value) -> String {
    let offset = |pos: &Value| -> usize {
        let line = pos["line"].as_u64().unwrap() as usize;
        let character = pos["character"].as_u64().unwrap() as usize;
        source
            .split_inclusive('\n')
            .take(line)
            .map(str::len)
            .sum::<usize>()
            + character
    };
    let mut placed: Vec<(usize, usize, String)> = edits
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                offset(&e["range"]["start"]),
                offset(&e["range"]["end"]),
                e["newText"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    placed.sort_by_key(|e| std::cmp::Reverse(e.0));
    let mut out = source.to_string();
    for (start, end, text) in placed {
        out.replace_range(start..end, &text);
    }
    out
}

/// Whether `source` lexes and parses.
fn parses(source: &str) -> bool {
    let Ok(tokens) = silt::lexer::Lexer::new(silt::source::FileId::default(), source).tokenize()
    else {
        return false;
    };
    silt::parser::Parser::new(tokens, source)
        .parse_program()
        .is_ok()
}

/// A function body whose value is not the declared `Result` is wrapped
/// at its tail expression, inside the braces, and the result parses.
#[test]
fn wrap_in_ok_wraps_the_tail_of_a_block_body() {
    let uri = "file:///tmp/silt_s2_wrap_tail.silt";
    let source = "fn produce() -> Result(Int, Int) {\n  let x = 1\n  x + 20\n}\n";
    let actions = actions_for(uri, source, "expected Result");
    let fix = actions
        .iter()
        .find(|a| a["title"] == "Wrap expression in `Ok(...)`")
        .unwrap_or_else(|| panic!("no Ok-wrap fix; got {actions:?}"));
    let fixed = apply_edits(source, &fix["edit"]["changes"][uri]);
    assert_eq!(
        fixed,
        "fn produce() -> Result(Int, Int) {\n  let x = 1\n  Ok(x + 20)\n}\n"
    );
    assert!(parses(&fixed), "{fixed}");
}

/// A `?` in a function without a `Result` return type: wrapping the
/// body in `Ok(...)` would not fix it, so no Ok-wrap is offered.
#[test]
fn no_wrap_in_ok_for_a_question_mark_in_a_unit_function() {
    let uri = "file:///tmp/silt_s2_qmark_unit.silt";
    let source = "fn parse() -> Result(Int, String) { Ok(1) }\n\
                  fn get() {\n  let x = parse()?\n  println(x)\n}\n\
                  fn main() { get() }\n";
    let actions = actions_for(uri, source, "expected Result");
    assert!(
        !actions
            .iter()
            .any(|a| a["title"] == "Wrap expression in `Ok(...)`"),
        "an Ok-wrap was offered; got {actions:?}"
    );
}
